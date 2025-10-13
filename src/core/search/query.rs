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
use crate::core::search::dummy::dummy_query::DummyQuery;
use crate::core::search::dummy::dummy_weight::DummyWeight;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::query_caching_policy::QueryCachingPolicy;
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::term_query::TermQuery;
use crate::core::search::weight::Weight;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::cmp::PartialEq;
use std::fmt::{Debug, Display, Formatter};
use std::hash::{Hash, Hasher};

pub trait Query: Eq + Hash + Display + Debug {
    fn as_string(&self, field: &str) -> String;
    type Weight<S, IRC>: Weight<IRC::LeafReader>
    where
        S: Similarity,
        IRC: IndexReaderContext;
    fn create_weight<S, IRC, QT, QCP>(
        self,
        _search: &IndexSearcher<IRC, S, QT, QCP>,
        _score_mod: &ScoreMode,
        _boost: f32,
        _per_reader_term_state: Option<TermStates<IRCTermState<IRC>>>,
    ) -> Result<Self::Weight<S, IRC>>
    where
        IRC: IndexReaderContext,
        S: Similarity,
        QT: QueryTimeout,
        QCP: QueryCachingPolicy,
        Self: Sized,
    {
        Err(LuceneError::unsupported_operation(format!(
            "Query {} does not implement create_weight",
            std::any::type_name::<Self>()
        )))
    }
    type RewriteQuery: Query;
    fn rewrite<IRC, S, QT, QCP>(
        &self,
        _searcher: &IndexSearcher<IRC, S, QT, QCP>,
    ) -> Result<Option<Self::RewriteQuery>>
    where
        IRC: IndexReaderContext,
        S: Similarity,
        QT: QueryTimeout,
        QCP: QueryCachingPolicy,
    {
        Ok(None)
    }
    fn visit<QV>(&self, visitor: &QV)
    where
        QV: QueryVisitor;
}

pub enum QueryEnum {
    Term(TermQuery),
}

impl Eq for QueryEnum {}

impl PartialEq<QueryEnum> for TermQuery {
    fn eq(&self, other: &QueryEnum) -> bool {
        match other {
            QueryEnum::Term(t) => self == t,
        }
    }
}

impl PartialEq<Self> for QueryEnum {
    fn eq(&self, other: &Self) -> bool {
        match self {
            QueryEnum::Term(t) => t == other,
        }
    }
}

impl Hash for QueryEnum {
    fn hash<H: Hasher>(&self, _state: &mut H) {
        match self {
            QueryEnum::Term(t) => {
                t.hash(_state);
            },
        }
    }
}

impl Display for QueryEnum {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            QueryEnum::Term(t) => {
                write!(f, "{}", t)
            },
        }
    }
}

impl Debug for QueryEnum {
    fn fmt(&self, _f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            QueryEnum::Term(t) => {
                write!(_f, "QueryEnum::Term({:?})", t)
            },
        }
    }
}

impl Query for QueryEnum {
    fn as_string(&self, field: &str) -> String {
        match self {
            QueryEnum::Term(t) => t.as_string(field),
        }
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
    ) -> Result<Self::Weight<S, IRC>>
    where
        IRC: IndexReaderContext,
        S: Similarity,
        QT: QueryTimeout,
        QCP: QueryCachingPolicy,
        Self: Sized,
    {
        todo!()
    }

    type RewriteQuery = DummyQuery;

    fn rewrite<IRC, S, QT, QCP>(
        &self,
        _searcher: &IndexSearcher<IRC, S, QT, QCP>,
    ) -> Result<Option<Self::RewriteQuery>>
    where
        IRC: IndexReaderContext,
        S: Similarity,
        QT: QueryTimeout,
        QCP: QueryCachingPolicy,
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

impl From<TermQuery> for QueryEnum {
    fn from(value: TermQuery) -> Self {
        QueryEnum::Term(value)
    }
}
