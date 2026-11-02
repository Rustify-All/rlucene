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
use crate::core::codecs::block_tree::intersect_terms_enum::IntersectTermsEnum;
use crate::core::codecs::block_tree::lucene90_block_tree_terms_reader::{
  OUTPUT_FLAG_HAS_TERMS, VERSION_MSB_VLONG_OUTPUT,
};
use crate::core::codecs::lucene90::block_tree::lucene90_block_tree_terms_reader::TermsReader;
use crate::core::codecs::lucene90::block_tree::segment_terms_enum::SegmentTermsEnum;
use crate::core::codecs::lucene90::block_tree::stats::Stats;
use crate::core::codecs::postings_reader_base::PostingsReaderBase;
use crate::core::index::BytesRef;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::terms::Terms;
use crate::core::store::{ByteArrayDataInput, DataInput, IndexInput};
use crate::core::util::automation::compiled_automaton::{AutomatonType, CompiledAutomaton};
use crate::core::util::clone::TryClone;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fst_impl::byte_sequence_outputs::ByteSequenceOutputs;
use crate::core::util::fst_impl::fst::{FST, FSTMetadata, read_metadata};
use crate::core::util::fst_impl::off_heap_fst_store::OffHeapFSTStore;
use crate::core::util::{ToInt, TryIntoInt};
use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

/// BlockTree's implementation of [`Terms`].
#[allow(clippy::type_complexity)]
pub struct FieldReader<I, PR>
where
  I: IndexInput,
  PR: PostingsReaderBase,
{
  pub(crate) num_terms: i64,
  pub(crate) field_info: Arc<FieldInfo>,
  pub(crate) sum_total_term_freq: i64,
  pub(crate) sum_doc_freq: i64,
  pub(crate) doc_count: i32,
  pub(crate) root_block_fp: i64,
  pub(crate) root_code: BytesRef<Arc<Vec<u8>>>,
  pub(crate) min_term: Arc<BytesRef<Vec<u8>>>,
  pub(crate) max_term: Arc<BytesRef<Vec<u8>>>,
  pub(crate) parent: Option<Arc<TermsReader<I, PR>>>,
  pub(crate) index: Option<Arc<FST<ByteSequenceOutputs, OffHeapFSTStore<I>>>>,
  tmp_data: Option<TmpData>,
}
struct TmpData {
  pub(crate) metadata: FSTMetadata<ByteSequenceOutputs>,
  index_start_fp: i64,
  pub(crate) root_code: BytesRef<Vec<u8>>,
}
impl<I, PR> FieldReader<I, PR>
where
  I: IndexInput,
  PR: PostingsReaderBase,
{
  #[allow(clippy::too_many_arguments)]
  pub(crate) fn new<I1>(
    field_info: Arc<FieldInfo>,
    num_terms: i64,
    root_code: BytesRef<Vec<u8>>,
    sum_total_term_freq: i64,
    sum_doc_freq: i64,
    doc_count: i32,
    index_start_fp: i64,
    meta_in: &mut I1,
    min_term: Arc<BytesRef<Vec<u8>>>,
    max_term: Arc<BytesRef<Vec<u8>>>,
  ) -> Result<Self>
  where
    I1: IndexInput,
  {
    debug_assert!(num_terms > 0);
    // Read FST metadata and build the index
    let metadata = read_metadata(meta_in, ByteSequenceOutputs::get_singleton().clone())?;
    let tmp = {
      TmpData {
        metadata,
        index_start_fp,
        root_code,
      }
    };
    Ok(Self {
      parent: None,
      field_info,
      num_terms,
      sum_total_term_freq,
      sum_doc_freq,
      doc_count,
      // init with padding value
      root_block_fp: 0,
      // init with padding value
      root_code: BytesRef::new(),
      min_term,
      max_term,
      index: None,
      tmp_data: Some(tmp),
    })
  }
  pub fn init_field_reader(index_in: Arc<I>, reader: &mut FieldReader<I, PR>) -> Result<()> {
    let tmp_data = match reader.tmp_data.take() {
      Some(tmp_data) => tmp_data,
      None => {
        return Err(LuceneError::illegal_state(
          "TmpData is None, cannot init FieldReader".to_string(),
        ));
      },
    };
    let store = OffHeapFSTStore::new(
      index_in,
      tmp_data.index_start_fp as usize,
      tmp_data.metadata.num_bytes as usize,
    );
    let index = match FST::from_fst_reader(tmp_data.metadata, store) {
      Some(fst) => fst,
      None => {
        return Err(LuceneError::illegal_state(
          "FST metadata and store are some, should not return None".to_string(),
        ));
      },
    };
    reader.index = Some(Arc::new(index));
    // ownership to ByteArrayDataInput
    let mut input = ByteArrayDataInput::with_range(
      tmp_data.root_code.bytes.as_slice(),
      tmp_data.root_code.offset,
      tmp_data.root_code.length,
    );
    reader.root_block_fp =
      ((reader.read_vlong_output(&mut input)? as u64) >> OUTPUT_FLAG_HAS_TERMS).try_convert()?;
    // ownership from ByteArrayDataInput
    let root_code = tmp_data.root_code;
    // Get empty output and adjust rootCode
    let empty_output = reader
      .index
      .as_ref()
      .and_then(|index| index.metadata().empty_output())
      .and_then(Option::as_ref);
    let root_code_final = match empty_output {
      Some(empty_output)
        if root_code.bytes[root_code.offset..root_code.offset + root_code.length]
          == empty_output.bytes[empty_output.offset..empty_output.offset + empty_output.length] =>
      {
        drop(root_code);
        empty_output.clone()
      },
      _ => BytesRef {
        bytes: Arc::new(root_code.bytes),
        offset: root_code.offset,
        length: root_code.length,
      },
    };
    reader.root_code = root_code_final;
    Ok(())
  }

  pub(crate) fn read_vlong_output<DI>(&self, input: &mut DI) -> Result<i64>
  where
    DI: DataInput,
  {
    let version = self
      .parent
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("block-tree terms reader is missing"))?
      .version;
    if version >= VERSION_MSB_VLONG_OUTPUT {
      read_msb_vlong(input)
    } else {
      input.read_vlong()
    }
  }
}
impl<I, PR> FieldReader<I, PR>
where
  I: IndexInput,
  PR: PostingsReaderBase,
{
  pub(crate) fn get_stats_object(&self) -> Result<Stats> {
    SegmentTermsEnum::new(self.try_clone()?)?.compute_block_stats()
  }
}

impl<I, PR> Terms for FieldReader<I, PR>
where
  I: IndexInput,
  PR: PostingsReaderBase,
{
  type TermsEnum = SegmentTermsEnum<I, PR>;

  fn iterator(&self) -> Result<Self::TermsEnum> {
    SegmentTermsEnum::new(self.try_clone()?)
  }

  type IntersectIter = IntersectTermsEnum<I, PR>;

  fn intersect(
    &self,
    compiled: &CompiledAutomaton,
    start_term: Option<&BytesRef<Vec<u8>>>,
  ) -> Result<Self::IntersectIter> {
    if compiled.type_ != AutomatonType::Normal {
      return Err(LuceneError::illegal_argument(
        "please use CompiledAutomaton.getTermsEnum instead",
      ));
    }
    IntersectTermsEnum::new(
      self.try_clone()?,
      compiled.get_automaton()?,
      compiled.common_suffix_ref.clone(),
      start_term,
    )
  }

  fn size(&self) -> Result<i64> {
    Ok(self.num_terms)
  }

  fn get_sum_total_term_freq(&self) -> Result<i64> {
    Ok(self.sum_total_term_freq)
  }

  fn get_sum_doc_freq(&self) -> Result<i64> {
    Ok(self.sum_doc_freq)
  }

  fn get_doc_count(&self) -> Result<i32> {
    Ok(self.doc_count)
  }

  fn get_stats(&self) -> Result<String> {
    Ok(self.get_stats_object()?.to_string())
  }

  fn has_freqs(&self) -> bool {
    self
      .field_info
      .get_index_options()
      .cmp(&IndexOptions::DocsAndFreqs)
      .to_int()
      >= 0
  }

  fn has_offsets(&self) -> bool {
    self
      .field_info
      .get_index_options()
      .cmp(&IndexOptions::DocsAndFreqsAndPositionsAndOffsets)
      .to_int()
      >= 0
  }

  fn has_positions(&self) -> bool {
    self
      .field_info
      .get_index_options()
      .cmp(&IndexOptions::DocsAndFreqsAndPositions)
      .to_int()
      >= 0
  }

  fn has_payloads(&self) -> bool {
    self.field_info.has_payloads()
  }

  fn get_min(&self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
    Ok(Some(Cow::Borrowed(self.min_term.as_ref())))
  }

  fn get_max(&self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
    Ok(Some(Cow::Borrowed(self.max_term.as_ref())))
  }
}
impl<I, PR> fmt::Display for FieldReader<I, PR>
where
  I: IndexInput,
  PR: PostingsReaderBase,
{
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let Some(parent) = self.parent.as_ref() else {
      return Err(fmt::Error);
    };
    write!(
      f,
      "BlockTreeTerms(seg={} terms={} postings={} positions={} docs={})",
      parent.segment, self.num_terms, self.sum_doc_freq, self.sum_total_term_freq, self.doc_count
    )
  }
}
impl<I, PR> TryClone for FieldReader<I, PR>
where
  I: IndexInput,
  PR: PostingsReaderBase,
{
  // used to init SegmentTermsEnum
  fn try_clone(&self) -> Result<Self> {
    let index = self
      .index
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("block-tree field reader has no terms index"))?;
    Ok(Self {
      num_terms: self.num_terms,
      field_info: self.field_info.clone(),
      sum_total_term_freq: self.sum_total_term_freq,
      sum_doc_freq: self.sum_doc_freq,
      doc_count: self.doc_count,
      root_block_fp: self.root_block_fp,
      root_code: self.root_code.clone(),
      min_term: self.min_term.clone(),
      max_term: self.max_term.clone(),
      parent: self.parent.clone(),
      index: Some(Arc::clone(index)),
      tmp_data: None,
    })
  }
}

/// Decodes variable-length bytes in MSB order back to an `i64`,
/// as written by
/// [`Lucene90BlockTreeTermsWriter::write_msb_vlong`](crate::core::codecs::lucene90::block_tree::lucene90_block_tree_terms_writer::write_msb_vlong).
///
///
/// Visible within the crate for testing.
pub(crate) fn read_msb_vlong<DI>(input: &mut DI) -> Result<i64>
where
  DI: DataInput,
{
  let mut l: i64 = 0;
  loop {
    let b = input.read_byte()?;
    l = (l << 7) | (b & 0x7F) as i64;
    if (b & 0x80) == 0 {
      break;
    }
  }
  Ok(l)
}
