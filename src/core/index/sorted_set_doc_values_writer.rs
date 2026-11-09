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
use crate::core::codecs::dummy::dummy_sorted_numeric_doc_values::DummySortedNumericDocValues;
use crate::core::index::BytesRefValueEnum2;
use crate::core::index::doc_values::{DocValues, EmptySortedSet, SortedDocValuesWithEmpty};
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::doc_values_writer::DocValuesWriter;
use crate::core::index::docs_with_field_set::DocsWithFieldSetDISI;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::singleton_sorted_set_doc_values::SingletonSortedSetDocValues;
use crate::core::index::sorted_doc_values::SortedDocValuesEnum2;
use crate::core::index::sorted_doc_values_writer::{
  BufferedSortedDocValues, SortedDocValuesWriterValues, get_sorted_values,
};
use crate::core::index::sorted_set_doc_values::SortedSetDocValues;
use crate::core::index::sorted_set_doc_values_terms_enum::SortedSetDocValuesTermsEnum;
use crate::core::index::sorter::DocMap;
use crate::core::index::{BytesRef, docs_with_field_set::DocsWithFieldSet, field_info::FieldInfo};
use crate::core::search::doc_id_set::DocIdSet;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::store::directory::Directory;
use crate::core::util::access::ByteSource;
use crate::core::util::accountable::Accountable;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bytes_ref_hash::{
  BytesRefHash, DEFAULT_CAPACITY, DirectBytesRefHash, DirectBytesStartArray,
};
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::long_values::LongValues;
use crate::core::util::packed::growable_writer::GrowableWriter;
use crate::core::util::packed::packed_long_values::{
  Builder, PackedLongValues, PackedLongValuesIterator,
};
use crate::core::util::packed::{Mutable, PackedInts, Reader};
use crate::core::util::ram_usage_estimator::size_of_array_capacity;
use crate::core::util::{BYTE_BLOCK_SIZE, ByteBlockPool, Counter, SharedCounter, TryIntoInt};
use std::borrow::Borrow;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

type BufferedWriterSortedSetDocValues<V = PackedLongValues> =
  BufferedSortedSetDocValues<DocsWithFieldSetDISI, V>;

pub(crate) enum SortedSetDocValuesWriterValues<
  V: Borrow<PackedLongValues> = PackedLongValues,
  O: Borrow<DocOrds> = DocOrds,
> {
  Buffered(BufferedWriterSortedSetDocValues<V>),
  Sorting(SortingSortedSetDocValues<BufferedWriterSortedSetDocValues<V>, O>),
}

pub(crate) enum SortedSetDocValuesWriterDocIdSetIterator {
  Buffered(BufferedWriterSortedSetDocValues),
  Singleton(SingletonSortedSetDocValues<BufferedSortedDocValues<DocsWithFieldSetDISI>>),
}

impl<V: Borrow<PackedLongValues>, O: Borrow<DocOrds>> DocValuesIterator
  for SortedSetDocValuesWriterValues<V, O>
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::Buffered(values) => values.advance_exact(target),
      Self::Sorting(values) => values.advance_exact(target),
    }
  }
}

impl<V: Borrow<PackedLongValues>, O: Borrow<DocOrds>>
  crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SortedSetDocValuesWriterValues<V, O>
{
}
impl<V: Borrow<PackedLongValues>, O: Borrow<DocOrds>>
  crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for SortedSetDocValuesWriterValues<V, O>
{
}

impl<V: Borrow<PackedLongValues>, O: Borrow<DocOrds>> DocIdSetIterator
  for SortedSetDocValuesWriterValues<V, O>
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

impl<V: Borrow<PackedLongValues>, O: Borrow<DocOrds>> SortedSetDocValues
  for SortedSetDocValuesWriterValues<V, O>
{
  type OrdValue<'a>
    = BytesRef<&'a [u8]>
  where
    Self: 'a;

  fn next_ord(&mut self) -> Result<i64> {
    match self {
      Self::Buffered(values) => values.next_ord(),
      Self::Sorting(values) => values.next_ord(),
    }
  }

  fn doc_value_count(&mut self) -> Result<i32> {
    match self {
      Self::Buffered(values) => values.doc_value_count(),
      Self::Sorting(values) => values.doc_value_count(),
    }
  }

  fn lookup_ord(&mut self, ord: i64) -> Result<Self::OrdValue<'_>> {
    match self {
      Self::Buffered(values) => values.lookup_ord(ord),
      Self::Sorting(values) => values.lookup_ord(ord),
    }
  }

  fn get_value_count(&self) -> Result<i64> {
    match self {
      Self::Buffered(values) => values.get_value_count(),
      Self::Sorting(values) => values.get_value_count(),
    }
  }

  type TermsEnum<'a>
    = SortedSetDocValuesTermsEnum<&'a mut Self>
  where
    Self: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    self.default_terms_enum()
  }

  type SortedDocValues = DummySortedDocValues;
}

impl DocValuesIterator for SortedSetDocValuesWriterDocIdSetIterator {
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::Buffered(values) => values.advance_exact(target),
      Self::Singleton(values) => values.advance_exact(target),
    }
  }
}

impl crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SortedSetDocValuesWriterDocIdSetIterator
{
}
impl crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for SortedSetDocValuesWriterDocIdSetIterator
{
}

impl DocIdSetIterator for SortedSetDocValuesWriterDocIdSetIterator {
  fn doc_id(&self) -> i32 {
    match self {
      Self::Buffered(values) => values.doc_id(),
      Self::Singleton(values) => values.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::Buffered(values) => values.next_doc(),
      Self::Singleton(values) => values.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Buffered(values) => values.advance(target),
      Self::Singleton(values) => values.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Buffered(values) => values.slow_advance(target),
      Self::Singleton(values) => values.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::Buffered(values) => values.cost(),
      Self::Singleton(values) => values.cost(),
    }
  }
}

impl SortedSetDocValues for SortedSetDocValuesWriterDocIdSetIterator {
  type OrdValue<'a>
    = BytesRef<&'a [u8]>
  where
    Self: 'a;

  fn next_ord(&mut self) -> Result<i64> {
    match self {
      Self::Buffered(values) => values.next_ord(),
      Self::Singleton(values) => values.next_ord(),
    }
  }

  fn doc_value_count(&mut self) -> Result<i32> {
    match self {
      Self::Buffered(values) => values.doc_value_count(),
      Self::Singleton(values) => values.doc_value_count(),
    }
  }

  fn lookup_ord(&mut self, ord: i64) -> Result<Self::OrdValue<'_>> {
    match self {
      Self::Buffered(values) => values.lookup_ord(ord),
      Self::Singleton(values) => values.lookup_ord(ord),
    }
  }

  fn get_value_count(&self) -> Result<i64> {
    match self {
      Self::Buffered(values) => values.get_value_count(),
      Self::Singleton(values) => values.get_value_count(),
    }
  }

  type TermsEnum<'a> = SortedSetDocValuesTermsEnum<&'a mut Self>;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    self.default_terms_enum()
  }

  fn is_single_valued(&self) -> bool {
    matches!(self, Self::Singleton(_))
  }

  type SortedDocValues = BufferedSortedDocValues<DocsWithFieldSetDISI>;

  fn get_sorted_doc_values(&mut self) -> Result<Self::SortedDocValues> {
    match self {
      Self::Singleton(values) => values.get_sorted_doc_values(),
      Self::Buffered(_) => Err(LuceneError::unsupported_operation("")),
    }
  }
}

/// Buffers pending byte slices per document, sorts them by `i32` ordinal, then writes them during
/// segment flush.
pub(crate) struct SortedSetDocValuesWriter {
  hash: Option<DirectBytesRefHash>,
  frozen_hash: Option<Arc<DirectBytesRefHash>>,
  ords: SortedSetOrdinals,         // stream of all termIDs
  counts: Option<SortedSetCounts>, // termIDs per doc
  docs_with_field: DocsWithFieldSet,
  iw_bytes_used: SharedCounter,
  bytes_used: i64, // this only tracks differences in 'pending' and 'pendingCounts'
  field_info: Arc<FieldInfo>,

  current_doc: i32,
  current_values: smallvec::SmallVec<[i32; 4]>,
  max_count: usize,

  // Sorted term IDs remain in BytesRefHash; is_sorted tracks whether sort has been called.
  is_sorted: bool,
  final_ord_map: Option<Arc<Vec<i32>>>,
  pool: Option<Arc<ByteBlockPool>>,
}

enum SortedSetCounts {
  Pending(Builder),
  Finished(PackedLongValues),
}

enum SortedSetOrdinals {
  Pending(Builder),
  Finished(PackedLongValues),
}

impl SortedSetDocValuesWriter {
  pub(crate) fn new(field_info: Arc<FieldInfo>, iw_bytes_used: SharedCounter) -> Result<Self> {
    let bytes_start_array =
      DirectBytesStartArray::with_counter(DEFAULT_CAPACITY, iw_bytes_used.clone());
    let hash = BytesRefHash::from_bytes_start_array(DEFAULT_CAPACITY, bytes_start_array)?;
    let pending = PackedLongValues::packed_long_values_builder_default(PackedInts::COMPACT)?;
    let docs_with_field = DocsWithFieldSet::new();
    let current_values = smallvec::SmallVec::<[i32; 4]>::new();
    let bytes_used = pending.ram_bytes_used()?
      + docs_with_field.ram_bytes_used()?
      + size_of_array_capacity::<i32>(if current_values.spilled() {
        current_values.capacity()
      } else {
        0
      });
    iw_bytes_used.add_and_get(bytes_used);
    Ok(Self {
      hash: Some(hash),
      frozen_hash: None,
      ords: SortedSetOrdinals::Pending(pending),
      counts: None,
      docs_with_field,
      iw_bytes_used,
      bytes_used,
      field_info,
      current_doc: -1,
      current_values,
      max_count: 0,
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
    debug_assert!(doc_id >= self.current_doc);
    if value.length > (BYTE_BLOCK_SIZE as usize - 2) {
      return Err(LuceneError::illegal_argument(format!(
        "DocValuesField \"{}\" is too large, must be <= {}",
        self.field_info.name,
        BYTE_BLOCK_SIZE - 2
      )));
    }
    if doc_id != self.current_doc {
      self.finish_current_doc()?;
      self.current_doc = doc_id;
    }
    self.add_one_value(value, pool)?;
    self.update_bytes_used()
  }
  // finalize currentDoc: this deduplicates the current term ids
  fn finish_current_doc(&mut self) -> Result<()> {
    if self.current_doc == -1 {
      return Ok(());
    }
    if self.current_values.len() > 1 {
      self.current_values.sort_unstable();
    }
    let mut last_value = -1;
    let mut count = 0;
    for &term_id in &self.current_values {
      // if it's not a duplicate
      if term_id != last_value {
        let SortedSetOrdinals::Pending(pending) = &mut self.ords else {
          return Err(LuceneError::illegal_state("Cannot be reused after build()"));
        };
        pending.add(term_id as i64)?;
        count += 1;
      }
      last_value = term_id;
    }
    // record the number of unique term ids for this doc
    if let Some(counts) = &mut self.counts {
      let SortedSetCounts::Pending(counts) = counts else {
        return Err(LuceneError::illegal_state("Cannot be reused after build()"));
      };
      counts.add(count as i64)?;
    } else if count != 1 {
      let mut pc = PackedLongValues::delta_packed_long_values_builder_default(PackedInts::COMPACT)?;
      for _ in 0..self.docs_with_field.cardinality() {
        pc.add(1)?;
      }
      pc.add(count as i64)?;
      self.counts = Some(SortedSetCounts::Pending(pc));
    }
    self.max_count = self.max_count.max(count);
    self.current_values.clear();
    self.docs_with_field.add(self.current_doc)?;
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
    if self.current_values.len() == self.current_values.capacity() {
      let required = self.current_values.len() + 1;
      ArrayUtil::oversize(required, std::mem::size_of::<i32>())?;
      self
        .current_values
        .reserve(required - self.current_values.len());
    }
    self.current_values.push(term_id);
    Ok(())
  }

  fn update_bytes_used(&mut self) -> Result<()> {
    let pc_used = if let Some(SortedSetCounts::Pending(pc)) = &self.counts {
      pc.ram_bytes_used()?
    } else {
      0
    };
    let SortedSetOrdinals::Pending(pending) = &self.ords else {
      return Err(LuceneError::illegal_state("Cannot be reused after build()"));
    };
    let new_used = pending.ram_bytes_used()?
      + pc_used
      + self.docs_with_field.ram_bytes_used()?
      + size_of_array_capacity::<i32>(if self.current_values.spilled() {
        self.current_values.capacity()
      } else {
        0
      });
    let delta = new_used - self.bytes_used;
    if delta != 0 {
      self.iw_bytes_used.add_and_get(delta);
    }
    self.bytes_used = new_used;
    Ok(())
  }

  pub(crate) fn get_values(
    ord_map: Arc<Vec<i32>>,
    hash: Arc<DirectBytesRefHash>,
    pool: Arc<ByteBlockPool>,
    ords: &PackedLongValues,
    ord_counts: Option<&PackedLongValues>,
    max_count: usize,
    docs_with_field: &DocsWithFieldSet,
  ) -> Result<SortedSetDocValuesWriterDocIdSetIterator> {
    let docs_iter = docs_with_field.iterator()?;
    match ord_counts {
      Some(ords_counts) => Ok(SortedSetDocValuesWriterDocIdSetIterator::Buffered(
        BufferedSortedSetDocValues::new(
          ord_map,
          hash,
          pool,
          || ords.clone(),
          || ords_counts.clone(),
          max_count,
          docs_iter,
        )?,
      )),
      None => Ok(SortedSetDocValuesWriterDocIdSetIterator::Singleton(
        DocValues::singleton_sorted(BufferedSortedDocValues::new(
          hash,
          pool,
          || ords.clone(),
          ord_map,
          docs_iter,
        )?)?,
      )),
    }
  }
}

impl Display for SortedSetDocValuesWriter {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl DocValuesWriter for SortedSetDocValuesWriter {
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
    let SortedSetOrdinals::Finished(ords) = &self.ords else {
      return Err(LuceneError::illegal_state(
        "must be finished before flushing",
      ));
    };
    if self.final_ord_map.is_none() || self.frozen_hash.is_none() {
      return Err(LuceneError::illegal_state(
        "must be finished before flushing",
      ));
    }
    let ord_counts = match &self.counts {
      Some(SortedSetCounts::Finished(counts)) => Some(counts),
      _ => None,
    };
    let ord_map = self
      .final_ord_map
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("missing final ordinal map while flushing"))?;
    let frozen_hash = self
      .frozen_hash
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("missing frozen hash while flushing"))?;
    let pool = self
      .pool
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("must be finished before flushing"))?;

    if ord_counts.is_none() {
      let sorted = get_sorted_values(
        frozen_hash,
        pool,
        ords,
        ord_map,
        &self.docs_with_field,
        sort_map,
      )?;
      let single_value_producer =
        crate::core::index::sorted_doc_values_writer::DocValuesProducerImpl::new(
          frozen_hash,
          pool,
          ords,
          ord_map,
          &self.docs_with_field,
          &self.field_info,
          sorted.as_ref(),
        )?;
      let producer = DocValuesProducerImpl2::new(single_value_producer);
      dv_consumer.add_sorted_set_field(write_state, segment_info, &self.field_info, &producer)?;
      return Ok(());
    }
    let ord_counts = ord_counts
      .ok_or_else(|| LuceneError::illegal_state("missing ordinal counts while flushing"))?;

    let doc_ords = if let Some(map) = sort_map {
      let docs_iter = self.docs_with_field.iterator()?;
      let mut values = BufferedSortedSetDocValues::new(
        ord_map.clone(),
        frozen_hash.clone(),
        pool.clone(),
        || ords,
        || ord_counts,
        self.max_count,
        docs_iter,
      )?;
      Some(DocOrds::new(
        segment_info.max_doc()?,
        map,
        &mut values,
        PackedInts::FASTEST,
        PackedInts::bits_required(self.max_count as i64)?,
      )?)
    } else {
      None
    };
    let producer = DocValuesProducerImpl1::new(
      &self.field_info,
      ord_map,
      frozen_hash,
      pool,
      ords,
      ord_counts,
      self.max_count,
      &self.docs_with_field,
      doc_ords.as_ref(),
    );
    dv_consumer.add_sorted_set_field(write_state, segment_info, &self.field_info, &producer)?;
    Ok(())
  }

  type DocIdSetIterator = SortedSetDocValuesWriterDocIdSetIterator;

  fn get_doc_values(&self) -> Result<Self::DocIdSetIterator> {
    let final_ord_map = self
      .final_ord_map
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("must be finished before getting doc values"))?;
    let frozen_hash = self
      .frozen_hash
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("missing frozen hash while getting doc values"))?;
    let SortedSetOrdinals::Finished(final_ords) = &self.ords else {
      return Err(LuceneError::illegal_state(
        "missing final ordinals while getting doc values",
      ));
    };
    let pool = self
      .pool
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("must be finished before getting doc values"))?;
    SortedSetDocValuesWriter::get_values(
      final_ord_map.clone(),
      frozen_hash.clone(),
      pool.clone(),
      final_ords,
      match &self.counts {
        Some(SortedSetCounts::Finished(counts)) => Some(counts),
        _ => None,
      },
      self.max_count,
      &self.docs_with_field,
    )
  }

  fn finish(&mut self, pool: &Arc<ByteBlockPool>) -> Result<()> {
    self.pool = Some(pool.clone());
    if matches!(self.ords, SortedSetOrdinals::Pending(_)) {
      debug_assert!(
        !matches!(self.counts, Some(SortedSetCounts::Finished(_)))
          && !self.is_sorted
          && self.final_ord_map.is_none()
      );
      self.finish_current_doc()?;
      let hash = self
        .hash
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("buffered value hash is missing"))?;
      let value_count = hash.size();
      let SortedSetOrdinals::Pending(pending) = &mut self.ords else {
        return Err(LuceneError::illegal_state("missing pending ordinal values"));
      };
      self.ords = SortedSetOrdinals::Finished(pending.build()?);
      if let Some(SortedSetCounts::Pending(mut counts)) = self.counts.take() {
        self.counts = Some(SortedSetCounts::Finished(counts.build()?));
      }
      hash.sort(pool.as_ref())?;
      self.is_sorted = true;
      let mut ord_map = vec![0; value_count];
      for ord in 0..value_count {
        let index = hash.ids[ord] as usize;
        ord_map[index] = ord as i32;
      }
      self.frozen_hash = self.hash.take().map(Arc::new);
      self.final_ord_map = Some(Arc::new(ord_map));
    } else {
      debug_assert!(self.is_sorted);
    }
    self.docs_with_field.finish();
    Ok(())
  }
}
pub(crate) struct DocValuesProducerImpl1<'a> {
  field_info: &'a Arc<FieldInfo>,
  ord_map: &'a Arc<Vec<i32>>,
  hash: &'a Arc<DirectBytesRefHash>,
  pool: &'a Arc<ByteBlockPool>,
  ords: &'a PackedLongValues,
  ord_counts: &'a PackedLongValues,
  max_count: usize,
  docs_with_field: &'a DocsWithFieldSet,
  doc_ords: Option<&'a DocOrds>,
}

impl CloseableRef for DocValuesProducerImpl1<'_> {
  fn close(&self) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }
}

impl<'a> DocValuesProducerImpl1<'a> {
  #[allow(clippy::too_many_arguments)]
  pub(crate) fn new(
    field_info: &'a Arc<FieldInfo>,
    ord_map: &'a Arc<Vec<i32>>,
    hash: &'a Arc<DirectBytesRefHash>,
    pool: &'a Arc<ByteBlockPool>,
    ords: &'a PackedLongValues,
    ord_counts: &'a PackedLongValues,
    max_count: usize,
    docs_with_field: &'a DocsWithFieldSet,
    doc_ords: Option<&'a DocOrds>,
  ) -> Self {
    Self {
      field_info,
      ord_map,
      hash,
      pool,
      ords,
      ord_counts,
      max_count,
      docs_with_field,
      doc_ords,
    }
  }
}
impl<'a> DocValuesProducer for DocValuesProducerImpl1<'a> {
  type NumericDocValues = DummyNumericDocValues;
  type BinaryDocValues = DummyBinaryDocValues;
  type SortedDocValues = DummySortedDocValues;
  type SortedNumericDocValues = DummySortedNumericDocValues;
  type SortedSetDocValues = SortedSetDocValuesWriterValues<&'a PackedLongValues, &'a DocOrds>;

  fn get_sorted_set(&self, field_info: &Arc<FieldInfo>) -> Result<Self::SortedSetDocValues> {
    if !Arc::ptr_eq(self.field_info, field_info) {
      return Err(LuceneError::illegal_argument("wrong fieldInfo"));
    }
    let docs_iter = self.docs_with_field.iterator()?;
    let buf = BufferedSortedSetDocValues::new(
      self.ord_map.clone(),
      self.hash.clone(),
      self.pool.clone(),
      || self.ords,
      || self.ord_counts,
      self.max_count,
      docs_iter,
    )?;
    match &self.doc_ords {
      Some(ords) => Ok(SortedSetDocValuesWriterValues::Sorting(
        SortingSortedSetDocValues::new(buf, *ords),
      )),
      None => Ok(SortedSetDocValuesWriterValues::Buffered(buf)),
    }
  }

  type DocValuesSkipper = DummyDocValuesSkipper;
}

pub(crate) struct DocValuesProducerImpl2<'a> {
  single_value_producer: crate::core::index::sorted_doc_values_writer::DocValuesProducerImpl<'a>,
}

impl CloseableRef for DocValuesProducerImpl2<'_> {
  fn close(&self) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }
}

impl<'a> DocValuesProducerImpl2<'a> {
  pub(crate) fn new(
    single_value_producer: crate::core::index::sorted_doc_values_writer::DocValuesProducerImpl<'a>,
  ) -> Self {
    Self {
      single_value_producer,
    }
  }
}

impl<'a> DocValuesProducer for DocValuesProducerImpl2<'a> {
  type NumericDocValues = DummyNumericDocValues;
  type BinaryDocValues = DummyBinaryDocValues;
  type SortedDocValues = DummySortedDocValues;
  type SortedNumericDocValues = DummySortedNumericDocValues;
  type SortedSetDocValues =
    SingletonSortedSetDocValues<SortedDocValuesWriterValues<&'a PackedLongValues, &'a Vec<i32>>>;

  fn get_sorted_set(&self, field_info: &Arc<FieldInfo>) -> Result<Self::SortedSetDocValues> {
    DocValues::singleton_sorted(self.single_value_producer.get_sorted(field_info)?)
  }

  type DocValuesSkipper = DummyDocValuesSkipper;
}

pub(crate) struct BufferedSortedSetDocValues<D, V: Borrow<PackedLongValues> = PackedLongValues> {
  ord_map: Arc<Vec<i32>>,
  hash: Arc<DirectBytesRefHash>,
  pool: Arc<ByteBlockPool>,
  ords_iter: PackedLongValuesIterator<V>,
  ord_counts_iter: PackedLongValuesIterator<V>,
  docs_with_field: D,
  current_doc: smallvec::SmallVec<[i32; 4]>,
  max_count: usize,
  ord_count: usize,
  ord_upto: usize,
}

impl<D, V: Borrow<PackedLongValues>> BufferedSortedSetDocValues<D, V> {
  // Decode the first stream before cloning or decoding the second owned stream.
  pub(crate) fn new(
    ord_map: Arc<Vec<i32>>,
    hash: Arc<DirectBytesRefHash>,
    pool: Arc<ByteBlockPool>,
    ords: impl FnOnce() -> V,
    ord_counts: impl FnOnce() -> V,
    max_count: usize,
    docs_with_field: D,
  ) -> Result<Self> {
    Ok(Self {
      ord_map,
      hash,
      pool,
      ords_iter: PackedLongValuesIterator::new(ords())?,
      ord_counts_iter: PackedLongValuesIterator::new(ord_counts())?,
      docs_with_field,
      current_doc: smallvec::SmallVec::new(),
      max_count,
      ord_count: 0,
      ord_upto: 0,
    })
  }
}

impl<D, V: Borrow<PackedLongValues>>
  crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for BufferedSortedSetDocValues<D, V>
where
  D: DocIdSetIterator,
{
}
impl<D, V: Borrow<PackedLongValues>> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for BufferedSortedSetDocValues<D, V>
where
  D: DocIdSetIterator,
{
}

impl<D, V: Borrow<PackedLongValues>> DocIdSetIterator for BufferedSortedSetDocValues<D, V>
where
  D: DocIdSetIterator,
{
  fn doc_id(&self) -> i32 {
    self.docs_with_field.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    let doc_id = self.docs_with_field.next_doc()?;
    if doc_id != NO_MORE_DOCS {
      let count = self.ord_counts_iter.next_value()? as usize;
      debug_assert!(count > 0);
      self.ord_count = count;
      if self.current_doc.is_empty() {
        self.current_doc.resize(self.max_count, 0);
      }
      for i in 0..count {
        let raw: i32 = self.ords_iter.next_value()?.try_convert()?;
        self.current_doc[i] = self.ord_map[raw as usize];
      }
      self.current_doc[..count].sort_unstable();
      self.ord_upto = 0;
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

impl<D, V: Borrow<PackedLongValues>> DocValuesIterator for BufferedSortedSetDocValues<D, V>
where
  D: DocIdSetIterator,
{
  fn advance_exact(&mut self, _target: i32) -> Result<bool> {
    Err(LuceneError::unsupported_operation(""))
  }
}

impl<D, V: Borrow<PackedLongValues>> SortedSetDocValues for BufferedSortedSetDocValues<D, V>
where
  D: DocIdSetIterator,
{
  type OrdValue<'a>
    = BytesRef<&'a [u8]>
  where
    Self: 'a;

  fn next_ord(&mut self) -> Result<i64> {
    let ord = self.current_doc[self.ord_upto] as i64;
    self.ord_upto += 1;
    Ok(ord)
  }

  fn doc_value_count(&mut self) -> Result<i32> {
    Ok(self.ord_count as i32)
  }

  fn lookup_ord(&mut self, ord: i64) -> Result<Self::OrdValue<'_>> {
    debug_assert!(ord >= 0 && (ord as usize) < self.ord_map.len());
    let idx: i32 = ord.try_convert()?;
    let hash_idx = self.hash.ids[idx as usize];
    let position = self.hash.get(hash_idx, self.pool.as_ref())?;
    let block = self.pool.get_buffer(position.block_index);
    Ok(BytesRef {
      bytes: block,
      offset: position.offset,
      length: position.length,
    })
  }

  fn get_value_count(&self) -> Result<i64> {
    Ok(self.ord_map.len() as i64)
  }

  type TermsEnum<'a>
    = SortedSetDocValuesTermsEnum<&'a mut Self>
  where
    Self: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    self.default_terms_enum()
  }

  type SortedDocValues = DummySortedDocValues;
}

pub struct SortingSortedSetDocValues<S, O: Borrow<DocOrds> = DocOrds> {
  input: S,
  ords: O,
  doc_id: i32,
  ord_upto: Option<usize>,
  count: i32,
}

impl<S, O: Borrow<DocOrds>> SortingSortedSetDocValues<S, O> {
  pub(crate) fn new(input: S, ords: O) -> Self {
    Self {
      input,
      ords,
      doc_id: -1,
      ord_upto: Some(0),
      count: 0,
    }
  }

  fn init_count(&mut self) -> Result<()> {
    let doc_id = self.doc_id.try_convert()?;
    self.ord_upto = self.ords.borrow().offsets[doc_id].checked_sub(1);
    self.count = self.ords.borrow().doc_value_counts.get(doc_id) as i32;
    Ok(())
  }
}

impl<S, O: Borrow<DocOrds>> DocValuesIterator for SortingSortedSetDocValues<S, O>
where
  S: SortedSetDocValues,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    // Needed by `IndexSorter::StringSorter`.
    self.doc_id = target;
    self.init_count()?;
    Ok(self.ords.borrow().offsets[self.doc_id as usize] > 0)
  }
}

impl<S, O: Borrow<DocOrds>> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SortingSortedSetDocValues<S, O>
where
  S: SortedSetDocValues,
{
}
impl<S, O: Borrow<DocOrds>> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for SortingSortedSetDocValues<S, O>
where
  S: SortedSetDocValues,
{
}

impl<S, O: Borrow<DocOrds>> DocIdSetIterator for SortingSortedSetDocValues<S, O>
where
  S: SortedSetDocValues,
{
  fn doc_id(&self) -> i32 {
    self.doc_id
  }

  fn next_doc(&mut self) -> Result<i32> {
    loop {
      self.doc_id += 1;
      let doc_index = self.doc_id as usize;
      if doc_index == self.ords.borrow().offsets.len() {
        self.doc_id = NO_MORE_DOCS;
        return Ok(self.doc_id);
      }
      if self.ords.borrow().offsets[doc_index] > 0 {
        break;
      }
    }
    self.init_count()?;
    Ok(self.doc_id)
  }

  fn advance(&mut self, _target: i32) -> Result<i32> {
    Err(LuceneError::unsupported_operation("use next_doc instead"))
  }

  fn cost(&self) -> Result<i64> {
    self.input.cost()
  }
}

impl<S, O: Borrow<DocOrds>> SortedSetDocValues for SortingSortedSetDocValues<S, O>
where
  S: SortedSetDocValues,
{
  type OrdValue<'a>
    = S::OrdValue<'a>
  where
    Self: 'a;

  fn next_ord(&mut self) -> Result<i64> {
    let ord_upto = self
      .ord_upto
      .ok_or_else(|| LuceneError::illegal_state("value -1 does not fit into usize"))?;
    let ord = self.ords.borrow().ords.get(ord_upto)?;
    self.ord_upto = Some(ord_upto + 1);
    Ok(ord)
  }

  fn doc_value_count(&mut self) -> Result<i32> {
    debug_assert!(self.doc_id >= 0);
    Ok(self.count)
  }

  fn lookup_ord(&mut self, ord: i64) -> Result<Self::OrdValue<'_>> {
    self.input.lookup_ord(ord)
  }

  fn get_value_count(&self) -> Result<i64> {
    self.input.get_value_count()
  }

  type TermsEnum<'a>
    = SortedSetDocValuesTermsEnum<&'a mut Self>
  where
    Self: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    self.default_terms_enum()
  }

  type SortedDocValues = DummySortedDocValues;
}

pub struct DocOrds {
  pub(crate) offsets: Vec<usize>,
  pub(crate) ords: PackedLongValues,
  pub(crate) doc_value_counts: GrowableWriter,
}
pub const START_BITS_PER_VALUE: i32 = 2;
impl DocOrds {
  pub(crate) fn new<DM, T>(
    max_doc: i32,
    sort_map: &DM,
    old_values: &mut T,
    acceptable_overhead_ratio: f32,
    bits_per_value: i32,
  ) -> Result<Self>
  where
    DM: DocMap,
    T: SortedSetDocValues,
  {
    let mut offsets = vec![0; max_doc as usize];
    let mut builder =
      PackedLongValues::packed_long_values_builder_default(acceptable_overhead_ratio)?;
    let mut doc_value_counts =
      GrowableWriter::new(bits_per_value, max_doc as usize, acceptable_overhead_ratio)?;
    let mut ord_offset = 1;
    loop {
      let doc_id = old_values.next_doc()?;
      if doc_id == NO_MORE_DOCS {
        break;
      }

      let new_doc_id = sort_map.old_to_new(doc_id)? as usize;
      let start_offset = ord_offset;
      let doc_value_count = old_values.doc_value_count()?;
      ord_offset += doc_value_count as usize;

      for _ in 0..doc_value_count {
        builder.add(old_values.next_ord()?)?;
      }

      doc_value_counts.set(new_doc_id, (ord_offset - start_offset) as i64)?;

      if start_offset != ord_offset {
        // do we have any values?
        offsets[new_doc_id] = start_offset;
      }
    }
    let ords = builder.build()?;

    Ok(DocOrds {
      offsets,
      ords,
      doc_value_counts,
    })
  }
}

// SortedSetDocValues
pub enum SortedSetDocValuesEnum2<A, B> {
  A(A),
  B(B),
}

impl<A, B> DocValuesIterator for SortedSetDocValuesEnum2<A, B>
where
  A: DocValuesIterator,
  B: DocValuesIterator,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      SortedSetDocValuesEnum2::A(t) => t.advance_exact(target),
      SortedSetDocValuesEnum2::B(s) => s.advance_exact(target),
    }
  }
}

impl<A, B> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SortedSetDocValuesEnum2<A, B>
where
  A: DocIdSetIterator,
  B: DocIdSetIterator,
{
}
impl<A, B> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for SortedSetDocValuesEnum2<A, B>
where
  A: DocIdSetIterator,
  B: DocIdSetIterator,
{
}

impl<A, B> DocIdSetIterator for SortedSetDocValuesEnum2<A, B>
where
  A: DocIdSetIterator,
  B: DocIdSetIterator,
{
  fn doc_id(&self) -> i32 {
    match self {
      SortedSetDocValuesEnum2::A(t) => t.doc_id(),
      SortedSetDocValuesEnum2::B(s) => s.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      SortedSetDocValuesEnum2::A(t) => t.next_doc(),
      SortedSetDocValuesEnum2::B(s) => s.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      SortedSetDocValuesEnum2::A(t) => t.advance(target),
      SortedSetDocValuesEnum2::B(s) => s.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      SortedSetDocValuesEnum2::A(t) => t.slow_advance(target),
      SortedSetDocValuesEnum2::B(s) => s.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      SortedSetDocValuesEnum2::A(t) => t.cost(),
      SortedSetDocValuesEnum2::B(s) => s.cost(),
    }
  }
}

impl<A, B> SortedSetDocValues for SortedSetDocValuesEnum2<A, B>
where
  A: SortedSetDocValues,
  B: SortedSetDocValues,
{
  type OrdValue<'a>
    = BytesRefValueEnum2<A::OrdValue<'a>, B::OrdValue<'a>>
  where
    Self: 'a;

  fn next_ord(&mut self) -> Result<i64> {
    match self {
      SortedSetDocValuesEnum2::A(t) => t.next_ord(),
      SortedSetDocValuesEnum2::B(s) => s.next_ord(),
    }
  }

  fn doc_value_count(&mut self) -> Result<i32> {
    match self {
      SortedSetDocValuesEnum2::A(t) => t.doc_value_count(),
      SortedSetDocValuesEnum2::B(s) => s.doc_value_count(),
    }
  }

  fn lookup_ord(&mut self, _ord: i64) -> Result<Self::OrdValue<'_>> {
    match self {
      SortedSetDocValuesEnum2::A(t) => t.lookup_ord(_ord).map(BytesRefValueEnum2::A),
      SortedSetDocValuesEnum2::B(s) => s.lookup_ord(_ord).map(BytesRefValueEnum2::B),
    }
  }

  fn get_value_count(&self) -> Result<i64> {
    match self {
      SortedSetDocValuesEnum2::A(t) => t.get_value_count(),
      SortedSetDocValuesEnum2::B(s) => s.get_value_count(),
    }
  }

  fn lookup_term<BS: ByteSource>(&mut self, key: &BytesRef<BS>) -> Result<i64> {
    match self {
      SortedSetDocValuesEnum2::A(t) => t.lookup_term(key),
      SortedSetDocValuesEnum2::B(s) => s.lookup_term(key),
    }
  }

  type TermsEnum<'a>
    = SortedSetDocValuesTermsEnum<&'a mut Self>
  where
    A: 'a,
    B: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    self.default_terms_enum()
  }

  fn is_single_valued(&self) -> bool {
    match self {
      SortedSetDocValuesEnum2::A(t) => t.is_single_valued(),
      SortedSetDocValuesEnum2::B(s) => s.is_single_valued(),
    }
  }

  type SortedDocValues = SortedDocValuesEnum2<A::SortedDocValues, B::SortedDocValues>;

  fn get_sorted_doc_values(&mut self) -> Result<Self::SortedDocValues> {
    match self {
      SortedSetDocValuesEnum2::A(t) => Ok(SortedDocValuesEnum2::A(t.get_sorted_doc_values()?)),
      SortedSetDocValuesEnum2::B(s) => Ok(SortedDocValuesEnum2::B(s.get_sorted_doc_values()?)),
    }
  }
}

pub enum SortedSetDocValuesEnum2WithUnsupportedSecondSorted<A, B> {
  A(A),
  B(B),
}

impl<A, B> DocValuesIterator for SortedSetDocValuesEnum2WithUnsupportedSecondSorted<A, B>
where
  A: DocValuesIterator,
  B: DocValuesIterator,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::A(values) => values.advance_exact(target),
      Self::B(values) => values.advance_exact(target),
    }
  }
}

impl<A, B> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SortedSetDocValuesEnum2WithUnsupportedSecondSorted<A, B>
where
  A: DocIdSetIterator,
  B: DocIdSetIterator,
{
}
impl<A, B> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for SortedSetDocValuesEnum2WithUnsupportedSecondSorted<A, B>
where
  A: DocIdSetIterator,
  B: DocIdSetIterator,
{
}

impl<A, B> DocIdSetIterator for SortedSetDocValuesEnum2WithUnsupportedSecondSorted<A, B>
where
  A: DocIdSetIterator,
  B: DocIdSetIterator,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::A(values) => values.doc_id(),
      Self::B(values) => values.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::A(values) => values.next_doc(),
      Self::B(values) => values.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::A(values) => values.advance(target),
      Self::B(values) => values.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::A(values) => values.slow_advance(target),
      Self::B(values) => values.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::A(values) => values.cost(),
      Self::B(values) => values.cost(),
    }
  }
}

impl<A, B> SortedSetDocValues for SortedSetDocValuesEnum2WithUnsupportedSecondSorted<A, B>
where
  A: SortedSetDocValues,
  B: SortedSetDocValues,
{
  type OrdValue<'a>
    = BytesRefValueEnum2<A::OrdValue<'a>, B::OrdValue<'a>>
  where
    Self: 'a;

  fn next_ord(&mut self) -> Result<i64> {
    match self {
      Self::A(values) => values.next_ord(),
      Self::B(values) => values.next_ord(),
    }
  }

  fn doc_value_count(&mut self) -> Result<i32> {
    match self {
      Self::A(values) => values.doc_value_count(),
      Self::B(values) => values.doc_value_count(),
    }
  }

  fn lookup_ord(&mut self, ord: i64) -> Result<Self::OrdValue<'_>> {
    match self {
      Self::A(values) => values.lookup_ord(ord).map(BytesRefValueEnum2::A),
      Self::B(values) => values.lookup_ord(ord).map(BytesRefValueEnum2::B),
    }
  }

  fn get_value_count(&self) -> Result<i64> {
    match self {
      Self::A(values) => values.get_value_count(),
      Self::B(values) => values.get_value_count(),
    }
  }

  fn lookup_term<BS: ByteSource>(&mut self, key: &BytesRef<BS>) -> Result<i64> {
    match self {
      Self::A(values) => values.lookup_term(key),
      Self::B(values) => values.lookup_term(key),
    }
  }

  type TermsEnum<'a>
    = SortedSetDocValuesTermsEnum<&'a mut Self>
  where
    A: 'a,
    B: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    self.default_terms_enum()
  }

  fn is_single_valued(&self) -> bool {
    match self {
      Self::A(values) => values.is_single_valued(),
      Self::B(values) => values.is_single_valued(),
    }
  }

  type SortedDocValues = A::SortedDocValues;

  fn get_sorted_doc_values(&mut self) -> Result<Self::SortedDocValues> {
    match self {
      Self::A(values) => values.get_sorted_doc_values(),
      Self::B(values) => match values.get_sorted_doc_values() {
        Err(error) => Err(error),
        Ok(_) => Err(LuceneError::illegal_state(
          "the second SortedSetDocValues unexpectedly returned sorted doc values",
        )),
      },
    }
  }
}

pub enum SortedSetDocValuesWithEmpty<A> {
  A(A),
  B(EmptySortedSet),
}

impl<A> DocValuesIterator for SortedSetDocValuesWithEmpty<A>
where
  A: DocValuesIterator,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::A(inner) => inner.advance_exact(target),
      Self::B(inner) => inner.advance_exact(target),
    }
  }
}

impl<A> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SortedSetDocValuesWithEmpty<A>
where
  A: DocIdSetIterator,
{
}
impl<A> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for SortedSetDocValuesWithEmpty<A>
where
  A: DocIdSetIterator,
{
}

impl<A> DocIdSetIterator for SortedSetDocValuesWithEmpty<A>
where
  A: DocIdSetIterator,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::A(inner) => inner.doc_id(),
      Self::B(inner) => inner.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::A(inner) => inner.next_doc(),
      Self::B(inner) => inner.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::A(inner) => inner.advance(target),
      Self::B(inner) => inner.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::A(inner) => inner.slow_advance(target),
      Self::B(inner) => inner.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::A(inner) => inner.cost(),
      Self::B(inner) => inner.cost(),
    }
  }
}

impl<A> SortedSetDocValues for SortedSetDocValuesWithEmpty<A>
where
  A: SortedSetDocValues,
{
  type OrdValue<'a>
    = BytesRefValueEnum2<A::OrdValue<'a>, <EmptySortedSet as SortedSetDocValues>::OrdValue<'a>>
  where
    Self: 'a;

  fn next_ord(&mut self) -> Result<i64> {
    match self {
      Self::A(inner) => inner.next_ord(),
      Self::B(inner) => inner.next_ord(),
    }
  }

  fn doc_value_count(&mut self) -> Result<i32> {
    match self {
      Self::A(inner) => inner.doc_value_count(),
      Self::B(inner) => inner.doc_value_count(),
    }
  }

  fn lookup_ord(&mut self, ord: i64) -> Result<Self::OrdValue<'_>> {
    match self {
      Self::A(inner) => inner.lookup_ord(ord).map(BytesRefValueEnum2::A),
      Self::B(inner) => inner.lookup_ord(ord).map(BytesRefValueEnum2::B),
    }
  }

  fn get_value_count(&self) -> Result<i64> {
    match self {
      Self::A(inner) => inner.get_value_count(),
      Self::B(inner) => inner.get_value_count(),
    }
  }

  fn lookup_term<BS: ByteSource>(&mut self, key: &BytesRef<BS>) -> Result<i64> {
    match self {
      Self::A(inner) => inner.lookup_term(key),
      Self::B(inner) => inner.lookup_term(key),
    }
  }

  type TermsEnum<'a>
    = SortedSetDocValuesTermsEnum<&'a mut Self>
  where
    A: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    self.default_terms_enum()
  }

  fn is_single_valued(&self) -> bool {
    match self {
      Self::A(inner) => inner.is_single_valued(),
      Self::B(inner) => inner.is_single_valued(),
    }
  }

  type SortedDocValues = SortedDocValuesWithEmpty<A::SortedDocValues>;

  fn get_sorted_doc_values(&mut self) -> Result<Self::SortedDocValues> {
    match self {
      Self::A(inner) => inner
        .get_sorted_doc_values()
        .map(SortedDocValuesWithEmpty::A),
      Self::B(inner) => inner
        .get_sorted_doc_values()
        .map(SortedDocValuesWithEmpty::B),
    }
  }
}

pub enum SingletonOrMultiSortedSetDocValuesEnum<A, B> {
  Singleton(A),
  Multi(B),
}

impl<A, B> DocValuesIterator for SingletonOrMultiSortedSetDocValuesEnum<A, B>
where
  A: DocValuesIterator,
  B: DocValuesIterator,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::Singleton(values) => values.advance_exact(target),
      Self::Multi(values) => values.advance_exact(target),
    }
  }
}

impl<A, B> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SingletonOrMultiSortedSetDocValuesEnum<A, B>
where
  A: DocIdSetIterator,
  B: DocIdSetIterator,
{
}
impl<A, B> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for SingletonOrMultiSortedSetDocValuesEnum<A, B>
where
  A: DocIdSetIterator,
  B: DocIdSetIterator,
{
}

impl<A, B> DocIdSetIterator for SingletonOrMultiSortedSetDocValuesEnum<A, B>
where
  A: DocIdSetIterator,
  B: DocIdSetIterator,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::Singleton(values) => values.doc_id(),
      Self::Multi(values) => values.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::Singleton(values) => values.next_doc(),
      Self::Multi(values) => values.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Singleton(values) => values.advance(target),
      Self::Multi(values) => values.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Singleton(values) => values.slow_advance(target),
      Self::Multi(values) => values.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::Singleton(values) => values.cost(),
      Self::Multi(values) => values.cost(),
    }
  }
}

impl<A, B> SortedSetDocValues for SingletonOrMultiSortedSetDocValuesEnum<A, B>
where
  A: SortedSetDocValues,
  B: SortedSetDocValues,
{
  type OrdValue<'a>
    = BytesRefValueEnum2<A::OrdValue<'a>, B::OrdValue<'a>>
  where
    Self: 'a;

  fn next_ord(&mut self) -> Result<i64> {
    match self {
      Self::Singleton(values) => values.next_ord(),
      Self::Multi(values) => values.next_ord(),
    }
  }

  fn append_ords(&mut self, ords: &mut Vec<i64>, count: usize) -> Result<()> {
    match self {
      Self::Singleton(values) => values.append_ords(ords, count),
      Self::Multi(values) => values.append_ords(ords, count),
    }
  }

  fn doc_value_count(&mut self) -> Result<i32> {
    match self {
      Self::Singleton(values) => values.doc_value_count(),
      Self::Multi(values) => values.doc_value_count(),
    }
  }

  fn lookup_ord(&mut self, ord: i64) -> Result<Self::OrdValue<'_>> {
    match self {
      Self::Singleton(values) => values.lookup_ord(ord).map(BytesRefValueEnum2::A),
      Self::Multi(values) => values.lookup_ord(ord).map(BytesRefValueEnum2::B),
    }
  }

  fn get_value_count(&self) -> Result<i64> {
    match self {
      Self::Singleton(values) => values.get_value_count(),
      Self::Multi(values) => values.get_value_count(),
    }
  }

  fn lookup_term<BS: ByteSource>(&mut self, key: &BytesRef<BS>) -> Result<i64> {
    match self {
      Self::Singleton(values) => values.lookup_term(key),
      Self::Multi(values) => values.lookup_term(key),
    }
  }

  type TermsEnum<'a>
    = SortedSetDocValuesTermsEnum<&'a mut Self>
  where
    A: 'a,
    B: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    self.default_terms_enum()
  }

  fn is_single_valued(&self) -> bool {
    match self {
      Self::Singleton(values) => values.is_single_valued(),
      Self::Multi(values) => values.is_single_valued(),
    }
  }

  type SortedDocValues = A::SortedDocValues;

  fn get_sorted_doc_values(&mut self) -> Result<Self::SortedDocValues> {
    match self {
      Self::Singleton(values) => values.get_sorted_doc_values(),
      Self::Multi(_) => Err(LuceneError::unsupported_operation("")),
    }
  }
}
