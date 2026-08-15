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
use crate::core::codecs::stored_fields_reader::StoredFieldsReader;
use crate::core::codecs::stored_fields_writer::StoredFieldsWriter;
use crate::core::index::BytesRef;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::stored_field_visitor::StoredFieldVisitor;
use crate::core::index::stored_fields::{RawStoredFieldsReader, StoredFields};
use crate::core::store::directory::Directory;
use crate::core::store::{DataInput, IOContext, IndexInput};
use crate::core::util::accountable::Accountable;
use crate::core::util::clone::TryClone;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::codecs::asserting_codec::assert_thread;
use crate::test_framework::core::util::test_util::{DefaultStoredFieldsFormat, TestUtil};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::ThreadId;

/// Just like the default stored fields format but with additional asserts.
pub struct AssertingStoredFieldsFormat {
  in_: DefaultStoredFieldsFormat,
}

impl AssertingStoredFieldsFormat {
  pub fn new() -> Self {
    Self {
      in_: TestUtil::get_default_codec().stored_fields_format(),
    }
  }
}

impl StoredFieldsFormat for AssertingStoredFieldsFormat {
  type StoredFieldsReader<T: IndexInput> = AssertingStoredFieldsReader<
    <DefaultStoredFieldsFormat as StoredFieldsFormat>::StoredFieldsReader<T>,
  >;

  fn fields_reader<D1, D2>(
    &self,
    directory: &D1,
    segment_info: &SegmentInfo<D2>,
    field_infos: Arc<FieldInfos>,
    context: &IOContext,
  ) -> Result<Self::StoredFieldsReader<D1::IndexInput>>
  where
    D1: Directory,
  {
    Ok(AssertingStoredFieldsReader::new(
      self
        .in_
        .fields_reader(directory, segment_info, field_infos, context)?,
      segment_info.max_doc()?,
      false,
    ))
  }

  type StoredFieldsWriter<D: Directory> = AssertingStoredFieldsWriter<
    <DefaultStoredFieldsFormat as StoredFieldsFormat>::StoredFieldsWriter<D>,
  >;

  fn fields_writer<D1, D2>(
    &self,
    directory: D1,
    segment_info: &mut SegmentInfo<D2>,
    context: &IOContext,
  ) -> Result<Self::StoredFieldsWriter<D1>>
  where
    D1: Directory,
  {
    Ok(AssertingStoredFieldsWriter::new(self.in_.fields_writer(
      directory,
      segment_info,
      context,
    )?))
  }
}

pub struct AssertingStoredFieldsReader<SFR> {
  in_: SFR,
  max_doc: i32,
  merging: AtomicBool,
  creation_thread: ThreadId,
}

impl<SFR> AssertingStoredFieldsReader<SFR>
where
  SFR: StoredFieldsReader,
{
  fn new(in_: SFR, max_doc: i32, merging: bool) -> Self {
    Self {
      in_,
      max_doc,
      merging: AtomicBool::new(merging),
      creation_thread: std::thread::current().id(),
    }
  }
}

impl<SFR> CloseableRef for AssertingStoredFieldsReader<SFR>
where
  SFR: StoredFieldsReader,
{
  fn close(&self) -> Result<()> {
    self.in_.close()?;
    self.in_.close()
  }
}

impl<SFR> RawStoredFieldsReader for AssertingStoredFieldsReader<SFR>
where
  SFR: StoredFieldsReader,
{
  type IndexInput = SFR::IndexInput;
}

impl<SFR> StoredFields for AssertingStoredFieldsReader<SFR>
where
  SFR: StoredFieldsReader,
{
  fn document_with_visitor<W>(
    &mut self,
    doc_id: i32,
    visitor: &mut impl StoredFieldVisitor,
    writer: Option<&mut W>,
  ) -> Result<()>
  where
    W: StoredFieldsWriter,
  {
    assert_thread("StoredFieldsReader", self.creation_thread);
    assert!(doc_id >= 0 && doc_id < self.max_doc);
    self.in_.document_with_visitor(doc_id, visitor, writer)
  }
}

impl<SFR> TryClone for AssertingStoredFieldsReader<SFR>
where
  SFR: StoredFieldsReader,
{
  fn try_clone(&self) -> Result<Self> {
    assert!(
      !self.merging.load(Ordering::Relaxed),
      "Merge instances do not support cloning"
    );
    Ok(Self::new(self.in_.try_clone()?, self.max_doc, false))
  }
}

impl<SFR> StoredFieldsReader for AssertingStoredFieldsReader<SFR>
where
  SFR: StoredFieldsReader,
{
  fn check_integrity(&self) -> Result<()> {
    self.in_.check_integrity()
  }

  fn get_merge_instance(&self) -> Result<Option<Self>>
  where
    Self: Sized,
  {
    match self.in_.get_merge_instance()? {
      Some(in_) => Ok(Some(Self::new(in_, self.max_doc, true))),
      None => {
        self.merging.store(true, Ordering::Relaxed);
        Ok(None)
      },
    }
  }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
  Undefined,
  Started,
  Finished,
}

pub struct AssertingStoredFieldsWriter<SFW> {
  in_: SFW,
  num_written: i32,
  doc_status: Status,
}

impl<SFW> AssertingStoredFieldsWriter<SFW>
where
  SFW: StoredFieldsWriter,
{
  fn new(in_: SFW) -> Self {
    Self {
      in_,
      num_written: 0,
      doc_status: Status::Undefined,
    }
  }
}

impl<SFW> StoredFieldsWriter for AssertingStoredFieldsWriter<SFW>
where
  SFW: StoredFieldsWriter,
{
  fn start_document(&mut self) -> Result<()> {
    assert!(self.doc_status != Status::Started);
    self.in_.start_document()?;
    self.num_written += 1;
    self.doc_status = Status::Started;
    Ok(())
  }

  fn finish_document(&mut self) -> Result<()> {
    assert!(self.doc_status == Status::Started);
    self.in_.finish_document()?;
    self.doc_status = Status::Finished;
    Ok(())
  }

  fn write_field_i32(&mut self, field_info: &FieldInfo, value: i32) -> Result<()> {
    assert!(self.doc_status == Status::Started);
    self.in_.write_field_i32(field_info, value)
  }

  fn write_field_i64(&mut self, field_info: &FieldInfo, value: i64) -> Result<()> {
    assert!(self.doc_status == Status::Started);
    self.in_.write_field_i64(field_info, value)
  }

  fn write_field_f32(&mut self, field_info: &FieldInfo, value: f32) -> Result<()> {
    assert!(self.doc_status == Status::Started);
    self.in_.write_field_f32(field_info, value)
  }

  fn write_field_f64(&mut self, field_info: &FieldInfo, value: f64) -> Result<()> {
    assert!(self.doc_status == Status::Started);
    self.in_.write_field_f64(field_info, value)
  }

  fn write_field_with_input(
    &mut self,
    field_info: &FieldInfo,
    input: &mut impl DataInput,
    length: i32,
  ) -> Result<()> {
    assert!(self.doc_status == Status::Started);
    self.in_.write_field_with_input(field_info, input, length)
  }

  fn write_field_bytes(&mut self, field_info: &FieldInfo, value: &BytesRef<Vec<u8>>) -> Result<()> {
    assert!(self.doc_status == Status::Started);
    self.in_.write_field_bytes(field_info, value)
  }

  fn write_field_str(&mut self, field_info: &FieldInfo, value: &str) -> Result<()> {
    assert!(self.doc_status == Status::Started);
    self.in_.write_field_str(field_info, value)
  }

  fn finish<D>(&mut self, num_docs: i32, dir: &D) -> Result<()>
  where
    D: Directory,
  {
    assert!(
      self.doc_status
        == if num_docs > 0 {
          Status::Finished
        } else {
          Status::Undefined
        }
    );
    self.in_.finish(num_docs, dir)?;
    assert_eq!(num_docs, self.num_written);
    Ok(())
  }
}

impl<SFW> Closeable for AssertingStoredFieldsWriter<SFW>
where
  SFW: StoredFieldsWriter,
{
  fn close(&mut self) -> Result<()> {
    self.in_.close()?;
    self.in_.close()
  }
}

impl<SFW> Accountable for AssertingStoredFieldsWriter<SFW>
where
  SFW: StoredFieldsWriter + Accountable,
{
  fn ram_bytes_used(&self) -> Result<i64> {
    self.in_.ram_bytes_used()
  }
}
