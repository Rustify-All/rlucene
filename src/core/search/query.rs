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
use crate::core::document::sorted_numeric_doc_values_range_query::{
    SortedNumericDocValuesRangeQuery, SortedNumericDocValuesRangeQueryWeight,
    SortedNumericDocValuesRangeSs,
};
use crate::core::document::sorted_numeric_doc_values_set_query::{
    DefaultScorerSupplierSs, SortedNumericDocValuesSetQuery, SortedNumericDocValuesSetQueryWeight,
};
use crate::core::document::sorted_set_doc_values_range_query::{
    SortedSetDocValuesRangeQuery, SortedSetDocValuesRangeQueryWeight, SortedSetDocValuesRangeSs,
    SortedSetDocValuesRangeSsScorerDisi,
};
use crate::core::index::index_reader_context::{IRCTermState, IndexReaderContext};
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::query_timeout::QueryTimeout;
use crate::core::index::term_states::TermStates;
use crate::core::search::QueryCache;
use crate::core::search::boolean_query::BooleanQuery;
use crate::core::search::boost_query::BoostQuery;
use crate::core::search::bulk_scorer::BulkScorer;
use crate::core::search::constant_score_query::{
    ConstantScoreQuery, ConstantScoreQueryWeight, ConstantScoreSs, ConstantScoreSsBulkScorer,
    ConstantScoreSsScorer, ConstantScoreSsScorerDisi, ConstantScoreSsScorerTpi,
};
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, DocIdSetIteratorEnum10};
use crate::core::search::dummy::dummy_matches::DummyMatches;
use crate::core::search::dummy::dummy_query::DummyQuery;
use crate::core::search::dummy::dummy_scorable::DummyScorable;
use crate::core::search::dummy::dummy_scorer_supplier::DummyScorerSupplier;
use crate::core::search::explanation::Explanation;
use crate::core::search::field_exists_query::{FieldExistsQuery, FieldExistsSs, FieldExistsWeight};
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::index_sort_sorted_numeric_doc_values_range_query::{
    ISSNDVRSsScorerDisi, IndexSortSortedNumericDocValuesRangeQuery,
    IndexSortSortedNumericDocValuesRangeQueryWeight, IndexSortSortedNumericDocValuesRangeSs,
};
use crate::core::search::leaf_collector::LeafCollector;
use crate::core::search::match_all_docs_query::{MatchAllDocsQuery, MatchAllSs, MatchAllWeight};
use crate::core::search::match_no_docs_query::{
    MatchNoDocsQuery, MatchNoDocsSs, MatchNoDocsWeight,
};
use crate::core::search::matches_utils::MatchWithNoTerms;
use crate::core::search::point_range_query::{PointRangeQuery, PointRangeSs, PointRangeWeight};
use crate::core::search::query_caching_policy::QueryCachingPolicy;
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::scorable::{ChildScorable, Scorable};
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::{Scorer, TwoPhaseState};
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::term_query::{TermQuery, TermSs, TermWeight};
use crate::core::search::two_phase_iterator::{TwoPhaseIterator, TwoPhaseIteratorEnum10};
use crate::core::search::weight::Weight;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::cmp::PartialEq;
use std::fmt::{Debug, Formatter};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

pub type QueryBaseWeight<Q, S, IRC, QCP, QC> = <Q as QueryBase>::Weight<S, IRC, QCP, QC>;
pub trait QueryBase: Eq + Hash + Debug {
    fn as_string(&self, field: &str) -> String;
    type Weight<S, IRC, QCP, QC>: Weight<IRC::LeafReader>
    where
        S: Similarity,
        IRC: IndexReaderContext,
        QCP: QueryCachingPolicy,
        QC: QueryCache;
    fn create_weight<S, IRC, QT, QCP, QC>(
        self,
        _searcher: &IndexSearcher<IRC, S, QT, QCP, QC>,
        _score_mode: &ScoreMode,
        _boost: f32,
        _per_reader_term_state: Option<TermStates<IRCTermState<IRC>>>,
    ) -> Result<Self::Weight<S, IRC, QCP, QC>>
    where
        IRC: IndexReaderContext,
        S: Similarity,
        QT: QueryTimeout,
        QCP: QueryCachingPolicy,
        QC: QueryCache,
        Self: Sized,
    {
        Err(LuceneError::unsupported_operation(format!(
            "Query {} does not implement create_weight",
            std::any::type_name::<Self>()
        )))
    }
    type RewriteQuery: QueryBase;
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
        Ok(None)
    }
    fn visit<QV>(&self, visitor: &QV)
    where
        QV: QueryVisitor;
}
pub enum Query {
    Term(TermQuery),
    MatchAll(MatchAllDocsQuery),
    MatchNoDoc(MatchNoDocsQuery),
    Dummy(DummyQuery),
    Boost(BoostQuery),
    ConstantScore(ConstantScoreQuery),
    PointRange(PointRangeQuery),
    SortedNumericDocValuesSet(SortedNumericDocValuesSetQuery),
    SortedNumericDocValuesRange(SortedNumericDocValuesRangeQuery),
    SortedSetDocValuesRange(SortedSetDocValuesRangeQuery),
    IndexSortSortedNumericDocValuesRange(IndexSortSortedNumericDocValuesRangeQuery),
    FieldExists(FieldExistsQuery),
    Boolean(BooleanQuery),
}
#[cfg(test)]
impl Clone for Query {
    fn clone(&self) -> Self {
        match self {
            Query::Term(t) => Query::Term(t.clone()),
            Query::MatchAll(m) => Query::MatchAll(m.clone()),
            Query::MatchNoDoc(m) => Query::MatchNoDoc(m.clone()),
            Query::Dummy(d) => Query::Dummy(d.clone()),
            Query::Boost(b) => Query::Boost(b.clone()),
            Query::ConstantScore(c) => Query::ConstantScore(c.clone()),
            Query::PointRange(c) => Query::PointRange(c.clone()),
            Query::SortedNumericDocValuesSet(c) => Query::SortedNumericDocValuesSet(c.clone()),
            Query::SortedNumericDocValuesRange(c) => Query::SortedNumericDocValuesRange(c.clone()),
            Query::SortedSetDocValuesRange(c) => Query::SortedSetDocValuesRange(c.clone()),
            Query::IndexSortSortedNumericDocValuesRange(c) => {
                Query::IndexSortSortedNumericDocValuesRange(c.clone())
            },
            Query::FieldExists(c) => Query::FieldExists(c.clone()),
            Query::Boolean(c) => Query::Boolean(c.clone()),
        }
    }
}
impl Default for Query {
    fn default() -> Self {
        Query::Dummy(DummyQuery::default())
    }
}

impl Eq for Query {}

impl PartialEq for Query {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Query::Term(t1), Query::Term(t2)) => t1 == t2,
            (Query::MatchAll(m1), Query::MatchAll(m2)) => m1 == m2,
            (Query::MatchNoDoc(m1), Query::MatchNoDoc(m2)) => m1 == m2,
            (Query::Dummy(d1), Query::Dummy(d2)) => d1 == d2,
            (Query::Boost(b1), Query::Boost(b2)) => b1 == b2,
            (Query::ConstantScore(c1), Query::ConstantScore(c2)) => c1 == c2,
            (Query::PointRange(c1), Query::PointRange(c2)) => c1 == c2,
            (Query::SortedNumericDocValuesSet(c1), Query::SortedNumericDocValuesSet(c2)) => {
                c1 == c2
            },
            (Query::SortedNumericDocValuesRange(c1), Query::SortedNumericDocValuesRange(c2)) => {
                c1 == c2
            },
            (
                Query::IndexSortSortedNumericDocValuesRange(c1),
                Query::IndexSortSortedNumericDocValuesRange(c2),
            ) => c1 == c2,
            (Query::FieldExists(c1), Query::FieldExists(c2)) => c1 == c2,
            (Query::Boolean(c1), Query::Boolean(c2)) => c1 == c2,
            _ => false,
        }
    }
}

impl Hash for Query {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Query::Term(t) => {
                t.hash(state);
            },
            Query::MatchAll(m) => {
                m.hash(state);
            },
            Query::MatchNoDoc(m) => {
                m.hash(state);
            },
            Query::Dummy(d) => {
                d.hash(state);
            },
            Query::Boost(b) => {
                b.hash(state);
            },
            Query::ConstantScore(c) => {
                c.hash(state);
            },
            Query::PointRange(c) => {
                c.hash(state);
            },
            Query::SortedNumericDocValuesSet(c) => {
                c.hash(state);
            },
            Query::SortedNumericDocValuesRange(c) => {
                c.hash(state);
            },
            Query::SortedSetDocValuesRange(c) => {
                c.hash(state);
            },
            Query::IndexSortSortedNumericDocValuesRange(c) => {
                c.hash(state);
            },
            Query::FieldExists(c) => {
                c.hash(state);
            },
            Query::Boolean(c) => {
                c.hash(state);
            },
        }
    }
}
impl Debug for Query {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Query::Term(t) => {
                write!(f, "Query::Term({:?})", t)
            },
            Query::MatchAll(m) => {
                write!(f, "Query::MatchAll({:?})", m)
            },
            Query::MatchNoDoc(m) => {
                write!(f, "Query::MatchNoDoc({:?})", m)
            },
            Query::Dummy(d) => {
                write!(f, "Query::Dummy({:?})", d)
            },
            Query::Boost(b) => {
                write!(f, "Query::Boost({:?})", b)
            },
            Query::ConstantScore(c) => {
                write!(f, "Query::ConstantScore({:?})", c)
            },
            Query::PointRange(c) => {
                write!(f, "Query::PointRange({:?})", c)
            },
            Query::SortedNumericDocValuesSet(c) => {
                write!(f, "Query::SortedNumericDocValuesSet({:?})", c)
            },
            Query::SortedNumericDocValuesRange(c) => {
                write!(f, "Query::SortedNumericDocValuesRange({:?})", c)
            },
            Query::SortedSetDocValuesRange(c) => {
                write!(f, "Query::SortedSetDocValuesRange({:?})", c)
            },
            Query::IndexSortSortedNumericDocValuesRange(c) => {
                write!(f, "Query::IndexSortSortedNumericDocValuesRange({:?})", c)
            },
            Query::FieldExists(c) => {
                write!(f, "Query::FieldExists({:?})", c)
            },
            Query::Boolean(c) => {
                write!(f, "Query::Boolean({:?})", c)
            },
        }
    }
}

impl QueryBase for Query {
    fn as_string(&self, field: &str) -> String {
        match self {
            Query::Term(t) => t.as_string(field),
            Query::MatchAll(m) => m.as_string(field),
            Query::MatchNoDoc(m) => m.as_string(field),
            Query::Dummy(d) => d.as_string(field),
            Query::Boost(b) => b.as_string(field),
            Query::ConstantScore(c) => c.as_string(field),
            Query::PointRange(c) => c.as_string(field),
            Query::SortedNumericDocValuesSet(c) => c.as_string(field),
            Query::SortedNumericDocValuesRange(c) => c.as_string(field),
            Query::SortedSetDocValuesRange(c) => c.as_string(field),
            Query::IndexSortSortedNumericDocValuesRange(c) => c.as_string(field),
            Query::FieldExists(c) => c.as_string(field),
            Query::Boolean(c) => c.as_string(field),
        }
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
        match self {
            Query::Term(t) => Ok(QueryWeight::Term(t.create_weight(
                searcher,
                score_mode,
                boost,
                per_reader_term_state,
            )?)),
            Query::MatchAll(m) => Ok(QueryWeight::MatchAll(m.create_weight(
                searcher,
                score_mode,
                boost,
                per_reader_term_state,
            )?)),
            Query::PointRange(p) => Ok(QueryWeight::PointRange(p.create_weight(
                searcher,
                score_mode,
                boost,
                per_reader_term_state,
            )?)),
            Query::MatchNoDoc(p) => Ok(QueryWeight::MatchNoDocs(p.create_weight(
                searcher,
                score_mode,
                boost,
                per_reader_term_state,
            )?)),
            Query::SortedNumericDocValuesSet(p) => Ok(QueryWeight::SortedNumericDocValuesSet(
                p.create_weight(searcher, score_mode, boost, per_reader_term_state)?,
            )),
            Query::SortedNumericDocValuesRange(p) => Ok(QueryWeight::SortedNumericDocValuesRange(
                p.create_weight(searcher, score_mode, boost, per_reader_term_state)?,
            )),
            Query::SortedSetDocValuesRange(p) => Ok(QueryWeight::SortedSetDocValuesRange(
                p.create_weight(searcher, score_mode, boost, per_reader_term_state)?,
            )),
            Query::IndexSortSortedNumericDocValuesRange(p) => {
                Ok(QueryWeight::IndexSortSortedNumericDocValuesRange(
                    p.create_weight(searcher, score_mode, boost, per_reader_term_state)?,
                ))
            },
            Query::FieldExists(p) => Ok(QueryWeight::FieldExists(p.create_weight(
                searcher,
                score_mode,
                boost,
                per_reader_term_state,
            )?)),
            Query::ConstantScore(p) => Ok(QueryWeight::ConstantScore(p.create_weight(
                searcher,
                score_mode,
                boost,
                per_reader_term_state,
            )?)),
            _ => Err(LuceneError::illegal_argument("")),
        }
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

impl From<TermQuery> for Query {
    fn from(value: TermQuery) -> Self {
        Query::Term(value)
    }
}
impl From<MatchAllDocsQuery> for Query {
    fn from(value: MatchAllDocsQuery) -> Self {
        Query::MatchAll(value)
    }
}
impl From<MatchNoDocsQuery> for Query {
    fn from(value: MatchNoDocsQuery) -> Self {
        Query::MatchNoDoc(value)
    }
}
impl From<DummyQuery> for Query {
    fn from(value: DummyQuery) -> Self {
        Query::Dummy(value)
    }
}
impl From<BoostQuery> for Query {
    fn from(value: BoostQuery) -> Self {
        Query::Boost(value)
    }
}
impl From<ConstantScoreQuery> for Query {
    fn from(value: ConstantScoreQuery) -> Self {
        Query::ConstantScore(value)
    }
}
impl From<PointRangeQuery> for Query {
    fn from(value: PointRangeQuery) -> Self {
        Query::PointRange(value)
    }
}
impl From<SortedNumericDocValuesSetQuery> for Query {
    fn from(value: SortedNumericDocValuesSetQuery) -> Self {
        Query::SortedNumericDocValuesSet(value)
    }
}
impl From<SortedNumericDocValuesRangeQuery> for Query {
    fn from(value: SortedNumericDocValuesRangeQuery) -> Self {
        Query::SortedNumericDocValuesRange(value)
    }
}
impl From<SortedSetDocValuesRangeQuery> for Query {
    fn from(value: SortedSetDocValuesRangeQuery) -> Self {
        Query::SortedSetDocValuesRange(value)
    }
}
impl From<IndexSortSortedNumericDocValuesRangeQuery> for Query {
    fn from(value: IndexSortSortedNumericDocValuesRangeQuery) -> Self {
        Query::IndexSortSortedNumericDocValuesRange(value)
    }
}
impl From<FieldExistsQuery> for Query {
    fn from(value: FieldExistsQuery) -> Self {
        Query::FieldExists(value)
    }
}
#[derive(Clone, Debug)]
pub struct IdentityQuery {
    pub(crate) query: Arc<Query>,
}
impl IdentityQuery {
    pub fn new(query: Arc<Query>) -> Self {
        Self { query }
    }
}

impl PartialEq for IdentityQuery {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.query, &other.query)
    }
}
impl Eq for IdentityQuery {}

impl Hash for IdentityQuery {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.query).hash(state);
    }
}
impl<Q> QueryBase for Arc<Q>
where
    Q: QueryBase + ?Sized,
{
    fn as_string(&self, field: &str) -> String {
        (**self).as_string(field)
    }

    type Weight<S, IRC, QCP, QC>
        = Q::Weight<S, IRC, QCP, QC>
    where
        S: Similarity,
        IRC: IndexReaderContext,
        QCP: QueryCachingPolicy,
        QC: QueryCache;

    fn create_weight<S, IRC, QT, QCP, QC>(
        self,
        _searcher: &IndexSearcher<IRC, S, QT, QCP, QC>,
        _score_mode: &ScoreMode,
        _boost: f32,
        _per_reader_term_state: Option<TermStates<IRCTermState<IRC>>>,
    ) -> Result<Self::Weight<S, IRC, QCP, QC>>
    where
        IRC: IndexReaderContext,
        S: Similarity,
        QT: QueryTimeout,
        QCP: QueryCachingPolicy,
        QC: QueryCache,
        Self: Sized,
    {
        Err(LuceneError::unsupported_operation(format!(
            "Arc<QueryBase> cannot be used to create_weight directly: {}",
            std::any::type_name::<Q>()
        )))
    }

    type RewriteQuery = Q::RewriteQuery;

    fn rewrite<IRC, S, QT, QCP, QC>(
        &self,
        searcher: &IndexSearcher<IRC, S, QT, QCP, QC>,
    ) -> Result<Option<Self::RewriteQuery>>
    where
        IRC: IndexReaderContext,
        S: Similarity,
        QT: QueryTimeout,
        QCP: QueryCachingPolicy,
        QC: QueryCache,
    {
        (**self).rewrite(searcher)
    }

    fn visit<QV>(&self, visitor: &QV)
    where
        QV: QueryVisitor,
    {
        (**self).visit(visitor)
    }
}

pub enum QueryWeight<S, IRC, QCP, QC>
where
    IRC: IndexReaderContext,
    S: Similarity,
    QCP: QueryCachingPolicy,
    QC: QueryCache,
{
    Term(TermWeight<S, IRC>),
    MatchAll(MatchAllWeight<<IRC as IndexReaderContext>::LeafReader>),
    PointRange(PointRangeWeight<<IRC as IndexReaderContext>::LeafReader>),
    MatchNoDocs(MatchNoDocsWeight<<IRC as IndexReaderContext>::LeafReader>),
    SortedNumericDocValuesSet(
        SortedNumericDocValuesSetQueryWeight<<IRC as IndexReaderContext>::LeafReader>,
    ),
    SortedNumericDocValuesRange(
        SortedNumericDocValuesRangeQueryWeight<<IRC as IndexReaderContext>::LeafReader>,
    ),
    SortedSetDocValuesRange(
        SortedSetDocValuesRangeQueryWeight<<IRC as IndexReaderContext>::LeafReader>,
    ),
    IndexSortSortedNumericDocValuesRange(
        IndexSortSortedNumericDocValuesRangeQueryWeight<<IRC as IndexReaderContext>::LeafReader>,
    ),
    FieldExists(FieldExistsWeight<<IRC as IndexReaderContext>::LeafReader>),
    ConstantScore(ConstantScoreQueryWeight<QueryWeight<S, IRC, QCP, QC>, IRC, QCP, QC>),
}
impl<S, IRC, QCP, QC> SegmentCacheable<IRC::LeafReader> for QueryWeight<S, IRC, QCP, QC>
where
    IRC: IndexReaderContext,
    S: Similarity,
    QCP: QueryCachingPolicy,
    QC: QueryCache,
{
    fn is_cacheable(&self, ctx: &LeafReaderContext<IRC::LeafReader>) -> Result<bool> {
        match self {
            QueryWeight::Term(w) => w.is_cacheable(ctx),
            QueryWeight::MatchAll(w) => w.is_cacheable(ctx),
            QueryWeight::PointRange(w) => w.is_cacheable(ctx),
            QueryWeight::MatchNoDocs(w) => w.is_cacheable(ctx),
            QueryWeight::SortedNumericDocValuesSet(w) => w.is_cacheable(ctx),
            QueryWeight::SortedNumericDocValuesRange(w) => w.is_cacheable(ctx),
            QueryWeight::SortedSetDocValuesRange(w) => w.is_cacheable(ctx),
            QueryWeight::IndexSortSortedNumericDocValuesRange(w) => w.is_cacheable(ctx),
            QueryWeight::FieldExists(w) => w.is_cacheable(ctx),
            QueryWeight::ConstantScore(w) => w.is_cacheable(ctx),
        }
    }
}
impl<S, IRC, QCP, QC> Weight<IRC::LeafReader> for QueryWeight<S, IRC, QCP, QC>
where
    IRC: IndexReaderContext,
    S: Similarity,
    QCP: QueryCachingPolicy,
    QC: QueryCache,
{
    type Matches = DummyMatches;

    fn matches(
        &self,
        _context: &LeafReaderContext<IRC::LeafReader>,
        _doc: i32,
    ) -> Result<Option<Self::Matches>> {
        todo!()
    }

    fn default_matches(
        &self,
        context: &LeafReaderContext<IRC::LeafReader>,
        doc: i32,
    ) -> Result<Option<MatchWithNoTerms>> {
        match self {
            QueryWeight::Term(w) => w.default_matches(context, doc),
            QueryWeight::MatchAll(w) => w.default_matches(context, doc),
            QueryWeight::PointRange(w) => w.default_matches(context, doc),
            QueryWeight::MatchNoDocs(w) => w.default_matches(context, doc),
            QueryWeight::SortedNumericDocValuesSet(w) => w.default_matches(context, doc),
            QueryWeight::SortedNumericDocValuesRange(w) => w.default_matches(context, doc),
            QueryWeight::SortedSetDocValuesRange(w) => w.default_matches(context, doc),
            QueryWeight::IndexSortSortedNumericDocValuesRange(w) => w.default_matches(context, doc),
            QueryWeight::FieldExists(w) => w.default_matches(context, doc),
            QueryWeight::ConstantScore(w) => w.default_matches(context, doc),
        }
    }

    fn explain(
        &self,
        context: &LeafReaderContext<IRC::LeafReader>,
        doc: i32,
    ) -> Result<Explanation> {
        match self {
            QueryWeight::Term(w) => w.explain(context, doc),
            QueryWeight::MatchAll(w) => w.explain(context, doc),
            QueryWeight::PointRange(w) => w.explain(context, doc),
            QueryWeight::MatchNoDocs(w) => w.explain(context, doc),
            QueryWeight::SortedNumericDocValuesSet(w) => w.explain(context, doc),
            QueryWeight::SortedNumericDocValuesRange(w) => w.explain(context, doc),
            QueryWeight::SortedSetDocValuesRange(w) => w.explain(context, doc),
            QueryWeight::IndexSortSortedNumericDocValuesRange(w) => w.explain(context, doc),
            QueryWeight::FieldExists(w) => w.explain(context, doc),
            QueryWeight::ConstantScore(w) => w.explain(context, doc),
        }
    }

    fn get_query(&self) -> Arc<Query> {
        match self {
            QueryWeight::Term(w) => w.get_query(),
            QueryWeight::MatchAll(w) => w.get_query(),
            QueryWeight::PointRange(w) => w.get_query(),
            QueryWeight::MatchNoDocs(w) => w.get_query(),
            QueryWeight::SortedNumericDocValuesSet(w) => w.get_query(),
            QueryWeight::SortedNumericDocValuesRange(w) => w.get_query(),
            QueryWeight::SortedSetDocValuesRange(w) => w.get_query(),
            QueryWeight::IndexSortSortedNumericDocValuesRange(w) => w.get_query(),
            QueryWeight::FieldExists(w) => w.get_query(),
            QueryWeight::ConstantScore(w) => w.get_query(),
        }
    }

    fn scorer(
        &self,
        _context: &LeafReaderContext<IRC::LeafReader>,
    ) -> Result<Option<<Self::ScorerSupplier as ScorerSupplier<IRC::LeafReader>>::Scorer>> {
        todo!()
    }

    type ScorerSupplier = DummyScorerSupplier;

    fn scorer_supplier(
        &self,
        _context: &LeafReaderContext<IRC::LeafReader>,
    ) -> Result<Option<Self::ScorerSupplier>> {
        todo!()
    }

    fn bulk_scorer(
        &self,
        _context: &LeafReaderContext<IRC::LeafReader>,
    ) -> Result<Option<<Self::ScorerSupplier as ScorerSupplier<IRC::LeafReader>>::BulkScorer>> {
        todo!()
    }

    fn count(&self, context: &LeafReaderContext<IRC::LeafReader>) -> Result<i32> {
        match self {
            QueryWeight::Term(w) => w.count(context),
            QueryWeight::MatchAll(w) => w.count(context),
            QueryWeight::PointRange(w) => w.count(context),
            QueryWeight::MatchNoDocs(w) => w.count(context),
            QueryWeight::SortedNumericDocValuesSet(w) => w.count(context),
            QueryWeight::SortedNumericDocValuesRange(w) => w.count(context),
            QueryWeight::SortedSetDocValuesRange(w) => w.count(context),
            QueryWeight::IndexSortSortedNumericDocValuesRange(w) => w.count(context),
            QueryWeight::FieldExists(w) => w.count(context),
            QueryWeight::ConstantScore(w) => w.count(context),
        }
    }

    fn default_count(&self, _context: &LeafReaderContext<IRC::LeafReader>) -> Result<i32> {
        match self {
            QueryWeight::Term(w) => w.default_count(_context),
            QueryWeight::MatchAll(w) => w.default_count(_context),
            QueryWeight::PointRange(w) => w.default_count(_context),
            QueryWeight::MatchNoDocs(w) => w.default_count(_context),
            QueryWeight::SortedNumericDocValuesSet(w) => w.default_count(_context),
            QueryWeight::SortedNumericDocValuesRange(w) => w.default_count(_context),
            QueryWeight::SortedSetDocValuesRange(w) => w.default_count(_context),
            QueryWeight::IndexSortSortedNumericDocValuesRange(w) => w.default_count(_context),
            QueryWeight::FieldExists(w) => w.default_count(_context),
            QueryWeight::ConstantScore(w) => w.default_count(_context),
        }
    }

    fn is_weight_cacheable(&self) -> bool {
        match self {
            QueryWeight::Term(w) => w.is_weight_cacheable(),
            QueryWeight::MatchAll(w) => w.is_weight_cacheable(),
            QueryWeight::PointRange(w) => w.is_weight_cacheable(),
            QueryWeight::MatchNoDocs(w) => w.is_weight_cacheable(),
            QueryWeight::SortedNumericDocValuesSet(w) => w.is_weight_cacheable(),
            QueryWeight::SortedNumericDocValuesRange(w) => w.is_weight_cacheable(),
            QueryWeight::SortedSetDocValuesRange(w) => w.is_weight_cacheable(),
            QueryWeight::IndexSortSortedNumericDocValuesRange(w) => w.is_weight_cacheable(),
            QueryWeight::FieldExists(w) => w.is_weight_cacheable(),
            QueryWeight::ConstantScore(w) => w.is_weight_cacheable(),
        }
    }
}
pub enum QueryWeightSS<S, IRC, QCP, QC>
where
    IRC: IndexReaderContext,
    S: Similarity,
    QCP: QueryCachingPolicy,
    QC: QueryCache,
{
    Term(TermSs<IRC, S>),
    MatchAll(MatchAllSs),
    PointRange(PointRangeSs<IRC::LeafReader>),
    MatchNoDocs(MatchNoDocsSs),
    SortedNumericDocValuesSet(DefaultScorerSupplierSs<IRC::LeafReader>),
    SortedNumericDocValuesRange(SortedNumericDocValuesRangeSs<IRC::LeafReader>),
    SortedSetDocValuesRange(SortedSetDocValuesRangeSs<IRC::LeafReader>),
    IndexSortSortedNumericDocValuesRange(IndexSortSortedNumericDocValuesRangeSs<IRC::LeafReader>),
    FieldExists(FieldExistsSs<IRC::LeafReader>),
    ConstantScore(ConstantScoreSs<QueryWeight<S, IRC, QCP, QC>, IRC, QCP, QC>),
}
impl<S, IRC, QCP, QC> ScorerSupplier<IRC::LeafReader> for QueryWeightSS<S, IRC, QCP, QC>
where
    IRC: IndexReaderContext,
    S: Similarity,
    QCP: QueryCachingPolicy,
    QC: QueryCache,
{
    type Scorer = QueryWeightScorer<S, IRC, QCP, QC>;
    type BulkScorer = QueryWeightBulkScorer<S, IRC, QCP, QC>;

    fn get(
        &mut self,
        lead_cost: i64,
        context: &LeafReaderContext<IRC::LeafReader>,
    ) -> Result<Option<Self::Scorer>> {
        match self {
            QueryWeightSS::Term(s) => Ok(s.get(lead_cost, context)?.map(QueryWeightScorer::Term)),
            QueryWeightSS::MatchAll(s) => {
                Ok(s.get(lead_cost, context)?.map(QueryWeightScorer::MatchAll))
            },
            QueryWeightSS::PointRange(s) => Ok(s
                .get(lead_cost, context)?
                .map(QueryWeightScorer::PointRange)),
            QueryWeightSS::MatchNoDocs(s) => {
                Ok(s.get(lead_cost, context)?.map(QueryWeightScorer::MatchNo))
            },
            QueryWeightSS::SortedNumericDocValuesSet(s) => Ok(s
                .get(lead_cost, context)?
                .map(QueryWeightScorer::SortedNumericDocValuesSet)),
            QueryWeightSS::SortedNumericDocValuesRange(s) => Ok(s
                .get(lead_cost, context)?
                .map(QueryWeightScorer::SortedNumericDocValuesRange)),
            QueryWeightSS::SortedSetDocValuesRange(s) => Ok(s
                .get(lead_cost, context)?
                .map(QueryWeightScorer::SortedSetDocValuesRange)),
            QueryWeightSS::IndexSortSortedNumericDocValuesRange(s) => Ok(s
                .get(lead_cost, context)?
                .map(QueryWeightScorer::IndexSortSortedNumericDocValuesRange)),
            QueryWeightSS::FieldExists(s) => Ok(s
                .get(lead_cost, context)?
                .map(QueryWeightScorer::FieldExists)),
            QueryWeightSS::ConstantScore(s) => Ok(s
                .get(lead_cost, context)?
                .map(QueryWeightScorer::ConstantScore)),
        }
    }

    fn bulk_scorer(
        &mut self,
        context: &LeafReaderContext<IRC::LeafReader>,
    ) -> Result<Option<Self::BulkScorer>> {
        match self {
            QueryWeightSS::Term(s) => Ok(s.bulk_scorer(context)?.map(QueryWeightBulkScorer::Term)),
            QueryWeightSS::MatchAll(s) => {
                Ok(s.bulk_scorer(context)?.map(QueryWeightBulkScorer::MatchAll))
            },
            QueryWeightSS::PointRange(s) => Ok(s
                .bulk_scorer(context)?
                .map(QueryWeightBulkScorer::PointRange)),
            QueryWeightSS::MatchNoDocs(s) => {
                Ok(s.bulk_scorer(context)?.map(QueryWeightBulkScorer::MatchNo))
            },
            QueryWeightSS::SortedNumericDocValuesSet(s) => Ok(s
                .bulk_scorer(context)?
                .map(QueryWeightBulkScorer::SortedNumericDocValuesSet)),
            QueryWeightSS::SortedNumericDocValuesRange(s) => Ok(s
                .bulk_scorer(context)?
                .map(QueryWeightBulkScorer::SortedNumericDocValuesRange)),
            QueryWeightSS::SortedSetDocValuesRange(s) => Ok(s
                .bulk_scorer(context)?
                .map(QueryWeightBulkScorer::SortedSetDocValuesRange)),
            QueryWeightSS::IndexSortSortedNumericDocValuesRange(s) => Ok(s
                .bulk_scorer(context)?
                .map(QueryWeightBulkScorer::IndexSortSortedNumericDocValuesRange)),
            QueryWeightSS::FieldExists(s) => Ok(s
                .bulk_scorer(context)?
                .map(QueryWeightBulkScorer::FieldExists)),
            QueryWeightSS::ConstantScore(s) => Ok(s
                .bulk_scorer(context)?
                .map(QueryWeightBulkScorer::ConstantScore)),
        }
    }

    fn cost(&mut self, context: &LeafReaderContext<IRC::LeafReader>) -> Result<i64> {
        match self {
            QueryWeightSS::Term(s) => s.cost(context),
            QueryWeightSS::MatchAll(s) => s.cost(context),
            QueryWeightSS::PointRange(s) => s.cost(context),
            QueryWeightSS::MatchNoDocs(s) => s.cost(context),
            QueryWeightSS::SortedNumericDocValuesSet(s) => s.cost(context),
            QueryWeightSS::SortedNumericDocValuesRange(s) => s.cost(context),
            QueryWeightSS::SortedSetDocValuesRange(s) => s.cost(context),
            QueryWeightSS::IndexSortSortedNumericDocValuesRange(s) => s.cost(context),
            QueryWeightSS::FieldExists(s) => s.cost(context),
            QueryWeightSS::ConstantScore(s) => s.cost(context),
        }
    }

    fn set_top_level_scoring_clause(&mut self) -> Result<()> {
        match self {
            QueryWeightSS::Term(s) => {
                <TermSs<IRC, S> as ScorerSupplier<IRC::LeafReader>>::set_top_level_scoring_clause(
                    s,
                )
            },
            QueryWeightSS::MatchAll(s) => {
                <MatchAllSs as ScorerSupplier<IRC::LeafReader>>::set_top_level_scoring_clause(s)
            },
            QueryWeightSS::PointRange(s) => {
                <PointRangeSs<IRC::LeafReader> as ScorerSupplier<
                    IRC::LeafReader,
                >>::set_top_level_scoring_clause(s)
            },
            QueryWeightSS::MatchNoDocs(s) => {
                <MatchNoDocsSs as ScorerSupplier<IRC::LeafReader>>::set_top_level_scoring_clause(s)
            },
            QueryWeightSS::SortedNumericDocValuesSet(s) => {
                <DefaultScorerSupplierSs<IRC::LeafReader> as ScorerSupplier<
                    IRC::LeafReader,
                >>::set_top_level_scoring_clause(s)
            },
            QueryWeightSS::SortedNumericDocValuesRange(s) => {
                <SortedNumericDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<
                    IRC::LeafReader,
                >>::set_top_level_scoring_clause(s)
            },
            QueryWeightSS::SortedSetDocValuesRange(s) => {
                <SortedSetDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<
                    IRC::LeafReader,
                >>::set_top_level_scoring_clause(s)
            },
            QueryWeightSS::IndexSortSortedNumericDocValuesRange(s) => {
                <IndexSortSortedNumericDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<
                    IRC::LeafReader,
                >>::set_top_level_scoring_clause(s)
            },
            QueryWeightSS::FieldExists(s) => {
                <FieldExistsSs<IRC::LeafReader> as ScorerSupplier<
                    IRC::LeafReader,
                >>::set_top_level_scoring_clause(s)
            },
            QueryWeightSS::ConstantScore(s) => {
                <ConstantScoreSs<QueryWeight<S, IRC, QCP, QC>, IRC, QCP, QC> as ScorerSupplier<
                    IRC::LeafReader,
                >>::set_top_level_scoring_clause(s)
            },
        }
    }
}

pub enum QueryWeightBulkScorer<S, IRC, QCP, QC>
where
    IRC: IndexReaderContext,
    S: Similarity,
    QCP: QueryCachingPolicy,
    QC: QueryCache,
{
    Term(<TermSs<IRC,S> as ScorerSupplier<IRC::LeafReader>>::BulkScorer),
    MatchAll(<MatchAllSs as ScorerSupplier<IRC::LeafReader>>::BulkScorer),
    PointRange(<PointRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::BulkScorer),
    MatchNo(<MatchNoDocsSs as ScorerSupplier<IRC::LeafReader>>::BulkScorer),
    SortedNumericDocValuesSet(<DefaultScorerSupplierSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::BulkScorer),
    SortedNumericDocValuesRange(<SortedNumericDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::BulkScorer),
    SortedSetDocValuesRange(<SortedSetDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::BulkScorer),
    IndexSortSortedNumericDocValuesRange(<IndexSortSortedNumericDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::BulkScorer),
    FieldExists(<FieldExistsSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::BulkScorer),
    ConstantScore(ConstantScoreSsBulkScorer<QueryWeight<S, IRC, QCP, QC>, IRC, QCP, QC>),
 }
impl<S, IRC, QCP, QC> BulkScorer for QueryWeightBulkScorer<S, IRC, QCP, QC>
where
    IRC: IndexReaderContext,
    QC: QueryCache,
    QCP: QueryCachingPolicy,
    S: Similarity,
{
    fn score<LC, B>(
        &mut self,
        collector: &mut LC,
        accept_docs: Option<&B>,
        min: i32,
        max: i32,
    ) -> Result<i32>
    where
        LC: LeafCollector,
        B: Bits,
    {
        match self {
            QueryWeightBulkScorer::Term(s) => s.score(collector, accept_docs, min, max),
            QueryWeightBulkScorer::MatchAll(s) => s.score(collector, accept_docs, min, max),
            QueryWeightBulkScorer::PointRange(s) => s.score(collector, accept_docs, min, max),
            QueryWeightBulkScorer::MatchNo(s) => s.score(collector, accept_docs, min, max),
            QueryWeightBulkScorer::SortedNumericDocValuesSet(s) => {
                s.score(collector, accept_docs, min, max)
            },
            QueryWeightBulkScorer::SortedNumericDocValuesRange(s) => {
                s.score(collector, accept_docs, min, max)
            },
            QueryWeightBulkScorer::SortedSetDocValuesRange(s) => {
                s.score(collector, accept_docs, min, max)
            },
            QueryWeightBulkScorer::IndexSortSortedNumericDocValuesRange(s) => {
                s.score(collector, accept_docs, min, max)
            },
            QueryWeightBulkScorer::FieldExists(s) => s.score(collector, accept_docs, min, max),
            QueryWeightBulkScorer::ConstantScore(s) => s.score(collector, accept_docs, min, max),
        }
    }

    fn cost(&mut self) -> Result<i64> {
        match self {
            QueryWeightBulkScorer::Term(s) => s.cost(),
            QueryWeightBulkScorer::MatchAll(s) => s.cost(),
            QueryWeightBulkScorer::PointRange(s) => s.cost(),
            QueryWeightBulkScorer::MatchNo(s) => s.cost(),
            QueryWeightBulkScorer::SortedNumericDocValuesSet(s) => s.cost(),
            QueryWeightBulkScorer::SortedNumericDocValuesRange(s) => s.cost(),
            QueryWeightBulkScorer::SortedSetDocValuesRange(s) => s.cost(),
            QueryWeightBulkScorer::IndexSortSortedNumericDocValuesRange(s) => s.cost(),
            QueryWeightBulkScorer::FieldExists(s) => s.cost(),
            QueryWeightBulkScorer::ConstantScore(s) => s.cost(),
        }
    }
}

pub enum QueryWeightScorer<S, IRC, QCP, QC>
where
    IRC: IndexReaderContext,
    S: Similarity,
    QCP: QueryCachingPolicy,
    QC: QueryCache,
{
    Term(<TermSs<IRC, S> as ScorerSupplier<IRC::LeafReader>>::Scorer),
    MatchAll(<MatchAllSs as ScorerSupplier<IRC::LeafReader>>::Scorer),
    PointRange(<PointRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer),
    MatchNo(<MatchNoDocsSs as ScorerSupplier<IRC::LeafReader>>::Scorer),
    SortedNumericDocValuesSet(
        <DefaultScorerSupplierSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer,
    ),
    SortedNumericDocValuesRange(
        <SortedNumericDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer,
    ),
    SortedSetDocValuesRange(
        <SortedSetDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer,
    ),
    IndexSortSortedNumericDocValuesRange(
        <IndexSortSortedNumericDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<
            IRC::LeafReader,
        >>::Scorer,
    ),
    FieldExists(<FieldExistsSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer),
    ConstantScore(ConstantScoreSsScorer<QueryWeight<S, IRC, QCP, QC>, IRC, QCP, QC>),
}

impl<S, IRC, QCP, QC> Scorable for QueryWeightScorer<S, IRC, QCP, QC>
where
    IRC: IndexReaderContext,
    QC: QueryCache,
    QCP: QueryCachingPolicy,
    S: Similarity,
{
    fn score(&mut self) -> Result<f32> {
        match self {
            QueryWeightScorer::Term(s) => s.score(),
            QueryWeightScorer::MatchAll(s) => s.score(),
            QueryWeightScorer::PointRange(s) => s.score(),
            QueryWeightScorer::MatchNo(s) => s.score(),
            QueryWeightScorer::SortedNumericDocValuesSet(s) => s.score(),
            QueryWeightScorer::SortedNumericDocValuesRange(s) => s.score(),
            QueryWeightScorer::SortedSetDocValuesRange(s) => s.score(),
            QueryWeightScorer::IndexSortSortedNumericDocValuesRange(s) => s.score(),
            QueryWeightScorer::FieldExists(s) => s.score(),
            QueryWeightScorer::ConstantScore(s) => s.score(),
        }
    }

    fn smoothing_score(&mut self, doc_id: i32) -> Result<f32> {
        match self {
            QueryWeightScorer::Term(s) => s.smoothing_score(doc_id),
            QueryWeightScorer::MatchAll(s) => s.smoothing_score(doc_id),
            QueryWeightScorer::PointRange(s) => s.smoothing_score(doc_id),
            QueryWeightScorer::MatchNo(s) => s.smoothing_score(doc_id),
            QueryWeightScorer::SortedNumericDocValuesSet(s) => s.smoothing_score(doc_id),
            QueryWeightScorer::SortedNumericDocValuesRange(s) => s.smoothing_score(doc_id),
            QueryWeightScorer::SortedSetDocValuesRange(s) => s.smoothing_score(doc_id),
            QueryWeightScorer::IndexSortSortedNumericDocValuesRange(s) => s.smoothing_score(doc_id),
            QueryWeightScorer::FieldExists(s) => s.smoothing_score(doc_id),
            QueryWeightScorer::ConstantScore(s) => s.smoothing_score(doc_id),
        }
    }

    fn set_min_competitive_score(&mut self, min_score: f32) -> Result<()> {
        match self {
            QueryWeightScorer::Term(s) => s.set_min_competitive_score(min_score),
            QueryWeightScorer::MatchAll(s) => s.set_min_competitive_score(min_score),
            QueryWeightScorer::PointRange(s) => s.set_min_competitive_score(min_score),
            QueryWeightScorer::MatchNo(s) => s.set_min_competitive_score(min_score),
            QueryWeightScorer::SortedNumericDocValuesSet(s) => {
                s.set_min_competitive_score(min_score)
            },
            QueryWeightScorer::SortedNumericDocValuesRange(s) => {
                s.set_min_competitive_score(min_score)
            },
            QueryWeightScorer::SortedSetDocValuesRange(s) => s.set_min_competitive_score(min_score),
            QueryWeightScorer::IndexSortSortedNumericDocValuesRange(s) => {
                s.set_min_competitive_score(min_score)
            },
            QueryWeightScorer::FieldExists(s) => s.set_min_competitive_score(min_score),
            QueryWeightScorer::ConstantScore(s) => s.set_min_competitive_score(min_score),
        }
    }

    type Scorable = DummyScorable;

    fn get_children(&self) -> Result<Vec<ChildScorable<Self::Scorable>>> {
        todo!()
    }

    fn cost(&mut self) -> Result<i64> {
        match self {
            QueryWeightScorer::Term(s) => s.cost(),
            QueryWeightScorer::MatchAll(s) => s.cost(),
            QueryWeightScorer::PointRange(s) => s.cost(),
            QueryWeightScorer::MatchNo(s) => s.cost(),
            QueryWeightScorer::SortedNumericDocValuesSet(s) => s.cost(),
            QueryWeightScorer::SortedNumericDocValuesRange(s) => s.cost(),
            QueryWeightScorer::SortedSetDocValuesRange(s) => s.cost(),
            QueryWeightScorer::IndexSortSortedNumericDocValuesRange(s) => s.cost(),
            QueryWeightScorer::FieldExists(s) => s.cost(),
            QueryWeightScorer::ConstantScore(s) => s.cost(),
        }
    }
}

impl<S, IRC, QCP, QC> Scorer for QueryWeightScorer<S, IRC, QCP, QC>
where
    IRC: IndexReaderContext,
    S: Similarity,
    QCP: QueryCachingPolicy,
    QC: QueryCache,
{
    type DocIdSetIterator = QueryWeightDisi<S, IRC, QCP, QC>;
    type DocIdSetIteratorRef<'a>

    = DocIdSetIteratorEnum10<
        <<TermSs<IRC, S> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::DocIdSetIteratorRef<'a>,
        <<MatchAllSs as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::DocIdSetIteratorRef<'a>,
        <<PointRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::DocIdSetIteratorRef<'a>,
        <<MatchNoDocsSs as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::DocIdSetIteratorRef<'a>,
        <<DefaultScorerSupplierSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::DocIdSetIteratorRef<'a>,
        <<SortedNumericDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::DocIdSetIteratorRef<'a>,
        <<SortedSetDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::DocIdSetIteratorRef<'a>,
        <<IndexSortSortedNumericDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::DocIdSetIteratorRef<'a>,
        <<FieldExistsSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::DocIdSetIteratorRef<'a>,
        <<ConstantScoreSs<QueryWeight<S, IRC, QCP, QC>, IRC, QCP, QC> as ScorerSupplier<
            IRC::LeafReader,
        >>::Scorer as Scorer>::DocIdSetIteratorRef<'a>,
    > where
        Self: 'a;
    type DocIdSetIteratorMut<'a>

    = DocIdSetIteratorEnum10<
        <<TermSs<IRC, S> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::DocIdSetIteratorMut<'a>,
        <<MatchAllSs as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::DocIdSetIteratorMut<'a>,
        <<PointRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::DocIdSetIteratorMut<'a>,
        <<MatchNoDocsSs as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::DocIdSetIteratorMut<'a>,
        <<DefaultScorerSupplierSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::DocIdSetIteratorMut<'a>,
        <<SortedNumericDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::DocIdSetIteratorMut<'a>,
        <<SortedSetDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::DocIdSetIteratorMut<'a>,
        <<IndexSortSortedNumericDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::DocIdSetIteratorMut<'a>,
        <<FieldExistsSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::DocIdSetIteratorMut<'a>,
        <<ConstantScoreSs<QueryWeight<S, IRC, QCP, QC>, IRC, QCP, QC> as ScorerSupplier<
            IRC::LeafReader,
        >>::Scorer as Scorer>::DocIdSetIteratorMut<'a>,
    > where
        Self: 'a,;
    type TwoPhaseIter = QueryWeightTpi<S, IRC, QCP, QC>;
    type TwoPhaseIterRef<'a>

    = TwoPhaseIteratorEnum10<
        <<TermSs<IRC, S> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIterRef<'a>,
        <<MatchAllSs as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIterRef<'a>,
        <<PointRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIterRef<'a>,
        <<MatchNoDocsSs as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIterRef<'a>,
        <<DefaultScorerSupplierSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIterRef<'a>,
        <<SortedNumericDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIterRef<'a>,
        <<SortedSetDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIterRef<'a>,
        <<IndexSortSortedNumericDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIterRef<'a>,
        <<FieldExistsSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIterRef<'a>,
        <<ConstantScoreSs<QueryWeight<S, IRC, QCP, QC>, IRC, QCP, QC> as ScorerSupplier<
            IRC::LeafReader,
        >>::Scorer as Scorer>::TwoPhaseIterRef<'a>,
    >  where
        Self: 'a,;
    type TwoPhaseIterMut<'a>

    = TwoPhaseIteratorEnum10<
        <<TermSs<IRC, S> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIterMut<'a>,
        <<MatchAllSs as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIterMut<'a>,
        <<PointRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIterMut<'a>,
        <<MatchNoDocsSs as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIterMut<'a>,
        <<DefaultScorerSupplierSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIterMut<'a>,
        <<SortedNumericDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIterMut<'a>,
        <<SortedSetDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIterMut<'a>,
        <<IndexSortSortedNumericDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIterMut<'a>,
        <<FieldExistsSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIterMut<'a>,
        <<ConstantScoreSs<QueryWeight<S, IRC, QCP, QC>, IRC, QCP, QC> as ScorerSupplier<
            IRC::LeafReader,
        >>::Scorer as Scorer>::TwoPhaseIterMut<'a>,
    >where
        Self: 'a,;

    fn doc_id(&mut self) -> Result<i32> {
        match self {
            QueryWeightScorer::Term(s) => s.doc_id(),
            QueryWeightScorer::MatchAll(s) => s.doc_id(),
            QueryWeightScorer::PointRange(s) => s.doc_id(),
            QueryWeightScorer::MatchNo(s) => s.doc_id(),
            QueryWeightScorer::SortedNumericDocValuesSet(s) => s.doc_id(),
            QueryWeightScorer::SortedNumericDocValuesRange(s) => s.doc_id(),
            QueryWeightScorer::SortedSetDocValuesRange(s) => s.doc_id(),
            QueryWeightScorer::IndexSortSortedNumericDocValuesRange(s) => s.doc_id(),
            QueryWeightScorer::FieldExists(s) => s.doc_id(),
            QueryWeightScorer::ConstantScore(s) => s.doc_id(),
        }
    }

    fn iterator(&self) -> Self::DocIdSetIteratorRef<'_> {
        match self {
            QueryWeightScorer::Term(s) => DocIdSetIteratorEnum10::A(s.iterator()),
            QueryWeightScorer::MatchAll(s) => DocIdSetIteratorEnum10::B(s.iterator()),
            QueryWeightScorer::PointRange(s) => DocIdSetIteratorEnum10::C(s.iterator()),
            QueryWeightScorer::MatchNo(s) => DocIdSetIteratorEnum10::D(s.iterator()),
            QueryWeightScorer::SortedNumericDocValuesSet(s) => {
                DocIdSetIteratorEnum10::E(s.iterator())
            },
            QueryWeightScorer::SortedNumericDocValuesRange(s) => {
                DocIdSetIteratorEnum10::F(s.iterator())
            },
            QueryWeightScorer::SortedSetDocValuesRange(s) => {
                DocIdSetIteratorEnum10::G(s.iterator())
            },
            QueryWeightScorer::IndexSortSortedNumericDocValuesRange(s) => {
                DocIdSetIteratorEnum10::H(s.iterator())
            },
            QueryWeightScorer::FieldExists(s) => DocIdSetIteratorEnum10::I(s.iterator()),
            QueryWeightScorer::ConstantScore(s) => DocIdSetIteratorEnum10::J(s.iterator()),
        }
    }

    fn iterator_mut(&mut self) -> Self::DocIdSetIteratorMut<'_> {
        match self {
            QueryWeightScorer::Term(s) => DocIdSetIteratorEnum10::A(s.iterator_mut()),
            QueryWeightScorer::MatchAll(s) => DocIdSetIteratorEnum10::B(s.iterator_mut()),
            QueryWeightScorer::PointRange(s) => DocIdSetIteratorEnum10::C(s.iterator_mut()),
            QueryWeightScorer::MatchNo(s) => DocIdSetIteratorEnum10::D(s.iterator_mut()),
            QueryWeightScorer::SortedNumericDocValuesSet(s) => {
                DocIdSetIteratorEnum10::E(s.iterator_mut())
            },
            QueryWeightScorer::SortedNumericDocValuesRange(s) => {
                DocIdSetIteratorEnum10::F(s.iterator_mut())
            },
            QueryWeightScorer::SortedSetDocValuesRange(s) => {
                DocIdSetIteratorEnum10::G(s.iterator_mut())
            },
            QueryWeightScorer::IndexSortSortedNumericDocValuesRange(s) => {
                DocIdSetIteratorEnum10::H(s.iterator_mut())
            },
            QueryWeightScorer::FieldExists(s) => DocIdSetIteratorEnum10::I(s.iterator_mut()),
            QueryWeightScorer::ConstantScore(s) => DocIdSetIteratorEnum10::J(s.iterator_mut()),
        }
    }

    fn take_iterator(self) -> Self::DocIdSetIterator {
        match self {
            QueryWeightScorer::Term(s) => QueryWeightDisi::Term(s.take_iterator()),
            QueryWeightScorer::MatchAll(s) => QueryWeightDisi::MatchAll(s.take_iterator()),
            QueryWeightScorer::PointRange(s) => QueryWeightDisi::PointRange(s.take_iterator()),
            QueryWeightScorer::MatchNo(s) => QueryWeightDisi::MatchNo(s.take_iterator()),
            QueryWeightScorer::SortedNumericDocValuesSet(s) => {
                QueryWeightDisi::SortedNumericDocValuesSet(s.take_iterator())
            },
            QueryWeightScorer::SortedNumericDocValuesRange(s) => {
                QueryWeightDisi::SortedNumericDocValuesRange(s.take_iterator())
            },
            QueryWeightScorer::SortedSetDocValuesRange(s) => {
                QueryWeightDisi::SortedSetDocValuesRange(s.take_iterator())
            },
            QueryWeightScorer::IndexSortSortedNumericDocValuesRange(s) => {
                QueryWeightDisi::IndexSortSortedNumericDocValuesRange(s.take_iterator())
            },
            QueryWeightScorer::FieldExists(s) => QueryWeightDisi::FieldExists(s.take_iterator()),
            QueryWeightScorer::ConstantScore(s) => {
                QueryWeightDisi::ConstantScore(s.take_iterator())
            },
        }
    }

    fn two_phase_iterator(&self) -> Result<Option<Self::TwoPhaseIterRef<'_>>> {
        match self {
            QueryWeightScorer::Term(s) => s
                .two_phase_iterator()
                .map(|res| res.map(TwoPhaseIteratorEnum10::A)),
            QueryWeightScorer::MatchAll(s) => s
                .two_phase_iterator()
                .map(|res| res.map(TwoPhaseIteratorEnum10::B)),
            QueryWeightScorer::PointRange(s) => s
                .two_phase_iterator()
                .map(|res| res.map(TwoPhaseIteratorEnum10::C)),
            QueryWeightScorer::MatchNo(s) => s
                .two_phase_iterator()
                .map(|res| res.map(TwoPhaseIteratorEnum10::D)),
            QueryWeightScorer::SortedNumericDocValuesSet(s) => s
                .two_phase_iterator()
                .map(|res| res.map(TwoPhaseIteratorEnum10::E)),
            QueryWeightScorer::SortedNumericDocValuesRange(s) => s
                .two_phase_iterator()
                .map(|res| res.map(TwoPhaseIteratorEnum10::F)),
            QueryWeightScorer::SortedSetDocValuesRange(s) => s
                .two_phase_iterator()
                .map(|res| res.map(TwoPhaseIteratorEnum10::G)),
            QueryWeightScorer::IndexSortSortedNumericDocValuesRange(s) => s
                .two_phase_iterator()
                .map(|res| res.map(TwoPhaseIteratorEnum10::H)),
            QueryWeightScorer::FieldExists(s) => s
                .two_phase_iterator()
                .map(|res| res.map(TwoPhaseIteratorEnum10::I)),
            QueryWeightScorer::ConstantScore(s) => s
                .two_phase_iterator()
                .map(|res| res.map(TwoPhaseIteratorEnum10::J)),
        }
    }

    fn two_phase_iterator_mut(&mut self) -> Result<Option<Self::TwoPhaseIterMut<'_>>> {
        match self {
            QueryWeightScorer::Term(s) => s
                .two_phase_iterator_mut()
                .map(|res| res.map(TwoPhaseIteratorEnum10::A)),
            QueryWeightScorer::MatchAll(s) => s
                .two_phase_iterator_mut()
                .map(|res| res.map(TwoPhaseIteratorEnum10::B)),
            QueryWeightScorer::PointRange(s) => s
                .two_phase_iterator_mut()
                .map(|res| res.map(TwoPhaseIteratorEnum10::C)),
            QueryWeightScorer::MatchNo(s) => s
                .two_phase_iterator_mut()
                .map(|res| res.map(TwoPhaseIteratorEnum10::D)),
            QueryWeightScorer::SortedNumericDocValuesSet(s) => s
                .two_phase_iterator_mut()
                .map(|res| res.map(TwoPhaseIteratorEnum10::E)),
            QueryWeightScorer::SortedNumericDocValuesRange(s) => s
                .two_phase_iterator_mut()
                .map(|res| res.map(TwoPhaseIteratorEnum10::F)),
            QueryWeightScorer::SortedSetDocValuesRange(s) => s
                .two_phase_iterator_mut()
                .map(|res| res.map(TwoPhaseIteratorEnum10::G)),
            QueryWeightScorer::IndexSortSortedNumericDocValuesRange(s) => s
                .two_phase_iterator_mut()
                .map(|res| res.map(TwoPhaseIteratorEnum10::H)),
            QueryWeightScorer::FieldExists(s) => s
                .two_phase_iterator_mut()
                .map(|res| res.map(TwoPhaseIteratorEnum10::I)),
            QueryWeightScorer::ConstantScore(s) => s
                .two_phase_iterator_mut()
                .map(|res| res.map(TwoPhaseIteratorEnum10::J)),
        }
    }

    fn take_two_phase_iterator(self) -> Result<Option<Self::TwoPhaseIter>>
    where
        Self: Sized,
    {
        match self {
            QueryWeightScorer::Term(s) => {
                Ok(s.take_two_phase_iterator()?.map(QueryWeightTpi::Term))
            },
            QueryWeightScorer::MatchAll(s) => {
                Ok(s.take_two_phase_iterator()?.map(QueryWeightTpi::MatchAll))
            },
            QueryWeightScorer::PointRange(s) => {
                Ok(s.take_two_phase_iterator()?.map(QueryWeightTpi::PointRange))
            },
            QueryWeightScorer::MatchNo(s) => {
                Ok(s.take_two_phase_iterator()?.map(QueryWeightTpi::MatchNo))
            },
            QueryWeightScorer::SortedNumericDocValuesSet(s) => Ok(s
                .take_two_phase_iterator()?
                .map(QueryWeightTpi::SortedNumericDocValuesSet)),
            QueryWeightScorer::SortedNumericDocValuesRange(s) => Ok(s
                .take_two_phase_iterator()?
                .map(QueryWeightTpi::SortedNumericDocValuesRange)),
            QueryWeightScorer::SortedSetDocValuesRange(s) => Ok(s
                .take_two_phase_iterator()?
                .map(QueryWeightTpi::SortedSetDocValuesRange)),
            QueryWeightScorer::IndexSortSortedNumericDocValuesRange(s) => Ok(s
                .take_two_phase_iterator()?
                .map(QueryWeightTpi::IndexSortSortedNumericDocValuesRange)),
            QueryWeightScorer::FieldExists(s) => Ok(s
                .take_two_phase_iterator()?
                .map(QueryWeightTpi::FieldExists)),
            QueryWeightScorer::ConstantScore(s) => Ok(s
                .take_two_phase_iterator()?
                .map(QueryWeightTpi::ConstantScore)),
        }
    }

    fn advance_shallow(&mut self, target: i32) -> Result<i32> {
        match self {
            QueryWeightScorer::Term(s) => s.advance_shallow(target),
            QueryWeightScorer::MatchAll(s) => s.advance_shallow(target),
            QueryWeightScorer::PointRange(s) => s.advance_shallow(target),
            QueryWeightScorer::MatchNo(s) => s.advance_shallow(target),
            QueryWeightScorer::SortedNumericDocValuesSet(s) => s.advance_shallow(target),
            QueryWeightScorer::SortedNumericDocValuesRange(s) => s.advance_shallow(target),
            QueryWeightScorer::SortedSetDocValuesRange(s) => s.advance_shallow(target),
            QueryWeightScorer::IndexSortSortedNumericDocValuesRange(s) => s.advance_shallow(target),
            QueryWeightScorer::FieldExists(s) => s.advance_shallow(target),
            QueryWeightScorer::ConstantScore(s) => s.advance_shallow(target),
        }
    }

    fn default_advance_shallow(&mut self, target: i32) -> Result<i32> {
        match self {
            QueryWeightScorer::Term(s) => s.default_advance_shallow(target),
            QueryWeightScorer::MatchAll(s) => s.default_advance_shallow(target),
            QueryWeightScorer::PointRange(s) => s.default_advance_shallow(target),
            QueryWeightScorer::MatchNo(s) => s.default_advance_shallow(target),
            QueryWeightScorer::SortedNumericDocValuesSet(s) => s.default_advance_shallow(target),
            QueryWeightScorer::SortedNumericDocValuesRange(s) => s.default_advance_shallow(target),
            QueryWeightScorer::SortedSetDocValuesRange(s) => s.default_advance_shallow(target),
            QueryWeightScorer::IndexSortSortedNumericDocValuesRange(s) => {
                s.default_advance_shallow(target)
            },
            QueryWeightScorer::FieldExists(s) => s.default_advance_shallow(target),
            QueryWeightScorer::ConstantScore(s) => s.default_advance_shallow(target),
        }
    }

    fn get_max_score(&mut self, up_to: i32) -> Result<f32> {
        match self {
            QueryWeightScorer::Term(s) => s.get_max_score(up_to),
            QueryWeightScorer::MatchAll(s) => s.get_max_score(up_to),
            QueryWeightScorer::PointRange(s) => s.get_max_score(up_to),
            QueryWeightScorer::MatchNo(s) => s.get_max_score(up_to),
            QueryWeightScorer::SortedNumericDocValuesSet(s) => s.get_max_score(up_to),
            QueryWeightScorer::SortedNumericDocValuesRange(s) => s.get_max_score(up_to),
            QueryWeightScorer::SortedSetDocValuesRange(s) => s.get_max_score(up_to),
            QueryWeightScorer::IndexSortSortedNumericDocValuesRange(s) => s.get_max_score(up_to),
            QueryWeightScorer::FieldExists(s) => s.get_max_score(up_to),
            QueryWeightScorer::ConstantScore(s) => s.get_max_score(up_to),
        }
    }

    fn default_cost(&mut self) -> Result<i64> {
        match self {
            QueryWeightScorer::Term(s) => s.default_cost(),
            QueryWeightScorer::MatchAll(s) => s.default_cost(),
            QueryWeightScorer::PointRange(s) => s.default_cost(),
            QueryWeightScorer::MatchNo(s) => s.default_cost(),
            QueryWeightScorer::SortedNumericDocValuesSet(s) => s.default_cost(),
            QueryWeightScorer::SortedNumericDocValuesRange(s) => s.default_cost(),
            QueryWeightScorer::SortedSetDocValuesRange(s) => s.default_cost(),
            QueryWeightScorer::IndexSortSortedNumericDocValuesRange(s) => s.default_cost(),
            QueryWeightScorer::FieldExists(s) => s.default_cost(),
            QueryWeightScorer::ConstantScore(s) => s.default_cost(),
        }
    }

    fn has_two_phase_iterator(&self) -> TwoPhaseState {
        match self {
            QueryWeightScorer::Term(s) => s.has_two_phase_iterator(),
            QueryWeightScorer::MatchAll(s) => s.has_two_phase_iterator(),
            QueryWeightScorer::PointRange(s) => s.has_two_phase_iterator(),
            QueryWeightScorer::MatchNo(s) => s.has_two_phase_iterator(),
            QueryWeightScorer::SortedNumericDocValuesSet(s) => s.has_two_phase_iterator(),
            QueryWeightScorer::SortedNumericDocValuesRange(s) => s.has_two_phase_iterator(),
            QueryWeightScorer::SortedSetDocValuesRange(s) => s.has_two_phase_iterator(),
            QueryWeightScorer::IndexSortSortedNumericDocValuesRange(s) => {
                s.has_two_phase_iterator()
            },
            QueryWeightScorer::FieldExists(s) => s.has_two_phase_iterator(),
            QueryWeightScorer::ConstantScore(s) => s.has_two_phase_iterator(),
        }
    }
}

pub enum QueryWeightDisi<S, IRC, QCP, QC>
where
    IRC: IndexReaderContext,
    S: Similarity,
    QCP: QueryCachingPolicy,
    QC: QueryCache,
{
    Term(<<TermSs<IRC,S> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::DocIdSetIterator),
    MatchAll(<<MatchAllSs as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::DocIdSetIterator),
    PointRange(<<PointRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::DocIdSetIterator),
    MatchNo(<<MatchNoDocsSs as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::DocIdSetIterator),
    SortedNumericDocValuesSet(<<DefaultScorerSupplierSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::DocIdSetIterator),
    SortedNumericDocValuesRange(<<SortedNumericDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::DocIdSetIterator),
    SortedSetDocValuesRange(SortedSetDocValuesRangeSsScorerDisi<IRC::LeafReader>),
    IndexSortSortedNumericDocValuesRange(ISSNDVRSsScorerDisi<IRC::LeafReader>),
    FieldExists(<<FieldExistsSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::DocIdSetIterator),
    ConstantScore(ConstantScoreSsScorerDisi<QueryWeight<S, IRC, QCP, QC>, IRC, QCP, QC>),
}

impl<S, IRC, QCP, QC> DocIdSetIterator for QueryWeightDisi<S, IRC, QCP, QC>
where
    IRC: IndexReaderContext,
    S: Similarity,
    QCP: QueryCachingPolicy,
    QC: QueryCache,
{
    fn doc_id(&self) -> i32 {
        match self {
            QueryWeightDisi::Term(s) => s.doc_id(),
            QueryWeightDisi::MatchAll(s) => s.doc_id(),
            QueryWeightDisi::PointRange(s) => s.doc_id(),
            QueryWeightDisi::MatchNo(s) => s.doc_id(),
            QueryWeightDisi::SortedNumericDocValuesSet(s) => s.doc_id(),
            QueryWeightDisi::SortedNumericDocValuesRange(s) => s.doc_id(),
            QueryWeightDisi::SortedSetDocValuesRange(s) => s.doc_id(),
            QueryWeightDisi::IndexSortSortedNumericDocValuesRange(s) => s.doc_id(),
            QueryWeightDisi::FieldExists(s) => s.doc_id(),
            QueryWeightDisi::ConstantScore(s) => s.doc_id(),
        }
    }

    fn next_doc(&mut self) -> Result<i32> {
        match self {
            QueryWeightDisi::Term(s) => s.next_doc(),
            QueryWeightDisi::MatchAll(s) => s.next_doc(),
            QueryWeightDisi::PointRange(s) => s.next_doc(),
            QueryWeightDisi::MatchNo(s) => s.next_doc(),
            QueryWeightDisi::SortedNumericDocValuesSet(s) => s.next_doc(),
            QueryWeightDisi::SortedNumericDocValuesRange(s) => s.next_doc(),
            QueryWeightDisi::SortedSetDocValuesRange(s) => s.next_doc(),
            QueryWeightDisi::IndexSortSortedNumericDocValuesRange(s) => s.next_doc(),
            QueryWeightDisi::FieldExists(s) => s.next_doc(),
            QueryWeightDisi::ConstantScore(s) => s.next_doc(),
        }
    }

    fn advance(&mut self, target: i32) -> Result<i32> {
        match self {
            QueryWeightDisi::Term(s) => s.advance(target),
            QueryWeightDisi::MatchAll(s) => s.advance(target),
            QueryWeightDisi::PointRange(s) => s.advance(target),
            QueryWeightDisi::MatchNo(s) => s.advance(target),
            QueryWeightDisi::SortedNumericDocValuesSet(s) => s.advance(target),
            QueryWeightDisi::SortedNumericDocValuesRange(s) => s.advance(target),
            QueryWeightDisi::SortedSetDocValuesRange(s) => s.advance(target),
            QueryWeightDisi::IndexSortSortedNumericDocValuesRange(s) => s.advance(target),
            QueryWeightDisi::FieldExists(s) => s.advance(target),
            QueryWeightDisi::ConstantScore(s) => s.advance(target),
        }
    }

    fn slow_advance(&mut self, target: i32) -> Result<i32> {
        match self {
            QueryWeightDisi::Term(s) => s.slow_advance(target),
            QueryWeightDisi::MatchAll(s) => s.slow_advance(target),
            QueryWeightDisi::PointRange(s) => s.slow_advance(target),
            QueryWeightDisi::MatchNo(s) => s.slow_advance(target),
            QueryWeightDisi::SortedNumericDocValuesSet(s) => s.slow_advance(target),
            QueryWeightDisi::SortedNumericDocValuesRange(s) => s.slow_advance(target),
            QueryWeightDisi::SortedSetDocValuesRange(s) => s.slow_advance(target),
            QueryWeightDisi::IndexSortSortedNumericDocValuesRange(s) => s.slow_advance(target),
            QueryWeightDisi::FieldExists(s) => s.slow_advance(target),
            QueryWeightDisi::ConstantScore(s) => s.slow_advance(target),
        }
    }

    fn cost(&self) -> Result<i64> {
        match self {
            QueryWeightDisi::Term(s) => s.cost(),
            QueryWeightDisi::MatchAll(s) => s.cost(),
            QueryWeightDisi::PointRange(s) => s.cost(),
            QueryWeightDisi::MatchNo(s) => s.cost(),
            QueryWeightDisi::SortedNumericDocValuesSet(s) => s.cost(),
            QueryWeightDisi::SortedNumericDocValuesRange(s) => s.cost(),
            QueryWeightDisi::SortedSetDocValuesRange(s) => s.cost(),
            QueryWeightDisi::IndexSortSortedNumericDocValuesRange(s) => s.cost(),
            QueryWeightDisi::FieldExists(s) => s.cost(),
            QueryWeightDisi::ConstantScore(s) => s.cost(),
        }
    }
}

pub enum QueryWeightTpi<S, IRC, QCP, QC>
where
    IRC: IndexReaderContext,
    S: Similarity,
    QCP: QueryCachingPolicy,
    QC: QueryCache,
{
    Term(<<TermSs<IRC,S> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIter),
    MatchAll(<<MatchAllSs as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIter),
    PointRange(<<PointRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIter),
    MatchNo(<<MatchNoDocsSs as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIter),
    SortedNumericDocValuesSet(<<DefaultScorerSupplierSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIter),
    SortedNumericDocValuesRange(<<SortedNumericDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIter),
    SortedSetDocValuesRange(<<SortedSetDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIter),
    IndexSortSortedNumericDocValuesRange(<<IndexSortSortedNumericDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<
            IRC::LeafReader,
        >>::Scorer as Scorer>::TwoPhaseIter),
    FieldExists(<<FieldExistsSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIter),
    ConstantScore(ConstantScoreSsScorerTpi<QueryWeight<S, IRC, QCP, QC>, IRC, QCP, QC>),
}

impl<S, IRC, QCP, QC> TwoPhaseIterator for QueryWeightTpi<S, IRC, QCP, QC>
where
    IRC: IndexReaderContext,
    S: Similarity,
    QCP: QueryCachingPolicy,
    QC: QueryCache,
{
    type DocIdSetIterator = QueryWeightDisi<S, IRC, QCP, QC>;
    type DocIdSetIteratorRef<'a>

    = DocIdSetIteratorEnum10<
        <<<TermSs<IRC, S> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIter as TwoPhaseIterator>::DocIdSetIteratorRef<'a>,
        <<<MatchAllSs as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIter as TwoPhaseIterator>::DocIdSetIteratorRef<'a>,
        <<<PointRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIter as TwoPhaseIterator>::DocIdSetIteratorRef<'a>,
        <<<MatchNoDocsSs as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIter as TwoPhaseIterator>::DocIdSetIteratorRef<'a>,
        <<<DefaultScorerSupplierSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIter as TwoPhaseIterator>::DocIdSetIteratorRef<'a>,
        <<<SortedNumericDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIter as TwoPhaseIterator>::DocIdSetIteratorRef<'a>,
        <<<SortedSetDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIter as TwoPhaseIterator>::DocIdSetIteratorRef<'a>,
        <<<IndexSortSortedNumericDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIter as TwoPhaseIterator>::DocIdSetIteratorRef<'a>,
        <<<FieldExistsSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIter as TwoPhaseIterator>::DocIdSetIteratorRef<'a>,
        <<<ConstantScoreSs<QueryWeight<S, IRC, QCP, QC>, IRC, QCP, QC> as ScorerSupplier<
            IRC::LeafReader,
        >>::Scorer as Scorer>::TwoPhaseIter as TwoPhaseIterator>::DocIdSetIteratorRef<'a>,
    >   where
        Self: 'a,;
    type DocIdSetIteratorMut<'a>

    = DocIdSetIteratorEnum10<
        <<<TermSs<IRC, S> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIter as TwoPhaseIterator>::DocIdSetIteratorMut<'a>,
        <<<MatchAllSs as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIter as TwoPhaseIterator>::DocIdSetIteratorMut<'a>,
        <<<PointRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIter as TwoPhaseIterator>::DocIdSetIteratorMut<'a>,
        <<<MatchNoDocsSs as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIter as TwoPhaseIterator>::DocIdSetIteratorMut<'a>,
        <<<DefaultScorerSupplierSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIter as TwoPhaseIterator>::DocIdSetIteratorMut<'a>,
        <<<SortedNumericDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIter as TwoPhaseIterator>::DocIdSetIteratorMut<'a>,
        <<<SortedSetDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIter as TwoPhaseIterator>::DocIdSetIteratorMut<'a>,
        <<<IndexSortSortedNumericDocValuesRangeSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIter as TwoPhaseIterator>::DocIdSetIteratorMut<'a>,
        <<<FieldExistsSs<IRC::LeafReader> as ScorerSupplier<IRC::LeafReader>>::Scorer as Scorer>::TwoPhaseIter as TwoPhaseIterator>::DocIdSetIteratorMut<'a>,
        <<<ConstantScoreSs<QueryWeight<S, IRC, QCP, QC>, IRC, QCP, QC> as ScorerSupplier<
            IRC::LeafReader,
        >>::Scorer as Scorer>::TwoPhaseIter as TwoPhaseIterator>::DocIdSetIteratorMut<'a>,
    >  where
        Self: 'a,;

    fn approximation_mut(&mut self) -> Result<Self::DocIdSetIteratorMut<'_>> {
        match self {
            QueryWeightTpi::Term(s) => s.approximation_mut().map(DocIdSetIteratorEnum10::A),
            QueryWeightTpi::MatchAll(s) => s.approximation_mut().map(DocIdSetIteratorEnum10::B),
            QueryWeightTpi::PointRange(s) => s.approximation_mut().map(DocIdSetIteratorEnum10::C),
            QueryWeightTpi::MatchNo(s) => s.approximation_mut().map(DocIdSetIteratorEnum10::D),
            QueryWeightTpi::SortedNumericDocValuesSet(s) => {
                s.approximation_mut().map(DocIdSetIteratorEnum10::E)
            },
            QueryWeightTpi::SortedNumericDocValuesRange(s) => {
                s.approximation_mut().map(DocIdSetIteratorEnum10::F)
            },
            QueryWeightTpi::SortedSetDocValuesRange(s) => {
                s.approximation_mut().map(DocIdSetIteratorEnum10::G)
            },
            QueryWeightTpi::IndexSortSortedNumericDocValuesRange(s) => {
                s.approximation_mut().map(DocIdSetIteratorEnum10::H)
            },
            QueryWeightTpi::FieldExists(s) => s.approximation_mut().map(DocIdSetIteratorEnum10::I),
            QueryWeightTpi::ConstantScore(s) => {
                s.approximation_mut().map(DocIdSetIteratorEnum10::J)
            },
        }
    }

    fn approximation(&self) -> Result<Self::DocIdSetIteratorRef<'_>> {
        match self {
            QueryWeightTpi::Term(s) => s.approximation().map(DocIdSetIteratorEnum10::A),
            QueryWeightTpi::MatchAll(s) => s.approximation().map(DocIdSetIteratorEnum10::B),
            QueryWeightTpi::PointRange(s) => s.approximation().map(DocIdSetIteratorEnum10::C),
            QueryWeightTpi::MatchNo(s) => s.approximation().map(DocIdSetIteratorEnum10::D),
            QueryWeightTpi::SortedNumericDocValuesSet(s) => {
                s.approximation().map(DocIdSetIteratorEnum10::E)
            },
            QueryWeightTpi::SortedNumericDocValuesRange(s) => {
                s.approximation().map(DocIdSetIteratorEnum10::F)
            },
            QueryWeightTpi::SortedSetDocValuesRange(s) => {
                s.approximation().map(DocIdSetIteratorEnum10::G)
            },
            QueryWeightTpi::IndexSortSortedNumericDocValuesRange(s) => {
                s.approximation().map(DocIdSetIteratorEnum10::H)
            },
            QueryWeightTpi::FieldExists(s) => s.approximation().map(DocIdSetIteratorEnum10::I),
            QueryWeightTpi::ConstantScore(s) => s.approximation().map(DocIdSetIteratorEnum10::J),
        }
    }

    fn set_empty(&mut self) -> Result<()> {
        match self {
            QueryWeightTpi::Term(s) => s.set_empty(),
            QueryWeightTpi::MatchAll(s) => s.set_empty(),
            QueryWeightTpi::PointRange(s) => s.set_empty(),
            QueryWeightTpi::MatchNo(s) => s.set_empty(),
            QueryWeightTpi::SortedNumericDocValuesSet(s) => s.set_empty(),
            QueryWeightTpi::SortedNumericDocValuesRange(s) => s.set_empty(),
            QueryWeightTpi::SortedSetDocValuesRange(s) => s.set_empty(),
            QueryWeightTpi::IndexSortSortedNumericDocValuesRange(s) => s.set_empty(),
            QueryWeightTpi::FieldExists(s) => s.set_empty(),
            QueryWeightTpi::ConstantScore(s) => s.set_empty(),
        }
    }

    fn matches(&mut self) -> Result<bool> {
        match self {
            QueryWeightTpi::Term(s) => s.matches(),
            QueryWeightTpi::MatchAll(s) => s.matches(),
            QueryWeightTpi::PointRange(s) => s.matches(),
            QueryWeightTpi::MatchNo(s) => s.matches(),
            QueryWeightTpi::SortedNumericDocValuesSet(s) => s.matches(),
            QueryWeightTpi::SortedNumericDocValuesRange(s) => s.matches(),
            QueryWeightTpi::SortedSetDocValuesRange(s) => s.matches(),
            QueryWeightTpi::IndexSortSortedNumericDocValuesRange(s) => s.matches(),
            QueryWeightTpi::FieldExists(s) => s.matches(),
            QueryWeightTpi::ConstantScore(s) => s.matches(),
        }
    }

    fn match_cost(&self) -> f32 {
        match self {
            QueryWeightTpi::Term(s) => s.match_cost(),
            QueryWeightTpi::MatchAll(s) => s.match_cost(),
            QueryWeightTpi::PointRange(s) => s.match_cost(),
            QueryWeightTpi::MatchNo(s) => s.match_cost(),
            QueryWeightTpi::SortedNumericDocValuesSet(s) => s.match_cost(),
            QueryWeightTpi::SortedNumericDocValuesRange(s) => s.match_cost(),
            QueryWeightTpi::SortedSetDocValuesRange(s) => s.match_cost(),
            QueryWeightTpi::IndexSortSortedNumericDocValuesRange(s) => s.match_cost(),
            QueryWeightTpi::FieldExists(s) => s.match_cost(),
            QueryWeightTpi::ConstantScore(s) => s.match_cost(),
        }
    }
}
