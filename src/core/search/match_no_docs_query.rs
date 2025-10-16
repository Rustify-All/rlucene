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
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::query_timeout::QueryTimeout;
use crate::core::index::term_states::TermStates;
use crate::core::search::QueryCache;
use crate::core::search::dummy::dummy_scorer_supplier::DummyScorerSupplier;
use crate::core::search::explanation::Explanation;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::matches_utils::MatchWithNoTerms;
use crate::core::search::query::{Query, QueryBase};
use crate::core::search::query_caching_policy::QueryCachingPolicy;
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::weight::Weight;
use crate::core::util::error::lucene_error::Result;
use std::marker::PhantomData;
use std::sync::Arc;

/// A query that matches no documents.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct MatchNoDocsQuery {
    reason: String,
}

impl Default for MatchNoDocsQuery {
    fn default() -> Self {
        Self::new()
    }
}

impl MatchNoDocsQuery {
    /// Default constructor
    pub fn new() -> Self {
        Self {
            reason: String::new(),
        }
    }

    /// Provides a reason explaining why this query was used
    pub fn with_reason(reason: String) -> Self {
        Self { reason }
    }
}

impl QueryBase for MatchNoDocsQuery {
    fn as_string(&self, _field: &str) -> String {
        format!("MatchNoDocsQuery(\"{}\")", self.reason)
    }

    type Weight<S, IRC>
        = MatchNoDocsWeight<IRC::LeafReader>
    where
        S: Similarity,
        IRC: IndexReaderContext;

    fn create_weight<S, IRC, QT, QCP, QC>(
        self,
        _searcher: &IndexSearcher<IRC, S, QT, QCP, QC>,
        _score_mode: &ScoreMode,
        _boost: f32,
        _per_reader_term_state: Option<TermStates<IRCTermState<IRC>>>,
    ) -> Result<Self::Weight<S, IRC>>
    where
        IRC: IndexReaderContext,
        S: Similarity,
        QT: QueryTimeout,
        QCP: QueryCachingPolicy,
        QC: QueryCache,
        Self: Sized,
    {
        Ok(MatchNoDocsWeight::new(self))
    }

    type RewriteQuery = MatchNoDocsQuery;

    fn visit<QV>(&self, _visitor: &QV)
    where
        QV: QueryVisitor,
    {
        todo!()
    }
}

pub struct MatchNoDocsWeight<LR>
where
    LR: LeafReader,
{
    parent_query: Arc<Query>,
    _leaf_reader: PhantomData<LR>,
}

impl<LR> MatchNoDocsWeight<LR>
where
    LR: LeafReader,
{
    pub fn new(query: MatchNoDocsQuery) -> Self {
        Self {
            parent_query: Arc::new(query.into()),
            _leaf_reader: PhantomData,
        }
    }
}

impl<LR> SegmentCacheable<LR> for MatchNoDocsWeight<LR>
where
    LR: LeafReader,
{
    fn is_cacheable(&self, _ctx: &LeafReaderContext<LR>) -> bool {
        true
    }
}

impl<LR> Weight<LR> for MatchNoDocsWeight<LR>
where
    LR: LeafReader,
{
    type Matches = MatchWithNoTerms;

    fn matches(
        &self,
        _context: &LeafReaderContext<LR>,
        _doc: i32,
    ) -> Result<Option<Self::Matches>> {
        Ok(None)
    }

    fn explain(&self, _context: &LeafReaderContext<LR>, _doc: i32) -> Result<Explanation> {
        let Query::MatchNoDoc(parent_query) = self.parent_query.as_ref() else {
            unreachable!("should never happen");
        };
        Ok(Explanation::no_match(parent_query.reason.clone(), vec![]))
    }

    fn get_query(&self) -> Arc<Query> {
        self.parent_query.clone()
    }

    type ScorerSupplier = DummyScorerSupplier;

    fn scorer_supplier(
        &self,
        _context: &LeafReaderContext<LR>,
    ) -> Result<Option<Self::ScorerSupplier>> {
        Ok(None)
    }

    fn count(&self, _context: &LeafReaderContext<LR>) -> Result<i32> {
        Ok(0)
    }
}

impl<LR> std::fmt::Debug for MatchNoDocsWeight<LR>
where
    LR: LeafReader,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "weight({:?})", self.parent_query)
    }
}
