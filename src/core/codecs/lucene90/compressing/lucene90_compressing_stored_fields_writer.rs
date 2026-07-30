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
use std::env;
use std::mem::discriminant;
use std::rc::Rc;

use std::sync::LazyLock;

use crate::core::codecs::CodecUtil;
use crate::core::codecs::compressing::lucene90_compressing_stored_fields_reader::Lucene90CompressingStoredFieldsReader;
use crate::core::codecs::compressing::stored_fields_ints::StoredFieldsInts;
use crate::core::codecs::compression::compression_mode::{
  CompressionModeBase, CompressionModeEnum, CompressorEnum,
};
use crate::core::codecs::compression::compressor::Compressor;
use crate::core::codecs::compression::matching_readers::MatchingReaders;
use crate::core::codecs::lucene90::fields_index::FieldsIndex;
use crate::core::codecs::lucene90::fields_index_writer::FieldsIndexWriter;
use crate::core::codecs::stored_fields_reader::StoredFieldsReader;
use crate::core::codecs::stored_fields_writer::{MergeVisitor, StoredFieldsWriter};
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::merge_state::{DocMap, MergeState, MergeStateDocMap};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::stored_fields::{RawStoredFieldsReader, StoredFields};
use crate::core::index::{BytesRef, DocIDMerger, IndexFileNames, Sub, SubBase, of};
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::store::directory::Directory;
use crate::core::store::{
  ByteBuffersDataOutput, DataInput, DataOutput, IOContext, IndexInput, IndexOutput,
};
use crate::core::util::accountable::Accountable;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::io_utils::IOUtils;
use crate::core::util::packed::PackedInts;
use crate::core::util::ram_usage_estimator::size_of_vec;

/// [`StoredFieldsWriter`] implementation for
/// [`Lucene90CompressingStoredFieldsFormat`](crate::core::codecs::lucene90::compressing::lucene90_compressing_stored_fields_format::Lucene90CompressingStoredFieldsFormat).
pub(crate) static TYPE_BITS: LazyLock<i32> =
  LazyLock::new(|| PackedInts::bits_required(NUMERIC_DOUBLE as i64).unwrap());

pub(crate) static TYPE_MASK: LazyLock<i64> = LazyLock::new(|| PackedInts::max_value(*TYPE_BITS));
pub struct Lucene90CompressingStoredFieldsWriter<D>
where
  D: Directory,
{
  segment: String,
  index_writer: FieldsIndexWriter<D>,
  meta_stream: D::IndexOutput,
  fields_stream: D::IndexOutput,
  compressor: CompressorEnum,
  closed: bool,
  compression_mode: CompressionModeEnum,
  chunk_size: i32,
  max_docs_per_chunk: i32,
  buffered_docs: ByteBuffersDataOutput,
  num_stored_fields: Vec<i32>,
  end_offsets: Vec<i32>,
  doc_base: usize,
  num_buffered_docs: i32,
  num_chunks: i64,
  num_dirty_chunks: i64,
  num_dirty_docs: i64,
  num_stored_fields_in_doc: i32,
}
impl<D> Lucene90CompressingStoredFieldsWriter<D>
where
  D: Directory,
{
  #[allow(clippy::too_many_arguments)]
  pub fn new<D2>(
    directory: D,
    si: &SegmentInfo<D2>,
    segment_suffix: &str,
    context: &IOContext,
    format_name: &str,
    compression_mode: CompressionModeEnum,
    chunk_size: i32,
    max_docs_per_chunk: i32,
    block_shift: i32,
  ) -> Result<Self>
  where
    D2: Directory,
  {
    let segment = si.name.clone();
    let compressor = compression_mode.new_compressor();
    let buffered_docs = ByteBuffersDataOutput::new_resettable_instance();
    let num_stored_fields = vec![0; 16];
    let end_offsets = vec![0; 16];

    let meta_file = IndexFileNames::segment_file_name(&segment, segment_suffix, META_EXTENSION);
    let mut directory = Some(directory);
    let mut meta_stream = None;
    let mut fields_stream = None;
    let mut index_writer = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      let dir = directory
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("directory is missing"))?;
      meta_stream = Some(dir.create_output(&meta_file, context)?);
      let meta = meta_stream
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("metadata output is missing"))?;
      CodecUtil::write_index_header(
        meta,
        &format!("{}Meta", INDEX_CODEC_NAME),
        VERSION_CURRENT,
        si.get_id(),
        segment_suffix,
      )?;
      debug_assert_eq!(
        CodecUtil::index_header_length(&format!("{}Meta", INDEX_CODEC_NAME), segment_suffix),
        meta.get_file_pointer()?
      );

      let fields_file =
        IndexFileNames::segment_file_name(&segment, segment_suffix, FIELDS_EXTENSION);
      fields_stream = Some(dir.create_output(&fields_file, context)?);
      let fields = fields_stream
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("stored fields output is missing"))?;
      CodecUtil::write_index_header(
        fields,
        format_name,
        VERSION_CURRENT,
        si.get_id(),
        segment_suffix,
      )?;
      debug_assert_eq!(
        CodecUtil::index_header_length(format_name, segment_suffix),
        fields.get_file_pointer()?
      );

      index_writer = Some(FieldsIndexWriter::new(
        directory
          .take()
          .ok_or_else(|| LuceneError::illegal_state("directory is missing"))?,
        &segment,
        segment_suffix,
        INDEX_EXTENSION,
        INDEX_CODEC_NAME,
        *si.get_id(),
        block_shift,
        context.clone(),
      )?);

      meta_stream
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("metadata output is missing"))?
        .write_vint(chunk_size)
    }));
    match result {
      Ok(Ok(())) => {},
      result => {
        IOUtils::close_resources_while_handling_error((
          meta_stream.as_mut(),
          fields_stream.as_mut(),
          index_writer.as_mut(),
        ))?;
        return match result {
          Ok(Err(error)) => Err(error),
          Err(payload) => std::panic::resume_unwind(payload),
          Ok(Ok(())) => Err(LuceneError::illegal_state(
            "stored fields writer initialization entered failure handling after success",
          )),
        };
      },
    }
    let meta_stream =
      meta_stream.ok_or_else(|| LuceneError::illegal_state("metadata output is missing"))?;
    let fields_stream =
      fields_stream.ok_or_else(|| LuceneError::illegal_state("stored fields output is missing"))?;
    let index_writer =
      index_writer.ok_or_else(|| LuceneError::illegal_state("fields index writer is missing"))?;

    Ok(Self {
      segment,
      compression_mode,
      compressor,
      closed: false,
      chunk_size,
      max_docs_per_chunk,
      buffered_docs,
      num_stored_fields,
      end_offsets,
      doc_base: 0,
      num_buffered_docs: 0,
      num_chunks: 0,
      num_dirty_chunks: 0,
      num_dirty_docs: 0,
      meta_stream,
      fields_stream,
      index_writer,
      num_stored_fields_in_doc: 0,
    })
  }

  fn save_ints(values: &[i32], length: i32, out: &mut impl DataOutput) -> Result<()> {
    if length == 1 {
      out.write_vint(values[0])?;
    } else {
      StoredFieldsInts::write_ints(values, 0, length, out)?;
    }
    Ok(())
  }
  fn write_header(&mut self, sliced: bool, dirty_chunk: bool) -> Result<()> {
    let sliced_bit = if sliced { 1 } else { 0 };
    let dirty_bit = if dirty_chunk { 2 } else { 0 };

    self.fields_stream.write_vint(self.doc_base as i32)?;
    self
      .fields_stream
      .write_vint((self.num_buffered_docs << 2) | dirty_bit | sliced_bit)?;
    // save numStoredFields
    Self::save_ints(
      &self.num_stored_fields,
      self.num_buffered_docs,
      &mut self.fields_stream,
    )?;
    // save lengths
    Self::save_ints(
      &self.end_offsets,
      self.num_buffered_docs,
      &mut self.fields_stream,
    )?;
    Ok(())
  }
  fn trigger_flush(&self) -> bool {
    self.buffered_docs.size() as i32 >= self.chunk_size
      || self.num_buffered_docs >= self.max_docs_per_chunk
  }
  fn flush(&mut self, force: bool) -> Result<()> {
    debug_assert!(self.trigger_flush() != force);

    self.num_chunks += 1;
    if force {
      self.num_dirty_chunks += 1;
      self.num_dirty_docs += self.num_buffered_docs as i64;
    }

    self.index_writer.write_index(
      self.num_buffered_docs,
      self.fields_stream.get_file_pointer()?,
    )?;

    // convert end offsets into lengths
    for i in (1..self.num_buffered_docs as usize).rev() {
      self.end_offsets[i] -= self.end_offsets[i - 1];
      debug_assert!(self.end_offsets[i] >= 0);
    }

    let sliced = self.buffered_docs.size() >= 2 * self.chunk_size as usize;
    let dirty_chunk = force;

    self.write_header(sliced, dirty_chunk)?;
    let mut byte_buffers = self.buffered_docs.get_data_input_ref()?;
    // compress stored fields to fieldsStream.
    if sliced {
      let capacity = byte_buffers.length();
      let mut compressed = 0;
      while compressed < capacity {
        let len = std::cmp::min(self.chunk_size as usize, capacity - compressed);
        let mut bbdi = byte_buffers.slice(compressed, len)?;
        self
          .compressor
          .compress(&mut bbdi, &mut self.fields_stream)?;
        compressed += len;
      }
    } else {
      self
        .compressor
        .compress(&mut byte_buffers, &mut self.fields_stream)?;
    }

    // reset
    self.doc_base += self.num_buffered_docs as usize;
    self.num_buffered_docs = 0;
    self.buffered_docs.reset();

    Ok(())
  }

  fn copy_one_doc<I>(
    &mut self,
    reader: &mut Lucene90CompressingStoredFieldsReader<I>,
    doc_id: i32,
  ) -> Result<()>
  where
    I: IndexInput,
  {
    debug_assert_eq!(reader.get_version(), VERSION_CURRENT);

    let mut doc = reader.serialized_document(doc_id)?;

    self.start_document()?;

    self
      .buffered_docs
      .copy_bytes(&mut doc.input, doc.length as usize)?;

    self.num_stored_fields_in_doc = doc.num_stored_fields;
    self.finish_document()?;

    Ok(())
  }
  fn copy_chunks<MD, CR>(
    &mut self,
    merge_state: &mut MergeState<MD, CR>,
    sub: &CompressingStoredFieldsMergeSub<MergeStateDocMap<CR>>,
    from_doc_id: i32,
    to_doc_id: i32,
  ) -> Result<()>
  where
    MD: Directory,
    CR: CodecReader,
  {
    let reader_wrap = match merge_state.stored_fields_readers[sub.reader_index] {
      Some(ref mut r) => r,
      _ => {
        return Err(LuceneError::illegal_state(
          "Expected Lucene90CompressingStoredFieldsReader",
        ));
      },
    };
    let reader = reader_wrap.raw_stored_fields_mut()?;
    debug_assert_eq!(reader.get_version(), VERSION_CURRENT);
    debug_assert_eq!(reader.get_chunk_size(), self.chunk_size);
    debug_assert!(
      discriminant(reader.get_compression_mode()) == discriminant(&self.compression_mode)
    );
    debug_assert!(!self.too_dirty(reader)?);
    debug_assert!(merge_state.live_docs[sub.reader_index].is_none());

    let mut doc_id = from_doc_id;
    let max_pointer = reader.get_max_pointer();

    // copy docs that belong to the previous chunk
    while doc_id < to_doc_id && reader.is_loaded(doc_id)? {
      self.copy_one_doc(reader, doc_id)?;
      doc_id += 1;
    }

    if doc_id >= to_doc_id {
      return Ok(());
    }

    let (mut from_pointer, to_pointer) = {
      let index = reader.get_index_reader();
      let from_pointer = index.get_start_pointer(doc_id)?;
      let to_pointer = if to_doc_id == sub.max_doc {
        max_pointer
      } else {
        index.get_start_pointer(to_doc_id)?
      };
      (from_pointer, to_pointer)
    };

    if from_pointer < to_pointer {
      if self.num_buffered_docs > 0 {
        self.flush(true)?;
      }
      {
        let raw_docs = reader.get_fields_stream();
        raw_docs.seek(from_pointer)?;
      }

      while from_pointer < to_pointer {
        let (code, buffered_docs) = {
          let raw_docs = reader.get_fields_stream();
          let base = raw_docs.read_vint()?;
          let code = raw_docs.read_vint()?;
          let buffered_docs = ((code as u32) >> 2) as i32;

          if base != doc_id {
            return Err(LuceneError::corrupt_index(format!(
              "invalid state: base={base} != docID={doc_id} (resource {raw_docs})"
            )));
          }
          // write a new index entry and new header for this chunk.
          self
            .index_writer
            .write_index(buffered_docs, self.fields_stream.get_file_pointer()?)?;
          self.fields_stream.write_vint(self.doc_base as i32)?;
          self.fields_stream.write_vint(code)?;
          doc_id += buffered_docs;
          self.doc_base += buffered_docs.try_convert()?;
          if doc_id > to_doc_id {
            return Err(LuceneError::corrupt_index(format!(
              "invalid state: base={base}, count={buffered_docs}, toDocID={to_doc_id} (resource {raw_docs})"
            )));
          }
          (code, buffered_docs)
        };
        // copy bytes until the next chunk boundary (or end of chunk
        // data). using the stored fields index for this
        // isn't the most efficient, but fast enough and
        // is a source of redundancy for detecting bad things.
        let end_chunk_pointer = if doc_id == sub.max_doc {
          max_pointer
        } else {
          reader.get_index_reader().get_start_pointer(doc_id)?
        };

        let raw_docs = reader.get_fields_stream();
        let num_bytes = end_chunk_pointer - raw_docs.get_file_pointer()?;
        self.fields_stream.copy_bytes(&mut *raw_docs, num_bytes)?;

        self.num_chunks += 1;

        if code & 2 != 0 {
          debug_assert!(buffered_docs < self.max_docs_per_chunk);
          self.num_dirty_chunks += 1;
          self.num_dirty_docs += buffered_docs as i64;
        }
        from_pointer = end_chunk_pointer;
      }
    }

    // copy leftover docs that don't form a complete chunk
    debug_assert!(!reader.is_loaded(doc_id)?);
    while doc_id < to_doc_id {
      self.copy_one_doc(reader, doc_id)?;
      doc_id += 1;
    }
    Ok(())
  }
  /// Returns `true` if we should recompress this reader, even though we could
  /// bulk merge compressed data.
  ///
  /// The last chunk written for a segment is typically incomplete, so without
  /// recompressing, in some worst-case situations (e.g. frequent reopen
  /// with tiny flushes), over time the compression ratio can degrade.
  /// This is a safety switch.
  fn too_dirty<I>(&self, candidate: &Lucene90CompressingStoredFieldsReader<I>) -> Result<bool>
  where
    I: IndexInput,
  {
    // A segment is considered dirty only if it has enough dirty docs to
    // make a full block AND more than 1% blocks are dirty.
    Ok(
      candidate.get_num_dirty_docs()? > self.max_docs_per_chunk as i64
        && candidate.get_num_dirty_chunks()? * 100 > candidate.get_num_chunks()?,
    )
  }
  fn get_merge_strategy<MD, CR>(
    &self,
    merge_state: &MergeState<MD, CR>,
    matching_readers: &MatchingReaders,
    reader_index: usize,
  ) -> Result<MergeStrategy>
  where
    MD: Directory,
    CR: CodecReader,
  {
    let candidate = match merge_state.stored_fields_readers[reader_index] {
      Some(ref r) => r,
      _ => {
        return Err(LuceneError::illegal_state(
          "Expected Lucene90CompressingStoredFieldsReader",
        ));
      },
    };
    if !matching_readers.matching_readers[reader_index] {
      return Ok(MergeStrategy::Visitor);
    }

    let reader = match candidate.raw_stored_fields() {
      Ok(reader) => reader,
      Err(LuceneError::UnsupportedOperation(_)) => return Ok(MergeStrategy::Visitor),
      Err(err) => return Err(err),
    };
    // Currently we only allow to handle the same version.
    if reader.get_version() != VERSION_CURRENT {
      return Ok(MergeStrategy::Visitor);
    }
    if *BULK_MERGE_ENABLED
      && discriminant(reader.get_compression_mode()) == discriminant(&self.compression_mode)
      && reader.get_chunk_size() == self.chunk_size
      && merge_state.live_docs[reader_index].is_none()
      && !self.too_dirty(reader)?
    {
      Ok(MergeStrategy::Bulk)
    } else {
      Ok(MergeStrategy::Doc)
    }
  }
}
pub static BULK_MERGE_ENABLED_SYSPROP: &str =
  "lucene90.compressing.stored.fields.writer.enableBulkMerge";

pub static BULK_MERGE_ENABLED: LazyLock<bool> = LazyLock::new(|| {
  env::var(BULK_MERGE_ENABLED_SYSPROP)
    .ok()
    .map(|v| v.parse::<bool>().unwrap_or(true))
    .unwrap_or(true)
});
impl<D> Closeable for Lucene90CompressingStoredFieldsWriter<D>
where
  D: Directory,
{
  fn close(&mut self) -> Result<()> {
    if self.closed {
      return Ok(());
    }

    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      IOUtils::close(0..4, |operation| match operation {
        0 => self.meta_stream.close(),
        1 => self.fields_stream.close(),
        2 => self.index_writer.close(),
        3 => self.compressor.close(),
        _ => unreachable!(),
      })
    }));
    self.closed = true;
    match close_result {
      Ok(result) => result,
      Err(payload) => std::panic::resume_unwind(payload),
    }
  }
}

impl<D> StoredFieldsWriter for Lucene90CompressingStoredFieldsWriter<D>
where
  D: Directory,
{
  fn start_document(&mut self) -> Result<()> {
    Ok(())
  }
  fn finish_document(&mut self) -> Result<()> {
    if self.num_buffered_docs as usize == self.num_stored_fields.len() {
      let new_len = ArrayUtil::oversize(self.num_buffered_docs as usize + 1, 4)?;
      ArrayUtil::grow_exact(&mut self.num_stored_fields, new_len)?;
      ArrayUtil::grow_exact(&mut self.end_offsets, new_len)?;
    }

    self.num_stored_fields[self.num_buffered_docs as usize] = self.num_stored_fields_in_doc;
    self.num_stored_fields_in_doc = 0;
    self.end_offsets[self.num_buffered_docs as usize] = self.buffered_docs.size().try_convert()?;
    self.num_buffered_docs += 1;

    if self.trigger_flush() {
      self.flush(false)?;
    }

    Ok(())
  }

  fn write_field_i32(&mut self, info: &FieldInfo, value: i32) -> Result<()> {
    self.num_stored_fields_in_doc += 1;
    let info_and_bits = ((info.number as i64) << *TYPE_BITS) | NUMERIC_INT as i64;
    self.buffered_docs.write_vlong(info_and_bits)?;
    self.buffered_docs.write_zint(value)?;
    Ok(())
  }

  fn write_field_i64(&mut self, info: &FieldInfo, value: i64) -> Result<()> {
    self.num_stored_fields_in_doc += 1;
    let info_and_bits = ((info.number as i64) << *TYPE_BITS) | NUMERIC_LONG as i64;
    self.buffered_docs.write_vlong(info_and_bits)?;
    write_tlong(&mut self.buffered_docs, value)?;
    Ok(())
  }

  fn write_field_f32(&mut self, info: &FieldInfo, value: f32) -> Result<()> {
    self.num_stored_fields_in_doc += 1;
    let info_and_bits = ((info.number as i64) << *TYPE_BITS) | NUMERIC_FLOAT as i64;
    self.buffered_docs.write_vlong(info_and_bits)?;
    write_zfloat(&mut self.buffered_docs, value)?;
    Ok(())
  }

  fn write_field_f64(&mut self, info: &FieldInfo, value: f64) -> Result<()> {
    self.num_stored_fields_in_doc += 1;
    let info_and_bits = ((info.number as i64) << *TYPE_BITS) | NUMERIC_DOUBLE as i64;
    self.buffered_docs.write_vlong(info_and_bits)?;
    write_zdouble(&mut self.buffered_docs, value)?;
    Ok(())
  }
  fn write_field_with_input(
    &mut self,
    info: &FieldInfo,
    value: &mut impl DataInput,
    length: i32,
  ) -> Result<()> {
    self.num_stored_fields_in_doc += 1;
    let info_and_bits = ((info.number as i64) << *TYPE_BITS) | BYTE_ARR as i64;
    self.buffered_docs.write_vlong(info_and_bits)?;
    self.buffered_docs.write_vint(length)?;
    self.buffered_docs.copy_bytes(value, length as usize)?;
    Ok(())
  }

  fn write_field_bytes(&mut self, info: &FieldInfo, value: &BytesRef<Vec<u8>>) -> Result<()> {
    self.num_stored_fields_in_doc += 1;
    let info_and_bits = ((info.number as i64) << *TYPE_BITS) | BYTE_ARR as i64;
    self.buffered_docs.write_vlong(info_and_bits)?;
    self.buffered_docs.write_vint(value.length as i32)?;
    self
      .buffered_docs
      .write_bytes_range(&value.bytes, value.offset, value.length)?;
    Ok(())
  }

  fn write_field_str(&mut self, info: &FieldInfo, value: &str) -> Result<()> {
    self.num_stored_fields_in_doc += 1;
    let info_and_bits = ((info.number as i64) << *TYPE_BITS) | STRING as i64;
    self.buffered_docs.write_vlong(info_and_bits)?;
    self.buffered_docs.write_string(value)?;
    Ok(())
  }
  fn finish<D1>(&mut self, num_docs: i32, _dir: &D1) -> Result<()>
  where
    D1: Directory,
  {
    if self.num_buffered_docs > 0 {
      self.flush(true)?;
    } else {
      debug_assert_eq!(self.buffered_docs.size(), 0);
    }

    if self.doc_base != num_docs as usize {
      return Err(LuceneError::illegal_state(format!(
        "Wrote {} docs, finish called with numDocs={}",
        self.doc_base, num_docs
      )));
    }

    self.index_writer.finish(
      num_docs,
      self.fields_stream.get_file_pointer()?,
      &mut self.meta_stream,
    )?;

    self.meta_stream.write_vlong(self.num_chunks)?;
    self.meta_stream.write_vlong(self.num_dirty_chunks)?;
    self.meta_stream.write_vlong(self.num_dirty_docs)?;

    CodecUtil::write_footer(&mut self.meta_stream)?;
    CodecUtil::write_footer(&mut self.fields_stream)?;

    debug_assert_eq!(self.buffered_docs.size(), 0);

    Ok(())
  }

  fn merge<MD, D1, CR>(&mut self, merge_state: &mut MergeState<MD, CR>, dir: &D1) -> Result<i32>
  where
    MD: Directory,
    D1: Directory,
    CR: CodecReader,
    Self: Sized,
  {
    let matching_readers = MatchingReaders::new(merge_state)?;
    let mut visitors: Vec<Option<MergeVisitor>> =
      vec![None; merge_state.stored_fields_readers.len()];
    let mut subs = Vec::with_capacity(merge_state.stored_fields_readers.len());

    for (i, reader) in merge_state.stored_fields_readers.iter().enumerate() {
      reader.as_ref().unwrap().check_integrity()?;
      let strategy = self.get_merge_strategy(merge_state, &matching_readers, i)?;
      if strategy == MergeStrategy::Visitor {
        visitors[i] = Some(MergeVisitor::new(merge_state, i)?);
      }
      subs.push(Sub::new(CompressingStoredFieldsMergeSub::new(
        merge_state.doc_maps[i].clone(),
        merge_state.max_docs[i],
        strategy,
        i,
      )));
    }

    let mut doc_id_merger = of(subs, merge_state.needs_index_sort)?;
    let mut doc_count = 0;
    let mut sub_opt = doc_id_merger.next()?;
    while let Some(sub_idx) = sub_opt {
      let sub = &doc_id_merger.get_subs()[sub_idx];
      debug_assert_eq!(sub.mapped_doc_id, doc_count);
      match sub.sub.merge_strategy {
        MergeStrategy::Bulk => {
          let from_doc = sub.sub.doc_id;
          let mut to_doc_id = from_doc;
          let current = sub_idx;

          loop {
            sub_opt = doc_id_merger.next()?;
            if sub_opt.is_none() || sub_opt.unwrap() != current {
              break;
            }
            to_doc_id += 1;
            debug_assert!(doc_id_merger.get_subs()[sub_opt.unwrap()].sub.doc_id == to_doc_id)
          }
          to_doc_id += 1; // exclusive bound
          self.copy_chunks(
            merge_state,
            &doc_id_merger.get_subs()[current].sub,
            from_doc,
            to_doc_id,
          )?;
          doc_count += to_doc_id - from_doc;
        },
        MergeStrategy::Doc => {
          let reader = match merge_state.stored_fields_readers[sub.sub.reader_index] {
            Some(ref mut reader) => reader,
            None => {
              return Err(LuceneError::illegal_state(
                "Expected Lucene90CompressingStoredFieldsReader",
              ));
            },
          };
          let raw_reader = reader.raw_stored_fields_mut()?;
          self.copy_one_doc(raw_reader, sub.sub.doc_id)?;
          doc_count += 1;
          sub_opt = doc_id_merger.next()?;
        },
        MergeStrategy::Visitor => {
          debug_assert!(visitors[sub.sub.reader_index].is_some());
          self.start_document()?;
          match visitors[sub.sub.reader_index] {
            Some(ref mut visitor) => {
              let reader = match merge_state.stored_fields_readers[sub.sub.reader_index] {
                Some(ref mut reader) => reader,
                None => {
                  return Err(LuceneError::illegal_state(
                    "StoredFieldsReader must exist for VISITOR strategy",
                  ));
                },
              };
              reader.document_with_visitor(sub.sub.doc_id, visitor, Some(self))?;
              self.finish_document()?;
              doc_count += 1;
            },
            None => {
              return Err(LuceneError::illegal_state(
                "Visitor must exist for VISITOR strategy".to_string(),
              ));
            },
          }
          sub_opt = doc_id_merger.next()?;
        },
      }
    }

    self.finish(doc_count, dir)?;
    Ok(doc_count)
  }
}

impl<D> Accountable for Lucene90CompressingStoredFieldsWriter<D>
where
  D: Directory,
{
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(
      self
        .buffered_docs
        .ram_bytes_used()?
        .saturating_add(size_of_vec(&self.num_stored_fields))
        .saturating_add(size_of_vec(&self.end_offsets)),
    )
  }
}
/// Merge strategy used during stored field merging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MergeStrategy {
  /// Copy chunk by chunk in a compressed format.
  Bulk,

  /// Copy document by document in a decompressed format.
  Doc,

  /// Copy field by field of decompressed documents.
  Visitor,
}
struct CompressingStoredFieldsMergeSub<DM>
where
  DM: DocMap,
{
  pub reader_index: usize,
  pub max_doc: i32,
  pub merge_strategy: MergeStrategy,
  pub doc_id: i32,
  pub doc_map: Rc<DM>,
}

impl<DM> CompressingStoredFieldsMergeSub<DM>
where
  DM: DocMap,
{
  fn new(
    doc_map: Rc<DM>,
    max_doc: i32,
    merge_strategy: MergeStrategy,
    reader_index: usize,
  ) -> Self {
    Self {
      reader_index,
      merge_strategy,
      max_doc,
      doc_id: -1,
      doc_map,
    }
  }
}

impl<DM> SubBase for CompressingStoredFieldsMergeSub<DM>
where
  DM: DocMap,
{
  fn next_doc(&mut self) -> Result<i32> {
    self.doc_id += 1;
    if self.doc_id == self.max_doc {
      Ok(NO_MORE_DOCS)
    } else {
      Ok(self.doc_id)
    }
  }

  type DocMap = DM;

  fn get_doc_map(&self) -> Result<&Self::DocMap> {
    Ok(&self.doc_map)
  }
}

use crate::core::util::TryIntoInt;
use crate::core::util::bit_util::BitUtil;

/// Extension of stored fields file
pub(crate) const FIELDS_EXTENSION: &str = "fdt";
/// Extension of stored fields index
pub(crate) const INDEX_EXTENSION: &str = "fdx";
/// Extension of stored fields meta
pub(crate) const META_EXTENSION: &str = "fdm";
/// Codec name for the index
pub(crate) const INDEX_CODEC_NAME: &str = "Lucene90FieldsIndex";
pub(crate) const STRING: i32 = 0x00;
pub(crate) const BYTE_ARR: i32 = 0x01;
pub(crate) const NUMERIC_INT: i32 = 0x02;
pub(crate) const NUMERIC_FLOAT: i32 = 0x03;
pub(crate) const NUMERIC_LONG: i32 = 0x04;
pub(crate) const NUMERIC_DOUBLE: i32 = 0x05;

pub(crate) const VERSION_START: i32 = 1;
pub(crate) const VERSION_CURRENT: i32 = VERSION_START;
pub(crate) const META_VERSION_START: i32 = 0;

// -0 isn't compressed.
pub(crate) const NEGATIVE_ZERO_FLOAT: u32 = (-0f32).to_bits();
pub(crate) const NEGATIVE_ZERO_DOUBLE: u64 = (-0f64).to_bits();

// for compression of timestamps
pub(crate) const SECOND: i64 = 1_000;
pub(crate) const HOUR: i64 = 60 * 60 * SECOND;
pub(crate) const DAY: i64 = 24 * HOUR;

pub(crate) const SECOND_ENCODING: i32 = 0x40;
pub(crate) const HOUR_ENCODING: i32 = 0x80;
pub(crate) const DAY_ENCODING: i32 = 0xC0;

/// Writes a float in a variable-length format. Writes between one and five
/// bytes. Small integral values typically take fewer bytes.
///
/// ZFloat --> Header, Bytes*?
///
/// - Header -->
///   [`DataOutput::write_byte`](crate::core::store::data_output::DataOutput::write_byte)
///   (Uint8). When it is equal to `0xFF` then the value is negative and
///   stored in the next 4 bytes. Otherwise, if the first bit is set, then
///   the other bits in the header encode the value plus one and no other
///   bytes are read. Otherwise, the value is a positive float value whose
///   first byte is the header, and 3 bytes need to be read to complete it.
/// - Bytes --> Potential additional bytes to read depending on the header.
pub(crate) fn write_zfloat(out: &mut impl DataOutput, f: f32) -> Result<()> {
  let int_val = f as i32;
  let float_bits = f.to_bits();

  if f == int_val as f32 && (-1..=0x7D).contains(&int_val) && float_bits != NEGATIVE_ZERO_FLOAT {
    // small integer [-1..125]: single byte
    out.write_byte((0x80 | (1 + int_val)) as u8)?;
  } else if (float_bits >> 31) == 0 {
    // other positive floats: 4 bytes
    out.write_byte((float_bits >> 24) as u8)?;
    out.write_short((float_bits >> 8) as i16)?;
    out.write_byte(float_bits as u8)?;
  } else {
    // negative float or special: 5 bytes
    out.write_byte(0xFFu8)?;
    out.write_int(float_bits as i32)?;
  }
  Ok(())
}
/// Writes a float in a variable-length format. Writes between one and five
/// bytes. Small integral values typically take fewer bytes.
///
/// ZFloat --> Header, Bytes*?
///
/// - Header -->
///   [`DataOutput::write_byte`](crate::core::store::data_output::DataOutput::write_byte)
///   (Uint8). When it is equal to `0xFF` then the value is negative and
///   stored in the next 8 bytes. When it is equal to `0xFE` then the value
///   is stored as a float in the next 4 bytes. Otherwise if the first bit
///   is set then the other bits in the header encode the value plus one and
///   no other bytes are read. Otherwise, the value is a positive float
///   value whose first byte is the header, and 7 bytes need to be read to
///   complete it.
/// - Bytes --> Potential additional bytes to read depending on the header.
pub(crate) fn write_zdouble(out: &mut impl DataOutput, d: f64) -> Result<()> {
  let int_val = d as i32;
  let double_bits = d.to_bits(); // u64

  if d == int_val as f64 && (-1..=0x7C).contains(&int_val) && double_bits != NEGATIVE_ZERO_DOUBLE {
    // small integer value [-1..124]: single byte
    out.write_byte((0x80 | (int_val + 1)) as u8)?;
  } else if d == (d as f32) as f64 {
    // d has an accurate float representation: 5 bytes
    out.write_byte(0xFE)?;
    out.write_int((d as f32).to_bits() as i32)?;
  } else if (double_bits >> 63) == 0 {
    // other positive doubles: 8 bytes
    out.write_byte((double_bits >> 56) as u8)?;
    out.write_int((double_bits >> 24) as u32 as i32)?; // lower 32 bits as i32
    out.write_short((double_bits >> 8) as i16)?;
    out.write_byte(double_bits as u8)?;
  } else {
    // other negative doubles: 9 bytes
    out.write_byte(0xFF)?;
    out.write_long(double_bits as i64)?;
  }
  Ok(())
}
/// Writes a long in a variable-length format. Writes between one and ten
/// bytes. Small values or values representing timestamps with day, hour
/// or second precision typically require fewer bytes.
///
/// ZLong --> Header, Bytes*?
///
/// - Header --> The first two bits indicate the compression scheme:
///   - 00 - uncompressed
///   - 01 - multiple of 1000 (second)
///   - 10 - multiple of 3600000 (hour)
///   - 11 - multiple of 86400000 (day)
///
///   Then the next bit is a continuation bit, indicating whether more bytes
///   need to be read,   and the last 5 bits are the lower bits of the
///   encoded value. In order to reconstruct the   value, you need to
///   combine the 5 lower bits of the header with a vLong in the next bytes
///   (if the continuation bit is set to 1). Then
///   [`BitUtil::zig_zag_decode`](BitUtil::zig_zag_decode_i64) it and
///   finally multiply by the multiple corresponding to the compression
///   scheme.
///
/// - Bytes --> Potential additional bytes to read depending on the header.
// T for "timestamp"
pub(crate) fn write_tlong(out: &mut impl DataOutput, mut l: i64) -> Result<()> {
  let mut header;

  if l % SECOND != 0 {
    header = 0;
  } else if l % DAY == 0 {
    // timestamp with day precision
    header = DAY_ENCODING;
    l /= DAY;
  } else if l % HOUR == 0 {
    // timestamp with hour precision, or day precision with a timezone
    header = HOUR_ENCODING;
    l /= HOUR;
  } else {
    // timestamp with second precision
    header = SECOND_ENCODING;
    l /= SECOND;
  }

  let zigzag_l = BitUtil::zig_zag_encode_i64(l);
  header |= (zigzag_l & 0x1F) as i32; // last 5 bits

  let upper_bits = ((zigzag_l as u64) >> 5) as i64;
  if upper_bits != 0 {
    header |= 0x20;
  }

  out.write_byte(header as u8)?;

  if upper_bits != 0 {
    out.write_vlong(upper_bits)?;
  }
  Ok(())
}
