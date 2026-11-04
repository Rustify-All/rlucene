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
use crate::core::codecs::CodecUtil;
use crate::core::codecs::competitive_impact_accumulator::CompetitiveImpactAccumulator;
use crate::core::codecs::lucene101::for_delta_util::ForDeltaUtil;
use crate::core::codecs::lucene101::lucene101_postings_format::{
  IntBlockTermState, Lucene101PostingsFormat,
};
use crate::core::codecs::lucene101::pfor_util::PForUtil;
use crate::core::codecs::lucene101::postings_util::PostingsUtil;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::postings_writer_base::PostingsWriterBase;
use crate::core::codecs::push_postings_writer_base::{
  FieldWriteOptions, PushPostingsWriterBaseAbstract,
};
use crate::core::index::field_info::FieldInfo;

use crate::core::index::index_writer::MAX_POSITION;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::postings_enum::PostingsEnum;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::index::{BytesRef, IndexFileNames};
use crate::core::store::directory::Directory;
use crate::core::store::{ByteBuffersDataOutput, DataOutput, IndexOutput};
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::{IOUtils, TryIntoInt};
use std::borrow::Cow;
use std::sync::Arc;

/// Writer for
/// [`Lucene101PostingsFormat`](crate::core::codecs::lucene101::lucene101_postings_format)
pub struct Lucene101PostingsWriter<O> {
  pub(crate) meta_out: O,
  pub(crate) doc_out: O,
  pub(crate) pos_out: Option<O>,
  pub(crate) pay_out: Option<O>,
  closed: bool,
  pub(crate) last_state: IntBlockTermState,
  /// Holds starting file pointers for current term:
  doc_start_fp: i64,
  pos_start_fp: i64,
  pay_start_fp: i64,

  // Fixed contiguous [document deltas, frequencies] encoding blocks.
  pub(crate) doc_freq_buffers: Box<[[i32; Lucene101PostingsFormat::BLOCK_SIZE]; 2]>,
  doc_buffer_upto: usize,

  // Fixed contiguous position deltas and, when enabled, payload lengths.
  pub(crate) position_buffers: Vec<i32>,
  // Both fixed offset blocks exist exactly when field metadata enables offsets.
  pub(crate) offset_buffers: Option<Box<[[i32; Lucene101PostingsFormat::BLOCK_SIZE]; 2]>>,
  pos_buffer_upto: usize,

  payload_bytes: Vec<u8>,
  payload_byte_upto: usize,

  level0_last_doc_id: i32,
  level0_last_pos_fp: usize,
  level0_last_pay_fp: usize,

  level1_last_doc_id: i32,
  level1_last_pos_fp: i64,
  level1_last_pay_fp: i64,

  doc_id: i32,
  last_doc_id: i32,
  last_position: i32,
  last_start_offset: i32,
  doc_count: i32,

  pfor_util: PForUtil,
  for_delta_util: ForDeltaUtil,

  field_has_norms: bool,
  level0_freq_norm_accumulator: CompetitiveImpactAccumulator,
  level1_competitive_freq_norm_accumulator: CompetitiveImpactAccumulator,

  max_num_impacts_at_level0: i32,
  max_impact_num_bytes_at_level0: i32,
  max_num_impacts_at_level1: i32,
  max_impact_num_bytes_at_level1: i32,
  /// Scratch output that we use to be able to prepend the encoded length,
  /// e.g. impacts.
  scratch_output: ByteBuffersDataOutput,
  /// Output for a single block. This is useful to be able to prepend skip
  /// data before each block, which can only be computed once the block
  /// is encoded. The content is then typically copied to
  /// `level1Output`.
  level0_output: ByteBuffersDataOutput,
  /// Output for groups of 32 blocks. This is useful to prepend skip data for
  /// these 32 blocks, which can only be done once we have encoded these
  /// 32 blocks. The content is then typically copied to `docCount`.
  level1_output: ByteBuffersDataOutput,
}
impl<O> Lucene101PostingsWriter<O>
where
  O: IndexOutput,
{
  fn pos_out(&self) -> Result<&O> {
    self
      .pos_out
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("positions output is missing"))
  }

  fn pay_out(&self) -> Result<&O> {
    self
      .pay_out
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("payload output is missing"))
  }

  pub fn new<D1, D2>(state: &SegmentWriteState<D1>, segment_info: &SegmentInfo<D2>) -> Result<Self>
  where
    D1: Directory<IndexOutput = O>,
  {
    let meta_file = IndexFileNames::segment_file_name(
      &segment_info.name,
      &state.segment_suffix,
      Lucene101PostingsFormat::META_EXTENSION,
    );
    let doc_file = IndexFileNames::segment_file_name(
      &segment_info.name,
      &state.segment_suffix,
      Lucene101PostingsFormat::DOC_EXTENSION,
    );

    let mut meta_out = None;
    let mut doc_out = None;
    let mut pos_out: Option<O> = None;
    let mut pay_out: Option<O> = None;

    let mut position_buffers = Vec::new();
    let mut offset_buffers = None;
    let mut payload_bytes = Vec::new();

    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<_> {
      meta_out = Some(state.directory.create_output(&meta_file, state.context)?);
      doc_out = Some(state.directory.create_output(&doc_file, state.context)?);
      CodecUtil::write_index_header(
        meta_out
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("meta output is missing"))?,
        Lucene101PostingsFormat::META_CODEC,
        Lucene101PostingsFormat::VERSION_CURRENT,
        segment_info.get_id(),
        &state.segment_suffix,
      )?;
      CodecUtil::write_index_header(
        doc_out
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("doc output is missing"))?,
        Lucene101PostingsFormat::DOC_CODEC,
        Lucene101PostingsFormat::VERSION_CURRENT,
        segment_info.get_id(),
        &state.segment_suffix,
      )?;

      let for_delta_util = ForDeltaUtil::new();
      let pfor_util = PForUtil::new();

      if state.field_infos.has_prox() {
        position_buffers = vec![
          0;
          Lucene101PostingsFormat::BLOCK_SIZE
            * (1 + usize::from(state.field_infos.has_payloads()))
        ];
        let pos_file = IndexFileNames::segment_file_name(
          &segment_info.name,
          &state.segment_suffix,
          Lucene101PostingsFormat::POS_EXTENSION,
        );
        pos_out = Some(state.directory.create_output(&pos_file, state.context)?);
        CodecUtil::write_index_header(
          pos_out
            .as_mut()
            .ok_or_else(|| LuceneError::illegal_state("positions output is missing"))?,
          Lucene101PostingsFormat::POS_CODEC,
          Lucene101PostingsFormat::VERSION_CURRENT,
          segment_info.get_id(),
          &state.segment_suffix,
        )?;

        if state.field_infos.has_payloads() {
          payload_bytes = vec![0; 128];
        }

        if state.field_infos.has_offsets() {
          offset_buffers = Some(Box::new([[0; Lucene101PostingsFormat::BLOCK_SIZE]; 2]));
        }

        if state.field_infos.has_payloads() || state.field_infos.has_offsets() {
          let pay_file = IndexFileNames::segment_file_name(
            &segment_info.name,
            &state.segment_suffix,
            Lucene101PostingsFormat::PAY_EXTENSION,
          );
          pay_out = Some(state.directory.create_output(&pay_file, state.context)?);
          CodecUtil::write_index_header(
            pay_out
              .as_mut()
              .ok_or_else(|| LuceneError::illegal_state("payload output is missing"))?,
            Lucene101PostingsFormat::PAY_CODEC,
            Lucene101PostingsFormat::VERSION_CURRENT,
            segment_info.get_id(),
            &state.segment_suffix,
          )?;
        }
      }

      success = true;
      Ok((for_delta_util, pfor_util))
    }));

    if !success {
      IOUtils::close_while_handling_exception((
        meta_out.as_mut(),
        doc_out.as_mut(),
        pos_out.as_mut(),
        pay_out.as_mut(),
      ));
    }
    let (for_delta_util, pfor_util) = unwrap_caught_result!(result)?;

    let doc_freq_buffers = Box::new([[0; Lucene101PostingsFormat::BLOCK_SIZE]; 2]);
    let (meta_out, doc_out) = match (meta_out, doc_out) {
      (Some(meta_out), Some(doc_out)) => (meta_out, doc_out),
      (mut meta_out, mut doc_out) => {
        IOUtils::close_while_handling_exception((
          meta_out.as_mut(),
          doc_out.as_mut(),
          pos_out.as_mut(),
          pay_out.as_mut(),
        ));
        return Err(LuceneError::illegal_state(
          "postings outputs are missing after successful construction",
        ));
      },
    };

    Ok(Self {
      meta_out,
      doc_out,
      pos_out,
      pay_out,
      closed: false,
      last_state: IntBlockTermState::new(),
      doc_start_fp: 0,
      pos_start_fp: 0,
      pay_start_fp: 0,
      doc_freq_buffers,
      doc_buffer_upto: 0,
      position_buffers,
      offset_buffers,
      pos_buffer_upto: 0,
      payload_bytes,
      payload_byte_upto: 0,
      level0_last_doc_id: 0,
      level0_last_pos_fp: 0,
      level0_last_pay_fp: 0,
      level1_last_doc_id: 0,
      level1_last_pos_fp: 0,
      level1_last_pay_fp: 0,
      doc_id: 0,
      last_doc_id: 0,
      last_position: 0,
      last_start_offset: 0,
      doc_count: 0,
      pfor_util,
      for_delta_util,
      field_has_norms: false,
      level0_freq_norm_accumulator: CompetitiveImpactAccumulator::new(),
      level1_competitive_freq_norm_accumulator: CompetitiveImpactAccumulator::new(),
      max_num_impacts_at_level0: 0,
      max_impact_num_bytes_at_level0: 0,
      max_num_impacts_at_level1: 0,
      max_impact_num_bytes_at_level1: 0,
      scratch_output: ByteBuffersDataOutput::new_resettable_instance(),
      level0_output: ByteBuffersDataOutput::new_resettable_instance(),
      level1_output: ByteBuffersDataOutput::new_resettable_instance(),
    })
  }
  fn flush_doc_block(&mut self, finish_term: bool, options: &FieldWriteOptions) -> Result<()> {
    debug_assert!(self.doc_buffer_upto != 0);

    if self.doc_buffer_upto < Lucene101PostingsFormat::BLOCK_SIZE {
      debug_assert!(finish_term);
      let [doc_delta_buffer, freq_buffer] = self.doc_freq_buffers.as_mut();
      PostingsUtil::write_vint_block(
        &mut self.level0_output,
        doc_delta_buffer,
        freq_buffer,
        self.doc_buffer_upto,
        options.write_freqs,
      )?;
    } else {
      if options.write_freqs {
        let impacts = self
          .level0_freq_norm_accumulator
          .get_competitive_freq_norm_pairs();
        let n = impacts.len() as i32;
        if n > self.max_num_impacts_at_level0 {
          self.max_num_impacts_at_level0 = n;
        }
        write_impacts(&impacts, &mut self.scratch_output)?;
        debug_assert!(self.level0_output.size() == 0);
        let scratch_len = self.scratch_output.size();
        if scratch_len > self.max_impact_num_bytes_at_level0 as usize {
          self.max_impact_num_bytes_at_level0 = scratch_len.try_convert()?;
        }
        self.level0_output.write_vlong(scratch_len as i64)?;
        self.scratch_output.copy_to(&mut self.level0_output)?;
        self.scratch_output.reset();

        if options.write_positions {
          let pos_out = self.pos_out()?;
          let pos_fp = pos_out.get_file_pointer()?;
          self
            .level0_output
            .write_vlong((pos_fp - self.level0_last_pos_fp) as i64)?;
          self.level0_output.write_byte(self.pos_buffer_upto as u8)?;
          self.level0_last_pos_fp = pos_fp;

          if options.write_offsets || options.write_payloads {
            let pay_out = self.pay_out()?;
            let pay_fp = pay_out.get_file_pointer()?;
            self
              .level0_output
              .write_vlong((pay_fp - self.level0_last_pay_fp) as i64)?;
            self
              .level0_output
              .write_vint(self.payload_byte_upto as i32)?;
            self.level0_last_pay_fp = pay_fp;
          }
        }
      }

      let mut num_skip_bytes = self.level0_output.size();
      self
        .for_delta_util
        .encode_deltas(&mut self.doc_freq_buffers[0], &mut self.level0_output)?;
      if options.write_freqs {
        self
          .pfor_util
          .encode(&mut self.doc_freq_buffers[1], &mut self.level0_output)?;
      }
      // docID - lastBlockDocID is at least 128, so it can never fit a
      // single byte with a vint Even if we subtracted 128,
      // only extremely dense blocks would be eligible to a single byte
      // so let's go with 2 bytes right away
      write_vint15(
        &mut self.scratch_output,
        self.doc_id - self.level0_last_doc_id,
      )?;
      write_vlong15(&mut self.scratch_output, self.level0_output.size() as i64)?;
      num_skip_bytes += self.scratch_output.size();

      self.level1_output.write_vlong(num_skip_bytes as i64)?;
      self.scratch_output.copy_to(&mut self.level1_output)?;
      self.scratch_output.reset();
    }

    self.level0_output.copy_to(&mut self.level1_output)?;
    self.level0_output.reset();

    self.level0_last_doc_id = self.doc_id;
    if options.write_freqs {
      self
        .level1_competitive_freq_norm_accumulator
        .add_all(&self.level0_freq_norm_accumulator);
      self.level0_freq_norm_accumulator.clear();
    }

    if (self.doc_count & Lucene101PostingsFormat::LEVEL1_MASK) == 0 {
      // true every 32 blocks (4,096 docs)
      self.write_level1_skip_data(options)?;
      self.level1_last_doc_id = self.doc_id;
      self.level1_competitive_freq_norm_accumulator.clear();
    } else if finish_term {
      self.level1_output.copy_to(&mut self.doc_out)?;
      self.level1_output.reset();
      self.level1_competitive_freq_norm_accumulator.clear();
    }

    Ok(())
  }
  fn write_level1_skip_data(&mut self, options: &FieldWriteOptions) -> Result<()> {
    self
      .doc_out
      .write_vint(self.doc_id - self.level1_last_doc_id)?;
    let level1_end;

    if options.write_freqs {
      let impacts = self
        .level1_competitive_freq_norm_accumulator
        .get_competitive_freq_norm_pairs();
      let n = impacts.len() as i32;
      if n > self.max_num_impacts_at_level1 {
        self.max_num_impacts_at_level1 = n;
      }
      write_impacts(&impacts, &mut self.scratch_output)?;
      let num_impact_bytes = self.scratch_output.size();
      if num_impact_bytes > self.max_impact_num_bytes_at_level1 as usize {
        self.max_impact_num_bytes_at_level1 = num_impact_bytes.try_convert()?;
      }
      if options.write_positions {
        let pos_fp = self.pos_out()?.get_file_pointer()? as i64;
        self
          .scratch_output
          .write_vlong(pos_fp - self.level1_last_pos_fp)?;
        self.scratch_output.write_byte(self.pos_buffer_upto as u8)?;
        self.level1_last_pos_fp = pos_fp;
        if options.write_offsets || options.write_payloads {
          let pay_fp = self.pay_out()?.get_file_pointer()? as i64;
          self
            .scratch_output
            .write_vlong(pay_fp - self.level1_last_pay_fp)?;
          self
            .scratch_output
            .write_vint(self.payload_byte_upto as i32)?;
          self.level1_last_pay_fp = pay_fp;
        }
      }
      let level1_len =
        2 * BitUtil::SHORT_BYTES + self.scratch_output.size() + self.level1_output.size();
      self.doc_out.write_vlong(level1_len as i64)?;
      level1_end = self.doc_out.get_file_pointer()? + level1_len;
      // There are at most 128 impacts, that require at most 2 bytes each
      debug_assert!(self.scratch_output.size() <= i16::MAX as usize);
      // Like impacts plus a few vlongs, still way under the max short
      // value
      debug_assert!((self.scratch_output.size() + BitUtil::SHORT_BYTES) <= i16::MAX as usize);
      self
        .doc_out
        .write_short((self.scratch_output.size() + BitUtil::SHORT_BYTES) as i16)?;
      self.doc_out.write_short(num_impact_bytes as i16)?;
      self.scratch_output.copy_to(&mut self.doc_out)?;
      self.scratch_output.reset();
    } else {
      self.doc_out.write_vlong(self.level1_output.size() as i64)?;
      level1_end = self.doc_out.get_file_pointer()? + self.level1_output.size();
    }

    self.level1_output.copy_to(&mut self.doc_out)?;
    self.level1_output.reset();
    debug_assert_eq!(self.doc_out.get_file_pointer()?, level1_end);
    Ok(())
  }

  /// The term consumer fixes the common position mode before its loop.
  fn add_position_impl<const SIMPLE_POSITIONS: bool>(
    &mut self,
    position: i32,
    payload: Option<&BytesRef<&[u8]>>,
    start_offset: i32,
    end_offset: i32,
    options: &FieldWriteOptions,
  ) -> Result<()> {
    // As unsigned values, negative positions also exceed MAX_POSITION.
    if position as u32 > MAX_POSITION as u32 {
      return Err(self.invalid_position_error(position));
    }

    let idx = self.pos_buffer_upto;
    self.position_buffers[idx] = position - self.last_position;
    if !SIMPLE_POSITIONS && options.write_payloads {
      if let Some(p) = payload.as_ref() {
        if p.length == 0 {
          self.position_buffers[Lucene101PostingsFormat::BLOCK_SIZE + idx] = 0;
        } else {
          self.position_buffers[Lucene101PostingsFormat::BLOCK_SIZE + idx] = p.length as i32;
          if self.payload_byte_upto + p.length > self.payload_bytes.len() {
            ArrayUtil::grow_capacity(&mut self.payload_bytes, self.payload_byte_upto + p.length)?;
          }
          let start = p.offset;
          let payload = &p.bytes[start..start + p.length];
          self.payload_bytes.truncate(self.payload_byte_upto);
          self.payload_bytes.extend_from_slice(payload);
          self.payload_byte_upto += p.length;
        }
      } else {
        self.position_buffers[Lucene101PostingsFormat::BLOCK_SIZE + idx] = 0;
      }
    }

    if !SIMPLE_POSITIONS && options.write_offsets {
      debug_assert!(start_offset >= self.last_start_offset);
      debug_assert!(end_offset >= start_offset);
      let offset_buffers = expect_invariant!(
        self.offset_buffers.as_mut(),
        "offset blocks initialized from field metadata",
      );
      offset_buffers[0][idx] = start_offset - self.last_start_offset;
      offset_buffers[1][idx] = end_offset - start_offset;
      self.last_start_offset = start_offset;
    }

    self.pos_buffer_upto += 1;
    self.last_position = position;

    if self.pos_buffer_upto == Lucene101PostingsFormat::BLOCK_SIZE {
      let po = self
        .pos_out
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("positions output is missing"))?;
      self.pfor_util.encode(
        &mut self.position_buffers[..Lucene101PostingsFormat::BLOCK_SIZE],
        po,
      )?;
      if !SIMPLE_POSITIONS && options.write_payloads {
        let pay_out = self
          .pay_out
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("payload output is missing"))?;
        self.pfor_util.encode(
          &mut self.position_buffers[Lucene101PostingsFormat::BLOCK_SIZE..],
          pay_out,
        )?;
        pay_out.write_vint(self.payload_byte_upto as i32)?;
        pay_out.write_bytes_range(&self.payload_bytes, 0, self.payload_byte_upto)?;
        self.payload_byte_upto = 0;
      }
      if !SIMPLE_POSITIONS && options.write_offsets {
        let pay_out = self
          .pay_out
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("payload output is missing"))?;
        let offset_buffers = expect_invariant!(
          self.offset_buffers.as_mut(),
          "offset blocks initialized from field metadata",
        );
        self.pfor_util.encode(&mut offset_buffers[0], pay_out)?;
        self.pfor_util.encode(&mut offset_buffers[1], pay_out)?;
      }
      self.pos_buffer_upto = 0;
    }

    Ok(())
  }

  #[cold]
  #[inline(never)]
  fn invalid_position_error(&self, position: i32) -> LuceneError {
    let message = if position < 0 {
      format!("position={} is < 0  resource {}", position, self.doc_out)
    } else {
      format!(
        "position={} is too large (> IndexWriter.MAX_POSITION={})  resource {}",
        position, MAX_POSITION, self.doc_out
      )
    };
    LuceneError::corrupt_index(message)
  }
}
impl<O> PostingsWriterBase for Lucene101PostingsWriter<O>
where
  O: IndexOutput,
{
  type TermState = IntBlockTermState;

  fn init<D1, D2, IO>(
    &mut self,
    terms_out: &mut IO,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<()>
  where
    D1: Directory,
    IO: IndexOutput,
  {
    CodecUtil::write_index_header(
      terms_out,
      Lucene101PostingsFormat::TERMS_CODEC,
      Lucene101PostingsFormat::VERSION_CURRENT,
      segment_info.get_id(),
      &state.segment_suffix,
    )?;
    terms_out.write_vint(Lucene101PostingsFormat::BLOCK_SIZE as i32)?;
    Ok(())
  }

  fn write_term<N, PE, T>(
    &mut self,
    _term: &BytesRef<Vec<u8>>,
    _terms_enum: &mut T,
    _docs_seen: &mut FixedBitSet,
    _norms: Option<&N>,
    _postings_enum: Option<PE>,
  ) -> Result<(Option<PE>, Option<Self::TermState>)>
  where
    N: NormsProducer,
    PE: PostingsEnum,
    T: TermsEnum<PostingsEnum = PE>,
  {
    Err(LuceneError::not_implemented(""))
  }

  fn encode_term<DO>(
    &mut self,
    _out: &mut DO,
    _field_info: &FieldInfo,
    _state: Cow<Self::TermState>,
    _absolute: bool,
  ) -> Result<()>
  where
    DO: DataOutput,
  {
    Err(LuceneError::unreachable("should not be called"))
  }

  fn set_field(&mut self, field_info: &Arc<FieldInfo>) {
    self.last_state = IntBlockTermState::new();
    self.field_has_norms = field_info.has_norms();
  }
}

impl<O> Closeable for Lucene101PostingsWriter<O>
where
  O: IndexOutput,
{
  fn close(&mut self) -> Result<()> {
    if self.closed {
      return Ok(());
    }

    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      CodecUtil::write_footer(&mut self.doc_out)?;
      if let Some(ref mut po) = self.pos_out {
        CodecUtil::write_footer(po)?;
      }
      if let Some(ref mut pay) = self.pay_out {
        CodecUtil::write_footer(pay)?;
      }
      self.meta_out.write_int(self.max_num_impacts_at_level0)?;
      self
        .meta_out
        .write_int(self.max_impact_num_bytes_at_level0)?;
      self.meta_out.write_int(self.max_num_impacts_at_level1)?;
      self
        .meta_out
        .write_int(self.max_impact_num_bytes_at_level1)?;
      self
        .meta_out
        .write_long(self.doc_out.get_file_pointer()? as i64)?;
      if let Some(ref po) = self.pos_out {
        self.meta_out.write_long(po.get_file_pointer()? as i64)?;
        if let Some(ref pay) = self.pay_out {
          self.meta_out.write_long(pay.get_file_pointer()? as i64)?;
        }
      }
      CodecUtil::write_footer(&mut self.meta_out)?;
      success = true;
      Ok(())
    }));
    if success {
      IOUtils::close((
        &mut self.meta_out,
        &mut self.doc_out,
        self.pos_out.as_mut(),
        self.pay_out.as_mut(),
      ))?;
    } else {
      IOUtils::close_while_handling_exception((
        &mut self.meta_out,
        &mut self.doc_out,
        self.pos_out.as_mut(),
        self.pay_out.as_mut(),
      ));
    }
    self.pos_out = None;
    self.pay_out = None;
    self.closed = true;
    unwrap_caught_result!(result)
  }
}
impl<O> PushPostingsWriterBaseAbstract for Lucene101PostingsWriter<O>
where
  O: IndexOutput,
{
  fn new_term_state(&mut self) -> Result<Self::TermState> {
    Ok(IntBlockTermState::new())
  }

  fn start_term(&mut self, options: &FieldWriteOptions) -> Result<()> {
    self.doc_start_fp = self.doc_out.get_file_pointer()? as i64;
    if options.write_positions
      && let Some(ref pos_out) = self.pos_out
    {
      let pos_fp = pos_out.get_file_pointer()?;
      self.pos_start_fp = pos_fp as i64;
      self.level0_last_pos_fp = pos_fp;
      self.level1_last_pos_fp = self.pos_start_fp;
      if options.write_payloads || options.write_offsets {
        let pay_fp = self.pay_out()?.get_file_pointer()?;
        self.pay_start_fp = pay_fp as i64;
        self.level0_last_pay_fp = pay_fp;
        self.level1_last_pay_fp = self.pay_start_fp;
      }
    }
    self.last_doc_id = -1;
    self.level0_last_doc_id = -1;
    self.level1_last_doc_id = -1;
    if options.write_freqs {
      self.level0_freq_norm_accumulator.clear();
    }
    Ok(())
  }

  fn finish_term(
    &mut self,
    state: &mut Self::TermState,
    options: &FieldWriteOptions,
  ) -> Result<()> {
    debug_assert!(state.base.doc_freq > 0);
    debug_assert_eq!(state.base.doc_freq, self.doc_count);

    let singleton_doc_id = if state.base.doc_freq == 1 {
      self.doc_freq_buffers[0][0] - 1
    } else {
      self.flush_doc_block(true, options)?;
      -1
    };

    let last_pos_block_offset = if options.write_positions {
      // totalTermFreq is just total number of positions(or payloads, or
      // offsets) associated with current term.
      debug_assert!(state.base.total_term_freq != -1);
      let offset = if state.base.total_term_freq > Lucene101PostingsFormat::BLOCK_SIZE as i64 {
        // record file offset for last pos in last block
        self.pos_out()?.get_file_pointer()? as i64 - self.pos_start_fp
      } else {
        -1
      };
      if self.pos_buffer_upto > 0 {
        debug_assert!(self.pos_buffer_upto < Lucene101PostingsFormat::BLOCK_SIZE);
        let mut last_payload_length = -1;
        let mut last_offset_length = -1;
        let mut payload_bytes_read_upto: usize = 0;
        let po_out = self
          .pos_out
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("positions output is missing"))?;
        for (i, &pos_delta) in self.position_buffers[..self.pos_buffer_upto]
          .iter()
          .enumerate()
        {
          if options.write_payloads {
            let payload_length = self.position_buffers[Lucene101PostingsFormat::BLOCK_SIZE + i];
            if payload_length != last_payload_length {
              last_payload_length = payload_length;
              po_out.write_vint((pos_delta << 1) | 1)?;
              po_out.write_vint(payload_length)?;
            } else {
              po_out.write_vint(pos_delta << 1)?;
            }
            if payload_length != 0 {
              let payload_length = payload_length as usize;
              po_out.write_bytes_range(
                &self.payload_bytes,
                payload_bytes_read_upto,
                payload_length,
              )?;
              payload_bytes_read_upto += payload_length;
            }
          } else {
            po_out.write_vint(pos_delta)?;
          }
          if options.write_offsets {
            let offset_buffers = expect_invariant!(
              self.offset_buffers.as_ref(),
              "offset blocks initialized from field metadata",
            );
            let delta = offset_buffers[0][i];
            let length = offset_buffers[1][i];
            if length == last_offset_length {
              po_out.write_vint(delta << 1)?;
            } else {
              po_out.write_vint((delta << 1) | 1)?;
              po_out.write_vint(length)?;
              last_offset_length = length;
            }
          }
        }
        if options.write_payloads {
          debug_assert_eq!(payload_bytes_read_upto, self.payload_byte_upto);
          self.payload_byte_upto = 0;
        }
      }
      offset
    } else {
      -1
    };

    state.doc_start_fp = self.doc_start_fp;
    state.pos_start_fp = self.pos_start_fp;
    state.pay_start_fp = self.pay_start_fp;
    state.singleton_doc_id = singleton_doc_id;
    state.last_pos_block_offset = last_pos_block_offset;

    self.doc_buffer_upto = 0;
    self.pos_buffer_upto = 0;
    self.last_doc_id = -1;
    self.doc_count = 0;
    Ok(())
  }

  fn start_doc<NDV>(
    &mut self,
    norms: Option<&mut NDV>,
    doc_id: i32,
    term_doc_freq: i32,
    options: &FieldWriteOptions,
  ) -> Result<()>
  where
    NDV: NumericDocValues,
  {
    if self.doc_buffer_upto == Lucene101PostingsFormat::BLOCK_SIZE {
      self.flush_doc_block(false, options)?;
      self.doc_buffer_upto = 0;
    }

    let doc_delta = doc_id - self.last_doc_id;
    if doc_id < 0 || doc_delta <= 0 {
      return Err(LuceneError::corrupt_index(format!(
        "docs out of order ({} <= {}) resource {}",
        doc_id, self.last_doc_id, self.doc_out
      )));
    }

    let idx = self.doc_buffer_upto;
    self.doc_freq_buffers[0][idx] = doc_delta;
    if options.write_freqs {
      self.doc_freq_buffers[1][idx] = term_doc_freq;
    }

    self.doc_id = doc_id;
    self.last_position = 0;
    self.last_start_offset = 0;

    if options.write_freqs {
      let norm = if self.field_has_norms {
        let norms = norms.ok_or_else(|| LuceneError::illegal_state("norms should not be None"))?;
        let found = norms.advance_exact(doc_id)?;
        if !found {
          1
        } else {
          let n = norms.long_value()?;
          debug_assert!(n != 0, "norm for doc {doc_id} is zero");
          n
        }
      } else {
        1
      };
      self.level0_freq_norm_accumulator.add(term_doc_freq, norm);
    }

    Ok(())
  }

  fn add_position(
    &mut self,
    position: i32,
    payload: Option<&BytesRef<&[u8]>>,
    start_offset: i32,
    end_offset: i32,
    options: &FieldWriteOptions,
  ) -> Result<()> {
    self.add_position_impl::<false>(position, payload, start_offset, end_offset, options)
  }

  fn add_position_without_payloads_or_offsets(
    &mut self,
    position: i32,
    options: &FieldWriteOptions,
  ) -> Result<()> {
    debug_assert!(!options.write_payloads && !options.write_offsets);
    self.add_position_impl::<true>(position, None, -1, -1, options)
  }

  fn finish_doc(&mut self) -> Result<()> {
    self.doc_buffer_upto += 1;
    self.doc_count += 1;
    self.last_doc_id = self.doc_id;
    Ok(())
  }

  fn encode_term_with_option<DO>(
    &mut self,
    out: &mut DO,
    _field_info: &FieldInfo,
    state: Cow<Self::TermState>,
    absolute: bool,
    options: &FieldWriteOptions,
  ) -> Result<()>
  where
    DO: DataOutput,
  {
    if absolute {
      self.last_state = IntBlockTermState::new();
      debug_assert_eq!(self.last_state.doc_start_fp, 0);
    }

    if self.last_state.singleton_doc_id != -1
      && state.singleton_doc_id != -1
      && state.doc_start_fp == self.last_state.doc_start_fp
    {
      // With runs of rare values such as ID fields, the increment of
      // pointers in the docs file is often 0.
      // Furthermore some ID schemes like auto-increment IDs or Flake IDs
      // are monotonic, so we encode the delta
      // between consecutive doc IDs to save space.
      let delta = (state.singleton_doc_id - self.last_state.singleton_doc_id) as i64;
      out.write_vlong((BitUtil::zig_zag_encode_i64(delta) << 1) | 1)?;
    } else {
      out.write_vlong((state.doc_start_fp - self.last_state.doc_start_fp) << 1)?;
      if state.singleton_doc_id != -1 {
        out.write_vint(state.singleton_doc_id)?;
      }
    }

    if options.write_positions {
      out.write_vlong(state.pos_start_fp - self.last_state.pos_start_fp)?;
      if options.write_payloads || options.write_offsets {
        out.write_vlong(state.pay_start_fp - self.last_state.pay_start_fp)?;
      }
      if state.last_pos_block_offset != -1 {
        out.write_vlong(state.last_pos_block_offset)?;
      }
    }

    self.last_state = state.into_owned();
    Ok(())
  }
}

use crate::core::index::impact::Impact;

/// Special vints that are encoded on 2 bytes if they require 15 bits or
/// less. VInt becomes especially slow when the number of bytes is
/// variable, so this special layout helps in the case when the number
/// likely requires 15 bits or less.
pub(crate) fn write_vint15<DO>(out: &mut DO, v: i32) -> Result<()>
where
  DO: DataOutput,
{
  debug_assert!(v >= 0);
  write_vlong15(out, v as i64)
}

/// See also [`write_vint15`].
pub(crate) fn write_vlong15<DO>(out: &mut DO, v: i64) -> Result<()>
where
  DO: DataOutput,
{
  debug_assert!(v >= 0);
  if v & !0x7FFF == 0 {
    out.write_short(v as i16)?;
  } else {
    let prefix = 0x8000 | (v & 0x7FFF);
    out.write_short(prefix as i16)?;
    out.write_vlong(v >> 15)?;
  }
  Ok(())
}
pub(crate) fn write_impacts<DO>(impacts: &[Impact], out: &mut DO) -> Result<()>
where
  DO: DataOutput,
{
  let mut previous = Impact { freq: 0, norm: 0 };
  for impact in impacts {
    debug_assert!(impact.freq > previous.freq);
    debug_assert!((impact.norm as u64) > (previous.norm as u64));
    let freq_delta = impact.freq - previous.freq - 1;
    let norm_delta = impact.norm.wrapping_sub(previous.norm).wrapping_sub(1);
    if norm_delta == 0 {
      // most of time, norm only increases by 1, so we can fold
      // everything in a single byte
      out.write_vint(freq_delta << 1)?;
    } else {
      out.write_vint((freq_delta << 1) | 1)?;
      out.write_zlong(norm_delta)?;
    }
    previous = impact.clone();
  }
  Ok(())
}
