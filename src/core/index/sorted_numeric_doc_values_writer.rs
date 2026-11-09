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
use crate::core::codecs::dummy::dummy_numeric_doc_values::DummyNumericDocValues;
use crate::core::codecs::dummy::dummy_sorted_doc_values::DummySortedDocValues;
use crate::core::codecs::dummy::dummy_sorted_set_doc_values::DummySortedSetDocValues;
use crate::core::index::doc_values::DocValues;
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::doc_values_writer::DocValuesWriter;
use crate::core::index::docs_with_field_set::{DocsWithFieldSet, DocsWithFieldSetDISI};
use crate::core::index::field_info::FieldInfo;
use crate::core::index::numeric_doc_values_writer::{
  BufferedNumericDocValues, BufferedSortingNumericDocValues, DocValuesProducerImpl, NumericDVs,
  get_sorted_values,
};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::singleton_sorted_numeric_doc_values::SingletonSortedNumericDocValues;
use crate::core::index::sorted_numeric_doc_values::SortedNumericDocValues;
use crate::core::index::sorter::DocMap;
use crate::core::search::doc_id_set::DocIdSet;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::store::directory::Directory;
use crate::core::util::accountable::Accountable;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::long_values::LongValues as OtherLongValues;
use crate::core::util::packed::PackedInts;
use crate::core::util::packed::packed_long_values::{
  Builder, PackedLongValues, PackedLongValuesIterator,
};
use crate::core::util::ram_usage_estimator::size_of_array_capacity;
use crate::core::util::{ByteBlockPool, Counter, SharedCounter, TryIntoInt};
use std::borrow::Borrow;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

type BufferedSingleSortedNumericDocValues =
  SingletonSortedNumericDocValues<BufferedNumericDocValues>;
type BufferedMultiSortedNumericDocValues<P = PackedLongValues> =
  BufferedSortedNumericDocValues<DocsWithFieldSetDISI, P>;

pub(crate) enum SortedNumericDocValuesWriterValues<P = PackedLongValues, V = LongValues> {
  Single(BufferedSingleSortedNumericDocValues),
  Multi(BufferedMultiSortedNumericDocValues<P>),
  SortedMulti(SortingSortedNumericDocValues<BufferedMultiSortedNumericDocValues<P>, V>),
}

impl<P: Borrow<PackedLongValues>, V: Borrow<LongValues>> DocValuesIterator
  for SortedNumericDocValuesWriterValues<P, V>
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::Single(values) => values.advance_exact(target),
      Self::Multi(values) => values.advance_exact(target),
      Self::SortedMulti(values) => values.advance_exact(target),
    }
  }
}

impl<P: Borrow<PackedLongValues>, V: Borrow<LongValues>>
  crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SortedNumericDocValuesWriterValues<P, V>
{
}
impl<P: Borrow<PackedLongValues>, V: Borrow<LongValues>>
  crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for SortedNumericDocValuesWriterValues<P, V>
{
}

impl<P: Borrow<PackedLongValues>, V: Borrow<LongValues>> DocIdSetIterator
  for SortedNumericDocValuesWriterValues<P, V>
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::Single(values) => values.doc_id(),
      Self::Multi(values) => values.doc_id(),
      Self::SortedMulti(values) => values.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::Single(values) => values.next_doc(),
      Self::Multi(values) => values.next_doc(),
      Self::SortedMulti(values) => values.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Single(values) => values.advance(target),
      Self::Multi(values) => values.advance(target),
      Self::SortedMulti(values) => values.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Single(values) => values.slow_advance(target),
      Self::Multi(values) => values.slow_advance(target),
      Self::SortedMulti(values) => values.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::Single(values) => values.cost(),
      Self::Multi(values) => values.cost(),
      Self::SortedMulti(values) => values.cost(),
    }
  }
}

impl<P: Borrow<PackedLongValues>, V: Borrow<LongValues>> SortedNumericDocValues
  for SortedNumericDocValuesWriterValues<P, V>
{
  fn next_value(&mut self) -> Result<i64> {
    match self {
      Self::Single(values) => values.next_value(),
      Self::Multi(values) => values.next_value(),
      Self::SortedMulti(values) => values.next_value(),
    }
  }

  fn doc_value_count(&mut self) -> Result<i32> {
    match self {
      Self::Single(values) => values.doc_value_count(),
      Self::Multi(values) => values.doc_value_count(),
      Self::SortedMulti(values) => values.doc_value_count(),
    }
  }

  fn is_single_valued(&self) -> bool {
    match self {
      Self::Single(values) => values.is_single_valued(),
      Self::Multi(values) => values.is_single_valued(),
      Self::SortedMulti(values) => values.is_single_valued(),
    }
  }

  type NumericDocValues = BufferedNumericDocValues;

  fn get_numeric_doc_values(&mut self) -> Result<Self::NumericDocValues> {
    match self {
      Self::Single(values) => values.get_numeric_doc_values(),
      Self::Multi(_) | Self::SortedMulti(_) => Err(LuceneError::unsupported_operation("")),
    }
  }
}

/// Buffers up pending `[i64]` per doc, sorts, then flushes when segment flushes.
pub(crate) struct SortedNumericDocValuesWriter {
  values: SortedNumericValues,         // stream of all values
  counts: Option<SortedNumericCounts>, // count of values per doc
  docs_with_field: DocsWithFieldSet,
  iw_bytes_used: SharedCounter,
  bytes_used: i64, // this only tracks differences in 'pending' and 'pendingCounts'
  field_info: Arc<FieldInfo>,
  current_doc: i32,
  current_values: smallvec::SmallVec<[i64; 4]>,
  current_upto: usize,
}

enum SortedNumericCounts {
  Pending(Builder),
  Finished(PackedLongValues),
}

enum SortedNumericValues {
  Pending(Builder),
  Finished(PackedLongValues),
}

impl SortedNumericDocValuesWriter {
  pub(crate) fn new(field_info: Arc<FieldInfo>, iw_bytes_used: SharedCounter) -> Result<Self> {
    let current_values: smallvec::SmallVec<[i64; 4]> = smallvec::smallvec![0; 4];
    let docs_with_field = DocsWithFieldSet::new();
    let pending = PackedLongValues::delta_packed_long_values_builder_default(PackedInts::COMPACT)?;

    let bytes_used = pending.ram_bytes_used()?
      + docs_with_field.ram_bytes_used()?
      + size_of_array_capacity::<i64>(if current_values.spilled() {
        current_values.capacity()
      } else {
        0
      });

    iw_bytes_used.add_and_get(bytes_used);

    Ok(Self {
      values: SortedNumericValues::Pending(pending),
      counts: None,
      docs_with_field,
      iw_bytes_used,
      bytes_used,
      field_info,
      current_doc: -1,
      current_values,
      current_upto: 0,
    })
  }

  pub(crate) fn add_value(&mut self, doc_id: i32, value: i64) -> Result<()> {
    debug_assert!(doc_id >= self.current_doc);
    if doc_id != self.current_doc {
      self.finish_current_doc()?;
      self.current_doc = doc_id;
    }
    self.add_one_value(value)?;
    self.update_bytes_used()?;
    Ok(())
  }
  // finalize currentDoc: this sorts the values in the current doc
  fn finish_current_doc(&mut self) -> Result<()> {
    if self.current_doc == -1 {
      return Ok(());
    }
    let current_values = &mut self.current_values[..self.current_upto];
    if current_values.len() > 1 {
      current_values.sort_unstable();
    }
    for &value in current_values.iter() {
      let SortedNumericValues::Pending(pending) = &mut self.values else {
        return Err(LuceneError::illegal_state("Cannot be reused after build()"));
      };
      pending.add(value)?;
    }
    // record the number of values for this doc
    if let Some(counts) = self.counts.as_mut() {
      let SortedNumericCounts::Pending(counts) = counts else {
        return Err(LuceneError::illegal_state("Cannot be reused after build()"));
      };
      counts.add(self.current_upto as i64)?;
    } else if self.current_upto != 1 {
      let mut pending_counts =
        PackedLongValues::delta_packed_long_values_builder_default(PackedInts::COMPACT)?;
      for _ in 0..self.docs_with_field.cardinality() {
        pending_counts.add(1)?;
      }
      pending_counts.add(self.current_upto as i64)?;
      self.counts = Some(SortedNumericCounts::Pending(pending_counts));
    }
    self.current_upto = 0;
    self.docs_with_field.add(self.current_doc)?;
    Ok(())
  }

  fn add_one_value(&mut self, value: i64) -> Result<()> {
    // The initialized prefix is retained across documents. Only current_upto
    // counts the values that belong to this document.
    if let Some(slot) = self.current_values.get_mut(self.current_upto) {
      *slot = value;
    } else {
      if self.current_values.len() == self.current_values.capacity() {
        ArrayUtil::oversize(self.current_upto + 1, std::mem::size_of::<i64>())?;
        self.current_values.reserve(1);
      }
      self.current_values.push(value);
    }
    self.current_upto += 1;
    Ok(())
  }

  fn update_bytes_used(&mut self) -> Result<()> {
    let pending_counts_usage = match &self.counts {
      Some(SortedNumericCounts::Pending(counts)) => counts.ram_bytes_used()?,
      _ => 0,
    };
    let SortedNumericValues::Pending(pending) = &self.values else {
      return Err(LuceneError::illegal_state("Cannot be reused after build()"));
    };
    let new_bytes_used = pending.ram_bytes_used()?
      + pending_counts_usage
      + self.docs_with_field.ram_bytes_used()?
      + size_of_array_capacity::<i64>(if self.current_values.spilled() {
        self.current_values.capacity()
      } else {
        0
      });

    let delta = new_bytes_used - self.bytes_used;
    if delta != 0 {
      self.iw_bytes_used.add_and_get(delta);
    }
    self.bytes_used = new_bytes_used;
    Ok(())
  }

  pub(crate) fn get_values(
    values: &PackedLongValues,
    value_counts: Option<&PackedLongValues>,
    docs_with_field: &DocsWithFieldSet,
  ) -> Result<SortedNumericDocValuesWriterValues> {
    let iter = docs_with_field.iterator()?;

    match value_counts {
      None => {
        let dv = BufferedNumericDocValues::new(values.clone(), iter)?;
        Ok(SortedNumericDocValuesWriterValues::Single(
          DocValues::singleton_numeric(dv)?,
        ))
      },
      Some(value_counts) => {
        let dv =
          BufferedSortedNumericDocValues::new(|| values.clone(), || value_counts.clone(), iter)?;
        Ok(SortedNumericDocValuesWriterValues::Multi(dv))
      },
    }
  }
}

impl Display for SortedNumericDocValuesWriter {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl DocValuesWriter for SortedNumericDocValuesWriter {
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
    // finish() normally provides the values before flush().
    if matches!(self.values, SortedNumericValues::Pending(_)) {
      self.finish_current_doc()?;
    }
    let built_values;
    let built_value_counts;
    let (values, value_counts) = match &mut self.values {
      SortedNumericValues::Finished(values) => {
        let counts = match &self.counts {
          Some(SortedNumericCounts::Finished(counts)) => Some(counts),
          _ => None,
        };
        (&*values, counts)
      },
      SortedNumericValues::Pending(pending) => {
        built_values = pending.build()?;
        built_value_counts = match &mut self.counts {
          Some(SortedNumericCounts::Pending(counts)) => Some(counts.build()?),
          _ => None,
        };
        (&built_values, built_value_counts.as_ref())
      },
    };
    let Some(value_counts) = value_counts else {
      let sorted = get_sorted_values(values, &self.docs_with_field, sort_map)?;
      let single_value_producer = DocValuesProducerImpl::new(
        sorted.as_ref(),
        &self.docs_with_field,
        values,
        &self.field_info,
      );
      let producer = DocValuesProducerImpl1::new(single_value_producer)?;
      dv_consumer.add_sorted_numeric_field(
        write_state,
        segment_info,
        &self.field_info,
        &producer,
      )?;
      return Ok(());
    };
    let sorted = if let Some(sort_map) = sort_map {
      let mut v = BufferedSortedNumericDocValues::new(
        || values,
        || value_counts,
        self.docs_with_field.iterator()?,
      )?;
      Some(LongValues::new(
        segment_info.max_doc()? as usize,
        sort_map,
        &mut v,
        PackedInts::FASTEST,
      )?)
    } else {
      None
    };

    let producer = DocValuesProducerImpl2::new(
      &self.field_info,
      &self.docs_with_field,
      values,
      sorted.as_ref(),
      value_counts,
    )?;
    dv_consumer.add_sorted_numeric_field(write_state, segment_info, &self.field_info, &producer)
  }

  type DocIdSetIterator = SortedNumericDocValuesWriterValues;

  fn get_doc_values(&self) -> Result<Self::DocIdSetIterator> {
    let SortedNumericValues::Finished(final_values) = &self.values else {
      return Err(LuceneError::illegal_state(
        "must be finished before getting doc values",
      ));
    };
    SortedNumericDocValuesWriter::get_values(
      final_values,
      match &self.counts {
        Some(SortedNumericCounts::Finished(counts)) => Some(counts),
        _ => None,
      },
      &self.docs_with_field,
    )
  }

  fn finish(&mut self, _pool: &Arc<ByteBlockPool>) -> Result<()> {
    if matches!(self.values, SortedNumericValues::Pending(_)) {
      debug_assert!(!matches!(
        self.counts,
        Some(SortedNumericCounts::Finished(_))
      ));
      self.finish_current_doc()?;
      let SortedNumericValues::Pending(pending) = &mut self.values else {
        return Err(LuceneError::illegal_state("missing pending numeric values"));
      };
      self.values = SortedNumericValues::Finished(pending.build()?);
      if let Some(SortedNumericCounts::Pending(counts)) = &mut self.counts {
        self.counts = Some(SortedNumericCounts::Finished(counts.build()?));
      }
    }
    self.docs_with_field.finish();
    Ok(())
  }
}
pub(crate) struct DocValuesProducerImpl1<'a> {
  single_value_producer: DocValuesProducerImpl<'a>,
}

impl CloseableRef for DocValuesProducerImpl1<'_> {
  fn close(&self) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }
}

impl<'a> DocValuesProducerImpl1<'a> {
  pub(crate) fn new(single_value_producer: DocValuesProducerImpl<'a>) -> Result<Self> {
    Ok(Self {
      single_value_producer,
    })
  }
}

impl<'a> DocValuesProducer for DocValuesProducerImpl1<'a> {
  type NumericDocValues = DummyNumericDocValues;
  type BinaryDocValues = DummyBinaryDocValues;
  type SortedDocValues = DummySortedDocValues;
  type SortedNumericDocValues = SingletonSortedNumericDocValues<
    BufferedSortingNumericDocValues<
      &'a PackedLongValues,
      &'a NumericDVs<crate::core::util::fixed_bit_set::FixedBitSet>,
    >,
  >;

  fn get_sorted_numeric(
    &self,
    field_info_in: &Arc<FieldInfo>,
  ) -> Result<Self::SortedNumericDocValues> {
    let v = self.single_value_producer.get_numeric(field_info_in)?;
    DocValues::singleton_numeric(v)
  }

  type SortedSetDocValues = DummySortedSetDocValues;
  type DocValuesSkipper = DummyDocValuesSkipper;
}

pub(crate) struct DocValuesProducerImpl2<'a> {
  field_info: &'a Arc<FieldInfo>,
  docs_with_field: &'a DocsWithFieldSet,
  values: &'a PackedLongValues,
  sorted: Option<&'a LongValues>,
  value_counts: &'a PackedLongValues,
}

impl CloseableRef for DocValuesProducerImpl2<'_> {
  fn close(&self) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }
}

impl<'a> DocValuesProducerImpl2<'a> {
  fn new(
    field_info: &'a Arc<FieldInfo>,
    docs_with_field: &'a DocsWithFieldSet,
    values: &'a PackedLongValues,
    sorted: Option<&'a LongValues>,
    value_counts: &'a PackedLongValues,
  ) -> Result<Self> {
    Ok(Self {
      field_info,
      docs_with_field,
      values,
      sorted,
      value_counts,
    })
  }
}

impl<'a> DocValuesProducer for DocValuesProducerImpl2<'a> {
  type NumericDocValues = DummyNumericDocValues;
  type BinaryDocValues = DummyBinaryDocValues;
  type SortedDocValues = DummySortedDocValues;
  type SortedNumericDocValues =
    SortedNumericDocValuesWriterValues<&'a PackedLongValues, &'a LongValues>;

  fn get_sorted_numeric(
    &self,
    field_info_in: &Arc<FieldInfo>,
  ) -> Result<Self::SortedNumericDocValues> {
    if !Arc::ptr_eq(self.field_info, field_info_in) {
      return Err(LuceneError::illegal_argument("wrong fieldInfo"));
    }
    let buf = BufferedSortedNumericDocValues::new(
      || self.values,
      || self.value_counts,
      self.docs_with_field.iterator()?,
    )?;
    match &self.sorted {
      Some(sorted) => Ok(SortedNumericDocValuesWriterValues::SortedMulti(
        SortingSortedNumericDocValues::new(buf, *sorted),
      )),
      None => Ok(SortedNumericDocValuesWriterValues::Multi(buf)),
    }
  }

  type SortedSetDocValues = DummySortedSetDocValues;
  type DocValuesSkipper = DummyDocValuesSkipper;
}

pub(crate) struct BufferedSortedNumericDocValues<D, P = PackedLongValues> {
  values_iter: PackedLongValuesIterator<P>,
  value_counts_iter: PackedLongValuesIterator<P>,
  docs_with_field: D,
  value_count: i32,
  value_upto: i32,
}

impl<D, P: Borrow<PackedLongValues>> BufferedSortedNumericDocValues<D, P> {
  // Defer owned clones so the values stream decodes before cloning the counts stream.
  pub(crate) fn new<F, C>(values: F, value_counts: C, docs_with_field: D) -> Result<Self>
  where
    F: FnOnce() -> P,
    C: FnOnce() -> P,
  {
    Ok(Self {
      values_iter: PackedLongValuesIterator::new(values())?,
      value_counts_iter: PackedLongValuesIterator::new(value_counts())?,
      docs_with_field,
      value_count: 0,
      value_upto: 0,
    })
  }
}

impl<D, P> DocValuesIterator for BufferedSortedNumericDocValues<D, P>
where
  D: DocIdSetIterator,
  P: Borrow<PackedLongValues>,
{
  fn advance_exact(&mut self, _target: i32) -> Result<bool> {
    Err(LuceneError::unsupported_operation(""))
  }
}

impl<D, P> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for BufferedSortedNumericDocValues<D, P>
where
  D: DocIdSetIterator,
  P: Borrow<PackedLongValues>,
{
}
impl<D, P> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for BufferedSortedNumericDocValues<D, P>
where
  D: DocIdSetIterator,
  P: Borrow<PackedLongValues>,
{
}

impl<D, P> DocIdSetIterator for BufferedSortedNumericDocValues<D, P>
where
  D: DocIdSetIterator,
  P: Borrow<PackedLongValues>,
{
  fn doc_id(&self) -> i32 {
    self.docs_with_field.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    for _ in self.value_upto..self.value_count {
      self.values_iter.next_value()?;
    }

    let doc_id = self.docs_with_field.next_doc()?;
    if doc_id != NO_MORE_DOCS {
      self.value_count = self.value_counts_iter.next_value()?.try_convert()?;
      self.value_upto = 0;
    }
    Ok(doc_id)
  }

  fn advance(&mut self, _target: i32) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn cost(&self) -> Result<i64> {
    self.docs_with_field.cost()
  }
}

impl<D, P> SortedNumericDocValues for BufferedSortedNumericDocValues<D, P>
where
  D: DocIdSetIterator,
  P: Borrow<PackedLongValues>,
{
  fn next_value(&mut self) -> Result<i64> {
    self.value_upto += 1;
    self.values_iter.next_value()
  }

  fn doc_value_count(&mut self) -> Result<i32> {
    Ok(self.value_count)
  }

  type NumericDocValues = DummyNumericDocValues;
}

pub struct SortingSortedNumericDocValues<S, V = LongValues> {
  input: S,
  values: V,
  doc_id: i32,
  upto: usize,
  num_values: i32,
}
impl<S, V> SortingSortedNumericDocValues<S, V> {
  pub(crate) fn new(input: S, values: V) -> Self {
    Self {
      input,
      values,
      doc_id: -1,
      upto: 0,
      num_values: -1,
    }
  }
}

impl<S, V> DocValuesIterator for SortingSortedNumericDocValues<S, V>
where
  S: SortedNumericDocValues,
  V: Borrow<LongValues>,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.doc_id = target;
    self.upto = self.values.borrow().offsets[self.doc_id as usize];

    if self.upto > 0 {
      self.num_values = self
        .values
        .borrow()
        .values
        .get(self.upto - 1)?
        .try_convert()?;
      Ok(true)
    } else {
      Ok(false)
    }
  }
}

impl<S, V> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SortingSortedNumericDocValues<S, V>
where
  S: SortedNumericDocValues,
  V: Borrow<LongValues>,
{
}
impl<S, V> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for SortingSortedNumericDocValues<S, V>
where
  S: SortedNumericDocValues,
  V: Borrow<LongValues>,
{
}

impl<S, V> DocIdSetIterator for SortingSortedNumericDocValues<S, V>
where
  S: SortedNumericDocValues,
  V: Borrow<LongValues>,
{
  fn doc_id(&self) -> i32 {
    self.doc_id
  }

  fn next_doc(&mut self) -> Result<i32> {
    loop {
      self.doc_id += 1;
      let doc_index = self.doc_id as usize;
      if doc_index >= self.values.borrow().offsets.len() {
        self.doc_id = NO_MORE_DOCS;
        return Ok(self.doc_id);
      }
      let offset = self.values.borrow().offsets[doc_index];
      if offset > 0 {
        self.upto = offset;
        self.num_values = self
          .values
          .borrow()
          .values
          .get(self.upto - 1)?
          .try_convert()?;
        return Ok(self.doc_id);
      }
    }
  }

  fn advance(&mut self, _target: i32) -> Result<i32> {
    Err(LuceneError::unsupported_operation("use nextDoc instead"))
  }

  fn cost(&self) -> Result<i64> {
    self.input.cost()
  }
}

impl<S, V> SortedNumericDocValues for SortingSortedNumericDocValues<S, V>
where
  S: SortedNumericDocValues,
  V: Borrow<LongValues>,
{
  fn next_value(&mut self) -> Result<i64> {
    let v = self.values.borrow().values.get(self.upto)?;
    self.upto += 1;
    Ok(v)
  }

  fn doc_value_count(&mut self) -> Result<i32> {
    Ok(self.num_values)
  }

  type NumericDocValues = DummyNumericDocValues;
}

pub struct LongValues {
  pub(crate) offsets: Vec<usize>,
  pub(crate) values: PackedLongValues,
}
impl LongValues {
  pub(crate) fn new<DM, T>(
    max_doc: usize,
    sort_map: &DM,
    old_values: &mut T,
    acceptable_overhead_ratio: f32,
  ) -> Result<Self>
  where
    DM: DocMap,
    T: SortedNumericDocValues,
  {
    let mut offsets = vec![0; max_doc];
    let mut value_builder =
      PackedLongValues::packed_long_values_builder_default(acceptable_overhead_ratio)?;
    let mut offset_index = 1;
    let mut doc_id;
    loop {
      doc_id = old_values.next_doc()?;
      if doc_id == NO_MORE_DOCS {
        break;
      }
      let new_doc_id = sort_map.old_to_new(doc_id)?;
      let num_values = old_values.doc_value_count()?;
      value_builder.add(num_values as i64)?;
      offsets[new_doc_id as usize] = offset_index;
      offset_index += 1;
      for _ in 0..num_values {
        let value = old_values.next_value()?;
        value_builder.add(value)?;
        offset_index += 1;
      }
    }

    Ok(LongValues {
      offsets,
      values: value_builder.build()?,
    })
  }
}
