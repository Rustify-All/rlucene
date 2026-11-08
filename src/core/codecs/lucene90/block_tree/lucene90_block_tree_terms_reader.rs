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
use crate::core::codecs::fields_producer::FieldsProducer;
use crate::core::codecs::lucene90::block_tree::field_reader::FieldReader;
use crate::core::codecs::postings_reader_base::PostingsReaderBase;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::fields::Fields;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::store::directory::Directory;
use crate::core::store::{DataInput, IndexInput, ReadAdvice};
use crate::core::util::clone::TryClone;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::io_utils::IOUtils;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::fmt;
use std::fmt::Display;
use std::sync::Arc;

/// A block-based terms index and dictionary that assigns terms to variable
/// length blocks according to how they share prefixes. The terms index is a
/// prefix trie whose leaves are term blocks. The advantage of this approach is
/// that `seek_exact` is often able to determine a term cannot exist without
/// doing any IO, and intersection with Automata is very fast. Note that this
/// terms dictionary has its own fixed terms index (i.e., it does not support a
/// pluggable terms index implementation).
///
/// **NOTE**: this terms dictionary supports `min/max_items_per_block` during
/// indexing to control how much memory the terms index uses.
///
/// The data structure used by this implementation is very similar to a [burst
/// trie](https://citeseer.ist.psu.edu/viewdoc/summary?doi=10.1.1.18.3499), but with added logic to break
/// up too-large blocks of all terms sharing a given prefix into smaller ones.
///
/// Use [`CheckIndex`](crate::core::index::check_index::CheckIndex) with the `-verbose` option to see summary statistics on the
/// blocks in the dictionary.
///
/// See [`Lucene90BlockTreeTermsWriter`](crate::core::codecs::lucene90::block_tree::lucene90_block_tree_terms_writer::Lucene90BlockTreeTermsWriter).
///
/// [`Lucene90BlockTreeTermsWriter`]: crate::core::codecs::lucene90::block_tree::lucene90_block_tree_terms_writer::Lucene90BlockTreeTermsWriter
pub struct Lucene90BlockTreeTermsReader<I, PR>
where
  I: IndexInput,
  PR: PostingsReaderBase,
{
  terms_reader: Arc<TermsReader<I, PR>>,
  // Open input to the terms index file (_X.tip)
  index_in: Arc<I>,
  field_map: RwLock<HashMap<i32, FieldReader<I, PR>>>,
  field_list: Vec<String>,
  field_infos: Arc<FieldInfos>,
}
pub struct TermsReader<I, PR>
where
  I: IndexInput,
  PR: PostingsReaderBase,
{
  // Open input to the main terms dict file (_X.tib)
  pub(crate) terms_in: I,
  pub(crate) postings_reader: PR,
  pub(crate) segment: Arc<str>,
  pub(crate) version: i32,
}

impl<I, PR> CloseableRef for TermsReader<I, PR>
where
  I: IndexInput,
  PR: PostingsReaderBase,
{
  fn close(&self) -> Result<()> {
    IOUtils::close((Some(&self.terms_in), Some(&self.postings_reader)))
  }
}

impl<I, PR> Lucene90BlockTreeTermsReader<I, PR>
where
  I: IndexInput,
  PR: PostingsReaderBase,
{
  pub fn new<D1, D2>(
    postings_reader: PR,
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self>
  where
    D1: Directory<IndexInput = I>,
  {
    let segment = &segment_info.name;
    let mut postings_reader = Some(postings_reader);
    let mut terms_in = None;
    let mut index_in = None;
    let mut terms_reader = None;
    let mut shared_index_in = None;
    let mut reader = None;
    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      terms_in = Some(state.with_file_name(segment, TERMS_EXTENSION, |name| {
        state.directory.open_input(name, state.context)
      })?);
      let terms_in_ref = terms_in
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("terms input is missing"))?;
      let version = CodecUtil::check_index_header(
        terms_in_ref,
        TERMS_CODEC_NAME,
        VERSION_START,
        VERSION_CURRENT,
        segment_info.get_id(),
        &state.segment_suffix,
      )?;

      index_in = Some(
        state.with_file_name(segment, TERMS_INDEX_EXTENSION, |name| {
          state.directory.open_input(
            name,
            &state
              .context
              .with_read_advice_self(ReadAdvice::RandomPreload)?,
          )
        })?,
      );
      CodecUtil::check_index_header(
        index_in
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("terms index input is missing"))?,
        TERMS_INDEX_CODEC_NAME,
        version,
        version,
        segment_info.get_id(),
        &state.segment_suffix,
      )?;

      let mut field_map = HashMap::new();
      let mut index_length = -1i64;
      let mut terms_length = -1i64;
      let mut meta_in = state.with_file_name(segment, TERMS_META_EXTENSION, |name| {
        state.directory.open_checksum_input(name)
      })?;
      let meta_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
          CodecUtil::check_index_header(
            &mut meta_in,
            TERMS_META_CODEC_NAME,
            version,
            version,
            segment_info.get_id(),
            &state.segment_suffix,
          )?;
          postings_reader
            .as_mut()
            .ok_or_else(|| LuceneError::illegal_state("postings reader is missing"))?
            .init(&mut meta_in, state, segment_info)?;

          let num_fields = meta_in.read_vint()?;
          if num_fields < 0 {
            return Err(LuceneError::corrupt_index(format!(
              "invalid numFields: {num_fields}"
            )));
          }

          for _ in 0..num_fields {
            let field = meta_in.read_vint()?;
            let num_terms = meta_in.read_vlong()?;
            if num_terms <= 0 {
              return Err(LuceneError::corrupt_index(format!(
                "Illegal numTerms for field number: {field}"
              )));
            }

            let root_code = read_bytes_ref(&mut meta_in)?;
            let field_info = state
              .field_infos
              .field_info_by_number(field)?
              .ok_or_else(|| {
                LuceneError::corrupt_index(format!("invalid field number: {field}"))
              })?;

            let sum_total_term_freq = meta_in.read_vlong()?;
            // when frequencies are omitted, sumDocFreq=sumTotalTermFreq and only one value
            // is written.
            let sum_doc_freq = if *field_info.get_index_options() == IndexOptions::Docs {
              sum_total_term_freq
            } else {
              meta_in.read_vlong()?
            };

            let doc_count = meta_in.read_vint()?;
            let min_term = Arc::new(read_bytes_ref(&mut meta_in)?);
            let max_term = if num_terms == 1 {
              let len = read_bytes_ref_len(&mut meta_in)?;
              let bytes = meta_in.get_bytes(len)?;
              debug_assert_eq!(&bytes[..len], &min_term.bytes[..min_term.length]);
              Arc::clone(&min_term)
            } else {
              Arc::new(read_bytes_ref(&mut meta_in)?)
            };

            let max_doc = segment_info.max_doc()?;
            if doc_count < 0 || doc_count > max_doc {
              return Err(LuceneError::corrupt_index(format!(
                "invalid docCount: {doc_count} maxDoc: {max_doc}"
              )));
            }

            if sum_doc_freq < doc_count as i64 {
              return Err(LuceneError::corrupt_index(format!(
                "invalid sumDocFreq: {sum_doc_freq} docCount: {doc_count}"
              )));
            }

            if sum_total_term_freq < sum_doc_freq {
              return Err(LuceneError::corrupt_index(format!(
                "invalid sumTotalTermFreq: {sum_total_term_freq} sumDocFreq: {sum_doc_freq}"
              )));
            }

            let index_start_fp = meta_in.read_vlong()?;
            let field_reader = FieldReader::new(
              field_info.clone(),
              num_terms,
              root_code,
              sum_total_term_freq,
              sum_doc_freq,
              doc_count,
              index_start_fp,
              &mut meta_in,
              min_term,
              max_term,
            )?;

            if field_map
              .insert(field_info.get_field_number(), field_reader)
              .is_some()
            {
              return Err(LuceneError::corrupt_index(format!(
                "duplicate field: {}",
                field_info.name
              )));
            }
          }

          index_length = meta_in.read_long()?;
          terms_length = meta_in.read_long()?;
          Ok(())
        }));
        let prior_result = match result {
          Ok(Ok(())) => None,
          result => Some(result),
        };
        CodecUtil::check_footer_with_error(&mut meta_in, prior_result)
      }));
      let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| meta_in.close()));
      IOUtils::use_or_suppress_caught_result(meta_result, close_result)?;

      // At this point the checksum of the meta file has been verified so the lengths
      // are likely correct
      CodecUtil::retrieve_checksum_with_expected(
        index_in
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("terms index input is missing"))?,
        index_length as usize,
      )?;
      CodecUtil::retrieve_checksum_with_expected(
        terms_in
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("terms input is missing"))?,
        terms_length as usize,
      )?;

      terms_reader = Some(Arc::new(TermsReader {
        terms_in: terms_in
          .take()
          .ok_or_else(|| LuceneError::illegal_state("terms input is missing"))?,
        postings_reader: postings_reader
          .take()
          .ok_or_else(|| LuceneError::illegal_state("postings reader is missing"))?,
        segment: Arc::clone(segment),
        version,
      }));
      shared_index_in =
        Some(Arc::new(index_in.take().ok_or_else(|| {
          LuceneError::illegal_state("terms index input is missing")
        })?));
      for field_reader in field_map.values_mut() {
        field_reader.parent = terms_reader.as_ref().map(Arc::clone);
        FieldReader::init_field_reader(
          shared_index_in
            .as_ref()
            .ok_or_else(|| LuceneError::illegal_state("terms index input is missing"))?
            .clone(),
          field_reader,
        )?;
      }
      let field_list = sort_field_names(&field_map, &state.field_infos)?;
      reader = Some(Lucene90BlockTreeTermsReader {
        terms_reader: terms_reader
          .take()
          .ok_or_else(|| LuceneError::illegal_state("terms reader is missing"))?,
        index_in: shared_index_in
          .take()
          .ok_or_else(|| LuceneError::illegal_state("terms index input is missing"))?,
        field_map: RwLock::new(field_map),
        field_list,
        field_infos: Arc::clone(&state.field_infos),
      });
      success = true;
      Ok(())
    }));

    if !success {
      IOUtils::close_while_handling_exception((
        shared_index_in.as_ref(),
        terms_reader.as_ref(),
        index_in.as_ref(),
        terms_in.as_ref(),
        postings_reader.as_ref(),
      ));
    }
    unwrap_caught_result!(result)?;
    reader
      .take()
      .ok_or_else(|| LuceneError::illegal_state("block tree terms reader is missing"))
  }
}
impl<I, PR> Fields for Lucene90BlockTreeTermsReader<I, PR>
where
  I: IndexInput,
  PR: PostingsReaderBase,
{
  type FieldIter<'a>
    = VecIter<'a, String>
  where
    I: 'a,
    PR: 'a;

  fn iterator(&self) -> Result<Self::FieldIter<'_>> {
    Ok(self.field_list.iter_ext())
  }

  type Terms = FieldReader<I, PR>;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    let Some(field_info) = self.field_infos.field_info_by_name(field)? else {
      return Ok(None);
    };
    self
      .field_map
      .read()
      .get(&field_info.number)
      .map(TryClone::try_clone)
      .transpose()
  }

  fn size(&self) -> Result<i32> {
    self.field_map.read().len().try_convert()
  }
}
impl<I, PR> Display for Lucene90BlockTreeTermsReader<I, PR>
where
  I: IndexInput,
  PR: PostingsReaderBase + Display,
{
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "Lucene90BlockTreeTermsReader(fields={}, delegate={})",
      self.field_map.read().len(),
      self.terms_reader.postings_reader
    )
  }
}

impl<I, PR> CloseableRef for Lucene90BlockTreeTermsReader<I, PR>
where
  I: IndexInput,
  PR: PostingsReaderBase,
{
  fn close(&self) -> Result<()> {
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      IOUtils::close((
        Some(self.index_in.as_ref()),
        Some(self.terms_reader.as_ref()),
      ))
    }));

    // Clear so refs to terms index are releasable even if the caller hangs onto us.
    self.field_map.write().clear();
    unwrap_caught_result!(close_result)
  }
}

impl<I, PR> FieldsProducer for Lucene90BlockTreeTermsReader<I, PR>
where
  I: IndexInput,
  PR: PostingsReaderBase,
{
  fn check_integrity(&self) -> Result<()> {
    CodecUtil::checksum_entire_file(self.index_in.as_ref())?;
    CodecUtil::checksum_entire_file(&self.terms_reader.terms_in)?;
    self.terms_reader.postings_reader.check_integrity()
  }
}

use crate::core::index::BytesRef;
use crate::core::util::TryIntoInt;
use crate::core::util::iterator::{VecIter, VecIteratorExt};

/// Arc outputs in the terms index may be empty; decoded block buffers are ordinary bytes.
pub(crate) type IndexArc = crate::core::util::fst_impl::fst::Arc<BytesRef<std::sync::Arc<Vec<u8>>>>;

pub(crate) const OUTPUT_FLAGS_NUM_BITS: i32 = 2;
#[allow(dead_code)]
pub(crate) const OUTPUT_FLAGS_MASK: i32 = 0x3;
pub(crate) const OUTPUT_FLAG_IS_FLOOR: i32 = 0x1;
pub(crate) const OUTPUT_FLAG_HAS_TERMS: i32 = 0x2;

/// Extension of terms file
pub(crate) const TERMS_EXTENSION: &str = "tim";
pub(crate) const TERMS_CODEC_NAME: &str = "BlockTreeTermsDict";
/// Initial terms format
pub const VERSION_START: i32 = 0;
/// Version that encodes output as MSB VLong for better FST sharing
/// (GITHUB#12620)
pub const VERSION_MSB_VLONG_OUTPUT: i32 = 1;
/// Version that specializes arc store for continuous label in FST
pub const VERSION_FST_CONTINUOUS_ARCS: i32 = 2;
/// Current terms format version
pub const VERSION_CURRENT: i32 = VERSION_FST_CONTINUOUS_ARCS;
/// Extension of terms index file
pub(crate) const TERMS_INDEX_EXTENSION: &str = "tip";
pub(crate) const TERMS_INDEX_CODEC_NAME: &str = "BlockTreeTermsIndex";
/// Extension of terms meta file
pub(crate) const TERMS_META_EXTENSION: &str = "tmd";
pub(crate) const TERMS_META_CODEC_NAME: &str = "BlockTreeTermsMeta";
fn read_bytes_ref<I>(input: &mut I) -> Result<BytesRef<Vec<u8>>>
where
  I: IndexInput,
{
  let num_bytes = read_bytes_ref_len(input)?;
  let buffer = input.get_bytes(num_bytes)?.into_owned();
  Ok(BytesRef::from_slice(buffer, 0, num_bytes))
}

fn read_bytes_ref_len<I: IndexInput>(input: &mut I) -> Result<usize> {
  let num_bytes = input.read_vint()?;
  if num_bytes < 0 {
    return Err(LuceneError::corrupt_index(format!(
      "invalid bytes length: {num_bytes} (resource={input})"
    )));
  }
  Ok(num_bytes as usize)
}
fn sort_field_names<I, PR>(
  field_map: &HashMap<i32, FieldReader<I, PR>>,
  field_infos: &FieldInfos,
) -> Result<Vec<String>>
where
  I: IndexInput,
  PR: PostingsReaderBase,
{
  let mut field_names = Vec::with_capacity(field_map.len());

  for field_number in field_map.keys() {
    let field_info = field_infos
      .field_info_by_number(*field_number)?
      .ok_or_else(|| {
        LuceneError::illegal_state(format!(
          "Missing field info for field number {field_number}"
        ))
      })?;
    field_names.push(field_info.name.clone());
  }

  field_names.sort_unstable();
  Ok(field_names)
}
