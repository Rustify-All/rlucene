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
use crate::core::codecs::compressing::lucene90_compressing_stored_fields_format::Lucene90CompressingStoredFieldsFormat;
use crate::core::codecs::compression::compression_mode::{
  CompressionModeBase, CompressionModeEnum, CompressorEnum, DecompressorEnum,
};
use crate::core::codecs::compression::compressor::Compressor;
use crate::core::codecs::compression::decompressor::Decompressor;
use crate::core::codecs::dummy::stored_fields_writer::DummyStoredFieldsWriter;
use crate::core::codecs::stored_fields_format::StoredFieldsFormat;
use crate::core::codecs::stored_fields_reader::StoredFieldsReader;
use crate::core::codecs::stored_fields_writer::{DefaultStoredFieldsWriter, StoredFieldsWriter};
use crate::core::codecs::{Codec, Codecs};
use crate::core::index::BytesRef;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorter::DocMap;
use crate::core::index::stored_field_visitor::{Status, StoredFieldVisitor};
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::stored_fields_consumer::{
  StoredFieldsConsumerBase, StoredFieldsConsumerDefaults,
};
use crate::core::index::tracking_tmp_output_directory_wrapper::TrackingTmpOutputDirectoryWrapper;
use crate::core::store::byte_buffers_data_input::ByteBuffersDataInput;
use crate::core::store::directory::Directory;
use crate::core::store::{DataInput, DataOutput, IOContext};
use crate::core::util::IOUtils;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::Result;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub(crate) struct SortingStoredFieldsConsumer<D>
where
  D: Directory,
{
  pub(crate) writer: Option<DefaultStoredFieldsWriter<TrackingTmpOutputDirectoryWrapper<D>>>,
  pub(crate) tmp_directory: TrackingTmpOutputDirectoryWrapper<D>,
  stored_fields_format: Lucene90CompressingStoredFieldsFormat,
}
impl<D> SortingStoredFieldsConsumer<D>
where
  D: Directory + Clone,
{
  pub(crate) fn new(dir: D) -> Result<Self> {
    let stored_fields_format = Lucene90CompressingStoredFieldsFormat::new(
      "TempStoredFields",
      CompressionModeEnum::Impl(NoCompression),
      128 * 1024,
      1,
      10,
    )?;
    let tmp_directory = TrackingTmpOutputDirectoryWrapper::new(dir);
    Ok(Self {
      writer: None,
      tmp_directory,
      stored_fields_format,
    })
  }
}

impl<D> StoredFieldsConsumerBase for SortingStoredFieldsConsumer<D>
where
  D: Directory + Clone,
{
  type Directory = D;

  fn init_stored_fields_writer<D1>(
    &mut self,
    _directory: &Self::Directory,
    _codec: &Codecs,
    info: &mut SegmentInfo<D1>,
  ) -> Result<()>
  where
    D1: Directory,
  {
    if self.writer.is_none() {
      self.writer = Some(self.stored_fields_format.fields_writer(
        self.tmp_directory.clone(),
        info,
        &IOContext::default_io_context()?,
      )?);
    }

    Ok(())
  }

  fn start_document(&mut self, last_doc: &mut i32, doc_id: i32) -> Result<()> {
    StoredFieldsConsumerDefaults::start_document(&mut self.writer, last_doc, doc_id)
  }

  fn finish_document(&mut self) -> Result<()> {
    StoredFieldsConsumerDefaults::finish_document(&mut self.writer)
  }

  fn flush<DM, D1>(
    &mut self,
    codec: &Codecs,
    state: &SegmentWriteState<Self::Directory>,
    sort_map: Option<&DM>,
    info: &mut SegmentInfo<D1>,
  ) -> Result<()>
  where
    DM: DocMap,
    D1: Directory,
  {
    StoredFieldsConsumerDefaults::flush(&mut self.writer, &self.tmp_directory, info)?;

    let mut reader = self.stored_fields_format.fields_reader(
      &self.tmp_directory,
      info,
      state.field_infos.clone(),
      &IOContext::default_io_context()?,
    )?;
    // Don't pull a merge instance, since merge instances optimize for
    // sequential access while we consume stored fields in random order here.
    let mut sort_writer =
      codec
        .stored_fields_format()
        .fields_writer(state.directory, info, state.context)?;

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      reader.check_integrity()?;
      let mut visitor = CopyVisitor::new(&mut sort_writer);
      let max_doc = info.max_doc()?;
      for doc_id in 0..max_doc {
        visitor.writer.start_document()?;
        let mapped_doc = if let Some(sort_map) = sort_map {
          sort_map.new_to_old(doc_id)?
        } else {
          doc_id
        };
        reader.document_with_visitor(
          mapped_doc,
          &mut visitor,
          None::<&mut DummyStoredFieldsWriter>,
        )?;
        visitor.writer.finish_document()?;
      }

      sort_writer.finish(max_doc, state.directory)?;
      Ok(())
    }));

    let finally_result: Result<()> = (|| {
      let reader_close_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reader.close()));
      let writer_close_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sort_writer.close()));
      let close_result = match reader_close_result {
        Ok(reader_close_result) => match writer_close_result {
          Ok(writer_close_result) => {
            IOUtils::use_or_suppress_result(reader_close_result, writer_close_result)
          },
          Err(payload) => match reader_close_result {
            Ok(()) => std::panic::resume_unwind(payload),
            Err(error) => Err(error),
          },
        },
        Err(payload) => std::panic::resume_unwind(payload),
      };
      close_result?;

      let file_names: Vec<String> = self
        .tmp_directory
        .get_temporary_files()
        .lock()
        .file_names
        .values()
        .cloned()
        .collect();
      IOUtils::delete_files(&self.tmp_directory, &file_names)?;
      Ok(())
    })();
    finally_result?;
    match result {
      Ok(result) => result,
      Err(payload) => std::panic::resume_unwind(payload),
    }
  }

  fn abort(&mut self) -> Result<()> {
    let abort_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      StoredFieldsConsumerDefaults::abort(&mut self.writer)
    }));
    let file_names: Vec<String> = self
      .tmp_directory
      .get_temporary_files()
      .lock()
      .file_names
      .values()
      .cloned()
      .collect();
    IOUtils::delete_files_ignoring_exceptions(&self.tmp_directory, &file_names);
    match abort_result {
      Ok(result) => result,
      Err(payload) => std::panic::resume_unwind(payload),
    }
  }
}

/// A visitor that copies every field it sees in the provided [`StoredFieldsWriter`]
pub(crate) struct CopyVisitor<'a, S>
where
  S: StoredFieldsWriter,
{
  pub(crate) writer: &'a mut S,
}
impl<'a, S> CopyVisitor<'a, S>
where
  S: StoredFieldsWriter,
{
  pub fn new(writer: &'a mut S) -> Self {
    Self { writer }
  }
}
impl<S> StoredFieldVisitor for CopyVisitor<'_, S>
where
  S: StoredFieldsWriter,
{
  fn binary_field_with_input<S1>(
    &mut self,
    field_info: Arc<FieldInfo>,
    input: &mut impl DataInput,
    length: i32,
    _writer: Option<&mut S1>,
  ) -> Result<()>
  where
    S1: StoredFieldsWriter,
  {
    self
      .writer
      .write_field_with_input(&field_info, input, length)
  }

  fn binary_field<S1>(
    &mut self,
    field_info: Arc<FieldInfo>,
    value: Vec<u8>,
    _writer: Option<&mut S1>,
  ) -> Result<()>
  where
    S1: StoredFieldsWriter,
  {
    self
      .writer
      .write_field_bytes(&field_info, &BytesRef::from_bytes(value))
  }

  fn string_field<S1>(
    &mut self,
    field_info: Arc<FieldInfo>,
    value: String,
    _writer: Option<&mut S1>,
  ) -> Result<()>
  where
    S1: StoredFieldsWriter,
  {
    self.writer.write_field_str(&field_info, &value)
  }

  fn int_field<S1>(
    &mut self,
    field_info: Arc<FieldInfo>,
    value: i32,
    _writer: Option<&mut S1>,
  ) -> Result<()>
  where
    S1: StoredFieldsWriter,
  {
    self.writer.write_field_i32(&field_info, value)
  }

  fn long_field<S1>(
    &mut self,
    field_info: Arc<FieldInfo>,
    value: i64,
    _writer: Option<&mut S1>,
  ) -> Result<()>
  where
    S1: StoredFieldsWriter,
  {
    self.writer.write_field_i64(&field_info, value)
  }

  fn float_field<S1>(
    &mut self,
    field_info: Arc<FieldInfo>,
    value: f32,
    _writer: Option<&mut S1>,
  ) -> Result<()>
  where
    S1: StoredFieldsWriter,
  {
    self.writer.write_field_f32(&field_info, value)
  }

  fn double_field<S1>(
    &mut self,
    field_info: Arc<FieldInfo>,
    value: f64,
    _writer: Option<&mut S1>,
  ) -> Result<()>
  where
    S1: StoredFieldsWriter,
  {
    self.writer.write_field_f64(&field_info, value)
  }

  fn needs_field<S1>(
    &mut self,
    _field_info: Arc<FieldInfo>,
    _writer: Option<&mut S1>,
  ) -> Result<Status>
  where
    S1: StoredFieldsWriter,
  {
    Ok(Status::Yes)
  }
}
#[derive(Debug)]
pub struct NoCompression;

impl Display for NoCompression {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl Clone for NoCompression {
  fn clone(&self) -> Self {
    NoCompression
  }
}

impl CompressionModeBase for NoCompression {
  fn new_compressor(&self) -> CompressorEnum {
    CompressorEnum::Impl1(CompressorImpl)
  }

  fn new_decompressor(&self) -> DecompressorEnum {
    DecompressorEnum::Impl1(DecompressorImpl)
  }
}

pub struct CompressorImpl;

impl Compressor for CompressorImpl {
  fn compress(
    &mut self,
    buffers_input: &mut ByteBuffersDataInput<&[u8]>,
    out: &mut impl DataOutput,
  ) -> Result<()> {
    let len = buffers_input.length();
    out.copy_bytes(buffers_input, len)
  }
}

impl Closeable for CompressorImpl {}

pub struct DecompressorImpl;

impl Clone for DecompressorImpl {
  fn clone(&self) -> Self {
    DecompressorImpl
  }
}

impl Decompressor for DecompressorImpl {
  fn decompress(
    &mut self,
    input: &mut impl DataInput,
    _original_length: i32,
    offset: i32,
    length: i32,
    bytes: &mut BytesRef<Vec<u8>>,
  ) -> Result<()> {
    ArrayUtil::grow_no_copy(&mut bytes.bytes, length as usize)?;
    input.skip_bytes(offset as i64)?;
    input.read_bytes(&mut bytes.bytes, 0, length as usize)?;
    bytes.offset = 0;
    bytes.length = length as usize;
    Ok(())
  }
}
