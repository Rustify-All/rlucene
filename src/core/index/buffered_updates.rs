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
use crate::core::util::bytes_ref_hash::DEFAULT_CAPACITY;
use std::cell::RefCell;
use std::collections::hash_map::Entry::{Occupied, Vacant};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicI32;

use parking_lot::Mutex;

use crate::core::index::BytesRef;
use crate::core::index::doc_values_update::DocValuesUpdate;
use crate::core::index::field_updates_buffer::FieldUpdatesBuffer;
use crate::core::index::term::Term;
use crate::core::search::query::QueryEnum;
use crate::core::util::access::SharedAccess;
use crate::core::util::accountable::Accountable;
use crate::core::util::allocator_byte::{AllocatorByteEnum, DirectTrackingAllocatorByte};
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bytes_ref_hash::{BytesRefHash, DirectBytesStartArray};
use crate::core::util::error::lucene_error::Result;
use crate::core::util::{
    ByteBlockPool, ByteBlockPoolBorrow, ByteBlockPoolLock, Counter, CounterEnum, CounterEnumBorrow,
    CounterEnumLock,
};

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
///   `DocumentWriterPerThread`, or through synchronized code in the
///   `DocumentsWriterDeleteQueue`.
pub(crate) struct BufferedUpdates<C, B>
where
    C: SharedAccess<CounterEnum>,
    B: SharedAccess<ByteBlockPool<C>>,
{
    pub(crate) num_field_updates: AtomicI32,
    pub delete_terms: DeletedTerms<C, B>,
    pub(crate) delete_queries: HashMap<Arc<QueryEnum>, i32>,
    pub(crate) field_updates: HashMap<String, FieldUpdatesBuffer>,
    bytes_used: C,
    field_updates_bytes_used: C,
    verbose_deletes: bool,
    r#gen: i64,

    segment_name: String,
}

impl MTBufferedUpdates {
    /// Creates a new `BufferedUpdates` instance.
    pub(crate) fn new_sync(segment_name: &str) -> Self {
        Self {
            num_field_updates: AtomicI32::new(0),
            delete_terms: DeletedTerms::new_sync(),
            delete_queries: HashMap::new(),
            field_updates: HashMap::new(),
            bytes_used: Arc::new(Mutex::new(CounterEnum::new_counter(true))),
            field_updates_bytes_used: Arc::new(Mutex::new(CounterEnum::new_counter(true))),
            verbose_deletes: false,
            r#gen: 0,
            segment_name: segment_name.to_string(),
        }
    }
    pub(crate) fn add_binary_update(
        &mut self,
        update: &DocValuesUpdate,
        doc_id_upto: i32,
    ) -> Result<()> {
        let buffer = match self.field_updates.entry(update.field.clone()) {
            Occupied(entry) => entry.into_mut(),
            Vacant(entry) => {
                let new_buffer = FieldUpdatesBuffer::from_binary_update(
                    self.field_updates_bytes_used.clone(),
                    update,
                    doc_id_upto,
                )?;
                entry.insert(new_buffer)
            },
        };

        if update.has_value {
            let binary_update = update.sub_update.get_binary();
            debug_assert!(binary_update.is_some());
            buffer.add_update_with_bytes_ref(
                &update.term,
                &binary_update.unwrap().get_value(),
                doc_id_upto,
            )?;
        } else {
            buffer.add_no_value(&update.term, doc_id_upto)?;
        }

        self.num_field_updates
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    pub(crate) fn add_numeric_update(
        &mut self,
        update: &DocValuesUpdate,
        doc_id_upto: i32,
    ) -> Result<()> {
        let buffer = match self.field_updates.entry(update.field.clone()) {
            Occupied(entry) => entry.into_mut(),
            Vacant(entry) => {
                let new_buffer = FieldUpdatesBuffer::from_numeric_update(
                    self.field_updates_bytes_used.clone(),
                    update,
                    doc_id_upto,
                )?;
                entry.insert(new_buffer)
            },
        };

        if update.has_value {
            let numeric_update = update.sub_update.get_numeric();
            debug_assert!(numeric_update.is_some());
            buffer.add_update_with_long(
                &update.term,
                numeric_update.unwrap().get_value(),
                doc_id_upto,
            )?;
        } else {
            buffer.add_no_value(&update.term, doc_id_upto)?;
        }

        self.num_field_updates
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    pub(crate) fn add_term(&mut self, term: &Term, doc_id_upto: i32) -> Result<()> {
        let current = self.delete_terms.get(term);
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
        self.delete_terms.put_sync(term, doc_id_upto)
    }
}

impl STBufferedUpdates {
    /// Creates a new `BufferedUpdates` instance.
    pub(crate) fn new(segment_name: String) -> Self {
        Self {
            num_field_updates: AtomicI32::new(0),
            delete_terms: DeletedTerms::new(),
            delete_queries: HashMap::new(),
            field_updates: HashMap::new(),
            bytes_used: Rc::new(RefCell::new(CounterEnum::new_counter(true))),
            field_updates_bytes_used: Rc::new(RefCell::new(CounterEnum::new_counter(true))),
            verbose_deletes: false,
            r#gen: 0,
            segment_name,
        }
    }
}

impl<C, B> BufferedUpdates<C, B>
where
    C: SharedAccess<CounterEnum>,
    B: SharedAccess<ByteBlockPool<C>>,
{
    pub(crate) fn add_query(&mut self, query: Arc<QueryEnum>, doc_id_upto: i32) {
        if self
            .delete_queries
            .insert(query.clone(), doc_id_upto)
            .is_none()
        {
            self.bytes_used
                .access_mut(|bytes_used| bytes_used.add_and_get(BYTES_PER_DEL_QUERY as i64));
        }
    }
    pub(crate) fn clear_delete_terms(&mut self) {
        self.delete_terms.clear()
    }
    pub(crate) fn clear(&mut self) {
        self.delete_terms.clear();
        self.delete_queries.clear();
        self.num_field_updates
            .store(0, std::sync::atomic::Ordering::SeqCst);
        self.field_updates.clear();

        self.bytes_used.access_mut(|bytes_used| {
            let used = -bytes_used.get();
            bytes_used.add_and_get(used)
        });

        self.field_updates_bytes_used
            .access_mut(|field_updates_bytes_used| {
                let used = -field_updates_bytes_used.get();
                field_updates_bytes_used.add_and_get(used);
            })
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

impl<C, B> Accountable for BufferedUpdates<C, B>
where
    C: SharedAccess<CounterEnum>,
    B: SharedAccess<ByteBlockPool<C>>,
{
    fn ram_bytes_used(&self) -> Result<i64> {
        // TODO: memory calculation not implemented
        Ok(0)
    }
}
impl<C, B> fmt::Display for BufferedUpdates<C, B>
where
    C: SharedAccess<CounterEnum>,
    B: SharedAccess<ByteBlockPool<C>>,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes_used = self.bytes_used.access(|bytes_used| bytes_used.get());
        if self.verbose_deletes {
            write!(
                f,
                "gen={} deleteTerms={} deleteQueries={} fieldUpdates={} bytesUsed={}",
                self.r#gen,
                self.delete_terms,
                self.delete_queries.len(),
                self.field_updates.len(),
                bytes_used
            )
        } else {
            let mut s = format!("gen={}", self.r#gen);
            if !self.delete_terms.is_empty() {
                s.push_str(&format!(
                    " {} unique deleted terms",
                    self.delete_terms.size()
                ));
            }
            if !self.delete_queries.is_empty() {
                s.push_str(&format!(" {} deleted queries", self.delete_queries.len()));
            }
            if self
                .num_field_updates
                .load(std::sync::atomic::Ordering::SeqCst)
                != 0
            {
                s.push_str(&format!(
                    " {} field updates",
                    self.num_field_updates
                        .load(std::sync::atomic::Ordering::SeqCst)
                ));
            }
            if bytes_used != 0 {
                s.push_str(&format!(" bytesUsed={bytes_used}"));
            }
            write!(f, "{s}")
        }
    }
}
/// for multi-threaded scenarios
pub type MTBufferedUpdates = BufferedUpdates<CounterEnumLock, ByteBlockPoolLock>;
pub type BufferedUpdatesLock = Arc<Mutex<MTBufferedUpdates>>;
/// for single-threaded scenarios
pub type STBufferedUpdates = BufferedUpdates<CounterEnumBorrow, ByteBlockPoolBorrow>;
pub type BufferedUpdatesBorrow = Rc<RefCell<STBufferedUpdates>>;

pub(crate) struct DeletedTerms<C, B>
where
    C: SharedAccess<CounterEnum>,
    B: SharedAccess<ByteBlockPool<C>>,
{
    bytes_used: C,
    pool: B,
    delete_terms: HashMap<String, BytesRefIntMap<C, B>>,
    terms_size: i32,
}
macro_rules! impl_deleted_terms {
    (
        $Type:ident,
        new = $new_fn:ident,
        put = $put_fn:ident,
        bytes_wrap = $bytes_wrap:expr_2021,
        pool_wrap  = $pool_wrap:expr_2021,
        pool_ctor  = $pool_ctor:ident,
        map_ctor   = $map_ctor:ident
    ) => {
        impl $Type {
            pub(crate) fn $new_fn() -> Self {
                let bytes_used = $bytes_wrap(CounterEnum::new_counter(false));
                let allocator =
                    AllocatorByteEnum::DTA(DirectTrackingAllocatorByte::new(bytes_used.clone()));
                let pool = $pool_wrap(ByteBlockPool::$pool_ctor(allocator));
                Self::new_impl(pool, bytes_used)
            }
            pub(crate) fn $put_fn(&mut self, term: &Term, value: i32) -> Result<()> {
                let hash = match self.delete_terms.entry(term.field.clone()) {
                    Vacant(vacant) => {
                        // TODO: memory calculation not implemented
                        self.bytes_used.access_mut(|bytes_used| {
                            let _ = bytes_used.add_and_get(0);
                        });
                        let new_map =
                            BytesRefIntMap::$map_ctor(self.pool.clone(), self.bytes_used.clone());
                        vacant.insert(new_map)
                    },
                    Occupied(occupied) => occupied.into_mut(),
                };
                if hash.put(&term.bytes, value)? {
                    self.terms_size += 1;
                }
                Ok(())
            }
        }
    };
}
impl_deleted_terms!(
    STDeletedTerms,
    new = new,
    put = put,
    bytes_wrap = |v| Rc::new(RefCell::new(v)),
    pool_wrap = |p| Rc::new(RefCell::new(p)),
    pool_ctor = new,
    map_ctor = new
);
impl_deleted_terms!(
    MTDeletedTerms,
    new = new_sync,
    put = put_sync,
    bytes_wrap = |v| Arc::new(Mutex::new(v)),
    pool_wrap = |p| Arc::new(Mutex::new(p)),
    pool_ctor = new_sync,
    map_ctor = new_sync
);

pub type MTDeletedTerms = DeletedTerms<CounterEnumLock, ByteBlockPoolLock>;
pub type STDeletedTerms = DeletedTerms<CounterEnumBorrow, ByteBlockPoolBorrow>;

impl<C, B> DeletedTerms<C, B>
where
    C: SharedAccess<CounterEnum>,
    B: SharedAccess<ByteBlockPool<C>>,
{
    /// Creates a new instance of `DeletedTerms`.
    fn new_impl(pool: B, bytes_used: C) -> Self {
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
    pub(crate) fn get(&self, term: &Term) -> i32 {
        if let Some(hash) = self.delete_terms.get(&term.field) {
            hash.get(&term.bytes)
        } else {
            -1
        }
    }
    pub(crate) fn clear(&mut self) {
        self.pool.access_mut(|p| p.reset(false, false));

        self.bytes_used.access_mut(|bytes_used| {
            let used = -bytes_used.get();
            let _ = bytes_used.add_and_get(used);
        });
        self.delete_terms.clear();
        self.terms_size = 0;
    }

    pub(crate) fn size(&self) -> i32 {
        self.terms_size
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.terms_size == 0
    }
    /// Just for test, not efficient.
    pub(crate) fn key_set(&self) -> HashSet<Term> {
        let mut set = HashSet::new();
        for (field, hash) in &self.delete_terms {
            for bytes in hash.key_set() {
                set.insert(Term::new(field.clone(), bytes));
            }
        }
        set
    }

    /// Consume all terms in a sorted order.
    ///
    /// Note: This is a destructive operation as it calls
    /// `BytesRefHash::sort()`.
    #[allow(clippy::type_complexity)]
    pub(crate) fn for_each_ordered<F>(&mut self, mut consumer: F) -> Result<()>
    where
        F: FnMut(&Term, i32) -> Result<()>,
    {
        let mut delete_fields: Vec<(&String, &mut BytesRefIntMap<C, B>)> =
            self.delete_terms.iter_mut().collect();
        delete_fields.sort_by(|a, b| a.0.cmp(b.0));

        let mut scratch = Term::new("", BytesRef::new());
        for (field, terms) in delete_fields {
            scratch.field = field.clone();
            terms.bytes_ref_hash.sort()?;
            let indices = &terms.bytes_ref_hash.ids;
            for i in 0..terms.bytes_ref_hash.count {
                let index = indices[i as usize];
                terms.bytes_ref_hash.get(index, &mut scratch.bytes);
                consumer(&scratch, terms.values[index as usize])?;
            }
        }
        Ok(())
    }
    #[cfg(debug_assertions)]
    pub(crate) fn get_pool(&self) -> B {
        self.pool.clone()
    }
}

pub trait DeletedTermConsumer {
    fn accept(&mut self, term: &Term, doc_id: i32) -> Result<()>;
}
impl<C, B> Accountable for DeletedTerms<C, B>
where
    C: SharedAccess<CounterEnum>,
    B: SharedAccess<ByteBlockPool<C>>,
{
    fn ram_bytes_used(&self) -> Result<i64> {
        // TODO: memory calculation not implemented
        Ok(0)
    }
}
impl<C, B> fmt::Display for DeletedTerms<C, B>
where
    C: SharedAccess<CounterEnum>,
    B: SharedAccess<ByteBlockPool<C>>,
{
    /// Used for `BufferedUpdates::VERBOSE_DELETES`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut entries = Vec::new();
        for term in self.key_set().iter() {
            entries.push(format!("{}={}", term, self.get(term)));
        }

        write!(f, "{{{}}}", entries.join(", "))
    }
}

struct BytesRefIntMap<C, B>
where
    C: SharedAccess<CounterEnum>,
    B: SharedAccess<ByteBlockPool<C>>,
{
    counter: C,
    pub(crate) bytes_ref_hash: BytesRefHash<C, B, DirectBytesStartArray<C>>,
    values: Vec<i32>,
}

macro_rules! impl_bytes_ref_int_map_new {
    (
        $Counter:ty, $Pool:ty, $start_array_ctor:path, $new_fn:ident
    ) => {
        impl BytesRefIntMap<$Counter, $Pool> {
            pub fn $new_fn(pool: $Pool, counter: $Counter) -> Self {
                let bytes_ref_hash = BytesRefHash::from_bytes_start_array(
                    pool,
                    DEFAULT_CAPACITY,
                    $start_array_ctor(DEFAULT_CAPACITY, counter.clone()),
                );
                BytesRefIntMap::new_impl(counter, bytes_ref_hash)
            }
        }
    };
}

impl_bytes_ref_int_map_new!(
    CounterEnumBorrow,
    ByteBlockPoolBorrow,
    DirectBytesStartArray::with_counter,
    new
);

impl_bytes_ref_int_map_new!(
    CounterEnumLock,
    ByteBlockPoolLock,
    DirectBytesStartArray::with_counter_sync,
    new_sync
);

impl<C, B> BytesRefIntMap<C, B>
where
    C: SharedAccess<CounterEnum>,
    B: SharedAccess<ByteBlockPool<C>>,
{
    fn new_impl(counter: C, bytes_ref_hash: BytesRefHash<C, B, DirectBytesStartArray<C>>) -> Self {
        let values = vec![0; DEFAULT_CAPACITY as usize];

        counter.access_mut(|c| c.add_and_get(INIT_RAM_BYTES));

        Self {
            counter,
            bytes_ref_hash,
            values,
        }
    }
    fn key_set(&self) -> HashSet<BytesRef<Vec<u8>>> {
        let mut scratch = BytesRef::new();
        let mut set = HashSet::new();

        for i in 0..self.bytes_ref_hash.size() {
            self.bytes_ref_hash.get(i, &mut scratch);
            set.insert(BytesRef::deep_copy_of(&scratch));
        }
        set
    }
    fn put(&mut self, key: &BytesRef<Vec<u8>>, value: i32) -> Result<bool> {
        debug_assert!(value >= 0, "Value must be non-negative.");
        let e = self.bytes_ref_hash.add(key)?;
        if e < 0 {
            self.values[(-e - 1) as usize] = value;
            Ok(false)
        } else {
            if e as usize >= self.values.len() {
                let origin_length = self.values.len();
                ArrayUtil::grow_with_len(&mut self.values, (e + 1) as usize);
                // TODO: memory calculation not implemented
                self.counter
                    .access_mut(|c| c.add_and_get(origin_length as i64));
            }
            self.values[e as usize] = value;
            Ok(true)
        }
    }
    fn get(&self, key: &BytesRef<Vec<u8>>) -> i32 {
        let e = self.bytes_ref_hash.find(key);
        if e == -1 { -1 } else { self.values[e as usize] }
    }
}

// TODO: memory calculation not implemented
const INIT_RAM_BYTES: i64 = 0;

/// Rough logic: HashMap has an array with varying load factor.
/// Entry consists of Query key, Integer value, int hash, and Entry next.
// TODO: memory calculation not implemented
pub const BYTES_PER_DEL_QUERY: i32 = 0;
pub const MAX_INT: i32 = i32::MAX;

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use rand::{Rng, RngCore};

    use crate::core::index::BytesRef;
    use crate::core::index::buffered_updates::{BufferedUpdates, DeletedTerms};
    use crate::core::index::term::Term;
    use crate::core::search::query::QueryEnum;
    use crate::core::search::term_query::TermQuery;
    use crate::core::util::accountable::Accountable;
    use crate::core::util::error::lucene_error::Result;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{at_least, random};

    #[allow(dead_code)] // for quick search
    pub struct TestBufferedUpdates;

    #[test]
    fn test_ram_bytes_used() -> Result<()> {
        let mut random = random();
        let mut bu = BufferedUpdates::new_sync("seg1");

        // TODO
        // assert_eq!(bu.ram_bytes_used(), 0);
        assert!(!bu.any());

        let queries = at_least(&mut random, 1);
        for _ in 0..queries {
            let doc_id_upto = if random.random_bool(0.5) {
                i32::MAX
            } else {
                random.random_range(0..100000)
            };
            let value = format!("{}", random.random_range(0..100));
            let term = Term::new("id", BytesRef::from_string(&value));
            bu.add_query(
                Arc::new(QueryEnum::Term(TermQuery::new(term.clone()))),
                doc_id_upto,
            );
        }

        let terms = at_least(&mut random, 1);
        for _ in 0..terms {
            let doc_id_upto = if random.random_bool(0.5) {
                i32::MAX
            } else {
                random.random_range(0..100000)
            };
            let value = format!("{}", random.random_range(0..100));
            let term = Term::new("id", BytesRef::from_string(&value));
            bu.add_term(&term, doc_id_upto)?;
        }

        assert!(
            bu.any(),
            "We have added a lot of docIds, terms, and queries, but `any()` returned false."
        );

        // TODO
        // let total_used = bu.ram_bytes_used();
        // assert!(total_used > 0);

        bu.clear_delete_terms();
        assert!(
            bu.any(),
            "Only terms and docIds are cleaned, the queries should still be in memory."
        );
        // TODO
        // assert!(
        //     total_used > bu.ram_bytes_used(),
        //     "Terms are cleaned, so memory usage should decrease."
        // );

        bu.clear();
        assert!(!bu.any());
        // TODO
        // assert_eq!(bu.ram_bytes_used()?, 0);

        Ok(())
    }
    #[test]
    fn test_deleted_terms() -> Result<()> {
        let mut random = random();
        let iters = at_least(&mut random, 10);
        let fields = ["a".to_string(), "b".to_string(), "c".to_string()];
        let mut actual = DeletedTerms::new();

        for _ in 0..iters {
            let mut expected = HashMap::new();
            assert!(actual.is_empty());

            let term_count = at_least(&mut random, 5000);
            let max_bytes_num = random.random_range(1..=3);

            for _ in 0..term_count {
                let byte_num = random.random_range(1..=max_bytes_num);
                let mut bytes = vec![0u8; byte_num];
                random.fill_bytes(&mut bytes);

                let field = &fields[random.random_range(0..fields.len())];
                let term = Term::new(field.clone(), BytesRef::from_bytes(bytes));
                let value = random.random_range(0..10_000_000);

                expected.insert(term.clone(), value);
                actual.put(&term, value)?;
            }

            assert_eq!(expected.len(), actual.size() as usize);

            for (term, expected_value) in &expected {
                assert_eq!(*expected_value, actual.get(term));
            }

            let mut expected_sorted: Vec<(Term, i32)> = expected
                .iter()
                .map(|(term, doc_id)| (Term::new(term.field.clone(), term.bytes.clone()), *doc_id))
                .collect();
            expected_sorted.sort_by_key(|entry| entry.0.clone());

            let mut actual_sorted: Vec<_> = Vec::new();
            let _ = actual.for_each_ordered(|term, doc_id| {
                let copy = Term::new(term.field.clone(), term.bytes.clone());
                actual_sorted.push((copy, doc_id));
                Ok(())
            });

            assert_eq!(expected_sorted, actual_sorted);

            actual.clear();
            assert_eq!(actual.size(), 0);
            assert_eq!(actual.ram_bytes_used()?, 0);
            let pool_guard = actual.get_pool();
            let pool = pool_guard.borrow();
            assert_eq!(pool.buffer_upto, -1);
        }

        Ok(())
    }
}
