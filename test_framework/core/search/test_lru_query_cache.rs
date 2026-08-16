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
use crate::core::index::index_reader::{
  CacheKey, IndexReader, IndexReaderContextKind, IndexReaderContextType,
};
use crate::core::index::leaf_reader_context::TopParentMeta;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::lru_query_cache::{
  Inner, LRUQueryCache, LRUQueryCacheBase, LRUQueryCacheDefaults,
};
use crate::core::search::query::Query;
use crate::core::search::query_cache::QueryCacheEnum;
use crate::core::search::query_caching_policy::QueryCachingPolicyEnum;
use crate::core::search::searcher_factory::{SearcherFactoryBase, SearcherFactoryDefaults};
use crate::core::util::error::lucene_error::Result;
use crate::core::util::predicate::Predicate;
use parking_lot::{Mutex, RwLockWriteGuard};
use rand::RngExt;
use rand::prelude::StdRng;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

#[allow(dead_code)] // for quick search
struct TestLRUQueryCache;

pub struct RandomSegmentSkippingPredicate {
  random: Mutex<StdRng>,
}

impl RandomSegmentSkippingPredicate {
  pub fn new(random: StdRng) -> Self {
    Self {
      random: Mutex::new(random),
    }
  }
}

impl Predicate<TopParentMeta> for RandomSegmentSkippingPredicate {
  fn test(&self, _context: &TopParentMeta) -> Result<bool> {
    Ok(self.random.lock().random_bool(0.5))
  }
}

pub struct CachingSearcherFactory<P> {
  query_cache: Arc<LRUQueryCache<P>>,
  query_caching_policy: Arc<QueryCachingPolicyEnum>,
}

impl<P> CachingSearcherFactory<P>
where
  P: Predicate<TopParentMeta>,
{
  pub fn new(
    query_cache: Arc<LRUQueryCache<P>>,
    query_caching_policy: Arc<QueryCachingPolicyEnum>,
  ) -> Self {
    Self {
      query_cache,
      query_caching_policy,
    }
  }
}

impl<IR, P> SearcherFactoryBase<IR> for CachingSearcherFactory<P>
where
  IR: IndexReader + 'static,
  IR::ContextKind: IndexReaderContextKind<Arc<IR>>,
  IndexReaderContextType<Arc<IR>>: Sync + 'static,
  P: Predicate<TopParentMeta> + Send + Sync + 'static,
{
  fn new_searcher(
    &self,
    reader: Arc<IR>,
    previous_reader: Option<&Arc<IR>>,
  ) -> Result<IndexSearcher<IndexReaderContextType<Arc<IR>>>> {
    let mut searcher = SearcherFactoryDefaults::new_searcher(reader, previous_reader)?;
    searcher.set_query_caching_policy(self.query_caching_policy.clone());
    searcher.set_query_cache(Some(QueryCacheEnum::custom(self.query_cache.clone())));
    Ok(searcher)
  }
}

#[derive(Clone)]
pub struct FineGrainedStatsLRUQueryCache {
  index_id: Arc<HashMap<CacheKey, i32>>,
  hit_count_1: Arc<AtomicI64>,
  hit_count_2: Arc<AtomicI64>,
  miss_count_1: Arc<AtomicI64>,
  miss_count_2: Arc<AtomicI64>,
  ram_bytes_usage: Arc<AtomicI64>,
  cache_size: Arc<AtomicI64>,
}

impl FineGrainedStatsLRUQueryCache {
  pub fn new(index_id: HashMap<CacheKey, i32>) -> Self {
    Self {
      index_id: Arc::new(index_id),
      hit_count_1: Arc::new(AtomicI64::new(0)),
      hit_count_2: Arc::new(AtomicI64::new(0)),
      miss_count_1: Arc::new(AtomicI64::new(0)),
      miss_count_2: Arc::new(AtomicI64::new(0)),
      ram_bytes_usage: Arc::new(AtomicI64::new(0)),
      cache_size: Arc::new(AtomicI64::new(0)),
    }
  }

  pub fn hit_count_1(&self) -> i64 {
    self.hit_count_1.load(Ordering::SeqCst)
  }

  pub fn hit_count_2(&self) -> i64 {
    self.hit_count_2.load(Ordering::SeqCst)
  }

  pub fn miss_count_1(&self) -> i64 {
    self.miss_count_1.load(Ordering::SeqCst)
  }

  pub fn miss_count_2(&self) -> i64 {
    self.miss_count_2.load(Ordering::SeqCst)
  }

  pub fn ram_bytes_usage(&self) -> i64 {
    self.ram_bytes_usage.load(Ordering::SeqCst)
  }

  pub fn cache_size(&self) -> i64 {
    self.cache_size.load(Ordering::SeqCst)
  }
}

impl<P> LRUQueryCacheBase<P> for FineGrainedStatsLRUQueryCache
where
  P: Predicate<TopParentMeta>,
{
  fn on_hit(&self, cache: &LRUQueryCache<P>, reader_core_key: &CacheKey, query: &Query) {
    LRUQueryCacheDefaults::on_hit(cache, reader_core_key, query);
    match self.index_id.get(reader_core_key) {
      Some(1) => {
        self.hit_count_1.fetch_add(1, Ordering::SeqCst);
      },
      Some(2) => {
        self.hit_count_2.fetch_add(1, Ordering::SeqCst);
      },
      _ => panic!("reader core key does not belong to either test index"),
    }
  }

  fn on_miss(&self, cache: &LRUQueryCache<P>, reader_core_key: &CacheKey, query: &Query) {
    LRUQueryCacheDefaults::on_miss(cache, reader_core_key, query);
    match self.index_id.get(reader_core_key) {
      Some(1) => {
        self.miss_count_1.fetch_add(1, Ordering::SeqCst);
      },
      Some(2) => {
        self.miss_count_2.fetch_add(1, Ordering::SeqCst);
      },
      _ => panic!("reader core key does not belong to either test index"),
    }
  }

  fn on_query_cache(
    &self,
    cache: &LRUQueryCache<P>,
    query: &Query,
    ram_bytes_used: i64,
    guard: &RwLockWriteGuard<'_, Inner>,
  ) {
    LRUQueryCacheDefaults::on_query_cache(cache, query, ram_bytes_used, guard);
    self
      .ram_bytes_usage
      .fetch_add(ram_bytes_used, Ordering::SeqCst);
  }

  fn on_query_eviction(
    &self,
    cache: &LRUQueryCache<P>,
    query: &Query,
    ram_bytes_used: i64,
    guard: &RwLockWriteGuard<'_, Inner>,
  ) {
    LRUQueryCacheDefaults::on_query_eviction(cache, query, ram_bytes_used, guard);
    self
      .ram_bytes_usage
      .fetch_sub(ram_bytes_used, Ordering::SeqCst);
  }

  fn on_doc_id_set_cache(
    &self,
    cache: &LRUQueryCache<P>,
    reader_core_key: &CacheKey,
    ram_bytes_used: i64,
  ) {
    LRUQueryCacheDefaults::on_doc_id_set_cache(cache, reader_core_key, ram_bytes_used);
    self
      .ram_bytes_usage
      .fetch_add(ram_bytes_used, Ordering::SeqCst);
    self.cache_size.fetch_add(1, Ordering::SeqCst);
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
    self
      .ram_bytes_usage
      .fetch_sub(sum_ram_bytes_used, Ordering::SeqCst);
    self.cache_size.fetch_sub(num_entries, Ordering::SeqCst);
  }

  fn on_clear(&self, cache: &LRUQueryCache<P>, guard: &RwLockWriteGuard<'_, Inner>) {
    LRUQueryCacheDefaults::on_clear(cache, guard);
    self.ram_bytes_usage.store(0, Ordering::SeqCst);
    self.cache_size.store(0, Ordering::SeqCst);
  }
}

#[derive(Clone, Default)]
pub struct EvictEmptySegmentCacheLRUQueryCache;

impl<P> LRUQueryCacheBase<P> for EvictEmptySegmentCacheLRUQueryCache
where
  P: Predicate<TopParentMeta>,
{
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
    assert!(num_entries > 0);
  }
}
