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
use crate::core::index::index_reader::Identity;
use crate::core::search::doc_id_set::DocIdSet;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, EmptyDISI};
use crate::core::util::HasIdentity;
use crate::core::util::accountable::Accountable;
use crate::core::util::bit_set::{BitSet, BitSetExtensions};
use crate::core::util::bit_set_iterator::BitSetIterator;
use crate::core::util::bits::Bits;
use crate::core::util::dummy::dummy_bits::DummyBits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::not_doc_id_set::NotDocDocIdSetIterator;
use crate::core::util::ram_usage_estimator::size_of_vec;
use crate::core::util::sparse_fixed_bit_set::SparseFixedBitSet;
use crate::either_docidsetiterator_named;
use std::borrow::Borrow;
use std::mem;
use std::sync::Arc;

// Number of documents in a block
const BLOCK_SIZE: usize = 1 << 16;
// The maximum length for an array, beyond that point we switch to a bitset
const MAX_ARRAY_LENGTH: usize = 1 << 12;
/// [`DocIdSet`] implementation inspired by [roaringbitmap.org](http://roaringbitmap.org/)
///
/// The space is divided into blocks of `2^16` bits, and each block is encoded
/// independently. In each block, if fewer than `2^12` bits are set, documents
/// are simply stored in a `Vec<i16>`. If more than `2^16 - 2^12` bits are set,
/// the inverse of the set is encoded in a simple `Vec<i16>`. Otherwise,
/// a [`FixedBitSet`] is used.
///
/// # Note
/// This is an internal API.
pub struct RoaringDocIdSet {
  doc_id_sets: Arc<Vec<Option<DocIdSetEnum>>>,
  cardinality: usize,
}
impl RoaringDocIdSet {
  fn new(doc_id_sets: Vec<Option<DocIdSetEnum>>, cardinality: usize) -> Self {
    RoaringDocIdSet {
      doc_id_sets: Arc::new(doc_id_sets),
      cardinality,
    }
  }

  pub(crate) fn cardinality(&self) -> usize {
    self.cardinality
  }
}
impl DocIdSet for RoaringDocIdSet {
  type DocIdSetIterator = Iterator;

  fn iterator(&self) -> Result<Self::DocIdSetIterator> {
    Ok(Iterator::new(
      self.doc_id_sets.clone(),
      self.cardinality as i64,
    ))
  }

  type Bits = DummyBits;

  fn bits(&self) -> Option<Self::Bits> {
    None
  }
}

impl Accountable for RoaringDocIdSet {
  fn ram_bytes_used(&self) -> Result<i64> {
    let mut size = (mem::size_of_val(self.doc_id_sets.as_ref()) as i64)
      .saturating_add(size_of_vec(self.doc_id_sets.as_ref()));
    for doc_id_set in self.doc_id_sets.iter().flatten() {
      size = size.saturating_add(doc_id_set.ram_bytes_used()?);
    }
    Ok(size)
  }
}
pub struct Builder {
  max_doc: usize,
  sets: Vec<Option<DocIdSetEnum>>,
  cardinality: usize,
  last_doc_id: i32,
  current_block: Option<usize>,
  current_block_cardinality: usize,
  // We start by filling the buffer and when it's full we copy the
  // content of the buffer to the FixedBitSet and put further
  // documents in that bitset
  buffer: Vec<i16>,
  dense_buffer: FixedBitSet,
}

impl Builder {
  pub fn new(max_doc: usize) -> Builder {
    Builder {
      max_doc,
      sets: Vec::new(),
      cardinality: 0,
      last_doc_id: -1,
      current_block: None,
      current_block_cardinality: 0,
      buffer: Vec::new(),
      dense_buffer: FixedBitSet::new(0),
    }
  }
  /// Add a new doc-id to this builder. NOTE: doc ids must be added in
  /// order.
  pub fn add(&mut self, doc_id: i32) -> Result<()> {
    if doc_id <= self.last_doc_id {
      return Err(LuceneError::illegal_argument(format!(
        "Doc ids must be added in-order, got {doc_id} which is <= lastDocID={}",
        self.last_doc_id
      )));
    }
    let block = (doc_id >> 16) as usize;
    if Some(block) != self.current_block {
      if self.current_block.is_none() {
        let sets_length = (self.max_doc + (1 << 16) - 1) >> 16;
        self.sets.resize_with(sets_length, || None);
      }
      self.flush(false)?;
      self.current_block = Some(block);
    }

    if self.current_block_cardinality < MAX_ARRAY_LENGTH {
      // self.buffer[self.current_block_cardinality as usize] = doc_id
      // as i16;
      self.buffer.push(doc_id as i16);
    } else {
      if self.dense_buffer.length() == 0 {
        // the buffer is full, let's move to a fixed bit set
        let num_bits = std::cmp::min(1 << 16, self.max_doc - (block << 16));
        self.dense_buffer = FixedBitSet::new(num_bits);
        for i in 0..self.buffer.len() {
          self.dense_buffer.set(self.buffer[i] as usize & 0xFFFF)?;
        }
      }
      self.dense_buffer.set(doc_id as usize & 0xFFFF)?;
    }
    self.last_doc_id = doc_id;
    self.current_block_cardinality += 1;
    Ok(())
  }
  /// Add the content of the provided DocIdSetIterator.
  pub fn add_disi<T>(&mut self, mut disi: T) -> Result<()>
  where
    T: DocIdSetIterator,
  {
    let mut doc = disi.next_doc()?;
    while doc != NO_MORE_DOCS {
      self.add(doc)?;
      doc = disi.next_doc()?;
    }
    Ok(())
  }
  /// Build an instance, consuming this builder.
  pub fn build(mut self) -> Result<RoaringDocIdSet> {
    self.flush(true)?;
    Ok(RoaringDocIdSet::new(self.sets, self.cardinality))
  }
  fn flush(&mut self, last_block: bool) -> Result<()> {
    debug_assert!(self.current_block_cardinality <= BLOCK_SIZE);
    if self.current_block_cardinality <= MAX_ARRAY_LENGTH {
      // use sparse encoding
      debug_assert_eq!(self.dense_buffer.length(), 0);
      if self.current_block_cardinality > 0 {
        let doc_ids = if last_block
          && self.buffer.len() == self.current_block_cardinality
          && self.buffer.capacity() == self.current_block_cardinality
        {
          std::mem::take(&mut self.buffer)
        } else {
          self.buffer[..self.current_block_cardinality].to_vec()
        };
        let sparse = Some(DocIdSetEnum::Sparse(ShortArrayDocIdSet::new(doc_ids)));
        self.buffer.clear();
        debug_assert!(self.buffer.is_empty());
        self.sets[self.current_block.ok_or_else(|| {
          LuceneError::illegal_state("non-empty roaring block has no current block")
        })?] = sparse;
      }
    } else {
      debug_assert_ne!(self.dense_buffer.length(), 0);
      debug_assert_eq!(
        self.dense_buffer.cardinality(),
        self.current_block_cardinality
      );
      if self.dense_buffer.length() == BLOCK_SIZE
        && BLOCK_SIZE - self.current_block_cardinality < MAX_ARRAY_LENGTH
      {
        let capacity = BLOCK_SIZE - self.current_block_cardinality;
        let mut excluded_docs: Vec<i16> = Vec::with_capacity(capacity);
        self.dense_buffer.flip_range(0, self.dense_buffer.length());
        let mut excluded_doc: usize = 0;

        for _ in 0..capacity {
          let v = self.dense_buffer.next_set_bit(excluded_doc);
          debug_assert_ne!(v, NO_MORE_DOCS as usize);
          excluded_docs.push(v as i16);
          excluded_doc = v + 1;
        }

        debug_assert!(
          excluded_doc == self.dense_buffer.length()
            || self.dense_buffer.next_set_bit(excluded_doc) == NO_MORE_DOCS as usize
        );
        let dense: Option<DocIdSetEnum> =
          Some(DocIdSetEnum::Dense(ShortArrayDocIdSet::new(excluded_docs)));
        self.buffer.clear();
        self.sets[self.current_block.ok_or_else(|| {
          LuceneError::illegal_state("non-empty roaring block has no current block")
        })?] = dense;
      } else {
        let result = std::mem::take(&mut self.dense_buffer);

        let medium: Option<DocIdSetEnum> = Some(DocIdSetEnum::Medium(
          result,
          self.current_block_cardinality as i64,
        ));
        self.buffer.clear();
        self.sets[self.current_block.ok_or_else(|| {
          LuceneError::illegal_state("non-empty roaring block has no current block")
        })?] = medium;
      }
    }
    self.cardinality += self.current_block_cardinality;
    self.dense_buffer = FixedBitSet::new(0);
    self.current_block_cardinality = 0;
    Ok(())
  }
}

pub struct ShortArrayDocIdSet<D = Arc<Vec<i16>>> {
  doc_ids: D,
}
impl<D> ShortArrayDocIdSet<D> {
  fn new(doc_ids: D) -> Self {
    ShortArrayDocIdSet { doc_ids }
  }
}

impl Accountable for ShortArrayDocIdSet {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(
      (mem::size_of_val(self.doc_ids.as_ref()) as i64)
        .saturating_add(size_of_vec(self.doc_ids.as_ref())),
    )
  }
}

impl Accountable for ShortArrayDocIdSet<Vec<i16>> {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(size_of_vec(&self.doc_ids))
  }
}

impl DocIdSet for ShortArrayDocIdSet {
  type DocIdSetIterator = ShortArrayDISI;

  fn iterator(&self) -> Result<Self::DocIdSetIterator> {
    Ok(ShortArrayDISI::new(self.doc_ids.clone()))
  }

  type Bits = DummyBits;

  fn bits(&self) -> Option<Self::Bits> {
    None
  }
}

pub struct ShortArrayDISI<D = Arc<Vec<i16>>> {
  i: Option<usize>,
  doc: i32,
  doc_ids: D,
}
impl<D: Borrow<Vec<i16>>> ShortArrayDISI<D> {
  fn new(doc_ids: D) -> Self {
    ShortArrayDISI {
      i: None,
      doc: -1,
      doc_ids,
    }
  }
  fn doc_id_index(&self, i: usize) -> i32 {
    <D as Borrow<Vec<i16>>>::borrow(&self.doc_ids)[i] as i32 & 0xFFFF
  }
}
impl<D: Borrow<Vec<i16>>> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for ShortArrayDISI<D>
{
}
impl<D: Borrow<Vec<i16>>> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for ShortArrayDISI<D>
{
}

impl<D: Borrow<Vec<i16>>> DocIdSetIterator for ShortArrayDISI<D> {
  fn doc_id(&self) -> i32 {
    self.doc
  }

  fn next_doc(&mut self) -> Result<i32> {
    let i = self.i.map_or(0, |i| i + 1);
    self.i = Some(i);
    if i >= <D as Borrow<Vec<i16>>>::borrow(&self.doc_ids).len() {
      self.doc = NO_MORE_DOCS;
      return Ok(self.doc);
    }
    self.doc = self.doc_id_index(i);
    Ok(self.doc)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    let mut lo = self.i.map_or(0, |i| i + 1);
    let mut hi = <D as Borrow<Vec<i16>>>::borrow(&self.doc_ids).len();
    while lo < hi {
      // Preserve the inclusive-range midpoint with an exclusive upper bound.
      let mid = lo + ((hi - lo - 1) >> 1);
      let mid_doc = self.doc_id_index(mid);
      if mid_doc < target {
        lo = mid + 1;
      } else {
        hi = mid;
      }
    }
    if lo == <D as Borrow<Vec<i16>>>::borrow(&self.doc_ids).len() {
      self.i = Some(<D as Borrow<Vec<i16>>>::borrow(&self.doc_ids).len());
      self.doc = NO_MORE_DOCS;
    } else {
      self.i = Some(lo);
      self.doc = self.doc_id_index(lo);
    }
    Ok(self.doc)
  }

  fn cost(&self) -> Result<i64> {
    Ok(<D as Borrow<Vec<i16>>>::borrow(&self.doc_ids).len() as i64)
  }
}

pub struct Iterator {
  block: Option<usize>,
  doc: i32,
  set_length: usize,
  sub: Disi,
  doc_id_sets: Arc<Vec<Option<DocIdSetEnum>>>,
  cardinality: i64,
}
impl Iterator {
  fn new(doc_id_sets: Arc<Vec<Option<DocIdSetEnum>>>, cardinality: i64) -> Self {
    let set_length = doc_id_sets.len();
    Iterator {
      block: None,
      doc: -1,
      set_length,
      sub: DocIdSetIteratorEnum::Empty(EmptyDISI::new()),
      doc_id_sets,
      cardinality,
    }
  }
  fn first_doc_from_next_block(&mut self) -> Result<i32> {
    loop {
      let block = self.block.map_or(0, |block| block + 1);
      self.block = Some(block);
      if block >= self.set_length {
        self.doc = NO_MORE_DOCS;
        break;
      }

      if let Some(doc_id_set) = self.doc_id_sets[block].as_ref() {
        self.sub = doc_id_set.iterator(&self.doc_id_sets, block)?;
        let sub_next = self.sub.next_doc()?;
        debug_assert!(sub_next != NO_MORE_DOCS);
        self.doc = ((block as i32) << 16) | sub_next;
        break;
      }
    }
    Ok(self.doc)
  }
}
impl crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions for Iterator {}
impl crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for Iterator {}

impl DocIdSetIterator for Iterator {
  fn doc_id(&self) -> i32 {
    self.doc
  }

  fn next_doc(&mut self) -> Result<i32> {
    let sub_next = self.sub.next_doc()?;
    if sub_next == NO_MORE_DOCS {
      return self.first_doc_from_next_block();
    }
    self.doc = ((self
      .block
      .ok_or_else(|| LuceneError::illegal_state("roaring iterator has no current block"))?
      as i32)
      << 16)
      | sub_next;
    Ok(self.doc)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    let target_block = (target >> 16) as usize;
    if Some(target_block) != self.block {
      self.block = Some(target_block);
      if target_block >= self.doc_id_sets.len() {
        self.doc = NO_MORE_DOCS;
        return Ok(self.doc);
      }
      let slot = &self.doc_id_sets[target_block];
      match slot.as_ref() {
        None => return self.first_doc_from_next_block(),
        Some(doc_id_set) => {
          self.sub = doc_id_set.iterator(&self.doc_id_sets, target_block)?;
        },
      }
    }
    let sub_next = self.sub.advance(target & 0xFFFF)?;
    if sub_next == NO_MORE_DOCS {
      return self.first_doc_from_next_block();
    }
    self.doc = ((self
      .block
      .ok_or_else(|| LuceneError::illegal_state("roaring iterator has no current block"))?
      as i32)
      << 16)
      | sub_next;
    Ok(self.doc)
  }

  fn cost(&self) -> Result<i64> {
    Ok(self.cardinality)
  }
}
enum DocIdSetEnum {
  Sparse(ShortArrayDocIdSet<Vec<i16>>),
  Medium(FixedBitSet, i64),
  Dense(ShortArrayDocIdSet<Vec<i16>>),
}
impl Accountable for DocIdSetEnum {
  fn ram_bytes_used(&self) -> Result<i64> {
    match self {
      Self::Sparse(set) => set.ram_bytes_used(),
      Self::Medium(set, _) => set.ram_bytes_used(),
      Self::Dense(set) => set.ram_bytes_used(),
    }
  }
}

impl DocIdSetEnum {
  fn iterator(&self, directory: &Arc<Vec<Option<DocIdSetEnum>>>, block: usize) -> Result<Disi> {
    match self {
      Self::Sparse(_) => Ok(DocIdSetIteratorEnum::Sparse(ShortArrayDISI::new(
        RoaringShortValues {
          directory: directory.clone(),
          block,
        },
      ))),
      Self::Medium(_, cost) => Ok(DocIdSetIteratorEnum::Medium(BitSetIterator::new(
        RoaringBitSetView {
          directory: directory.clone(),
          block,
        },
        *cost,
      )?)),
      Self::Dense(_) => Ok(DocIdSetIteratorEnum::Dense(NotDocDocIdSetIterator::new(
        ShortArrayDISI::new(RoaringShortValues {
          directory: directory.clone(),
          block,
        }),
        BLOCK_SIZE as i32,
      ))),
    }
  }
}
either_docidsetiterator_named!(pub DocIdSetIteratorEnum{ Sparse: A, Medium: B,Dense:C,Empty:D});
pub type Disi = DocIdSetIteratorEnum<
  ShortArrayDISI<RoaringShortValues>,
  BitSetIterator<RoaringBitSetView>,
  NotDocDocIdSetIterator<ShortArrayDISI<RoaringShortValues>>,
  EmptyDISI,
>;

/// Retains a sparse or inverted sparse block in its immutable directory.
pub struct RoaringShortValues {
  directory: Arc<Vec<Option<DocIdSetEnum>>>,
  block: usize,
}
impl Borrow<Vec<i16>> for RoaringShortValues {
  fn borrow(&self) -> &Vec<i16> {
    // Only DocIdSetEnum::iterator creates this view, for the selected immutable block.
    match &self.directory[self.block] {
      Some(DocIdSetEnum::Sparse(values) | DocIdSetEnum::Dense(values)) => &values.doc_ids,
      _ => panic_invariant!(
        "Only the private Sparse/Dense iterator arms create this view and its directory never changes",
        "a short-array view retains its original immutable block kind"
      ),
    }
  }
}

/// A read-only bit-set projection that keeps the immutable directory alive.
pub struct RoaringBitSetView {
  directory: Arc<Vec<Option<DocIdSetEnum>>>,
  block: usize,
}
impl RoaringBitSetView {
  fn fixed_bits(&self) -> &FixedBitSet {
    // Creation is confined to the Medium arm of DocIdSetEnum::iterator; the directory never changes.
    match &self.directory[self.block] {
      Some(DocIdSetEnum::Medium(bits, _)) => bits,
      _ => panic_invariant!(
        "Only the private Medium iterator arm creates this view and its directory never changes",
        "a bit-set view retains its original immutable block kind"
      ),
    }
  }
}
impl HasIdentity for RoaringBitSetView {
  fn identity(&self) -> &Identity {
    self.fixed_bits().identity()
  }
}
impl Accountable for RoaringBitSetView {
  fn ram_bytes_used(&self) -> Result<i64> {
    self.fixed_bits().ram_bytes_used()
  }
}
impl Bits for RoaringBitSetView {
  fn get(&self, index: usize) -> Result<bool> {
    self.fixed_bits().get(index)
  }
  fn length(&self) -> usize {
    self.fixed_bits().length()
  }
  fn copy_of(&self) -> Result<FixedBitSet> {
    Bits::copy_of(self.fixed_bits())
  }
  fn to_string(&self) -> String {
    Bits::to_string(self.fixed_bits())
  }
}
impl BitSet for RoaringBitSetView {
  fn clear(&mut self) -> Result<()> {
    Err(LuceneError::unsupported_operation(
      "cannot mutate a BitSet through Arc",
    ))
  }

  fn set(&mut self, _i: usize) -> Result<()> {
    Err(LuceneError::unsupported_operation(
      "cannot mutate a BitSet through Arc",
    ))
  }

  fn get_and_set(&mut self, _i: usize) -> Result<bool> {
    Err(LuceneError::unsupported_operation(
      "cannot mutate a BitSet through Arc",
    ))
  }

  fn clear_with_index(&mut self, _i: usize) -> Result<()> {
    Err(LuceneError::unsupported_operation(
      "cannot mutate a BitSet through Arc",
    ))
  }

  fn clear_range(&mut self, _start_index: usize, _end_index: usize) -> Result<()> {
    Err(LuceneError::unsupported_operation(
      "cannot mutate a BitSet through Arc",
    ))
  }

  fn cardinality(&self) -> usize {
    self.fixed_bits().cardinality()
  }

  fn approximate_cardinality(&self) -> usize {
    self.fixed_bits().approximate_cardinality()
  }

  fn prev_set_bit(&self, index: usize) -> Option<usize> {
    self.fixed_bits().prev_set_bit(index)
  }

  fn next_set_bit(&self, index: usize) -> usize {
    self.fixed_bits().next_set_bit(index)
  }

  fn next_set_bit_range(&self, start: usize, end: usize) -> usize {
    self.fixed_bits().next_set_bit_range(start, end)
  }

  fn or<T1>(&mut self, _iter: &mut T1) -> Result<()>
  where
    T1: DocIdSetIterator,
  {
    Err(LuceneError::unsupported_operation(
      "cannot mutate a BitSet through Arc",
    ))
  }

  fn default_or<T1>(&mut self, _iter: &mut T1) -> Result<()>
  where
    T1: DocIdSetIterator,
  {
    Err(LuceneError::unsupported_operation(
      "cannot mutate a BitSet through Arc",
    ))
  }

  fn ensure_capacity(&mut self, _num_bits: usize) -> Result<()> {
    Err(LuceneError::unsupported_operation(
      "cannot mutate a BitSet through Arc",
    ))
  }
}

impl BitSetExtensions for RoaringBitSetView {
  fn as_fixed_bit_set(&self) -> Option<&FixedBitSet> {
    self.fixed_bits().as_fixed_bit_set()
  }

  fn as_sparse_fixed_bit_set(&self) -> Option<&SparseFixedBitSet> {
    self.fixed_bits().as_sparse_fixed_bit_set()
  }
}
