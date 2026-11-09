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
use crate::core::codecs::doc_values_consumer::DocValuesConsumer;
use crate::core::index::binary_doc_values_writer::{
  BinaryDocValuesWriter, BufferedBinaryDocValues,
};
use crate::core::index::docs_with_field_set::DocsWithFieldSetDISI;
use crate::core::index::numeric_doc_values_writer::{
  BufferedNumericDocValues, NumericDocValuesWriter,
};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorted_doc_values_writer::{
  BufferedSortedDocValues, SortedDocValuesWriter,
};
use crate::core::index::sorted_numeric_doc_values_writer::{
  SortedNumericDocValuesWriter, SortedNumericDocValuesWriterValues,
};
use crate::core::index::sorted_set_doc_values_writer::{
  SortedSetDocValuesWriter, SortedSetDocValuesWriterDocIdSetIterator,
};
use crate::core::index::sorter::DocMap;
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, DocIdSetIteratorEnum5};
use crate::core::store::directory::Directory;
use crate::core::util::ByteBlockPool;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::paged_bytes::PagedBytesDataInput;
use std::fmt::Display;
use std::sync::Arc;

pub(crate) trait DocValuesWriter: Display {
  fn flush<D1, D2, DM, DC>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    sort_map: Option<&DM>,
    dv_consumer: &mut DC,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = DC::IndexOutput>,
    DM: DocMap,
    DC: DocValuesConsumer;

  type DocIdSetIterator: DocIdSetIterator;
  fn get_doc_values(&self) -> Result<Self::DocIdSetIterator>;
  fn finish(&mut self, pool: &Arc<ByteBlockPool>) -> Result<()>;
}

pub(crate) enum DocValuesWriterEnum {
  Binary(Box<BinaryDocValuesWriter>),
  Numeric(Box<NumericDocValuesWriter>),
  SortedNumeric(Box<SortedNumericDocValuesWriter>),
  Sorted(Box<SortedDocValuesWriter>),
  SortedSet(Box<SortedSetDocValuesWriter>),
}
impl Display for DocValuesWriterEnum {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      DocValuesWriterEnum::Binary(writer) => writer.fmt(f),
      DocValuesWriterEnum::Numeric(writer) => writer.fmt(f),
      DocValuesWriterEnum::SortedNumeric(writer) => writer.fmt(f),
      DocValuesWriterEnum::Sorted(writer) => writer.fmt(f),
      DocValuesWriterEnum::SortedSet(writer) => writer.fmt(f),
    }
  }
}
impl DocValuesWriter for DocValuesWriterEnum {
  fn flush<D1, D2, DM, DC>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    sort_map: Option<&DM>,
    dv_consumer: &mut DC,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<()>
  where
    D1: Directory<IndexOutput = DC::IndexOutput>,
    DM: DocMap,
    DC: DocValuesConsumer,
  {
    match self {
      DocValuesWriterEnum::Binary(writer) => {
        writer.flush(write_state, sort_map, dv_consumer, segment_info)
      },
      DocValuesWriterEnum::Numeric(writer) => {
        writer.flush(write_state, sort_map, dv_consumer, segment_info)
      },
      DocValuesWriterEnum::SortedNumeric(writer) => {
        writer.flush(write_state, sort_map, dv_consumer, segment_info)
      },
      DocValuesWriterEnum::Sorted(writer) => {
        writer.flush(write_state, sort_map, dv_consumer, segment_info)
      },
      DocValuesWriterEnum::SortedSet(writer) => {
        writer.flush(write_state, sort_map, dv_consumer, segment_info)
      },
    }
  }

  type DocIdSetIterator = DocValuesWriterDISI;

  fn get_doc_values(&self) -> Result<Self::DocIdSetIterator> {
    match self {
      DocValuesWriterEnum::Binary(writer) => Ok(DocIdSetIteratorEnum5::A(writer.get_doc_values()?)),
      DocValuesWriterEnum::Numeric(writer) => {
        Ok(DocIdSetIteratorEnum5::B(writer.get_doc_values()?))
      },
      DocValuesWriterEnum::SortedNumeric(writer) => {
        Ok(DocIdSetIteratorEnum5::C(writer.get_doc_values()?))
      },
      DocValuesWriterEnum::Sorted(writer) => Ok(DocIdSetIteratorEnum5::D(writer.get_doc_values()?)),
      DocValuesWriterEnum::SortedSet(writer) => {
        Ok(DocIdSetIteratorEnum5::E(writer.get_doc_values()?))
      },
    }
  }

  fn finish(&mut self, pool: &Arc<ByteBlockPool>) -> Result<()> {
    match self {
      DocValuesWriterEnum::Binary(writer) => writer.finish(pool),
      DocValuesWriterEnum::Numeric(writer) => writer.finish(pool),
      DocValuesWriterEnum::SortedNumeric(writer) => writer.finish(pool),
      DocValuesWriterEnum::Sorted(writer) => writer.finish(pool),
      DocValuesWriterEnum::SortedSet(writer) => writer.finish(pool),
    }
  }
}

pub(crate) type DocValuesWriterDISI = DocIdSetIteratorEnum5<
  BufferedBinaryDocValues<DocsWithFieldSetDISI, PagedBytesDataInput>,
  BufferedNumericDocValues,
  SortedNumericDocValuesWriterValues,
  BufferedSortedDocValues<DocsWithFieldSetDISI>,
  SortedSetDocValuesWriterDocIdSetIterator,
>;
