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
/// A [`Query`] wrapper that allows giving a boost to the wrapped query.
///
/// Boost values that are less than one will give less importance to this query
/// compared to other ones, while values that are greater than one will give
/// more importance to the scores returned by this query.
///
///
/// More complex boosts can be applied by using `FunctionScoreQuery` in the
use crate::core::index::index_reader_context::{IRCTermState, IndexReaderContext};
use crate::core::index::query_timeout::QueryTimeout;
use crate::core::index::term_states::TermStates;
use crate::core::search::QueryCache;
use crate::core::search::dummy::dummy_query::DummyQuery;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::query::{Query, QueryBase, QueryWeight};
use crate::core::search::query_caching_policy::QueryCachingPolicy;
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::hash::{Hash, Hasher};

#[derive(Debug)]
pub struct BoostQuery {
    query: Box<Query>,
    boost: f32,
}
impl BoostQuery {
    pub fn new(query: Query, boost: f32) -> Result<Self> {
        if !boost.is_finite() || boost < 0.0 {
            return Err(LuceneError::illegal_argument(format!(
                "boost must be a positive float, got {}",
                boost
            )));
        }
        Ok(Self {
            query: Box::new(query),
            boost,
        })
    }
    pub fn get_query(&self) -> &Query {
        &self.query
    }
    pub fn get_boost(&self) -> f32 {
        self.boost
    }
}
#[cfg(test)]
impl Clone for BoostQuery {
    fn clone(&self) -> Self {
        Self {
            query: Box::new((*self.query).clone()),
            boost: self.boost,
        }
    }
}
impl PartialEq for BoostQuery {
    fn eq(&self, other: &Self) -> bool {
        self.boost.to_bits() == other.boost.to_bits() && self.query == other.query
    }
}
impl Eq for BoostQuery {}
impl QueryBase for BoostQuery {
    fn as_string(&self, field: &str) -> String {
        let inner = self.query.as_string(field);
        let mut s = String::new();
        s.push('(');
        s.push_str(&inner);
        s.push(')');
        s.push('^');
        s.push_str(&self.boost.to_string());
        s
    }

    type Weight<S, IRC, QCP, QC>
        = QueryWeight<S, IRC, QCP, QC>
    where
        S: Similarity,
        IRC: IndexReaderContext,
        QCP: QueryCachingPolicy,
        QC: QueryCache;
    fn create_weight<S, IRC, QT, QCP, QC>(
        self,
        searcher: &IndexSearcher<IRC, S, QT, QCP, QC>,
        score_mode: &ScoreMode,
        boost: f32,
        per_reader_term_state: Option<TermStates<IRCTermState<IRC>>>,
    ) -> Result<Self::Weight<S, IRC, QCP, QC>>
    where
        IRC: IndexReaderContext,
        S: Similarity,
        QT: QueryTimeout,
        QCP: QueryCachingPolicy,
        QC: QueryCache,
        Self: Sized,
    {
        self.query.create_weight(
            searcher,
            score_mode,
            self.boost * boost,
            per_reader_term_state,
        )
    }

    type RewriteQuery = DummyQuery;

    fn rewrite<IRC, S, QT, QCP, QC>(
        &self,
        _searcher: &IndexSearcher<IRC, S, QT, QCP, QC>,
    ) -> Result<Option<Self::RewriteQuery>>
    where
        IRC: IndexReaderContext,
        S: Similarity,
        QT: QueryTimeout,
        QCP: QueryCachingPolicy,
        QC: QueryCache,
    {
        todo!()
    }

    fn visit<QV>(&self, _visitor: &QV)
    where
        QV: QueryVisitor,
    {
        todo!()
    }
}

impl Hash for BoostQuery {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.query.hash(state);
        self.boost.to_bits().hash(state);
    }
}
