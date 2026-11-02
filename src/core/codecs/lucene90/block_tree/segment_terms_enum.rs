/*
 * Licensed to the Apache Software Foundation (ASF) under one or more
 * contributor license agreements.  See the NOTICE file distributed with
 * this work for additional information regarding copyright ownership.
 * The ASF licenses this file to You under the Apache License, Version 2.0
 * (the "License"); you may not use this file except in compliance with
 * the License.  You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use crate::core::store::data_input_ext::DataInputExt;
use crate::core::util::access::ByteSource;
use std::fmt::{Display, Formatter};

use crate::core::codecs::block_term_state::TermStateEnum;
use crate::core::codecs::block_tree::floor_data::FloorData;
use crate::core::codecs::block_tree::lucene90_block_tree_terms_reader::{
  NO_OUTPUT, OUTPUT_FLAG_HAS_TERMS, OUTPUT_FLAG_IS_FLOOR, OUTPUT_FLAGS_NUM_BITS,
};
use crate::core::codecs::lucene90::block_tree::field_reader::FieldReader;
use crate::core::codecs::lucene90::block_tree::segment_terms_enum_frame::SegmentTermsEnumFrame;
use crate::core::codecs::lucene90::block_tree::stats::Stats;
use crate::core::codecs::postings_reader_base::PostingsReaderBase;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{PrepareSeekStatus, SeekStatus, TermsEnum};
use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::store::{ByteArrayDataInput, DataInput, IndexInput};
use crate::core::util::ToInt;
use crate::core::util::accountable::Accountable;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::dummy::dummy_attribute_source::DummyAttributeSource;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fst_impl::fst::Arc;
use crate::core::util::fst_impl::reverse_random_access_reader::ReverseRandomAccessReader;
use crate::core::util::group_vint_util::GroupVIntUtil;

pub(crate) const STATIC_FRAME_IDX: usize = usize::MAX;

/// Iterates through terms in this field.
pub struct SegmentTermsEnum<I, PR>
where
  I: IndexInput,
  PR: PostingsReaderBase,
{
  // Lazy init: input stream
  pub(crate) input: Option<I>,
  // Dynamic frame indices equal their ordinals; the cached frame is separate.
  pub(crate) stack: Vec<SegmentTermsEnumFrame<PR::TermState>>,
  pub(crate) static_frame: Box<SegmentTermsEnumFrame>,
  pub(crate) current_frame_idx: usize,
  pub(crate) term_exists: bool,
  pub(crate) fr: FieldReader<I, PR>,
  target_before_current_length: i32,
  output_accumulator: OutputAccumulator,
  valid_index_prefix: usize,
  eof: bool,
  pub(crate) term: BytesRefBuilder<Vec<u8>>,
  fst_reader: Option<ReverseRandomAccessReader<I::RandomAccessSlice>>,
  arcs: Vec<Arc<BytesRef<std::sync::Arc<Vec<u8>>>>>,
  prepare_seek_status: PrepareSeekStatus,
}

impl<I, PR> SegmentTermsEnum<I, PR>
where
  I: IndexInput,
  PR: PostingsReaderBase,
{
  pub fn new(fr: FieldReader<I, PR>) -> Result<Self> {
    // Construct SegmentTerms first
    let fst_reader = match &fr.index {
      Some(index) => Some(index.get_bytes_reader()?),
      None => None,
    };

    let arc = match fr.index.as_ref() {
      Some(index) => {
        let arc = index.get_first_arc_owned();
        debug_assert!(arc.is_final());
        arc
      },
      None => Arc::default(),
    };
    let arcs = vec![arc];

    // Used to hold seek by TermState, or cached seek.
    let static_frame = Box::new(SegmentTermsEnumFrame::new(-1, &fr)?);
    Ok(Self {
      input: None,
      stack: Vec::new(),
      static_frame,
      current_frame_idx: STATIC_FRAME_IDX,
      term_exists: false,
      fr,
      target_before_current_length: 0,
      output_accumulator: OutputAccumulator::new(),
      valid_index_prefix: 0,
      eof: false,
      term: BytesRefBuilder::new(),
      fst_reader,
      arcs,
      prepare_seek_status: PrepareSeekStatus::NotFound,
    })
  }
  pub(crate) fn init_index_input(&mut self) -> Result<()> {
    if self.input.is_none() {
      self.input = Some(
        self
          .fr
          .parent
          .as_ref()
          .ok_or_else(|| LuceneError::illegal_state("terms reader is missing"))?
          .terms_in
          .try_clone()?,
      );
    }
    Ok(())
  }

  /// Runs through the entire terms dictionary and computes block statistics.
  pub fn compute_block_stats(&mut self) -> Result<Stats> {
    let parent = self
      .fr
      .parent
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("terms reader is missing"))?;
    let mut stats = Stats::new(parent.segment.to_string(), self.fr.field_info.name.clone());
    if let Some(index) = self.fr.index.as_ref() {
      stats.index_num_bytes = index.fst_reader.ram_bytes_used()?;
    }

    self.current_frame_idx = STATIC_FRAME_IDX;
    let arc = if let Some(index) = self.fr.index.as_ref() {
      index.get_first_arc(&mut self.arcs[0]);
      debug_assert!(self.arcs[0].is_final());
      Some(0)
    } else {
      None
    };
    self.current_frame_idx = self.push_root_frame(arc)?;
    self.stack[self.current_frame_idx].fp_orig = self.stack[self.current_frame_idx].fp;
    SegmentTermsEnumFrame::load_block(self.current_frame_idx, self)?;
    self.valid_index_prefix = 0;
    let is_floor = !self.stack[self.current_frame_idx].is_last_in_floor;
    stats.start_block(&self.stack[self.current_frame_idx], is_floor);

    'all_terms: loop {
      while self.stack[self.current_frame_idx].next_ent
        == self.stack[self.current_frame_idx].ent_count
      {
        stats.end_block(&self.stack[self.current_frame_idx])?;
        if !self.stack[self.current_frame_idx].is_last_in_floor {
          SegmentTermsEnumFrame::load_next_floor_block(self.current_frame_idx, self)?;
          stats.start_block(&self.stack[self.current_frame_idx], true);
          break;
        }
        if self.stack[self.current_frame_idx].ord == 0 {
          break 'all_terms;
        }
        let last_fp = self.stack[self.current_frame_idx].fp_orig;
        self.current_frame_idx = self.stack[self.current_frame_idx].ord as usize - 1;
        debug_assert_eq!(
          Some(last_fp),
          self.stack[self.current_frame_idx].last_sub_fp
        );
      }

      loop {
        if SegmentTermsEnumFrame::next(self.current_frame_idx, self)? {
          let last_sub_fp = self.stack[self.current_frame_idx]
            .last_sub_fp
            .ok_or_else(|| LuceneError::illegal_state("term block has no current sub-block"))?;
          self.current_frame_idx = self.push_frame(None, last_sub_fp, self.term.length())?;
          self.stack[self.current_frame_idx].fp_orig = self.stack[self.current_frame_idx].fp;
          SegmentTermsEnumFrame::load_block(self.current_frame_idx, self)?;
          let is_floor = !self.stack[self.current_frame_idx].is_last_in_floor;
          stats.start_block(&self.stack[self.current_frame_idx], is_floor);
        } else {
          stats.term(self.term.get_bytes_ref());
          break;
        }
      }
    }

    stats.finish();

    self.current_frame_idx = STATIC_FRAME_IDX;
    let arc = if let Some(index) = self.fr.index.as_ref() {
      index.get_first_arc(&mut self.arcs[0]);
      debug_assert!(self.arcs[0].is_final());
      Some(0)
    } else {
      None
    };
    self.current_frame_idx = self.push_root_frame(arc)?;
    self.stack[self.current_frame_idx].rewind()?;
    SegmentTermsEnumFrame::load_block(self.current_frame_idx, self)?;
    self.valid_index_prefix = 0;
    self.term.clear();

    Ok(stats)
  }
  fn get_frame(&mut self, ord: usize) -> Result<usize> {
    let frame_idx = ord;
    if frame_idx >= self.stack.len() {
      ArrayUtil::grow_capacity(&mut self.stack, frame_idx + 1)?;
      for i in self.stack.len()..=frame_idx {
        let frame = SegmentTermsEnumFrame::<PR::TermState>::new(i as i32, &self.fr)?;
        self.stack.push(frame);
      }
    }

    debug_assert_eq!(self.stack[frame_idx].ord, ord as i32, "Frame ord mismatch");
    Ok(frame_idx)
  }
  pub(crate) fn get_arc(&mut self, ord: usize) -> Result<usize> {
    if ord >= self.arcs.len() {
      ArrayUtil::grow_capacity(&mut self.arcs, ord + 1)?;
      let no_output = &*NO_OUTPUT;
      while self.arcs.len() <= ord {
        self
          .arcs
          .push(Arc::with_outputs(no_output.clone(), no_output.clone()));
      }
    }
    Ok(ord)
  }
  fn push_root_frame(&mut self, arc: Option<usize>) -> Result<usize> {
    self.output_accumulator.reset();
    self.output_accumulator.push(&self.fr.root_code);
    self.push_frame_with_length(arc, 0)
  }
  pub(crate) fn push_frame_with_length(
    &mut self,
    arc: Option<usize>,
    length: usize,
  ) -> Result<usize> {
    self.output_accumulator.prepare_read();
    let code = self.fr.read_vlong_output(&mut self.output_accumulator)?;
    let fp_seek = ((code as u64) >> OUTPUT_FLAGS_NUM_BITS) as usize;
    let current_ord = if self.current_frame_idx == STATIC_FRAME_IDX {
      -1
    } else {
      self.stack[self.current_frame_idx].ord
    };
    let ord = (current_ord + 1) as usize;
    let frame_idx = self.get_frame(ord)?;
    let f = &mut self.stack[frame_idx];
    f.has_terms = (code & OUTPUT_FLAG_HAS_TERMS as i64) != 0;
    f.has_terms_orig = f.has_terms;
    f.is_floor = (code & OUTPUT_FLAG_IS_FLOOR as i64) != 0;

    if f.is_floor {
      f.set_floor_data(&self.output_accumulator)?;
    }
    let pushed_frame_idx = self.push_frame(arc, fp_seek, length)?;
    debug_assert_eq!(pushed_frame_idx, frame_idx);
    Ok(frame_idx)
  }
  pub(crate) fn push_frame(
    &mut self,
    arc: Option<usize>,
    fp: usize,
    length: usize,
  ) -> Result<usize> {
    let current_ord = if self.current_frame_idx == STATIC_FRAME_IDX {
      -1
    } else {
      self.stack[self.current_frame_idx].ord
    };
    let ord = (current_ord + 1) as usize;
    let frame_idx = self.get_frame(ord)?;
    let f = &mut self.stack[frame_idx];
    f.arc = arc;

    if f.fp_orig == fp && f.next_ent != -1 {
      if f.ord > self.target_before_current_length {
        f.rewind()?;
      }
      debug_assert_eq!(length, f.prefix_length);
    } else {
      f.next_ent = -1;
      f.prefix_length = length;
      f.state.as_mut().term_block_ord = 0;
      f.fp_orig = fp;
      f.fp = fp;
      f.last_sub_fp = None;
    }

    Ok(frame_idx)
  }
  #[cfg(any(test, debug_assertions))]
  fn clear_eof(&mut self) -> bool {
    self.eof = false;
    true
  }
  #[cfg(any(test, debug_assertions))]
  fn set_eof(&mut self) -> bool {
    self.eof = true;
    true
  }

  pub fn prepare_seek_exact<BS: ByteSource>(
    &mut self,
    target: &BytesRef<BS>,
    prefetch: bool,
  ) -> Result<Option<()>> {
    if self.fr.index.is_none() {
      return Err(LuceneError::illegal_state("terms index was not loaded"));
    }
    if self.fr.size()? > 0
      && (target
        .compare_to(self.fr.get_min()?.as_ref().ok_or_else(|| {
          LuceneError::illegal_state("terms minimum is missing for a non-empty field")
        })?)
        .to_int()
        < 0
        || target
          .compare_to(self.fr.get_max()?.as_ref().ok_or_else(|| {
            LuceneError::illegal_state("terms maximum is missing for a non-empty field")
          })?)
          .to_int()
          > 0)
    {
      self.prepare_seek_status = PrepareSeekStatus::NotFound;
      return Ok(None);
    }
    self.term.grow(1 + target.length)?;
    #[cfg(any(test, debug_assertions))]
    debug_assert!(self.clear_eof());
    let mut target_upto;
    let target_before_current_length = if self.current_frame_idx == STATIC_FRAME_IDX {
      -1
    } else {
      self.stack[self.current_frame_idx].ord
    };
    self.target_before_current_length = target_before_current_length;
    self.output_accumulator.reset();

    // -1 means equal to staticFrame
    let mut arc_index;
    if self.current_frame_idx != STATIC_FRAME_IDX {
      let mut arc;
      // We are already seek'd; find the common
      // prefix of new seek term vs current term and
      // re-use the corresponding seek state.  For
      // example, if app first seeks to foobar, then
      // seeks to foobaz, we can re-use the seek state
      // for the first 5 bytes.
      arc_index = 0;
      arc = &mut self.arcs[arc_index];
      debug_assert!(arc.is_final());
      self.output_accumulator.push(&arc.output);

      target_upto = 0;
      let mut last_frame: usize = 0;
      debug_assert!(self.valid_index_prefix <= self.term.length());

      let target_limit = std::cmp::min(target.length, self.valid_index_prefix);

      let mut cmp = 0;

      while target_upto < target_limit {
        let term_byte = self.term.byte_at(target_upto) as i32;
        let target_byte = target.as_byte_slice()[target_upto] as i32;
        cmp = term_byte - target_byte;
        if cmp != 0 {
          break;
        }
        arc_index = 1 + target_upto;
        arc = &mut self.arcs[arc_index];
        debug_assert_eq!(arc.label(), target.as_byte_slice()[target_upto] as i32);
        self.output_accumulator.push(&arc.output);

        if arc.is_final() {
          last_frame = (self.stack[last_frame].ord + 1) as usize;
        }

        target_upto += 1;
      }

      if cmp == 0 {
        // Second compare the rest of the term, but
        // don't save arc/output/frame; we only do this
        // to find out if the target term is before,
        // equal or after the current term
        let a = &self.term.bytes_ref.bytes[target_upto..self.term.length()];
        let b = &target.as_byte_slice()[target_upto..];
        cmp = a.cmp(b).to_int();
      }

      if cmp < 0 {
        self.current_frame_idx = last_frame;
      } else if cmp > 0 {
        // Uncommon case: target term
        // is before current term; this means we can
        // keep the currentFrame, but we must rewind it
        // (so we scan from the start)
        self.target_before_current_length = self.stack[last_frame].ord;
        self.current_frame_idx = last_frame;
        self.stack[last_frame].rewind()?;
      } else {
        debug_assert_eq!(self.term.length(), target.length);
        if self.term_exists {
          self.prepare_seek_status = PrepareSeekStatus::Found;
          return Ok(Some(()));
        }
      }
    } else {
      {
        arc_index = 0;
        let arc = &mut self.arcs[0];
        self.target_before_current_length = -1;

        self
          .fr
          .index
          .as_ref()
          .ok_or_else(|| LuceneError::illegal_state("terms index was not loaded"))?
          .get_first_arc(arc);
        debug_assert!(arc.is_final());

        self.output_accumulator.push(&arc.output);

        self.current_frame_idx = STATIC_FRAME_IDX;

        target_upto = 0;
        self.output_accumulator.push(&arc.next_final_output);
      }
      self.current_frame_idx = self.push_frame_with_length(Some(0), 0)?;
      self.output_accumulator.pop(&self.arcs[0].next_final_output);
    }
    // We are done sharing the common prefix with the incoming target and where we
    // are currently seek'd; now continue walking the index:
    while target_upto < target.length {
      let target_label = target.as_byte_slice()[target_upto] as i32;

      let next_arc_idx = self.get_arc(1 + target_upto)?;
      let fr_index = self
        .fr
        .index
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("terms index was not loaded"))?;
      let reader = self
        .fst_reader
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("terms index reader is missing"))?;

      debug_assert!(arc_index < next_arc_idx);
      let (follow_arcs, next_arcs) = self.arcs.split_at_mut(next_arc_idx);
      let v = fr_index.find_target_arc(
        target_label,
        &follow_arcs[arc_index],
        &mut next_arcs[0],
        reader,
      )?;

      if v.is_none() {
        // index exhausted
        {
          self.valid_index_prefix = self.stack[self.current_frame_idx].prefix_length;
        }
        SegmentTermsEnumFrame::scan_to_floor_frame_with_target(
          self.current_frame_idx,
          target,
          self,
          true,
        )?;
        if !self.stack[self.current_frame_idx].has_terms {
          self.term_exists = false;
          self.term.set_byte_at(target_upto, target_label as u8);
          self.term.set_length(target_upto + 1);
          self.prepare_seek_status = PrepareSeekStatus::NotFound;
          return Ok(None);
        }

        if prefetch {
          SegmentTermsEnumFrame::prefetch_block(self.current_frame_idx, self)?;
        }
        self.prepare_seek_status = PrepareSeekStatus::Pending;
        return Ok(Some(()));
      } else {
        arc_index = next_arc_idx;

        let arc = &mut self.arcs[next_arc_idx];
        self.term.set_byte_at(target_upto, target_label as u8);
        self.output_accumulator.push(&arc.output);
        target_upto += 1;
        if arc.is_final() {
          self.output_accumulator.push(&arc.next_final_output);
          self.current_frame_idx = self.push_frame_with_length(Some(next_arc_idx), target_upto)?;
          self
            .output_accumulator
            .pop(&self.arcs[next_arc_idx].next_final_output);
        }
      }
    }
    {
      self.valid_index_prefix = self.stack[self.current_frame_idx].prefix_length;
    }
    SegmentTermsEnumFrame::scan_to_floor_frame_with_target(
      self.current_frame_idx,
      target,
      self,
      true,
    )?;
    if !self.stack[self.current_frame_idx].has_terms {
      self.term_exists = false;
      self.term.set_length(target_upto);
      self.prepare_seek_status = PrepareSeekStatus::NotFound;
      return Ok(None);
    }

    if prefetch {
      SegmentTermsEnumFrame::prefetch_block(self.current_frame_idx, self)?;
    }
    self.prepare_seek_status = PrepareSeekStatus::Pending;
    Ok(Some(()))
  }
}

impl<I, PR> BytesRefIterator for SegmentTermsEnum<I, PR>
where
  I: IndexInput,
  PR: PostingsReaderBase,
{
  type Value<'a>
    = BytesRef<&'a [u8]>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    let input_none = { self.input.is_none() };
    if input_none {
      let arc = if let Some(index) = self.fr.index.as_ref() {
        let arc = &mut self.arcs[0];
        index.get_first_arc(arc);
        debug_assert!(arc.is_final());
        Some(0)
      } else {
        None
      };
      self.current_frame_idx = self.push_root_frame(arc)?;
      SegmentTermsEnumFrame::load_block(self.current_frame_idx, self)?;
    }
    self.target_before_current_length = if self.current_frame_idx == STATIC_FRAME_IDX {
      -1
    } else {
      self.stack[self.current_frame_idx].ord
    };
    debug_assert!(!self.eof);

    if self.current_frame_idx == STATIC_FRAME_IDX {
      // If seek was previously called and the term was
      // cached, or seek(TermState) was called, usually
      // caller is just going to pull a D/&PEnum or get
      // docFreq, etc.  But, if they then call next(),
      // this method catches up all internal state so next()
      // works properly:
      let v = std::mem::take(&mut self.term.bytes_ref);
      let found = self.seek_exact(&v)?;
      self.term.bytes_ref = v;
      debug_assert!(found);
    }
    loop {
      let res = {
        let current_frame = &self.stack[self.current_frame_idx];
        current_frame.next_ent == current_frame.ent_count
      };
      if res {
        let is_last_in_floor = { self.stack[self.current_frame_idx].is_last_in_floor };
        if !is_last_in_floor {
          SegmentTermsEnumFrame::load_next_floor_block(self.current_frame_idx, self)?;
          break;
        } else {
          let (next_ent, last_sub_fp, last_fp) = {
            let current_frame_idx = self.current_frame_idx;
            let current_ord = self.stack[current_frame_idx].ord;
            if current_ord == 0 {
              #[cfg(any(test, debug_assertions))]
              debug_assert!(self.set_eof());
              self.term.clear();
              self.valid_index_prefix = 0;
              self.stack[current_frame_idx].rewind()?;
              self.term_exists = false;
              return Ok(None);
            }

            let last_fp = self.stack[current_frame_idx].fp_orig;
            self.current_frame_idx = current_ord as usize - 1;
            let current_frame = &self.stack[self.current_frame_idx];
            (current_frame.next_ent, current_frame.last_sub_fp, last_fp)
          };

          if next_ent == -1 || last_sub_fp != Some(last_fp) {
            // We popped into a frame that's not loaded
            // yet or not scan'd to the right entry
            SegmentTermsEnumFrame::scan_to_floor_frame(self.current_frame_idx, self)?;
            SegmentTermsEnumFrame::load_block(self.current_frame_idx, self)?;
            SegmentTermsEnumFrame::scan_to_sub_block(self.current_frame_idx, last_fp, self)?;
          }
          self.valid_index_prefix = self
            .valid_index_prefix
            .min(self.stack[self.current_frame_idx].prefix_length);
        }
      } else {
        break;
      }
    }

    loop {
      let has_next = SegmentTermsEnumFrame::next(self.current_frame_idx, self)?;
      if has_next {
        let last_sub_fp = self.stack[self.current_frame_idx]
          .last_sub_fp
          .ok_or_else(|| LuceneError::illegal_state("term block has no current sub-block"))?;
        let length = { self.term.length() };
        self.current_frame_idx = self.push_frame(None, last_sub_fp, length)?;
        // This is a "next" frame -- even if it's
        // floor'd we must pretend it isn't so we don't
        // try to scan to the right floor frame:
        SegmentTermsEnumFrame::load_block(self.current_frame_idx, self)?;
        continue;
      } else {
        let term = self.term.get_bytes_ref();
        return Ok(Some(BytesRef {
          bytes: term.bytes.as_slice(),
          offset: term.offset,
          length: term.length,
        }));
      };
    }
  }
}

impl<I, PR> TermsEnum for SegmentTermsEnum<I, PR>
where
  I: IndexInput,
  PR: PostingsReaderBase,
{
  type AttributeSource<'a>
    = &'a DummyAttributeSource
  where
    Self: 'a;
  type AttributeSourceMut<'a>
    = &'a mut DummyAttributeSource
  where
    Self: 'a;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn seek_exact<BS: ByteSource>(&mut self, target: &BytesRef<BS>) -> Result<bool> {
    match self.prepare_seek_exact(target, false)? {
      Some(_) => self.get_prepare_seek_exact_status(target),
      None => Ok(false),
    }
  }

  fn prepare_seek_exact<BS: ByteSource>(&mut self, target: &BytesRef<BS>) -> Result<Option<()>> {
    self.prepare_seek_exact(target, true)
  }
  fn get_prepare_seek_exact_status<BS: ByteSource>(
    &mut self,
    target: &BytesRef<BS>,
  ) -> Result<bool> {
    match self.prepare_seek_status {
      PrepareSeekStatus::NotFound => Ok(false),
      PrepareSeekStatus::Found => Ok(true),
      PrepareSeekStatus::Pending => {
        SegmentTermsEnumFrame::load_block(self.current_frame_idx, self)?;
        let result =
          SegmentTermsEnumFrame::scan_to_term(self.current_frame_idx, target, true, self)?;
        Ok(result == SeekStatus::Found)
      },
    }
  }

  fn seek_ceil<BS: ByteSource>(&mut self, target: &BytesRef<BS>) -> Result<SeekStatus> {
    if self.fr.index.is_none() {
      return Err(LuceneError::illegal_state("terms index was not loaded"));
    }

    self.term.grow(1 + target.length)?;
    #[cfg(any(test, debug_assertions))]
    debug_assert!(self.clear_eof());

    let mut target_upto;

    self.target_before_current_length = if self.current_frame_idx == STATIC_FRAME_IDX {
      -1
    } else {
      self.stack[self.current_frame_idx].ord
    };
    self.output_accumulator.reset();
    let mut arc_index;
    if self.current_frame_idx != STATIC_FRAME_IDX {
      let mut arc;
      // We are already seek'd; find the common
      // prefix of new seek term vs current term and
      // re-use the corresponding seek state.  For
      // example, if app first seeks to foobar, then
      // seeks to foobaz, we can re-use the seek state
      // for the first 5 bytes.
      arc_index = 0;
      arc = &self.arcs[arc_index];
      debug_assert!(arc.is_final());
      self.output_accumulator.push(&arc.output);
      target_upto = 0;

      let mut last_frame_index: usize = 0;
      debug_assert!(self.valid_index_prefix <= self.term.length());

      let target_limit = std::cmp::min(target.length, self.valid_index_prefix);
      let mut cmp = 0;

      while target_upto < target_limit {
        let term_byte = self.term.byte_at(target_upto) as i32;
        let target_byte = target.as_byte_slice()[target_upto] as i32;
        cmp = term_byte - target_byte;
        if cmp != 0 {
          break;
        }
        arc_index = 1 + target_upto;
        arc = &self.arcs[arc_index];
        debug_assert_eq!(arc.label(), target.as_byte_slice()[target_upto] as i32);
        self.output_accumulator.push(&arc.output);

        if arc.is_final() {
          last_frame_index = (self.stack[last_frame_index].ord + 1) as usize;
        }

        target_upto += 1;
      }

      if cmp == 0 {
        cmp = self.term.bytes_ref.bytes[target_upto..self.term.length()]
          .cmp(&target.as_byte_slice()[target_upto..])
          .to_int();
      }

      if cmp < 0 {
        self.current_frame_idx = last_frame_index;
      } else if cmp > 0 {
        self.target_before_current_length = 0;
        self.stack[last_frame_index].rewind()?;
        self.current_frame_idx = last_frame_index;
      } else {
        debug_assert_eq!(self.term.length(), target.length);
        if self.term_exists {
          return Ok(SeekStatus::Found);
        }
      }
    } else {
      self.target_before_current_length = -1;
      arc_index = 0;
      let arc = &mut self.arcs[arc_index];
      self
        .fr
        .index
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("terms index was not loaded"))?
        .get_first_arc(arc);

      debug_assert!(arc.is_final());

      self.output_accumulator.push(&arc.output);

      self.current_frame_idx = STATIC_FRAME_IDX;

      target_upto = 0;
      self.output_accumulator.push(&arc.next_final_output);
      self.current_frame_idx = self.push_frame_with_length(Some(0), 0)?;
      self.output_accumulator.pop(&self.arcs[0].next_final_output);
    }
    let mut next_arc_idx;
    while target_upto < target.length {
      let target_label = target.as_byte_slice()[target_upto] as i32;

      next_arc_idx = self.get_arc(1 + target_upto)?;
      let fr_index = self
        .fr
        .index
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("terms index was not loaded"))?;

      let reader = self
        .fst_reader
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("terms index reader is missing"))?;

      debug_assert!(arc_index < next_arc_idx);
      let (follow_arcs, next_arcs) = self.arcs.split_at_mut(next_arc_idx);
      let v = fr_index.find_target_arc(
        target_label,
        &follow_arcs[arc_index],
        &mut next_arcs[0],
        reader,
      )?;

      if v.is_none() {
        self.valid_index_prefix = self.stack[self.current_frame_idx].prefix_length;
        SegmentTermsEnumFrame::scan_to_floor_frame_with_target(
          self.current_frame_idx,
          target,
          self,
          true,
        )?;
        SegmentTermsEnumFrame::load_block(self.current_frame_idx, self)?;

        let result =
          SegmentTermsEnumFrame::scan_to_term(self.current_frame_idx, target, false, self)?;
        return if result == SeekStatus::End {
          {
            self.term.copy_bytes_from_ref(target)?;
            self.term_exists = false;
          }

          if self.next()?.is_some() {
            Ok(SeekStatus::NotFound)
          } else {
            Ok(SeekStatus::End)
          }
        } else {
          Ok(result)
        };
      } else {
        arc_index = next_arc_idx;
        let arc = &self.arcs[arc_index];
        self.term.set_byte_at(target_upto, target_label as u8);
        self.output_accumulator.push(&arc.output);

        target_upto += 1;
        if arc.is_final() {
          self.output_accumulator.push(&arc.next_final_output);
          self.current_frame_idx = self.push_frame_with_length(Some(arc_index), target_upto)?;
          self
            .output_accumulator
            .pop(&self.arcs[arc_index].next_final_output);
        }
      }
    }
    self.valid_index_prefix = self.stack[self.current_frame_idx].prefix_length;
    SegmentTermsEnumFrame::scan_to_floor_frame_with_target(
      self.current_frame_idx,
      target,
      self,
      true,
    )?;
    SegmentTermsEnumFrame::load_block(self.current_frame_idx, self)?;
    let result = SegmentTermsEnumFrame::scan_to_term(self.current_frame_idx, target, false, self)?;

    match result {
      SeekStatus::End => {
        {
          self.term.copy_bytes_from_ref(target)?;
          self.term_exists = false;
        }
        if self.next()?.is_some() {
          Ok(SeekStatus::NotFound)
        } else {
          Ok(SeekStatus::End)
        }
      },
      _ => Ok(result),
    }
  }

  fn seek_exact_with_ord(&mut self, _ord: i64) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    target: &BytesRef<BS>,
    other_state: &TermStateEnum,
  ) -> Result<()> {
    #[cfg(any(test, debug_assertions))]
    debug_assert!(self.clear_eof());
    if target.compare_to(self.term.get_bytes_mut_ref()).to_int() != 0 || !self.term_exists {
      self.static_frame.state = other_state.clone();
      self.current_frame_idx = STATIC_FRAME_IDX;
      self.term.copy_bytes_from_ref(target)?;
      self.static_frame.meta_data_upto = self
        .static_frame
        .get_term_block_ord(TermStateEnum::get_block_term_state)?;
      debug_assert!(self.static_frame.meta_data_upto > 0);
      self.valid_index_prefix = 0;
    }
    Ok(())
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    debug_assert!(!self.eof);
    Ok(BytesRef {
      bytes: self.term.bytes_ref.bytes.as_slice(),
      offset: self.term.bytes_ref.offset,
      length: self.term.bytes_ref.length,
    })
  }

  fn ord(&self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn doc_freq(&mut self) -> Result<i32> {
    debug_assert!(!self.eof);
    Ok(
      (if self.current_frame_idx == STATIC_FRAME_IDX {
        SegmentTermsEnumFrame::<PR::TermState>::decode_meta_data::<true, _, _>(
          self.current_frame_idx,
          self,
        )
      } else {
        SegmentTermsEnumFrame::<PR::TermState>::decode_meta_data::<false, _, _>(
          self.current_frame_idx,
          self,
        )
      })?
      .doc_freq,
    )
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    debug_assert!(!self.eof);
    Ok(
      (if self.current_frame_idx == STATIC_FRAME_IDX {
        SegmentTermsEnumFrame::<PR::TermState>::decode_meta_data::<true, _, _>(
          self.current_frame_idx,
          self,
        )
      } else {
        SegmentTermsEnumFrame::<PR::TermState>::decode_meta_data::<false, _, _>(
          self.current_frame_idx,
          self,
        )
      })?
      .total_term_freq,
    )
  }

  type PostingsEnum = PR::PostingsEnum;

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    debug_assert!(!self.eof);

    (if self.current_frame_idx == STATIC_FRAME_IDX {
      SegmentTermsEnumFrame::<PR::TermState>::decode_meta_data::<true, _, _>(
        self.current_frame_idx,
        self,
      )
    } else {
      SegmentTermsEnumFrame::<PR::TermState>::decode_meta_data::<false, _, _>(
        self.current_frame_idx,
        self,
      )
    })?;

    let field_info = &self.fr.field_info;
    let postings_reader = &self
      .fr
      .parent
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("terms reader is missing"))?
      .postings_reader;
    let v = if self.current_frame_idx == STATIC_FRAME_IDX {
      postings_reader.postings(field_info, &self.static_frame.state, reuse, flags)?
    } else {
      postings_reader.postings(
        field_info,
        &self.stack[self.current_frame_idx].state,
        reuse,
        flags,
      )?
    }
    .ok_or_else(|| LuceneError::illegal_state("could not get postings enum"))?;
    Ok(v)
  }

  type ImpactsEnum = PR::ImpactsEnum;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    debug_assert!(!self.eof);
    (if self.current_frame_idx == STATIC_FRAME_IDX {
      SegmentTermsEnumFrame::<PR::TermState>::decode_meta_data::<true, _, _>(
        self.current_frame_idx,
        self,
      )
    } else {
      SegmentTermsEnumFrame::<PR::TermState>::decode_meta_data::<false, _, _>(
        self.current_frame_idx,
        self,
      )
    })?;
    let field_info = &self.fr.field_info;
    let postings_reader = &self
      .fr
      .parent
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("terms reader is missing"))?
      .postings_reader;

    if self.current_frame_idx == STATIC_FRAME_IDX {
      postings_reader.impacts(field_info, &self.static_frame.state, flags)
    } else {
      postings_reader.impacts(field_info, &self.stack[self.current_frame_idx].state, flags)
    }
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    debug_assert!(!self.eof);
    (if self.current_frame_idx == STATIC_FRAME_IDX {
      SegmentTermsEnumFrame::<PR::TermState>::decode_meta_data::<true, _, _>(
        self.current_frame_idx,
        self,
      )
    } else {
      SegmentTermsEnumFrame::<PR::TermState>::decode_meta_data::<false, _, _>(
        self.current_frame_idx,
        self,
      )
    })?;
    if self.current_frame_idx == STATIC_FRAME_IDX {
      Ok(self.static_frame.state.clone())
    } else {
      Ok(self.stack[self.current_frame_idx].state.clone().into())
    }
  }
}

pub struct OutputAccumulator {
  pub(crate) outputs: Vec<BytesRef<std::sync::Arc<Vec<u8>>>>,
  pub(crate) num: usize,
  pub(crate) current_index: usize,
  pub(crate) index: usize,
}
impl OutputAccumulator {
  pub(crate) fn new() -> Self {
    Self {
      outputs: Vec::new(),
      num: 0,
      current_index: 0,
      index: 0,
    }
  }
  pub(crate) fn push(&mut self, output: &BytesRef<std::sync::Arc<Vec<u8>>>) {
    if !BytesRef::equals(output, &NO_OUTPUT) {
      debug_assert!(output.length > 0);
      if self.outputs.len() == self.num {
        if self.outputs.capacity() == 0 {
          self.outputs.reserve_exact(16);
        }
        self.outputs.push(output.clone());
      } else if !BytesRef::equals(&self.outputs[self.num], output) {
        self.outputs[self.num] = output.clone();
      }
      self.num += 1;
    }
  }

  pub(crate) fn pop(&mut self, output: &BytesRef<std::sync::Arc<Vec<u8>>>) {
    if !BytesRef::equals(output, &NO_OUTPUT) {
      debug_assert!(self.num > 0);
      debug_assert!(&self.outputs[self.num - 1] == output);
      self.num -= 1;
    }
  }
  pub(crate) fn pop_n(&mut self, cnt: usize) {
    debug_assert!(self.num >= cnt);
    self.num -= cnt;
  }

  pub(crate) fn output_count(&self) -> usize {
    self.num
  }

  pub(crate) fn reset(&mut self) {
    self.num = 0;
  }

  pub(crate) fn prepare_read(&mut self) {
    self.index = 0;
    self.current_index = 0;
  }
  /// Set the last arc as the source of the floorData.  
  /// This won't change the reading position of this [`OutputAccumulator`].
  pub(crate) fn set_floor_data(&self, floor_data: &mut ByteArrayDataInput<FloorData>) {
    debug_assert!(
      self.current_index == self.num - 1,
      "floor data should be stored in last arc, got current_index={}, num={}",
      self.current_index,
      self.num
    );

    let output = &self.outputs[self.current_index];
    let start = output.offset + self.index;
    let length = output.length - self.index;

    if floor_data.bytes.shares_storage_with(&output.bytes) {
      floor_data.reset_meta(start, length);
    } else {
      floor_data.reset_with_range(FloorData::Shared(output.bytes.clone()), start, length);
    }
  }
}

impl Display for OutputAccumulator {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "OutputAccumulator")
  }
}

impl crate::core::util::close::Closeable for OutputAccumulator {}

impl DataInput for OutputAccumulator {
  fn read_byte(&mut self) -> Result<u8> {
    let mut current = &self.outputs[self.current_index];
    if self.index >= current.length {
      self.current_index += 1;
      self.index = 0;
      current = &self.outputs[self.current_index];
    }
    let byte = current.bytes[current.offset + self.index];
    self.index += 1;
    Ok(byte)
  }

  fn get_bytes(&mut self, _len: usize) -> Result<std::borrow::Cow<'_, [u8]>> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn read_bytes(&mut self, _b: &mut [u8], _offset: usize, _len: usize) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn read_group_vint(&mut self, dst: &mut [i32], offset: usize) -> Result<()> {
    GroupVIntUtil::read_group_vint_i32(self, dst, offset)
  }

  fn skip_bytes(&mut self, _num_bytes: i64) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }
}

impl DataInputExt for OutputAccumulator {}
