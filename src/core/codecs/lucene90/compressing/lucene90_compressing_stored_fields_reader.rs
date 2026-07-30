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
use crate::core::codecs::compressing::lucene90_compressing_stored_fields_writer::{
  BYTE_ARR, DAY, DAY_ENCODING, FIELDS_EXTENSION, HOUR, HOUR_ENCODING, INDEX_CODEC_NAME,
  INDEX_EXTENSION, META_EXTENSION, META_VERSION_START, NUMERIC_DOUBLE, NUMERIC_FLOAT, NUMERIC_INT,
  NUMERIC_LONG, SECOND, SECOND_ENCODING, STRING, TYPE_BITS, TYPE_MASK, VERSION_CURRENT,
  VERSION_START,
};
use crate::core::codecs::compressing::stored_fields_ints::StoredFieldsInts;
use crate::core::codecs::compression::compression_mode::{
  CompressionModeBase, CompressionModeEnum, DecompressorEnum,
};
use crate::core::codecs::compression::decompressor::Decompressor;
use crate::core::codecs::lucene90::fields_index::FieldsIndex;
use crate::core::codecs::lucene90::fields_index_reader::FieldsIndexReader;
use crate::core::codecs::stored_fields_reader::{DefaultStoredFieldsReader, StoredFieldsReader};
use crate::core::codecs::stored_fields_writer::StoredFieldsWriter;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::stored_field_visitor::{Status, StoredFieldVisitor};
use crate::core::index::stored_fields::{RawStoredFieldsReader, StoredFields};
use crate::core::index::{BytesRef, IndexFileNames};
use crate::core::store::directory::Directory;
use crate::core::store::{
  ByteArrayDataInput, DataInput, DataInputEnum3, IOContext, IndexInput, ReadAdvice,
};
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::clone::TryClone as OtherClone;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{IOUtils, SliceCopyOps, TryIntoInt};
use std::cmp::min;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const PREFETCH_CACHE_SIZE: usize = 1 << 4;
const PREFETCH_CACHE_MASK: usize = PREFETCH_CACHE_SIZE - 1;
/// [`StoredFieldsReader`] implementation for
/// [`Lucene90CompressingStoredFieldsFormat`](crate::core::codecs::lucene90::compressing::lucene90_compressing_stored_fields_format::Lucene90CompressingStoredFieldsFormat).
pub struct Lucene90CompressingStoredFieldsReader<I>
where
  I: IndexInput,
{
  version: i32,
  field_infos: Arc<FieldInfos>,
  index_reader: FieldsIndexReader<I>,
  max_pointer: usize,
  chunk_size: i32,
  compression_mode: CompressionModeEnum,
  decompressor: DecompressorEnum,
  num_docs: i32,
  merging: bool,
  state: BlockState<I>,
  // number of written blocks
  num_chunks: i64,
  // number of incomplete compressed blocks written
  num_dirty_chunks: i64,
  // cumulative number of docs in incomplete chunks
  num_dirty_docs: i64,
  // Cache of recently prefetched block IDs. This helps reduce chances of
  // prefetching the same block multiple times, which is otherwise likely
  // due to index sorting or recursive graph bisection clustering similar
  // documents together. NOTE: this cache must be small since it's fully
  // scanned.
  prefetched_block_id_cache: [i64; PREFETCH_CACHE_SIZE],
  prefetched_block_id_cache_index: usize,
  closed: AtomicBool,
}

impl<I> Lucene90CompressingStoredFieldsReader<I>
where
  I: IndexInput,
{
  // -0 isn't compressed.
  const NEGATIVE_ZERO_FLOAT: u32 = (-0f32).to_bits();
  const NEGATIVE_ZERO_DOUBLE: u64 = (-0f64).to_bits();

  // for compression of timestamps
  const SECOND: i64 = 1_000;
  const HOUR: i64 = 60 * 60 * Self::SECOND;
  const DAY: i64 = 24 * Self::HOUR;

  const SECOND_ENCODING: u8 = 0x40;
  const HOUR_ENCODING: u8 = 0x80;
  const DAY_ENCODING: u8 = 0xC0;
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
    D2: Directory,
  {
    let segment = &si.name;
    let num_docs = si.max_doc()?;

    let fields_stream_fn =
      IndexFileNames::segment_file_name(segment, segment_suffix, FIELDS_EXTENSION);
    let mut meta_in = None;
    let mut fields_stream = None;
    let mut fields_index_reader = None;
    let mut reader = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      fields_stream = Some(dir.open_input(
        &fields_stream_fn,
        &context.with_read_advice_self(ReadAdvice::Random)?,
      )?);
      let fields_stream_ref = fields_stream
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("stored fields input is missing"))?;

      let version = CodecUtil::check_index_header(
        fields_stream_ref,
        format_name,
        VERSION_START,
        VERSION_CURRENT,
        si.get_id(),
        segment_suffix,
      )?;

      debug_assert_eq!(
        CodecUtil::index_header_length(format_name, segment_suffix),
        fields_stream_ref.get_file_pointer()?
      );

      let meta_stream_fm =
        IndexFileNames::segment_file_name(segment, segment_suffix, META_EXTENSION);
      meta_in = Some(dir.open_checksum_input(&meta_stream_fm)?);
      let meta = meta_in.as_mut().unwrap();

      CodecUtil::check_index_header(
        meta,
        &format!("{}Meta", INDEX_CODEC_NAME),
        META_VERSION_START,
        version,
        si.get_id(),
        segment_suffix,
      )?;

      let chunk_size = meta.read_vint()?;

      let decompressor = compression_mode.new_decompressor();
      let prefetched_block_id_cache = [-1i64; PREFETCH_CACHE_SIZE];

      let merging = false;
      // NOTE: data file is too costly to verify checksum against all the
      // bytes on open, but for now we at least verify proper
      // structure of the checksum footer: which looks
      // for FOOTER_MAGIC + algorithmID. This is cheap and can detect some
      // forms of corruption such as file truncation.
      CodecUtil::retrieve_checksum(fields_stream_ref)?;
      fields_index_reader = Some(FieldsIndexReader::new(
        dir,
        si.name.to_string(),
        segment_suffix,
        INDEX_EXTENSION,
        INDEX_CODEC_NAME,
        si.get_id(),
        meta,
        context,
      )?);
      let max_pointer = fields_index_reader
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("stored fields index input is missing"))?
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
      };
      let state = BlockState::new(
        merging,
        fields_stream
          .take()
          .ok_or_else(|| LuceneError::illegal_state("stored fields input is missing"))?,
        compression_mode.new_decompressor(),
        chunk_size,
      );
      reader = Some(Self {
        version,
        field_infos,
        index_reader: fields_index_reader
          .take()
          .ok_or_else(|| LuceneError::illegal_state("stored fields index input is missing"))?,
        max_pointer,
        chunk_size,
        compression_mode,
        decompressor,
        num_docs,
        merging,
        state,
        num_chunks,
        num_dirty_chunks,
        num_dirty_docs,
        prefetched_block_id_cache,
        prefetched_block_id_cache_index: 0,
        closed: AtomicBool::new(false),
      });
      CodecUtil::check_footer(meta)?;
      meta.close()?;
      Ok(())
    }));
    match result {
      Ok(Ok(())) => reader
        .take()
        .ok_or_else(|| LuceneError::illegal_state("stored fields reader is missing")),
      result => {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
          match result {
            Ok(Err(error)) => match meta_in.as_mut() {
              Some(meta) => Err(CodecUtil::check_footer_with_error(meta, error)),
              None => Err(error),
            },
            Err(payload) => {
              if let Some(meta) = meta_in.as_mut() {
                let error = LuceneError::tragedy_from_panic(
                  "panic while constructing stored fields reader",
                  payload.as_ref(),
                );
                if let error @ LuceneError::CorruptIndex(_) =
                  CodecUtil::check_footer_with_error(meta, error)
                {
                  return Err(error);
                }
              }
              std::panic::resume_unwind(payload)
            },
            Ok(Ok(())) => unreachable!(),
          }
        }));
        IOUtils::close_resources_while_handling_error((
          reader.as_ref(),
          fields_index_reader.as_ref(),
          fields_stream.as_ref(),
          meta_in.as_ref(),
        ))?;
        match result {
          Ok(Err(error)) => Err(error),
          Ok(Ok(())) => Err(LuceneError::illegal_state(
            "stored fields reader construction failed without an error",
          )),
          Err(payload) => std::panic::resume_unwind(payload),
        }
      },
    }
  }

  fn with_reader(
    reader: &Lucene90CompressingStoredFieldsReader<I>,
    merging: bool,
  ) -> Result<Lucene90CompressingStoredFieldsReader<I>> {
    let fields_stream = reader.state.fields_stream.try_clone()?;
    Ok(Self {
      version: reader.version,
      field_infos: Arc::clone(&reader.field_infos),
      index_reader: reader.index_reader.try_clone()?,
      max_pointer: reader.max_pointer,
      chunk_size: reader.chunk_size,
      compression_mode: reader.compression_mode.clone(),
      decompressor: reader.decompressor.clone(),
      num_docs: reader.num_docs,
      merging,
      state: BlockState::new(
        merging,
        fields_stream,
        reader.compression_mode.new_decompressor(),
        reader.chunk_size,
      ),
      num_chunks: reader.num_dirty_chunks,
      num_dirty_chunks: reader.num_dirty_docs,
      num_dirty_docs: reader.num_dirty_docs,
      prefetched_block_id_cache: [-1i64; PREFETCH_CACHE_SIZE],
      prefetched_block_id_cache_index: 0,
      closed: AtomicBool::new(false),
    })
  }

  /// Ensures the reader is open.
  ///
  /// # Errors
  ///
  /// Returns `LuceneError::AlreadyClosed` if this `FieldsReader` is closed.
  pub fn ensure_open(&self) -> Result<()> {
    if self.closed.load(Ordering::Relaxed) {
      Err(LuceneError::already_closed("this FieldsReader is closed"))
    } else {
      Ok(())
    }
  }
  pub fn read_field<S>(
    input: &mut impl DataInput,
    visitor: &mut impl StoredFieldVisitor,
    info: Arc<FieldInfo>,
    bits: i32,
    writer: Option<&mut S>,
  ) -> Result<()>
  where
    S: StoredFieldsWriter,
  {
    match bits & *TYPE_MASK as i32 {
      BYTE_ARR => {
        let length = input.read_vint()?;
        visitor.binary_field_with_input(info, input, length, writer)?;
      },
      STRING => {
        let s = input.read_string()?;
        visitor.string_field(info, s, writer)?;
      },
      NUMERIC_INT => {
        let v = input.read_zint()?;
        visitor.int_field(info, v, writer)?;
      },
      NUMERIC_FLOAT => {
        let v = read_zfloat(input)?;
        visitor.float_field(info, v, writer)?;
      },
      NUMERIC_LONG => {
        let v = read_tlong(input)?;
        visitor.long_field(info, v, writer)?;
      },
      NUMERIC_DOUBLE => {
        let v = read_zdouble(input)?;
        visitor.double_field(info, v, writer)?;
      },
      other => {
        return Err(LuceneError::illegal_state(format!(
          "Unknown type flag: {other:x}"
        )));
      },
    }
    Ok(())
  }
  fn skip_field(input: &mut impl DataInput, bits: i32) -> Result<()> {
    match bits & *TYPE_MASK as i32 {
      BYTE_ARR | STRING => {
        let length = input.read_vint()?;
        input.skip_bytes(length as i64)?;
      },
      NUMERIC_INT => {
        input.read_zint()?;
      },
      NUMERIC_FLOAT => {
        read_zfloat(input)?;
      },
      NUMERIC_LONG => {
        read_tlong(input)?;
      },
      NUMERIC_DOUBLE => {
        read_zdouble(input)?;
      },
      other => {
        return Err(LuceneError::illegal_state(format!(
          "Unknown type flag: {other:x}"
        )));
      },
    }
    Ok(())
  }
  pub(crate) fn serialized_document(&mut self, doc_id: i32) -> Result<SerializedDocument<'_, I>> {
    if !self.state.contains(doc_id) {
      let pointer = self.index_reader.get_start_pointer(doc_id)?;
      self.state.fields_stream.seek(pointer)?;
      self.state.reset(doc_id, self.num_docs)?;
    }

    debug_assert!(self.state.contains(doc_id));
    self.state.document(doc_id)
  }
  /// Checks if a given docID was loaded in the current block state.
  pub(crate) fn is_loaded(&self, doc_id: i32) -> Result<bool> {
    if !self.merging {
      return Err(LuceneError::illegal_state(
        "is_loaded should only ever get called on a merge instance",
      ));
    }

    if self.version != VERSION_CURRENT {
      return Err(LuceneError::illegal_state(
        "is_loaded should only ever get called when the reader is on the current version",
      ));
    }

    Ok(self.state.contains(doc_id))
  }
  pub(crate) fn get_version(&self) -> i32 {
    self.version
  }
  pub(crate) fn get_compression_mode(&self) -> &CompressionModeEnum {
    &self.compression_mode
  }
  pub(crate) fn get_index_reader(&mut self) -> &mut FieldsIndexReader<I> {
    &mut self.index_reader
  }
  pub(crate) fn get_max_pointer(&self) -> usize {
    self.max_pointer
  }
  pub(crate) fn get_fields_stream(&mut self) -> &mut I {
    &mut self.state.fields_stream
  }
  pub fn get_chunk_size(&self) -> i32 {
    self.chunk_size
  }

  pub fn num_docs(&self) -> i32 {
    self.num_docs
  }

  pub fn get_num_dirty_docs(&self) -> Result<i64> {
    if self.version != VERSION_CURRENT {
      return Err(LuceneError::illegal_state(
        "getNumDirtyDocs should only ever get called when the reader is on the current version",
      ));
    }
    debug_assert!(self.num_dirty_docs >= 0);
    Ok(self.num_dirty_docs)
  }

  pub fn get_num_dirty_chunks(&self) -> Result<i64> {
    if self.version != VERSION_CURRENT {
      return Err(LuceneError::illegal_state(
        "getNumDirtyChunks should only ever get called when the reader is on the current version",
      ));
    }
    debug_assert!(self.num_dirty_chunks >= 0);
    Ok(self.num_dirty_chunks)
  }

  pub fn get_num_chunks(&self) -> Result<i64> {
    if self.version != VERSION_CURRENT {
      return Err(LuceneError::illegal_state(
        "getNumChunks should only ever get called when the reader is on the current version",
      ));
    }
    debug_assert!(self.num_chunks >= 0);
    Ok(self.num_chunks)
  }
}

impl<I> CloseableRef for Lucene90CompressingStoredFieldsReader<I>
where
  I: IndexInput,
{
  fn close(&self) -> Result<()> {
    if self.closed.load(Ordering::Relaxed) {
      return Ok(());
    }

    IOUtils::close_refs_tuple((Some(&self.index_reader), Some(&self.state.fields_stream)))?;
    self.closed.store(true, Ordering::Relaxed);
    Ok(())
  }
}

impl<I> StoredFields for Lucene90CompressingStoredFieldsReader<I>
where
  I: IndexInput,
{
  fn prefetch(&mut self, doc_id: i32) -> Result<()> {
    let block_id = self.index_reader.get_block_id(doc_id)?;
    for &prefetched in &self.prefetched_block_id_cache {
      if prefetched == block_id {
        return Ok(());
      }
    }

    let block_start_pointer = self.index_reader.get_block_start_pointer(block_id)?;
    let block_length = self.index_reader.get_block_length(block_id)?;

    self
      .state
      .fields_stream
      .prefetch(block_start_pointer, block_length)?;

    self.prefetched_block_id_cache[self.prefetched_block_id_cache_index & PREFETCH_CACHE_MASK] =
      block_id;
    self.prefetched_block_id_cache_index += 1;

    Ok(())
  }

  fn document_with_visitor<S>(
    &mut self,
    doc_id: i32,
    visitor: &mut impl StoredFieldVisitor,
    mut writer: Option<&mut S>,
  ) -> Result<()>
  where
    S: StoredFieldsWriter,
  {
    let field_infos = &self.field_infos.clone();
    let mut doc = self.serialized_document(doc_id)?;
    for field_idx in 0..doc.num_stored_fields {
      let info_and_bits = doc.input.read_vlong()?;
      let field_number = ((info_and_bits as u64 >> *TYPE_BITS) as i64) as i32;
      let field_info = field_infos.field_info_by_number(field_number)?;
      let bits = (info_and_bits & *TYPE_MASK) as i32;

      debug_assert!(
        bits <= NUMERIC_DOUBLE,
        "bits={bits:#x} is out of valid range"
      );
      match field_info {
        Some(field_info) => {
          match visitor.needs_field(field_info.clone(), writer.as_deref_mut())? {
            Status::Yes => {
              Self::read_field(
                &mut doc.input,
                visitor,
                field_info.clone(),
                bits,
                writer.as_deref_mut(),
              )?;
            },
            Status::No => {
              // don't skipField on last field value; treat like
              // STOP
              if field_idx == doc.num_stored_fields - 1 {
                return Ok(());
              }
              Self::skip_field(&mut doc.input, bits)?;
            },
            Status::Stop => return Ok(()),
          }
        },
        None => {
          return Err(LuceneError::illegal_state(format!(
            "field_info is None with number: {field_number}"
          )));
        },
      }
    }
    Ok(())
  }
}

impl<I> OtherClone for Lucene90CompressingStoredFieldsReader<I>
where
  I: IndexInput,
{
  fn try_clone(&self) -> Result<Self>
  where
    Self: Sized,
  {
    self.ensure_open()?;
    Lucene90CompressingStoredFieldsReader::with_reader(self, false)
  }
}

impl<I> StoredFieldsReader for Lucene90CompressingStoredFieldsReader<I>
where
  I: IndexInput,
{
  fn check_integrity(&self) -> Result<()> {
    self.index_reader.check_integrity()?;
    CodecUtil::checksum_entire_file(&self.state.fields_stream)?;
    Ok(())
  }

  fn get_merge_instance(&self) -> Result<Option<Self>>
  where
    Self: Sized,
  {
    self.ensure_open()?;
    Ok(Some(Lucene90CompressingStoredFieldsReader::with_reader(
      self, true,
    )?))
  }
}

impl<I> RawStoredFieldsReader for Lucene90CompressingStoredFieldsReader<I>
where
  I: IndexInput,
{
  type IndexInput = I;

  fn raw_stored_fields_mut(&mut self) -> Result<&mut DefaultStoredFieldsReader<Self::IndexInput>> {
    Ok(self)
  }

  fn raw_stored_fields(&self) -> Result<&DefaultStoredFieldsReader<Self::IndexInput>> {
    Ok(self)
  }
}

/// Keeps state about the current block of documents.
struct BlockState<I>
where
  I: IndexInput,
{
  doc_base: usize,
  chunk_docs: i32,
  /// Whether the block has been sliced, this happens for large documents.
  sliced: bool,
  offsets: Vec<i64>,
  num_stored_fields: Vec<i64>,
  start_pointer: usize,
  spare: BytesRef<Vec<u8>>,
  spare2: BytesRef<Vec<u8>>,
  bytes: BytesRef<Arc<Vec<u8>>>,
  merging: bool,
  fields_stream: I,
  decompressor: DecompressorEnum,
  chunk_size: i32,
}
impl<I> BlockState<I>
where
  I: IndexInput,
{
  /// Creates a new `BlockState` with default values.
  fn new(merging: bool, fields_stream: I, decompressor: DecompressorEnum, chunk_size: i32) -> Self {
    BlockState {
      doc_base: 0,
      chunk_docs: 0,
      sliced: false,
      offsets: Vec::new(),
      num_stored_fields: Vec::new(),
      start_pointer: 0,
      spare: BytesRef::new(),
      spare2: BytesRef::new(),
      bytes: BytesRef::new(),
      merging,
      fields_stream,
      decompressor,
      chunk_size,
    }
  }

  fn contains(&self, doc_id: i32) -> bool {
    doc_id >= self.doc_base as i32 && doc_id < self.doc_base as i32 + self.chunk_docs
  }
  /// Reset this block so that it stores state for the block that contains the
  /// given doc id.
  fn reset(&mut self, doc_id: i32, num_docs: i32) -> Result<()> {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      self.do_reset(doc_id, num_docs)
    }));
    if !matches!(&result, Ok(Ok(()))) {
      // if the read failed, set chunkDocs to 0 so that it does not
      // contain any docs anymore and is not reused. This should help
      // get consistent errors when trying to get several
      // documents which are in the same corrupted block since it will
      // force the header to be decoded again
      self.chunk_docs = 0;
    }
    match result {
      Ok(result) => result,
      Err(payload) => std::panic::resume_unwind(payload),
    }
  }

  fn do_reset(&mut self, doc_id: i32, num_docs: i32) -> Result<()> {
    self.doc_base = self.fields_stream.read_vint()?.try_convert()?;
    let token = self.fields_stream.read_vint()?;
    self.chunk_docs = ((token as u32) >> 2) as i32;

    if !self.contains(doc_id) || self.doc_base as i32 + self.chunk_docs > num_docs {
      return Err(LuceneError::corrupt_index(format!(
        "Corrupted: docID={}, docBase={}, chunkDocs={}, numDocs={} (resource={})",
        doc_id, self.doc_base, self.chunk_docs, num_docs, self.fields_stream
      )));
    }

    self.sliced = (token & 1) != 0;

    ArrayUtil::grow_no_copy(&mut self.offsets, self.chunk_docs as usize + 1)?;
    ArrayUtil::grow_no_copy(&mut self.num_stored_fields, self.chunk_docs as usize)?;

    if self.chunk_docs == 1 {
      self.num_stored_fields[0] = self.fields_stream.read_vint()? as i64;
      self.offsets[1] = self.fields_stream.read_vint()? as i64;
    } else {
      // Number of stored fields per document
      StoredFieldsInts::read_ints(
        &mut self.fields_stream,
        self.chunk_docs,
        &mut self.num_stored_fields,
        0,
      )?;
      // The stream encodes the length of each document and we decode
      // it into a list of monotonically increasing offsets
      StoredFieldsInts::read_ints(
        &mut self.fields_stream,
        self.chunk_docs,
        &mut self.offsets,
        1,
      )?;

      for i in 0..self.chunk_docs as usize {
        self.offsets[i + 1] += self.offsets[i];
      }
      // Additional validation: only the empty document has a serialized
      // length of 0
      for i in 0..self.chunk_docs as usize {
        let len = self.offsets[i + 1] - self.offsets[i];
        let stored_fields = self.num_stored_fields[i];
        if (len == 0) != (stored_fields == 0) {
          return Err(LuceneError::corrupt_index(format!(
            "length={len}, numStoredFields={stored_fields} (resource={})",
            self.fields_stream
          )));
        }
      }
    }

    self.start_pointer = self.fields_stream.get_file_pointer()?;

    if self.merging {
      let total_length = self.offsets[self.chunk_docs as usize].try_convert()?;
      // decompress eagerly
      if self.sliced {
        self.spare2.offset = 0;
        self.spare2.length = 0;
        let mut decompressed = 0;
        while decompressed < total_length {
          let to_decompress = min(total_length - decompressed, self.chunk_size);
          self.decompressor.decompress(
            &mut self.fields_stream,
            to_decompress,
            0,
            to_decompress,
            &mut self.spare,
          )?;

          let new_len = self.spare2.length + self.spare.length;
          ArrayUtil::grow_with_len(&mut self.spare2.bytes, new_len)?;
          self.spare2.bytes.copy_from(
            &self.spare.bytes[self.spare.offset..(self.spare.offset + self.spare.length)],
            self.spare2.length,
          );
          self.spare2.length = new_len;
          decompressed += to_decompress;
        }
      } else {
        self.decompressor.decompress(
          &mut self.fields_stream,
          total_length,
          0,
          total_length,
          &mut self.spare2,
        )?;
      }
      let bytes = std::mem::take(&mut self.spare2.bytes);
      self.bytes = BytesRef::from_slice(Arc::new(bytes), self.spare2.offset, self.spare2.length);
      if self.bytes.length != total_length as usize {
        return Err(LuceneError::corrupt_index(format!(
          "Corrupted: expected chunk size = {}, got {} (resource={})",
          total_length, self.bytes.length, self.fields_stream
        )));
      }
    }
    Ok(())
  }
  /// Get the serialized representation of the given docID.
  /// This docID has to be contained in the current block.
  pub fn document(&mut self, doc_id: i32) -> Result<SerializedDocument<'_, I>> {
    if !self.contains(doc_id) {
      return Err(LuceneError::illegal_argument(""));
    }

    let index = (doc_id - self.doc_base as i32) as usize;
    let offset = self.offsets[index].try_convert()?;
    let length = (self.offsets[index + 1] - self.offsets[index]).try_convert()?;
    let total_length = self.offsets[self.chunk_docs as usize].try_convert()?;
    let num_stored_fields = self.num_stored_fields[index].try_convert()?;

    let document_input = if length == 0 {
      DataInputEnum3::A(ByteArrayDataInput::new())
    } else if self.merging {
      DataInputEnum3::C(ByteArrayDataInput::with_range(
        self.bytes.bytes.clone(),
        self.bytes.offset + offset as usize,
        length,
      ))
    } else {
      let mut bytes = BytesRef::new();
      self.fields_stream.seek(self.start_pointer)?;

      if self.sliced {
        self.decompressor.decompress(
          &mut self.fields_stream,
          self.chunk_size,
          offset,
          min(length as i32, self.chunk_size - offset),
          &mut bytes,
        )?;
        DataInputEnum3::B(DataInputImpl::new(
          &mut self.decompressor,
          self.chunk_size,
          &mut self.fields_stream,
          bytes,
          length as i32,
        ))
      } else {
        self.decompressor.decompress(
          &mut self.fields_stream,
          total_length,
          offset,
          length as i32,
          &mut bytes,
        )?;
        debug_assert_eq!(bytes.length, length);
        DataInputEnum3::A(ByteArrayDataInput::with_range(
          std::mem::take(&mut bytes.bytes),
          bytes.offset,
          bytes.length,
        ))
      }
    };

    Ok(SerializedDocument::new(
      document_input,
      length as i32,
      num_stored_fields,
    ))
  }
}

type DataInputs<'a, I> = DataInputEnum3<
  ByteArrayDataInput<Vec<u8>>,
  DataInputImpl<'a, I>,
  ByteArrayDataInput<Arc<Vec<u8>>>,
>;
/// A serialized document. You need to decode its input to get an actual
/// `Document`.
pub struct SerializedDocument<'a, I>
where
  I: IndexInput,
{
  /// The serialized data input.
  pub(crate) input: DataInputs<'a, I>,

  /// The number of bytes on which the document is encoded.
  pub(crate) length: i32,

  /// The number of stored fields in the document.
  pub(crate) num_stored_fields: i32,
}

impl<'a, I> SerializedDocument<'a, I>
where
  I: IndexInput,
{
  fn new(input: DataInputs<'a, I>, length: i32, num_stored_fields: i32) -> Self {
    SerializedDocument {
      input,
      length,
      num_stored_fields,
    }
  }
}

pub struct DataInputImpl<'a, I>
where
  I: IndexInput,
{
  decompressed: i32,
  length: i32,
  decompressor: &'a mut DecompressorEnum,
  chunk_size: i32,
  fields_stream: &'a mut I,
  bytes: BytesRef<Vec<u8>>,
}
impl<'a, I> DataInputImpl<'a, I>
where
  I: IndexInput,
{
  fn new(
    decompressor: &'a mut DecompressorEnum,
    chunk_size: i32,
    fields_stream: &'a mut I,
    bytes: BytesRef<Vec<u8>>,
    length: i32,
  ) -> Self {
    let decompressed = bytes.length as i32;
    DataInputImpl {
      decompressed,
      length,
      decompressor,
      chunk_size,
      fields_stream,
      bytes,
    }
  }
  fn fill_buffer(&mut self) -> Result<()> {
    debug_assert!(self.decompressed <= self.length);

    if self.decompressed == self.length {
      return Err(LuceneError::eof(""));
    }

    let to_decompress = std::cmp::min(self.length - self.decompressed, self.chunk_size);
    self.decompressor.decompress(
      self.fields_stream,
      to_decompress,
      0,
      to_decompress,
      &mut self.bytes,
    )?;
    self.decompressed += to_decompress;
    Ok(())
  }
}

impl<I> Display for DataInputImpl<'_, I>
where
  I: IndexInput,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "DataInputImpl in Lucene90CompressingStoredFieldsReader")
  }
}

impl<I> crate::core::util::close::Closeable for DataInputImpl<'_, I> where I: IndexInput {}

impl<I> DataInput for DataInputImpl<'_, I>
where
  I: IndexInput,
{
  fn read_byte(&mut self) -> Result<u8> {
    if self.bytes.length == 0 {
      self.fill_buffer()?;
    }
    self.bytes.length -= 1;
    let b = self.bytes.bytes[self.bytes.offset];
    self.bytes.offset += 1;
    Ok(b)
  }

  fn read_bytes(&mut self, b: &mut [u8], mut offset: usize, mut len: usize) -> Result<()> {
    while len > self.bytes.length {
      b.copy_from(
        &self.bytes.bytes[self.bytes.offset..(self.bytes.offset + self.bytes.length)],
        offset,
      );
      len -= self.bytes.length;
      offset += self.bytes.length;
      self.fill_buffer()?;
    }
    b.copy_from(
      &self.bytes.bytes[self.bytes.offset..(self.bytes.offset + len)],
      offset,
    );
    self.bytes.offset += len;
    self.bytes.length -= len;
    Ok(())
  }

  fn read_group_vint(&mut self, dst: &mut [i32], offset: usize) -> Result<()> {
    GroupVIntUtil::read_group_vint_i32(self, dst, offset)
  }

  fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
    if num_bytes < 0 {
      return Err(LuceneError::illegal_argument(format!(
        "num_bytes must be >= 0, got {num_bytes}"
      )));
    }
    let mut num_bytes = num_bytes as usize;

    while num_bytes > self.bytes.length {
      num_bytes -= self.bytes.length;
      self.fill_buffer()?;
    }
    self.bytes.offset += num_bytes;
    self.bytes.length -= num_bytes;
    Ok(())
  }
}

use crate::core::util::bit_util::BitUtil;
use crate::core::util::group_vint_util::GroupVIntUtil;

/// Reads a float in a variable-length format. Reads between one and five
/// bytes. Small integral values typically take fewer bytes.
pub fn read_zfloat(input: &mut impl DataInput) -> Result<f32> {
  let b = input.read_byte()? as i32;
  if b == 0xFF {
    // negative value
    let bits = input.read_int()? as u32;
    Ok(f32::from_bits(bits))
  } else if (b & 0x80) != 0 {
    // small integer [-1..125]
    Ok(((b & 0x7F) - 1) as f32)
  } else {
    // positive float
    let high = b << 24;
    let mid = (input.read_short()? as u16 as i32) << 8;
    let low = input.read_byte()? as i32;
    let bits = high | mid | low;
    Ok(f32::from_bits(bits as u32))
  }
}
/// Reads a double in a variable-length format. Reads between one and nine
/// bytes. Small integral values typically take fewer bytes.
pub fn read_zdouble(input: &mut impl DataInput) -> Result<f64> {
  let b = input.read_byte()? as i32;
  if b == 0xFF {
    // negative value (full i64 bits)
    let bits = input.read_long()? as u64;
    Ok(f64::from_bits(bits))
  } else if b == 0xFE {
    // float encoded as f32
    let bits = input.read_int()? as u32;
    Ok(f32::from_bits(bits) as f64)
  } else if (b & 0x80) != 0 {
    // small integer [-1..124]
    Ok(((b & 0x7F) - 1) as f64)
  } else {
    // positive double
    let high = (b as u64) << 56;
    let mid1 = (input.read_int()? as u32 as u64) << 24;
    let mid2 = (input.read_short()? as u16 as u64) << 8;
    let low = input.read_byte()? as u64;
    let bits = high | mid1 | mid2 | low;
    Ok(f64::from_bits(bits))
  }
}
/// Reads a long in a variable-length format. Reads between one and nine
/// bytes. Small values typically take fewer bytes.
pub fn read_tlong(input: &mut impl DataInput) -> Result<i64> {
  let header = input.read_byte()? as i32;

  let mut bits = (header & 0x1F) as i64;
  if (header & 0x20) != 0 {
    // continuation bit is set
    bits |= input.read_vlong()? << 5;
  }

  let mut l = BitUtil::zig_zag_decode_i64(bits as u64);

  match header & DAY_ENCODING {
    SECOND_ENCODING => l = l.wrapping_mul(SECOND),
    HOUR_ENCODING => l = l.wrapping_mul(HOUR),
    DAY_ENCODING => l = l.wrapping_mul(DAY),
    0 => {},
    _ => {
      debug_assert!(false, "should not be here");
      return Err(LuceneError::unreachable("invalid tlong encoding"));
    },
  }

  Ok(l)
}
