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
use crate::core::codecs::block_term_state::TermStateEnum;
use crate::core::codecs::lucene101::for_delta_util::ForDeltaUtil;
use crate::core::codecs::lucene101::for_util::ForUtil;
use crate::core::codecs::lucene101::lucene101_postings_format::{
  IntBlockTermState, Lucene101PostingsFormat,
};
use crate::core::codecs::lucene101::pfor_util::PForUtil;
use crate::core::codecs::lucene101::postings_util::PostingsUtil;
use crate::core::codecs::postings_reader_base::PostingsReaderBase;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::impact::Impact;
use crate::core::index::impacts::Impacts;
use crate::core::index::impacts_enum::ImpactsEnum;
use crate::core::index::impacts_source::ImpactsSource;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::postings_enum::{
  FREQS, OFFSETS, PAYLOADS, POSITIONS, PostingsEnum, feature_requested,
};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::{BytesRef, IndexFileNames};
use crate::core::internal::vectorization::default_vectorization_provider::DefaultVectorizationProvider;
use crate::core::internal::vectorization::posting_decoding_util::PostingDecodingUtil;
use crate::core::internal::vectorization::vectorization_provider::{
  DEFAULT_VECTORIZATION_PROVIDER, VectorizationProvider,
};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::store::directory::Directory;
use crate::core::store::{ByteArrayDataInput, DataInput, IndexInput, ReadAdvice};
use crate::core::util::TryIntoInt;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::io_utils::IOUtils;
use crate::core::util::vector_util::VECTOR_UTIL;
use std::borrow::Cow;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::{fmt, ptr};

pub struct Lucene101PostingsReader<I>
where
  I: IndexInput,
{
  doc_in: I,
  pos_in: Option<I>,
  pay_in: Option<I>,
  max_num_impacts_at_level0: i32,
  max_impact_num_bytes_at_level0: i32,
  max_num_impacts_at_level1: i32,
  max_impact_num_bytes_at_level1: i32,
  vectorization_provider: DefaultVectorizationProvider,
}
impl<I> Lucene101PostingsReader<I>
where
  I: IndexInput,
{
  pub fn new<D1, D2>(state: &SegmentReadState<D1>, segment_info: &SegmentInfo<D2>) -> Result<Self>
  where
    D1: Directory<IndexInput = I>,
    D2: Directory,
  {
    let meta_name = IndexFileNames::segment_file_name(
      &segment_info.name,
      &state.segment_suffix,
      Lucene101PostingsFormat::META_EXTENSION,
    );
    let mut max_num_impacts_at_level0 = 0;
    let mut max_impact_num_bytes_at_level0 = 0;
    let mut max_num_impacts_at_level1 = 0;
    let mut max_impact_num_bytes_at_level1 = 0;
    let mut expected_doc_file_length = 0;
    let mut version = 0;
    let mut expected_pos_file_length = 0;
    let mut expected_pay_file_length = 0;
    let mut meta_in_opt = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      meta_in_opt = Some(state.directory.open_checksum_input(&meta_name)?);
      if let Some(ref mut meta_in) = meta_in_opt {
        version = CodecUtil::check_index_header(
          meta_in,
          Lucene101PostingsFormat::META_CODEC,
          Lucene101PostingsFormat::VERSION_START,
          Lucene101PostingsFormat::VERSION_CURRENT,
          segment_info.get_id(),
          &state.segment_suffix,
        )?;
        max_num_impacts_at_level0 = meta_in.read_int()?;
        max_impact_num_bytes_at_level0 = meta_in.read_int()?;
        max_num_impacts_at_level1 = meta_in.read_int()?;
        max_impact_num_bytes_at_level1 = meta_in.read_int()?;
        expected_doc_file_length = meta_in.read_long()?.try_convert()?;
        (expected_pos_file_length, expected_pay_file_length) = if state.field_infos.has_prox() {
          let pos_len = meta_in.read_long()?;
          let pay_len = if state.field_infos.has_payloads() || state.field_infos.has_offsets() {
            meta_in.read_long()?
          } else {
            -1
          };
          (pos_len, pay_len)
        } else {
          (-1, -1)
        };
        CodecUtil::check_footer(meta_in)?;
      }
      Ok(())
    }));
    match result {
      Ok(Ok(())) => {
        meta_in_opt
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("postings metadata input is missing"))?
          .close()?;
      },
      result => {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
          match result {
            Ok(Err(error)) => match meta_in_opt.as_mut() {
              Some(meta_in) => Err(CodecUtil::check_footer_with_error(meta_in, error)),
              None => Err(error),
            },
            Err(payload) => {
              if let Some(meta_in) = meta_in_opt.as_mut() {
                let error = LuceneError::tragedy_from_panic(
                  "panic while reading postings metadata",
                  payload.as_ref(),
                );
                if let error @ LuceneError::CorruptIndex(_) =
                  CodecUtil::check_footer_with_error(meta_in, error)
                {
                  return Err(error);
                }
              }
              std::panic::resume_unwind(payload)
            },
            Ok(Ok(())) => unreachable!(),
          }
        }));
        IOUtils::close_resources_while_handling_error(meta_in_opt.as_ref())?;
        match result {
          Ok(result) => result?,
          Err(payload) => std::panic::resume_unwind(payload),
        }
      },
    }
    // NOTE: these data files are too costly to verify checksum against all
    // the bytes on open, but for now we at least verify proper
    // structure of the checksum footer: which looks
    // for FOOTER_MAGIC + algorithmID. This is cheap and can detect some
    // forms of corruption such as file truncation.
    let doc_name = IndexFileNames::segment_file_name(
      &segment_info.name,
      &state.segment_suffix,
      Lucene101PostingsFormat::DOC_EXTENSION,
    );
    // Postings have a forward-only access pattern, so pass
    // ReadAdvice.NORMAL to perform readahead.
    let mut doc_in_opt = None;
    let mut pos_in_opt: Option<I> = None;
    let mut pay_in_opt: Option<I> = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Self> {
      doc_in_opt = Some(state.directory.open_input(
        &doc_name,
        &state.context.with_read_advice_self(ReadAdvice::Normal)?,
      )?);
      let doc_in = doc_in_opt
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("postings docs input is missing"))?;
      CodecUtil::check_index_header(
        doc_in,
        Lucene101PostingsFormat::DOC_CODEC,
        version,
        version,
        segment_info.get_id(),
        &state.segment_suffix,
      )?;
      CodecUtil::retrieve_checksum_with_expected(doc_in, expected_doc_file_length)?;

      if state.field_infos.has_prox() {
        let pos_name = IndexFileNames::segment_file_name(
          &segment_info.name,
          &state.segment_suffix,
          Lucene101PostingsFormat::POS_EXTENSION,
        );
        pos_in_opt = Some(state.directory.open_input(&pos_name, state.context)?);
        let pos_in = pos_in_opt
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("postings positions input is missing"))?;
        CodecUtil::check_index_header(
          pos_in,
          Lucene101PostingsFormat::POS_CODEC,
          version,
          version,
          segment_info.get_id(),
          &state.segment_suffix,
        )?;
        CodecUtil::retrieve_checksum_with_expected(pos_in, expected_pos_file_length as usize)?;

        if state.field_infos.has_payloads() || state.field_infos.has_offsets() {
          let pay_name = IndexFileNames::segment_file_name(
            &segment_info.name,
            &state.segment_suffix,
            Lucene101PostingsFormat::PAY_EXTENSION,
          );
          pay_in_opt = Some(state.directory.open_input(&pay_name, state.context)?);
          let pay_in = pay_in_opt
            .as_mut()
            .ok_or_else(|| LuceneError::illegal_state("postings payloads input is missing"))?;
          CodecUtil::check_index_header(
            pay_in,
            Lucene101PostingsFormat::PAY_CODEC,
            version,
            version,
            segment_info.get_id(),
            &state.segment_suffix,
          )?;
          CodecUtil::retrieve_checksum_with_expected(pay_in, expected_pay_file_length as usize)?;
        }
      }

      Ok(Self {
        doc_in: doc_in_opt
          .take()
          .ok_or_else(|| LuceneError::illegal_state("postings docs input is missing"))?,
        pos_in: pos_in_opt.take(),
        pay_in: pay_in_opt.take(),
        max_num_impacts_at_level0,
        max_impact_num_bytes_at_level0,
        max_num_impacts_at_level1,
        max_impact_num_bytes_at_level1,
        vectorization_provider: DefaultVectorizationProvider,
      })
    }));
    match result {
      Ok(result @ Ok(_)) => result,
      result => {
        IOUtils::close_resources_while_handling_error((
          doc_in_opt.as_ref(),
          pos_in_opt.as_ref(),
          pay_in_opt.as_ref(),
        ))?;
        match result {
          Ok(result) => result,
          Err(payload) => std::panic::resume_unwind(payload),
        }
      },
    }
  }
}

impl<I> Display for Lucene101PostingsReader<I>
where
  I: IndexInput,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "Lucene101PostingsReader(positions={},payloads={})",
      self.pos_in.is_some(),
      self.pay_in.is_some()
    )
  }
}

impl<I> PostingsReaderBase for Lucene101PostingsReader<I>
where
  I: IndexInput,
{
  fn init<D1, D2>(
    &self,
    terms_in: &mut impl IndexInput,
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<()>
  where
    D1: Directory,
    D2: Directory,
  {
    // Make sure we are talking to the matching postings writer
    CodecUtil::check_index_header(
      terms_in,
      Lucene101PostingsFormat::TERMS_CODEC,
      Lucene101PostingsFormat::VERSION_START,
      Lucene101PostingsFormat::VERSION_CURRENT,
      segment_info.get_id(),
      &state.segment_suffix,
    )?;
    let index_block_size = terms_in.read_vint()?;
    if index_block_size as usize != ForUtil::BLOCK_SIZE {
      return Err(LuceneError::illegal_state(format!(
        "index-time BLOCK_SIZE ({}) != read-time BLOCK_SIZE ({})",
        index_block_size,
        ForUtil::BLOCK_SIZE
      )));
    }

    Ok(())
  }

  fn new_term_state(&self) -> Result<TermStateEnum> {
    Ok(TermStateEnum::Int(IntBlockTermState::new()))
  }

  fn decode_term(
    &self,
    input: &mut impl DataInput,
    field_info: &Arc<FieldInfo>,
    term_state: &mut TermStateEnum,
    absolute: bool,
  ) -> Result<()> {
    let term_state = match term_state {
      TermStateEnum::Int(s) => s,
      _ => {
        return Err(LuceneError::illegal_state(
          "term_state should be IntBlockTermState",
        ));
      },
    };
    if absolute {
      term_state.doc_start_fp = 0;
      term_state.pos_start_fp = 0;
      term_state.pay_start_fp = 0;
    }

    let l = input.read_vlong()?;
    if (l & 0x1) == 0 {
      term_state.doc_start_fp += ((l as u64) >> 1) as i64;
      if term_state.base.doc_freq == 1 {
        term_state.singleton_doc_id = input.read_vint()?;
      } else {
        term_state.singleton_doc_id = -1;
      }
    } else {
      debug_assert!(!absolute);
      debug_assert_ne!(term_state.singleton_doc_id, -1);
      let delta = BitUtil::zig_zag_decode_i64((l as u64) >> 1);
      term_state.singleton_doc_id += delta as i32;
    }

    if *field_info.get_index_options() >= IndexOptions::DocsAndFreqsAndPositions {
      term_state.pos_start_fp += input.read_vlong()?;

      if *field_info.get_index_options() >= IndexOptions::DocsAndFreqsAndPositionsAndOffsets
        || field_info.has_payloads()
      {
        term_state.pay_start_fp += input.read_vlong()?;
      }
      if term_state.base.total_term_freq > ForUtil::BLOCK_SIZE as i64 {
        term_state.last_pos_block_offset = input.read_vlong()?;
      } else {
        term_state.last_pos_block_offset = -1;
      }
    }
    Ok(())
  }

  type PostingsEnum = BlockPostingsEnum<I>;

  fn postings(
    &self,
    field_info: &FieldInfo,
    term_state: &TermStateEnum,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Option<Self::PostingsEnum>> {
    if let Some(mut e) = reuse
      && e.can_reuse(&self.doc_in, field_info, flags, false, self)
    {
      e.reset(term_state, flags, self)?;
      return Ok(Some(e));
    }

    let mut block = BlockPostingsEnum::new(field_info, flags, false, self)?;
    block.reset(term_state, flags, self)?;
    Ok(Some(block))
  }

  type ImpactsEnum = BlockPostingsEnum<I>;

  fn impacts(
    &self,
    field_info: &FieldInfo,
    term_state: &TermStateEnum,
    flags: i32,
  ) -> Result<Self::ImpactsEnum> {
    let mut block = BlockPostingsEnum::new(field_info, flags, true, self)?;
    block.reset(term_state, flags, self)?;
    Ok(block)
  }

  fn check_integrity(&self) -> Result<()> {
    CodecUtil::checksum_entire_file(&self.doc_in)?;
    if let Some(ref pos_in) = self.pos_in {
      CodecUtil::checksum_entire_file(pos_in)?;
    }
    if let Some(ref pay_in) = self.pay_in {
      CodecUtil::checksum_entire_file(pay_in)?;
    }
    Ok(())
  }
}

impl<I> CloseableRef for Lucene101PostingsReader<I>
where
  I: IndexInput,
{
  fn close(&self) -> Result<()> {
    IOUtils::close_refs(
      [
        Some(&self.doc_in),
        self.pos_in.as_ref(),
        self.pay_in.as_ref(),
      ]
      .into_iter()
      .flatten(),
    )
  }
}

pub struct BlockPostingsEnum<I>
where
  I: IndexInput,
{
  for_delta_util: Option<ForDeltaUtil>,
  pfor_util: Option<PForUtil>,

  doc_buffer: [i32; ForUtil::BLOCK_SIZE],
  doc: i32,

  level0_last_doc_id: i32,
  level0_doc_end_fp: i64,

  level1_last_doc_id: i32,
  level1_doc_end_fp: i64,
  level1_doc_count_upto: i32,
  /// number of docs in this posting list
  doc_freq: i32,
  /// sum of freqBuffer in this posting list (or docFreq when omitted)
  total_term_freq: i64,
  /// docid when there is a single pulsed posting, otherwise -1
  singleton_doc_id: i32,
  /// number of remaining docs in this postings list
  doc_count_left: i32,
  /// last doc ID of the previous block
  prev_doc_id: i32,

  doc_buffer_size: i32,
  doc_buffer_upto: i32,

  doc_in_util: Option<PostingDecodingUtil<I>>,

  freq_buffer: [i32; ForUtil::BLOCK_SIZE],
  pos_delta_buffer: Vec<i32>,
  payload_length_buffer: Vec<i32>,
  payload_bytes: Vec<u8>,
  offset_start_delta_buffer: Vec<i32>,
  offset_length_buffer: Vec<i32>,
  payload_byte_upto: i32,
  payload_length: i32,

  last_start_offset: i32,
  start_offset: i32,
  end_offset: i32,

  pos_buffer_upto: i32,

  pub(crate) pos_in_util: Option<PostingDecodingUtil<I>>,
  pub(crate) pay_in_util: Option<PostingDecodingUtil<I>>,
  pub(crate) payload: Option<BytesRef<Vec<u8>>>,

  pub(crate) options: IndexOptions,
  pub(crate) index_has_freq: bool,
  pub(crate) index_has_pos: bool,
  pub(crate) index_has_offsets: bool,
  pub(crate) index_has_payloads: bool,
  pub(crate) index_has_offsets_or_payloads: bool,

  pub(crate) flags: i32,
  pub(crate) needs_freq: bool,
  pub(crate) needs_pos: bool,
  pub(crate) needs_offsets: bool,
  pub(crate) needs_payloads: bool,
  pub(crate) needs_offsets_or_payloads: bool,
  pub(crate) needs_impacts: bool,
  pub(crate) needs_docs_and_freqs_only: bool,

  /// offset of the freq block
  freq_fp: i64,
  /// current position
  position: i32,
  /// value of docBufferUpto on the last doc ID when positions have been read
  pos_doc_buffer_upto: i32,
  /// how many positions "behind" we are; nextPosition must
  /// skip these to "catch up":
  pos_pending_count: i32,
  /// File pointer where the last (vInt encoded) pos delta
  /// block is.  We need this to know whether to bulk
  /// decode vs vInt decode the block:
  last_pos_block_fp: i64,
  /// level 0 skip data
  level0_pos_end_fp: i64,
  level0_block_pos_upto: i32,
  level0_pay_end_fp: i64,
  level0_block_pay_upto: i32,
  level0_serialized_impacts: Option<BytesRef<Vec<u8>>>,
  #[allow(dead_code)]
  level0_impacts: Option<MutableImpactList>,
  /// level 1 skip data
  level1_pos_end_fp: i64,
  level1_block_pos_upto: i32,
  level1_pay_end_fp: i64,
  level1_block_pay_upto: i32,
  level1_serialized_impacts: Option<BytesRef<Vec<u8>>>,
  #[allow(dead_code)]
  level1_impacts: Option<MutableImpactList>,
  // true if we shallow-advanced to a new block that we have not decoded yet
  needs_refilling: bool,

  max_num_impacts_at_level0: i32,
  max_num_impacts_at_level1: i32,
  max_impact_num_bytes_at_level0: i32,
  max_impact_num_bytes_at_level1: i32,
}

impl<I> BlockPostingsEnum<I>
where
  I: IndexInput,
{
  pub fn new(
    field_info: &FieldInfo,
    flags: i32,
    needs_impacts: bool,
    reader: &Lucene101PostingsReader<I>,
  ) -> Result<Self> {
    let options = *field_info.get_index_options();
    let index_has_freq = options >= IndexOptions::DocsAndFreqs;
    let index_has_pos = options >= IndexOptions::DocsAndFreqsAndPositions;
    let index_has_offsets = options >= IndexOptions::DocsAndFreqsAndPositionsAndOffsets;
    let index_has_payloads = field_info.has_payloads();
    let index_has_offsets_or_payloads = index_has_offsets || index_has_payloads;

    let needs_freq = index_has_freq && feature_requested(flags, FREQS);
    let needs_pos = index_has_pos && feature_requested(flags, POSITIONS);
    let needs_offsets = index_has_offsets && feature_requested(flags, OFFSETS);
    let needs_payloads = index_has_payloads && feature_requested(flags, PAYLOADS);
    let needs_offsets_or_payloads = needs_offsets || needs_payloads;
    let needs_docs_and_freqs_only = !needs_pos && !needs_impacts;

    let freq_buffer = if needs_freq {
      [0; ForUtil::BLOCK_SIZE]
    } else {
      [1; ForUtil::BLOCK_SIZE]
    };

    let (level0_serialized_impacts, level1_serialized_impacts, level0_impacts, level1_impacts) =
      if needs_freq && needs_impacts {
        (
          Some(BytesRef::with_capacity(
            reader.max_impact_num_bytes_at_level0 as usize,
          )?),
          Some(BytesRef::with_capacity(
            reader.max_impact_num_bytes_at_level1 as usize,
          )?),
          MutableImpactList::with_capacity(reader.max_num_impacts_at_level0.try_convert()?),
          MutableImpactList::with_capacity(reader.max_num_impacts_at_level1.try_convert()?),
        )
      } else {
        (
          None,
          None,
          MutableImpactList::default(),
          MutableImpactList::default(),
        )
      };

    let (pos_in_util, pos_delta_buffer) = if needs_pos {
      let pi = reader.pos_in.as_ref().unwrap().try_clone()?;
      let util = DEFAULT_VECTORIZATION_PROVIDER.new_posting_decoding_util(pi);
      (Some(util), vec![0; ForUtil::BLOCK_SIZE])
    } else {
      (None, vec![])
    };

    let pay_in_util = if needs_offsets_or_payloads {
      let pi = reader.pay_in.as_ref().unwrap().try_clone()?;
      let util = DEFAULT_VECTORIZATION_PROVIDER.new_posting_decoding_util(pi);
      Some(util)
    } else {
      None
    };

    let (offset_start_delta_buffer, offset_length_buffer, start_offset, end_offset) =
      if needs_offsets {
        (
          vec![0; ForUtil::BLOCK_SIZE],
          vec![0; ForUtil::BLOCK_SIZE],
          0,
          0,
        )
      } else {
        (vec![], vec![], -1, -1)
      };

    let (payload_length_buffer, payload_bytes, payload) = if index_has_payloads {
      (
        vec![0; ForUtil::BLOCK_SIZE],
        vec![0; 128],
        Some(BytesRef::new()),
      )
    } else {
      (vec![], vec![], None)
    };

    Ok(BlockPostingsEnum {
      for_delta_util: None,
      pfor_util: None,
      doc_buffer: [0; ForUtil::BLOCK_SIZE],
      doc: -1,
      level0_last_doc_id: 0,
      level0_doc_end_fp: 0,
      level1_last_doc_id: 0,
      level1_doc_end_fp: 0,
      level1_doc_count_upto: 0,
      doc_freq: 0,
      total_term_freq: 0,
      singleton_doc_id: 0,
      doc_count_left: 0,
      prev_doc_id: 0,
      doc_buffer_size: 0,
      doc_buffer_upto: 0,
      doc_in_util: None,
      freq_buffer,
      pos_delta_buffer,
      payload_length_buffer,
      offset_start_delta_buffer,
      offset_length_buffer,
      payload_bytes,
      payload_byte_upto: 0,
      payload_length: 0,

      last_start_offset: 0,
      start_offset,
      end_offset,

      pos_buffer_upto: 0,

      pos_in_util,
      pay_in_util,
      payload,

      options,
      index_has_freq,
      index_has_pos,
      index_has_offsets,
      index_has_payloads,
      index_has_offsets_or_payloads,

      flags,
      needs_freq,
      needs_pos,
      needs_offsets,
      needs_payloads,
      needs_offsets_or_payloads,
      needs_impacts,
      needs_docs_and_freqs_only,

      freq_fp: 0,
      position: 0,
      pos_doc_buffer_upto: 0,
      pos_pending_count: 0,
      last_pos_block_fp: 0,

      level0_pos_end_fp: 0,
      level0_block_pos_upto: 0,
      level0_pay_end_fp: 0,
      level0_block_pay_upto: 0,
      level0_serialized_impacts,
      level0_impacts: Some(level0_impacts),

      level1_pos_end_fp: 0,
      level1_block_pos_upto: 0,
      level1_pay_end_fp: 0,
      level1_block_pay_upto: 0,
      level1_serialized_impacts,
      level1_impacts: Some(level1_impacts),

      needs_refilling: false,
      max_num_impacts_at_level0: reader.max_num_impacts_at_level0,
      max_num_impacts_at_level1: reader.max_num_impacts_at_level1,

      max_impact_num_bytes_at_level0: reader.max_impact_num_bytes_at_level0,
      max_impact_num_bytes_at_level1: reader.max_impact_num_bytes_at_level1,
    })
  }
  pub fn can_reuse(
    &self,
    doc_in: &I,
    field_info: &FieldInfo,
    flags: i32,
    needs_impacts: bool,
    reader: &Lucene101PostingsReader<I>,
  ) -> bool {
    ptr::eq(doc_in, &reader.doc_in)
      && self.options == *field_info.get_index_options()
      && self.index_has_payloads == field_info.has_payloads()
      && self.flags == flags
      && self.needs_impacts == needs_impacts
  }

  pub fn reset(
    &mut self,
    term_state: &TermStateEnum,
    _flags: i32,
    reader: &Lucene101PostingsReader<I>,
  ) -> Result<&mut Self> {
    let term_state = match term_state {
      TermStateEnum::Int(s) => s,
      _ => {
        return Err(LuceneError::illegal_state(
          "term_state should be IntBlockTermState",
        ));
      },
    };
    self.doc_freq = term_state.base.doc_freq;
    self.singleton_doc_id = term_state.singleton_doc_id;
    if self.doc_freq > 1 {
      if self.doc_in_util.is_none() {
        let doc_in = reader.doc_in.try_clone()?;
        self.doc_in_util = Some(DEFAULT_VECTORIZATION_PROVIDER.new_posting_decoding_util(doc_in));
      }
      prefetch_postings(&mut self.doc_in_util.as_mut().unwrap().input, term_state)?;
    }

    if self.for_delta_util.is_none() && (self.doc_freq as usize) >= ForUtil::BLOCK_SIZE {
      self.for_delta_util = Some(ForDeltaUtil::new());
    }
    self.total_term_freq = if self.index_has_freq {
      term_state.base.total_term_freq
    } else {
      term_state.base.doc_freq as i64
    };
    if self.needs_freq
      && self.pfor_util.is_none()
      && (self.total_term_freq as usize) >= ForUtil::BLOCK_SIZE
    {
      self.pfor_util = Some(PForUtil::new());
    }
    // Where this term's postings start in the .pos file:
    let pos_term_start_fp = term_state.pos_start_fp;
    // Where this term's payloads/offsets start in the .pay
    let pay_term_start_fp = term_state.pay_start_fp;
    if let Some(ref mut pos_in) = self.pos_in_util {
      pos_in.input.seek(pos_term_start_fp as usize)?;
      if let Some(ref mut pay_in) = self.pay_in_util {
        pay_in.input.seek(pay_term_start_fp as usize)?;
      }
    }
    self.level1_pos_end_fp = pos_term_start_fp;
    self.level1_pay_end_fp = pay_term_start_fp;
    self.level0_pos_end_fp = pos_term_start_fp;
    self.level0_pay_end_fp = pay_term_start_fp;
    self.pos_pending_count = 0;
    self.payload_byte_upto = 0;
    self.last_pos_block_fp = if term_state.base.total_term_freq as usize <= ForUtil::BLOCK_SIZE {
      if term_state.base.total_term_freq as usize == ForUtil::BLOCK_SIZE {
        -1
      } else {
        term_state.pos_start_fp
      }
    } else {
      term_state.pos_start_fp + term_state.last_pos_block_offset
    };

    self.level1_block_pos_upto = 0;
    self.level1_block_pay_upto = 0;
    self.level0_block_pos_upto = 0;
    self.level0_block_pay_upto = 0;
    self.pos_buffer_upto = ForUtil::BLOCK_SIZE as i32;

    self.doc = -1;
    self.prev_doc_id = -1;
    self.doc_count_left = self.doc_freq;
    self.freq_fp = -1;
    self.level0_last_doc_id = -1;
    if (self.doc_freq) < Lucene101PostingsFormat::LEVEL1_NUM_DOCS {
      self.level1_last_doc_id = NO_MORE_DOCS;
      if self.doc_freq > 1 {
        self
          .doc_in_util
          .as_mut()
          .unwrap()
          .input
          .seek(term_state.doc_start_fp as usize)?;
      }
    } else {
      self.level1_last_doc_id = -1;
      self.level1_doc_end_fp = term_state.doc_start_fp;
    }
    self.level1_doc_count_upto = 0;
    self.doc_buffer_size = ForUtil::BLOCK_SIZE as i32;
    self.doc_buffer_upto = ForUtil::BLOCK_SIZE as i32;
    self.pos_doc_buffer_upto = ForUtil::BLOCK_SIZE as i32;
    Ok(self)
  }
  fn refill_full_block(&mut self) -> Result<()> {
    let for_delta_util = self.for_delta_util.as_mut().unwrap();
    let doc_in_util = self.doc_in_util.as_mut().unwrap();
    for_delta_util.decode_and_prefix_sum(
      &mut *doc_in_util,
      self.prev_doc_id,
      &mut self.doc_buffer,
    )?;
    if self.index_has_freq {
      let doc_in = &mut self.doc_in_util.as_mut().unwrap().input;
      if self.needs_freq {
        self.freq_fp = doc_in.get_file_pointer()? as i64;
      }
      PForUtil::skip(doc_in)?;
    }

    self.doc_count_left -= ForUtil::BLOCK_SIZE as i32;
    self.prev_doc_id = self.doc_buffer[ForUtil::BLOCK_SIZE - 1];
    self.doc_buffer_upto = 0;
    self.pos_doc_buffer_upto = 0;
    Ok(())
  }

  fn refill_remainder(&mut self) -> Result<()> {
    debug_assert!(self.doc_count_left >= 0 && (self.doc_count_left as usize) < ForUtil::BLOCK_SIZE);

    if self.doc_freq == 1 {
      self.doc_buffer[0] = self.singleton_doc_id;
      self.freq_buffer[0] = self.total_term_freq as i32;
      self.doc_buffer[1] = NO_MORE_DOCS;
      debug_assert_eq!(self.freq_fp, -1);
      self.doc_count_left = 0;
      self.doc_buffer_size = 1;
    } else {
      let doc_in = &mut self.doc_in_util.as_mut().unwrap().input;
      PostingsUtil::read_vint_block(
        doc_in,
        &mut self.doc_buffer,
        &mut self.freq_buffer,
        self.doc_count_left as usize,
        self.index_has_freq,
        self.needs_freq,
      )?;

      prefix_sum(
        &mut self.doc_buffer,
        self.doc_count_left as usize,
        self.prev_doc_id,
      );
      self.doc_buffer[self.doc_count_left as usize] = NO_MORE_DOCS;
      self.freq_fp = -1;
      self.doc_buffer_size = self.doc_count_left;
      self.doc_count_left = 0;
    }
    self.prev_doc_id = self.doc_buffer[ForUtil::BLOCK_SIZE - 1];
    self.doc_buffer_upto = 0;
    self.pos_doc_buffer_upto = 0;
    debug_assert_eq!(self.doc_buffer[self.doc_buffer_size as usize], NO_MORE_DOCS);
    Ok(())
  }
  fn refill_docs(&mut self) -> Result<()> {
    debug_assert!(self.doc_count_left >= 0);
    if (self.doc_count_left as usize) >= ForUtil::BLOCK_SIZE {
      self.refill_full_block()?;
    } else {
      self.refill_remainder()?;
    }
    Ok(())
  }
  fn skip_level1_to(&mut self, target: i32) -> Result<()> {
    let doc_in = &mut self.doc_in_util.as_mut().unwrap().input;
    loop {
      self.prev_doc_id = self.level1_last_doc_id;
      self.level0_last_doc_id = self.level1_last_doc_id;
      doc_in.seek(self.level1_doc_end_fp as usize)?;
      self.level0_pos_end_fp = self.level1_pos_end_fp;
      self.level0_block_pos_upto = self.level1_block_pos_upto;
      self.level0_pay_end_fp = self.level1_pay_end_fp;
      self.level0_block_pay_upto = self.level1_block_pay_upto;

      self.doc_count_left = self.doc_freq - self.level1_doc_count_upto;
      self.level1_doc_count_upto += Lucene101PostingsFormat::LEVEL1_NUM_DOCS;

      if self.doc_count_left < Lucene101PostingsFormat::LEVEL1_NUM_DOCS {
        self.level1_last_doc_id = NO_MORE_DOCS;
        break;
      }

      self.level1_last_doc_id += doc_in.read_vint()?;
      let delta = doc_in.read_vlong()?;
      self.level1_doc_end_fp = delta + doc_in.get_file_pointer()? as i64;

      if self.index_has_freq {
        let skip1_end_fp = doc_in.read_short()? as i64 + doc_in.get_file_pointer()? as i64;
        let num_impact_bytes = doc_in.read_short()? as usize;

        if self.needs_impacts && self.level1_last_doc_id >= target {
          let byte_ref = self.level1_serialized_impacts.as_mut().unwrap();
          doc_in.read_bytes(&mut byte_ref.bytes, 0, num_impact_bytes)?;
          byte_ref.length = num_impact_bytes;
        } else {
          IndexInput::skip_bytes(&mut *doc_in, num_impact_bytes as i64)?;
        }

        if self.index_has_pos {
          self.level1_pos_end_fp += doc_in.read_vlong()?;
          self.level1_block_pos_upto = doc_in.read_byte()? as i32;
          if self.index_has_offsets_or_payloads {
            self.level1_pay_end_fp += doc_in.read_vlong()?;
            self.level1_block_pay_upto = doc_in.read_vint()?;
          }
          debug_assert_eq!(doc_in.get_file_pointer()? as i64, skip1_end_fp);
        }
      }

      if self.level1_last_doc_id >= target {
        break;
      }
    }
    Ok(())
  }
  fn do_move_to_next_level0_block(&mut self) -> Result<()> {
    debug_assert!(self.doc_buffer_upto as usize == ForUtil::BLOCK_SIZE);
    if let Some(ref mut pos_in) = self.pos_in_util {
      if self.level0_pos_end_fp >= pos_in.input.get_file_pointer()? as i64 {
        pos_in.input.seek(self.level0_pos_end_fp as usize)?;
        self.pos_pending_count = self.level0_block_pos_upto;
        if let Some(ref mut pay_in) = self.pay_in_util {
          debug_assert!(self.level0_pay_end_fp >= pay_in.input.get_file_pointer()? as i64);
          pay_in.input.seek(self.level0_pay_end_fp as usize)?;
          self.payload_byte_upto = self.level0_block_pay_upto;
        }
        self.pos_buffer_upto = ForUtil::BLOCK_SIZE as i32;
      } else {
        debug_assert!(self.freq_fp == -1);
        self.pos_pending_count += sum_over_range(
          &self.freq_buffer,
          self.pos_doc_buffer_upto as usize,
          ForUtil::BLOCK_SIZE,
        );
      }
    }

    if (self.doc_count_left as usize) >= ForUtil::BLOCK_SIZE {
      {
        let doc_in = &mut self.doc_in_util.as_mut().unwrap().input;
        doc_in.read_vlong()?;
        let doc_delta = read_vint15(doc_in)?;
        self.level0_last_doc_id += doc_delta;

        let block_length = read_vlong15(doc_in)?;
        self.level0_doc_end_fp = doc_in.get_file_pointer()? as i64 + block_length;
        if self.index_has_freq {
          let num_impact_bytes = doc_in.read_vint()?.try_convert()?;
          if self.needs_impacts {
            let bi = self.level0_serialized_impacts.as_mut().unwrap();
            doc_in.read_bytes(&mut bi.bytes, 0, num_impact_bytes)?;
            bi.length = num_impact_bytes;
          } else {
            IndexInput::skip_bytes(doc_in, num_impact_bytes as i64)?;
          }

          if self.index_has_pos {
            self.level0_pos_end_fp += doc_in.read_vlong()?;
            self.level0_block_pos_upto = doc_in.read_byte()? as i32;
            if self.index_has_offsets_or_payloads {
              self.level0_pay_end_fp += doc_in.read_vlong()?;
              self.level0_block_pay_upto = doc_in.read_vint()?;
            }
          }
        }
      }
      self.refill_full_block()?;
    } else {
      self.level0_last_doc_id = NO_MORE_DOCS;
      self.refill_remainder()?;
    }
    Ok(())
  }
  fn move_to_next_level0_block(&mut self) -> Result<()> {
    if self.doc == self.level1_last_doc_id {
      // advance level 1 skip data
      self.skip_level1_to(self.doc + 1)?;
    }
    // Now advance level 0 skip data
    self.prev_doc_id = self.level0_last_doc_id;

    if self.needs_docs_and_freqs_only && (self.doc_count_left as usize) >= ForUtil::BLOCK_SIZE {
      // Optimize the common path for exhaustive evaluation
      {
        let doc_in = &mut self.doc_in_util.as_mut().unwrap().input;
        let level0_num_bytes = doc_in.read_vlong()?;
        IndexInput::skip_bytes(doc_in, level0_num_bytes)?;
      }
      self.refill_full_block()?;
      self.level0_last_doc_id = self.doc_buffer[ForUtil::BLOCK_SIZE - 1];
    } else {
      self.do_move_to_next_level0_block()?;
    }
    Ok(())
  }
  #[allow(dead_code)]
  fn read_level0_pos_data(&mut self) -> Result<()> {
    // Due to Rust's borrowing rules, we wrote this logic directly at the
    // call site.
    Ok(())
  }
  fn seek_pos_data(
    &mut self,
    pos_fp: i64,
    pos_upto: i32,
    pay_fp: i64,
    pay_upto: i32,
  ) -> Result<()> {
    // If nextBlockPosFP is less than the current FP, it means that the
    // block of positions for the first docs of the next block are
    // already decoded. In this case we just accumulate frequencies
    // into posPendingCount instead of seeking backwards and decoding the
    // same pos block again.
    let pos_in = &mut self.pos_in_util.as_mut().unwrap().input;
    if pos_fp >= pos_in.get_file_pointer()? as i64 {
      pos_in.seek(pos_fp as usize)?;
      self.pos_pending_count = pos_upto;
      // needs payloads or offsets
      if let Some(ref mut pay_in) = self.pay_in_util {
        debug_assert!(self.level0_pay_end_fp >= pay_in.input.get_file_pointer()? as i64);
        pay_in.input.seek(pay_fp as usize)?;
        self.payload_byte_upto = pay_upto;
      }
      self.pos_buffer_upto = ForUtil::BLOCK_SIZE as i32;
    } else {
      self.pos_pending_count += sum_over_range(
        &self.freq_buffer,
        self.pos_doc_buffer_upto as usize,
        ForUtil::BLOCK_SIZE,
      );
    }
    Ok(())
  }
  fn skip_level0_to(&mut self, target: i32) -> Result<()> {
    let (mut pos_fp, mut pos_upto, mut pay_fp, mut pay_upto): (i64, i32, i64, i32);
    {
      loop {
        self.prev_doc_id = self.level0_last_doc_id;

        pos_fp = self.level0_pos_end_fp;
        pos_upto = self.level0_block_pos_upto;
        pay_fp = self.level0_pay_end_fp;
        pay_upto = self.level0_block_pay_upto;

        if (self.doc_count_left as usize) >= ForUtil::BLOCK_SIZE {
          let doc_in = &mut self.doc_in_util.as_mut().unwrap().input;
          let num_skip_bytes: usize = doc_in.read_vlong()?.try_convert()?;
          let skip0_end = doc_in.get_file_pointer()? + num_skip_bytes;
          let doc_delta = read_vint15(doc_in)?;
          self.level0_last_doc_id += doc_delta;
          let found = target <= self.level0_last_doc_id;
          let block_length = read_vlong15(doc_in)?;
          self.level0_doc_end_fp = doc_in.get_file_pointer()? as i64 + block_length;

          if self.index_has_freq {
            if !found && !self.needs_pos {
              doc_in.seek(skip0_end)?;
            } else {
              let num_impact_bytes = doc_in.read_vint()?.try_convert()?;
              if self.needs_impacts && found {
                let bytes = self.level0_serialized_impacts.as_mut().unwrap();
                doc_in.read_bytes(&mut bytes.bytes, 0, num_impact_bytes)?;
                bytes.length = num_impact_bytes;
              } else {
                IndexInput::skip_bytes(&mut *doc_in, num_impact_bytes as i64)?;
              }
              if self.needs_pos {
                // self.read_level0_pos_data()?;
                self.level0_pos_end_fp += doc_in.read_vlong()?;
                self.level0_block_pos_upto = doc_in.read_byte()? as i32;
                if self.index_has_offsets_or_payloads {
                  self.level0_pay_end_fp += doc_in.read_vlong()?;
                  self.level0_block_pay_upto = doc_in.read_vint()?;
                }
              } else {
                doc_in.seek(skip0_end)?;
              }
            }
          }
          if found {
            break;
          }
          doc_in.seek(self.level0_doc_end_fp as usize)?;
          self.doc_count_left -= ForUtil::BLOCK_SIZE as i32;
        } else {
          self.level0_last_doc_id = NO_MORE_DOCS;
          break;
        }
      }
    }
    if self.pos_in_util.is_some() {
      self.seek_pos_data(pos_fp, pos_upto, pay_fp, pay_upto)?;
    }

    Ok(())
  }
  fn do_advance_shallow(&mut self, target: i32) -> Result<()> {
    if target > self.level1_last_doc_id {
      // advance skip data on level 1
      self.skip_level1_to(target)?;
    } else if self.needs_refilling {
      let doc_in = &mut self.doc_in_util.as_mut().unwrap().input;
      doc_in.seek(self.level0_doc_end_fp as usize)?;
      self.doc_count_left -= ForUtil::BLOCK_SIZE as i32;
    }

    self.skip_level0_to(target)?;
    Ok(())
  }
  fn skip_positions(&mut self, freq: i32) -> Result<()> {
    let mut to_skip = self.pos_pending_count - freq;
    let left_in_block = ForUtil::BLOCK_SIZE as i32 - self.pos_buffer_upto;

    if to_skip < left_in_block {
      let end = self.pos_buffer_upto + to_skip;
      if self.needs_payloads {
        self.payload_byte_upto += sum_over_range(
          &self.payload_length_buffer,
          self.pos_buffer_upto as usize,
          end as usize,
        );
      }
      self.pos_buffer_upto = end;
    } else {
      to_skip -= left_in_block;
      {
        let pos_in = &mut self.pos_in_util.as_mut().unwrap().input;
        while to_skip >= ForUtil::BLOCK_SIZE as i32 {
          debug_assert!(pos_in.get_file_pointer()? as i64 != self.last_pos_block_fp);
          PForUtil::skip(pos_in)?;

          if let Some(ref mut pay_in) = self.pay_in_util {
            let pay_in = &mut pay_in.input;
            if self.index_has_payloads {
              PForUtil::skip(pay_in)?;
              let num_bytes = pay_in.read_vint()?.try_convert()?;
              let pos = pay_in.get_file_pointer()?;
              pay_in.seek(pos + num_bytes)?;
            }

            if self.index_has_offsets {
              PForUtil::skip(&mut *pay_in)?;
              PForUtil::skip(&mut *pay_in)?;
            }
          }

          to_skip -= ForUtil::BLOCK_SIZE as i32;
        }
      }
      self.refill_positions()?;

      if self.needs_payloads {
        self.payload_byte_upto = sum_over_range(&self.payload_length_buffer, 0, to_skip as usize);
      }

      self.pos_buffer_upto = to_skip;
    }

    Ok(())
  }
  fn refill_last_position_block(&mut self) -> Result<()> {
    let count = (self.total_term_freq % ForUtil::BLOCK_SIZE as i64) as usize;
    let mut payload_length = 0;
    let mut offset_length = 0;
    self.payload_byte_upto = 0;

    let pos_in = &mut self.pos_in_util.as_mut().unwrap().input;

    for i in 0..count {
      let code = pos_in.read_vint()?;
      if self.index_has_payloads {
        if (code & 1) != 0 {
          payload_length = pos_in.read_vint()?;
        }
        if !self.payload_length_buffer.is_empty() {
          self.payload_length_buffer[i] = payload_length;
          self.pos_delta_buffer[i] = ((code as u32) >> 1) as i32;

          if payload_length != 0 {
            let need = self.payload_byte_upto + payload_length;
            if need as usize > self.payload_bytes.len() {
              ArrayUtil::grow_with_len(&mut self.payload_bytes, need as usize)?;
            }

            pos_in.read_bytes(
              &mut self.payload_bytes,
              self.payload_byte_upto as usize,
              payload_length as usize,
            )?;
            self.payload_byte_upto += payload_length;
          }
        } else {
          IndexInput::skip_bytes(&mut *pos_in, payload_length as i64)?;
        }
      } else {
        self.pos_delta_buffer[i] = code;
      }

      if self.index_has_offsets {
        let delta_code = pos_in.read_vint()?;
        if (delta_code & 1) != 0 {
          offset_length = pos_in.read_vint()?;
        }

        if !self.offset_start_delta_buffer.is_empty() {
          self.offset_start_delta_buffer[i] = ((delta_code as u32) >> 1) as i32;
          self.offset_length_buffer[i] = offset_length;
        }
      }
    }

    self.payload_byte_upto = 0;
    Ok(())
  }
  fn refill_offsets_or_payloads(&mut self) -> Result<()> {
    if self.index_has_payloads {
      if self.needs_payloads {
        let pay_in_util = self.pay_in_util.as_mut().unwrap();
        self
          .pfor_util
          .as_mut()
          .unwrap()
          .decode(pay_in_util, &mut self.payload_length_buffer)?;

        let num_bytes = self
          .pay_in_util
          .as_mut()
          .unwrap()
          .input
          .read_vint()?
          .try_convert()?;
        if num_bytes > self.payload_bytes.len() {
          ArrayUtil::grow_no_copy(&mut self.payload_bytes, num_bytes)?;
        }

        self.pay_in_util.as_mut().unwrap().input.read_bytes(
          &mut self.payload_bytes,
          0,
          num_bytes,
        )?;
      } else if let Some(ref mut pay_in) = self.pay_in_util {
        // this works, because when writing a vint block we always force
        // the first length to be written
        let pay_in = &mut pay_in.input;
        PForUtil::skip(pay_in)?;
        let num_bytes = pay_in.read_vint()?.try_convert()?;
        let pos = pay_in.get_file_pointer()?;
        pay_in.seek(pos + num_bytes)?;
      }
      self.payload_byte_upto = 0;
    }

    if self.index_has_offsets {
      if self.needs_offsets {
        let pay_in_util = self.pay_in_util.as_mut().unwrap();
        let pfor_util = self.pfor_util.as_mut().unwrap();
        pfor_util.decode(pay_in_util, &mut self.offset_start_delta_buffer)?;
        pfor_util.decode(pay_in_util, &mut self.offset_length_buffer)?;
      } else if let Some(ref mut pay_in) = self.pay_in_util {
        // this works, because when writing a vint block we always force
        // the first length to be written
        let input = &mut pay_in.input;
        PForUtil::skip(input)?;
        PForUtil::skip(input)?;
      }
    }
    Ok(())
  }
  fn refill_positions(&mut self) -> Result<()> {
    let pos = {
      let pos_in = &self.pos_in_util.as_mut().unwrap().input;
      pos_in.get_file_pointer()?
    };
    if pos as i64 == self.last_pos_block_fp {
      self.refill_last_position_block()?;
      return Ok(());
    }

    self.pfor_util.as_mut().unwrap().decode(
      self.pos_in_util.as_mut().unwrap(),
      &mut self.pos_delta_buffer,
    )?;

    if self.index_has_offsets_or_payloads {
      self.refill_offsets_or_payloads()?;
    }

    Ok(())
  }
  fn accumulate_pending_positions(&mut self) -> Result<()> {
    let freq = self.freq()?;

    self.pos_pending_count += sum_over_range(
      &self.freq_buffer,
      self.pos_doc_buffer_upto as usize,
      self.doc_buffer_upto as usize,
    );
    self.pos_doc_buffer_upto = self.doc_buffer_upto;

    debug_assert!(self.pos_pending_count > 0);

    if self.pos_pending_count > freq {
      self.skip_positions(freq)?;
      self.pos_pending_count = freq;
    }
    Ok(())
  }
  fn accumulate_payload_and_offsets(&mut self) {
    if self.needs_payloads {
      self.payload_length = self.payload_length_buffer[self.pos_buffer_upto as usize];
      let payload = self.payload.as_mut().unwrap();
      payload.offset = self.payload_byte_upto as usize;
      payload.length = self.payload_length as usize;
      // TODO IMPORTANT could we avoid copying the payload?
      payload.bytes.clone_from(&self.payload_bytes);
      self.payload_byte_upto += self.payload_length;
    }

    if self.needs_offsets {
      let pos = self.pos_buffer_upto as usize;
      self.start_offset = self.last_start_offset + self.offset_start_delta_buffer[pos];
      self.end_offset = self.start_offset + self.offset_length_buffer[pos];
      self.last_start_offset = self.start_offset;
    }
  }
}

impl<I> PostingsEnum for BlockPostingsEnum<I>
where
  I: IndexInput,
{
  fn freq(&mut self) -> Result<i32> {
    if self.freq_fp != -1 {
      let doc_in = &mut self.doc_in_util.as_mut().unwrap().input;
      doc_in.seek(self.freq_fp as usize)?;
      self
        .pfor_util
        .as_mut()
        .unwrap()
        .decode(self.doc_in_util.as_mut().unwrap(), &mut self.freq_buffer)?;
      self.freq_fp = -1;
    }
    Ok(self.freq_buffer[(self.doc_buffer_upto - 1) as usize])
  }

  fn next_position(&mut self) -> Result<i32> {
    if !self.needs_pos {
      return Ok(-1);
    }

    debug_assert!(self.pos_doc_buffer_upto <= self.doc_buffer_upto);

    if self.pos_doc_buffer_upto != self.doc_buffer_upto {
      self.accumulate_pending_positions()?;
      self.position = 0;
      self.last_start_offset = 0;
    }

    if self.pos_buffer_upto == ForUtil::BLOCK_SIZE as i32 {
      self.refill_positions()?;
      self.pos_buffer_upto = 0;
    }

    self.position += self.pos_delta_buffer[self.pos_buffer_upto as usize];

    if self.needs_offsets_or_payloads {
      self.accumulate_payload_and_offsets();
    }

    self.pos_buffer_upto += 1;
    self.pos_pending_count -= 1;

    Ok(self.position)
  }

  fn start_offset(&self) -> Result<i32> {
    if !self.needs_offsets {
      Ok(-1)
    } else {
      Ok(self.start_offset)
    }
  }

  fn end_offset(&self) -> Result<i32> {
    if !self.needs_offsets {
      Ok(-1)
    } else {
      Ok(self.end_offset)
    }
  }

  fn get_payload(&self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
    if !self.needs_payloads || self.payload_length == 0 {
      Ok(None)
    } else {
      Ok(Some(Cow::Borrowed(self.payload.as_ref().unwrap())))
    }
  }
}

impl<I> DocIdSetIterator for BlockPostingsEnum<I>
where
  I: IndexInput,
{
  fn doc_id(&self) -> i32 {
    self.doc
  }

  fn next_doc(&mut self) -> Result<i32> {
    if self.doc_buffer_upto == ForUtil::BLOCK_SIZE as i32 {
      self.move_to_next_level0_block()?;
    }
    let doc = self.doc_buffer[self.doc_buffer_upto as usize];
    self.doc = doc;
    self.doc_buffer_upto += 1;
    Ok(doc)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    if target > self.level0_last_doc_id || self.needs_refilling {
      if target > self.level0_last_doc_id {
        self.do_advance_shallow(target)?;
      }
      self.refill_docs()?;
      self.needs_refilling = false;
    }
    let next = VECTOR_UTIL.find_next_geq(
      &self.doc_buffer,
      target,
      self.doc_buffer_upto as usize,
      self.doc_buffer_size as usize,
    );
    self.doc = self.doc_buffer[next];
    self.doc_buffer_upto = (next + 1) as i32;
    Ok(self.doc)
  }

  fn cost(&self) -> Result<i64> {
    Ok(self.doc_freq as i64)
  }
}

impl<I> ImpactsSource for BlockPostingsEnum<I>
where
  I: IndexInput,
{
  fn advance_shallow(&mut self, target: i32) -> Result<()> {
    if target > self.level0_last_doc_id {
      // advance level 0 skip data
      self.do_advance_shallow(target)?;

      // If we are on the last doc ID of a block and we are advancing on
      // the doc ID just beyond this block, then we decode the
      // block. This may not be necessary, but this helps avoid
      // having to check whether we are in a block that is not decoded yet
      // in `next_doc`.
      if self.doc_buffer_upto == ForUtil::BLOCK_SIZE as i32 && target == self.doc + 1 {
        self.refill_docs()?;
        self.needs_refilling = false;
      } else {
        self.needs_refilling = true;
      }
    }
    Ok(())
  }

  type Impacts<'a>
    = ImpactsImpl<'a>
  where
    I: 'a;

  fn get_impacts(&self) -> Result<Self::Impacts<'_>> {
    debug_assert!(self.needs_impacts);

    Ok(ImpactsImpl {
      index_has_freq: self.index_has_freq,
      level0_last_doc_id: self.level0_last_doc_id,
      level1_last_doc_id: self.level1_last_doc_id,
      level0_serialized_impacts: self.level0_serialized_impacts.as_ref(),
      level1_serialized_impacts: self.level1_serialized_impacts.as_ref(),
      max_num_impacts_at_level0: self.max_num_impacts_at_level0 as usize,
      max_num_impacts_at_level1: self.max_num_impacts_at_level1 as usize,
    })
  }
}
impl<I> ImpactsEnum for BlockPostingsEnum<I> where I: IndexInput {}

pub struct ImpactsImpl<'a> {
  index_has_freq: bool,
  level0_last_doc_id: i32,
  level1_last_doc_id: i32,
  level0_serialized_impacts: Option<&'a BytesRef<Vec<u8>>>,
  level1_serialized_impacts: Option<&'a BytesRef<Vec<u8>>>,
  max_num_impacts_at_level0: usize,
  max_num_impacts_at_level1: usize,
}
impl ImpactsImpl<'_> {
  fn read_impacts(
    serialized: &[u8],
    serialized_len: usize,
    level_impacts_len: usize,
  ) -> Result<MutableImpactList> {
    let mut scratch = ByteArrayDataInput::with_range(serialized, 0, serialized_len);
    let mut level_impacts = MutableImpactList::with_capacity(level_impacts_len);
    read_impacts(&mut scratch, &mut level_impacts)?;
    Ok(level_impacts)
  }
}
impl Impacts for ImpactsImpl<'_> {
  fn num_levels(&self) -> i32 {
    if !self.index_has_freq || self.level1_last_doc_id == NO_MORE_DOCS {
      1
    } else {
      2
    }
  }

  fn get_doc_id_upto(&self, level: i32) -> i32 {
    if !self.index_has_freq {
      NO_MORE_DOCS
    } else if level == 0 {
      self.level0_last_doc_id
    } else if level == 1 {
      self.level1_last_doc_id
    } else {
      NO_MORE_DOCS
    }
  }

  fn get_impacts(&self, level: i32) -> Result<Vec<Impact>> {
    if self.index_has_freq {
      // We don't reuse level0_impacts and level1_impacts like Java Lucene does.
      if level == 0 && self.level0_last_doc_id != NO_MORE_DOCS {
        let level0_serialized_impacts_bytes_ref = self.level0_serialized_impacts.as_ref().unwrap();
        let level0_impacts = ImpactsImpl::read_impacts(
          level0_serialized_impacts_bytes_ref.bytes.as_ref(),
          level0_serialized_impacts_bytes_ref.length,
          self.max_num_impacts_at_level0,
        )?;
        return Ok(level0_impacts.impacts[..level0_impacts.length].to_vec());
      }
      if level == 1 {
        let level1_serialized_impacts_bytes_ref = self.level1_serialized_impacts.as_ref().unwrap();
        let level1_impacts = ImpactsImpl::read_impacts(
          level1_serialized_impacts_bytes_ref.bytes.as_ref(),
          level1_serialized_impacts_bytes_ref.length,
          self.max_num_impacts_at_level1,
        )?;
        return Ok(level1_impacts.impacts[..level1_impacts.length].to_vec());
      }
    }
    Ok(vec![Impact::new(i32::MAX, 1)])
  }
}

#[derive(Default)]
pub(crate) struct MutableImpactList {
  pub(crate) length: usize,
  pub(crate) impacts: Vec<Impact>,
}
impl MutableImpactList {
  pub(crate) fn with_capacity(capacity: usize) -> Self {
    let mut impacts = Vec::with_capacity(capacity);
    impacts.resize_with(capacity, || Impact {
      freq: i32::MAX,
      norm: 1,
    });
    MutableImpactList { length: 0, impacts }
  }

  pub(crate) fn get(&self, index: usize) -> &Impact {
    &self.impacts[index]
  }

  pub(crate) fn size(&self) -> usize {
    self.length
  }
}

fn prefix_sum(buffer: &mut [i32], count: usize, base: i32) {
  buffer[0] = buffer[0].wrapping_add(base);
  for i in 1..count {
    buffer[i] = buffer[i].wrapping_add(buffer[i - 1]);
  }
}

/// See also [`Lucene101PostingsWriter::writeVInt15`](crate::core::codecs::lucene101::lucene101_postings_writer::write_vint15).
pub(crate) fn read_vint15(input: &mut impl DataInput) -> Result<i32> {
  let s = input.read_short()?;
  if s >= 0 {
    Ok(s as i32)
  } else {
    Ok((s as i32) & 0x7FFF | (input.read_vint()? << 15))
  }
}

/// See also [`Lucene101PostingsWriter::writeVLong15`](crate::core::codecs::lucene101::lucene101_postings_writer::write_vlong15).
pub(crate) fn read_vlong15(input: &mut impl DataInput) -> Result<i64> {
  let s = input.read_short()?;
  if s >= 0 {
    Ok(s as i64)
  } else {
    Ok((s as i64) & 0x7FFF | (input.read_vlong()? << 15))
  }
}
pub(crate) fn read_impacts(
  in_: &mut ByteArrayDataInput<&[u8]>,
  reuse: &mut MutableImpactList,
) -> Result<()> {
  let mut freq = 0;
  let mut norm = 0;
  let mut length = 0;

  while in_.get_position() < in_.length() {
    let freq_delta = in_.read_vint()?;
    freq += 1 + ((freq_delta as u32) >> 1) as i32;
    if (freq_delta & 1) != 0 {
      norm += 1 + in_.read_zlong()?;
    } else {
      norm += 1;
    }
    let impact = &mut reuse.impacts[length];
    impact.freq = freq;
    impact.norm = norm;
    length += 1;
  }
  reuse.length = length;
  Ok(())
}
fn sum_over_range(arr: &[i32], start: usize, end: usize) -> i32 {
  arr[start..end].iter().sum()
}

fn prefetch_postings(doc_in: &mut impl IndexInput, state: &IntBlockTermState) -> Result<()> {
  debug_assert!(state.base.doc_freq > 1);
  if doc_in.get_file_pointer()? as i64 != state.doc_start_fp {
    // Don't prefetch if the input is already positioned at the right
    // offset, which suggests that the caller is streaming
    // the entire inverted index (e.g. for merging), let the read-ahead
    // logic do its work instead. Note that this heuristic doesn't work
    // for terms that have skip data, since skip data is
    // stored after the last term, but handling all terms that have <128
    // docs is a good start already.
    doc_in.prefetch(state.doc_start_fp as usize, 1)?;
  }
  // Note: we don't prefetch positions or offsets, which are less likely
  // to be needed.
  Ok(())
}
