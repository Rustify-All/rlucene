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
use crate::core::index::index_reader::{CacheHelper, CacheKey};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::{LRCacherHelper, LeafReader};
use crate::core::index::leaf_reader_context::{LeafReaderContext, TopParentMeta};
use crate::core::index::reader_util::ReaderUtil;
use crate::core::search::bulk_scorer::BulkScorer;
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::constant_score_weight::ConstantScoreWeight;
use crate::core::search::doc_id_set::{DocIdSet, EmptyDocIdSet};
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::search::doc_id_set_iterator::{
    DocIdSetIterator, DocIdSetIteratorEnum2, DocIdSetIteratorEnum3, EmptyDISI,
};

use crate::core::search::dummy::dummy_disi::DummyDISI;
use crate::core::search::dummy::dummy_two_phase_iterator::DummyTwoPhaseIterator;
use crate::core::search::explanation::Explanation;
use crate::core::search::leaf_collector::LeafCollector;
use crate::core::search::query::{IdentityQuery, Query, QueryBase};
use crate::core::search::query_cache::QueryCache;
use crate::core::search::query_caching_policy::{QueryCachingPolicy, QueryCachingPolicyEnum};
use crate::core::search::scorable::Scorable;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::ScorerEnum2;
use crate::core::search::scorer_supplier::{ScorerSupplier, ScorerSupplierEnum, ScorerSupplierEnum3};
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::weight::{BoxWeight, DefaultBulkScorer, Weight, WeightScorerSupplier};
use crate::core::util::TryIntoInt;
use crate::core::util::accountable::Accountable;
use crate::core::util::bit_doc_id_set::BitDocIdSet;
use crate::core::util::bit_set::BitSet;
use crate::core::util::dummy::dummy_bits::DummyBits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::predicate::Predicate;
use crate::core::util::roaring_doc_id_set::RoaringDocIdSet;
use crate::core::util::roaring_doc_id_set::builder::Builder;
use linked_hash_map::LinkedHashMap;
use parking_lot::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fmt::{Display, Formatter};
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

/// A [`QueryCache`] that evicts queries using an LRU (least-recently-used) eviction policy
/// in order to remain under a given maximum size and number of bytes used.
///
///
/// This structure is thread-safe.
///
/// Note that query eviction runs in linear time with the total number of segments that have
/// cache entries, so this cache works best with [`QueryCachingPolicy`] implementations that
/// only cache on "large" segments. It is advised to not share this cache across too many indices.
///
///
/// A default query cache and policy instance is used in `IndexSearcher`.
/// If you want to replace those defaults it is typically done like this:
///
/// This cache exposes some global statistics:
/// - [`get_hit_count()`](LRUQueryCache::get_hit_count): hit count
/// - [`get_miss_count()`](LRUQueryCache::get_miss_count): miss count
/// - [`get_cache_size()`](LRUQueryCache::get_cache_size): number of cache entries
/// - [`get_cache_count()`](LRUQueryCache::get_cache_count): total number of `DocIdSet`s that have ever been cached
/// - [`get_eviction_count()`](LRUQueryCache::get_eviction_count): number of evicted entries
///
///
/// In case you would like to have more fine-grained statistics, such as per-index or
/// per-query-class statistics, it is possible to override various callbacks:
/// [`on_hit`](LRUQueryCache::on_hit), [`on_miss`](LRUQueryCache::on_miss), [`on_query_cache`](LRUQueryCache::on_query_cache), [`on_query_eviction`](LRUQueryCache::on_query_eviction),
/// [`on_docidset_cache`](LRUQueryCache::on_doc_id_set_cache), [`on_docidset_eviction`](LRUQueryCache::on_doc_id_set_eviction) and [`on_clear`](LRUQueryCache::on_clear).
///
/// It is better to not perform heavy computations in these methods since they are called
/// synchronously and under a lock.
///
///
/// # See also
/// [`QueryCachingPolicy`]
///
/// # Experimental
/// This API is marked as experimental, following the original Lucene design.
pub struct LRUQueryCache<P>
where
    P: Predicate<TopParentMeta>,
{
    max_size: i32,
    max_ram_bytes_used: i64,
    skip_cache_factor: f32,
    hit_count: AtomicU64,
    miss_count: AtomicU64,
    // these variables are volatile so that we do not need to sync reads
    // but increments need to be performed under the lock
    ram_bytes_used: AtomicI64,
    cache_count: AtomicI64,
    cache_size: AtomicI64,
    inner: RwLock<Inner>,
    leaves_to_cache: P,
}
pub struct Inner {
    unique_queries: Mutex<LinkedHashMap<Arc<Query>, IdentityQuery>>,
    cache: HashMap<CacheKey, LeafCache>,
}

impl LRUQueryCache<MinSegmentSizePredicate> {
    pub fn new(max_size: i32, max_ram_bytes_used: i64) -> Result<Self> {
        Self::with_skip_cache_factor(
            max_size,
            max_ram_bytes_used,
            10f32,
            MinSegmentSizePredicate::new(10000),
        )
    }
}

impl<P> LRUQueryCache<P>
where
    P: Predicate<TopParentMeta>,
{
    /// Expert: Create a new instance that will cache at most `max_size` queries with at most
    /// `max_ram_bytes_used` bytes of memory, only on leaves that satisfy `leaves_to_cache`.
    ///
    ///
    /// Also, clauses whose cost is `skip_cache_factor` times more than the cost of the
    /// top-level query will not be cached in order to not slow down queries too much.
    pub fn with_skip_cache_factor(
        max_size: i32,
        max_ram_bytes_used: i64,
        skip_cache_factor: f32,
        leaves_to_cache: P,
    ) -> Result<Self> {
        if skip_cache_factor < 1.0 {
            return Err(LuceneError::illegal_argument(format!(
                "skipCacheFactor must be no less than 1, get {}",
                skip_cache_factor
            )));
        }

        Ok(Self {
            max_size,
            max_ram_bytes_used,
            skip_cache_factor,
            hit_count: AtomicU64::new(0),
            miss_count: AtomicU64::new(0),
            ram_bytes_used: AtomicI64::new(0),
            cache_count: AtomicI64::new(0),
            cache_size: AtomicI64::new(0),
            inner: RwLock::new(Inner {
                unique_queries: Mutex::new(LinkedHashMap::with_capacity(16)),
                cache: HashMap::new(),
            }),
            leaves_to_cache,
        })
    }
    /// Expert: callback when there is a cache hit on a given query.
    /// Implementing this method is typically useful in order to compute
    /// more fine-grained statistics about the query cache.
    ///
    /// See also [`on_miss`](Self::on_miss).
    ///
    /// Experimental: this API follows the original Lucene experimental status.
    pub(crate) fn on_hit(&self, _reader_core_key: &CacheKey, _query: &Query) {
        self.hit_count.fetch_add(1, Ordering::Relaxed);
    }
    /// Expert: callback when there is a cache miss on a given query.
    ///
    /// See also [`on_hit`](Self::on_hit).
    ///
    /// Experimental: this API follows the original Lucene experimental status.
    pub(crate) fn on_miss(&self, _reader_core_key: &CacheKey, _query: &Query) {
        self.miss_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    /// Expert: callback when a query is added to this cache.
    /// Implementing this method is typically useful in order to compute
    /// more fine-grained statistics about the query cache.
    ///
    /// See also [`on_query_eviction`](Self::on_query_eviction).
    ///
    /// Experimental: this API follows the original Lucene experimental status.
    pub(crate) fn on_query_cache(
        &self,
        _query: &Query,
        ram_bytes_used: i64,
        _rwlock: &RwLockWriteGuard<Inner>,
    ) {
        self.ram_bytes_used
            .fetch_add(ram_bytes_used, Ordering::Relaxed);
    }
    /// Expert: callback when a query is evicted from this cache.
    ///
    /// See also [`on_query_cache`](Self::on_query_cache).
    ///
    /// Experimental: this API follows the original Lucene experimental status.
    pub(crate) fn on_query_eviction(
        &self,
        _query: &Query,
        ram_bytes_used: i64,
        _guard: &RwLockWriteGuard<Inner>,
    ) {
        self.ram_bytes_used
            .fetch_sub(ram_bytes_used, Ordering::Relaxed);
    }
    /// Expert: callback when a [`DocIdSet`] is added to this cache.
    /// Implementing this method is typically useful in order to compute
    /// more fine-grained statistics about the query cache.
    ///
    /// See also [`on_doc_id_set_eviction`](Self::on_doc_id_set_eviction).
    ///
    /// Experimental: this API follows the original Lucene experimental status.
    pub(crate) fn on_doc_id_set_cache(&self, _reader_core_key: &CacheKey, ram_bytes_used: i64) {
        self.cache_size.fetch_add(1, Ordering::Relaxed);
        self.cache_count.fetch_add(1, Ordering::Relaxed);
        self.ram_bytes_used
            .fetch_add(ram_bytes_used, Ordering::Relaxed);
    }

    /// Expert: callback when one or more [`DocIdSet`]s are removed from this cache.
    ///
    /// See also [`on_docidset_cache`](Self::on_docidset_cache).
    ///
    /// Experimental: this API follows the original Lucene experimental status.
    pub(crate) fn on_doc_id_set_eviction(
        &self,
        _reader_core_key: &CacheKey,
        num_entries: i64,
        sum_ram_bytes_used: i64,
    ) {
        self.ram_bytes_used
            .fetch_sub(sum_ram_bytes_used, Ordering::Relaxed);
        self.cache_size.fetch_sub(num_entries, Ordering::Relaxed);
    }
    /// Expert: callback when the cache is completely cleared.
    ///
    /// Experimental: this API follows the original Lucene experimental status.
    pub(crate) fn on_clear(&self, _guard: &RwLockWriteGuard<Inner>) {
        self.ram_bytes_used.store(0, Ordering::Relaxed);
        self.cache_size.store(0, Ordering::Relaxed);
    }
    /// Whether evictions are required.
    pub(crate) fn requires_eviction(&self, guard: &RwLockWriteGuard<Inner>) -> bool {
        let size = guard.unique_queries.lock().len();
        if size == 0 {
            return false;
        }
        size as i32 > self.max_size
            || self.ram_bytes_used.load(Ordering::Relaxed) > self.max_ram_bytes_used
    }
    pub(crate) fn get<C>(
        &self,
        key: &Query,
        cache_helper: &C,
        inner: &RwLockReadGuard<Inner>,
    ) -> Option<Arc<CacheAndCountEnum>>
    where
        C: CacheHelper,
    {
        // TODO: 这里没有assert

        let reader_key = cache_helper.get_key();

        let leaf_cache = match inner.cache.get(&reader_key) {
            Some(c) => c,
            None => {
                self.on_miss(&reader_key, key);
                return None;
            },
        };
        // this get call moves the query to the most-recently-used position
        let mut unique_queries = inner.unique_queries.lock();
        let singleton = match unique_queries.get_refresh(key) {
            Some(c) => c,
            None => {
                self.on_miss(&reader_key, key);
                return None;
            },
        };

        match leaf_cache.get(singleton) {
            Some(c) => {
                self.on_hit(&reader_key, singleton.query.as_ref());
                Some(c)
            },
            None => {
                self.on_miss(&reader_key, singleton.query.as_ref());
                None
            },
        }
    }

    pub(crate) fn put_if_absent<C>(
        &self,
        query: Arc<Query>,
        cached: CacheAndCountEnum,
        cache_helper: &C,
    ) where
        C: CacheHelper,
    {
        // TODO: 这里没有assert
        // under a lock to make sure that mostRecentlyUsedQueries and cache remain sync'ed
        let mut inner = self.inner.write();

        let (singleton, inserted) = {
            let mut uq = inner.unique_queries.lock();
            if let Some(iq) = uq.get_refresh(query.as_ref()) {
                (iq.clone(), false)
            } else {
                let iq = IdentityQuery::new(query.clone());
                let prev = uq.insert(query, iq.clone());
                debug_assert!(prev.is_none());
                (iq, true)
            }
        };

        if inserted {
            self.on_query_cache(
                singleton.query.as_ref(),
                self.get_ram_bytes_used(singleton.query.as_ref()),
                &inner,
            );
        }

        let key = cache_helper.get_key();
        let leaf_cache = match inner.cache.entry(key.clone()) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(cache) => {
                let leaf_cache = LeafCache::new(key);
                let lc_ref = cache.insert(leaf_cache);
                self.ram_bytes_used.fetch_add(
                    // TODO: memory calculation not implemented
                    0,
                    std::sync::atomic::Ordering::Relaxed,
                );
                // TODO IMPORTANT 这里没有调用add_close_listener
                lc_ref
            },
        };

        leaf_cache.put_if_absent(singleton, cached, self);
        self.evict_if_necessary(&mut inner);
    }
    pub(crate) fn evict_if_necessary(&self, guard: &mut RwLockWriteGuard<Inner>) {
        loop {
            if !self.requires_eviction(guard) {
                break;
            }

            let singleton = {
                let mut unique_queries = guard.unique_queries.lock();
                match unique_queries.pop_front() {
                    Some((_key, singleton)) => singleton,
                    None => break,
                }
            };
            self.on_eviction(singleton, guard);
        }
    }

    /// Remove all cache entries for the given core cache key.
    pub(crate) fn clear_core_cache_key(&self, core_key: &CacheKey) {
        let mut inner = self.inner.write();

        if let Some(leaf_cache) = inner.cache.remove(core_key) {
            // TODO: memory calculation not implemented
            self.ram_bytes_used
                .fetch_sub(0, std::sync::atomic::Ordering::Relaxed);

            let num_entries = leaf_cache.cache.len();
            debug_assert!(num_entries <= i64::MAX as usize);
            if num_entries > 0 {
                self.on_doc_id_set_eviction(
                    core_key,
                    num_entries as i64,
                    leaf_cache
                        .ram_bytes_used
                        .load(std::sync::atomic::Ordering::Relaxed),
                );
            } else {
                debug_assert_eq!(num_entries, 0);
                debug_assert_eq!(
                    leaf_cache
                        .ram_bytes_used
                        .load(std::sync::atomic::Ordering::Relaxed),
                    0
                );
            }
        }
    }
    /// Remove all cache entries for the given query.
    pub fn clear_query(&self, query: &Query) {
        let mut inner = self.inner.write();
        let v = {
            let mut unique_queries = inner.unique_queries.lock();
            unique_queries.remove(query)
        };
        if let Some(singleton) = v {
            self.on_eviction(singleton, &mut inner);
        }
    }

    pub(crate) fn on_eviction(
        &self,
        singleton: IdentityQuery,
        guard: &mut RwLockWriteGuard<Inner>,
    ) {
        self.on_query_eviction(
            singleton.query.as_ref(),
            self.get_ram_bytes_used(singleton.query.as_ref()),
            guard,
        );

        for leaf_cache in guard.cache.values_mut() {
            leaf_cache.remove(&singleton, self);
        }
    }
    /// Clear the content of this cache.
    pub(crate) fn clear(&self) {
        let mut inner = self.inner.write();
        inner.cache.clear();
        inner.unique_queries.lock().clear();
        self.on_clear(&inner);
    }
    fn get_ram_bytes_used(&self, _query: &Query) -> i64 {
        // TODO: memory calculation not implemented
        0
    }
    /// Return the total number of times that a [`Query`](crate::core::search::query::Query) has been looked up in this [`QueryCache`](crate::core::search::query_cache::QueryCache).
    /// Note that this number is incremented once per segment, so running a cached query only once
    /// will increment this counter by the number of segments that are wrapped by the searcher.
    /// By definition, [`get_total_count()`](Self::get_total_count) is the sum of [`get_hit_count()`](Self::get_hit_count) and [`get_miss_count()`](Self::get_miss_count).
    ///
    /// See also [`get_hit_count()`](Self::get_hit_count) and [`get_miss_count()`](Self::get_miss_count).
    pub fn get_total_count(&self) -> u64 {
        self.get_hit_count() + self.get_miss_count()
    }
    /// Over the [`get_total_count()`](Self::get_total_count) total number of times that a query has been looked up,
    /// return how many times a cached [`DocIdSet`] has been found and returned.
    ///
    /// See also [`get_total_count()`](Self::get_total_count) and [`get_miss_count()`](Self::get_miss_count).
    pub fn get_hit_count(&self) -> u64 {
        self.hit_count.load(Ordering::Relaxed)
    }
    /// Over the [`get_total_count()`](Self::get_total_count) total number of times that a query has been looked up,
    /// return how many times this query was not contained in the cache.
    ///
    /// See also [`get_total_count()`](Self::get_total_count) and [`get_hit_count()`](Self::get_hit_count).
    pub fn get_miss_count(&self) -> u64 {
        self.miss_count.load(Ordering::Relaxed)
    }
    /// Return the total number of [`DocIdSet`]s which are currently stored in the cache.
    ///
    /// See also [`get_cache_count()`](Self::get_cache_count) and [`get_eviction_count()`](Self::get_eviction_count).
    pub fn get_cache_size(&self) -> i64 {
        self.cache_size.load(Ordering::Relaxed)
    }
    /// Return the total number of cache entries that have been generated and put in the cache.
    /// It is highly desirable to have a [`get_hit_count()`](Self::get_hit_count) that is much higher
    /// than the [`get_cache_count()`](Self::get_cache_count), as the opposite would indicate that
    /// the query cache makes efforts in order to cache queries but then they do not get reused.
    ///
    /// See also [`get_cache_size()`](Self::get_cache_size) and [`get_eviction_count()`](Self::get_eviction_count).
    pub fn get_cache_count(&self) -> i64 {
        self.cache_count.load(Ordering::Relaxed)
    }
    /// Return the number of cache entries that have been removed from the cache either in order to
    /// stay under the maximum configured size or RAM usage, or because a segment has been closed.
    /// High numbers of evictions might mean that queries are not reused or that the
    /// [`QueryCachingPolicy`] caches too aggressively on NRT segments which get merged early.
    ///
    /// See also [`get_cache_count()`](Self::get_cache_count) and [`get_cache_size()`](Self::get_cache_size).
    pub fn get_eviction_count(&self) -> i64 {
        self.get_cache_count() - self.get_cache_size()
    }
    #[cfg(test)]
    pub(crate) fn assert_consistent(&self) -> Result<()> {
        use std::collections::HashSet;
        let inner = self.inner.write();

        if self.requires_eviction(&inner) {
            debug_assert!(
                false,
                "requires evictions: size={}, maxSize={}, ramBytesUsed={}, maxRamBytesUsed={}",
                inner.unique_queries.lock().len(),
                self.max_size,
                self.ram_bytes_used.load(Ordering::Relaxed),
                self.max_ram_bytes_used
            );
        }

        let mru_id_set: HashSet<IdentityQuery> = {
            let uq = inner.unique_queries.lock();
            uq.values().cloned().collect()
        };

        for leaf_cache in inner.cache.values() {
            let mut keys: HashSet<IdentityQuery> = leaf_cache.cache.keys().cloned().collect();
            keys.retain(|k| !mru_id_set.contains(k));
            if !keys.is_empty() {
                debug_assert!(
                    false,
                    "One leaf cache contains more keys than the top-level cache: {:?}",
                    keys
                );
            }
        }

        // TODO: memory calculation not implemented
        let mut recomputed_ram_bytes_used = inner.cache.len() as i64;

        {
            let uq = inner.unique_queries.lock();
            for singleton in uq.values() {
                recomputed_ram_bytes_used += self.get_ram_bytes_used(singleton.query.as_ref());
            }
        }

        for leaf_cache in inner.cache.values() {
            recomputed_ram_bytes_used +=
                // TODO: memory calculation not implemented
                leaf_cache.cache.len() as i64;
            for cached in leaf_cache.cache.values() {
                recomputed_ram_bytes_used += cached.ram_bytes_used()?;
            }
        }

        let current_ram = self.ram_bytes_used.load(Ordering::Relaxed);
        if recomputed_ram_bytes_used != current_ram {
            debug_assert!(
                false,
                "ramBytesUsed mismatch : {} != {}",
                current_ram, recomputed_ram_bytes_used
            );
        }

        let mut recomputed_cache_size: i64 = 0;
        for leaf_cache in inner.cache.values() {
            recomputed_cache_size += leaf_cache.cache.len() as i64;
        }
        if recomputed_cache_size != self.get_cache_size() {
            return Err(LuceneError::illegal_state(format!(
                "cacheSize mismatch : {} != {}",
                self.get_cache_size(),
                recomputed_cache_size
            )));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn cached_queries(&self) -> Vec<Arc<Query>> {
        let inner = self.inner.read();
        let uq = inner.unique_queries.lock();
        uq.keys().cloned().collect()
    }
}
impl<P> Accountable for Arc<LRUQueryCache<P>>
where
    P: Predicate<TopParentMeta>,
{
    fn ram_bytes_used(&self) -> Result<i64> {
        todo!()
    }
}
impl<P> QueryCache for Arc<LRUQueryCache<P>>
where
    P: Predicate<TopParentMeta>,
{
    fn do_cache<LR: LeafReader>(
        &self,
        weight: BoxWeight<LR>,
        _policy: Arc<QueryCachingPolicyEnum>,
    ) -> BoxWeight<LR> {
        weight
    }
}

pub(crate) struct LeafCache {
    key: CacheKey,
    cache: HashMap<IdentityQuery, Arc<CacheAndCountEnum>>,
    ram_bytes_used: AtomicI64,
}
impl LeafCache {
    pub(crate) fn new(key: CacheKey) -> Self {
        Self {
            key,
            cache: HashMap::new(),
            ram_bytes_used: AtomicI64::new(0),
        }
    }
    pub(crate) fn on_doc_id_set_cache<P>(&self, ram_bytes_used: i64, parent: &LRUQueryCache<P>)
    where
        P: Predicate<TopParentMeta>,
    {
        self.ram_bytes_used
            .fetch_add(ram_bytes_used, std::sync::atomic::Ordering::Relaxed);
        parent.on_doc_id_set_cache(&self.key, ram_bytes_used);
    }
    pub(crate) fn on_doc_id_set_eviction<P>(&self, ram_bytes_used: i64, parent: &LRUQueryCache<P>)
    where
        P: Predicate<TopParentMeta>,
    {
        self.ram_bytes_used
            .fetch_sub(ram_bytes_used, std::sync::atomic::Ordering::Relaxed);
        parent.on_doc_id_set_eviction(&self.key, 1, ram_bytes_used);
    }

    pub(crate) fn get(&self, query: &IdentityQuery) -> Option<Arc<CacheAndCountEnum>> {
        // TODO: 没有assert
        self.cache.get(query).cloned()
    }

    pub(crate) fn put_if_absent<P>(
        &mut self,
        query: IdentityQuery,
        cached: CacheAndCountEnum,
        parent: &LRUQueryCache<P>,
    ) where
        P: Predicate<TopParentMeta>,
    {
        // TODO: 没有assert
        match self.cache.entry(query) {
            Entry::Vacant(e) => {
                e.insert(Arc::new(cached));
                self.on_doc_id_set_cache(
                    // TODO: memory calculation not implemented
                    0, parent,
                );
            },
            Entry::Occupied(_) => {},
        }
    }

    pub(crate) fn remove<P>(&mut self, query: &IdentityQuery, parent: &LRUQueryCache<P>)
    where
        P: Predicate<TopParentMeta>,
    {
        if let Some(_removed) = self.cache.remove(query) {
            self.on_doc_id_set_eviction(
                // TODO: memory calculation not implemented
                0, parent,
            );
        }
    }
}
impl Accountable for LeafCache {
    fn ram_bytes_used(&self) -> Result<i64> {
        todo!()
    }
}
pub struct ScorerSupplierImpl1<S, C, P, LR>
where
    LR: LeafReader,
    S: ScorerSupplier<LR>,
    C: CacheHelper,
    P: Predicate<TopParentMeta>,
{
    cost: i64,
    skip_cache_factor: f32,
    supplier: S,
    max_doc: i32,
    lru_query_cache: Arc<LRUQueryCache<P>>,
    query: Arc<Query>,
    cache_helper: C,
    _marker: PhantomData<LR>,
}
impl<S, C, P, LR> ScorerSupplierImpl1<S, C, P, LR>
where
    LR: LeafReader,
    S: ScorerSupplier<LR>,
    C: CacheHelper,
    P: Predicate<TopParentMeta>,
{
    pub(crate) fn new(
        cost: i64,
        skip_cache_factor: f32,
        supplier: S,
        max_doc: i32,
        lru_query_cache: Arc<LRUQueryCache<P>>,
        query: Arc<Query>,
        cache_helper: C,
    ) -> Result<Self> {
        Ok(Self {
            cost,
            skip_cache_factor,
            supplier,
            max_doc,
            lru_query_cache,
            query,
            cache_helper,
            _marker: PhantomData,
        })
    }
}
#[allow(clippy::upper_case_acronyms)]
pub type DISI = DocIdSetIteratorEnum2<EmptyDISI, CacheAndCountDISI>;
impl<S, C, P, LR> ScorerSupplier<LR> for ScorerSupplierImpl1<S, C, P, LR>
where
    LR: LeafReader,
    S: ScorerSupplier<LR>,
    C: CacheHelper,
    P: Predicate<TopParentMeta>,
{
    type Scorer = ScorerEnum2<S::Scorer, ConstantScoreScorer<DISI, DummyTwoPhaseIterator>>;
    type BulkScorer = DefaultBulkScorer<Self::Scorer>;

    fn get(&mut self, lead_cost: i64, context: &LeafReaderContext<LR>) -> Result<Self::Scorer> {
        if (self.cost as f32 / self.skip_cache_factor) > lead_cost as f32 {
            let scorer = self.supplier.get(lead_cost, context)?;
            return Ok(ScorerEnum2::A(scorer));
        };
        let mut bulk_scorer = match self.supplier.bulk_scorer(context)? {
            Some(bulk_scorer) => bulk_scorer,
            None => return Err(LuceneError::illegal_state("BulkScorer should not be None")),
        };
        let cached = cache_impl(&mut bulk_scorer, self.max_doc.try_convert()?)?;
        let disi = cached.iterator()?;
        self.lru_query_cache
            .put_if_absent(self.query.clone(), cached, &self.cache_helper);
        let disi = match disi {
            Some(disi) => DISI::B(disi),
            None => DISI::A(EmptyDISI::default()),
        };
        Ok(ScorerEnum2::B(ConstantScoreScorer::with_disi(
            0.0,
            ScoreMode::CompleteNoScores,
            disi,
        )))
    }

    fn bulk_scorer(&mut self, context: &LeafReaderContext<LR>) -> Result<Option<Self::BulkScorer>> {
        Ok(Some(self.default_bulk_scorer(context)?))
    }

    fn cost(&mut self, _context: &LeafReaderContext<LR>) -> Result<i64> {
        Ok(self.cost)
    }
}

pub struct ScorerSupplierImpl2 {
    disi: CacheAndCountDISI,
    cost: i64,
}
impl ScorerSupplierImpl2 {
    pub(crate) fn new(disi: CacheAndCountDISI) -> Result<Self> {
        let cost = disi.cost()?;
        Ok(Self { disi, cost })
    }
}
impl<LR> ScorerSupplier<LR> for ScorerSupplierImpl2
where
    LR: LeafReader,
{
    type Scorer = ConstantScoreScorer<CacheAndCountDISI, DummyTwoPhaseIterator>;
    type BulkScorer = DefaultBulkScorer<Self::Scorer>;

    fn get(&mut self, _lead_cost: i64, _context: &LeafReaderContext<LR>) -> Result<Self::Scorer> {
        Ok(ConstantScoreScorer::with_disi(
            0.0,
            ScoreMode::CompleteNoScores,
            std::mem::take(&mut self.disi),
        ))
    }

    fn bulk_scorer(&mut self, context: &LeafReaderContext<LR>) -> Result<Option<Self::BulkScorer>> {
        Ok(Some(self.default_bulk_scorer(context)?))
    }

    fn cost(&mut self, _context: &LeafReaderContext<LR>) -> Result<i64> {
        Ok(self.cost)
    }
}
pub type CachingWrapperWeightSupplier<LR> =
    crate::core::search::dummy::dummy_scorer_supplier::DummyScorerSupplier;
pub type CachingWrapperWeightScorer<LR> =
    <CachingWrapperWeightSupplier<LR> as ScorerSupplier<LR>>::Scorer;
pub type CachingWrapperWeightBulkScorer<LR> =
    <CachingWrapperWeightSupplier<LR> as ScorerSupplier<LR>>::BulkScorer;
/// Cache of doc ids with a count.
pub(crate) struct CacheAndCount<D>
where
    D: DocIdSet,
{
    cache: D,
    count: usize,
}
impl CacheAndCount<EmptyDocIdSet> {
    pub(crate) fn empty() -> Self {
        Self {
            cache: EmptyDocIdSet,
            count: 0,
        }
    }
}

impl<D> CacheAndCount<D>
where
    D: DocIdSet,
{
    pub(crate) fn new(cache: D, count: usize) -> Self {
        Self { cache, count }
    }

    pub(crate) fn iterator(&self) -> Result<Option<D::DocIdSetIterator>> {
        self.cache.iterator()
    }
    pub(crate) fn count(&self) -> usize {
        self.count
    }
}
impl<D> Accountable for CacheAndCount<D>
where
    D: DocIdSet,
{
    fn ram_bytes_used(&self) -> Result<i64> {
        todo!()
    }
}

fn cache_into_bit_set<BS>(
    scorer: &mut BS,
    max_doc: usize,
) -> Result<CacheAndCount<BitDocIdSet<FixedBitSet>>>
where
    BS: BulkScorer,
{
    let mut collector = LeafCollectorImpl::new(max_doc);
    scorer.score(&mut collector, None::<&DummyBits>, 0, NO_MORE_DOCS)?;
    let v = BitDocIdSet::with_cost(
        Some(std::mem::take(&mut collector.bit_set)),
        collector.count as i64,
    )?;
    Ok(CacheAndCount::new(v, collector.count))
}

struct LeafCollectorImpl {
    bit_set: FixedBitSet,
    count: usize,
}
impl LeafCollectorImpl {
    fn new(max_doc: usize) -> Self {
        Self {
            bit_set: FixedBitSet::new(max_doc),
            count: 0,
        }
    }
}

impl Display for LeafCollectorImpl {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", std::any::type_name::<Self>())
    }
}

impl LeafCollector for LeafCollectorImpl {
    fn collect<S>(&mut self, doc: i32, _scorer: &mut S) -> Result<()>
    where
        S: Scorable,
    {
        self.count += 1;
        self.bit_set.set(doc.try_convert()?);
        Ok(())
    }

    type DocIdSetIteratorRef<'a>
        = DummyDISI
    where
        Self: 'a;
}

fn cache_into_roaring_doc_id_set<BS>(
    scorer: &mut BS,
    max_doc: usize,
) -> Result<CacheAndCount<RoaringDocIdSet>>
where
    BS: BulkScorer,
{
    let mut collector = RoaringCollectorImpl::new(max_doc);
    scorer.score(&mut collector, None::<&DummyBits>, 0, NO_MORE_DOCS)?;
    let cache = collector.builder.build();
    let cardinality = cache.cardinality();
    Ok(CacheAndCount::new(cache, cardinality))
}

struct RoaringCollectorImpl {
    builder: Builder,
}

impl RoaringCollectorImpl {
    fn new(max_doc: usize) -> Self {
        Self {
            builder: Builder::new(max_doc),
        }
    }
}

impl Display for RoaringCollectorImpl {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", std::any::type_name::<Self>())
    }
}

impl LeafCollector for RoaringCollectorImpl {
    fn collect<S>(&mut self, doc: i32, _scorer: &mut S) -> Result<()>
    where
        S: Scorable,
    {
        self.builder.add(doc)?;
        Ok(())
    }

    type DocIdSetIteratorRef<'a>
        = DummyDISI
    where
        Self: 'a;
}
pub(crate) enum CacheAndCountEnum {
    BitSet(CacheAndCount<BitDocIdSet<FixedBitSet>>),
    Roaring(CacheAndCount<RoaringDocIdSet>),
    Empty(CacheAndCount<EmptyDocIdSet>),
}
impl CacheAndCountEnum {
    pub(crate) fn count(&self) -> usize {
        match self {
            CacheAndCountEnum::BitSet(c) => c.count(),
            CacheAndCountEnum::Roaring(c) => c.count(),
            CacheAndCountEnum::Empty(c) => c.count(),
        }
    }
    pub(crate) fn iterator(&self) -> Result<Option<CacheAndCountDISI>> {
        match self {
            CacheAndCountEnum::BitSet(c) => Ok(c.iterator()?.map(DocIdSetIteratorEnum3::B)),
            CacheAndCountEnum::Roaring(c) => Ok(c.iterator()?.map(DocIdSetIteratorEnum3::C)),
            CacheAndCountEnum::Empty(c) => Ok(c.iterator()?.map(DocIdSetIteratorEnum3::A)),
        }
    }
}
impl Accountable for CacheAndCountEnum {
    fn ram_bytes_used(&self) -> Result<i64> {
        match self {
            CacheAndCountEnum::BitSet(c) => c.ram_bytes_used(),
            CacheAndCountEnum::Roaring(c) => c.ram_bytes_used(),
            CacheAndCountEnum::Empty(c) => c.ram_bytes_used(),
        }
    }
}
pub type CacheAndCountDISI = DocIdSetIteratorEnum3<
    <EmptyDocIdSet as DocIdSet>::DocIdSetIterator,
    <BitDocIdSet<FixedBitSet> as DocIdSet>::DocIdSetIterator,
    <RoaringDocIdSet as DocIdSet>::DocIdSetIterator,
>;
// for std::mem::take
impl Default for CacheAndCountDISI {
    fn default() -> Self {
        DocIdSetIteratorEnum3::A(EmptyDISI::default())
    }
}
/// Default cache implementation: uses [`RoaringDocIdSet`] for sets that have a density < 1%,
/// and a [`BitDocIdSet`] over a [`FixedBitSet`] otherwise.
fn cache_impl<BS>(scorer: &mut BS, max_doc: usize) -> Result<CacheAndCountEnum>
where
    BS: BulkScorer,
{
    let cost = scorer.cost()?;
    if cost * 100 >= max_doc as i64 {
        // FixedBitSet is faster for dense sets and will enable the random-access
        // optimization in ConjunctionDISI
        let v = cache_into_bit_set(scorer, max_doc)?;
        Ok(CacheAndCountEnum::BitSet(v))
    } else {
        let v = cache_into_roaring_doc_id_set(scorer, max_doc)?;
        Ok(CacheAndCountEnum::Roaring(v))
    }
}
pub struct MinSegmentSizePredicate {
    min_size: i32,
}
impl MinSegmentSizePredicate {
    pub(crate) fn new(min_size: i32) -> Self {
        Self { min_size }
    }
}
impl Predicate<TopParentMeta> for MinSegmentSizePredicate {
    fn test(&self, context: &TopParentMeta) -> Result<bool> {
        let max_doc = context.max_doc;
        if max_doc < self.min_size {
            return Ok(false);
        }
        let doc = context.max_doc;
        let size: i32 = context.leaves_num.try_convert()?;
        let average_total_docs = doc / size;
        Ok((max_doc * 2) > average_total_docs)
    }
}
