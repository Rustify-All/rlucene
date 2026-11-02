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
use crate::core::codecs::block_term_state::TermStateEnum;
use crate::core::codecs::block_tree::field_reader::FieldReader;
use crate::core::codecs::block_tree::intersect_terms_enum_frame::IntersectTermsEnumFrame;
use crate::core::codecs::block_tree::lucene90_block_tree_terms_reader::IndexArc;
use crate::core::codecs::block_tree::segment_terms_enum::OutputAccumulator;
use crate::core::codecs::postings_reader_base::PostingsReaderBase;
use crate::core::index::BytesRef;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::store::IndexInput;
use crate::core::util::access::ByteSource;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::automation::compiled_automaton::AutomatonEnum;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::dummy::dummy_attribute_source::DummyAttributeSource;
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use crate::core::util::fst_impl::fst::Arc;
use crate::core::util::fst_impl::reverse_random_access_reader::ReverseRandomAccessReader;
use crate::core::util::{StringHelper, ToInt};

/// Used to implement efficient [`Terms::intersect`] for the block-tree.
///
/// Note that this enum cannot seek, except for the initial term during
/// initialization. It only advances by calling `next()` while iterating
/// through the intersection of the automaton and the terms.
///
/// It does not use the terms index at all: on initialization, it loads the
/// root block and scans forward until it reaches the initial term. Likewise,
/// on each call to `next()`, it scans forward until it finds a term that
/// matches the current automaton transition.
pub struct IntersectTermsEnum<I, PR>
where
  I: IndexInput,
  PR: PostingsReaderBase,
{
  pub(crate) input: I,
  pub(crate) stack: Vec<IntersectTermsEnumFrame<PR::TermState>>,
  arcs: Vec<IndexArc>,
  pub(crate) automaton: AutomatonEnum,
  common_suffix: Option<std::sync::Arc<BytesRef<Vec<u8>>>>,
  current_frame: usize,
  current_transition: usize,
  term: BytesRef<Vec<u8>>,
  fst_reader: ReverseRandomAccessReader<I::RandomAccessSlice>,
  pub(crate) fr: FieldReader<I, PR>,
  saved_start_term: Option<BytesRef<Vec<u8>>>,
  pub(crate) output_accumulator: OutputAccumulator,
}
impl<I, PR> IntersectTermsEnum<I, PR>
where
  I: IndexInput,
  PR: PostingsReaderBase,
{
  pub(crate) fn new(
    fr: FieldReader<I, PR>,
    mut automaton: AutomatonEnum,
    common_suffix: Option<std::sync::Arc<BytesRef<Vec<u8>>>>,
    start_term: Option<&BytesRef<Vec<u8>>>,
  ) -> Result<Self> {
    let input = fr
      .parent
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("terms reader is missing"))?
      .terms_in
      .try_clone()?;

    let mut stack = Vec::with_capacity(5);
    stack.push(IntersectTermsEnumFrame::new(0, &fr)?);

    let fr_index = fr
      .index
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("terms index was not loaded"))?;
    let fst_reader = fr_index.get_bytes_reader()?;

    // get first arc
    let first_arc_idx = 0;
    let mut arcs = Vec::with_capacity(5);
    arcs.push(fr_index.get_first_arc_owned());
    debug_assert!(arcs[first_arc_idx].is_final());

    {
      let f = &mut stack[0];
      f.fp = fr.root_block_fp as usize;
      f.fp_orig = fr.root_block_fp as usize;
      f.prefix = 0;
      IntersectTermsEnumFrame::set_state(&mut automaton, f, 0)?;
      f.arc = first_arc_idx;
    }

    let mut ite = IntersectTermsEnum {
      input,
      stack,
      arcs,
      automaton,
      common_suffix,
      current_frame: 0,
      current_transition: 0,
      term: BytesRef::new(),
      fst_reader,
      fr,
      saved_start_term: None,
      output_accumulator: OutputAccumulator::new(),
    };

    {
      let f = &mut ite.stack[first_arc_idx];
      f.arc = first_arc_idx;
      let code = ite.fr.root_code.clone();
      IntersectTermsEnumFrame::load_from_frame_index_data(&mut ite, code, first_arc_idx)?;
    }

    // only for assert
    debug_assert!(ite.set_saved_start_term(start_term)?);

    // push initial output
    {
      let arc = &ite.arcs[ite.stack[0].arc];
      if let Some(output) = arc.output.as_ref() {
        ite.output_accumulator.push(output);
      }
    }

    if let Some(st) = start_term.as_ref() {
      ite.seek_to_start_term(st)?;
    }

    ite.current_transition = ite.current_frame;

    Ok(ite)
  }

  pub(crate) fn set_saved_start_term(
    &mut self,
    start_term: Option<&BytesRef<Vec<u8>>>,
  ) -> Result<bool> {
    self.saved_start_term = start_term.map(BytesRef::deep_copy_of).transpose()?;
    Ok(true)
  }
  pub(crate) fn get_frame(&mut self, ord: usize) -> Result<()> {
    if ord >= self.stack.len() {
      ArrayUtil::grow_capacity(&mut self.stack, ord + 1)?;
      for i in self.stack.len()..=ord {
        let frame = IntersectTermsEnumFrame::new(i, &self.fr)?;
        self.stack.push(frame);
      }
    }
    debug_assert!(self.stack[ord].ord == ord);
    Ok(())
  }
  pub(crate) fn get_arc(&mut self, ord: usize) -> Result<usize> {
    if ord >= self.arcs.len() {
      ArrayUtil::grow_capacity(&mut self.arcs, ord + 1)?;
      while self.arcs.len() <= ord {
        self.arcs.push(Arc::with_outputs(None, None));
      }
    }
    Ok(ord)
  }
  fn push_frame(&mut self, state: i32) -> Result<usize> {
    debug_assert!(self.current_frame < self.stack.len());
    let ord = self.stack[self.current_frame].ord;
    let new_ord = ord + 1;

    self.get_frame(new_ord)?;

    let (last_sub_fp, prefix, suffix) = {
      let current = &self.stack[self.current_frame];
      (current.last_sub_fp, current.prefix, current.suffix)
    };

    let f_prefix = {
      let f = &mut self.stack[new_ord];
      f.fp = last_sub_fp;
      f.fp_orig = last_sub_fp;
      f.prefix = prefix + suffix;
      IntersectTermsEnumFrame::set_state(&mut self.automaton, f, state)?;
      f.prefix
    };
    // Walk the arc through the index -- we only
    // "bother" with this so we can get the floor data
    // from the index and skip floor blocks when
    // possible:
    let mut arc_idx = {
      let current = &self.stack[self.current_frame];
      current.arc
    };
    let mut idx = prefix;
    debug_assert!(suffix > 0);

    let init_output_count = self.output_accumulator.output_count();

    while idx < f_prefix {
      let target = self.term.bytes[idx] as i32;
      let next_idx = self.get_arc(idx + 1)?;
      let fr_index = self
        .fr
        .index
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("terms index was not loaded"))?;
      let reader = &mut self.fst_reader;

      debug_assert!(arc_idx < next_idx);
      let (follow_arcs, next_arcs) = self.arcs.split_at_mut(next_idx);
      let v = fr_index.find_target_arc(target, &follow_arcs[arc_idx], &mut next_arcs[0], reader)?;
      debug_assert!(v.is_some());
      arc_idx = next_idx;
      if let Some(output) = self.arcs[arc_idx].output.as_ref() {
        self.output_accumulator.push(output);
      }
      idx += 1;
    }

    let f = &mut self.stack[new_ord];
    f.arc = arc_idx;
    f.output_num = self.output_accumulator.output_count() - init_output_count;

    {
      let arc = &self.arcs[f.arc];
      debug_assert!(arc.is_final());
      if let Some(output) = arc.next_final_output.as_ref() {
        self.output_accumulator.push(output);
      }
    }

    IntersectTermsEnumFrame::load_from_output_accumulator(self, new_ord)?;
    let frame = &self.stack[new_ord];
    if let Some(output) = self.arcs[frame.arc].next_final_output.as_ref() {
      self.output_accumulator.pop(output);
    }
    Ok(new_ord)
  }
  fn get_state(&mut self) -> Result<i32> {
    let frame = &self.stack[self.current_frame];
    let mut state = frame.state;

    for idx in 0..frame.suffix {
      let b = frame.suffixes_reader.bytes[frame.start_byte_pos + idx] as i32;
      state = self.automaton.step(state, b)?;
      debug_assert!(state != -1);
    }
    Ok(state)
  }
  // NOTE: specialized to only doing the first-time
  // seek, but we could generalize it to allow
  // arbitrary seekExact/Ceil.  Note that this is a
  // seekFloor!
  fn seek_to_start_term(&mut self, target: &BytesRef<Vec<u8>>) -> Result<()> {
    debug_assert!(self.stack[self.current_frame].ord == 0);

    if self.term.length < target.length {
      ArrayUtil::grow_with(&mut self.term.bytes, target.length, || 0)?;
    }

    debug_assert!(self.stack[self.current_frame].arc == 0);

    for _ in 0..=(target.length) {
      loop {
        let frame_idx = self.current_frame;

        let (
          sav_next_ent,
          save_pos,
          save_len_pos,
          save_start_byte_pos,
          save_suffix,
          save_last_sub_fp,
          save_term_block_ord,
        ) = {
          let f = &self.stack[frame_idx];
          (
            f.next_ent,
            f.suffixes_reader.get_position(),
            f.suffix_lengths_reader.get_position(),
            f.start_byte_pos,
            f.suffix,
            f.last_sub_fp,
            f.term_state.as_ref().term_block_ord,
          )
        };

        let is_sub_block = self.stack[frame_idx].next()?;

        {
          let f = &self.stack[frame_idx];
          self.term.length = f.prefix + f.suffix;

          if self.term.bytes.len() < self.term.length {
            ArrayUtil::grow_with(&mut self.term.bytes, self.term.length, || 0)?;
          }

          let src_start = f.start_byte_pos;
          let src_end = src_start + f.suffix;
          let dst_start = f.prefix;
          let dst_end = dst_start + f.suffix;

          self.term.bytes[dst_start..dst_end]
            .copy_from_slice(&f.suffixes_reader.bytes[src_start..src_end]);
        }

        if is_sub_block && StringHelper::starts_with_byte_ref(target, &self.term) {
          // Recurse
          let state = self.get_state()?;
          self.current_frame = self.push_frame(state)?;
          break;
        } else {
          let cmp = self.term.cmp(target).to_int();
          if cmp < 0 {
            if self.stack[frame_idx].next_ent == self.stack[frame_idx].ent_count {
              if !self.stack[frame_idx].is_last_in_floor {
                IntersectTermsEnumFrame::load_next_floor_block(self, frame_idx)?;
                continue;
              } else {
                return Ok(());
              }
            }
            continue;
          } else if cmp == 0 {
            return Ok(());
          } else {
            {
              // Fallback to prior entry: the semantics of
              // this method is that the first call to
              // next() will return the term after the
              // requested term
              let f = &mut self.stack[frame_idx];
              f.next_ent = sav_next_ent;
              f.last_sub_fp = save_last_sub_fp;
              f.start_byte_pos = save_start_byte_pos;
              f.suffix = save_suffix;
              f.suffixes_reader.set_position(save_pos);
              f.suffix_lengths_reader.set_position(save_len_pos);
              f.term_state.as_mut().term_block_ord = save_term_block_ord;
            }

            {
              let f = &self.stack[frame_idx];
              let src_start = f.start_byte_pos;
              let src_end = src_start + f.suffix;
              let dst_start = f.prefix;
              let dst_end = dst_start + f.suffix;

              self.term.bytes[dst_start..dst_end]
                .copy_from_slice(&f.suffixes_reader.bytes[src_start..src_end]);
              self.term.length = f.prefix + f.suffix;
              // If the last entry was a block we don't
              // need to bother recursing and pushing to
              // the last term under it because the first
              // next() will simply skip the frame anyway
            }
            return Ok(());
          }
        }
      }
    }

    debug_assert!(false);
    Ok(())
  }

  pub(crate) fn pop_push_next(&mut self) -> Result<bool> {
    // Pop finished frames
    loop {
      let frame_idx = self.current_frame;
      if self.stack[frame_idx].next_ent != self.stack[frame_idx].ent_count {
        break;
      }
      if !self.stack[frame_idx].is_last_in_floor {
        // Advance to next floor block
        IntersectTermsEnumFrame::load_next_floor_block(self, frame_idx)?;
        break;
      } else {
        let ord = self.stack[frame_idx].ord;
        if ord == 0 {
          return Err(LuceneError::no_more_terms(""));
        }
        let last_fp = self.stack[frame_idx].fp_orig;
        let output_num = self.stack[frame_idx].output_num;
        self.output_accumulator.pop_n(output_num);

        self.current_frame = ord - 1;
        self.current_transition = self.current_frame;

        debug_assert!(self.stack[self.current_frame].last_sub_fp == last_fp);
      }
    }
    self.stack[self.current_frame].next()
  }
  fn next_(&mut self) -> Result<Option<&BytesRef<Vec<u8>>>> {
    let mut is_sub_block = self.pop_push_next()?;

    'next_term: loop {
      debug_assert!(
        self.stack[self.current_frame].transition_index
          == self.stack[self.current_transition].transition_index
      );

      let mut state: i32;
      let mut last_state: i32;

      let suffix = { self.stack[self.current_frame].suffix };
      // NOTE: suffix == 0 can only happen on the first term in a block, when
      // there is a term exactly matching a prefix in the index.  If we
      // could somehow re-org the code so we only checked this case immediately
      // after pushing a frame...
      if suffix != 0 {
        let label = {
          let frame = &self.stack[self.current_frame];
          let suffix_bytes = &frame.suffixes_reader.bytes;
          // This is the first byte of the suffix of the term we are now on:
          let label = suffix_bytes[frame.start_byte_pos] as i32;

          if label < self.stack[self.current_transition].transition.min {
            // Common case: we are scanning terms in this block to "catch up" to
            // current transition in the automaton:
            let min_trans = self.stack[self.current_transition].transition.min;
            while self.stack[self.current_frame].next_ent < self.stack[self.current_frame].ent_count
            {
              is_sub_block = self.stack[self.current_frame].next()?;
              let b = self.stack[self.current_frame].suffixes_reader.bytes
                [self.stack[self.current_frame].start_byte_pos] as i32;
              if b >= min_trans {
                continue 'next_term;
              }
            }
            // End of frame:
            is_sub_block = self.pop_push_next()?;
            continue 'next_term;
          }
          label
        };
        let mut transition_max = { self.stack[self.current_transition].transition.max };
        // Advance where we are in the automaton to match this label:
        while label > transition_max {
          let frame = &mut self.stack[self.current_frame];
          if frame.transition_index >= frame.transition_count - 1 {
            // Pop this frame: no further matches are possible because
            // we've moved beyond what the max transition will allow
            if frame.ord == 0 {
              return Ok(None);
            }
            let output_num = frame.output_num;
            self.output_accumulator.pop_n(output_num);
            let parent_ord = frame.ord - 1;
            self.current_frame = parent_ord;
            self.current_transition = self.current_frame;
            is_sub_block = self.pop_push_next()?;
            continue 'next_term;
          }

          frame.transition_index += 1;
          self
            .automaton
            .get_next_transition(&mut self.stack[self.current_transition].transition)?;

          if label < self.stack[self.current_transition].transition.min {
            let current_frame = &self.stack[self.current_frame];
            let min_trans = self.stack[self.current_transition].transition.min;
            let mut c = current_frame.next_ent < current_frame.ent_count;
            while c {
              is_sub_block = self.stack[self.current_frame].next()?;
              let b = self.stack[self.current_frame].suffixes_reader.bytes
                [self.stack[self.current_frame].start_byte_pos] as i32;
              if b >= min_trans {
                continue 'next_term;
              }
              let current_frame = &self.stack[self.current_frame];
              c = current_frame.next_ent < current_frame.ent_count
            }
            // End of frame:
            is_sub_block = self.pop_push_next()?;
            continue 'next_term;
          }
          transition_max = self.stack[self.current_transition].transition.max;
        }

        if let Some(common_suffix) = self.common_suffix.as_ref() {
          let frame = &mut self.stack[self.current_frame];
          if !is_sub_block {
            let term_len = frame.prefix + frame.suffix;
            if term_len < common_suffix.length {
              is_sub_block = self.pop_push_next()?;
              continue 'next_term;
            }

            let mut suffix_bytes_pos: usize;
            let mut common_suffix_bytes_pos: usize = 0;
            if common_suffix.length > frame.suffix {
              // A prefix of the common suffix overlaps with
              // the suffix of the block prefix so we first
              // test whether the prefix part matches:
              let len_in_prefix = common_suffix.length - frame.suffix;
              let mut term_bytes_pos = frame.prefix - len_in_prefix;
              let term_bytes_end = frame.prefix;
              while term_bytes_pos < term_bytes_end {
                if self.term.bytes[term_bytes_pos] != common_suffix.bytes[common_suffix_bytes_pos] {
                  is_sub_block = self.pop_push_next()?;
                  continue 'next_term;
                }
                term_bytes_pos += 1;
                common_suffix_bytes_pos += 1;
              }
              suffix_bytes_pos = frame.start_byte_pos;
            } else {
              suffix_bytes_pos = frame.start_byte_pos + frame.suffix - common_suffix.length;
            }

            let common_suffix_bytes_pos_end = common_suffix.length;
            while common_suffix_bytes_pos < common_suffix_bytes_pos_end {
              if self.stack[self.current_frame].suffixes_reader.bytes[suffix_bytes_pos]
                != common_suffix.bytes[common_suffix_bytes_pos]
              {
                is_sub_block = self.pop_push_next()?;
                continue 'next_term;
              }
              suffix_bytes_pos += 1;
              common_suffix_bytes_pos += 1;
            }
          }
        }
        let frame = &self.stack[self.current_frame];
        last_state = frame.state;
        state = self.stack[self.current_transition].transition.dest;

        let end = frame.start_byte_pos + frame.suffix;
        let mut idx = frame.start_byte_pos + 1;
        while idx < end {
          last_state = state;
          state = self
            .automaton
            .step(state, frame.suffixes_reader.bytes[idx] as i32)?;
          if state == -1 {
            is_sub_block = self.pop_push_next()?;
            continue 'next_term;
          }
          idx += 1;
        }
      } else {
        let frame = &self.stack[self.current_frame];
        state = frame.state;
        last_state = frame.last_state;
      }

      if is_sub_block {
        self.copy_term()?;
        let new_ord = self.push_frame(state)?;
        self.current_frame = new_ord;
        self.current_transition = self.current_frame;
        self.stack[new_ord].last_state = last_state;
      } else if self.automaton.is_accept(state)? {
        self.copy_term()?;
        debug_assert!(
          self
            .saved_start_term
            .as_ref()
            .is_none_or(|s| self.term > *s)
        );
        return Ok(Some(&self.term));
      }

      is_sub_block = self.pop_push_next()?;
    }
  }

  fn copy_term(&mut self) -> Result<()> {
    let current_frame = &self.stack[self.current_frame];
    let len = current_frame.prefix + current_frame.suffix;

    if self.term.bytes.len() < len {
      ArrayUtil::grow_with(&mut self.term.bytes, len, || 0)?;
    }

    let src_start = current_frame.start_byte_pos;
    let src_end = src_start + current_frame.suffix;
    let dst_start = current_frame.prefix;
    let dst_end = dst_start + current_frame.suffix;

    self.term.bytes[dst_start..dst_end]
      .copy_from_slice(&current_frame.suffixes_reader.bytes[src_start..src_end]);

    self.term.length = len;
    Ok(())
  }
}

impl<I, PR> BytesRefIterator for IntersectTermsEnum<I, PR>
where
  I: IndexInput,
  PR: PostingsReaderBase,
{
  type Value<'a>
    = &'a BytesRef<Vec<u8>>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    match self.next_() {
      Ok(Some(v)) => Ok(Some(v)),
      Ok(None) => Ok(None),
      Err(e) => match e.kind() {
        LuceneErrorKind::NoMoreTerms(_) => Ok(None),
        _ => Err(e),
      },
    }
  }
}

impl<I, PR> TermsEnum for IntersectTermsEnum<I, PR>
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

  fn seek_exact<BS: ByteSource>(&mut self, _term: &BytesRef<BS>) -> Result<bool> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn prepare_seek_exact<BS: ByteSource>(&mut self, _text: &BytesRef<BS>) -> Result<Option<()>> {
    Ok(Some(()))
  }

  fn get_prepare_seek_exact_status<BS: ByteSource>(
    &mut self,
    target: &BytesRef<BS>,
  ) -> Result<bool> {
    self.seek_exact(target)
  }

  fn seek_ceil<BS: ByteSource>(&mut self, _term: &BytesRef<BS>) -> Result<SeekStatus> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn seek_exact_with_ord(&mut self, _ord: i64) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    _state: &TermStateEnum,
  ) -> Result<()> {
    if !self.seek_exact(term)? {
      return Err(LuceneError::illegal_argument(format!(
        "term= {term} does not exist"
      )));
    }
    Ok(())
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    Ok(&self.term)
  }

  fn ord(&self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn doc_freq(&mut self) -> Result<i32> {
    Ok(IntersectTermsEnumFrame::decode_meta_data(self, self.current_frame)?.doc_freq)
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    Ok(IntersectTermsEnumFrame::decode_meta_data(self, self.current_frame)?.total_term_freq)
  }

  type PostingsEnum = PR::PostingsEnum;

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    IntersectTermsEnumFrame::decode_meta_data(self, self.current_frame)?;
    let current_frame = &self.stack[self.current_frame];
    let v = self
      .fr
      .parent
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("terms reader is missing"))?
      .postings_reader
      .postings(&self.fr.field_info, &current_frame.term_state, reuse, flags)?
      .ok_or_else(|| LuceneError::illegal_state("could not get postings enum"))?;
    Ok(v)
  }

  type ImpactsEnum = PR::ImpactsEnum;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    IntersectTermsEnumFrame::decode_meta_data(self, self.current_frame)?;
    let current_frame = &self.stack[self.current_frame];
    self
      .fr
      .parent
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("terms reader is missing"))?
      .postings_reader
      .impacts(&self.fr.field_info, &current_frame.term_state, flags)
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    IntersectTermsEnumFrame::decode_meta_data(self, self.current_frame)?;
    let current_frame = &self.stack[self.current_frame];
    Ok(current_frame.term_state.clone().into())
  }
}
