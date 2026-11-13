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
use crate::core::index::BytesRef;
use crate::core::index::doc_values_update::DocValuesUpdate;
use crate::core::index::field_updates_buffer::FieldUpdatesBuffer;
use crate::core::index::term::Term;
use crate::core::search::query::Query;
use crate::core::util::accountable::Accountable;
use crate::core::util::allocator_byte::DirectTrackingAllocatorByte;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bytes_ref_hash::DEFAULT_CAPACITY;
use crate::core::util::bytes_ref_hash::{BytesRefHash, DirectBytesStartArray};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::ram_usage_estimator::{
  size_of_array_capacity, size_of_hash_map, size_of_str, size_of_string,
};
use crate::core::util::{AtomicCounter, ByteBlockPool, Counter, SharedCounter};
#[cfg(test)]
use parking_lot::Mutex;
use std::collections::hash_map::Entry::{Occupied, Vacant};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fmt::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::AtomicI32;

/// Holds buffered deletes and updates, including deletions by docID, term, or
/// query for a single segment.
///
/// This structure is used to manage buffered pending deletes and updates that
/// apply to the segment to be flushed. Once this deletes and updates are pushed
/// (during a flush in `DocumentsWriter`), they are converted into a
/// `FrozenBufferedUpdates` instance and forwarded to the
/// `BufferedUpdatesStream`.
///
/// # Note
/// - Instances of this structure are accessed either via a private instance on
///   `DocumentWriterPerThread`, or while holding the `DocumentsWriterDeleteQueue` lock.
pub(crate) struct BufferedUpdates {
  pub(crate) num_field_updates: AtomicI32,
  pub delete_terms: DeletedTerms,
  pub(crate) delete_queries: HashMap<Query, i32>,
  pub(crate) field_updates: HashMap<Box<str>, Box<FieldUpdatesBuffer>>,
  bytes_used: AtomicCounter,
  pub(crate) field_updates_bytes_used: SharedCounter,
  verbose_deletes: bool,
  gen_: i64,
}

impl BufferedUpdates {
  /// Creates a new `BufferedUpdates` instance.
  pub(crate) fn new() -> Self {
    Self {
      num_field_updates: AtomicI32::new(0),
      delete_terms: DeletedTerms::new(),
      delete_queries: HashMap::new(),
      field_updates: HashMap::new(),
      bytes_used: AtomicCounter::new(),
      field_updates_bytes_used: Arc::new(AtomicCounter::new()),
      verbose_deletes: false,
      gen_: 0,
    }
  }
  pub(crate) fn add_binary_update(
    &mut self,
    update: &DocValuesUpdate,
    doc_id_upto: i32,
  ) -> Result<()> {
    let old_map_size = size_of_hash_map(&self.field_updates);
    let result = catch_unwind(AssertUnwindSafe(|| {
      let buffer = if let Some(buffer) = self.field_updates.get_mut(update.field.as_str()) {
        buffer
      } else {
        match self.field_updates.entry(update.field.as_str().into()) {
          Occupied(entry) => entry.into_mut(),
          Vacant(entry) => {
            let new_buffer = FieldUpdatesBuffer::from_binary_update(
              self.field_updates_bytes_used.clone(),
              update,
              doc_id_upto,
            )?;
            self.field_updates_bytes_used.add_and_get(
              size_of_str(entry.key()).saturating_add(size_of::<FieldUpdatesBuffer>() as i64),
            );
            entry.insert(Box::new(new_buffer))
          },
        }
      };

      if update.has_value {
        let binary_update = update
          .sub_update
          .get_binary()
          .ok_or_else(|| LuceneError::illegal_state("binary update has no binary value"))?;
        buffer.add_update_with_bytes_ref(
          update.buffered_term()?,
          binary_update.get_value()?,
          doc_id_upto,
        )?;
      } else {
        buffer.add_no_value(update.buffered_term()?, doc_id_upto)?;
      }

      self
        .num_field_updates
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
      Ok(())
    }));
    // Entry creation can reserve buckets even when constructing or updating the
    // value fails or panics. Charge their inline payload before propagating the
    // original failure; this counter also follows the map into a frozen packet.
    self
      .field_updates_bytes_used
      .add_and_get(size_of_hash_map(&self.field_updates).saturating_sub(old_map_size));
    unwrap_caught_result!(result)
  }
  pub(crate) fn add_numeric_update(
    &mut self,
    update: &DocValuesUpdate,
    doc_id_upto: i32,
  ) -> Result<()> {
    let old_map_size = size_of_hash_map(&self.field_updates);
    let result = catch_unwind(AssertUnwindSafe(|| {
      let buffer = if let Some(buffer) = self.field_updates.get_mut(update.field.as_str()) {
        buffer
      } else {
        match self.field_updates.entry(update.field.as_str().into()) {
          Occupied(entry) => entry.into_mut(),
          Vacant(entry) => {
            let new_buffer = FieldUpdatesBuffer::from_numeric_update(
              self.field_updates_bytes_used.clone(),
              update,
              doc_id_upto,
            )?;
            self.field_updates_bytes_used.add_and_get(
              size_of_str(entry.key()).saturating_add(size_of::<FieldUpdatesBuffer>() as i64),
            );
            entry.insert(Box::new(new_buffer))
          },
        }
      };

      if update.has_value {
        let numeric_update = update
          .sub_update
          .get_numeric()
          .ok_or_else(|| LuceneError::illegal_state("numeric update has no numeric value"))?;
        buffer.add_update_with_long(
          update.buffered_term()?,
          numeric_update.get_value()?,
          doc_id_upto,
        )?;
      } else {
        buffer.add_no_value(update.buffered_term()?, doc_id_upto)?;
      }

      self
        .num_field_updates
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
      Ok(())
    }));
    self
      .field_updates_bytes_used
      .add_and_get(size_of_hash_map(&self.field_updates).saturating_sub(old_map_size));
    unwrap_caught_result!(result)
  }
  pub(crate) fn add_term(&mut self, term: &Term, doc_id_upto: i32) -> Result<()> {
    let current = self.delete_terms.get(term)?;
    if current != -1 && doc_id_upto < current {
      // Only record the new number if it's greater than the
      // current one.
      // This is important because if multiple
      // threads are replacing the same doc at nearly the
      // same time, it's possible that one thread that got a
      // higher docID is scheduled before the other
      // threads.
      // If we blindly replace than we can
      // incorrectly get both docs indexed.
      return Ok(());
    }
    self.delete_terms.put(term, doc_id_upto)
  }
  pub(crate) fn add_query(&mut self, query: &Query, doc_id_upto: i32) -> Result<()> {
    let query_ram_bytes_used = query.ram_bytes_used()?;
    if let Some(limit) = self.delete_queries.get_mut(query) {
      *limit = doc_id_upto;
      return Ok(());
    }
    let old_size = size_of_hash_map(&self.delete_queries);
    self.delete_queries.insert(query.clone(), doc_id_upto);
    self.bytes_used.add_and_get(
      size_of_hash_map(&self.delete_queries)
        .saturating_sub(old_size)
        .saturating_add(query_ram_bytes_used),
    );
    Ok(())
  }
  pub(crate) fn clear_delete_terms(&mut self) {
    self.delete_terms.clear()
  }
  pub(crate) fn clear(&mut self) {
    self.delete_terms.clear();
    self.delete_queries = HashMap::new();
    self
      .num_field_updates
      .store(0, std::sync::atomic::Ordering::SeqCst);
    // Inline values live in the map's heap allocation in Rust. Release it when
    // resetting the flush-budget counter, just as we do for delete_queries.
    self.field_updates = HashMap::new();

    let used = -self.bytes_used.get();
    self.bytes_used.add_and_get(used);

    let used = -self.field_updates_bytes_used.get();
    self.field_updates_bytes_used.add_and_get(used);
  }
  pub(crate) fn any(&self) -> bool {
    self.delete_terms.size() > 0
      || !self.delete_queries.is_empty()
      || self
        .num_field_updates
        .load(std::sync::atomic::Ordering::SeqCst)
        > 0
  }
}

impl Accountable for BufferedUpdates {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(
      self
        .bytes_used
        .get()
        .wrapping_add(self.field_updates_bytes_used.get())
        .wrapping_add(self.delete_terms.ram_bytes_used()?),
    )
  }
}
impl fmt::Display for BufferedUpdates {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let bytes_used = self.bytes_used.get();
    if self.verbose_deletes {
      write!(
        f,
        "gen={} deleteTerms={} deleteQueries={} fieldUpdates={} bytesUsed={}",
        self.gen_,
        self.delete_terms,
        self.delete_queries.len(),
        self.field_updates.len(),
        bytes_used
      )
    } else {
      let mut s = format!("gen={}", self.gen_);
      if !self.delete_terms.is_empty() {
        write!(s, " {} unique deleted terms", self.delete_terms.size())?;
      }
      if !self.delete_queries.is_empty() {
        write!(s, " {} deleted queries", self.delete_queries.len())?;
      }
      if self
        .num_field_updates
        .load(std::sync::atomic::Ordering::SeqCst)
        != 0
      {
        write!(
          s,
          " {} field updates",
          self
            .num_field_updates
            .load(std::sync::atomic::Ordering::SeqCst)
        )?;
      }
      if bytes_used != 0 {
        write!(s, " bytesUsed={bytes_used}")?;
      }
      write!(f, "{s}")
    }
  }
}
#[cfg(test)]
pub type BufferedUpdatesLock = Arc<Mutex<BufferedUpdates>>;

pub(crate) struct DeletedTerms {
  bytes_used: SharedCounter,
  pool: ByteBlockPool,
  delete_terms: HashMap<String, BytesRefIntMap>,
  terms_size: i32,
}
impl DeletedTerms {
  pub(crate) fn new() -> Self {
    let bytes_used = Arc::new(AtomicCounter::new());
    let allocator = DirectTrackingAllocatorByte::new(bytes_used.clone());
    let pool = ByteBlockPool::new(allocator);
    Self::new_impl(pool, bytes_used)
  }
  pub(crate) fn put(&mut self, term: &Term, value: i32) -> Result<()> {
    let hash = if let Some(hash) = self.delete_terms.get_mut(&term.field) {
      hash
    } else {
      match self.delete_terms.entry(term.field.clone()) {
        Vacant(vacant) => {
          self.bytes_used.add_and_get(size_of_string(vacant.key()));
          let new_map = BytesRefIntMap::new(self.bytes_used.clone())?;
          vacant.insert(new_map)
        },
        Occupied(occupied) => occupied.into_mut(),
      }
    };
    if hash.put(&term.bytes, value, &mut self.pool)? {
      self.terms_size += 1;
    }
    Ok(())
  }
  /// Creates a new instance of `DeletedTerms`.
  fn new_impl(pool: ByteBlockPool, bytes_used: SharedCounter) -> Self {
    Self {
      bytes_used,
      pool,
      delete_terms: HashMap::new(),
      terms_size: 0,
    }
  }
  /// Gets the newest document ID of the deleted term.
  ///
  /// Returns the newest document ID if the term exists, otherwise returns
  /// `-1`.
  pub(crate) fn get(&self, term: &Term) -> Result<i32> {
    if let Some(hash) = self.delete_terms.get(&term.field) {
      hash.get(&term.bytes, &self.pool)
    } else {
      Ok(-1)
    }
  }
  pub(crate) fn clear(&mut self) {
    self.pool.reset(false, false);
    let used = -self.bytes_used.get();
    self.bytes_used.add_and_get(used);
    self.delete_terms = HashMap::new();
    self.terms_size = 0;
  }

  pub(crate) fn size(&self) -> i32 {
    self.terms_size
  }

  pub(crate) fn is_empty(&self) -> bool {
    self.terms_size == 0
  }
  /// Just for test, not efficient.
  pub(crate) fn key_set(&self) -> Result<HashSet<Term>> {
    let mut set = HashSet::with_capacity(self.terms_size as usize);
    for (field, hash) in &self.delete_terms {
      for bytes in hash.key_set(&self.pool)? {
        set.insert(Term::new(field.clone(), bytes));
      }
    }
    Ok(set)
  }

  /// Consume all terms in a sorted order.
  ///
  /// Note: This is a destructive operation as it calls
  /// `BytesRefHash::sort()`.
  #[allow(clippy::type_complexity)]
  pub(crate) fn for_each_ordered<F>(&mut self, mut consumer: F) -> Result<()>
  where
    F: FnMut(&str, &BytesRef<&[u8]>, i32) -> Result<()>,
  {
    let mut delete_fields: smallvec::SmallVec<[(&String, &mut BytesRefIntMap); 1]> =
      smallvec::SmallVec::with_capacity(self.delete_terms.len());
    delete_fields.extend(self.delete_terms.iter_mut());
    delete_fields.sort_unstable_by(|a, b| a.0.cmp(b.0));

    for (field, terms) in delete_fields {
      terms.bytes_ref_hash.sort(&self.pool)?;
      let indices = &terms.bytes_ref_hash.ids;
      for &index in &indices[..terms.bytes_ref_hash.count] {
        let position = terms.bytes_ref_hash.get(index, &self.pool)?;
        let block = self.pool.get_buffer(position.block_index);
        let bytes = &block[position.offset..position.offset + position.length];
        let scratch = BytesRef {
          bytes,
          offset: 0,
          length: position.length,
        };
        consumer(field, &scratch, terms.values[index as usize])?;
      }
    }
    Ok(())
  }
  #[cfg(test)]
  pub(crate) fn get_pool(&self) -> &ByteBlockPool {
    &self.pool
  }
}

impl Accountable for DeletedTerms {
  fn ram_bytes_used(&self) -> Result<i64> {
    // BytesRefIntMap is inline in each bucket, so its shallow size is accounted for
    // by the bucket rather than by a separate child allocation.
    Ok(
      self
        .bytes_used
        .get()
        .saturating_add(size_of_hash_map(&self.delete_terms)),
    )
  }
}
impl fmt::Display for DeletedTerms {
  /// Used for `BufferedUpdates::VERBOSE_DELETES`.
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    use std::fmt::Write as _;

    let terms = self.key_set().map_err(|_| fmt::Error)?;
    let mut entries = String::new();
    for (i, term) in terms.iter().enumerate() {
      if i > 0 {
        entries.push_str(", ");
      }
      write!(
        &mut entries,
        "{}={}",
        term,
        self.get(term).map_err(|_| fmt::Error)?
      )?;
    }

    write!(f, "{{{entries}}}")
  }
}

struct BytesRefIntMap {
  counter: SharedCounter,
  pub(crate) bytes_ref_hash: BytesRefHash<DirectBytesStartArray>,
  values: smallvec::SmallVec<[i32; DEFAULT_CAPACITY]>,
}

impl BytesRefIntMap {
  pub fn new(counter: SharedCounter) -> Result<Self> {
    let bytes_ref_hash = BytesRefHash::from_bytes_start_array(
      DEFAULT_CAPACITY,
      DirectBytesStartArray::with_counter(DEFAULT_CAPACITY, counter.clone()),
    )?;
    Ok(BytesRefIntMap::new_impl(counter, bytes_ref_hash))
  }
  fn new_impl(counter: SharedCounter, bytes_ref_hash: BytesRefHash<DirectBytesStartArray>) -> Self {
    // Inline slots are already included in the map bucket payload.
    let values = smallvec::SmallVec::with_capacity(DEFAULT_CAPACITY);

    Self {
      counter,
      bytes_ref_hash,
      values,
    }
  }
  fn key_set(&self, pool: &ByteBlockPool) -> Result<HashSet<BytesRef<Vec<u8>>>> {
    let mut set = HashSet::with_capacity(self.bytes_ref_hash.size());

    for i in 0..self.bytes_ref_hash.size() {
      let position = self.bytes_ref_hash.get(i as i32, pool)?;
      let block = pool.get_buffer(position.block_index);
      // Each set entry must outlive the pool and subsequent hash changes.
      set.insert(BytesRef::from_bytes(
        block[position.offset..position.offset + position.length].to_vec(),
      ));
    }
    Ok(set)
  }
  fn put(&mut self, key: &BytesRef<Vec<u8>>, value: i32, pool: &mut ByteBlockPool) -> Result<bool> {
    debug_assert!(value >= 0, "Value must be non-negative.");
    let e = self.bytes_ref_hash.add(key, pool)?;
    if e < 0 {
      self.values[(-e - 1) as usize] = value;
      Ok(false)
    } else {
      let index = e as usize;
      if index >= self.values.len() {
        let old_capacity = if self.values.spilled() {
          self.values.capacity()
        } else {
          0
        };
        debug_assert_eq!(index, self.values.len());
        ArrayUtil::oversize(index + 1, size_of::<i32>())?;
        self.values.reserve(index + 1 - self.values.len());
        let capacity = if self.values.spilled() {
          self.values.capacity()
        } else {
          0
        };
        self.counter.add_and_get(size_of_array_capacity::<i32>(
          capacity.saturating_sub(old_capacity),
        ));
        self.values.push(value);
      } else {
        self.values[index] = value;
      }
      Ok(true)
    }
  }
  fn get(&self, key: &BytesRef<Vec<u8>>, pool: &ByteBlockPool) -> Result<i32> {
    let e = self.bytes_ref_hash.find(key, pool)?;
    if e == -1 {
      Ok(-1)
    } else {
      self.values.get(e as usize).copied().ok_or_else(|| {
        LuceneError::illegal_state(format!("missing value for bytes reference ID {e}"))
      })
    }
  }
}

pub const MAX_INT: i32 = i32::MAX;
