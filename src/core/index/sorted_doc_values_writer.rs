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
use crate::core::codecs::block_term_state::TermStateEnum;
use crate::core::codecs::doc_values_consumer::DocValuesConsumer;
use crate::core::codecs::doc_values_producer::DocValuesProducer;
use crate::core::codecs::dummy::dummy_binary_doc_values::DummyBinaryDocValues;
use crate::core::codecs::dummy::dummy_doc_values_skipper::DummyDocValuesSkipper;
use crate::core::codecs::dummy::dummy_numeric_doc_values::DummyNumericDocValues;
use crate::core::codecs::dummy::dummy_sorted_numeric_doc_values::DummySortedNumericDocValues;
use crate::core::codecs::dummy::dummy_sorted_set_doc_values::DummySortedSetDocValues;
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::doc_values_writer::DocValuesWriter;
use crate::core::index::docs_with_field_set::{DocsWithFieldSet, DocsWithFieldSetDISI};
use crate::core::index::field_info::FieldInfo;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorted_doc_values::SortedDocValues;
use crate::core::index::sorted_doc_values_terms_enum::SortedDocValuesTermsEnum;
use crate::core::index::sorter::DocMap;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::index::{BytesRef, BytesRefValue, BytesRefValueEnum};
use crate::core::search::doc_id_set::DocIdSet;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::store::directory::Directory;
use crate::core::util::access::ByteSource;
use crate::core::util::accountable::Accountable;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bytes_ref_hash::{
  BytesRefHash, DEFAULT_CAPACITY, DirectBytesRefHash, DirectBytesStartArray,
};
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::packed::PackedInts;
use crate::core::util::packed::packed_long_values::{
  Builder, PackedLongValues, PackedLongValuesIterator,
};
use crate::core::util::{BYTE_BLOCK_SIZE, ByteBlockPool, Counter, SharedCounter, TryIntoInt};
use std::borrow::Borrow;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

type BufferedWriterSortedDocValues<V = PackedLongValues> =
  BufferedSortedDocValues<DocsWithFieldSetDISI, V>;

pub(crate) enum SortedDocValuesWriterTermsEnum<
  'a,
  V: Borrow<PackedLongValues> + 'a = PackedLongValues,
  M: Borrow<Vec<i32>> + 'a = Arc<Vec<i32>>,
> {
  Buffered(<BufferedWriterSortedDocValues<V> as SortedDocValues>::TermsEnum<'a>),
  Sorting(
    <SortingSortedDocValues<BufferedWriterSortedDocValues<V>, M> as SortedDocValues>::TermsEnum<'a>,
  ),
}

impl<'a, V: Borrow<PackedLongValues> + 'a, M: Borrow<Vec<i32>> + 'a> BytesRefIterator
  for SortedDocValuesWriterTermsEnum<'a, V, M>
{
  type Value<'b>
    = BytesRefValueEnum<'b>
  where
    Self: 'b;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    match self {
      Self::Buffered(terms) => Ok(terms.next()?.map(BytesRefValue::into_value)),
      Self::Sorting(terms) => Ok(terms.next()?.map(BytesRefValue::into_value)),
    }
  }

  fn set_next(&mut self) -> Result<bool> {
    match self {
      Self::Buffered(terms) => terms.set_next(),
      Self::Sorting(terms) => terms.set_next(),
    }
  }
}

impl<'a, V: Borrow<PackedLongValues> + 'a, M: Borrow<Vec<i32>> + 'a> TermsEnum
  for SortedDocValuesWriterTermsEnum<'a, V, M>
{
  type AttributeSource<'b>
    = <<BufferedWriterSortedDocValues<V> as SortedDocValues>::TermsEnum<'a> as TermsEnum>::AttributeSource<'b>
  where
    Self: 'b;
  type AttributeSourceMut<'b>
    = <<BufferedWriterSortedDocValues<V> as SortedDocValues>::TermsEnum<'a> as TermsEnum>::AttributeSourceMut<'b>
  where
    Self: 'b;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    match self {
      Self::Buffered(terms) => terms.attributes(),
      Self::Sorting(terms) => terms.attributes(),
    }
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    match self {
      Self::Buffered(terms) => terms.attributes_mut(),
      Self::Sorting(terms) => terms.attributes_mut(),
    }
  }

  fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool> {
    match self {
      Self::Buffered(terms) => terms.seek_exact(term),
      Self::Sorting(terms) => terms.seek_exact(term),
    }
  }

  fn prepare_seek_exact<BS: ByteSource>(&mut self, text: &BytesRef<BS>) -> Result<Option<()>> {
    match self {
      Self::Buffered(terms) => terms.prepare_seek_exact(text),
      Self::Sorting(terms) => terms.prepare_seek_exact(text),
    }
  }

  fn get_prepare_seek_exact_status<BS: ByteSource>(
    &mut self,
    target: &BytesRef<BS>,
  ) -> Result<bool> {
    match self {
      Self::Buffered(terms) => terms.get_prepare_seek_exact_status(target),
      Self::Sorting(terms) => terms.get_prepare_seek_exact_status(target),
    }
  }

  fn seek_ceil<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<SeekStatus> {
    match self {
      Self::Buffered(terms) => terms.seek_ceil(term),
      Self::Sorting(terms) => terms.seek_ceil(term),
    }
  }

  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    match self {
      Self::Buffered(terms) => terms.seek_exact_with_ord(ord),
      Self::Sorting(terms) => terms.seek_exact_with_ord(ord),
    }
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    state: &TermStateEnum,
  ) -> Result<()> {
    match self {
      Self::Buffered(terms) => terms.seek_exact_with_state(term, state),
      Self::Sorting(terms) => terms.seek_exact_with_state(term, state),
    }
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    match self {
      Self::Buffered(terms) => Ok(terms.term()?.into_value()),
      Self::Sorting(terms) => Ok(terms.term()?.into_value()),
    }
  }

  fn ord(&self) -> Result<i64> {
    match self {
      Self::Buffered(terms) => terms.ord(),
      Self::Sorting(terms) => terms.ord(),
    }
  }

  fn doc_freq(&mut self) -> Result<i32> {
    match self {
      Self::Buffered(terms) => terms.doc_freq(),
      Self::Sorting(terms) => terms.doc_freq(),
    }
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    match self {
      Self::Buffered(terms) => terms.total_term_freq(),
      Self::Sorting(terms) => terms.total_term_freq(),
    }
  }

  type PostingsEnum =
    <<BufferedWriterSortedDocValues<V> as SortedDocValues>::TermsEnum<'a> as TermsEnum>::PostingsEnum;

  fn postings(&mut self, reuse: Option<Self::PostingsEnum>) -> Result<Self::PostingsEnum> {
    match self {
      Self::Buffered(terms) => terms.postings(reuse),
      Self::Sorting(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    match self {
      Self::Buffered(terms) => terms.postings_with_flags(reuse, flags),
      Self::Sorting(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  type ImpactsEnum =
    <<BufferedWriterSortedDocValues<V> as SortedDocValues>::TermsEnum<'a> as TermsEnum>::ImpactsEnum;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    match self {
      Self::Buffered(terms) => terms.impacts(flags),
      Self::Sorting(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    match self {
      Self::Buffered(terms) => terms.term_state(),
      Self::Sorting(terms) => terms.term_state(),
    }
  }
}

pub(crate) enum SortedDocValuesWriterValues<
  V: Borrow<PackedLongValues> = PackedLongValues,
  M: Borrow<Vec<i32>> = Arc<Vec<i32>>,
> {
  Buffered(BufferedWriterSortedDocValues<V>),
  Sorting(SortingSortedDocValues<BufferedWriterSortedDocValues<V>, M>),
}

impl<V: Borrow<PackedLongValues>, M: Borrow<Vec<i32>>> DocValuesIterator
  for SortedDocValuesWriterValues<V, M>
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::Buffered(values) => values.advance_exact(target),
      Self::Sorting(values) => values.advance_exact(target),
    }
  }
}

impl<V: Borrow<PackedLongValues>, M: Borrow<Vec<i32>>>
  crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SortedDocValuesWriterValues<V, M>
{
}
impl<V: Borrow<PackedLongValues>, M: Borrow<Vec<i32>>>
  crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for SortedDocValuesWriterValues<V, M>
{
}

impl<V: Borrow<PackedLongValues>, M: Borrow<Vec<i32>>> DocIdSetIterator
  for SortedDocValuesWriterValues<V, M>
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::Buffered(values) => values.doc_id(),
      Self::Sorting(values) => values.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::Buffered(values) => values.next_doc(),
      Self::Sorting(values) => values.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Buffered(values) => values.advance(target),
      Self::Sorting(values) => values.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Buffered(values) => values.slow_advance(target),
      Self::Sorting(values) => values.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::Buffered(values) => values.cost(),
      Self::Sorting(values) => values.cost(),
    }
  }
}

impl<V: Borrow<PackedLongValues>, M: Borrow<Vec<i32>>> SortedDocValues
  for SortedDocValuesWriterValues<V, M>
{
  type OrdValue<'a>
    = BytesRef<&'a [u8]>
  where
    Self: 'a;

  fn ord_value(&mut self) -> Result<i32> {
    match self {
      Self::Buffered(values) => values.ord_value(),
      Self::Sorting(values) => values.ord_value(),
    }
  }

  fn lookup_ord(&mut self, ord: i32) -> Result<Self::OrdValue<'_>> {
    match self {
      Self::Buffered(values) => values.lookup_ord(ord),
      Self::Sorting(values) => values.lookup_ord(ord),
    }
  }

  fn get_value_count(&self) -> Result<i32> {
    match self {
      Self::Buffered(values) => values.get_value_count(),
      Self::Sorting(values) => values.get_value_count(),
    }
  }

  fn lookup_term<BS: ByteSource>(&mut self, key: &BytesRef<BS>) -> Result<i32> {
    match self {
      Self::Buffered(values) => values.lookup_term(key),
      Self::Sorting(values) => values.lookup_term(key),
    }
  }

  type TermsEnum<'a>
    = SortedDocValuesWriterTermsEnum<'a, V, M>
  where
    Self: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    match self {
      Self::Buffered(values) => values
        .terms_enum()
        .map(SortedDocValuesWriterTermsEnum::Buffered),
      Self::Sorting(values) => values
        .terms_enum()
        .map(SortedDocValuesWriterTermsEnum::Sorting),
    }
  }
}

/// Buffers pending byte slices per document, sorts them by `i32` ordinal, then writes them during
/// segment flush.
pub(crate) struct SortedDocValuesWriter {
  hash: Option<DirectBytesRefHash>,
  frozen_hash: Option<Arc<DirectBytesRefHash>>,
  pending: Builder,
  docs_with_field: DocsWithFieldSet,
  iw_bytes_used: SharedCounter,
  bytes_used: i64, // this currently only tracks differences in 'pending'
  field_info: Arc<FieldInfo>,
  last_doc_id: i32,

  final_ords: Option<PackedLongValues>,
  // Sorted term IDs remain in BytesRefHash; is_sorted tracks whether sort has been called.
  is_sorted: bool,
  final_ord_map: Option<Arc<Vec<i32>>>,
  pool: Option<Arc<ByteBlockPool>>,
}

impl SortedDocValuesWriter {
  pub(crate) fn new(field_info: Arc<FieldInfo>, iw_bytes_used: SharedCounter) -> Result<Self> {
    let bytes_start_array =
      DirectBytesStartArray::with_counter(DEFAULT_CAPACITY, iw_bytes_used.clone());
    let hash = BytesRefHash::from_bytes_start_array(DEFAULT_CAPACITY, bytes_start_array)?;
    let pending = PackedLongValues::delta_packed_long_values_builder_default(PackedInts::COMPACT)?;
    let docs_with_field = DocsWithFieldSet::new();
    let bytes_used = pending.ram_bytes_used()? + docs_with_field.ram_bytes_used()?;
    iw_bytes_used.add_and_get(bytes_used);

    Ok(Self {
      hash: Some(hash),
      frozen_hash: None,
      pending,
      docs_with_field,
      iw_bytes_used,
      bytes_used,
      field_info,
      last_doc_id: -1,
      final_ords: None,
      is_sorted: false,
      final_ord_map: None,
      pool: None,
    })
  }

  pub(crate) fn add_value(
    &mut self,
    doc_id: i32,
    value: &BytesRef<&[u8]>,
    pool: &mut ByteBlockPool,
  ) -> Result<()> {
    if doc_id <= self.last_doc_id {
      return Err(LuceneError::illegal_argument(format!(
        "DocValuesField \"{}\" appears more than once in this document (only one value is allowed per field)",
        self.field_info.name
      )));
    }

    if value.length > (BYTE_BLOCK_SIZE as usize - 2) {
      return Err(LuceneError::illegal_argument(format!(
        "DocValuesField \"{}\" is too large, must be <= {}",
        self.field_info.name,
        BYTE_BLOCK_SIZE - 2
      )));
    }

    self.add_one_value(value, pool)?;
    self.docs_with_field.add(doc_id)?;
    self.last_doc_id = doc_id;
    Ok(())
  }

  fn add_one_value(&mut self, value: &BytesRef<&[u8]>, pool: &mut ByteBlockPool) -> Result<()> {
    let hash = self
      .hash
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("cannot add values after finishing"))?;
    let mut term_id = hash.add(value, pool)?;
    if term_id < 0 {
      term_id = -term_id - 1;
    } else {
      // reserve additional space for each unique value:
      // 1. when indexing, when hash is 50% full, rehash() suddenly needs 2*size ints.
      // 2. when flushing, we need 1 int per value (slot in the ordMap).
      self
        .iw_bytes_used
        .add_and_get((2 * BitUtil::INT_BYTES) as i64);
    }

    self.pending.add(term_id as i64)?;
    self.update_bytes_used()
  }

  fn update_bytes_used(&mut self) -> Result<()> {
    let new_bytes_used = self.pending.ram_bytes_used()? + self.docs_with_field.ram_bytes_used()?;
    let delta = new_bytes_used - self.bytes_used;
    if delta != 0 {
      self.iw_bytes_used.add_and_get(delta);
    }
    self.bytes_used = new_bytes_used;
    Ok(())
  }

  fn sort_doc_values<SDV, DM>(
    max_doc: usize,
    sort_map: &DM,
    old_values: &mut SDV,
  ) -> Result<Vec<i32>>
  where
    SDV: SortedDocValues,
    DM: DocMap,
  {
    let mut ords = vec![-1; max_doc];
    let mut doc_id;
    loop {
      doc_id = old_values.next_doc()?;
      if doc_id == NO_MORE_DOCS {
        break;
      }
      let new_doc_id = sort_map.old_to_new(doc_id)?;
      ords[new_doc_id as usize] = old_values.ord_value()?;
    }
    Ok(ords)
  }
}

impl Display for SortedDocValuesWriter {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl DocValuesWriter for SortedDocValuesWriter {
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
    if !self.is_sorted {
      return Err(LuceneError::illegal_state(
        "must be finished before getting doc values",
      ));
    }
    let Some(frozen_hash) = self.frozen_hash.as_ref() else {
      return Err(LuceneError::illegal_state(
        "must be finished before getting doc values",
      ));
    };
    let Some(final_ords) = self.final_ords.as_ref() else {
      return Err(LuceneError::illegal_state(
        "must be finished before getting doc values",
      ));
    };
    let Some(final_ord_map) = self.final_ord_map.as_ref() else {
      return Err(LuceneError::illegal_state(
        "must be finished before getting doc values",
      ));
    };
    let pool = self
      .pool
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("must be finished before getting doc values"))?;
    let sorted = get_sorted_values(
      frozen_hash,
      pool,
      final_ords,
      final_ord_map,
      &self.docs_with_field,
      sort_map,
    )?;
    let producer = DocValuesProducerImpl::new(
      frozen_hash,
      pool,
      final_ords,
      final_ord_map,
      &self.docs_with_field,
      &self.field_info,
      sorted.as_ref(),
    )?;
    dv_consumer.add_sorted_field(write_state, segment_info, &self.field_info, &producer)?;
    Ok(())
  }

  type DocIdSetIterator = BufferedSortedDocValues<DocsWithFieldSetDISI>;

  fn get_doc_values(&self) -> Result<Self::DocIdSetIterator> {
    if !self.is_sorted {
      return Err(LuceneError::illegal_state(
        "must be finished before getting doc values",
      ));
    }
    let Some(frozen_hash) = self.frozen_hash.as_ref() else {
      return Err(LuceneError::illegal_state(
        "must be finished before getting doc values",
      ));
    };
    let Some(final_ords) = self.final_ords.as_ref() else {
      return Err(LuceneError::illegal_state(
        "must be finished before getting doc values",
      ));
    };
    let Some(final_ord_map) = self.final_ord_map.as_ref() else {
      return Err(LuceneError::illegal_state(
        "must be finished before getting doc values",
      ));
    };
    let pool = self
      .pool
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("must be finished before getting doc values"))?;
    BufferedSortedDocValues::new(
      frozen_hash.clone(),
      pool.clone(),
      || final_ords.clone(),
      final_ord_map.clone(),
      self.docs_with_field.iterator()?,
    )
  }

  fn finish(&mut self, pool: &Arc<ByteBlockPool>) -> Result<()> {
    self.pool = Some(pool.clone());
    self.docs_with_field.finish();
    if !self.is_sorted {
      self.update_bytes_used()?;
      debug_assert!(self.final_ord_map.is_none() && self.final_ords.is_none());
      let hash = self
        .hash
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("buffered value hash is missing"))?;
      let value_count = hash.size();
      hash.sort(pool.as_ref())?;
      self.is_sorted = true;
      let ords = self.pending.build()?;

      let mut ord_map = vec![0i32; value_count];
      for ord in 0..value_count {
        let index = hash.ids[ord] as usize;
        ord_map[index] = ord as i32;
      }
      self.frozen_hash = self.hash.take().map(Arc::new);
      self.final_ords = Some(ords);
      self.final_ord_map = Some(Arc::new(ord_map));
    }
    Ok(())
  }
}

pub(crate) struct DocValuesProducerImpl<'a> {
  hash: &'a Arc<DirectBytesRefHash>,
  pool: &'a Arc<ByteBlockPool>,
  ords: &'a PackedLongValues,
  ord_map: &'a Arc<Vec<i32>>,
  docs_with_field: &'a DocsWithFieldSet,
  writer_field_info: &'a Arc<FieldInfo>,
  sorted: Option<&'a Vec<i32>>,
}

impl CloseableRef for DocValuesProducerImpl<'_> {
  fn close(&self) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }
}

impl<'a> DocValuesProducerImpl<'a> {
  pub(crate) fn new(
    hash: &'a Arc<DirectBytesRefHash>,
    pool: &'a Arc<ByteBlockPool>,
    ords: &'a PackedLongValues,
    ord_map: &'a Arc<Vec<i32>>,
    docs_with_field: &'a DocsWithFieldSet,
    writer_field_info: &'a Arc<FieldInfo>,
    sorted: Option<&'a Vec<i32>>,
  ) -> Result<Self> {
    Ok(Self {
      hash,
      pool,
      ords,
      ord_map,
      docs_with_field,
      writer_field_info,
      sorted,
    })
  }
}

impl<'a> DocValuesProducer for DocValuesProducerImpl<'a> {
  type NumericDocValues = DummyNumericDocValues;
  type BinaryDocValues = DummyBinaryDocValues;
  type SortedDocValues = SortedDocValuesWriterValues<&'a PackedLongValues, &'a Vec<i32>>;

  fn get_sorted(&self, field_info_in: &Arc<FieldInfo>) -> Result<Self::SortedDocValues> {
    if !Arc::ptr_eq(self.writer_field_info, field_info_in) {
      return Err(LuceneError::illegal_argument("wrong fieldInfo"));
    }
    let buf = BufferedSortedDocValues::new(
      self.hash.clone(),
      self.pool.clone(),
      || self.ords,
      self.ord_map.clone(),
      self.docs_with_field.iterator()?,
    )?;
    match self.sorted {
      Some(sorted) => Ok(SortedDocValuesWriterValues::Sorting(
        SortingSortedDocValues::new(buf, sorted),
      )),
      None => Ok(SortedDocValuesWriterValues::Buffered(buf)),
    }
  }

  type SortedNumericDocValues = DummySortedNumericDocValues;
  type SortedSetDocValues = DummySortedSetDocValues;
  type DocValuesSkipper = DummyDocValuesSkipper;
}

pub(crate) struct BufferedSortedDocValues<D, V: Borrow<PackedLongValues> = PackedLongValues> {
  hash: Arc<DirectBytesRefHash>,
  pool: Arc<ByteBlockPool>,
  ord_map: Arc<Vec<i32>>,
  ord: i32,
  iter: PackedLongValuesIterator<V>,
  docs_with_field: D,
}

impl<D, V: Borrow<PackedLongValues>> BufferedSortedDocValues<D, V> {
  // Delay owned cloning until after all constructor arguments have been evaluated.
  pub(crate) fn new(
    hash: Arc<DirectBytesRefHash>,
    pool: Arc<ByteBlockPool>,
    doc_to_ord: impl FnOnce() -> V,
    ord_map: Arc<Vec<i32>>,
    docs_with_field: D,
  ) -> Result<Self> {
    Ok(Self {
      hash,
      pool,
      ord_map,
      ord: -1,
      iter: PackedLongValuesIterator::new(doc_to_ord())?,
      docs_with_field,
    })
  }
}

impl<D, V: Borrow<PackedLongValues>> DocValuesIterator for BufferedSortedDocValues<D, V>
where
  D: DocIdSetIterator,
{
  fn advance_exact(&mut self, _target: i32) -> Result<bool> {
    Err(LuceneError::unsupported_operation(""))
  }
}

impl<D, V: Borrow<PackedLongValues>>
  crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for BufferedSortedDocValues<D, V>
where
  D: DocIdSetIterator,
{
}
impl<D, V: Borrow<PackedLongValues>> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for BufferedSortedDocValues<D, V>
where
  D: DocIdSetIterator,
{
}

impl<D, V: Borrow<PackedLongValues>> DocIdSetIterator for BufferedSortedDocValues<D, V>
where
  D: DocIdSetIterator,
{
  fn doc_id(&self) -> i32 {
    self.docs_with_field.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    let doc_id = self.docs_with_field.next_doc()?;
    if doc_id != NO_MORE_DOCS {
      let raw_ord: i32 = self.iter.next_value()?.try_convert()?;
      let mapped = self.ord_map[raw_ord as usize];
      self.ord = mapped;
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

impl<D, V: Borrow<PackedLongValues>> SortedDocValues for BufferedSortedDocValues<D, V>
where
  D: DocIdSetIterator,
{
  type OrdValue<'a>
    = BytesRef<&'a [u8]>
  where
    Self: 'a;

  fn ord_value(&mut self) -> Result<i32> {
    Ok(self.ord)
  }

  fn lookup_ord(&mut self, ord: i32) -> Result<Self::OrdValue<'_>> {
    let ord_index = ord as usize;
    debug_assert!(ord >= 0 && ord_index < self.hash.ids.len());
    let index = self.hash.ids[ord_index];
    debug_assert!(
      index >= 0 && (index as usize) < self.hash.ids.len(),
      "sorted_values[ord] out of range"
    );
    let position = self.hash.get(index, self.pool.as_ref())?;
    let block = self.pool.get_buffer(position.block_index);
    Ok(BytesRef {
      bytes: block,
      offset: position.offset,
      length: position.length,
    })
  }

  fn get_value_count(&self) -> Result<i32> {
    self.hash.size().try_convert()
  }

  type TermsEnum<'a>
    = SortedDocValuesTermsEnum<&'a mut Self>
  where
    Self: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    self.default_terms_enum()
  }
}

pub struct SortingSortedDocValues<S, M = Arc<Vec<i32>>> {
  input: S,
  ords: M,
  doc_id: i32,
}

impl<S, M> SortingSortedDocValues<S, M> {
  pub(crate) fn new(input: S, ords: M) -> Self {
    Self {
      input,
      ords,
      doc_id: -1,
    }
  }
}

impl<S, M> DocValuesIterator for SortingSortedDocValues<S, M>
where
  S: SortedDocValues,
  M: Borrow<Vec<i32>>,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    // Needed by `IndexSorter::StringSorter`.
    self.doc_id = target;
    Ok(<M as Borrow<Vec<i32>>>::borrow(&self.ords)[target as usize] != -1)
  }
}

impl<S, M> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SortingSortedDocValues<S, M>
where
  S: SortedDocValues,
  M: Borrow<Vec<i32>>,
{
}
impl<S, M> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for SortingSortedDocValues<S, M>
where
  S: SortedDocValues,
  M: Borrow<Vec<i32>>,
{
}

impl<S, M> DocIdSetIterator for SortingSortedDocValues<S, M>
where
  S: SortedDocValues,
  M: Borrow<Vec<i32>>,
{
  fn doc_id(&self) -> i32 {
    self.doc_id
  }

  fn next_doc(&mut self) -> Result<i32> {
    loop {
      self.doc_id += 1;
      let doc_index = self.doc_id as usize;
      if doc_index == <M as Borrow<Vec<i32>>>::borrow(&self.ords).len() {
        self.doc_id = NO_MORE_DOCS;
        break;
      }
      if <M as Borrow<Vec<i32>>>::borrow(&self.ords)[doc_index] != -1 {
        break;
      }
      // skip missing docs
    }
    Ok(self.doc_id)
  }

  fn advance(&mut self, _target: i32) -> Result<i32> {
    Err(LuceneError::unsupported_operation("use next_doc instead"))
  }

  fn cost(&self) -> Result<i64> {
    self.input.cost()
  }
}

impl<S, M> SortedDocValues for SortingSortedDocValues<S, M>
where
  S: SortedDocValues,
  M: Borrow<Vec<i32>>,
{
  type OrdValue<'a>
    = S::OrdValue<'a>
  where
    Self: 'a;

  fn ord_value(&mut self) -> Result<i32> {
    Ok(<M as Borrow<Vec<i32>>>::borrow(&self.ords)[self.doc_id as usize])
  }

  fn lookup_ord(&mut self, ord: i32) -> Result<Self::OrdValue<'_>> {
    self.input.lookup_ord(ord)
  }

  fn get_value_count(&self) -> Result<i32> {
    self.input.get_value_count()
  }

  type TermsEnum<'a>
    = SortedDocValuesTermsEnum<&'a mut Self>
  where
    Self: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    self.default_terms_enum()
  }
}

pub(crate) fn get_sorted_values<DM>(
  hash: &Arc<DirectBytesRefHash>,
  pool: &Arc<ByteBlockPool>,
  ords: &PackedLongValues,
  ord_map: &Arc<Vec<i32>>,
  docs_with_field: &DocsWithFieldSet,
  sort_map: Option<&DM>,
) -> Result<Option<Vec<i32>>>
where
  DM: DocMap,
{
  let sorted = if let Some(sort_map) = sort_map {
    let docs_iter = docs_with_field.iterator()?;
    let mut old_values = BufferedSortedDocValues::new(
      hash.clone(),
      pool.clone(),
      || ords,
      ord_map.clone(),
      docs_iter,
    )?;
    Some(SortedDocValuesWriter::sort_doc_values(
      sort_map.size() as usize,
      sort_map,
      &mut old_values,
    )?)
  } else {
    None
  };

  Ok(sorted)
}
