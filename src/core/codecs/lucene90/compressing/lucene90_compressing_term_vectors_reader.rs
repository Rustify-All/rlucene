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
use crate::core::codecs::compressing::lucene90_compressing_term_vectors_writer::FLAGS_BITS;
use crate::core::codecs::compressing::lucene90_compressing_term_vectors_writer::{
  META_VERSION_START, OFFSETS, PACKED_BLOCK_SIZE, PAYLOADS, POSITIONS, VECTORS_EXTENSION,
  VECTORS_INDEX_CODEC_NAME, VECTORS_INDEX_EXTENSION, VECTORS_META_CODEC_NAME,
  VECTORS_META_EXTENSION, VERSION_CURRENT, VERSION_START,
};
use crate::core::codecs::compression::compression_mode::{
  CompressionModeBase, CompressionModeEnum, DecompressorEnum,
};
use crate::core::codecs::compression::decompressor::Decompressor;
use crate::core::codecs::lucene90::fields_index::FieldsIndex;
use crate::core::codecs::lucene90::fields_index_reader::FieldsIndexReader;
use crate::core::codecs::term_vectors_reader::{DefaultTermVectorsReader, TermVectorsReader};
use crate::core::index::automaton_terms_enum::AutomatonTermsEnum;
use crate::core::index::base_terms_enum::BaseTermsEnumTermStateImpl;
use crate::core::index::bytes_ref::BytesRefValueEnum;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::fields::Fields;
use crate::core::index::filtered_terms_enum::{FilteredTermsEnum, FilteredTermsEnumBase};
use crate::core::index::postings_enum::{FREQS, PostingsEnum};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::slow_impacts_enum::SlowImpactsEnum;
use crate::core::index::term_vectors::{RawTermVectors, TermVectors};
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::index::{BytesRef, IndexFileNames};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::store::byte_buffers_data_input::ByteBuffersDataInput;
use crate::core::store::directory::Directory;
use crate::core::store::{ByteArrayDataInput, DataInput, IOContext, IndexInput, ReadAdvice};
use crate::core::util::IOUtils;
use crate::core::util::access::ByteSource;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::automation::compiled_automaton::CompiledAutomaton;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::clone::TryClone;
use crate::core::util::close::CloseableRef;
use crate::core::util::dummy::dummy_attribute_source::DummyAttributeSource;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::iterator::IteratorExt;
use crate::core::util::long_values::LongValues;
use crate::core::util::packed::Format::Packed;
use crate::core::util::packed::block_packed_reader_iterator::BlockPackedReaderIterator;
use crate::core::util::packed::direct_reader::DirectReader;
use crate::core::util::packed::direct_writer::bits_required;
use crate::core::util::packed::{PackedImpl, PackedInts, ReaderIterator};
use crate::core::util::{ToInt, TryIntoInt};
use std::io::Cursor;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct Lucene90CompressingTermVectorsReader<I>
where
  I: IndexInput,
{
  field_infos: Arc<FieldInfos>,
  pub(crate) index_reader: FieldsIndexReader<I>,
  pub(crate) vectors_stream: I,
  version: i32,
  packed_ints_version: i32,
  compression_mode: CompressionModeEnum,
  decompressor: DecompressorEnum,
  reader: BlockPackedReaderIterator,
  chunk_size: i32,
  num_docs: i32,
  closed: AtomicBool,
  // number of written blocks
  num_chunks: i64,
  // number of incomplete compressed blocks written
  num_dirty_chunks: i64,
  // cumulative number of docs in incomplete chunks
  num_dirty_docs: i64,
  // end of the data section
  max_pointer: usize,
  block_state: BlockState,
  // Cache of recently prefetched block IDs. This helps reduce chances of prefetching the same block
  // multiple times, which is otherwise likely due to index sorting or recursive graph bisection
  // clustering similar documents together. NOTE: this cache must be small since it's fully scanned.
  prefetched_block_id_cache: [i64; PREFETCH_CACHE_SIZE],
  prefetched_block_id_cache_index: usize,
}

impl<I> Lucene90CompressingTermVectorsReader<I>
where
  I: IndexInput,
{
  pub fn new<D1, D2>(
    dir: &D1,
    si: &SegmentInfo<D2>,
    segment_suffix: &str,
    field_infos: Arc<FieldInfos>,
    context: &IOContext,
    format_name: &str,
    compression_mode: CompressionModeEnum,
  ) -> Result<Self>
  where
    D1: Directory<IndexInput = I>,
  {
    let segment = &si.name;
    let num_docs = si.max_doc()?;
    let mut meta_in = None;
    let mut vectors_stream = None;
    let mut fields_index_reader = None;
    let mut reader = None;
    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      let vectors_stream_fn =
        IndexFileNames::segment_file_name(segment, segment_suffix, VECTORS_EXTENSION);
      vectors_stream = Some(dir.open_input(
        &vectors_stream_fn,
        &context.with_read_advice_self(ReadAdvice::Random)?,
      )?);
      let vectors_stream_ref = vectors_stream
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("term vectors input is missing"))?;

      let version = CodecUtil::check_index_header(
        vectors_stream_ref,
        format_name,
        VERSION_START,
        VERSION_CURRENT,
        si.get_id(),
        segment_suffix,
      )?;
      debug_assert_eq!(
        CodecUtil::index_header_length(format_name, segment_suffix),
        vectors_stream_ref.get_file_pointer()?
      );

      let meta_stream_fn =
        IndexFileNames::segment_file_name(segment, segment_suffix, VECTORS_META_EXTENSION);
      let meta = meta_in.insert(dir.open_checksum_input(&meta_stream_fn)?);

      CodecUtil::check_index_header(
        meta,
        VECTORS_META_CODEC_NAME,
        META_VERSION_START,
        version,
        si.get_id(),
        segment_suffix,
      )?;

      let packed_ints_version = meta.read_vint()?;
      let chunk_size = meta.read_vint()?;
      // NOTE: data file is too costly to verify checksum against all the bytes on open,
      // but for now we at least verify proper structure of the checksum footer: which looks
      // for FOOTER_MAGIC + algorithmID. This is cheap and can detect some forms of corruption
      // such as file truncation.
      CodecUtil::retrieve_checksum(vectors_stream_ref)?;

      fields_index_reader = Some(FieldsIndexReader::new(
        dir,
        &si.name,
        segment_suffix,
        VECTORS_INDEX_EXTENSION,
        VECTORS_INDEX_CODEC_NAME,
        si.get_id(),
        meta,
        context,
      )?);
      let max_pointer = fields_index_reader
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("term vectors index input is missing"))?
        .get_max_pointer();

      let num_chunks = meta.read_vlong()?;
      let num_dirty_chunks = meta.read_vlong()?;
      let num_dirty_docs = meta.read_vlong()?;

      if num_chunks < num_dirty_chunks {
        return Err(LuceneError::corrupt_index(format!(
          "Cannot have more dirty chunks than chunks: numChunks={num_chunks}, numDirtyChunks={num_dirty_chunks} (resource={meta})"
        )));
      }
      if (num_dirty_chunks == 0) != (num_dirty_docs == 0) {
        return Err(LuceneError::corrupt_index(format!(
          "Cannot have dirty chunks without dirty docs or vice-versa: numDirtyChunks={num_dirty_chunks}, numDirtyDocs={num_dirty_docs} (resource={meta})"
        )));
      }
      if num_dirty_docs < num_dirty_chunks {
        return Err(LuceneError::corrupt_index(format!(
          "Cannot have more dirty chunks than documents within dirty chunks: numDirtyChunks={num_dirty_chunks}, numDirtyDocs={num_dirty_docs} (resource={meta})"
        )));
      }

      let decompressor = compression_mode.new_decompressor();

      let prefetched_block_id_cache = [-1i64; PREFETCH_CACHE_SIZE];

      reader = Some(Self {
        field_infos,
        compression_mode,
        version,
        packed_ints_version,
        chunk_size,
        num_docs,
        vectors_stream: vectors_stream
          .take()
          .ok_or_else(|| LuceneError::illegal_state("term vectors input is missing"))?,
        index_reader: fields_index_reader
          .take()
          .ok_or_else(|| LuceneError::illegal_state("term vectors index input is missing"))?,
        max_pointer,
        num_chunks,
        num_dirty_chunks,
        num_dirty_docs,
        decompressor,
        reader: BlockPackedReaderIterator::new(packed_ints_version, PACKED_BLOCK_SIZE, 0)?,
        prefetched_block_id_cache,
        prefetched_block_id_cache_index: 0,
        closed: AtomicBool::new(false),
        block_state: BlockState::new(None, None, 0),
      });
      CodecUtil::check_footer_with_error::<(), _>(meta, None)?;
      meta.close()?;
      success = true;
      Ok(())
    }));

    let result = if success {
      result
    } else {
      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        match meta_in.as_mut() {
          Some(meta) => CodecUtil::check_footer_with_error(meta, Some(result)),
          None => IOUtils::rethrow_always(result),
        }
      }));
      IOUtils::close_while_handling_exception((
        reader.as_ref(),
        fields_index_reader.as_ref(),
        vectors_stream.as_ref(),
        meta_in.as_ref(),
      ));
      result
    };
    unwrap_caught_result!(result)?;
    reader
      .take()
      .ok_or_else(|| LuceneError::illegal_state("term vectors reader is missing"))
  }

  pub fn with_reader(reader: &Lucene90CompressingTermVectorsReader<I>) -> Result<Self> {
    Ok(Self {
      vectors_stream: reader.vectors_stream.try_clone()?,
      index_reader: reader.index_reader.try_clone()?,
      packed_ints_version: reader.packed_ints_version,
      compression_mode: reader.compression_mode.clone(),
      decompressor: reader.decompressor.clone(),
      reader: BlockPackedReaderIterator::new(reader.packed_ints_version, PACKED_BLOCK_SIZE, 0)?,
      chunk_size: reader.chunk_size,
      num_docs: reader.num_docs,
      version: reader.version,
      num_chunks: reader.num_chunks,
      num_dirty_chunks: reader.num_dirty_chunks,
      num_dirty_docs: reader.num_dirty_docs,
      max_pointer: reader.max_pointer,
      closed: AtomicBool::new(false),
      block_state: BlockState::new(None, None, 0),
      prefetched_block_id_cache: [-1; PREFETCH_CACHE_SIZE],
      prefetched_block_id_cache_index: 0,
      field_infos: Arc::clone(&reader.field_infos),
    })
  }

  pub(crate) fn get_compression_mode(&self) -> &CompressionModeEnum {
    &self.compression_mode
  }

  pub(crate) fn get_chunk_size(&self) -> i32 {
    self.chunk_size
  }

  pub(crate) fn get_packed_ints_version(&self) -> i32 {
    self.packed_ints_version
  }

  pub(crate) fn get_version(&self) -> i32 {
    self.version
  }

  pub(crate) fn get_max_pointer(&self) -> usize {
    self.max_pointer
  }

  pub(crate) fn get_num_dirty_docs(&self) -> Result<i64> {
    if self.version != VERSION_CURRENT {
      return Err(LuceneError::illegal_state(
        "get_num_dirty_docs should only ever get called when the reader is on the current version",
      ));
    }
    debug_assert!(self.num_dirty_docs >= 0);
    Ok(self.num_dirty_docs)
  }

  pub(crate) fn get_num_dirty_chunks(&self) -> Result<i64> {
    if self.version != VERSION_CURRENT {
      return Err(LuceneError::illegal_state(
        "get_num_dirty_chunks should only ever get called when the reader is on the current version",
      ));
    }
    debug_assert!(self.num_dirty_chunks >= 0);
    Ok(self.num_dirty_chunks)
  }

  pub(crate) fn get_num_chunks(&self) -> Result<i64> {
    if self.version != VERSION_CURRENT {
      return Err(LuceneError::illegal_state(
        "get_num_chunks should only ever get called when the reader is on the current version",
      ));
    }
    debug_assert!(self.num_chunks >= 0);
    Ok(self.num_chunks)
  }

  #[allow(dead_code)]
  pub(crate) fn num_docs(&self) -> i32 {
    0
  }
  pub fn ensure_open(&self) -> Result<()> {
    if self.closed.load(Ordering::Relaxed) {
      Err(LuceneError::already_closed("this FieldsReader is closed"))
    } else {
      Ok(())
    }
  }
  fn slice(input: &mut I) -> Result<Vec<u8>> {
    let length = input.read_vint()?.try_convert()?;
    // Only retained metadata uses this helper; fill its final independent owner.
    let mut buf = vec![0; length];
    input.read_bytes(&mut buf, 0, length)?;
    Ok(buf)
  }
  pub(crate) fn is_loaded(&self, doc_id: i32) -> bool {
    let bs = &self.block_state;
    let doc_base = bs.doc_base.unwrap_or(-1);
    doc_base <= doc_id && doc_id < doc_base + bs.chunk_docs
  }
  fn position_index(
    skip: usize,
    num_fields: usize,
    num_terms: &[usize],
    term_freqs: &[usize],
  ) -> Vec<Vec<usize>> {
    let mut position_index = Vec::with_capacity(num_fields);
    let mut term_index = 0;
    #[allow(clippy::needless_range_loop)] // Preserve indexed failure timing for malformed chunks.
    for i in 0..skip {
      let term_count = num_terms[i];
      term_index += term_count;
    }
    for i in 0..num_fields {
      let term_count = num_terms[skip + i];
      let mut arr = Vec::with_capacity(term_count + 1);
      arr.push(0);
      let mut position = 0;
      for j in 0..term_count {
        let freq = term_freqs[term_index + j];
        position += freq;
        arr.push(position);
      }
      term_index += term_count;
      position_index.push(arr);
    }
    position_index
  }

  #[allow(clippy::too_many_arguments)]
  pub(crate) fn read_positions(
    &mut self,
    skip: usize,
    num_fields: usize,
    flags: &[u8],
    num_terms: &[usize],
    term_freqs: &[usize],
    flag: i32,
    total_positions: usize,
    position_index: &[Vec<usize>],
  ) -> Result<Vec<Vec<i32>>> {
    let mut positions = vec![Vec::new(); num_fields];
    // reset reader
    self.reader.reset(total_positions);

    // skip
    let mut to_skip = 0;
    let mut term_index = 0;
    for i in 0..skip {
      let f = i32::from(flags[i]);
      let term_count = num_terms[i];
      if (f & flag) != 0 {
        for j in 0..term_count {
          to_skip += term_freqs[term_index + j];
        }
      }
      term_index += term_count;
    }
    self.reader.skip(to_skip, &mut self.vectors_stream)?;
    // read doc positions
    for i in 0..num_fields {
      let f = i32::from(flags[skip + i]);
      let term_count = num_terms[skip + i];

      if (f & flag) != 0 {
        let total_freq = position_index[i][term_count];
        let mut field_positions = Vec::with_capacity(total_freq);
        while field_positions.len() < total_freq {
          let remaining = total_freq - field_positions.len();
          let next_positions = self
            .reader
            .next_batch(remaining, &mut self.vectors_stream)?;
          let slice = &next_positions.longs
            [next_positions.offset..next_positions.offset + next_positions.length];
          assert!(slice.len() <= remaining);
          field_positions.extend(slice.iter().map(|&val| val as i32));
        }
        positions[i] = field_positions;
      }
    }
    let read = self.reader.ord();
    self
      .reader
      .skip(total_positions - read, &mut self.vectors_stream)?;
    Ok(positions)
  }
}

impl<I> CloseableRef for Lucene90CompressingTermVectorsReader<I>
where
  I: IndexInput,
{
  /// Close the underlying [`IndexInput`]s.
  fn close(&self) -> Result<()> {
    if !self.closed.load(Ordering::Relaxed) {
      IOUtils::close((Some(&self.index_reader), Some(&self.vectors_stream)))?;
      self.closed.store(true, Ordering::Relaxed);
    }
    Ok(())
  }
}

impl<I> TermVectors for Lucene90CompressingTermVectorsReader<I>
where
  I: IndexInput,
{
  fn prefetch(&mut self, doc_id: i32) -> Result<()> {
    let block_id = self.index_reader.get_block_id(doc_id)?;
    for &prefetched_block_id in &self.prefetched_block_id_cache {
      if prefetched_block_id == block_id {
        return Ok(());
      }
    }
    let block_start_pointer = self.index_reader.get_block_start_pointer(block_id)?;
    let block_length = self.index_reader.get_block_length(block_id)?;
    self
      .vectors_stream
      .prefetch(block_start_pointer, block_length)?;
    let idx = self.prefetched_block_id_cache_index & PREFETCH_CACHE_MASK;
    self.prefetched_block_id_cache[idx] = block_id;
    self.prefetched_block_id_cache_index += 1;
    Ok(())
  }

  type Fields = TVFields;

  fn get(&mut self, doc: i32) -> Result<Option<Self::Fields>> {
    self.ensure_open()?;
    // seek to the right place
    let start_pointer = if self.is_loaded(doc) {
      self
        .block_state
        .start_pointer
        .ok_or_else(|| LuceneError::illegal_state("start_pointer was None"))?
    } else {
      self.index_reader.get_start_pointer(doc)?
    };
    self.vectors_stream.seek(start_pointer)?;
    // decode
    // - docBase: first doc ID of the chunk
    // - chunkDocs: number of docs of the chunk
    let doc_base = self.vectors_stream.read_vint()?;
    let chunk_docs = ((self.vectors_stream.read_vint()? as u32) >> 1) as i32;
    if doc < doc_base || doc >= doc_base + chunk_docs || (doc_base + chunk_docs) > self.num_docs {
      return Err(LuceneError::corrupt_index(format!(
        "docBase={},chunkDocs={},doc={},resource={}",
        doc_base, chunk_docs, doc, self.vectors_stream
      )));
    }
    self.block_state = BlockState::new(Some(start_pointer), Some(doc_base), chunk_docs);
    let (skip, num_fields, total_fields) = if chunk_docs == 1 {
      let nf = self.vectors_stream.read_vint()? as usize;
      (0, nf, nf)
    } else {
      self.reader.reset(chunk_docs as usize);
      let mut sum = 0;
      for _ in doc_base..doc {
        sum += self.reader.next_value(&mut self.vectors_stream)? as usize;
      }
      let skip = sum;
      let num_fields = self.reader.next_value(&mut self.vectors_stream)? as usize;
      sum += num_fields;
      for _ in (doc + 1)..(doc_base + chunk_docs) {
        sum += self.reader.next_value(&mut self.vectors_stream)? as usize;
      }
      (skip, num_fields, sum)
    };

    if num_fields == 0 {
      return Ok(None);
    }

    // read field numbers that have term vectors
    let field_nums = {
      let token = self.vectors_stream.read_byte()?;
      debug_assert!(token != 0); // means no term vectors, cannot happen since we checked for numFields == 0
      let bits_per_field_num = (token & 0x1F) as i32;
      let mut total_distinct_fields = (token >> 5) as i32;
      if total_distinct_fields == 0x07 {
        total_distinct_fields += self.vectors_stream.read_vint()?;
      }
      total_distinct_fields += 1;
      let mut it = PackedInts::get_reader_iterator_no_header(
        &mut self.vectors_stream,
        Packed(PackedImpl::new(0)),
        self.packed_ints_version,
        total_distinct_fields,
        bits_per_field_num,
        1,
      )?;
      let mut field_nums =
        smallvec::SmallVec::<[i32; 4]>::with_capacity(total_distinct_fields as usize);
      for _ in 0..total_distinct_fields {
        field_nums.push(it.next()? as i32);
      }
      field_nums
    };

    // read field numbers and flags
    let mut field_num_offs = Vec::with_capacity(num_fields);
    let (flags, expanded_flags) = {
      let bits_per_off = bits_required((field_nums.len() - 1) as i64)?;
      let bytes = Self::slice(&mut self.vectors_stream)?;
      let length = bytes.len();
      let all_field_num_offs = DirectReader::get_instance(
        ByteBuffersDataInput::new(vec![Cursor::new(bytes)], length)?,
        bits_per_off,
      )?;
      let v = self.vectors_stream.read_vint()?;
      let mut expanded_flags = smallvec::SmallVec::<[u8; 32]>::new();
      let flags = match v {
        0 => {
          let length = self.vectors_stream.read_vint()?.try_convert()?;
          let bytes = self.vectors_stream.get_bytes(length)?;
          let field_flags = DirectReader::get_instance(
            ByteBuffersDataInput::new(vec![Cursor::new(bytes.as_ref())], length)?,
            *FLAGS_BITS,
          )?;
          expanded_flags.reserve_exact(total_fields);
          for i in 0..total_fields {
            let field_num_off = all_field_num_offs.get(i)? as usize;
            debug_assert!(field_num_off < field_nums.len());
            expanded_flags.push(field_flags.get(field_num_off)? as u8);
          }
          None
        },
        1 => {
          let bytes = Self::slice(&mut self.vectors_stream)?;
          let length = bytes.len();
          Some(DirectReader::get_instance(
            ByteBuffersDataInput::new(vec![Cursor::new(bytes)], length)?,
            *FLAGS_BITS,
          )?)
        },
        _ => {
          return Err(LuceneError::illegal_state(format!(
            "invalid flag selector: {v}"
          )));
        },
      };
      for off in (skip..).take(num_fields) {
        field_num_offs.push(all_field_num_offs.get(off)? as usize);
      }
      (flags, expanded_flags)
    };

    // number of terms per field for all fields
    let (num_terms, total_terms) = {
      let bits_required = self.vectors_stream.read_vint()?;
      let length = self.vectors_stream.read_vint()?.try_convert()?;
      let bytes = self.vectors_stream.get_bytes(length)?;
      let packed_num_terms = DirectReader::get_instance(
        ByteBuffersDataInput::new(vec![Cursor::new(bytes.as_ref())], length)?,
        bits_required,
      )?;
      let mut sum = 0;
      let mut num_terms = smallvec::SmallVec::<[usize; 32]>::with_capacity(total_fields);
      for i in 0..total_fields {
        let count = packed_num_terms.get(i)?;
        sum += count;
        num_terms.push(count as usize);
      }
      (num_terms, sum as usize)
    };

    // term lengths
    let mut doc_off = 0;
    let mut doc_len = 0;
    #[allow(unused)]
    let mut total_len = 0;
    let mut field_lengths = Vec::with_capacity(num_fields);
    let mut prefix_lengths = Vec::with_capacity(num_fields);
    let mut suffix_lengths = Vec::with_capacity(num_fields);
    {
      self.reader.reset(total_terms);

      // skip
      let mut to_skip = 0;
      #[allow(clippy::needless_range_loop)] // Preserve indexed failure timing for malformed chunks.
      for i in 0..skip {
        to_skip += num_terms[i];
      }
      self.reader.skip(to_skip, &mut self.vectors_stream)?;

      // read prefix lengths
      for i in 0..num_fields {
        let term_count = num_terms[skip + i];
        let mut field_prefix_lengths = Vec::with_capacity(term_count);
        let mut j = 0;

        while j < term_count {
          let next = self
            .reader
            .next_batch(term_count - j, &mut self.vectors_stream)?;
          let src = &next.longs[next.offset..][..next.length];
          for &val in src {
            field_prefix_lengths.push(val as usize);
          }
          j += next.length;
        }

        prefix_lengths.push(field_prefix_lengths);
      }

      self
        .reader
        .skip(total_terms - self.reader.ord(), &mut self.vectors_stream)?;

      self.reader.reset(total_terms);

      #[allow(clippy::needless_range_loop)] // Preserve indexed failure timing for malformed chunks.
      for i in 0..skip {
        let term_count = num_terms[i];
        for _ in 0..term_count {
          doc_off += self.reader.next_value(&mut self.vectors_stream)? as usize;
        }
      }

      for i in 0..num_fields {
        let term_count = num_terms[skip + i];
        let mut field_suffix_lengths = Vec::with_capacity(term_count);
        let mut j = 0;
        while j < term_count {
          let next = self
            .reader
            .next_batch(term_count - j, &mut self.vectors_stream)?;
          for k in 0..next.length {
            field_suffix_lengths.push(next.longs[next.offset + k] as usize);
            j += 1;
          }
        }
        let field_length = sum(&field_suffix_lengths);
        doc_len += field_length;
        field_lengths.push(field_length);
        suffix_lengths.push(field_suffix_lengths);
      }

      total_len = doc_off + doc_len;
      #[allow(clippy::needless_range_loop)] // Preserve indexed failure timing for malformed chunks.
      for i in (skip + num_fields)..total_fields {
        let term_count = num_terms[i];
        for _ in 0..term_count {
          total_len += self.reader.next_value(&mut self.vectors_stream)? as usize;
        }
      }
    }

    // term freqs
    let term_freqs = {
      let mut term_freqs = Vec::with_capacity(total_terms);
      self.reader.reset(total_terms);
      debug_assert!(total_terms <= i32::MAX as usize);
      while term_freqs.len() < total_terms {
        let next = self
          .reader
          .next_batch(total_terms - term_freqs.len(), &mut self.vectors_stream)?;
        term_freqs.extend(
          next.longs[next.offset..][..next.length]
            .iter()
            .map(|&freq| 1 + freq as usize),
        );
      }
      term_freqs
    };

    // total number of positions, offsets and payloads
    let mut total_positions = 0;
    let mut total_offsets = 0;
    let mut total_payloads = 0;
    let mut term_index = 0;
    let mut all_flags = expanded_flags;
    if flags.is_some() {
      all_flags.reserve_exact(total_fields);
    }
    #[allow(clippy::needless_range_loop)] // Preserve indexed failure timing for malformed chunks.
    for i in 0..total_fields {
      let f = if let Some(flags) = &flags {
        let f = flags.get(i)? as u8;
        all_flags.push(f);
        f
      } else {
        all_flags[i]
      };
      let term_count = num_terms[i];
      for _ in 0..term_count {
        let freq = term_freqs[term_index];
        term_index += 1;
        if (i32::from(f) & POSITIONS) != 0 {
          total_positions += freq;
        }
        if (i32::from(f) & OFFSETS) != 0 {
          total_offsets += freq;
        }
        if (i32::from(f) & PAYLOADS) != 0 {
          total_payloads += freq;
        }
      }
      debug_assert!(i != total_fields - 1 || term_index == total_terms);
    }

    // position index
    let position_index = Self::position_index(skip, num_fields, &num_terms, &term_freqs);

    // positions
    let mut positions = if total_positions > 0 {
      self.read_positions(
        skip,
        num_fields,
        &all_flags,
        &num_terms,
        &term_freqs,
        POSITIONS,
        total_positions,
        &position_index,
      )?
    } else {
      Vec::new()
    };
    let (start_offsets, lengths) = if total_offsets > 0 {
      // average number of chars per term
      stack_or_heap_buffer!(chars_per_term, f32, field_nums.len(), 16, 0.0);
      for v in chars_per_term.iter_mut() {
        *v = f32::from_bits(self.vectors_stream.read_int()? as u32);
      }

      let mut start_offsets = self.read_positions(
        skip,
        num_fields,
        &all_flags,
        &num_terms,
        &term_freqs,
        OFFSETS,
        total_offsets,
        &position_index,
      )?;

      let mut lengths = self.read_positions(
        skip,
        num_fields,
        &all_flags,
        &num_terms,
        &term_freqs,
        OFFSETS,
        total_offsets,
        &position_index,
      )?;

      for i in 0..num_fields {
        let f_start_offsets = &mut start_offsets[i];
        let f_positions = positions.get(i).map_or(&[][..], Vec::as_slice);
        // patch offsets from positions
        if !f_start_offsets.is_empty() && !f_positions.is_empty() {
          let field_chars_per_term = chars_per_term[field_num_offs[i]];
          let f_positions = &f_positions[..f_start_offsets.len()];
          let f_start_offsets = f_start_offsets.as_mut_slice();
          for j in 0..f_start_offsets.len() {
            f_start_offsets[j] += (field_chars_per_term * f_positions[j] as f32) as i32;
          }
        }

        if !f_start_offsets.is_empty() {
          let f_prefix_lengths = &prefix_lengths[i];
          let f_suffix_lengths = &suffix_lengths[i];
          let f_lengths = &mut lengths[i];
          let term_count = num_terms[skip + i];
          for j in 0..term_count {
            // delta-decode start offsets and  patch lengths using term lengths
            let term_length = (f_prefix_lengths[j] + f_suffix_lengths[j]) as i32;
            let pos_start = position_index[i][j];
            let pos_end = position_index[i][j + 1];
            f_lengths[pos_start] += term_length;
            for k in (pos_start + 1)..pos_end {
              f_start_offsets[k] += f_start_offsets[k - 1];
              f_lengths[k] += term_length;
            }
          }
        }
      }

      (start_offsets, lengths)
    } else {
      (Vec::new(), Vec::new())
    };

    if total_positions > 0 {
      // delta-decode positions
      for i in 0..num_fields {
        let f_positions = &mut positions[i];
        let f_position_index = &position_index[i];
        if !f_positions.is_empty() {
          let term_count = num_terms[skip + i];
          for j in 0..term_count {
            // delta-decode start offsets
            for k in (f_position_index[j] + 1)..f_position_index[j + 1] {
              f_positions[k] += f_positions[k - 1];
            }
          }
        }
      }
    }
    let mut payload_index = if total_payloads > 0 {
      vec![Vec::new(); num_fields]
    } else {
      Vec::new()
    };
    let mut total_payload_length = 0;
    let mut payload_off = 0;
    let mut payload_len = 0;

    if total_payloads > 0 {
      self.reader.reset(total_payloads);
      // skip
      let mut term_index = 0;
      for i in 0..skip {
        let f = i32::from(all_flags[i]);
        let term_count = num_terms[i];
        if (f & PAYLOADS) != 0 {
          for j in 0..term_count {
            let freq = term_freqs[term_index + j];
            for _ in 0..freq {
              let l = self.reader.next_value(&mut self.vectors_stream)? as usize;
              payload_off += l;
            }
          }
        }
        term_index += term_count;
      }
      total_payload_length = payload_off;

      // read doc payload lengths
      for i in 0..num_fields {
        let f = i32::from(all_flags[skip + i]);
        let term_count = num_terms[skip + i];
        if (f & PAYLOADS) != 0 {
          let total_freq = position_index[i][term_count];
          let mut field_payload_index = Vec::with_capacity(total_freq + 1);
          field_payload_index.push(payload_len);
          for j in 0..term_count {
            let freq = term_freqs[term_index + j];
            for _ in 0..freq {
              let payload_length = self.reader.next_value(&mut self.vectors_stream)? as usize;
              payload_len += payload_length;
              field_payload_index.push(payload_len);
            }
          }
          debug_assert_eq!(field_payload_index.len(), total_freq + 1);
          payload_index[i] = field_payload_index;
        }
        term_index += term_count;
      }
      total_payload_length += payload_len;
      for i in (skip + num_fields)..total_fields {
        let f = i32::from(all_flags[i]);
        let term_count = num_terms[i];
        if (f & PAYLOADS) != 0 {
          for j in 0..term_count {
            let freq = term_freqs[term_index + j];
            for _ in 0..freq {
              total_payload_length += self.reader.next_value(&mut self.vectors_stream)? as usize;
            }
          }
        }
        term_index += term_count;
      }

      debug_assert_eq!(term_index, total_terms);
    }
    // decompress data
    let mut suffix_bytes = BytesRef::new();
    self.decompressor.decompress(
      &mut self.vectors_stream,
      (total_len + total_payload_length) as i32,
      (doc_off + payload_off) as i32,
      (doc_len + payload_len) as i32,
      &mut suffix_bytes,
    )?;
    suffix_bytes.length = doc_len;
    let suffix_bytes = BytesRef::from_slice(
      Rc::new(suffix_bytes.bytes),
      suffix_bytes.offset,
      suffix_bytes.length,
    );
    let payload_offset = suffix_bytes.offset + doc_len;
    debug_assert!(
      payload_offset <= suffix_bytes.bytes.len()
        && payload_len <= suffix_bytes.bytes.len()
        && payload_offset + payload_len <= suffix_bytes.bytes.len()
    );

    let field_flags = if all_flags.spilled() && skip == 0 && num_fields == total_fields {
      all_flags.into_vec()
    } else {
      all_flags[skip..skip + num_fields].to_vec()
    };

    let field_term_freqs: smallvec::SmallVec<[Vec<usize>; 1]> =
      if num_fields == 1 && skip == 0 && num_terms[0] == term_freqs.len() {
        // This field owns the entire decoded chunk; keep its exact-sized buffer.
        smallvec::smallvec![term_freqs]
      } else {
        let mut field_term_freqs = smallvec::smallvec![Vec::new(); num_fields];
        let mut term_idx = 0;
        #[allow(clippy::needless_range_loop)]
        // Preserve indexed failure timing for malformed chunks.
        for n in 0..skip {
          term_idx += num_terms[n];
        }

        for (i, slot) in field_term_freqs.iter_mut().enumerate().take(num_fields) {
          let term_count = num_terms[skip + i];
          let mut v = Vec::with_capacity(term_count);
          for _ in 0..term_count {
            v.push(term_freqs[term_idx]);
            term_idx += 1;
          }
          *slot = v;
        }
        field_term_freqs
      };

    debug_assert_eq!(sum(&field_lengths), doc_len);

    Ok(Some(TVFields::new(
      field_nums,
      field_flags,
      field_num_offs,
      field_lengths,
      prefix_lengths
        .into_iter()
        .zip(suffix_lengths)
        .zip(field_term_freqs)
        .zip(position_index)
        .zip(
          positions
            .into_iter()
            .chain(std::iter::repeat_with(Vec::new))
            .take(num_fields),
        )
        .zip(
          start_offsets
            .into_iter()
            .chain(std::iter::repeat_with(Vec::new))
            .take(num_fields),
        )
        .zip(
          lengths
            .into_iter()
            .chain(std::iter::repeat_with(Vec::new))
            .take(num_fields),
        )
        .zip(
          payload_index
            .into_iter()
            .chain(std::iter::repeat_with(Vec::new))
            .take(num_fields),
        )
        .map(
          |(
            (
              (
                ((((prefix_lengths, suffix_lengths), term_freqs), position_index), positions),
                start_offsets,
              ),
              lengths,
            ),
            payload_index,
          )| {
            Rc::new(TVTermData::new(
              prefix_lengths,
              suffix_lengths,
              term_freqs,
              position_index,
              positions,
              start_offsets,
              lengths,
              payload_index,
            ))
          },
        )
        .collect(),
      payload_offset,
      payload_len,
      suffix_bytes,
      self.field_infos.clone(),
    )?))
  }

  type Terms = <Self::Fields as Fields>::Terms;

  fn get_field_terms(
    &mut self,
    doc: i32,
    field: &str,
  ) -> Result<Option<<Self::Fields as Fields>::Terms>> {
    self.default_get_field_terms(doc, field)
  }
}

impl<I> RawTermVectors for Lucene90CompressingTermVectorsReader<I>
where
  I: IndexInput,
{
  type IndexInput = I;

  fn raw_term_vectors_mut(&mut self) -> Result<&mut DefaultTermVectorsReader<Self::IndexInput>> {
    Ok(self)
  }

  fn raw_term_vectors(&self) -> Result<&DefaultTermVectorsReader<Self::IndexInput>> {
    Ok(self)
  }
}

impl<I> TryClone for Lucene90CompressingTermVectorsReader<I>
where
  I: IndexInput,
{
  fn try_clone(&self) -> Result<Self>
  where
    Self: Sized,
  {
    Lucene90CompressingTermVectorsReader::with_reader(self)
  }
}

impl<I> TermVectorsReader for Lucene90CompressingTermVectorsReader<I>
where
  I: IndexInput,
{
  fn check_integrity(&self) -> Result<()> {
    self.index_reader.check_integrity()?;
    CodecUtil::checksum_entire_file(&self.vectors_stream)?;
    Ok(())
  }

  fn get_merge_instance(&self) -> Result<Option<Self>>
  where
    Self: Sized,
  {
    Ok(Some(Lucene90CompressingTermVectorsReader::with_reader(
      self,
    )?))
  }
}

struct BlockState {
  start_pointer: Option<usize>,
  doc_base: Option<i32>,
  chunk_docs: i32,
}
impl BlockState {
  pub(crate) fn new(start_pointer: Option<usize>, doc_base: Option<i32>, chunk_docs: i32) -> Self {
    BlockState {
      start_pointer,
      doc_base,
      chunk_docs,
    }
  }
}

/// Immutable data shared by one term-vector field and its independent iterators.
#[derive(Default)]
pub struct TVTermData {
  prefix_lengths: Box<[usize]>,
  suffix_lengths: Box<[usize]>,
  term_freqs: Box<[usize]>,
  position_index: Box<[usize]>,
  positions: Box<[i32]>,
  start_offsets: Box<[i32]>,
  lengths: Box<[i32]>,
  payload_index: Box<[usize]>,
}

impl TVTermData {
  /// Moves the decoded field arrays into one shared immutable field owner.
  #[allow(clippy::too_many_arguments)]
  pub fn new(
    prefix_lengths: Vec<usize>,
    suffix_lengths: Vec<usize>,
    term_freqs: Vec<usize>,
    position_index: Vec<usize>,
    positions: Vec<i32>,
    start_offsets: Vec<i32>,
    lengths: Vec<i32>,
    payload_index: Vec<usize>,
  ) -> Self {
    Self {
      prefix_lengths: prefix_lengths.into_boxed_slice(),
      suffix_lengths: suffix_lengths.into_boxed_slice(),
      term_freqs: term_freqs.into_boxed_slice(),
      position_index: position_index.into_boxed_slice(),
      positions: positions.into_boxed_slice(),
      start_offsets: start_offsets.into_boxed_slice(),
      lengths: lengths.into_boxed_slice(),
      payload_index: payload_index.into_boxed_slice(),
    }
  }
}

pub struct TVFields {
  field_nums: Box<[usize]>,
  field_flags: Box<[u8]>,
  field_lengths: Box<[usize]>,

  term_data: Box<[Rc<TVTermData>]>,

  payload_offset: usize,
  payload_length: usize,
  suffix_bytes: BytesRef<Rc<Vec<u8>>>,

  field_infos: Arc<FieldInfos>,
}
impl TVFields {
  #[allow(clippy::too_many_arguments)]
  pub(crate) fn new(
    field_nums: smallvec::SmallVec<[i32; 4]>,
    field_flags: Vec<u8>,
    mut field_num_offs: Vec<usize>,
    field_lengths: Vec<usize>,
    term_data: Vec<Rc<TVTermData>>,
    payload_offset: usize,
    payload_length: usize,
    suffix_bytes: BytesRef<Rc<Vec<u8>>>,
    field_infos: Arc<FieldInfos>,
  ) -> Result<Self> {
    for off in &mut field_num_offs {
      let field_num = field_nums[*off];
      let field_info = field_infos.field_info_by_number(field_num)?;
      match field_info {
        Some(_) => {},
        None => {
          return Err(LuceneError::illegal_state(format!(
            "Field number {field_num} not found in field infos"
          )));
        },
      }
      *off = field_num as usize;
    }

    Ok(Self {
      // Reuse the document's offsets buffer for its validated field numbers.
      field_nums: field_num_offs.into_boxed_slice(),
      field_flags: field_flags.into_boxed_slice(),
      field_lengths: field_lengths.into_boxed_slice(),

      term_data: term_data.into_boxed_slice(),
      payload_offset,
      payload_length,
      suffix_bytes,
      field_infos,
    })
  }
}
pub struct TVFieldsIterator<'a> {
  fields: &'a TVFields,
  index: usize,
}

impl<'a> IteratorExt for TVFieldsIterator<'a> {
  type Item = &'a String;

  fn next(&mut self) -> Result<Option<Self::Item>> {
    if !self.has_next()? {
      return Ok(None);
    }
    let field_num = self.fields.field_nums[self.index] as i32;
    self.index += 1;
    let field_info = self.fields.field_infos.field_info_by_number(field_num)?;
    match field_info {
      Some(info) => Ok(Some(&info.name)),
      None => Err(LuceneError::illegal_state(format!(
        "Field number {field_num} not found in field infos"
      ))),
    }
  }

  fn has_next(&self) -> Result<bool> {
    Ok(self.index < self.fields.field_nums.len())
  }
}

impl Fields for TVFields {
  type FieldIter<'a> = TVFieldsIterator<'a>;

  fn iterator(&self) -> Result<Self::FieldIter<'_>> {
    Ok(TVFieldsIterator {
      fields: self,
      index: 0,
    })
  }

  type Terms = TVTerms;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    let field_info = match self.field_infos.field_info_by_name(field)? {
      Some(info) => info,
      None => return Ok(None),
    };

    let mut idx = None;
    for (i, &number) in self.field_nums.iter().enumerate() {
      if number as i32 == field_info.number {
        idx = Some(i);
        break;
      }
    }
    let Some(idx) = idx else {
      return Ok(None);
    };
    let num_terms = self.term_data[idx].prefix_lengths.len();
    if num_terms == 0 {
      // no term
      return Ok(None);
    }

    let mut field_off = 0;
    for (i, &len) in self.field_lengths.iter().enumerate() {
      if i < idx {
        field_off += len;
      } else {
        break;
      }
    }
    let field_len = self.field_lengths[idx];

    let term_bytes = BytesRef::from_slice(
      self.suffix_bytes.bytes.clone(),
      self.suffix_bytes.offset + field_off,
      field_len,
    );
    let tv_terms = TVTerms::new(
      num_terms,
      i32::from(self.field_flags[idx]),
      self.term_data[idx].clone(),
      self.payload_offset,
      self.payload_length,
      term_bytes,
    );
    Ok(Some(tv_terms))
  }

  fn size(&self) -> Result<i32> {
    debug_assert!(self.field_nums.len() <= i32::MAX as usize);
    Ok(self.field_nums.len() as i32)
  }
}

pub struct TVTerms {
  num_terms: usize,
  flags: i32,
  total_term_freq: i64,

  term_data: Rc<TVTermData>,

  payload_offset: usize,
  payload_length: usize,
  term_bytes: BytesRef<Rc<Vec<u8>>>,
}
impl TVTerms {
  #[allow(clippy::too_many_arguments)]
  pub(crate) fn new(
    num_terms: usize,
    flags: i32,
    term_data: Rc<TVTermData>,
    payload_offset: usize,
    payload_length: usize,
    term_bytes: BytesRef<Rc<Vec<u8>>>,
  ) -> Self {
    let total_term_freq = term_data.term_freqs.iter().map(|&x| x as i64).sum();

    TVTerms {
      num_terms,
      flags,
      term_data,
      payload_offset,
      payload_length,
      term_bytes,
      total_term_freq,
    }
  }
}
impl Terms for TVTerms {
  type TermsEnum = TVTermsEnum;

  fn iterator(&self) -> Result<Self::TermsEnum> {
    let terms_enum = TVTermsEnum::new(
      self.num_terms,
      self.flags,
      self.term_data.clone(),
      self.payload_offset,
      self.payload_length,
      ByteArrayDataInput::with_range(
        self.term_bytes.bytes.clone(),
        self.term_bytes.offset,
        self.term_bytes.length,
      ),
    )?;
    Ok(terms_enum)
  }

  type IntersectIter
    = FilteredTermsEnum<Self::TermsEnum, AutomatonTermsEnum>
  where
    Self::TermsEnum: BytesRefIterator,
    AutomatonTermsEnum: FilteredTermsEnumBase;
  fn intersect(
    &self,
    compiled: &CompiledAutomaton,
    start_term: Option<&BytesRef<Vec<u8>>>,
  ) -> Result<Self::IntersectIter> {
    self.default_intersect(compiled, start_term)
  }

  fn size(&self) -> Result<i64> {
    Ok(self.num_terms as i64)
  }

  fn get_sum_total_term_freq(&self) -> Result<i64> {
    Ok(self.total_term_freq)
  }

  fn get_sum_doc_freq(&self) -> Result<i64> {
    Ok(self.num_terms as i64)
  }

  fn get_doc_count(&self) -> Result<i32> {
    Ok(1)
  }

  fn has_freqs(&self) -> bool {
    true
  }

  fn has_offsets(&self) -> bool {
    (self.flags & OFFSETS) != 0
  }

  fn has_positions(&self) -> bool {
    (self.flags & POSITIONS) != 0
  }

  fn has_payloads(&self) -> bool {
    (self.flags & PAYLOADS) != 0
  }
}

pub struct TVTermsEnum {
  num_terms: usize,
  start_pos: usize,
  ord: Option<usize>,

  term_data: Rc<TVTermData>,

  input: ByteArrayDataInput<Rc<Vec<u8>>>,
  payload_offset: usize,
  payload_length: usize,
  term: BytesRef<Vec<u8>>,
}
impl TVTermsEnum {
  #[allow(clippy::too_many_arguments)]
  pub(crate) fn new(
    num_terms: usize,
    _flags: i32,
    term_data: Rc<TVTermData>,
    payload_offset: usize,
    payload_length: usize,
    input: ByteArrayDataInput<Rc<Vec<u8>>>,
  ) -> Result<Self> {
    let start_pos = input.get_position();
    debug_assert!(start_pos <= i32::MAX as usize);

    let mut term_enum = TVTermsEnum {
      num_terms,
      term_data,
      payload_offset,
      payload_length,
      input,
      start_pos,
      ord: None,
      term: BytesRef::with_capacity(16)?,
    };

    term_enum.reset();
    Ok(term_enum)
  }
  pub fn reset(&mut self) {
    self.term.length = 0;
    self.input.set_position(self.start_pos);
    self.ord = None;
  }
}

impl BytesRefIterator for TVTermsEnum {
  type Value<'a>
    = &'a BytesRef<Vec<u8>>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    if self
      .ord
      .map_or(self.num_terms == 0, |v| v + 1 == self.num_terms)
    {
      return Ok(None);
    }
    let ord = self.ord.map_or(0, |v| v + 1);
    self.ord = Some(ord);

    debug_assert!(ord < self.num_terms + 1);

    let prefix_len = self.term_data.prefix_lengths[ord];
    let suffix_len = self.term_data.suffix_lengths[ord];
    let total_len = prefix_len + suffix_len;

    self.term.offset = 0;
    self.term.length = total_len;

    if self.term.length > self.term.bytes.len() {
      ArrayUtil::grow_with(&mut self.term.bytes, self.term.length, || 0)?;
    }

    self
      .input
      .read_bytes(&mut self.term.bytes, prefix_len, suffix_len)?;
    self.ord = Some(ord);
    Ok(Some(&self.term))
  }
}

impl TermsEnum for TVTermsEnum {
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

  fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool> {
    Ok(self.seek_ceil(term)? == SeekStatus::Found)
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

  fn seek_ceil<BS: ByteSource>(&mut self, text: &BytesRef<BS>) -> Result<SeekStatus> {
    if let Some(ord) = self.ord
      && ord < self.num_terms
    {
      let cmp = self.term.compare_to(text).to_int();
      if cmp == 0 {
        return Ok(SeekStatus::Found);
      } else if cmp > 0 {
        self.reset();
      }
    }

    // linear scan
    loop {
      let term = self.next()?;
      match term {
        None => return Ok(SeekStatus::End),
        Some(t) => {
          let cmp = (*t).compare_to(text).to_int();
          if cmp > 0 {
            return Ok(SeekStatus::NotFound);
          } else if cmp == 0 {
            return Ok(SeekStatus::Found);
          }
        },
      }
    }
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
      return Err(LuceneError::illegal_state(format!(
        "term {} does not exist",
        term
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
    Ok(1)
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    let ord = self
      .ord
      .ok_or_else(|| LuceneError::illegal_state("ord is None"))?;
    Ok(self.term_data.term_freqs[ord] as i64)
  }

  type PostingsEnum = TVPostingsEnum;

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    _flags: i32,
  ) -> Result<Self::PostingsEnum> {
    let ord = self
      .ord
      .ok_or_else(|| LuceneError::illegal_state("ord is None"))?;
    let freq = self.term_data.term_freqs[ord];
    let position_index = self.term_data.position_index[ord];
    let term_data = self.term_data.clone();
    let payloads = BytesRef::from_slice(
      self.input.bytes.clone(),
      self.payload_offset,
      self.payload_length,
    );
    let docs_enum = match reuse {
      Some(mut docs_enum) => {
        docs_enum.reset(freq, position_index, term_data, payloads);
        docs_enum
      },
      None => TVPostingsEnum::with_term(freq, position_index, term_data, payloads),
    };
    Ok(docs_enum)
  }

  type ImpactsEnum = SlowImpactsEnum<TVPostingsEnum>;

  fn impacts(&mut self, _flags: i32) -> Result<Self::ImpactsEnum> {
    let delegate = self.postings_with_flags(None, FREQS as i32)?;
    Ok(SlowImpactsEnum::new(delegate))
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    Ok(BaseTermsEnumTermStateImpl.into())
  }
}

pub struct TVPostingsEnum {
  term_data: Rc<TVTermData>,
  doc: i32,
  term_freq: usize,
  position_index: usize,

  payload: BytesRef<Rc<Vec<u8>>>,
  base_payload_offset: usize,
  i: Option<usize>,

  payload_length: usize,
  payload_offset: usize,
}
struct EmptyTVBuffers {
  term_data: Rc<TVTermData>,
  payload: Rc<Vec<u8>>,
}
impl Default for TVPostingsEnum {
  fn default() -> Self {
    Self::new()
  }
}

impl TVPostingsEnum {
  pub fn new() -> Self {
    thread_local! {
      static EMPTY_BUFFERS: EmptyTVBuffers = EmptyTVBuffers {
        term_data: Rc::new(TVTermData::default()),
        payload: Rc::new(Vec::new()),
      };
    }
    let (term_data, payload) =
      EMPTY_BUFFERS.with(|buffers| (Rc::clone(&buffers.term_data), Rc::clone(&buffers.payload)));
    Self::with_term(
      0,
      0,
      term_data,
      BytesRef {
        bytes: payload,
        offset: 0,
        length: 0,
      },
    )
  }

  fn with_term(
    term_freq: usize,
    position_index: usize,
    term_data: Rc<TVTermData>,
    payloads: BytesRef<Rc<Vec<u8>>>,
  ) -> Self {
    Self {
      term_data,
      doc: -1,
      term_freq,
      position_index,
      payload: BytesRef {
        bytes: payloads.bytes,
        offset: 0,
        length: 0,
      },
      base_payload_offset: payloads.offset,
      i: None,
      payload_length: 0,
      payload_offset: 0,
    }
  }
  #[allow(clippy::too_many_arguments)]
  pub fn reset(
    &mut self,
    freq: usize,
    position_index: usize,
    term_data: Rc<TVTermData>,
    payloads: BytesRef<Rc<Vec<u8>>>,
  ) {
    self.term_freq = freq;
    self.position_index = position_index;
    self.term_data = term_data;

    self.base_payload_offset = payloads.offset;

    self.payload.bytes = payloads.bytes;
    self.payload.offset = 0;
    self.payload.length = 0;

    self.payload_length = 0;
    self.payload_offset = 0;

    self.doc = -1;
    self.i = None;
  }

  fn check_doc(&self) -> Result<()> {
    if self.doc == NO_MORE_DOCS {
      Err(LuceneError::illegal_state("DocsEnum exhausted"))
    } else if self.doc == -1 {
      Err(LuceneError::illegal_state("DocsEnum not started"))
    } else {
      Ok(())
    }
  }
  fn check_position(&self) -> Result<usize> {
    self.check_doc()?;
    match self.i {
      None => Err(LuceneError::illegal_state("Position enum not started")),
      Some(i) if i >= self.term_freq => Err(LuceneError::illegal_state("Read past last position")),
      Some(i) => Ok(i),
    }
  }
}

impl crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions for TVPostingsEnum {}
impl crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for TVPostingsEnum {}

impl DocIdSetIterator for TVPostingsEnum {
  fn doc_id(&self) -> i32 {
    self.doc
  }

  fn next_doc(&mut self) -> Result<i32> {
    if self.doc == -1 {
      self.doc = 0;
    } else {
      self.doc = NO_MORE_DOCS;
    }
    Ok(self.doc)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.slow_advance(target)
  }

  fn cost(&self) -> Result<i64> {
    Ok(1)
  }
}

impl PostingsEnum for TVPostingsEnum {
  fn freq(&mut self) -> Result<i32> {
    self.check_doc()?;
    self.term_freq.try_convert()
  }

  fn next_position(&mut self) -> Result<i32> {
    if self.doc != 0 {
      return Err(LuceneError::illegal_state(""));
    }
    let i;
    let equal = match self.i {
      Some(idx) => {
        i = idx + 1;
        idx + 1 >= self.term_freq
      },
      None => {
        // -1 + 1
        i = 0;
        self.term_freq == 0
      },
    };
    self.i = Some(i);
    if equal {
      return Err(LuceneError::illegal_state("Read past last position"));
    }
    if !self.term_data.payload_index.is_empty() {
      let index = self.position_index + i;
      self.payload_offset = self.base_payload_offset + self.term_data.payload_index[index];
      self.payload_length =
        self.term_data.payload_index[index + 1] - self.term_data.payload_index[index];
    }
    if self.term_data.positions.is_empty() {
      Ok(-1)
    } else {
      Ok(self.term_data.positions[self.position_index + i])
    }
  }

  fn start_offset(&self) -> Result<i32> {
    let i = self.check_position()?;
    if self.term_data.start_offsets.is_empty() {
      Ok(-1)
    } else {
      Ok(self.term_data.start_offsets[self.position_index + i])
    }
  }

  fn end_offset(&self) -> Result<i32> {
    let i = self.check_position()?;
    if self.term_data.start_offsets.is_empty() {
      Ok(-1)
    } else {
      let index = self.position_index + i;
      Ok(self.term_data.start_offsets[index] + self.term_data.lengths[index])
    }
  }

  fn get_payload(&self) -> Result<Option<BytesRefValueEnum<'_>>> {
    let _ = self.check_position()?;
    if self.term_data.payload_index.is_empty() || self.payload_length == 0 {
      Ok(None)
    } else {
      let bytes =
        &self.payload.bytes[self.payload_offset..self.payload_offset + self.payload_length];
      Ok(Some(BytesRefValueEnum::Slice(BytesRef {
        bytes,
        offset: 0,
        length: self.payload_length,
      })))
    }
  }
}
const PREFETCH_CACHE_SIZE: usize = 1 << 4;
pub(crate) const PREFETCH_CACHE_MASK: usize = PREFETCH_CACHE_SIZE - 1;
pub(crate) fn sum(arr: &[usize]) -> usize {
  let mut sum = 0;
  for &el in arr {
    sum += el;
  }
  sum
}
