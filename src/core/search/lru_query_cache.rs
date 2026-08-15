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
use crate::core::index::index_reader::{CacheHelper, CacheKey, Identity, IndexReader};
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::{LeafReaderContext, TopParentMeta};
use crate::core::index::reader_util::ReaderUtil;
use crate::core::search::bulk_scorer::BulkScorer;
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::constant_score_weight::ConstantScoreWeight;
use crate::core::search::doc_id_set::{DocIdSet, EmptyDocIdSet};
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::doc_id_set_iterator::{
  DocIdSetIterator, DocIdSetIteratorEnum2, DocIdSetIteratorEnum3, EmptyDISI,
};

use crate::core::search::explanation::Explanation;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::leaf_collector::LeafCollector;
use crate::core::search::query::{
  Query, QueryBase, QueryWeight, QueryWeightSs, QueryWeightSsBulkScorer, QueryWeightSsScorer,
};
use crate::core::search::query_cache::QueryCache;
use crate::core::search::query_caching_policy::{QueryCachingPolicy, QueryCachingPolicyEnum};
use crate::core::search::scorable::Scorable;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::weight::Weight;
use crate::core::util::accountable::Accountable;
use crate::core::util::bit_doc_id_set::BitDocIdSet;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::predicate::Predicate;
use crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED;
use crate::core::util::roaring_doc_id_set::Builder;
use crate::core::util::roaring_doc_id_set::RoaringDocIdSet;
use crate::core::util::{HasIdentity, TryIntoInt};
#[cfg(test)]
use crate::test_framework::core::search::test_lru_query_cache::{
  EvictEmptySegmentCacheLRUQueryCache, FineGrainedStatsLRUQueryCache,
};
use linked_hash_map::LinkedHashMap;
use parking_lot::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fmt::{Display, Formatter};
use std::mem;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

pub(crate) const HASHTABLE_RAM_BYTES_PER_ENTRY: i64 =
  mem::size_of::<(CacheKey, LeafCache)>() as i64;
const LEAF_CACHE_HASHTABLE_RAM_BYTES_PER_ENTRY: i64 =
  mem::size_of::<(Identity, Arc<CacheAndCountEnum>)>() as i64;
const LINKED_HASHTABLE_RAM_BYTES_PER_ENTRY: i64 =
  mem::size_of::<(Arc<Query>, Arc<Query>, usize, usize)>() as i64;

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
/// per-query-type statistics, customize the following callbacks:
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
pub struct LRUQueryCache<P> {
  hook: LRUQueryCacheHook,
  max_size: i32,
  max_ram_bytes_used: i64,
  skip_cache_factor: f32,
  hit_count: AtomicU64,
  miss_count: AtomicU64,
  // These atomics avoid locking reads.
  // but increments need to be performed under the lock
  ram_bytes_used: AtomicI64,
  cache_count: AtomicI64,
  cache_size: AtomicI64,
  inner: RwLock<Inner>,
  leaves_to_cache: P,
}
pub struct Inner {
  unique_queries: Mutex<LinkedHashMap<Arc<Query>, Arc<Query>>>,
  cache: HashMap<CacheKey, LeafCache>,
}

#[derive(Clone, Default)]
pub(crate) enum LRUQueryCacheHook {
  #[default]
  Default,
  #[cfg(test)]
  FineGrainedStats(FineGrainedStatsLRUQueryCache),
  #[cfg(test)]
  EvictEmptySegmentCache(EvictEmptySegmentCacheLRUQueryCache),
}

pub(crate) struct LRUQueryCacheDefaults;

pub(crate) trait LRUQueryCacheBase<P>
where
  P: Predicate<TopParentMeta>,
{
  fn on_hit(&self, cache: &LRUQueryCache<P>, reader_core_key: &CacheKey, query: &Query) {
    LRUQueryCacheDefaults::on_hit(cache, reader_core_key, query);
  }

  fn on_miss(&self, cache: &LRUQueryCache<P>, reader_core_key: &CacheKey, query: &Query) {
    LRUQueryCacheDefaults::on_miss(cache, reader_core_key, query);
  }

  fn on_query_cache(
    &self,
    cache: &LRUQueryCache<P>,
    query: &Query,
    ram_bytes_used: i64,
    guard: &RwLockWriteGuard<'_, Inner>,
  ) {
    LRUQueryCacheDefaults::on_query_cache(cache, query, ram_bytes_used, guard);
  }

  fn on_query_eviction(
    &self,
    cache: &LRUQueryCache<P>,
    query: &Query,
    ram_bytes_used: i64,
    guard: &RwLockWriteGuard<'_, Inner>,
  ) {
    LRUQueryCacheDefaults::on_query_eviction(cache, query, ram_bytes_used, guard);
  }

  fn on_doc_id_set_cache(
    &self,
    cache: &LRUQueryCache<P>,
    reader_core_key: &CacheKey,
    ram_bytes_used: i64,
  ) {
    LRUQueryCacheDefaults::on_doc_id_set_cache(cache, reader_core_key, ram_bytes_used);
  }

  fn on_doc_id_set_eviction(
    &self,
    cache: &LRUQueryCache<P>,
    reader_core_key: &CacheKey,
    num_entries: i64,
    sum_ram_bytes_used: i64,
  ) {
    LRUQueryCacheDefaults::on_doc_id_set_eviction(
      cache,
      reader_core_key,
      num_entries,
      sum_ram_bytes_used,
    );
  }

  fn on_clear(&self, cache: &LRUQueryCache<P>, guard: &RwLockWriteGuard<'_, Inner>) {
    LRUQueryCacheDefaults::on_clear(cache, guard);
  }
}

impl<P> LRUQueryCacheBase<P> for LRUQueryCacheHook
where
  P: Predicate<TopParentMeta>,
{
  fn on_hit(&self, cache: &LRUQueryCache<P>, reader_core_key: &CacheKey, query: &Query) {
    match self {
      Self::Default => LRUQueryCacheDefaults::on_hit(cache, reader_core_key, query),
      #[cfg(test)]
      Self::FineGrainedStats(hook) => hook.on_hit(cache, reader_core_key, query),
      #[cfg(test)]
      Self::EvictEmptySegmentCache(hook) => hook.on_hit(cache, reader_core_key, query),
    }
  }

  fn on_miss(&self, cache: &LRUQueryCache<P>, reader_core_key: &CacheKey, query: &Query) {
    match self {
      Self::Default => LRUQueryCacheDefaults::on_miss(cache, reader_core_key, query),
      #[cfg(test)]
      Self::FineGrainedStats(hook) => hook.on_miss(cache, reader_core_key, query),
      #[cfg(test)]
      Self::EvictEmptySegmentCache(hook) => hook.on_miss(cache, reader_core_key, query),
    }
  }

  fn on_query_cache(
    &self,
    cache: &LRUQueryCache<P>,
    query: &Query,
    ram_bytes_used: i64,
    guard: &RwLockWriteGuard<'_, Inner>,
  ) {
    match self {
      Self::Default => LRUQueryCacheDefaults::on_query_cache(cache, query, ram_bytes_used, guard),
      #[cfg(test)]
      Self::FineGrainedStats(hook) => hook.on_query_cache(cache, query, ram_bytes_used, guard),
      #[cfg(test)]
      Self::EvictEmptySegmentCache(hook) => {
        hook.on_query_cache(cache, query, ram_bytes_used, guard)
      },
    }
  }

  fn on_query_eviction(
    &self,
    cache: &LRUQueryCache<P>,
    query: &Query,
    ram_bytes_used: i64,
    guard: &RwLockWriteGuard<'_, Inner>,
  ) {
    match self {
      Self::Default => {
        LRUQueryCacheDefaults::on_query_eviction(cache, query, ram_bytes_used, guard)
      },
      #[cfg(test)]
      Self::FineGrainedStats(hook) => hook.on_query_eviction(cache, query, ram_bytes_used, guard),
      #[cfg(test)]
      Self::EvictEmptySegmentCache(hook) => {
        hook.on_query_eviction(cache, query, ram_bytes_used, guard)
      },
    }
  }

  fn on_doc_id_set_cache(
    &self,
    cache: &LRUQueryCache<P>,
    reader_core_key: &CacheKey,
    ram_bytes_used: i64,
  ) {
    match self {
      Self::Default => {
        LRUQueryCacheDefaults::on_doc_id_set_cache(cache, reader_core_key, ram_bytes_used)
      },
      #[cfg(test)]
      Self::FineGrainedStats(hook) => {
        hook.on_doc_id_set_cache(cache, reader_core_key, ram_bytes_used)
      },
      #[cfg(test)]
      Self::EvictEmptySegmentCache(hook) => {
        hook.on_doc_id_set_cache(cache, reader_core_key, ram_bytes_used)
      },
    }
  }

  fn on_doc_id_set_eviction(
    &self,
    cache: &LRUQueryCache<P>,
    reader_core_key: &CacheKey,
    num_entries: i64,
    sum_ram_bytes_used: i64,
  ) {
    match self {
      Self::Default => LRUQueryCacheDefaults::on_doc_id_set_eviction(
        cache,
        reader_core_key,
        num_entries,
        sum_ram_bytes_used,
      ),
      #[cfg(test)]
      Self::FineGrainedStats(hook) => {
        hook.on_doc_id_set_eviction(cache, reader_core_key, num_entries, sum_ram_bytes_used)
      },
      #[cfg(test)]
      Self::EvictEmptySegmentCache(hook) => {
        hook.on_doc_id_set_eviction(cache, reader_core_key, num_entries, sum_ram_bytes_used)
      },
    }
  }

  fn on_clear(&self, cache: &LRUQueryCache<P>, guard: &RwLockWriteGuard<'_, Inner>) {
    match self {
      Self::Default => LRUQueryCacheDefaults::on_clear(cache, guard),
      #[cfg(test)]
      Self::FineGrainedStats(hook) => hook.on_clear(cache, guard),
      #[cfg(test)]
      Self::EvictEmptySegmentCache(hook) => hook.on_clear(cache, guard),
    }
  }
}

impl LRUQueryCacheDefaults {
  pub(crate) fn on_hit<P>(cache: &LRUQueryCache<P>, _reader_core_key: &CacheKey, _query: &Query)
  where
    P: Predicate<TopParentMeta>,
  {
    cache.hit_count.fetch_add(1, Ordering::Relaxed);
  }

  pub(crate) fn on_miss<P>(cache: &LRUQueryCache<P>, _reader_core_key: &CacheKey, _query: &Query)
  where
    P: Predicate<TopParentMeta>,
  {
    cache.miss_count.fetch_add(1, Ordering::Relaxed);
  }

  pub(crate) fn on_query_cache<P>(
    cache: &LRUQueryCache<P>,
    _query: &Query,
    ram_bytes_used: i64,
    _guard: &RwLockWriteGuard<'_, Inner>,
  ) where
    P: Predicate<TopParentMeta>,
  {
    cache
      .ram_bytes_used
      .fetch_add(ram_bytes_used, Ordering::SeqCst);
  }

  pub(crate) fn on_query_eviction<P>(
    cache: &LRUQueryCache<P>,
    _query: &Query,
    ram_bytes_used: i64,
    _guard: &RwLockWriteGuard<'_, Inner>,
  ) where
    P: Predicate<TopParentMeta>,
  {
    cache
      .ram_bytes_used
      .fetch_sub(ram_bytes_used, Ordering::SeqCst);
  }

  pub(crate) fn on_doc_id_set_cache<P>(
    cache: &LRUQueryCache<P>,
    _reader_core_key: &CacheKey,
    ram_bytes_used: i64,
  ) where
    P: Predicate<TopParentMeta>,
  {
    cache.cache_size.fetch_add(1, Ordering::SeqCst);
    cache.cache_count.fetch_add(1, Ordering::SeqCst);
    cache
      .ram_bytes_used
      .fetch_add(ram_bytes_used, Ordering::SeqCst);
  }

  pub(crate) fn on_doc_id_set_eviction<P>(
    cache: &LRUQueryCache<P>,
    _reader_core_key: &CacheKey,
    num_entries: i64,
    sum_ram_bytes_used: i64,
  ) where
    P: Predicate<TopParentMeta>,
  {
    cache
      .ram_bytes_used
      .fetch_sub(sum_ram_bytes_used, Ordering::SeqCst);
    cache.cache_size.fetch_sub(num_entries, Ordering::SeqCst);
  }

  pub(crate) fn on_clear<P>(cache: &LRUQueryCache<P>, _guard: &RwLockWriteGuard<'_, Inner>)
  where
    P: Predicate<TopParentMeta>,
  {
    cache.ram_bytes_used.store(0, Ordering::SeqCst);
    cache.cache_size.store(0, Ordering::SeqCst);
  }
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

impl<P> LRUQueryCache<P> {
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
      hook: LRUQueryCacheHook::Default,
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
}

impl<P> LRUQueryCache<P>
where
  P: Predicate<TopParentMeta>,
{
  #[cfg(test)]
  pub(crate) fn with_hook(mut self, hook: LRUQueryCacheHook) -> Self {
    self.hook = hook;
    self
  }
  /// Expert: callback when there is a cache hit on a given query.
  /// Implementing this method is typically useful in order to compute
  /// more fine-grained statistics about the query cache.
  ///
  /// See also [`on_miss`](Self::on_miss).
  ///
  /// Experimental: this API follows the original Lucene experimental status.
  pub(crate) fn on_hit(&self, reader_core_key: &CacheKey, query: &Query) {
    <LRUQueryCacheHook as LRUQueryCacheBase<P>>::on_hit(&self.hook, self, reader_core_key, query);
  }
  /// Expert: callback when there is a cache miss on a given query.
  ///
  /// See also [`on_hit`](Self::on_hit).
  ///
  /// Experimental: this API follows the original Lucene experimental status.
  pub(crate) fn on_miss(&self, reader_core_key: &CacheKey, query: &Query) {
    <LRUQueryCacheHook as LRUQueryCacheBase<P>>::on_miss(&self.hook, self, reader_core_key, query);
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
    query: &Query,
    ram_bytes_used: i64,
    guard: &RwLockWriteGuard<Inner>,
  ) {
    <LRUQueryCacheHook as LRUQueryCacheBase<P>>::on_query_cache(
      &self.hook,
      self,
      query,
      ram_bytes_used,
      guard,
    );
  }
  /// Expert: callback when a query is evicted from this cache.
  ///
  /// See also [`on_query_cache`](Self::on_query_cache).
  ///
  /// Experimental: this API follows the original Lucene experimental status.
  pub(crate) fn on_query_eviction(
    &self,
    query: &Query,
    ram_bytes_used: i64,
    guard: &RwLockWriteGuard<Inner>,
  ) {
    <LRUQueryCacheHook as LRUQueryCacheBase<P>>::on_query_eviction(
      &self.hook,
      self,
      query,
      ram_bytes_used,
      guard,
    );
  }
  /// Expert: callback when a [`DocIdSet`] is added to this cache.
  /// Implementing this method is typically useful in order to compute
  /// more fine-grained statistics about the query cache.
  ///
  /// See also [`on_doc_id_set_eviction`](Self::on_doc_id_set_eviction).
  ///
  /// Experimental: this API follows the original Lucene experimental status.
  pub(crate) fn on_doc_id_set_cache(&self, reader_core_key: &CacheKey, ram_bytes_used: i64) {
    <LRUQueryCacheHook as LRUQueryCacheBase<P>>::on_doc_id_set_cache(
      &self.hook,
      self,
      reader_core_key,
      ram_bytes_used,
    );
  }

  /// Expert: callback when one or more [`DocIdSet`]s are removed from this cache.
  ///
  /// See also [`on_docidset_cache`](Self::on_docidset_cache).
  ///
  /// Experimental: this API follows the original Lucene experimental status.
  pub(crate) fn on_doc_id_set_eviction(
    &self,
    reader_core_key: &CacheKey,
    num_entries: i64,
    sum_ram_bytes_used: i64,
  ) {
    <LRUQueryCacheHook as LRUQueryCacheBase<P>>::on_doc_id_set_eviction(
      &self.hook,
      self,
      reader_core_key,
      num_entries,
      sum_ram_bytes_used,
    );
  }
  /// Expert: callback when the cache is completely cleared.
  ///
  /// Experimental: this API follows the original Lucene experimental status.
  pub(crate) fn on_clear(&self, guard: &RwLockWriteGuard<Inner>) {
    <LRUQueryCacheHook as LRUQueryCacheBase<P>>::on_clear(&self.hook, self, guard);
  }
  /// Whether evictions are required.
  pub(crate) fn requires_eviction(&self, guard: &RwLockWriteGuard<Inner>) -> bool {
    let size = guard.unique_queries.lock().len();
    if size == 0 {
      return false;
    }
    size as i32 > self.max_size
      || self.ram_bytes_used.load(Ordering::SeqCst) > self.max_ram_bytes_used
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
    debug_assert!({ !matches!(key, Query::Boost(_)) });
    debug_assert!({ !matches!(key, Query::ConstantScore(_)) });
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
        self.on_hit(&reader_key, singleton.as_ref());
        Some(c)
      },
      None => {
        self.on_miss(&reader_key, singleton.as_ref());
        None
      },
    }
  }

  pub(crate) fn put_if_absent<C>(
    self: &Arc<Self>,
    query: Arc<Query>,
    cached: CacheAndCountEnum,
    cache_helper: &C,
  ) -> Result<()>
  where
    C: CacheHelper,
    P: Send + Sync + 'static,
  {
    debug_assert!({ !matches!(query.as_ref(), Query::Boost(_)) });
    debug_assert!({ !matches!(query.as_ref(), Query::ConstantScore(_)) });
    // under a lock to make sure that mostRecentlyUsedQueries and cache remain sync'ed
    let mut inner = self.inner.write();

    let (singleton, inserted) = {
      let mut uq = inner.unique_queries.lock();
      if let Some(iq) = uq.get_refresh(query.as_ref()) {
        (iq.clone(), false)
      } else {
        let prev = uq.insert(query.clone(), query.clone());
        debug_assert!(prev.is_none());
        (query, true)
      }
    };

    if inserted {
      self.on_query_cache(
        singleton.as_ref(),
        self.get_ram_bytes_used(singleton.as_ref()),
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
          HASHTABLE_RAM_BYTES_PER_ENTRY,
          std::sync::atomic::Ordering::SeqCst,
        );
        let cache = Arc::downgrade(self);
        cache_helper.add_closed_listener(Arc::new(move |core_key: &CacheKey| {
          if let Some(cache) = cache.upgrade() {
            cache.clear_core_cache_key(core_key);
          }
          Ok(())
        }))?;
        lc_ref
      },
    };

    leaf_cache.put_if_absent(singleton.as_ref(), cached, self);
    self.evict_if_necessary(&mut inner)
  }
  pub(crate) fn evict_if_necessary(&self, guard: &mut RwLockWriteGuard<Inner>) -> Result<()> {
    loop {
      if !self.requires_eviction(guard) {
        break;
      }

      let singleton = {
        let mut unique_queries = guard.unique_queries.lock();
        let size = unique_queries.len();
        let (query, singleton) = {
          let mut iterator = unique_queries.entries();
          match iterator.next() {
            Some(entry) => (entry.key().clone(), entry.get().clone()),
            None => break,
          }
        };
        let _ = unique_queries.remove(query.as_ref());
        if size == unique_queries.len() {
          // Defensive parity with Java Lucene: production Rust query keys are expected to keep
          // their Hash/Eq state stable after entering the cache. If a future interior-mutable
          // query violates that invariant, fail fast instead of silently leaving cache state
          // inconsistent.
          // size did not decrease, because the hash of the query changed since it has been
          // put into the cache
          return Err(LuceneError::concurrent_modification(format!(
            "Removal from the cache failed! This \
             is probably due to a query which has been modified after having been put into \
             the cache or a badly implemented clone(). Query class: [{}], query: [{}]",
            query.name(),
            query.to_string("").unwrap_or_else(|_| format!("{query:?}"))
          )));
        }
        singleton
      };
      self.on_eviction(singleton.as_ref(), guard);
    }
    Ok(())
  }

  /// Remove all cache entries for the given core cache key.
  pub(crate) fn clear_core_cache_key(&self, core_key: &CacheKey) {
    let mut inner = self.inner.write();

    if let Some(leaf_cache) = inner.cache.remove(core_key) {
      self.ram_bytes_used.fetch_sub(
        HASHTABLE_RAM_BYTES_PER_ENTRY,
        std::sync::atomic::Ordering::SeqCst,
      );

      let num_entries = leaf_cache.cache.len();
      debug_assert!(num_entries <= i64::MAX as usize);
      if num_entries > 0 {
        self.on_doc_id_set_eviction(
          core_key,
          num_entries as i64,
          leaf_cache
            .ram_bytes_used
            .load(std::sync::atomic::Ordering::SeqCst),
        );
      } else {
        debug_assert_eq!(num_entries, 0);
        debug_assert_eq!(
          leaf_cache
            .ram_bytes_used
            .load(std::sync::atomic::Ordering::SeqCst),
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
      self.on_eviction(singleton.as_ref(), &mut inner);
    }
  }

  pub(crate) fn on_eviction(&self, singleton: &Query, guard: &mut RwLockWriteGuard<Inner>) {
    self.on_query_eviction(singleton, self.get_ram_bytes_used(singleton), guard);

    for leaf_cache in guard.cache.values_mut() {
      leaf_cache.remove(singleton, self);
    }
  }
  /// Clear the content of this cache.
  pub(crate) fn clear(&self) {
    let mut inner = self.inner.write();
    inner.cache.clear();
    inner.unique_queries.lock().clear();
    self.on_clear(&inner);
  }
  fn get_ram_bytes_used(&self, query: &Query) -> i64 {
    let query_ram_bytes_used = query
      .ram_bytes_used()
      .unwrap_or(QUERY_DEFAULT_RAM_BYTES_USED);
    LINKED_HASHTABLE_RAM_BYTES_PER_ENTRY.saturating_add(query_ram_bytes_used)
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
    self.cache_size.load(Ordering::SeqCst)
  }
  /// Return the total number of cache entries that have been generated and put in the cache.
  /// It is highly desirable to have a [`get_hit_count()`](Self::get_hit_count) that is much higher
  /// than the [`get_cache_count()`](Self::get_cache_count), as the opposite would indicate that
  /// the query cache makes efforts in order to cache queries but then they do not get reused.
  ///
  /// See also [`get_cache_size()`](Self::get_cache_size) and [`get_eviction_count()`](Self::get_eviction_count).
  pub fn get_cache_count(&self) -> i64 {
    self.cache_count.load(Ordering::SeqCst)
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
  pub fn assert_consistent(&self) -> Result<()> {
    let inner = self.inner.write();

    if self.requires_eviction(&inner) {
      return Err(LuceneError::illegal_state(format!(
        "requires evictions: size={}, maxSize={}, ramBytesUsed={}, maxRamBytesUsed={}",
        inner.unique_queries.lock().len(),
        self.max_size,
        self.ram_bytes_used.load(Ordering::SeqCst),
        self.max_ram_bytes_used
      )));
    }

    let unique_query_identities = {
      let uq = inner.unique_queries.lock();
      uq.values()
        .map(|singleton| singleton.identity().clone())
        .collect::<std::collections::HashSet<_>>()
    };

    for leaf_cache in inner.cache.values() {
      let keys = leaf_cache
        .cache
        .keys()
        .filter(|key| !unique_query_identities.contains(*key))
        .cloned()
        .collect::<Vec<_>>();
      if !keys.is_empty() {
        return Err(LuceneError::illegal_state(format!(
          "One leaf cache contains more keys than the top-level cache: {:?}",
          keys
        )));
      }
    }

    let mut recomputed_ram_bytes_used =
      HASHTABLE_RAM_BYTES_PER_ENTRY.saturating_mul(inner.cache.len() as i64);

    {
      let uq = inner.unique_queries.lock();
      for singleton in uq.values() {
        recomputed_ram_bytes_used += self.get_ram_bytes_used(singleton.as_ref());
      }
    }

    for leaf_cache in inner.cache.values() {
      for cached in leaf_cache.cache.values() {
        recomputed_ram_bytes_used += LeafCache::ram_bytes_used_for_cache_entry(cached.as_ref());
      }
    }

    let current_ram = self.ram_bytes_used.load(Ordering::SeqCst);
    if recomputed_ram_bytes_used != current_ram {
      return Err(LuceneError::illegal_state(format!(
        "ramBytesUsed mismatch : {} != {}",
        current_ram, recomputed_ram_bytes_used
      )));
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
impl<P> Accountable for LRUQueryCache<P>
where
  P: Predicate<TopParentMeta>,
{
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(self.ram_bytes_used.load(Ordering::SeqCst))
  }
}
impl<P, IRC> QueryCache<IRC> for Arc<LRUQueryCache<P>>
where
  P: Predicate<TopParentMeta> + Send + Sync + 'static,
  IRC: IndexReaderContext,
{
  fn do_cache(
    &self,
    mut weight: QueryWeight<IRC>,
    policy: Arc<QueryCachingPolicyEnum>,
  ) -> Result<QueryWeight<IRC>>
  where
    IRC: IndexReaderContext + 'static,
  {
    while weight.is_cache_wrapper() {
      weight = weight.into_inner_weight().ok_or_else(|| {
        LuceneError::illegal_state("cache wrapper weights must expose their inner weight")
      })?;
    }
    Ok(Box::new(CachingWrapperWeight::new(
      weight,
      policy,
      self.clone(),
    )))
  }
}

pub(crate) struct LeafCache {
  key: CacheKey,
  cache: HashMap<Identity, Arc<CacheAndCountEnum>>,
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
    self
      .ram_bytes_used
      .fetch_add(ram_bytes_used, std::sync::atomic::Ordering::SeqCst);
    parent.on_doc_id_set_cache(&self.key, ram_bytes_used);
  }
  pub(crate) fn on_doc_id_set_eviction<P>(&self, ram_bytes_used: i64, parent: &LRUQueryCache<P>)
  where
    P: Predicate<TopParentMeta>,
  {
    self
      .ram_bytes_used
      .fetch_sub(ram_bytes_used, std::sync::atomic::Ordering::SeqCst);
    parent.on_doc_id_set_eviction(&self.key, 1, ram_bytes_used);
  }

  pub(crate) fn get(&self, query: &Query) -> Option<Arc<CacheAndCountEnum>> {
    debug_assert!({ !matches!(query, Query::Boost(_)) });
    debug_assert!({ !matches!(query, Query::ConstantScore(_)) });
    self.cache.get(query.identity()).cloned()
  }

  pub(crate) fn put_if_absent<P>(
    &mut self,
    query: &Query,
    cached: CacheAndCountEnum,
    parent: &LRUQueryCache<P>,
  ) where
    P: Predicate<TopParentMeta>,
  {
    debug_assert!({ !matches!(query, Query::Boost(_)) });
    debug_assert!({ !matches!(query, Query::ConstantScore(_)) });
    match self.cache.entry(query.identity().clone()) {
      Entry::Vacant(e) => {
        let cached = Arc::new(cached);
        let ram_bytes_used = Self::ram_bytes_used_for_cache_entry(cached.as_ref());
        e.insert(cached);
        self.on_doc_id_set_cache(ram_bytes_used, parent);
      },
      Entry::Occupied(_) => {},
    }
  }

  pub(crate) fn remove<P>(&mut self, query: &Query, parent: &LRUQueryCache<P>)
  where
    P: Predicate<TopParentMeta>,
  {
    if let Some(removed) = self.cache.remove(query.identity()) {
      self.on_doc_id_set_eviction(
        Self::ram_bytes_used_for_cache_entry(removed.as_ref()),
        parent,
      );
    }
  }

  fn ram_bytes_used_for_cache_entry(cached: &CacheAndCountEnum) -> i64 {
    LEAF_CACHE_HASHTABLE_RAM_BYTES_PER_ENTRY
      .saturating_add(mem::size_of_val(cached) as i64)
      .saturating_add(cached.ram_bytes_used().unwrap_or(0))
  }
}
impl Accountable for LeafCache {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(self.ram_bytes_used.load(Ordering::SeqCst))
  }
}
pub struct CachingWrapperWeight<P, IRC> {
  in_: QueryWeight<IRC>,
  base: ConstantScoreWeight,
  policy: Arc<QueryCachingPolicyEnum>,
  used: AtomicBool,
  lru_cache: Arc<LRUQueryCache<P>>,
}
impl<P, IRC> CachingWrapperWeight<P, IRC>
where
  IRC: IndexReaderContext,
{
  pub(crate) fn new(
    in_: QueryWeight<IRC>,
    policy: Arc<QueryCachingPolicyEnum>,
    lru_cache: Arc<LRUQueryCache<P>>,
  ) -> Self {
    Self {
      in_,
      base: ConstantScoreWeight::new(1.0),
      policy,
      used: AtomicBool::new(false),
      lru_cache,
    }
  }
}

impl<P, IRC> CachingWrapperWeight<P, IRC>
where
  P: Predicate<TopParentMeta>,
  IRC: IndexReaderContext,
{
  fn should_cache(&self, context: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    let top_context = ReaderUtil::get_top_level_context(context);
    let max_doc = top_context.max_doc;
    let v = self.cache_entry_has_reasonable_worst_case_size(max_doc)
      && self.lru_cache.leaves_to_cache.test(top_context)?;
    Ok(v)
  }
  pub(crate) fn cache_entry_has_reasonable_worst_case_size(&self, max_doc: i32) -> bool {
    // The worst-case (dense) is a bit set which needs one bit per document
    let worst_case_ram_usage = (max_doc as i64) / 8;
    let total_ram_available = self.lru_cache.max_ram_bytes_used;
    // Imagine the worst-case that a cache entry is large than the size of
    // the cache: not only will this entry be trashed immediately but it
    // will also evict all current entries from the cache. For this reason
    // we only cache on an IndexReader if we have available room for
    // 5 different filters on this reader to avoid excessive trashing
    worst_case_ram_usage * 5 < total_ram_available
  }
}

impl<P, IRC> SegmentCacheable<IRC> for CachingWrapperWeight<P, IRC>
where
  P: Predicate<TopParentMeta>,
  IRC: IndexReaderContext,
{
  fn is_cacheable(&self, ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    self.in_.is_cacheable(ctx)
  }
}

impl<P, IRC> Weight<IRC> for CachingWrapperWeight<P, IRC>
where
  P: Predicate<TopParentMeta> + Send + Sync + 'static,
  IRC: IndexReaderContext,
{
  fn matches<'a>(
    &'a self,
    context: &'a LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &'a IndexSearcher<IRC>,
  ) -> Result<Option<crate::core::search::query::QueryWeightMatches<'a>>> {
    self.in_.matches(context, doc, searcher)
  }

  fn explain(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Explanation> {
    let scorer = self.scorer(context, searcher)?;
    self
      .base
      .explain(scorer, doc, self.get_query().to_string("")?)
  }
  fn get_query(&self) -> Arc<Query> {
    self.in_.get_query()
  }

  type ScorerSupplier = QueryWeightSs<IRC>;

  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::ScorerSupplier>> {
    if self
      .used
      .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
      .is_ok()
    {
      self.policy.on_use(self.get_query().as_ref());
    }

    if !self.in_.is_cacheable(context)? {
      return self.in_.scorer_supplier(context, searcher);
    }

    if !self.should_cache(context)? {
      return self.in_.scorer_supplier(context, searcher);
    }
    let reader = context.reader();
    let Some(cache_helper) = reader.get_core_cache_helper()? else {
      return self.in_.scorer_supplier(context, searcher);
    };
    let cached = {
      let Some(inner_read) = self.lru_cache.inner.try_read() else {
        return self.in_.scorer_supplier(context, searcher);
      };
      self
        .lru_cache
        .get(self.get_query().as_ref(), &cache_helper, &inner_read)
    };
    match cached {
      None => {
        let query = self.get_query();
        if self.policy.should_cache(query.as_ref())? {
          let Some(mut supplier) = self.in_.scorer_supplier(context, searcher)? else {
            self.lru_cache.put_if_absent(
              query,
              CacheAndCountEnum::Empty(CacheAndCount::empty()),
              &cache_helper,
            )?;
            return Ok(None);
          };
          let cost = supplier.cost(context, searcher)?;
          let max_doc = reader.max_doc()?;
          let ss = ScorerSupplierImpl1::new(
            cost,
            self.lru_cache.skip_cache_factor,
            supplier,
            max_doc,
            self.lru_cache.clone(),
            query,
            cache_helper,
          )?;
          let s = Box::new(ss);
          return Ok(Some(s));
        }
        self.in_.scorer_supplier(context, searcher)
      },
      Some(cached) => {
        if matches!(&*cached, CacheAndCountEnum::Empty(_)) {
          return Ok(None);
        }
        let disi = cached.iterator()?;
        let s: QueryWeightSs<IRC> = Box::new(ScorerSupplierImpl2::new(disi)?);
        Ok(Some(s))
      },
    }
  }

  fn count(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<i32> {
    let reader = context.reader();

    if reader.has_deletions()? {
      return self.in_.count(context, searcher);
    }

    if self
      .used
      .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
      .is_ok()
    {
      self.policy.on_use(self.get_query().as_ref());
    }

    if !self.in_.is_cacheable(context)? {
      return self.in_.count(context, searcher);
    }

    if !self.should_cache(context)? {
      return self.in_.count(context, searcher);
    }

    let Some(cache_helper) = reader.get_core_cache_helper()? else {
      return self.in_.count(context, searcher);
    };

    let Some(inner_read) = self.lru_cache.inner.try_read() else {
      return self.in_.count(context, searcher);
    };

    let query = self.get_query();
    let cached = self
      .lru_cache
      .get(query.as_ref(), &cache_helper, &inner_read);

    if let Some(cached) = cached {
      return cached.count().try_convert();
    }

    self.in_.count(context, searcher)
  }

  fn is_cache_wrapper(&self) -> bool {
    true
  }

  fn into_inner_weight(self: Box<Self>) -> Option<QueryWeight<IRC>> {
    Some(self.in_)
  }
}
pub struct ScorerSupplierImpl1<C, P, IRC> {
  cost: i64,
  skip_cache_factor: f32,
  supplier: QueryWeightSs<IRC>,
  max_doc: i32,
  lru_query_cache: Arc<LRUQueryCache<P>>,
  query: Arc<Query>,
  cache_helper: C,
}
impl<C, P, IRC> ScorerSupplierImpl1<C, P, IRC>
where
  IRC: IndexReaderContext,
{
  pub(crate) fn new(
    cost: i64,
    skip_cache_factor: f32,
    supplier: QueryWeightSs<IRC>,
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
    })
  }
}
#[allow(clippy::upper_case_acronyms)]
pub type DISI = DocIdSetIteratorEnum2<EmptyDISI, CacheAndCountDISI>;
impl<C, P, IRC> ScorerSupplier<IRC> for ScorerSupplierImpl1<C, P, IRC>
where
  IRC: IndexReaderContext,
  C: CacheHelper,
  P: Predicate<TopParentMeta> + Send + Sync + 'static,
{
  type Scorer = QueryWeightSsScorer;
  type BulkScorer = QueryWeightSsBulkScorer;

  fn get(
    &mut self,
    lead_cost: i64,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Self::Scorer> {
    if (self.cost as f32 / self.skip_cache_factor) > lead_cost as f32 {
      let scorer = self.supplier.get(lead_cost, context, searcher)?;
      return Ok(scorer);
    };
    let mut bulk_scorer = match self.supplier.bulk_scorer(context, searcher)? {
      Some(bulk_scorer) => bulk_scorer,
      None => return Err(LuceneError::illegal_state("BulkScorer should not be None")),
    };
    let cached = cache_impl(&mut bulk_scorer, self.max_doc.try_convert()?)?;
    let disi = cached.iterator()?;
    self
      .lru_query_cache
      .put_if_absent(self.query.clone(), cached, &self.cache_helper)?;
    let disi = DISI::B(disi);
    Ok(Box::new(ConstantScoreScorer::from_disi(
      0.0,
      ScoreMode::CompleteNoScores,
      disi,
    )))
  }

  fn bulk_scorer(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::BulkScorer>> {
    Ok(Some(Box::new(self.default_bulk_scorer(context, searcher)?)))
  }

  fn cost(
    &mut self,
    _context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC>,
  ) -> Result<i64> {
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
impl<IRC> ScorerSupplier<IRC> for ScorerSupplierImpl2
where
  IRC: IndexReaderContext,
{
  type Scorer = QueryWeightSsScorer;
  type BulkScorer = QueryWeightSsBulkScorer;

  fn get(
    &mut self,
    _lead_cost: i64,
    _context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC>,
  ) -> Result<Self::Scorer> {
    Ok(Box::new(ConstantScoreScorer::from_disi(
      0.0,
      ScoreMode::CompleteNoScores,
      std::mem::take(&mut self.disi),
    )))
  }

  fn bulk_scorer(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::BulkScorer>> {
    Ok(Some(Box::new(self.default_bulk_scorer(context, searcher)?)))
  }

  fn cost(
    &mut self,
    _context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC>,
  ) -> Result<i64> {
    Ok(self.cost)
  }
}

/// Cache of doc ids with a count.
pub(crate) struct CacheAndCount<D> {
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

impl<D> CacheAndCount<D> {
  pub(crate) fn new(cache: D, count: usize) -> Self {
    Self { cache, count }
  }
}

impl<D> CacheAndCount<D>
where
  D: DocIdSet,
{
  pub(crate) fn iterator(&self) -> Result<D::DocIdSetIterator> {
    self.cache.iterator()
  }
}

impl<D> CacheAndCount<D> {
  pub(crate) fn count(&self) -> usize {
    self.count
  }
}
impl<D> Accountable for CacheAndCount<D>
where
  D: DocIdSet,
{
  fn ram_bytes_used(&self) -> Result<i64> {
    self.cache.ram_bytes_used()
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
  scorer.score(&mut collector, None::<&dyn Bits>, 0, NO_MORE_DOCS)?;
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
  fn collect(&mut self, doc: i32, _scorer: &mut dyn Scorable) -> Result<()> {
    self.count += 1;
    self.bit_set.set(doc.try_convert()?);
    Ok(())
  }
}

fn cache_into_roaring_doc_id_set<BS>(
  scorer: &mut BS,
  max_doc: usize,
) -> Result<CacheAndCount<RoaringDocIdSet>>
where
  BS: BulkScorer,
{
  let mut collector = RoaringCollectorImpl::new(max_doc);
  scorer.score(&mut collector, None::<&dyn Bits>, 0, NO_MORE_DOCS)?;
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
  fn collect(&mut self, doc: i32, _scorer: &mut dyn Scorable) -> Result<()> {
    self.builder.add(doc)?;
    Ok(())
  }
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
  pub(crate) fn iterator(&self) -> Result<CacheAndCountDISI> {
    match self {
      CacheAndCountEnum::BitSet(c) => Ok(DocIdSetIteratorEnum3::B(c.iterator()?)),
      CacheAndCountEnum::Roaring(c) => Ok(DocIdSetIteratorEnum3::C(c.iterator()?)),
      CacheAndCountEnum::Empty(c) => Ok(DocIdSetIteratorEnum3::A(c.iterator()?)),
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
