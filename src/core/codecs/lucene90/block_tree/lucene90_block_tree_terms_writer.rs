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
use crate::core::codecs::block_tree::compression_algorithm::CompressionAlgorithm;

use crate::core::codecs::block_tree::lucene90_block_tree_terms_reader::{
  TERMS_CODEC_NAME, TERMS_EXTENSION, TERMS_INDEX_CODEC_NAME, TERMS_INDEX_EXTENSION,
  TERMS_META_CODEC_NAME, TERMS_META_EXTENSION, VERSION_CURRENT, VERSION_MSB_VLONG_OUTPUT,
  VERSION_START,
};
use crate::core::codecs::fields_consumer::FieldsConsumer;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::postings_writer_base::PostingsWriterBase;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::fields::Fields;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::postings_enum::PostingsEnum;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::index::{BytesRef, BytesRefBuilder, IndexFileNames};
use crate::core::store::directory::Directory;
use crate::core::store::dummy::dummy_directory::DummyDirectory;
use crate::core::store::{ByteArrayDataOutput, ByteBuffersDataOutput, DataOutput, IndexOutput};
use crate::core::util::IOUtils;
#[cfg(debug_assertions)]
use crate::core::util::ToInt;
use crate::core::util::access::{ByteSourceMut, SharedAccessVec};
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::close::Closeable;
use crate::core::util::compress::lowercase_ascii_compression::LowercaseAsciiCompression;
use crate::core::util::compress::lz4::{HashTableEnum, HighCompressionHashTable, LZ4};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::fst_impl::byte_sequence_outputs::ByteSequenceOutputs;
use crate::core::util::fst_impl::bytes_ref_fst_enum::BytesRefFSTEnum;
use crate::core::util::fst_impl::fst::{FST, InputType, VERSION_90};
use crate::core::util::fst_impl::fst_compiler::{
  Builder, DataOutputEnum, FSTCompiler, get_on_heap_reader_writer,
};
use crate::core::util::fst_impl::util::Util;
use crate::core::util::ints_ref_builder::IntsRefBuilder;
use crate::core::util::packed::PackedInts;
use crate::core::util::to_string_utils::ToStringUtils;
use crate::core::util::{CoreHelper, SliceCopyOps, StringHelper, TryIntoInt};
use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;
/*
 TODO:

   - Currently there is a one-to-one mapping of indexed
     term to term block, but we could decouple the two, ie,
     put more terms into the index than there are blocks.
     The index would take up more RAM but then it'd be able
     to avoid seeking more often and could make PK/FuzzyQ
     faster if the additional indexed terms could store
     the offset into the terms block.

   - The blocks are not written in true depth-first
     order, meaning if you just next() the file pointer will
     sometimes jump backwards.  For example, block foo* will
     be written before block f* because it finished before.
     This could possibly hurt performance if the terms dict is
     not hot, since OSs anticipate sequential file access.  We
     could fix the writer to re-order the blocks as a 2nd
     pass.

   - Each block encodes the term suffixes packed
     sequentially using a separate vInt per term, which is
     1) wasteful and 2) slow (must linear scan to find a
     particular suffix).  We should instead 1) make
     random-access array so we can directly access the Nth
     suffix, and 2) bulk-encode this array using bulk int[]
     codecs; then at search time we can binary search when
     we seek a particular term.
*/

/// Block-based terms index and dictionary writer.
///
/// Writes terms dict and index, block-encoding (column stride) each term's
/// metadata for each set of terms between two index terms.
///
/// Files:
///
/// - `.tim`: Term Dictionary
/// - `.tmd`: Term Metadata
/// - `.tip`: Term Index
///
/// ---
///
/// ## Term Dictionary (.tim)
///
/// The `.tim` file contains the list of terms in each field along with per-term
/// statistics (such as docFreq) and per-term metadata (typically pointers to
/// the postings list for that term in the inverted index).
///
/// The `.tim` file is arranged in blocks: each block contains a variable number
/// of entries (by default 25–48), where each entry is either a term or a
/// reference to a sub-block.
///
/// **NOTE:** The term dictionary can plug into different postings
/// implementations: the postings writer/reader are responsible for encoding and
/// decoding the postings metadata and term metadata.
///
/// Structure:
///
/// - TermsDict (.tim) → Header, FieldDict<sup>NumFields</sup>, Footer
/// - FieldDict → PostingsHeader, NodeBlock<sup>NumBlocks</sup>
/// - NodeBlock → OuterNode | InnerNode
/// - OuterNode → EntryCount, SuffixLength, Bytes<sup>SuffixLength</sup>,
///   StatsLength, TermStats<sup>EntryCount</sup>, MetaLength,
///   TermMetadata<sup>EntryCount</sup>
/// - InnerNode → EntryCount, SuffixLength`[, Sub?]`,
///   Bytes<sup>SuffixLength</sup>, StatsLength,
///   TermStats?<sup>EntryCount</sup>, MetaLength,
///   TermMetadata?<sup>EntryCount</sup>
/// - TermStats → DocFreq, TotalTermFreq
/// - Header → `CodecUtil::write_header`
/// - EntryCount, SuffixLength, StatsLength, DocFreq, MetaLength → `write_vint`
/// - TotalTermFreq → `write_vlong`
/// - Footer → `CodecUtil::write_footer`
///
/// Notes:
///
/// - Header stores version information for the BlockTree implementation.
/// - `DocFreq` is the number of documents that contain the term.
/// - `TotalTermFreq` is the total number of occurrences of the term, encoded as
///   the delta from `DocFreq`.
/// - `PostingsHeader` and `TermMetadata` are pluggable and format-specific.
/// - Inner node entries use a bit to mark sub-blocks; in that case, TermStats
///   and TermMetadata are omitted.
///
/// ---
///
/// ## Term Metadata (.tmd)
///
/// The `.tmd` file contains term metadata (e.g. FST index metadata) and
/// field-level statistics (e.g. sumTotalTermFreq).
///
/// Structure:
///
/// - TermsMeta (.tmd) → Header, NumFields, FieldStats<sup>NumFields</sup>,
///   TermIndexLength, TermDictLength, Footer
/// - FieldStats → FieldNumber, NumTerms, RootCodeLength,
///   Bytes<sup>RootCodeLength</sup>, SumTotalTermFreq?, SumDocFreq, DocCount,
///   MinTerm, MaxTerm, IndexStartFP, FSTHeader, FSTMetadata
///
/// Encoding:
///
/// - Header, FSTHeader → [`CodecUtil::write_header`](CodecUtil::write_header)
/// - TermIndexLength, TermDictLength → `write_long`
/// - MinTerm, MaxTerm → `write_vint` + Bytes
/// - NumFields, FieldNumber, RootCodeLength, DocCount → `write_vint`
/// - NumTerms, SumTotalTermFreq, SumDocFreq, IndexStartFP → `write_vlong`
/// - Footer → `CodecUtil::write_footer`
///
/// Notes:
///
/// - `FieldNumber` comes from `.fnm` (`FieldInfos`)
/// - `NumTerms` is the number of unique terms for the field
/// - `RootCode` points to the root block of the field
/// - `SumDocFreq` counts the number of term-document pairs
/// - `DocCount` is the number of documents that have at least one term in the
///   field
/// - `MinTerm` / `MaxTerm` are the smallest/largest lexicographic terms
///
/// ---
///
/// ## Term Index (.tip)
///
/// The `.tip` file contains an index into the term dictionary, allowing
/// efficient random access. The index also helps determine when a term does not
/// exist on disk.
///
/// Structure:
///
/// - TermsIndex (.tip) → Header, FSTIndex<sup>NumFields</sup>, Footer
/// - Header → `CodecUtil::write_header`
/// - FSTIndex → `FST<BytesRef>`
/// - Footer → `CodecUtil::write_footer`
///
/// Notes:
/// - The .tip file contains a separate FST for each field. The FST maps a term
///   prefix to the on-disk block that holds all terms starting with that
///   prefix. Each field's IndexStartFP points to its FST.
/// - It's possible that an on-disk block would contain too many terms (more than
///   the allowed maximum (default: 48)). When this happens, the block is
///   subdivided into new blocks (called "floor blocks"), and then the output
///   in the FST for the block's prefix encodes the leading byte of each
///   subblock,and its file pointer.
///
/// See also [`Lucene90BlockTreeTermsReader`](crate::core::codecs::lucene90::block_tree::lucene90_block_tree_terms_reader::Lucene90BlockTreeTermsReader).
pub struct Lucene90BlockTreeTermsWriter<O, PW>
where
  O: IndexOutput,
  PW: PostingsWriterBase,
{
  meta_out: O,
  terms_out: O,
  index_out: O,
  max_doc: i32,
  min_items_in_block: i32,
  max_items_in_block: i32,
  version: i32,
  postings_writer: PW,
  field_infos: Arc<FieldInfos>,
  fields: Vec<ByteBuffersDataOutput>,
  closed: bool,
}
impl<O, PW> Lucene90BlockTreeTermsWriter<O, PW>
where
  O: IndexOutput,
  PW: PostingsWriterBase,
{
  /// Create a new writer. The number of items (terms or sub-blocks) per block will aim to be between
  /// `min_items_per_block` and `max_items_per_block`, though in some cases the blocks may be smaller than the
  /// min.
  pub fn new<D1, D2>(
    state: &SegmentWriteState<D1>,
    postings_writer: PW,
    min_items_in_block: i32,
    max_items_in_block: i32,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self>
  where
    D1: Directory<IndexOutput = O>,
    D2: Directory,
  {
    Self::with_version(
      state,
      postings_writer,
      min_items_in_block,
      max_items_in_block,
      VERSION_CURRENT,
      segment_info,
    )
  }
  /// Creates a writer with an explicit version for backward-compatibility tests.
  pub fn with_version<D1, D2>(
    state: &SegmentWriteState<D1>,
    mut postings_writer: PW,
    min_items_in_block: i32,
    max_items_in_block: i32,
    version: i32,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self>
  where
    D1: Directory<IndexOutput = O>,
    D2: Directory,
  {
    let mut max_doc = 0;
    let field_infos = Arc::clone(&state.field_infos);
    let mut terms_out = None;
    let setup_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      Self::validate_settings(min_items_in_block, max_items_in_block)?;
      if !(VERSION_START..=VERSION_CURRENT).contains(&version) {
        return Err(LuceneError::illegal_argument(format!(
          "Expected version in range [{}, {}], but got {}",
          VERSION_START, VERSION_CURRENT, version
        )));
      }
      max_doc = segment_info.max_doc()?;
      let terms_name = IndexFileNames::segment_file_name(
        &segment_info.name,
        &state.segment_suffix,
        TERMS_EXTENSION,
      );
      terms_out = Some(state.directory.create_output(&terms_name, state.context)?);
      Ok(())
    }));
    match setup_result {
      Ok(Ok(())) => {},
      Ok(Err(error)) => {
        IOUtils::close_resources_while_handling_error((terms_out.as_mut(), &mut postings_writer))?;
        return Err(error);
      },
      Err(payload) => {
        IOUtils::close_resources_while_handling_error((terms_out.as_mut(), &mut postings_writer))?;
        std::panic::resume_unwind(payload);
      },
    }
    let mut terms_out = match terms_out {
      Some(terms_out) => terms_out,
      None => {
        IOUtils::close_resources_while_handling_error(&mut postings_writer)?;
        return Err(LuceneError::illegal_state("terms output is missing"));
      },
    };
    let mut meta_out = None;
    let mut index_out = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      CodecUtil::write_index_header(
        &mut terms_out,
        TERMS_CODEC_NAME,
        version,
        segment_info.get_id(),
        &state.segment_suffix,
      )?;
      let index_name = IndexFileNames::segment_file_name(
        &segment_info.name,
        &state.segment_suffix,
        TERMS_INDEX_EXTENSION,
      );
      index_out = Some(state.directory.create_output(&index_name, state.context)?);
      CodecUtil::write_index_header(
        index_out
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("terms index output is missing"))?,
        TERMS_INDEX_CODEC_NAME,
        version,
        segment_info.get_id(),
        &state.segment_suffix,
      )?;
      let meta_name = IndexFileNames::segment_file_name(
        &segment_info.name,
        &state.segment_suffix,
        TERMS_META_EXTENSION,
      );
      meta_out = Some(state.directory.create_output(&meta_name, state.context)?);
      CodecUtil::write_index_header(
        meta_out
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("terms metadata output is missing"))?,
        TERMS_META_CODEC_NAME,
        version,
        segment_info.get_id(),
        &state.segment_suffix,
      )?;

      postings_writer.init(
        meta_out
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("terms metadata output is missing"))?,
        state,
        segment_info,
      )
    }));

    match result {
      Ok(Ok(())) => {},
      Ok(Err(error)) => {
        IOUtils::close_resources_while_handling_error((
          meta_out.as_mut(),
          &mut terms_out,
          index_out.as_mut(),
          &mut postings_writer,
        ))?;
        return Err(error);
      },
      Err(payload) => {
        IOUtils::close_resources_while_handling_error((
          meta_out.as_mut(),
          &mut terms_out,
          index_out.as_mut(),
          &mut postings_writer,
        ))?;
        std::panic::resume_unwind(payload);
      },
    }
    let (meta_out, index_out) = match (meta_out, index_out) {
      (Some(meta_out), Some(index_out)) => (meta_out, index_out),
      (mut meta_out, mut index_out) => {
        IOUtils::close_resources_while_handling_error((
          meta_out.as_mut(),
          &mut terms_out,
          index_out.as_mut(),
          &mut postings_writer,
        ))?;
        return Err(LuceneError::illegal_state(
          "terms outputs are missing after successful construction",
        ));
      },
    };

    Ok(Self {
      meta_out,
      terms_out,
      index_out,
      max_doc,
      min_items_in_block,
      max_items_in_block,
      version,
      postings_writer,
      field_infos,
      fields: vec![],
      closed: false,
    })
  }
  /// Returns `IllegalArgumentError` if any setting is invalid.
  pub fn validate_settings(min_items_in_block: i32, max_items_in_block: i32) -> Result<()> {
    if min_items_in_block <= 1 {
      return Err(LuceneError::illegal_argument(format!(
        "min_items_in_block must be >= 2; got {min_items_in_block}"
      )));
    }

    if min_items_in_block > max_items_in_block {
      return Err(LuceneError::illegal_argument(format!(
        "max_items_in_block must be >= min_items_in_block; got max_items_in_block={max_items_in_block}, min_items_in_block={min_items_in_block}"
      )));
    }

    if 2 * (min_items_in_block - 1) > max_items_in_block {
      return Err(LuceneError::illegal_argument(format!(
        "max_items_in_block must be at least 2*(min_items_in_block-1); got max_items_in_block={max_items_in_block}, min_items_in_block={min_items_in_block}"
      )));
    }

    Ok(())
  }
}
impl<O, PW> FieldsConsumer for Lucene90BlockTreeTermsWriter<O, PW>
where
  O: IndexOutput,
  PW: PostingsWriterBase,
{
  type IndexOutput = O;

  fn write<D1, D2, F, N>(
    &mut self,
    _state: &SegmentWriteState<D1>,
    _segment_info: &SegmentInfo<D2>,
    fields: &mut F,
    norms: Option<&N>,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = O>,
    D2: Directory,
    F: Fields,
    PW: PostingsWriterBase,
    N: NormsProducer,
  {
    #[cfg(debug_assertions)]
    let mut last_field: Option<String> = None;
    let mut field_names = fields.iterator()?;
    while field_names.has_next()? {
      match field_names.next()? {
        Some(field) => {
          #[cfg(debug_assertions)]
          {
            debug_assert!({
              let v = last_field.is_none() || last_field.as_ref().unwrap().cmp(field).to_int() < 0;
              last_field = Some(field.clone());
              v
            });
          }

          let Some(terms) = fields.terms(field)? else {
            continue;
          };
          let mut terms_enum = terms.iterator()?;
          let field_info = self
            .field_infos
            .field_info_by_name(field)?
            .ok_or_else(|| LuceneError::illegal_state(format!("Missing fields:{field}")))?;
          let mut terms_writer = TermsWriter::new(
            field_info.clone(),
            self.max_doc,
            &mut self.postings_writer,
            self.min_items_in_block,
            self.max_items_in_block,
            self.version,
            &mut self.terms_out,
          )?;
          let mut reuse = None;
          while let Some(byte_ref) = terms_enum.next()? {
            // due to borrow check, we have to clone here early for init PendingTerm
            let term = BytesRef::from_bytes(
              byte_ref.bytes[byte_ref.offset..byte_ref.offset + byte_ref.length].to_vec(),
            );
            reuse = terms_writer.write(term, &mut terms_enum, norms, reuse)?;
          }
          terms_writer.finish(&mut self.fields, &mut self.index_out)?;
        },
        None => break,
      }
    }
    Ok(())
  }
}

impl<O, PW> Closeable for Lucene90BlockTreeTermsWriter<O, PW>
where
  O: IndexOutput,
  PW: PostingsWriterBase,
{
  fn close(&mut self) -> Result<()> {
    if self.closed {
      return Ok(());
    }
    self.closed = true;

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      self.meta_out.write_vint(self.fields.len() as i32)?;
      for field_meta in &self.fields {
        field_meta.copy_to(&mut self.meta_out)?;
      }
      CodecUtil::write_footer(&mut self.index_out)?;
      self
        .meta_out
        .write_long(self.index_out.get_file_pointer()? as i64)?;
      CodecUtil::write_footer(&mut self.terms_out)?;
      self
        .meta_out
        .write_long(self.terms_out.get_file_pointer()? as i64)?;
      CodecUtil::write_footer(&mut self.meta_out)
    }));
    match result {
      Ok(Ok(())) => IOUtils::close(0..4, |operation| match operation {
        0 => self.meta_out.close(),
        1 => self.terms_out.close(),
        2 => self.index_out.close(),
        3 => self.postings_writer.close(),
        _ => unreachable!(),
      }),
      Ok(Err(error)) => {
        IOUtils::close_resources_while_handling_error((
          &mut self.meta_out,
          &mut self.terms_out,
          &mut self.index_out,
          &mut self.postings_writer,
        ))?;
        Err(error)
      },
      Err(payload) => {
        IOUtils::close_resources_while_handling_error((
          &mut self.meta_out,
          &mut self.terms_out,
          &mut self.index_out,
          &mut self.postings_writer,
        ))?;
        std::panic::resume_unwind(payload)
      },
    }
  }
}
impl<O, PW> Drop for Lucene90BlockTreeTermsWriter<O, PW>
where
  O: IndexOutput,
  PW: PostingsWriterBase,
{
  fn drop(&mut self) {
    match self.close() {
      Ok(_) => {},
      Err(e) => {
        eprintln!("Error closing Lucene90BlockTreeTermsWriter: {}", e);
      },
    }
  }
}
trait PendingEntry {
  fn is_term(&self) -> bool;
}
pub struct PendingTerm {
  pub term_bytes: Arc<Vec<u8>>,
  pub state: TermStateEnum,
}
impl Default for PendingTerm {
  fn default() -> Self {
    Self {
      term_bytes: Arc::new(vec![]),
      state: TermStateEnum::Int(Default::default()),
    }
  }
}

impl PendingEntry for PendingTerm {
  fn is_term(&self) -> bool {
    true
  }
}

impl PendingTerm {
  pub fn new(mut term: BytesRef<Vec<u8>>, state: TermStateEnum) -> Self {
    debug_assert!(term.offset == 0);
    debug_assert!(term.length == term.bytes.len());
    Self {
      term_bytes: Arc::new(std::mem::take(&mut term.bytes)),
      state,
    }
  }
}
impl fmt::Display for PendingTerm {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let s = ToStringUtils::bytes_ref_to_string_from_bytes(self.term_bytes.clone().to_vec());
    write!(f, "TERM: {s}")
  }
}
pub struct PendingBlock {
  pub prefix: BytesRef<Vec<u8>>,
  pub fp: i64,
  pub index: Option<FST<ByteSequenceOutputs, DataOutputEnum<DummyDirectory>>>,
  pub sub_indices: Vec<FST<ByteSequenceOutputs, DataOutputEnum<DummyDirectory>>>,
  pub has_terms: bool,
  pub is_floor: bool,
  pub floor_lead_byte: i32,
}
impl PendingBlock {
  pub fn new(
    prefix: BytesRef<Vec<u8>>,
    fp: i64,
    has_terms: bool,
    is_floor: bool,
    floor_lead_byte: i32,
    sub_indices: Vec<FST<ByteSequenceOutputs, DataOutputEnum<DummyDirectory>>>,
  ) -> Self {
    Self {
      prefix,
      fp,
      has_terms,
      is_floor,
      floor_lead_byte,
      index: None,
      sub_indices,
    }
  }
  fn compile_index(
    mut blocks: Vec<PendingBlock>,
    scratch_bytes: &mut ByteBuffersDataOutput,
    scratch_ints_ref: &mut IntsRefBuilder<Vec<i32>>,
    version: i32,
  ) -> Result<PendingBlock> {
    debug_assert!(
      (blocks.len() > 1 && blocks[0].is_floor) || (!blocks[0].is_floor && blocks.len() == 1),
      "is_floor={}, blocks.len()={}",
      blocks[0].is_floor,
      blocks.len()
    );
    debug_assert_eq!(scratch_bytes.size(), 0);

    let (is_floor, fp, prefix_len) = {
      let first_block = &mut blocks[0];
      let output = encode_output(first_block.fp, first_block.has_terms, first_block.is_floor);
      if version >= VERSION_MSB_VLONG_OUTPUT {
        write_msb_vlong(scratch_bytes, output)?;
      } else {
        scratch_bytes.write_vlong(output)?;
      }
      (
        first_block.is_floor,
        first_block.fp,
        first_block.prefix.length,
      )
    };

    if is_floor {
      debug_assert!((blocks.len() - 1) <= i32::MAX as usize);
      scratch_bytes.write_vint((blocks.len() - 1) as i32)?;
      for block in &blocks[1..] {
        debug_assert!(block.floor_lead_byte != -1);
        scratch_bytes.write_byte(block.floor_lead_byte as u8)?;
        debug_assert!(block.fp > fp);
        let delta_fp = ((block.fp - fp) << 1) | if block.has_terms { 1 } else { 0 };
        scratch_bytes.write_vlong(delta_fp)?;
      }
    }

    let mut estimate_size = prefix_len as i64;
    for block in blocks.iter() {
      for sub_index in &block.sub_indices {
        estimate_size += sub_index.num_bytes();
      }
    }

    let estimate_bits_required = PackedInts::bits_required(estimate_size)?;
    let page_bits = estimate_bits_required.clamp(6, 15);

    let outputs = ByteSequenceOutputs::get_singleton();
    let fst_version = if version >= VERSION_CURRENT {
      VERSION_CURRENT
    } else {
      VERSION_90
    };

    let mut builder = Builder::new(InputType::Byte1, outputs.clone());
    // Disable suffixes sharing for block tree index because suffixes are mostly
    // dropped from the FST index and left in the term blocks.
    builder.suffix_ram_limit_mb(0.0)?;
    builder.data_output(DataOutputEnum::ReadWriter(get_on_heap_reader_writer(
      page_bits,
    )?));
    builder.with_version(fst_version)?;
    let mut fst_compiler = builder.build()?;

    let bytes = scratch_bytes.get_array_copy();
    let len = bytes.len();
    debug_assert!(!bytes.is_empty());

    Util::to_ints_ref(&blocks[0].prefix, scratch_ints_ref)?;
    fst_compiler.add(
      scratch_ints_ref.get(),
      BytesRef::from_slice(Arc::from(bytes), 0, len),
    )?;
    scratch_bytes.reset();

    for block in blocks.iter_mut() {
      for sub_index in std::mem::take(&mut block.sub_indices) {
        block.append(&mut fst_compiler, sub_index, scratch_ints_ref)?;
      }
    }
    let first_block = &mut blocks[0];
    let meta = fst_compiler
      .compile()?
      .ok_or_else(|| LuceneError::number_format("fst_metadata is None"))?;
    first_block.index = Some(FST::from_fst_reader(meta, fst_compiler.get_fst_reader()?).unwrap());

    debug_assert!(first_block.sub_indices.is_empty());
    Ok(blocks.remove(0))
  }
  fn append(
    &self,
    fst_compiler: &mut FSTCompiler<ByteSequenceOutputs, DummyDirectory>,
    sub_index: FST<ByteSequenceOutputs, DataOutputEnum<DummyDirectory>>,
    scratch_ints_ref: &mut IntsRefBuilder<Vec<i32>>,
  ) -> Result<()> {
    let mut sub_index_enum = BytesRefFSTEnum::new(sub_index)?;

    while let Some(index_ent) = sub_index_enum.next_value()? {
      Util::to_ints_ref(&index_ent.input, scratch_ints_ref)?;
      fst_compiler.add(scratch_ints_ref.get(), index_ent.output.clone())?;
    }

    Ok(())
  }
}

struct StatsWriter<'a, DO>
where
  DO: DataOutput,
{
  has_freqs: bool,
  singleton_count: i32,
  out: &'a mut DO,
}

impl<'a, DO> StatsWriter<'a, DO>
where
  DO: DataOutput,
{
  fn new(out: &'a mut DO, has_freqs: bool) -> Self {
    Self {
      has_freqs,
      singleton_count: 0,
      out,
    }
  }
  fn add(&mut self, df: i32, ttf: i64) -> Result<()> {
    if df == 1 && (!self.has_freqs || ttf == 1) {
      self.singleton_count += 1;
    } else {
      self.finish()?;
      self.out.write_vint(df << 1)?;
      if self.has_freqs {
        self.out.write_vlong(ttf - df as i64)?;
      }
    }
    Ok(())
  }

  fn finish(&mut self) -> Result<()> {
    if self.singleton_count > 0 {
      self.out.write_vint(((self.singleton_count - 1) << 1) | 1)?;
      self.singleton_count = 0;
    }
    Ok(())
  }
}
pub struct TermsWriter<'a, O, PW>
where
  O: IndexOutput,
  PW: PostingsWriterBase,
{
  field_info: Arc<FieldInfo>,
  num_terms: i64,
  docs_seen: FixedBitSet,
  sum_total_term_freq: i64,
  sum_doc_freq: i64,
  // Records index into pending where the current prefix at that
  // length "started"; for example, if current term starts with 't',
  // startsByPrefix[0] is the index into pending for the first
  // term/sub-block starting with 't'.  We use this to figure out when
  // to write a new block:
  last_term: BytesRefBuilder<Vec<u8>>,
  prefix_starts: Vec<i32>,
  // Pending stack of terms and blocks.  As terms arrive (in sorted order)
  // we append to this stack, and once the top of the stack has enough
  // terms starting with a common prefix, we write a new block with
  // those terms and replace those terms in the stack with a new block:
  pending: Vec<PendingEntryEnum>,
  // Reused in writeBlocks:
  new_blocks: Vec<PendingBlock>,
  suffix_lengths_writer: ByteBuffersDataOutput,
  suffix_writer: BytesRefBuilder<Vec<u8>>,
  stats_writer: ByteBuffersDataOutput,
  meta_writer: ByteBuffersDataOutput,
  spare_writer: ByteBuffersDataOutput,
  spare_bytes: Vec<u8>,
  compression_hash_table: Option<HashTableEnum>,
  min_items_in_block: i32,
  max_items_in_block: i32,
  scratch_bytes: ByteBuffersDataOutput,
  scratch_ints_ref: IntsRefBuilder<Vec<i32>>,
  version: i32,
  first_pending_term_bytes: Option<Arc<Vec<u8>>>,
  last_pending_term_bytes: Arc<Vec<u8>>,
  terms_out: &'a mut O,
  postings_writer: &'a mut PW,
}
impl<'a, O, PW> TermsWriter<'a, O, PW>
where
  O: IndexOutput,
  PW: PostingsWriterBase,
{
  fn new(
    field_info: Arc<FieldInfo>,
    max_doc: i32,
    postings_writer: &'a mut PW,
    min_items_in_block: i32,
    max_items_in_block: i32,
    version: i32,
    terms_out: &'a mut O,
  ) -> Result<Self> {
    debug_assert_ne!(*field_info.get_index_options(), IndexOptions::None);

    postings_writer.set_field(field_info.clone());

    let v = Self {
      field_info,
      num_terms: 0,
      docs_seen: FixedBitSet::new(max_doc.try_convert()?),
      sum_total_term_freq: 0,
      sum_doc_freq: 0,
      last_term: BytesRefBuilder::new(),
      prefix_starts: vec![0; 8],
      pending: Vec::new(),
      new_blocks: Vec::new(),
      suffix_lengths_writer: ByteBuffersDataOutput::new_resettable_instance(),
      suffix_writer: BytesRefBuilder::new(),
      stats_writer: ByteBuffersDataOutput::new_resettable_instance(),
      meta_writer: ByteBuffersDataOutput::new_resettable_instance(),
      spare_writer: ByteBuffersDataOutput::new_resettable_instance(),
      spare_bytes: Vec::new(),
      compression_hash_table: None,
      min_items_in_block,
      max_items_in_block,
      scratch_bytes: ByteBuffersDataOutput::new_resettable_instance(),
      scratch_ints_ref: IntsRefBuilder::new(),
      version,
      first_pending_term_bytes: None,
      last_pending_term_bytes: Arc::new(vec![]),
      terms_out,
      postings_writer,
    };
    Ok(v)
  }
  pub fn write_blocks(&mut self, prefix_length: usize, count: usize) -> Result<()> {
    debug_assert!(count > 0);
    // Root block better write all remaining pending entries:
    debug_assert!(prefix_length > 0 || count == self.pending.len());

    let mut last_suffix_lead_label = -1;
    // True if we saw at least one term in this block (we record if a block
    // only points to sub-blocks in the terms index so we can avoid seeking
    // to it when we are looking for a term):
    let mut has_terms = false;
    let mut has_sub_blocks = false;

    let start = self.pending.len() - count;
    let end = self.pending.len();
    let mut next_block_start = start;
    let mut next_floor_lead_label = -1;

    for i in start..end {
      let (suffix_lead_label, is_term) = {
        match &self.pending[i] {
          PendingEntryEnum::Term(term) => {
            let v = if term.term_bytes.len() == prefix_length {
              // Suffix is 0, i.e. prefix 'foo' and term is
              // 'foo' so the term has empty string suffix
              // in this block
              debug_assert_eq!(
                last_suffix_lead_label, -1,
                "i={i} last_suffix_lead_label={last_suffix_lead_label}"
              );
              -1
            } else {
              term.term_bytes[prefix_length] as i32
            };
            (v, true)
          },
          PendingEntryEnum::Block(block) => {
            debug_assert!(block.prefix.length > prefix_length);
            (
              block.prefix.bytes[block.prefix.offset + prefix_length] as i32,
              false,
            )
          },
        }
      };

      if suffix_lead_label != last_suffix_lead_label {
        let items_in_block = i - next_block_start;
        if items_in_block >= self.min_items_in_block as usize
          && end - next_block_start > self.max_items_in_block as usize
        {
          // The count is too large for one block, so we must break it into "floor"
          // blocks, where we record
          // the leading label of the suffix of the first term in each floor block, so at
          // search time we can
          // jump to the right floor block.  We just use a naive greedy segmenter here:
          // make a new floor
          // block as soon as we have at least minItemsInBlock.  This is not always best:
          // it often produces
          // a too-small block as the final block:
          let is_floor = items_in_block < count;
          let block = self.write_block(
            prefix_length,
            is_floor,
            next_floor_lead_label,
            next_block_start,
            i,
            has_terms,
            has_sub_blocks,
          )?;
          self.new_blocks.push(block);

          has_terms = false;
          has_sub_blocks = false;
          next_floor_lead_label = suffix_lead_label;
          next_block_start = i;
        }
        last_suffix_lead_label = suffix_lead_label;
      }
      if is_term {
        has_terms = true;
      } else {
        has_sub_blocks = true;
      }
    }

    if next_block_start < end {
      let items_in_block = end - next_block_start;
      let is_floor = items_in_block < count;
      let block = self.write_block(
        prefix_length,
        is_floor,
        next_floor_lead_label,
        next_block_start,
        end,
        has_terms,
        has_sub_blocks,
      )?;
      self.new_blocks.push(block);
    }

    debug_assert!(!self.new_blocks.is_empty());

    debug_assert!(self.new_blocks[0].is_floor || self.new_blocks.len() == 1);

    let first_block = PendingBlock::compile_index(
      std::mem::take(&mut self.new_blocks),
      &mut self.scratch_bytes,
      &mut self.scratch_ints_ref,
      self.version,
    )?;

    let remove_start = self.pending.len() - count;
    self.pending.drain(remove_start..);
    self.pending.push(PendingEntryEnum::Block(first_block));

    Ok(())
  }

  fn all_equal(b: &[u8], start_offset: usize, end_offset: usize, value: u8) -> Result<bool> {
    CoreHelper::check_from_to_index(start_offset, end_offset, b.len())?;
    Ok(b[start_offset..end_offset].iter().all(|&x| x == value))
  }
  /// Writes the specified slice (start is inclusive, end is exclusive) from
  /// the pending stack as a new block.
  ///
  /// If `is_floor` is `true`, it means there were too many (more than
  /// `max_items_in_block`) entries sharing the same prefix, so we broke
  /// it into multiple floor blocks. In that case, we record the starting
  /// label of the suffix of each floor block.
  #[allow(clippy::too_many_arguments)]
  pub fn write_block(
    &mut self,
    prefix_length: usize,
    is_floor: bool,
    floor_lead_label: i32,
    start: usize,
    end: usize,
    has_terms: bool,
    has_sub_blocks: bool,
  ) -> Result<PendingBlock> {
    debug_assert!(end > start);

    let start_fp = self.terms_out.get_file_pointer()? as i64;
    let has_floor_lead = is_floor && floor_lead_label != -1;

    let mut prefix_bytes = vec![0u8; prefix_length + if has_floor_lead { 1 } else { 0 }];
    prefix_bytes.copy_from(&self.last_term.bytes_ref.bytes[0..prefix_length], 0);
    let mut prefix = BytesRef::from_bytes(prefix_bytes);
    prefix.length = prefix_length;

    let num_entries = end - start;
    let mut code = (num_entries << 1) as i32;
    if end == self.pending.len() {
      code |= 1;
    }
    self.terms_out.write_vint(code)?;

    let is_leaf_block = !has_sub_blocks;
    let mut sub_indices = Vec::new();
    let mut absolute = true;

    if is_leaf_block {
      let mut stats_writer = StatsWriter::new(
        &mut self.stats_writer,
        *self.field_info.get_index_options() != IndexOptions::Docs,
      );
      for i in start..end {
        let term = match &self.pending[i] {
          PendingEntryEnum::Term(term) => term,
          _ => return Err(LuceneError::illegal_state("Expected PendingTerm")),
        };
        debug_assert!(StringHelper::starts_with_byte_array(
          &term.term_bytes,
          &prefix
        ));
        let state = &term.state;
        let suffix = term.term_bytes.len() - prefix_length;

        self.suffix_lengths_writer.write_vint(suffix as i32)?;
        self
          .suffix_writer
          .append_with_range(&term.term_bytes, prefix_length, suffix)?;
        debug_assert!(
          floor_lead_label == -1 || (term.term_bytes[prefix_length] as i32) >= floor_lead_label
        );

        match state {
          TermStateEnum::Block(block) => {
            stats_writer.add(block.doc_freq, block.total_term_freq)?;
          },
          TermStateEnum::Int(int) => {
            stats_writer.add(int.base.doc_freq, int.base.total_term_freq)?;
          },
          _ => {
            return Err(LuceneError::illegal_state("should be Block or Int"));
          },
        }

        self.postings_writer.encode_term(
          &mut self.meta_writer,
          &self.field_info,
          Cow::Borrowed(state),
          absolute,
        )?;
        absolute = false;
      }
      stats_writer.finish()?;
    } else {
      let mut stats_writer = StatsWriter::new(
        &mut self.stats_writer,
        *self.field_info.get_index_options() != IndexOptions::Docs,
      );
      for i in start..end {
        match &mut self.pending[i] {
          PendingEntryEnum::Term(term) => {
            debug_assert!(StringHelper::starts_with_byte_array(
              &term.term_bytes,
              &prefix
            ));
            let state = &term.state;
            let suffix_len = term.term_bytes.len() - prefix_length;

            self
              .suffix_lengths_writer
              .write_vint((suffix_len << 1) as i32)?;
            self
              .suffix_writer
              .append_with_range(&term.term_bytes, prefix_length, suffix_len)?;
            match state {
              TermStateEnum::Block(block) => {
                stats_writer.add(block.doc_freq, block.total_term_freq)?;
              },
              TermStateEnum::Int(int) => {
                stats_writer.add(int.base.doc_freq, int.base.total_term_freq)?;
              },
              _ => {
                return Err(LuceneError::illegal_state("should be Block or Int"));
              },
            }
            // meta
            self.postings_writer.encode_term(
              &mut self.meta_writer,
              &self.field_info,
              Cow::Borrowed(state),
              absolute,
            )?;
            absolute = false;
          },
          PendingEntryEnum::Block(block) => {
            debug_assert!(StringHelper::starts_with_byte_ref(&block.prefix, &prefix));
            let suffix = block.prefix.length - prefix_length;
            debug_assert!(suffix > 0);

            // write block suffix
            self
              .suffix_lengths_writer
              .write_vint(((suffix << 1) | 1) as i32)?;
            self
              .suffix_writer
              .append_with_range(&block.prefix.bytes, prefix_length, suffix)?;

            debug_assert!(
              floor_lead_label == -1
                || (block.prefix.bytes[prefix_length] as i32) >= floor_lead_label
            );
            debug_assert!(block.fp < start_fp);

            self
              .suffix_lengths_writer
              .write_vlong(start_fp - block.fp)?;
            sub_indices.push(block.index.take().unwrap());
          },
        }
      }
      stats_writer.finish()?;
      debug_assert!(!sub_indices.is_empty());
    }
    // Write suffixes byte[] blob to terms dict output, either uncompressed,
    // compressed with LZ4 or with LowercaseAsciiCompression.
    let mut compression_alg = CompressionAlgorithm::NoCompression;
    let suffix_len = self.suffix_writer.length();
    // If there are 2 suffix bytes or less per term, then we don't bother
    // compressing as suffix are unlikely what
    // makes the terms dictionary large, and it also tends to be frequently the case
    // for dense IDs like
    // auto-increment IDs, so not compressing in that case helps not hurt ID lookups
    // by too much. We also only start compressing when the prefix length is
    // greater than 2 since blocks whose prefix length is
    // 1 or 2 always all get visited when running a fuzzy query whose max number of
    // edits is 2.
    if suffix_len > 2 * num_entries && prefix_length > 2 {
      if suffix_len > 6 * num_entries {
        if self.compression_hash_table.is_none() {
          self.compression_hash_table =
            Some(HashTableEnum::High(HighCompressionHashTable::default()));
        }
        LZ4::compress(
          self.suffix_writer.bytes_ref.bytes.as_ref(),
          0,
          suffix_len.try_convert()?,
          &mut self.spare_writer,
          self.compression_hash_table.as_mut().unwrap(),
        )?;

        if self.spare_writer.size() < (suffix_len - (suffix_len >> 2)) {
          compression_alg = CompressionAlgorithm::Lz4;
        }
      }

      if compression_alg == CompressionAlgorithm::NoCompression {
        self.spare_writer.reset();

        if self.spare_bytes.len() < suffix_len {
          ArrayUtil::grow_no_copy(&mut self.spare_bytes, suffix_len)?;
        }

        if LowercaseAsciiCompression::compress(
          &self.suffix_writer.bytes_ref.bytes,
          suffix_len,
          &mut self.spare_bytes,
          &mut self.spare_writer,
        )? {
          compression_alg = CompressionAlgorithm::LowercaseAscii;
        }
      }
    }

    let mut token = (suffix_len as u64) << 3;
    if is_leaf_block {
      token |= 0x04;
    }
    token |= compression_alg.code() as u64;
    self.terms_out.write_vlong(token.try_convert()?)?;

    if compression_alg == CompressionAlgorithm::NoCompression {
      self
        .terms_out
        .write_bytes_with_len(&self.suffix_writer.bytes_ref.bytes, suffix_len)?;
    } else {
      self.spare_writer.copy_to(self.terms_out)?;
    }
    self.suffix_writer.set_length(0);
    self.spare_writer.reset();

    // suffix lengths
    let num_suffix_bytes = self.suffix_lengths_writer.size();
    ArrayUtil::grow_no_copy(&mut self.spare_bytes, num_suffix_bytes)?;
    {
      let mut data_output = ByteArrayDataOutput::with_bytes(self.spare_bytes.as_slice_mut());
      self.suffix_lengths_writer.copy_to(&mut data_output)?;
    }
    self.suffix_lengths_writer.reset();

    if Self::all_equal(&self.spare_bytes, 1, num_suffix_bytes, self.spare_bytes[0])? {
      debug_assert!(num_suffix_bytes <= i32::MAX as usize);
      self
        .terms_out
        .write_vint(((num_suffix_bytes << 1) | 1) as i32)?;
      self.terms_out.write_byte(self.spare_bytes[0])?;
    } else {
      debug_assert!(num_suffix_bytes <= i32::MAX as usize);
      self.terms_out.write_vint((num_suffix_bytes << 1) as i32)?;
      self
        .terms_out
        .write_bytes_with_len(&self.spare_bytes, num_suffix_bytes)?;
    }

    // stats
    let num_stats_bytes = self.stats_writer.size() as i32;
    self.terms_out.write_vint(num_stats_bytes)?;
    self.stats_writer.copy_to(self.terms_out)?;
    self.stats_writer.reset();

    // meta
    self.terms_out.write_vint(self.meta_writer.size() as i32)?;
    self.meta_writer.copy_to(self.terms_out)?;
    self.meta_writer.reset();

    if has_floor_lead {
      prefix.bytes[prefix.length] = floor_lead_label as u8;
      prefix.length += 1;
    }

    Ok(PendingBlock::new(
      prefix,
      start_fp,
      has_terms,
      is_floor,
      floor_lead_label,
      sub_indices,
    ))
  }
  pub fn write<N, PE>(
    &mut self,
    text: BytesRef<Vec<u8>>,
    terms_enum: &mut impl TermsEnum<PostingsEnum = PE>,
    norms: Option<&N>,
    postings_enum: Option<PE>,
  ) -> Result<Option<PE>>
  where
    N: NormsProducer,
    PE: PostingsEnum,
  {
    let (reuse, state_opt) = self.postings_writer.write_term(
      &text,
      terms_enum,
      &mut self.docs_seen,
      norms,
      postings_enum,
    )?;

    if let Some(state) = &state_opt {
      let (total_term_freq, doc_freq) = match state {
        TermStateEnum::Block(block) => {
          debug_assert!(block.doc_freq != 0);
          (block.total_term_freq, block.doc_freq)
        },
        TermStateEnum::Int(int) => {
          debug_assert!(int.base.doc_freq != 0);
          (int.base.total_term_freq, int.base.doc_freq)
        },
        _ => {
          return Err(LuceneError::illegal_state("should be Block or Int"));
        },
      };
      debug_assert!(
        *self.field_info.get_index_options() == IndexOptions::Docs
          || total_term_freq >= doc_freq as i64
      );

      self.push_term(&text)?;

      let term = PendingTerm::new(text, state_opt.unwrap());

      self.sum_doc_freq += doc_freq as i64;
      self.sum_total_term_freq += total_term_freq;
      self.num_terms += 1;

      if self.first_pending_term_bytes.is_none() {
        self.first_pending_term_bytes = Some(term.term_bytes.clone());
      }
      self.last_pending_term_bytes = term.term_bytes.clone();
      self.pending.push(PendingEntryEnum::Term(term));
    }

    Ok(reuse)
  }
  fn push_term(&mut self, text: &BytesRef<Vec<u8>>) -> Result<()> {
    let last_bytes = self.last_term.get_bytes_ref();
    let mut prefix_length = CoreHelper::miss_match(
      &last_bytes.bytes[..self.last_term.length()],
      &text.bytes[text.offset..text.offset + text.length],
    );
    if prefix_length == -1 {
      debug_assert!(self.last_term.length() == 0);
      prefix_length = 0;
    }

    for i in (prefix_length as usize..last_bytes.length).rev() {
      let prefix_top_size = self.pending.len() as i32 - self.prefix_starts[i];
      if prefix_top_size >= self.min_items_in_block {
        self.write_blocks(i + 1, prefix_top_size as usize)?;
        self.prefix_starts[i] -= prefix_top_size - 1;
      }
    }

    if self.prefix_starts.len() < text.length {
      ArrayUtil::grow_with_len(&mut self.prefix_starts, text.length)?;
    }

    for i in prefix_length as usize..text.length {
      self.prefix_starts[i] = self.pending.len() as i32;
    }

    self.last_term.copy_bytes_from_ref(text)?;
    Ok(())
  }
  pub fn finish(&mut self, fields: &mut Vec<ByteBuffersDataOutput>, index_out: &mut O) -> Result<()>
  where
    O: IndexOutput,
    PW: PostingsWriterBase,
  {
    if self.num_terms > 0 {
      self.push_term(&BytesRef::new())?;
      self.push_term(&BytesRef::new())?;

      let pending_len = self.pending.len();
      self.write_blocks(0, pending_len)?;

      debug_assert!(
        self.pending.len() == 1
          && match self.pending[0] {
            PendingEntryEnum::Block(_) => true,
            PendingEntryEnum::Term(_) => false,
          }
      );
      let mut root = match self.pending.pop().unwrap() {
        PendingEntryEnum::Block(b) => b,
        _ => return Err(LuceneError::illegal_state("expected final root block")),
      };
      debug_assert_eq!(root.prefix.length, 0);

      let root_code = root.index.as_ref().unwrap().get_empty_output();
      debug_assert!(root_code.is_some());

      let mut meta_out = ByteBuffersDataOutput::new();

      meta_out.write_vint(self.field_info.get_field_number())?;
      meta_out.write_vlong(self.num_terms)?;

      let root_code = root_code.unwrap();
      debug_assert!(root_code.length <= i32::MAX as usize);
      meta_out.write_vint(root_code.length as i32)?;
      debug_assert!(root_code.offset <= i32::MAX as usize);
      debug_assert!(root_code.length <= i32::MAX as usize);
      meta_out.write_bytes_range(&root_code.bytes, root_code.offset, root_code.length)?;
      debug_assert!(*self.field_info.get_index_options() != IndexOptions::None);

      if *self.field_info.get_index_options() != IndexOptions::Docs {
        meta_out.write_vlong(self.sum_total_term_freq)?;
      }
      meta_out.write_vlong(self.sum_doc_freq)?;
      meta_out.write_vint(self.docs_seen.cardinality().try_convert()?)?;
      let first_term_bytes = self.first_pending_term_bytes.take().unwrap();
      self.write_bytes_ref(&mut meta_out, &BytesRef::from_bytes(first_term_bytes))?;
      let last_term_bytes = std::mem::take(&mut self.last_pending_term_bytes);
      self.write_bytes_ref(&mut meta_out, &BytesRef::from_bytes(last_term_bytes))?;
      meta_out.write_vlong(index_out.get_file_pointer()? as i64)?;
      root
        .index
        .as_mut()
        .unwrap()
        .save(&mut meta_out, index_out)?;

      fields.push(meta_out);
    } else {
      debug_assert!(
        self.sum_total_term_freq == 0
          || (*self.field_info.get_index_options() == IndexOptions::Docs
            && self.sum_total_term_freq == -1)
      );
      debug_assert_eq!(self.sum_doc_freq, 0);
      debug_assert_eq!(self.docs_seen.cardinality(), 0);
    }

    Ok(())
  }
  fn write_bytes_ref<AV>(&self, out: &mut impl DataOutput, bytes: &BytesRef<AV>) -> Result<()>
  where
    AV: SharedAccessVec<u8>,
  {
    debug_assert!(bytes.length <= i32::MAX as usize);
    out.write_vint(bytes.length as i32)?;
    debug_assert!(bytes.offset <= i32::MAX as usize);
    bytes.bytes.access(|v| {
      out.write_bytes_range(v, bytes.offset, bytes.length)?;
      // Help the compiler infer types.
      Ok::<(), LuceneError>(())
    })?;
    Ok(())
  }
}

enum PendingEntryEnum {
  Term(PendingTerm),
  Block(PendingBlock),
}

use crate::core::codecs::block_tree::lucene90_block_tree_terms_reader::{
  OUTPUT_FLAG_HAS_TERMS, OUTPUT_FLAG_IS_FLOOR,
};
use crate::core::util::iterator::IteratorExt;

pub(crate) const DEFAULT_MIN_BLOCK_SIZE: i32 = 25;
pub(crate) const DEFAULT_MAX_BLOCK_SIZE: i32 = 48;

pub fn encode_output(fp: i64, has_terms: bool, is_floor: bool) -> i64 {
  debug_assert!(fp < (1i64 << 62));
  (fp << 2)
    | if has_terms {
      OUTPUT_FLAG_HAS_TERMS as i64
    } else {
      0
    }
    | if is_floor {
      OUTPUT_FLAG_IS_FLOOR as i64
    } else {
      0
    }
}
/// Encodes long value to variable length byte[], in MSB order.
pub(crate) fn write_msb_vlong(out: &mut impl DataOutput, mut l: i64) -> Result<()> {
  debug_assert!(l >= 0);
  // Keep zero bits on most significant byte to have more chance to get prefix
  // bytes shared. e.g. we expect 0x7FFF stored as [0x81, 0xFF, 0x7F] but
  // not [0xFF, 0xFF, 0x40]
  let bits = 64 - l.leading_zeros();
  let bytes_needed = ((bits.saturating_sub(1)) / 7 + 1) as usize;
  l <<= 64 - bytes_needed * 7;
  for _ in 1..bytes_needed {
    let byte = ((l >> 57) & 0x7F) as u8 | 0x80;
    out.write_byte(byte)?;
    l <<= 7;
  }
  let last_byte = ((l >> 57) & 0x7F) as u8;
  out.write_byte(last_byte)?;
  Ok(())
}
