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
use crate::core::search::query_caching_policy::QueryCachingPolicy;

pub struct DummyQueryCachingPolicy;
impl QueryCachingPolicy for DummyQueryCachingPolicy {
    fn on_use<Q>(&self, _query: &Q)
    where
        Q: QueryBase,
    {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn should_cache<Q>(&self, _query: &Q) -> crate::core::util::error::lucene_error::Result<bool>
    where
        Q: QueryBase,
    {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}
