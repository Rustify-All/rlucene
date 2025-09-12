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
use crate::core::codecs::Codec;
use crate::core::codecs::compressing::lucene90_compressing_stored_fields_format::Lucene90CompressingStoredFieldsFormat;
use crate::core::codecs::compression::compression_mode::{
    CompressionModeBase, CompressionModeEnum, CompressorEnum, DecompressorEnum,
};
use crate::core::codecs::compression::compressor::Compressor;
use crate::core::codecs::compression::decompressor::Decompressor;
use crate::core::codecs::stored_fields_format::StoredFieldsFormat;
use crate::core::codecs::stored_fields_reader::StoredFieldsReader;
use crate::core::codecs::stored_fields_writer::{StoredFieldsWriter, StoredFieldsWriterEnum};
use crate::core::index::BytesRef;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorter::DocMap;
use crate::core::index::stored_field_visitor::{Status, StoredFieldVisitor};
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::stored_fields_consumer::StoredFieldsConsumerBase;
use crate::core::index::tracking_tmp_output_directory_wrapper::TrackingTmpOutputDirectoryWrapper;
use crate::core::store::byte_buffers_data_input::ByteBuffersDataInput;
use crate::core::store::directory::Directory;
use crate::core::store::random_access_input::RandomAccessInput;
use crate::core::store::{DataInput, DataOutput, IOContext};
use crate::core::util::IOUtils;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::error::lucene_error::Result;
use std::fmt::{Display, Formatter};
use std::rc::Rc;
use std::sync::Arc;

pub(crate) struct SortingStoredFieldsConsumer<D>
where
    D: Directory,
{
    pub(crate) writer: Option<StoredFieldsWriterEnum<TrackingTmpOutputDirectoryWrapper<D>>>,
    tmp_directory: TrackingTmpOutputDirectoryWrapper<D>,
    stored_fields_format: Option<Lucene90CompressingStoredFieldsFormat>,
}
impl<D> SortingStoredFieldsConsumer<D>
where
    D: Directory,
{
    pub(crate) fn new(directory: Arc<D>) -> Self {
        let tmp_directory = TrackingTmpOutputDirectoryWrapper::new(directory);
        Self {
            writer: None,
            tmp_directory,
            stored_fields_format: None,
        }
    }
}

impl<D> StoredFieldsConsumerBase for SortingStoredFieldsConsumer<D>
where
    D: Directory,
{
    type Directory = D;

    fn init_stored_fields_writer<D1>(&mut self, info: &mut SegmentInfo<D1>) -> Result<()>
    where
        D1: Directory,
    {
        let stored_fields_format = Lucene90CompressingStoredFieldsFormat::new(
            "TempStoredFields",
            CompressionModeEnum::Impl(NoCompression),
            128 * 1024,
            1,
            10,
        )?;
        self.writer = Some(stored_fields_format.fields_writer(
            &self.tmp_directory,
            info,
            &IOContext::default_io_context()?,
        )?);
        self.stored_fields_format = Some(stored_fields_format);
        Ok(())
    }

    fn flush<DM, D1>(
        &mut self,
        state: &SegmentWriteState<Self::Directory>,
        sort_map: Option<Rc<DM>>,
        codec: &impl Codec,
        info: &mut SegmentInfo<D1>,
    ) -> Result<()>
    where
        DM: DocMap,
        D1: Directory,
    {
        let mut reader = self.stored_fields_format.as_ref().unwrap().fields_reader(
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

        reader.check_integrity()?;
        let mut visitor = CopyVisitor;
        let max_doc = info.max_doc()?;
        for doc_id in 0..max_doc {
            sort_writer.start_document()?;
            let mapped_doc = if let Some(sort_map) = &sort_map {
                sort_map.new_to_old(doc_id)
            } else {
                doc_id
            };
            reader.document_with_visitor(mapped_doc, &mut visitor, &mut sort_writer)?;
            sort_writer.finish_document()?;
        }

        sort_writer.finish(max_doc, state.directory)?;

        let names = &self.tmp_directory.get_temporary_files().borrow().file_names;
        IOUtils::delete_files(&self.tmp_directory, names.values())?;

        Ok(())
    }

    fn abort(&mut self) -> Result<()> {
        let file_names = &self.tmp_directory.get_temporary_files().borrow().file_names;
        IOUtils::delete_files(&self.tmp_directory, file_names.values())?;
        Ok(())
    }
}

/// A visitor that copies every field it sees in the provided [`StoredFieldsWriter`]
#[derive(Default)]
pub(crate) struct CopyVisitor;
impl StoredFieldVisitor for CopyVisitor {
    fn binary_field_with_input(
        &mut self,
        field_info: Arc<FieldInfo>,
        input: &mut impl DataInput,
        length: i32,
        writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        writer.write_field_with_input(&field_info, input, length)
    }

    fn binary_field(
        &mut self,
        field_info: Arc<FieldInfo>,
        value: Vec<u8>,
        writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        writer.write_field_bytes(&field_info, &BytesRef::from_bytes(value))
    }

    fn string_field(
        &mut self,
        field_info: Arc<FieldInfo>,
        value: String,
        writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        writer.write_field_str(&field_info, &value)
    }

    fn int_field(
        &mut self,
        field_info: Arc<FieldInfo>,
        value: i32,
        writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        writer.write_field_i32(&field_info, value)
    }

    fn long_field(
        &mut self,
        field_info: Arc<FieldInfo>,
        value: i64,
        writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        writer.write_field_i64(&field_info, value)
    }

    fn float_field(
        &mut self,
        field_info: Arc<FieldInfo>,
        value: f32,
        writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        writer.write_field_f32(&field_info, value)
    }

    fn double_field(
        &mut self,
        field_info: Arc<FieldInfo>,
        value: f64,
        writer: &mut impl StoredFieldsWriter,
    ) -> Result<()> {
        writer.write_field_f64(&field_info, value)
    }

    fn needs_field(
        &mut self,
        _field_info: Arc<FieldInfo>,
        _writer: &mut impl StoredFieldsWriter,
    ) -> Result<Status> {
        Ok(Status::Yes)
    }
}

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
        if let Some(new_array) = ArrayUtil::grow_no_copy(&bytes.bytes, length as usize) {
            bytes.bytes = new_array
        }
        input.skip_bytes(offset as i64)?;
        input.read_bytes(&mut bytes.bytes, 0, length)?;
        bytes.offset = 0;
        bytes.length = length as usize;
        Ok(())
    }
}
