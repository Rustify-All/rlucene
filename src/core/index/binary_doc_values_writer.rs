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
use crate::core::codecs::doc_values_producer::DocValuesProducer;
use crate::core::codecs::dummy::dummy_doc_values_skipper::DummyDocValuesSkipper;
use crate::core::codecs::dummy::dummy_numeric_doc_values::DummyNumericDocValues;
use crate::core::codecs::dummy::dummy_sorted_doc_values::DummySortedDocValues;
use crate::core::codecs::dummy::dummy_sorted_numeric_doc_values::DummySortedNumericDocValues;
use crate::core::codecs::dummy::dummy_sorted_set_doc_values::DummySortedSetDocValues;
use crate::core::index::binary_doc_values::BinaryDocValues;
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::doc_values_writer::DocValuesWriter;
use crate::core::index::docs_with_field_set::{DocsWithFieldSet, DocsWithFieldSetDISI};
use crate::core::index::field_info::FieldInfo;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorter::DocMap;
use crate::core::index::{BytesRef, BytesRefBuilder, BytesRefValue, BytesRefValueEnum};
use crate::core::search::doc_id_set::DocIdSet;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::store::directory::Directory;
use crate::core::store::{DataInput, DataOutput};
use crate::core::util::accountable::Accountable;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::packed::PackedInts;
use crate::core::util::packed::packed_long_values::{
  Builder, PackedLongValues, PackedLongValuesIterator,
};
use crate::core::util::paged_bytes::{
  PagedBytes, PagedBytesDataInput, PagedBytesDataOutput, get_data_input, get_data_output,
};
use crate::core::util::{
  AtomicCounter, ByteBlockPool, BytesRefArray, Counter, SharedCounter, SortableBytesRefArray,
  TryIntoInt,
};
use std::borrow::Borrow;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

/// Buffers up pending `[u8]` per doc, then flushes when segment flushes.
pub(crate) struct BinaryDocValuesWriter {
  field_info: Arc<FieldInfo>,
  bytes_out: PagedBytesDataOutput,
  iw_bytes_used: SharedCounter,
  lengths: BinaryLengths,
  docs_with_field: DocsWithFieldSet,
  bytes_used: i64,
  last_doc_id: i32,
  max_length: usize,
}

enum BinaryLengths {
  Pending(Builder),
  Finished(PackedLongValues),
}

impl BinaryDocValuesWriter {
  pub(crate) fn new(field_info: Arc<FieldInfo>, iw_bytes_used: SharedCounter) -> Result<Self> {
    let bytes = PagedBytes::new(BLOCK_BITS);
    let bytes_out = get_data_output(bytes)?;
    let lengths = PackedLongValues::delta_packed_long_values_builder_default(PackedInts::COMPACT)?;
    let docs_with_field = DocsWithFieldSet::new();

    let bytes_used = lengths.ram_bytes_used()? + docs_with_field.ram_bytes_used()?;
    iw_bytes_used.add_and_get(bytes_used);

    Ok(Self {
      field_info,
      bytes_out,
      iw_bytes_used,
      lengths: BinaryLengths::Pending(lengths),
      docs_with_field,
      bytes_used,
      last_doc_id: -1,
      max_length: 0,
    })
  }
  pub(crate) fn add_value(&mut self, doc_id: i32, value: &BytesRef<&[u8]>) -> Result<()> {
    if doc_id <= self.last_doc_id {
      return Err(LuceneError::illegal_argument(format!(
        "DocValuesField \"{}\" appears more than once in this document (only one value is allowed per field)",
        self.field_info.name
      )));
    }
    if value.length > MAX_LENGTH {
      return Err(LuceneError::illegal_argument(format!(
        "DocValuesField \"{}\" is too large, must be <= {}",
        self.field_info.name, MAX_LENGTH
      )));
    }

    self.max_length = self.max_length.max(value.length);
    let BinaryLengths::Pending(lengths) = &mut self.lengths else {
      return Err(LuceneError::illegal_state("Cannot be reused after build()"));
    };
    lengths.add(value.length as i64)?;

    self
      .bytes_out
      .write_bytes_range(value.bytes, value.offset, value.length)?;

    self.docs_with_field.add(doc_id)?;
    let lengths_bytes_used = lengths.ram_bytes_used()?;
    self.update_bytes_used(lengths_bytes_used)?;

    self.last_doc_id = doc_id;
    Ok(())
  }

  fn update_bytes_used(&mut self, lengths_bytes_used: i64) -> Result<()> {
    let new_bytes_used = lengths_bytes_used
      + self.bytes_out.paged_bytes.ram_bytes_used()?
      + self.docs_with_field.ram_bytes_used()?;
    let delta = new_bytes_used - self.bytes_used;
    if delta != 0 {
      self.iw_bytes_used.add_and_get(delta);
    }
    self.bytes_used = new_bytes_used;
    Ok(())
  }
}

impl Display for BinaryDocValuesWriter {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl DocValuesWriter for BinaryDocValuesWriter {
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
    self.bytes_out.paged_bytes.freeze(false)?;
    if let BinaryLengths::Pending(lengths) = &mut self.lengths {
      self.lengths = BinaryLengths::Finished(lengths.build()?);
    }
    let BinaryLengths::Finished(final_lengths) = &self.lengths else {
      return Err(LuceneError::illegal_state(
        "final lengths are unavailable after they were built",
      ));
    };
    let sorted = match sort_map {
      Some(sort_map) => {
        let mut buffered_binary_doc_values = BufferedBinaryDocValues::new(
          || final_lengths,
          self.max_length,
          get_data_input(&self.bytes_out.paged_bytes)?,
          self.docs_with_field.iterator()?,
        )?;
        Some(BinaryDVs::new(
          segment_info.max_doc()?.try_convert()?,
          sort_map,
          &mut buffered_binary_doc_values,
        )?)
      },
      None => None,
    };
    let producer = DocValuesProducerImpl::new(
      &self.field_info,
      final_lengths,
      self.max_length,
      &self.bytes_out.paged_bytes,
      &self.docs_with_field,
      sorted.as_ref(),
    )?;
    dv_consumer.add_binary_field(write_state, segment_info, &self.field_info, &producer)
  }

  type DocIdSetIterator = BufferedBinaryDocValues<DocsWithFieldSetDISI, PagedBytesDataInput>;

  fn get_doc_values(&self) -> Result<Self::DocIdSetIterator> {
    let BinaryLengths::Finished(final_lengths) = &self.lengths else {
      return Err(LuceneError::illegal_state(
        "must be finished before getting doc values".to_string(),
      ));
    };
    BufferedBinaryDocValues::new(
      || final_lengths.clone(),
      self.max_length,
      get_data_input(&self.bytes_out.paged_bytes)?,
      self.docs_with_field.iterator()?,
    )
  }

  fn finish(&mut self, _pool: &Arc<ByteBlockPool>) -> Result<()> {
    self.docs_with_field.finish();
    if let BinaryLengths::Pending(lengths) = &mut self.lengths {
      self.lengths = BinaryLengths::Finished(lengths.build()?);
    }
    Ok(())
  }
}

pub(crate) struct DocValuesProducerImpl<'a> {
  field_info: &'a Arc<FieldInfo>,
  final_lengths: &'a PackedLongValues,
  max_length: usize,
  paged_bytes: &'a PagedBytes,
  docs_with_field: &'a DocsWithFieldSet,
  sorted: Option<&'a BinaryDVs>,
}

impl CloseableRef for DocValuesProducerImpl<'_> {
  fn close(&self) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }
}

impl<'a> DocValuesProducerImpl<'a> {
  pub(crate) fn new(
    field_info: &'a Arc<FieldInfo>,
    final_lengths: &'a PackedLongValues,
    max_length: usize,
    paged_bytes: &'a PagedBytes,
    docs_with_field: &'a DocsWithFieldSet,
    sorted: Option<&'a BinaryDVs>,
  ) -> Result<Self> {
    Ok(Self {
      field_info,
      final_lengths,
      max_length,
      paged_bytes,
      docs_with_field,
      sorted,
    })
  }
}

pub(crate) enum BufferedSortingBinaryDocValues<V = PackedLongValues, D = Arc<BinaryDVs>> {
  Buffered(BufferedBinaryDocValues<DocsWithFieldSetDISI, PagedBytesDataInput, V>),
  Sorting(SortingBinaryDocValues<D>),
}

impl<V: Borrow<PackedLongValues>, D: Borrow<BinaryDVs>> DocValuesIterator
  for BufferedSortingBinaryDocValues<V, D>
{
  fn advance_exact(&mut self, _target: i32) -> Result<bool> {
    match self {
      Self::Buffered(inner) => inner.advance_exact(_target),
      Self::Sorting(inner) => inner.advance_exact(_target),
    }
  }
}

impl<V: Borrow<PackedLongValues>, D: Borrow<BinaryDVs>>
  crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for BufferedSortingBinaryDocValues<V, D>
{
}
impl<V: Borrow<PackedLongValues>, D: Borrow<BinaryDVs>>
  crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for BufferedSortingBinaryDocValues<V, D>
{
}

impl<V: Borrow<PackedLongValues>, D: Borrow<BinaryDVs>> DocIdSetIterator
  for BufferedSortingBinaryDocValues<V, D>
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::Buffered(inner) => inner.doc_id(),
      Self::Sorting(inner) => inner.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::Buffered(inner) => inner.next_doc(),
      Self::Sorting(inner) => inner.next_doc(),
    }
  }

  fn advance(&mut self, _target: i32) -> Result<i32> {
    match self {
      Self::Buffered(inner) => inner.advance(_target),
      Self::Sorting(inner) => inner.advance(_target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::Buffered(inner) => inner.cost(),
      Self::Sorting(inner) => inner.cost(),
    }
  }
}

impl<V: Borrow<PackedLongValues>, D: Borrow<BinaryDVs>> BinaryDocValues
  for BufferedSortingBinaryDocValues<V, D>
{
  type Value<'a>
    = BytesRefValueEnum<'a>
  where
    Self: 'a;

  fn binary_value(&self) -> Result<Self::Value<'_>> {
    match self {
      Self::Buffered(inner) => inner
        .binary_value()
        .map(|value| BytesRefValueEnum::Buffer(std::borrow::Cow::Borrowed(value))),
      Self::Sorting(inner) => inner.binary_value(),
    }
  }
}

impl<'a> DocValuesProducer for DocValuesProducerImpl<'a> {
  type NumericDocValues = DummyNumericDocValues;
  type BinaryDocValues = BufferedSortingBinaryDocValues<&'a PackedLongValues, &'a BinaryDVs>;

  fn get_binary(&self, field_info: &Arc<FieldInfo>) -> Result<Self::BinaryDocValues> {
    if !Arc::ptr_eq(field_info, self.field_info) {
      return Err(LuceneError::illegal_argument("wrong fieldInfo"));
    }
    match self.sorted {
      Some(sorted) => Ok(BufferedSortingBinaryDocValues::Sorting(
        SortingBinaryDocValues::new(sorted),
      )),
      None => Ok(BufferedSortingBinaryDocValues::Buffered(
        BufferedBinaryDocValues::new(
          || self.final_lengths,
          self.max_length,
          get_data_input(self.paged_bytes)?,
          self.docs_with_field.iterator()?,
        )?,
      )),
    }
  }

  type SortedDocValues = DummySortedDocValues;
  type SortedNumericDocValues = DummySortedNumericDocValues;
  type SortedSetDocValues = DummySortedSetDocValues;
  type DocValuesSkipper = DummyDocValuesSkipper;
}

// iterates over the values we have in ram
pub(crate) struct BufferedBinaryDocValues<D, DI, V = PackedLongValues> {
  value: BytesRefBuilder<Vec<u8>>,
  lengths_iterator: PackedLongValuesIterator<V>,
  docs_with_field: D,
  bytes_iter: DI,
}

impl<D, DI, V: Borrow<PackedLongValues>> BufferedBinaryDocValues<D, DI, V> {
  pub(crate) fn new(
    lengths: impl FnOnce() -> V,
    max_length: usize,
    bytes_iter: DI,
    docs_with_field: D,
  ) -> Result<Self> {
    let mut value = BytesRefBuilder::new();
    value.grow(max_length)?;
    // Acquire owned lengths only after grow succeeds, preserving the original clone order.
    Ok(Self {
      value,
      lengths_iterator: PackedLongValuesIterator::new(lengths())?,
      docs_with_field,
      bytes_iter,
    })
  }
}

impl<D, DI, V: Borrow<PackedLongValues>>
  crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for BufferedBinaryDocValues<D, DI, V>
where
  D: DocIdSetIterator,
  DI: DataInput,
{
}
impl<D, DI, V: Borrow<PackedLongValues>>
  crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for BufferedBinaryDocValues<D, DI, V>
where
  D: DocIdSetIterator,
  DI: DataInput,
{
}

impl<D, DI, V: Borrow<PackedLongValues>> DocIdSetIterator for BufferedBinaryDocValues<D, DI, V>
where
  D: DocIdSetIterator,
  DI: DataInput,
{
  fn doc_id(&self) -> i32 {
    self.docs_with_field.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    let doc_id = self.docs_with_field.next_doc()?;
    if doc_id != NO_MORE_DOCS {
      let length = self.lengths_iterator.next_value()?.try_convert()?;
      self.value.set_length(length);
      self
        .bytes_iter
        .read_bytes(&mut self.value.bytes_ref.bytes, 0, length)?;
    }
    Ok(doc_id)
  }

  fn advance(&mut self, _target: i32) -> Result<i32> {
    Err(LuceneError::unsupported_operation("use next_doc instead"))
  }

  fn cost(&self) -> Result<i64> {
    self.docs_with_field.cost()
  }
}

impl<D, DI, V: Borrow<PackedLongValues>> DocValuesIterator for BufferedBinaryDocValues<D, DI, V>
where
  D: DocIdSetIterator,
  DI: DataInput,
{
  fn advance_exact(&mut self, _target: i32) -> Result<bool> {
    Err(LuceneError::unsupported_operation("use next_doc instead"))
  }
}

impl<D, DI, V: Borrow<PackedLongValues>> BinaryDocValues for BufferedBinaryDocValues<D, DI, V>
where
  D: DocIdSetIterator,
  DI: DataInput,
{
  type Value<'a>
    = &'a BytesRef<Vec<u8>>
  where
    Self: 'a;

  fn binary_value(&self) -> Result<Self::Value<'_>> {
    Ok(self.value.get_bytes_ref())
  }
}

pub struct SortingBinaryDocValues<D = Arc<BinaryDVs>> {
  dvs: D,
  doc_id: i32,
}

impl<D: Borrow<BinaryDVs>> SortingBinaryDocValues<D> {
  pub(crate) fn new(dvs: D) -> Self {
    Self { dvs, doc_id: -1 }
  }
}

impl<D: Borrow<BinaryDVs>> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SortingBinaryDocValues<D>
{
}
impl<D: Borrow<BinaryDVs>> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for SortingBinaryDocValues<D>
{
}

impl<D: Borrow<BinaryDVs>> DocIdSetIterator for SortingBinaryDocValues<D> {
  fn doc_id(&self) -> i32 {
    self.doc_id
  }

  fn next_doc(&mut self) -> Result<i32> {
    loop {
      self.doc_id += 1;
      let doc_index = self.doc_id as usize;
      if doc_index == self.dvs.borrow().offsets.len() {
        self.doc_id = NO_MORE_DOCS;
        break;
      }
      if self.dvs.borrow().offsets[doc_index] > 0 {
        break;
      }
    }
    Ok(self.doc_id)
  }

  fn advance(&mut self, _target: i32) -> Result<i32> {
    Err(LuceneError::unsupported_operation("use next_doc instead"))
  }

  fn cost(&self) -> Result<i64> {
    Ok(self.dvs.borrow().values.size() as i64)
  }
}

impl<D: Borrow<BinaryDVs>> DocValuesIterator for SortingBinaryDocValues<D> {
  fn advance_exact(&mut self, _target: i32) -> Result<bool> {
    Err(LuceneError::unsupported_operation("use next_doc instead"))
  }
}

impl<D: Borrow<BinaryDVs>> BinaryDocValues for SortingBinaryDocValues<D> {
  type Value<'a>
    = crate::core::index::BytesRefValueEnum<'a>
  where
    Self: 'a;

  fn binary_value(&self) -> Result<Self::Value<'_>> {
    let idx = self.dvs.borrow().offsets[self.doc_id as usize] as usize - 1;
    self.dvs.borrow().values.get(idx)
  }
}

pub struct BinaryDVs {
  pub(crate) offsets: Vec<i32>,
  pub(crate) values: BytesRefArray,
}

impl BinaryDVs {
  pub(crate) fn new<DM, T>(max_doc: usize, sort_map: &DM, old_values: &mut T) -> Result<Self>
  where
    DM: DocMap,
    T: BinaryDocValues,
  {
    let mut offsets = vec![0; max_doc];
    let counter = Arc::new(AtomicCounter::new());
    let mut values = BytesRefArray::new(counter)?;
    let mut offset = 1; // 0 means missing, and there is at most one value per doc.
    let mut doc_id;
    loop {
      doc_id = old_values.next_doc()?;
      if doc_id == NO_MORE_DOCS {
        break;
      }
      let new_doc = sort_map.old_to_new(doc_id)?.try_convert()?;
      let val = old_values.binary_value()?;
      values.append(&val.as_bytes_ref())?;
      offsets[new_doc] = offset;
      offset += 1;
    }
    Ok(BinaryDVs { offsets, values })
  }
}

use crate::core::util::array_util::ArrayUtil;

// 4 kB block sizes for PagedBytes storage:
const BLOCK_BITS: usize = 12;
const MAX_LENGTH: usize = ArrayUtil::MAX_ARRAY_LENGTH;
