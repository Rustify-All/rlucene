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
use crate::core::codecs::stored_fields_format::StoredFieldsFormat;
use crate::core::codecs::stored_fields_writer::{StoredFieldsWriter, StoredFieldsWriterEnum};
use crate::core::document::field::FieldDataEnum;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorter::DocMap;
use crate::core::index::sorting_stored_fields_consumer::SortingStoredFieldsConsumer;
use crate::core::store::IOContext;
use crate::core::store::directory::Directory;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::number::Number;
use std::sync::Arc;

pub(crate) struct StoredFieldsConsumer<D>
where
    D: Directory,
{
    directory: Arc<D>,
    pub(crate) writer: Option<StoredFieldsWriterEnum<D>>,
    last_doc: i32,
    sub: Option<SortingStoredFieldsConsumer<D>>,
}
impl<D> StoredFieldsConsumer<D>
where
    D: Directory,
{
    pub(crate) fn new(directory: Arc<D>, sub: Option<SortingStoredFieldsConsumer<D>>) -> Self {
        Self {
            directory,
            writer: None,
            last_doc: -1,
            sub,
        }
    }
    fn init_stored_fields_writer<D1>(
        &mut self,
        codec: &impl Codec,
        info: &mut SegmentInfo<D1>,
    ) -> Result<()>
    where
        D1: Directory,
    {
        match self.sub {
            Some(ref mut sub) => {
                if sub.writer.is_none() {
                    sub.init_stored_fields_writer(info)?;
                }
            },
            None => {
                if self.writer.is_none() {
                    let writer = codec.stored_fields_format().fields_writer(
                        self.directory.as_ref(),
                        info,
                        &IOContext::default_io_context()?,
                    )?;
                    self.writer = Some(writer);
                }
            },
        }
        Ok(())
    }

    pub(crate) fn start_document<D1>(
        &mut self,
        codec: &impl Codec,
        doc_id: i32,
        info: &mut SegmentInfo<D1>,
    ) -> Result<()>
    where
        D1: Directory,
    {
        debug_assert!(self.last_doc < doc_id);
        self.init_stored_fields_writer(codec, info)?;

        match self.sub {
            Some(ref mut sub) => {
                while self.last_doc + 1 < doc_id {
                    self.last_doc += 1;
                    if let Some(writer) = &mut sub.writer {
                        writer.start_document()?;
                        writer.finish_document()?;
                    }
                }
                self.last_doc += 1;
                if let Some(writer) = &mut sub.writer {
                    writer.start_document()?;
                }
            },
            None => {
                while self.last_doc + 1 < doc_id {
                    self.last_doc += 1;
                    if let Some(writer) = &mut self.writer {
                        writer.start_document()?;
                        writer.finish_document()?;
                    }
                }
                self.last_doc += 1;
                match self.writer {
                    None => return Err(LuceneError::illegal_state("writer must be initialized")),
                    Some(ref mut v) => v.start_document()?,
                }
            },
        }
        Ok(())
    }

    pub(crate) fn write_field(&mut self, info: &FieldInfo, value: &FieldDataEnum) -> Result<()> {
        match self.writer {
            Some(ref mut writer) => match value {
                FieldDataEnum::Binary(bytes) => {
                    writer.write_field_bytes(info, bytes)?;
                },
                FieldDataEnum::String(s) => {
                    writer.write_field_str(info, s)?;
                },
                FieldDataEnum::Number(num) => {
                    match num {
                        Number::I32(n) => writer.write_field_i32(info, *n),
                        Number::I64(n) => writer.write_field_i64(info, *n),
                        Number::F32(n) => writer.write_field_f32(info, *n),
                        Number::F64(n) => writer.write_field_f64(info, *n),
                        _ => return Err(LuceneError::illegal_argument("unsupported number type")),
                    }
                }?,
                _ => return Err(LuceneError::illegal_argument("unsupported field type")),
            },
            None => return Err(LuceneError::illegal_argument("writer must be initialized")),
        }
        Ok(())
    }

    pub(crate) fn finish_document(&mut self) -> Result<()> {
        match self.sub {
            Some(ref mut sub) => {
                let writer = sub.writer.as_mut().expect("sub writer must be initialized");
                writer.finish_document()?;
            },
            None => {
                let writer = self.writer.as_mut().expect("writer must be initialized");
                writer.finish_document()?;
            },
        }
        Ok(())
    }

    pub(crate) fn finish<D1>(
        &mut self,
        codec: &impl Codec,
        max_doc: i32,
        info: &mut SegmentInfo<D1>,
    ) -> Result<()>
    where
        D1: Directory,
    {
        while self.last_doc < max_doc - 1 {
            self.start_document(codec, self.last_doc + 1, info)?;
            self.finish_document()?;
        }
        Ok(())
    }

    pub(crate) fn flush<DM, D1>(
        &mut self,
        _sort_map: Option<Arc<DM>>,
        info: &SegmentInfo<D1>,
        dir: &D,
    ) -> Result<()>
    where
        DM: DocMap,
        D1: Directory,
    {
        match self.sub {
            Some(ref mut sub) => {
                sub.writer.as_mut().unwrap().finish(info.max_doc()?, dir)?;
                let _ = sub.writer.take();
                unimplemented!("这里要调用sub的flush方法");
            },
            None => {
                self.writer.as_mut().unwrap().finish(info.max_doc()?, dir)?;
                let _ = self.writer.take();
            },
        }
        Ok(())
    }

    pub(crate) fn abort(&mut self) -> Result<()> {
        match self.sub {
            Some(ref mut sub) => sub.abort(),
            None => Ok(()),
        }
    }
}

pub(crate) trait StoredFieldsConsumerBase {
    type Directory: Directory;
    fn init_stored_fields_writer<D1>(&mut self, info: &mut SegmentInfo<D1>) -> Result<()>
    where
        D1: Directory;
    fn flush<DM, D1>(
        &mut self,
        state: &SegmentWriteState<Self::Directory>,
        sort_map: Option<Arc<DM>>,
        codec: &impl Codec,
        info: &mut SegmentInfo<D1>,
    ) -> Result<()>
    where
        DM: DocMap,
        D1: Directory;
    fn abort(&mut self) -> Result<()>;
}
