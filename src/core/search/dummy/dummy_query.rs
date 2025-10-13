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
use crate::core::index::index_reader_context::{IRCTermState, IndexReaderContext};
use crate::core::index::query_timeout::QueryTimeout;
use crate::core::index::term_states::TermStates;
use crate::core::search::dummy::dummy_weight::DummyWeight;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::query::Query;
use crate::core::search::query_caching_policy::QueryCachingPolicy;
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::similarities_impl::similarities::Similarity;
use std::fmt::Display;

#[derive(Eq, Hash, PartialEq, Debug)]
pub struct DummyQuery {}
impl Query for DummyQuery {
    fn as_string(&self, _field: &str) -> String {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    type Weight<S, IRC>
        = DummyWeight
    where
        S: Similarity,
        IRC: IndexReaderContext;

    fn create_weight<S, IRC, QT, QCP>(
        self,
        _search: &IndexSearcher<IRC, S, QT, QCP>,
        _score_mod: &ScoreMode,
        _boost: f32,
        _per_reader_term_state: Option<TermStates<IRCTermState<IRC>>>,
    ) -> crate::core::util::error::lucene_error::Result<Self::Weight<S, IRC>>
    where
        IRC: IndexReaderContext,
        S: Similarity,
        QT: QueryTimeout,
        QCP: QueryCachingPolicy,
        Self: Sized,
    {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    type RewriteQuery = DummyQuery;

    fn rewrite<IRC, S, QT, QCP>(
        &self,
        _searcher: &IndexSearcher<IRC, S, QT, QCP>,
    ) -> crate::core::util::error::lucene_error::Result<Option<Self::RewriteQuery>>
    where
        IRC: IndexReaderContext,
        S: Similarity,
        QT: QueryTimeout,
        QCP: QueryCachingPolicy,
    {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn visit<QV>(&self, _visitor: &QV)
    where
        QV: QueryVisitor,
    {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}
impl Display for DummyQuery {
    fn fmt(&self, _f: &mut std::fmt::Formatter) -> std::fmt::Result {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}
