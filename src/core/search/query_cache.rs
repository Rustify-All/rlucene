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
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::search::lru_query_cache::{LRUQueryCache, MinSegmentSizePredicate};
use crate::core::search::query::QueryWeight;
use crate::core::search::query_caching_policy::QueryCachingPolicyEnum;
use crate::core::util::error::lucene_error::Result;
use std::sync::Arc;

/// A cache for queries.
pub trait QueryCache<IRC>
where
  IRC: IndexReaderContext,
{
  /// Return a wrapper around the provided `weight` that will cache matching documents
  /// per-segment according to the given `policy`.
  /// **Note:** The returned weight will only be equivalent if scores are not needed.
  ///
  /// See also [`Collector::score_mode`](crate::core::search::collector::Collector::score_mode).
  fn do_cache(
    &self,
    weight: QueryWeight<IRC>,
    policy: Arc<QueryCachingPolicyEnum>,
  ) -> Result<QueryWeight<IRC>>
  where
    IRC: IndexReaderContext + 'static;
}
pub type BoxQueryCache<IRC> = Box<dyn QueryCache<IRC> + Send + Sync>;
pub enum QueryCacheEnum<IRC> {
  Lru(Arc<LRUQueryCache<MinSegmentSizePredicate>>),
  Custom(BoxQueryCache<IRC>),
}
impl<IRC> QueryCacheEnum<IRC>
where
  IRC: IndexReaderContext + 'static,
{
  pub fn custom<QC>(cache: QC) -> Self
  where
    QC: QueryCache<IRC> + Send + Sync + 'static,
  {
    Self::Custom(Box::new(cache))
  }
}
impl<IRC> QueryCache<IRC> for QueryCacheEnum<IRC>
where
  IRC: IndexReaderContext + 'static,
{
  fn do_cache(
    &self,
    weight: QueryWeight<IRC>,
    policy: Arc<QueryCachingPolicyEnum>,
  ) -> Result<QueryWeight<IRC>>
  where
    IRC: IndexReaderContext,
  {
    match self {
      QueryCacheEnum::Lru(cache) => cache.do_cache(weight, policy),
      QueryCacheEnum::Custom(cache) => cache.do_cache(weight, policy),
    }
  }
}

impl<IRC> From<Arc<LRUQueryCache<MinSegmentSizePredicate>>> for QueryCacheEnum<IRC>
where
  IRC: IndexReaderContext,
{
  fn from(v: Arc<LRUQueryCache<MinSegmentSizePredicate>>) -> Self {
    QueryCacheEnum::Lru(v)
  }
}
