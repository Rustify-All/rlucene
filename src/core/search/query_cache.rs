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
use crate::core::index::leaf_reader::LeafReader;
use crate::core::search::query_caching_policy::QueryCachingPolicyEnum;
use crate::core::search::weight::BoxWeight;
use std::sync::Arc;

/// A cache for queries.
pub trait QueryCache {
    /// Return a wrapper around the provided `weight` that will cache matching documents
    /// per-segment according to the given `policy`.
    /// **Note:** The returned weight will only be equivalent if scores are not needed.
    ///
    /// See also [`Collector::score_mode`](crate::core::search::collector::Collector::score_mode).
    fn do_cache<LR: LeafReader>(
        &self,
        weight: BoxWeight<LR>,
        policy: Arc<QueryCachingPolicyEnum>,
    ) -> BoxWeight<LR>;
}
