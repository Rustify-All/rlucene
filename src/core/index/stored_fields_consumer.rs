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
use crate::core::codecs::stored_fields_format::StoredFieldsFormat;
use crate::core::codecs::stored_fields_writer::StoredFieldsWriter;
use crate::core::codecs::{Codec, CodecStoredFieldsWriter, Codecs};
use crate::core::document::field::FieldDataEnum;
use crate::core::index::BytesRef;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorter::DocMap;
use crate::core::index::sorting_stored_fields_consumer::SortingStoredFieldsConsumer;
use crate::core::store::IO_CONTEXT_DEFAULT;

use crate::core::store::directory::Directory;
use crate::core::util::IOUtils;
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::number::Number;
#[cfg(test)]
use crate::test_framework::core::index::test_stored_fields_consumer::TestStoredFieldsConsumerHook;
#[cfg(test)]
use crate::test_framework::core::util::failure_context::{
  ExecutionMethod, ExecutionOwner, ExecutionScope,
};

pub(crate) struct StoredFieldsConsumer<D>
where
  D: Directory,
{
  directory: D,
  codec: Codecs,
  last_doc: i32,
  hook: StoredFieldsConsumerHook<D>,
}

pub(crate) enum StoredFieldsConsumerHook<D>
where
  D: Directory,
{
  Default {
    writer: Option<CodecStoredFieldsWriter<D>>,
  },
  Sorting(SortingStoredFieldsConsumer<D>),
  #[cfg(test)]
  TestStoredFieldsConsumer(TestStoredFieldsConsumerHook<D>),
}

pub(crate) struct StoredFieldsConsumerDefaults;

impl<D> Default for StoredFieldsConsumerHook<D>
where
  D: Directory,
{
  fn default() -> Self {
    Self::Default { writer: None }
  }
}

impl<D> StoredFieldsConsumerHook<D>
where
  D: Directory,
{
  fn write_field<V: StoredFieldValue>(&mut self, info: &FieldInfo, value: &V) -> Result<()> {
    match self {
      Self::Default { writer } => StoredFieldsConsumerDefaults::write_field(writer, info, value),
      Self::Sorting(hook) => {
        StoredFieldsConsumerDefaults::write_field(&mut hook.writer, info, value)
      },
      #[cfg(test)]
      Self::TestStoredFieldsConsumer(hook) => {
        StoredFieldsConsumerDefaults::write_field(&mut hook.writer, info, value)
      },
    }
  }

  fn ram_bytes_used(&self) -> Result<i64> {
    match self {
      Self::Default { writer } => writer.as_ref().map_or(Ok(0), Accountable::ram_bytes_used),
      Self::Sorting(hook) => hook
        .writer
        .as_ref()
        .map_or(Ok(0), Accountable::ram_bytes_used),
      #[cfg(test)]
      Self::TestStoredFieldsConsumer(hook) => hook
        .writer
        .as_ref()
        .map_or(Ok(0), Accountable::ram_bytes_used),
    }
  }
}

impl<D> StoredFieldsConsumer<D>
where
  D: Directory + Clone,
{
  pub(crate) fn new(codec: Codecs, directory: D, hook: StoredFieldsConsumerHook<D>) -> Self {
    Self {
      directory,
      codec,
      last_doc: -1,
      hook,
    }
  }
  fn init_stored_fields_writer<D1>(&mut self, info: &mut SegmentInfo<D1>) -> Result<()> {
    self
      .hook
      .init_stored_fields_writer(&self.directory, &self.codec, info)
  }

  pub(crate) fn start_document<D1>(
    &mut self,
    doc_id: i32,
    info: &mut SegmentInfo<D1>,
  ) -> Result<()> {
    debug_assert!(self.last_doc < doc_id);
    self.init_stored_fields_writer(info)?;

    self.hook.start_document(&mut self.last_doc, doc_id)
  }

  pub(crate) fn write_field<V: StoredFieldValue>(
    &mut self,
    info: &FieldInfo,
    value: &V,
  ) -> Result<()> {
    self.hook.write_field(info, value)
  }

  pub(crate) fn finish_document(&mut self) -> Result<()> {
    #[cfg(test)]
    let _execution_scope = ExecutionScope::enter(
      ExecutionOwner::StoredFieldsConsumer,
      ExecutionMethod::FinishDocument,
    );
    self.hook.finish_document()
  }

  pub(crate) fn finish<D1>(&mut self, max_doc: i32, info: &mut SegmentInfo<D1>) -> Result<()> {
    while self.last_doc < max_doc - 1 {
      self.start_document(self.last_doc + 1, info)?;
      self.finish_document()?;
    }
    Ok(())
  }

  pub(crate) fn flush<DM, D1>(
    &mut self,
    state: &SegmentWriteState<D>,
    sort_map: Option<&DM>,
    info: &mut SegmentInfo<D1>,
  ) -> Result<()>
  where
    DM: DocMap,
  {
    self.hook.flush(&self.codec, state, sort_map, info)
  }

  pub(crate) fn abort(&mut self) -> Result<()> {
    self.hook.abort()
  }
}

impl<D> Accountable for StoredFieldsConsumer<D>
where
  D: Directory,
{
  fn ram_bytes_used(&self) -> Result<i64> {
    self.hook.ram_bytes_used()
  }
}

impl StoredFieldsConsumerDefaults {
  pub(crate) fn init_stored_fields_writer<D, D1>(
    writer: &mut Option<CodecStoredFieldsWriter<D>>,
    directory: &D,
    codec: &Codecs,
    info: &mut SegmentInfo<D1>,
  ) -> Result<()>
  where
    D: Directory + Clone,
  {
    if writer.is_none() {
      *writer = Some(codec.stored_fields_format().fields_writer(
        directory.clone(),
        info,
        IO_CONTEXT_DEFAULT.as_ref().map_err(Clone::clone)?,
      )?);
    }
    Ok(())
  }

  pub(crate) fn start_document<TW>(
    writer: &mut Option<TW>,
    last_doc: &mut i32,
    doc_id: i32,
  ) -> Result<()>
  where
    TW: StoredFieldsWriter,
  {
    let writer = writer
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("writer must be initialized"))?;
    while *last_doc + 1 < doc_id {
      *last_doc += 1;
      writer.start_document()?;
      writer.finish_document()?;
    }
    *last_doc += 1;
    writer.start_document()
  }

  fn write_field<TW, V>(writer: &mut Option<TW>, info: &FieldInfo, value: &V) -> Result<()>
  where
    TW: StoredFieldsWriter,
    V: StoredFieldValue,
  {
    let writer = writer
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("writer must be initialized"))?;
    value.write_to(writer, info)
  }

  pub(crate) fn finish_document<TW>(writer: &mut Option<TW>) -> Result<()>
  where
    TW: StoredFieldsWriter,
  {
    writer
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("writer must be initialized"))?
      .finish_document()
  }

  pub(crate) fn flush<TW, WD, D1>(
    writer: &mut Option<TW>,
    directory: &WD,
    info: &mut SegmentInfo<D1>,
  ) -> Result<()>
  where
    TW: StoredFieldsWriter,
    WD: Directory,
  {
    let writer = writer
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("writer must be initialized"))?;
    let finish_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      writer.finish(info.max_doc()?, directory)
    }));
    writer.close()?;
    unwrap_caught_result!(finish_result)
  }

  pub(crate) fn abort<TW>(writer: &mut Option<TW>) -> Result<()>
  where
    TW: StoredFieldsWriter,
  {
    IOUtils::close_while_handling_exception(writer.as_mut());
    Ok(())
  }
}

pub(crate) trait StoredFieldValue {
  fn write_to<W: StoredFieldsWriter>(&self, writer: &mut W, info: &FieldInfo) -> Result<()>;
}

impl StoredFieldValue for BytesRef<Vec<u8>> {
  fn write_to<W: StoredFieldsWriter>(&self, writer: &mut W, info: &FieldInfo) -> Result<()> {
    writer.write_field_bytes(info, self)
  }
}

impl StoredFieldValue for FieldDataEnum {
  fn write_to<W: StoredFieldsWriter>(&self, writer: &mut W, info: &FieldInfo) -> Result<()> {
    match self {
      Self::Binary(bytes) => bytes.write_to(writer, info),
      Self::String(s) => writer.write_field_str(info, s),
      Self::SharedString(s) => writer.write_field_str(info, s),
      Self::Number(num) => match num {
        Number::I32(n) => writer.write_field_i32(info, *n),
        Number::I64(n) => writer.write_field_i64(info, *n),
        Number::F32(n) => writer.write_field_f32(info, *n),
        Number::F64(n) => writer.write_field_f64(info, *n),
        _ => Err(LuceneError::illegal_argument("unsupported number type")),
      },
      _ => Err(LuceneError::illegal_argument("unsupported field type")),
    }
  }
}

pub(crate) trait StoredFieldsConsumerBase {
  type Directory: Directory;
  fn init_stored_fields_writer<D1>(
    &mut self,
    directory: &Self::Directory,
    codec: &Codecs,
    info: &mut SegmentInfo<D1>,
  ) -> Result<()>;
  fn start_document(&mut self, last_doc: &mut i32, doc_id: i32) -> Result<()>;
  fn finish_document(&mut self) -> Result<()>;
  fn flush<DM, D1>(
    &mut self,
    codec: &Codecs,
    state: &SegmentWriteState<Self::Directory>,
    sort_map: Option<&DM>,
    info: &mut SegmentInfo<D1>,
  ) -> Result<()>
  where
    DM: DocMap;
  fn abort(&mut self) -> Result<()>;
}

impl<D> StoredFieldsConsumerBase for StoredFieldsConsumerHook<D>
where
  D: Directory + Clone,
{
  type Directory = D;

  fn init_stored_fields_writer<D1>(
    &mut self,
    directory: &Self::Directory,
    codec: &Codecs,
    info: &mut SegmentInfo<D1>,
  ) -> Result<()> {
    match self {
      Self::Default { writer } => {
        StoredFieldsConsumerDefaults::init_stored_fields_writer(writer, directory, codec, info)
      },
      Self::Sorting(hook) => hook.init_stored_fields_writer(directory, codec, info),
      #[cfg(test)]
      Self::TestStoredFieldsConsumer(hook) => {
        hook.init_stored_fields_writer(directory, codec, info)
      },
    }
  }

  fn start_document(&mut self, last_doc: &mut i32, doc_id: i32) -> Result<()> {
    match self {
      Self::Default { writer } => {
        StoredFieldsConsumerDefaults::start_document(writer, last_doc, doc_id)
      },
      Self::Sorting(hook) => {
        StoredFieldsConsumerDefaults::start_document(&mut hook.writer, last_doc, doc_id)
      },
      #[cfg(test)]
      Self::TestStoredFieldsConsumer(hook) => hook.start_document(last_doc, doc_id),
    }
  }

  fn finish_document(&mut self) -> Result<()> {
    match self {
      Self::Default { writer } => StoredFieldsConsumerDefaults::finish_document(writer),
      Self::Sorting(hook) => StoredFieldsConsumerDefaults::finish_document(&mut hook.writer),
      #[cfg(test)]
      Self::TestStoredFieldsConsumer(hook) => hook.finish_document(),
    }
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
  {
    match self {
      Self::Default { writer } => {
        StoredFieldsConsumerDefaults::flush(writer, state.directory, info)
      },
      Self::Sorting(hook) => hook.flush(codec, state, sort_map, info),
      #[cfg(test)]
      Self::TestStoredFieldsConsumer(hook) => hook.flush(codec, state, sort_map, info),
    }
  }

  fn abort(&mut self) -> Result<()> {
    match self {
      Self::Default { writer } => StoredFieldsConsumerDefaults::abort(writer),
      Self::Sorting(hook) => hook.abort(),
      #[cfg(test)]
      Self::TestStoredFieldsConsumer(hook) => hook.abort(),
    }
  }
}
