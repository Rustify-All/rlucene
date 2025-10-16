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
use crate::core::search::query::QueryBase;
use crate::core::util::error::lucene_error::Result;

/// A policy defining which filters should be cached.
///
/// Implementations of this trait must be thread-safe.
///
/// See also: [`UsageTrackingQueryCachingPolicy`](crate::core::search::usage_tracking_query_caching_policy::UsageTrackingQueryCachingPolicy), [`LRUQueryCache`](crate::core::search::lru_query_cache::LRUQueryCache).

// TODO: add APIs for integration with `IndexWriter::IndexReaderWarmer`
pub trait QueryCachingPolicy {
    /// Callback that is called every time that a cached filter is used.
    /// This is typically useful if the policy wants to track usage statistics
    /// in order to make decisions.
    fn on_use<Q>(&self, query: &Q)
    where
        Q: QueryBase;

    /// Whether the given [`Query`] is worth caching.
    ///
    /// This method will be called by the [`QueryCache`](crate::core::search::query_cache:QueryCache) to know whether to cache.
    /// It will first attempt to load a [`DocIdSet`](crate::core::search::doc_id_set::DocIdSet) from the cache. If it is not cached yet
    /// and this method returns `true` then a cache entry will be generated.
    /// Otherwise an uncached scorer will be returned.
    fn should_cache<Q>(&self, query: &Q) -> Result<bool>
    where
        Q: QueryBase;
}
