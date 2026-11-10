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
use crate::core::codecs::dummy::dummy_binary_doc_values::DummyBinaryDocValues;
use crate::core::codecs::dummy::dummy_doc_values_skipper::DummyDocValuesSkipper;
use crate::core::codecs::dummy::dummy_sorted_doc_values::DummySortedDocValues;
use crate::core::codecs::dummy::dummy_sorted_numeric_doc_values::DummySortedNumericDocValues;
use crate::core::codecs::dummy::dummy_sorted_set_doc_values::DummySortedSetDocValues;
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::doc_values_writer::DocValuesWriter;
use crate::core::index::docs_with_field_set::{DocsWithFieldSet, DocsWithFieldSetDISI};
use crate::core::index::field_info::FieldInfo;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorter::DocMap;
use crate::core::search::doc_id_set::DocIdSet;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::store::directory::Directory;
use crate::core::util::accountable::Accountable;
use crate::core::util::bit_set::BitSet;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::packed::PackedInts;
use crate::core::util::packed::packed_long_values::{
  Builder, PackedLongValues, PackedLongValuesIterator,
};
use crate::core::util::{ByteBlockPool, Counter, SharedCounter};
use std::borrow::Borrow;
use std::cell::Cell;
use std::fmt::{Display, Formatter};
use std::marker::PhantomData;
use std::sync::Arc;

/// Buffers pending `i64` values per document, then writes them during segment flush.
pub(crate) struct NumericDocValuesWriter {
  values: NumericValues,
  iw_bytes_used: SharedCounter,
  bytes_used: i64,
  docs_with_field: DocsWithFieldSet,
  field_info: Arc<FieldInfo>,
  last_doc_id: i32,
}

enum NumericValues {
  Pending(Builder),
  Finished(PackedLongValues),
}

impl NumericDocValuesWriter {
  pub(crate) fn new(field_info: Arc<FieldInfo>, iw_bytes_used: SharedCounter) -> Result<Self> {
    let pending = PackedLongValues::delta_packed_long_values_builder_default(PackedInts::COMPACT)?;
    let docs_with_field = DocsWithFieldSet::new();
    let bytes_used = pending.ram_bytes_used()? + docs_with_field.ram_bytes_used()?;

    iw_bytes_used.add_and_get(bytes_used);

    Ok(Self {
      values: NumericValues::Pending(pending),
      iw_bytes_used,
      bytes_used,
      docs_with_field,
      field_info,
      last_doc_id: -1,
    })
  }

  pub(crate) fn add_value(&mut self, doc_id: i32, value: i64) -> Result<()> {
    if doc_id <= self.last_doc_id {
      return Err(LuceneError::illegal_argument(format!(
        "DocValuesField \"{}\" appears more than once in this document (only one value is allowed per field)",
        self.field_info.name
      )));
    }

    let NumericValues::Pending(pending) = &mut self.values else {
      return Err(LuceneError::illegal_state(
        "DocsWithFieldSet must not be changed after finish() is called",
      ));
    };
    pending.add(value)?;
    self.docs_with_field.add(doc_id)?;
    let new_bytes_used = pending.ram_bytes_used()? + self.docs_with_field.ram_bytes_used()?;
    let delta = new_bytes_used - self.bytes_used;
    if delta != 0 {
      self.iw_bytes_used.add_and_get(delta);
    }
    self.bytes_used = new_bytes_used;
    self.last_doc_id = doc_id;
    Ok(())
  }
}

impl Display for NumericDocValuesWriter {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl DocValuesWriter for NumericDocValuesWriter {
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
    if let NumericValues::Pending(pending) = &mut self.values {
      self.values = NumericValues::Finished(std::mem::take(pending).build()?);
    }
    let NumericValues::Finished(final_values) = &self.values else {
      return Err(LuceneError::illegal_state(
        "final numeric values are unavailable after they were built",
      ));
    };
    let sorted = get_sorted_values(final_values, &self.docs_with_field, sort_map)?;
    match sorted.as_ref() {
      Some(sorted) => {
        let producer = DocValuesProducerImpl::new(&self.field_info, || {
          Ok(SortingNumericDocValues::new(sorted))
        });
        dv_consumer.add_numeric_field(write_state, segment_info, &self.field_info, &producer)?;
      },
      None if self.docs_with_field.is_dense() => {
        let producer = DocValuesProducerImpl::new(&self.field_info, || {
          BufferedNumericDocValues::new(final_values, self.docs_with_field.dense_iterator()?)
        });
        dv_consumer.add_numeric_field(write_state, segment_info, &self.field_info, &producer)?;
      },
      None => {
        let producer = DocValuesProducerImpl::new(&self.field_info, || {
          BufferedNumericDocValues::new(final_values, self.docs_with_field.sparse_iterator()?)
        });
        dv_consumer.add_numeric_field(write_state, segment_info, &self.field_info, &producer)?;
      },
    }
    Ok(())
  }

  type DocIdSetIterator = BufferedNumericDocValues;

  fn get_doc_values(&self) -> Result<Self::DocIdSetIterator> {
    let NumericValues::Finished(final_values) = &self.values else {
      return Err(LuceneError::illegal_state(
        "must be finished before getting doc values",
      ));
    };
    let iter = self.docs_with_field.iterator()?;
    BufferedNumericDocValues::new(final_values.clone(), iter)
  }

  fn finish(&mut self, _pool: &Arc<ByteBlockPool>) -> Result<()> {
    self.docs_with_field.finish();
    if let NumericValues::Pending(pending) = &mut self.values {
      self.values = NumericValues::Finished(std::mem::take(pending).build()?);
    }
    Ok(())
  }
}

pub(crate) struct DocValuesProducerImpl<'a, F> {
  writer_field_info: &'a Arc<FieldInfo>,
  make_values: F,
}

impl<F> CloseableRef for DocValuesProducerImpl<'_, F> {
  fn close(&self) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }
}

impl<'a, F> DocValuesProducerImpl<'a, F> {
  pub(crate) fn new(writer_field_info: &'a Arc<FieldInfo>, make_values: F) -> Self {
    Self {
      writer_field_info,
      make_values,
    }
  }
}
impl<F, V> DocValuesProducer for DocValuesProducerImpl<'_, F>
where
  F: Fn() -> Result<V>,
  V: NumericDocValues,
{
  type NumericDocValues = V;

  fn get_numeric(&self, field_info: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
    if !Arc::ptr_eq(field_info, self.writer_field_info) {
      return Err(LuceneError::illegal_argument("wrong fieldInfo"));
    }
    (self.make_values)()
  }

  type BinaryDocValues = DummyBinaryDocValues;
  type SortedDocValues = DummySortedDocValues;
  type SortedNumericDocValues = DummySortedNumericDocValues;
  type SortedSetDocValues = DummySortedSetDocValues;
  type DocValuesSkipper = DummyDocValuesSkipper;
}

// iterates over the values we have in ram
pub(crate) struct BufferedNumericDocValues<V = PackedLongValues, D = DocsWithFieldSetDISI> {
  iter: PackedLongValuesIterator<V>,
  doc_with_field: D,
  value: i64,
}
impl<V: Borrow<PackedLongValues>, D: DocIdSetIterator> BufferedNumericDocValues<V, D> {
  pub(crate) fn new(values: V, doc_with_field: D) -> Result<Self> {
    Ok(Self {
      iter: PackedLongValuesIterator::new(values)?,
      doc_with_field,
      value: 0,
    })
  }
}

impl<V: Borrow<PackedLongValues>, D: DocIdSetIterator> DocValuesIterator
  for BufferedNumericDocValues<V, D>
{
  fn advance_exact(&mut self, _target: i32) -> Result<bool> {
    Err(LuceneError::unsupported_operation(""))
  }
}

impl<V: Borrow<PackedLongValues>, D: DocIdSetIterator>
  crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for BufferedNumericDocValues<V, D>
{
}
impl<V: Borrow<PackedLongValues>, D: DocIdSetIterator>
  crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for BufferedNumericDocValues<V, D>
{
}

impl<V: Borrow<PackedLongValues>, D: DocIdSetIterator> DocIdSetIterator
  for BufferedNumericDocValues<V, D>
{
  fn doc_id(&self) -> i32 {
    self.doc_with_field.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    let doc_id = self.doc_with_field.next_doc()?;
    if doc_id != NO_MORE_DOCS {
      self.value = self.iter.next_value()?;
    }
    Ok(doc_id)
  }

  fn advance(&mut self, _target: i32) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn cost(&self) -> Result<i64> {
    self.doc_with_field.cost()
  }
}

impl<V: Borrow<PackedLongValues>, D: DocIdSetIterator> NumericDocValues
  for BufferedNumericDocValues<V, D>
{
  fn long_value(&mut self) -> Result<i64> {
    Ok(self.value)
  }
}

pub struct SortingNumericDocValues<T, D = Arc<NumericDVs<T>>> {
  dvs: D,
  marker: PhantomData<T>,
  doc_id: i32,
  cost: Cell<i64>,
}

impl<T, D> SortingNumericDocValues<T, D> {
  pub(crate) fn new(dvs: D) -> Self {
    Self {
      dvs,
      marker: PhantomData,
      doc_id: -1,
      cost: Cell::new(-1),
    }
  }
}

impl<T, D> DocValuesIterator for SortingNumericDocValues<T, D>
where
  T: BitSet,
  D: Borrow<NumericDVs<T>>,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    // needed in IndexSorter#{Long|Int|Double|Float}Sorter
    self.doc_id = target;
    Ok(self.dvs.borrow().advance_exact(target))
  }
}

impl<T, D> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SortingNumericDocValues<T, D>
where
  T: BitSet,
  D: Borrow<NumericDVs<T>>,
{
}
impl<T, D> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for SortingNumericDocValues<T, D>
where
  T: BitSet,
  D: Borrow<NumericDVs<T>>,
{
}

impl<T, D> DocIdSetIterator for SortingNumericDocValues<T, D>
where
  T: BitSet,
  D: Borrow<NumericDVs<T>>,
{
  fn doc_id(&self) -> i32 {
    self.doc_id
  }

  fn next_doc(&mut self) -> Result<i32> {
    if self.doc_id + 1 == self.dvs.borrow().max_doc() {
      self.doc_id = NO_MORE_DOCS;
    } else {
      self.doc_id = self.dvs.borrow().advance(self.doc_id + 1);
    }
    Ok(self.doc_id)
  }

  fn advance(&mut self, _target: i32) -> Result<i32> {
    Err(LuceneError::unsupported_operation("use nextDoc() instead"))
  }

  fn cost(&self) -> Result<i64> {
    if self.cost.get() == -1 {
      self.cost.set(self.dvs.borrow().cost());
    }
    Ok(self.cost.get())
  }
}

impl<T, D> NumericDocValues for SortingNumericDocValues<T, D>
where
  T: BitSet,
  D: Borrow<NumericDVs<T>>,
{
  fn long_value(&mut self) -> Result<i64> {
    Ok(self.dvs.borrow().values[self.doc_id as usize])
  }
}

pub struct NumericDVs<T> {
  pub values: Vec<i64>,
  pub docs_with_field: Option<T>,
  pub max_doc: i32,
}

impl<T> NumericDVs<T> {
  pub(crate) fn new(values: Vec<i64>, docs_with_field: Option<T>) -> Self {
    debug_assert!(values.len() <= i32::MAX as usize);
    let max_doc = values.len() as i32;
    Self {
      values,
      docs_with_field,
      max_doc,
    }
  }

  pub(crate) fn max_doc(&self) -> i32 {
    self.max_doc
  }
}

impl<T> NumericDVs<T>
where
  T: BitSet,
{
  fn advance_exact(&self, target: i32) -> bool {
    match &self.docs_with_field {
      Some(bits) => bits.get(target as usize).unwrap_or(false),
      None => true,
    }
  }
  pub(crate) fn advance(&self, target: i32) -> i32 {
    if let Some(bits) = &self.docs_with_field {
      bits.next_set_bit(target as usize) as i32
    } else {
      // Only called when target is less than maxDoc
      target
    }
  }
  pub(crate) fn cost(&self) -> i64 {
    match &self.docs_with_field {
      Some(bits) => bits.cardinality() as i64,
      None => self.max_doc as i64,
    }
  }
}

pub(crate) fn sort_doc_values<DV, M>(
  max_doc: i32,
  sort_map: &M,
  old_doc_values: &mut DV,
  dense: bool,
) -> Result<NumericDVs<FixedBitSet>>
where
  DV: NumericDocValues,
  M: DocMap,
{
  let max_doc = max_doc as usize;
  let mut docs_with_field = if !dense {
    Some(FixedBitSet::new(max_doc))
  } else {
    None
  };

  let mut values = vec![0i64; max_doc];

  loop {
    let doc_id = old_doc_values.next_doc()?;
    if doc_id == NO_MORE_DOCS {
      break;
    }

    let new_doc_id = sort_map.old_to_new(doc_id)? as usize;
    if let Some(bits) = &mut docs_with_field {
      bits.set(new_doc_id)?;
    }

    values[new_doc_id] = old_doc_values.long_value()?;
  }
  Ok(NumericDVs::new(values, docs_with_field))
}

pub(crate) fn get_sorted_values<DM>(
  values: &PackedLongValues,
  docs_with_field: &DocsWithFieldSet,
  sort_map: Option<&DM>,
) -> Result<Option<NumericDVs<FixedBitSet>>>
where
  DM: DocMap,
{
  let sorter = if let Some(sort_map) = sort_map {
    let dense = sort_map.size() == docs_with_field.cardinality();
    let iter = docs_with_field.iterator()?;
    let mut old_values = BufferedNumericDocValues::new(values, iter)?;
    let sorted = sort_doc_values(sort_map.size(), sort_map, &mut old_values, dense)?;
    Some(sorted)
  } else {
    None
  };
  Ok(sorter)
}
