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
use crate::core::codecs::term_vectors_format::TermVectorsFormat;
use crate::core::codecs::term_vectors_reader::{DefaultTermVectorsReader, TermVectorsReader};
use crate::core::codecs::term_vectors_writer::TermVectorsWriter;
use crate::core::index::BytesRef;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::fields::Fields;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::term_vectors::{RawTermVectors, TermVectors};
use crate::core::store::directory::Directory;
use crate::core::store::{IOContext, IndexInput};
use crate::core::util::accountable::Accountable;
use crate::core::util::clone::TryClone;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::index::asserting_leaf_reader::AssertingFields;
use crate::test_framework::core::util::test_util::{DefaultTermVectorsFormat, TestUtil};
use std::sync::Arc;

/// Just like the default vectors format but with additional asserts.
pub struct AssertingTermVectorsFormat {
  in_: DefaultTermVectorsFormat,
}

impl AssertingTermVectorsFormat {
  pub fn new() -> Self {
    Self {
      in_: TestUtil::get_default_codec().term_vectors_format(),
    }
  }
}

impl TermVectorsFormat for AssertingTermVectorsFormat {
  type TermVectorsReader<T: IndexInput> = AssertingTermVectorsReader<
    <DefaultTermVectorsFormat as TermVectorsFormat>::TermVectorsReader<T>,
  >;

  fn vectors_reader<D1, D2>(
    &self,
    directory: &D1,
    segment_info: &SegmentInfo<D2>,
    field_infos: Arc<FieldInfos>,
    context: &IOContext,
  ) -> Result<Self::TermVectorsReader<D1::IndexInput>>
  where
    D1: Directory,
  {
    Ok(AssertingTermVectorsReader::new(self.in_.vectors_reader(
      directory,
      segment_info,
      field_infos,
      context,
    )?))
  }

  type TermVectorsWriter<D: Directory> = AssertingTermVectorsWriter<
    <DefaultTermVectorsFormat as TermVectorsFormat>::TermVectorsWriter<D>,
  >;

  fn vectors_writer<D1, D2>(
    &self,
    directory: D1,
    segment_info: &SegmentInfo<D2>,
    context: &IOContext,
  ) -> Result<Self::TermVectorsWriter<D1>>
  where
    D1: Directory,
  {
    Ok(AssertingTermVectorsWriter::new(self.in_.vectors_writer(
      directory,
      segment_info,
      context,
    )?))
  }
}

pub struct AssertingTermVectorsReader<TVR> {
  in_: TVR,
}

impl<TVR> AssertingTermVectorsReader<TVR>
where
  TVR: TermVectorsReader,
{
  fn new(in_: TVR) -> Self {
    Self { in_ }
  }
}

impl<TVR> CloseableRef for AssertingTermVectorsReader<TVR>
where
  TVR: TermVectorsReader,
{
  fn close(&self) -> Result<()> {
    self.in_.close()?;
    self.in_.close()
  }
}

impl<TVR> RawTermVectors for AssertingTermVectorsReader<TVR>
where
  TVR: TermVectorsReader,
{
  type IndexInput = TVR::IndexInput;

  fn raw_term_vectors_mut(&mut self) -> Result<&mut DefaultTermVectorsReader<Self::IndexInput>> {
    Err(LuceneError::unsupported_operation(
      "raw term vectors are not available",
    ))
  }

  fn raw_term_vectors(&self) -> Result<&DefaultTermVectorsReader<Self::IndexInput>> {
    Err(LuceneError::unsupported_operation(
      "raw term vectors are not available",
    ))
  }
}

impl<TVR> TermVectors for AssertingTermVectorsReader<TVR>
where
  TVR: TermVectorsReader,
{
  type Fields = AssertingFields<TVR::Fields>;
  type Terms = <Self::Fields as Fields>::Terms;

  fn get(&mut self, doc: i32) -> Result<Option<Self::Fields>> {
    Ok(self.in_.get(doc)?.map(AssertingFields::new))
  }

  fn get_field_terms(
    &mut self,
    doc: i32,
    field: &str,
  ) -> Result<Option<<Self::Fields as Fields>::Terms>> {
    self.default_get_field_terms(doc, field)
  }
}

impl<TVR> TryClone for AssertingTermVectorsReader<TVR>
where
  TVR: TermVectorsReader,
{
  fn try_clone(&self) -> Result<Self> {
    Ok(Self::new(self.in_.try_clone()?))
  }
}

impl<TVR> TermVectorsReader for AssertingTermVectorsReader<TVR>
where
  TVR: TermVectorsReader,
{
  fn check_integrity(&self) -> Result<()> {
    self.in_.check_integrity()
  }

  fn get_merge_instance(&self) -> Result<Option<Self>>
  where
    Self: Sized,
  {
    Ok(
      self
        .in_
        .get_merge_instance()?
        .map(AssertingTermVectorsReader::new),
    )
  }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
  Undefined,
  Started,
  Finished,
}

pub struct AssertingTermVectorsWriter<TVW> {
  in_: TVW,
  doc_status: Status,
  field_status: Status,
  term_status: Status,
  doc_count: i32,
  field_count: i32,
  term_count: i32,
  position_count: i32,
  has_positions: bool,
}

impl<TVW> AssertingTermVectorsWriter<TVW>
where
  TVW: TermVectorsWriter,
{
  fn new(in_: TVW) -> Self {
    Self {
      in_,
      doc_status: Status::Undefined,
      field_status: Status::Undefined,
      term_status: Status::Undefined,
      doc_count: 0,
      field_count: 0,
      term_count: 0,
      position_count: 0,
      has_positions: false,
    }
  }
}

impl<TVW> TermVectorsWriter for AssertingTermVectorsWriter<TVW>
where
  TVW: TermVectorsWriter,
{
  fn start_document(&mut self, num_vector_fields: i32) -> Result<()> {
    assert_eq!(self.field_count, 0);
    assert!(self.doc_status != Status::Started);
    self.in_.start_document(num_vector_fields)?;
    self.doc_status = Status::Started;
    self.field_count = num_vector_fields;
    self.doc_count += 1;
    Ok(())
  }

  fn finish_document(&mut self) -> Result<()> {
    assert_eq!(self.field_count, 0);
    assert!(self.doc_status == Status::Started);
    self.in_.finish_document()?;
    self.doc_status = Status::Finished;
    Ok(())
  }

  fn start_field(
    &mut self,
    field_info: &FieldInfo,
    num_terms: usize,
    positions: bool,
    offsets: bool,
    payloads: bool,
  ) -> Result<()> {
    assert_eq!(self.term_count, 0);
    assert!(self.doc_status == Status::Started);
    assert!(self.field_status != Status::Started);
    self
      .in_
      .start_field(field_info, num_terms, positions, offsets, payloads)?;
    self.field_status = Status::Started;
    self.term_count = i32::try_from(num_terms).expect("term count must fit in an i32");
    self.has_positions = positions || offsets || payloads;
    Ok(())
  }

  fn finish_field(&mut self) -> Result<()> {
    assert_eq!(self.term_count, 0);
    assert!(self.field_status == Status::Started);
    self.in_.finish_field()?;
    self.field_status = Status::Finished;
    self.field_count -= 1;
    Ok(())
  }

  fn start_term(&mut self, term: &BytesRef<Vec<u8>>, freq: i32) -> Result<()> {
    assert!(self.doc_status == Status::Started);
    assert!(self.field_status == Status::Started);
    assert!(self.term_status != Status::Started);
    self.in_.start_term(term, freq)?;
    self.term_status = Status::Started;
    self.position_count = if self.has_positions { freq } else { 0 };
    Ok(())
  }

  fn finish_term(&mut self) -> Result<()> {
    assert_eq!(self.position_count, 0);
    assert!(self.doc_status == Status::Started);
    assert!(self.field_status == Status::Started);
    assert!(self.term_status == Status::Started);
    self.in_.finish_term()?;
    self.term_status = Status::Finished;
    self.term_count -= 1;
    Ok(())
  }

  fn add_position(
    &mut self,
    position: i32,
    start_offset: i32,
    end_offset: i32,
    payload: Option<&BytesRef<Vec<u8>>>,
  ) -> Result<()> {
    assert!(self.doc_status == Status::Started);
    assert!(self.field_status == Status::Started);
    assert!(self.term_status == Status::Started);
    self
      .in_
      .add_position(position, start_offset, end_offset, payload)?;
    self.position_count -= 1;
    Ok(())
  }

  fn finish(&mut self, num_docs: i32) -> Result<()> {
    assert_eq!(self.doc_count, num_docs);
    assert!(
      self.doc_status
        == if num_docs > 0 {
          Status::Finished
        } else {
          Status::Undefined
        }
    );
    assert!(self.field_status != Status::Started);
    assert!(self.term_status != Status::Started);
    self.in_.finish(num_docs)
  }
}

impl<TVW> Closeable for AssertingTermVectorsWriter<TVW>
where
  TVW: TermVectorsWriter,
{
  fn close(&mut self) -> Result<()> {
    self.in_.close()?;
    self.in_.close()
  }
}

impl<TVW> Accountable for AssertingTermVectorsWriter<TVW>
where
  TVW: TermVectorsWriter,
{
  fn ram_bytes_used(&self) -> Result<i64> {
    self.in_.ram_bytes_used()
  }
}
