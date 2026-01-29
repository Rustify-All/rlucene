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
    SNDVRQSs, SortedNumericDocValuesRangeQuery, SortedNumericDocValuesRangeQueryWeight,
};
use crate::core::document::sorted_numeric_doc_values_set_query::{
    SNDVSQSs, SortedNumericDocValuesSetQuery, SortedNumericDocValuesSetQueryWeight,
};
use crate::core::document::sorted_set_doc_values_range_query::{
    SSDVRQSs, SortedSetDocValuesRangeQuery, SortedSetDocValuesRangeQueryWeight,
};
use crate::core::index::index_reader::Identity;
use crate::core::index::index_reader_context::{IRCTermState, IndexReaderContext};
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::query_timeout::QueryTimeout;
use crate::core::index::term_states::TermStates;
use crate::core::search::QueryCache;
use crate::core::search::boolean_query::BooleanQuery;
use crate::core::search::boolean_weight::BooleanWeight;
use crate::core::search::boost_query::BoostQuery;
use crate::core::search::bulk_scorer::{BulkScorerEnum9, BulkScorerEnum11};
use crate::core::search::constant_score_query::{
    ConstantScoreQuery, ConstantScoreQueryWeight, ConstantScoreSs,
};
use crate::core::search::dummy::dummy_matches::DummyMatches;
use crate::core::search::dummy::dummy_query::DummyQuery;
use crate::core::search::explanation::Explanation;
use crate::core::search::field_exists_query::{
    FieldExistsESs, FieldExistsQuery, FieldExistsWeight,
};
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::index_sort_sorted_numeric_doc_values_range_query::{
    ISSNDVRQSs, IndexSortSortedNumericDocValuesRangeQuery,
    IndexSortSortedNumericDocValuesRangeQueryWeight,
};
use crate::core::search::match_all_docs_query::{MatchAllDocsQuery, MatchAllSs, MatchAllWeight};
use crate::core::search::match_no_docs_query::{
    MatchNoDocsQuery, MatchNoDocsSs, MatchNoDocsWeight,
};
use crate::core::search::matches_utils::MatchWithNoTerms;
use crate::core::search::point_range_query::{PointRangeQuery, PointRangeSs, PointRangeWeight};
use crate::core::search::query_caching_policy::QueryCachingPolicy;
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::{ScorerEnum9, ScorerEnum11};
use crate::core::search::scorer_supplier::{
    ScorerSupplier, ScorerSupplierEnum9, ScorerSupplierEnum11,
};
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::term_query::{TermQuery, TermSs, TermWeight};
use crate::core::search::weight::{Weight, WeightSs};
use crate::core::util::core_helper::HasIdentity;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::cmp::PartialEq;
use std::fmt::{Debug, Formatter};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

pub type QueryBaseWeight<Q, S, IRC, QCP, QC> = <Q as QueryBase>::Weight<S, IRC, QCP, QC>;
pub trait QueryBase: Eq + Hash + Debug + HasIdentity {
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
    fn rewrite<IRC, S, QT, QCP, QC>(
        self,
        _searcher: &IndexSearcher<IRC, S, QT, QCP, QC>,
    ) -> Result<Query>
    where
        IRC: IndexReaderContext,
        S: Similarity,
        QT: QueryTimeout,
        QCP: QueryCachingPolicy,
        QC: QueryCache,
        Self: Sized;

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

impl HasIdentity for Query {
    fn identity(&self) -> &Identity {
        match self {
            Query::Term(t) => t.identity(),
            Query::MatchAll(m) => m.identity(),
            Query::MatchNoDoc(m) => m.identity(),
            Query::Dummy(d) => d.identity(),
            Query::Boost(b) => b.identity(),
            Query::ConstantScore(c) => c.identity(),
            Query::PointRange(c) => c.identity(),
            Query::SortedNumericDocValuesSet(c) => c.identity(),
            Query::SortedNumericDocValuesRange(c) => c.identity(),
            Query::SortedSetDocValuesRange(c) => c.identity(),
            Query::IndexSortSortedNumericDocValuesRange(c) => c.identity(),
            Query::FieldExists(c) => c.identity(),
            Query::Boolean(c) => c.identity(),
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
            Query::Boolean(p) => Ok(QueryWeight::Boolean(p.create_weight(
                searcher,
                score_mode,
                boost,
                per_reader_term_state,
            )?)),
            _ => Err(LuceneError::illegal_argument("")),
        }
    }

    fn rewrite<IRC, S, QT, QCP, QC>(
        self,
        searcher: &IndexSearcher<IRC, S, QT, QCP, QC>,
    ) -> Result<Query>
    where
        IRC: IndexReaderContext,
        S: Similarity,
        QT: QueryTimeout,
        QCP: QueryCachingPolicy,
        QC: QueryCache,
    {
        match self {
            Query::Term(t) => t.rewrite(searcher),
            Query::MatchAll(m) => m.rewrite(searcher),
            Query::MatchNoDoc(m) => m.rewrite(searcher),
            Query::Dummy(d) => d.rewrite(searcher),
            Query::Boost(b) => b.rewrite(searcher),
            Query::ConstantScore(c) => c.rewrite(searcher),
            Query::PointRange(c) => c.rewrite(searcher),
            Query::SortedNumericDocValuesSet(c) => c.rewrite(searcher),
            Query::SortedNumericDocValuesRange(c) => c.rewrite(searcher),
            Query::SortedSetDocValuesRange(c) => c.rewrite(searcher),
            Query::IndexSortSortedNumericDocValuesRange(c) => c.rewrite(searcher),
            Query::FieldExists(c) => c.rewrite(searcher),
            Query::Boolean(c) => c.rewrite(searcher),
        }
    }

    fn visit<QV>(&self, _visitor: &QV)
    where
        QV: QueryVisitor,
    {
        todo!()
    }
}

macro_rules! impl_from_for_query {
    ( $( $ty:ty => $variant:ident ),+ $(,)? ) => {
        $(
            impl From<$ty> for Query {
                #[inline]
                fn from(value: $ty) -> Self {
                    Query::$variant(value)
                }
            }
        )+
    };
}

impl_from_for_query! {
    TermQuery => Term,
    MatchAllDocsQuery => MatchAll,
    MatchNoDocsQuery => MatchNoDoc,
    DummyQuery => Dummy,
    BoostQuery => Boost,
    ConstantScoreQuery => ConstantScore,
    PointRangeQuery => PointRange,
    SortedNumericDocValuesSetQuery => SortedNumericDocValuesSet,
    SortedNumericDocValuesRangeQuery => SortedNumericDocValuesRange,
    SortedSetDocValuesRangeQuery => SortedSetDocValuesRange,
    IndexSortSortedNumericDocValuesRangeQuery => IndexSortSortedNumericDocValuesRange,
    FieldExistsQuery => FieldExists,
    BooleanQuery => Boolean,
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
    Q: QueryBase,
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

    fn rewrite<IRC, S, QT, QCP, QC>(
        self,
        _searcher: &IndexSearcher<IRC, S, QT, QCP, QC>,
    ) -> Result<Query>
    where
        IRC: IndexReaderContext,
        S: Similarity,
        QT: QueryTimeout,
        QCP: QueryCachingPolicy,
        QC: QueryCache,
    {
        Err(LuceneError::unsupported_operation(format!(
            "Arc<QueryBase> cannot be used to rewrite directly: {}",
            std::any::type_name::<Q>()
        )))
    }

    fn visit<QV>(&self, visitor: &QV)
    where
        QV: QueryVisitor,
    {
        (**self).visit(visitor)
    }
}

type BooleanQueryWeight<S, IRC> = BooleanWeight<
    S,
    BaseQueryWeight<S, IRC>,
    <IRC as IndexReaderContext>::LeafReader,
>;
type BooleanQueryWeightSs<S, IRC> =
    WeightSs<BooleanQueryWeight<S, IRC>, <IRC as IndexReaderContext>::LeafReader>;

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
    #[allow(clippy::type_complexity)]
    ConstantScore(ConstantScoreQueryWeight<BaseQueryWeight<S, IRC>, IRC, QCP, QC>),
    Boolean(BooleanQueryWeight<S, IRC>),
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
            QueryWeight::Boolean(w) => w.is_cacheable(ctx),
        }
    }
}
impl<S, IRC> SegmentCacheable<IRC::LeafReader> for BaseQueryWeight<S, IRC>
where
    IRC: IndexReaderContext,
    S: Similarity,
{
    fn is_cacheable(&self, ctx: &LeafReaderContext<IRC::LeafReader>) -> Result<bool> {
        match self {
            BaseQueryWeight::Term(w) => w.is_cacheable(ctx),
            BaseQueryWeight::MatchAll(w) => w.is_cacheable(ctx),
            BaseQueryWeight::PointRange(w) => w.is_cacheable(ctx),
            BaseQueryWeight::MatchNoDocs(w) => w.is_cacheable(ctx),
            BaseQueryWeight::SortedNumericDocValuesSet(w) => w.is_cacheable(ctx),
            BaseQueryWeight::SortedNumericDocValuesRange(w) => w.is_cacheable(ctx),
            BaseQueryWeight::SortedSetDocValuesRange(w) => w.is_cacheable(ctx),
            BaseQueryWeight::IndexSortSortedNumericDocValuesRange(w) => w.is_cacheable(ctx),
            BaseQueryWeight::FieldExists(w) => w.is_cacheable(ctx),
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
            QueryWeight::Boolean(w) => w.default_matches(context, doc),
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
            QueryWeight::Boolean(w) => w.explain(context, doc),
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
            QueryWeight::Boolean(w) => w.get_query(),
        }
    }

    fn scorer(
        &self,
        context: &LeafReaderContext<IRC::LeafReader>,
    ) -> Result<Option<<Self::ScorerSupplier as ScorerSupplier<IRC::LeafReader>>::Scorer>> {
        match self {
            QueryWeight::Term(w) => Ok(w.scorer(context)?.map(ScorerEnum11::A)),
            QueryWeight::MatchAll(w) => Ok(w.scorer(context)?.map(ScorerEnum11::B)),
            QueryWeight::PointRange(w) => Ok(w.scorer(context)?.map(ScorerEnum11::C)),
            QueryWeight::MatchNoDocs(w) => Ok(w.scorer(context)?.map(ScorerEnum11::D)),
            QueryWeight::SortedNumericDocValuesSet(w) => {
                Ok(w.scorer(context)?.map(ScorerEnum11::E))
            },
            QueryWeight::SortedNumericDocValuesRange(w) => {
                Ok(w.scorer(context)?.map(ScorerEnum11::F))
            },
            QueryWeight::SortedSetDocValuesRange(w) => Ok(w.scorer(context)?.map(ScorerEnum11::G)),
            QueryWeight::IndexSortSortedNumericDocValuesRange(w) => {
                Ok(w.scorer(context)?.map(ScorerEnum11::H))
            },
            QueryWeight::FieldExists(w) => Ok(w.scorer(context)?.map(ScorerEnum11::I)),
            QueryWeight::ConstantScore(w) => Ok(w.scorer(context)?.map(ScorerEnum11::J)),
            QueryWeight::Boolean(w) => Ok(w.scorer(context)?.map(ScorerEnum11::K)),
        }
    }

    type ScorerSupplier = ScorerSupplierEnum11<
        TermSs<IRC, S>,
        MatchAllSs,
        PointRangeSs<IRC::LeafReader>,
        MatchNoDocsSs,
        SNDVSQSs<IRC::LeafReader>,
        SNDVRQSs<IRC::LeafReader>,
        SSDVRQSs<IRC::LeafReader>,
        ISSNDVRQSs<IRC::LeafReader>,
        FieldExistsESs<IRC::LeafReader>,
        ConstantScoreSs<BaseQueryWeight<S, IRC>, IRC, QCP, QC>,
        BooleanQueryWeightSs<S, IRC>,
    >;

    fn scorer_supplier(
        &self,
        context: &LeafReaderContext<IRC::LeafReader>,
    ) -> Result<Option<Self::ScorerSupplier>> {
        match self {
            QueryWeight::Term(w) => Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum11::A)),
            QueryWeight::MatchAll(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum11::B))
            },
            QueryWeight::PointRange(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum11::C))
            },
            QueryWeight::MatchNoDocs(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum11::D))
            },
            QueryWeight::SortedNumericDocValuesSet(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum11::E))
            },
            QueryWeight::SortedNumericDocValuesRange(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum11::F))
            },
            QueryWeight::SortedSetDocValuesRange(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum11::G))
            },
            QueryWeight::IndexSortSortedNumericDocValuesRange(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum11::H))
            },
            QueryWeight::FieldExists(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum11::I))
            },
            QueryWeight::ConstantScore(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum11::J))
            },
            QueryWeight::Boolean(w) => Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum11::K)),
        }
    }

    fn bulk_scorer(
        &self,
        context: &LeafReaderContext<IRC::LeafReader>,
    ) -> Result<Option<<Self::ScorerSupplier as ScorerSupplier<IRC::LeafReader>>::BulkScorer>> {
        match self {
            QueryWeight::Term(w) => Ok(w.bulk_scorer(context)?.map(BulkScorerEnum11::A)),
            QueryWeight::MatchAll(w) => Ok(w.bulk_scorer(context)?.map(BulkScorerEnum11::B)),
            QueryWeight::PointRange(w) => Ok(w.bulk_scorer(context)?.map(BulkScorerEnum11::C)),
            QueryWeight::MatchNoDocs(w) => Ok(w.bulk_scorer(context)?.map(BulkScorerEnum11::D)),
            QueryWeight::SortedNumericDocValuesSet(w) => {
                Ok(w.bulk_scorer(context)?.map(BulkScorerEnum11::E))
            },
            QueryWeight::SortedNumericDocValuesRange(w) => {
                Ok(w.bulk_scorer(context)?.map(BulkScorerEnum11::F))
            },
            QueryWeight::SortedSetDocValuesRange(w) => {
                Ok(w.bulk_scorer(context)?.map(BulkScorerEnum11::G))
            },
            QueryWeight::IndexSortSortedNumericDocValuesRange(w) => {
                Ok(w.bulk_scorer(context)?.map(BulkScorerEnum11::H))
            },
            QueryWeight::FieldExists(w) => Ok(w.bulk_scorer(context)?.map(BulkScorerEnum11::I)),
            QueryWeight::ConstantScore(w) => Ok(w.bulk_scorer(context)?.map(BulkScorerEnum11::J)),
            QueryWeight::Boolean(w) => Ok(w.bulk_scorer(context)?.map(BulkScorerEnum11::K)),
        }
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
            QueryWeight::Boolean(w) => w.count(context),
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
            QueryWeight::Boolean(w) => w.default_count(_context),
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
            QueryWeight::Boolean(w) => w.is_weight_cacheable(),
        }
    }
}
pub enum BaseQueryWeight<S, IRC>
where
    IRC: IndexReaderContext,
    S: Similarity,
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
}
impl<S, IRC> Weight<IRC::LeafReader> for BaseQueryWeight<S, IRC>
where
    IRC: IndexReaderContext,
    S: Similarity,
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
            BaseQueryWeight::Term(w) => w.default_matches(context, doc),
            BaseQueryWeight::MatchAll(w) => w.default_matches(context, doc),
            BaseQueryWeight::PointRange(w) => w.default_matches(context, doc),
            BaseQueryWeight::MatchNoDocs(w) => w.default_matches(context, doc),
            BaseQueryWeight::SortedNumericDocValuesSet(w) => w.default_matches(context, doc),
            BaseQueryWeight::SortedNumericDocValuesRange(w) => w.default_matches(context, doc),
            BaseQueryWeight::SortedSetDocValuesRange(w) => w.default_matches(context, doc),
            BaseQueryWeight::IndexSortSortedNumericDocValuesRange(w) => {
                w.default_matches(context, doc)
            },
            BaseQueryWeight::FieldExists(w) => w.default_matches(context, doc),
        }
    }

    fn explain(
        &self,
        context: &LeafReaderContext<IRC::LeafReader>,
        doc: i32,
    ) -> Result<Explanation> {
        match self {
            BaseQueryWeight::Term(w) => w.explain(context, doc),
            BaseQueryWeight::MatchAll(w) => w.explain(context, doc),
            BaseQueryWeight::PointRange(w) => w.explain(context, doc),
            BaseQueryWeight::MatchNoDocs(w) => w.explain(context, doc),
            BaseQueryWeight::SortedNumericDocValuesSet(w) => w.explain(context, doc),
            BaseQueryWeight::SortedNumericDocValuesRange(w) => w.explain(context, doc),
            BaseQueryWeight::SortedSetDocValuesRange(w) => w.explain(context, doc),
            BaseQueryWeight::IndexSortSortedNumericDocValuesRange(w) => w.explain(context, doc),
            BaseQueryWeight::FieldExists(w) => w.explain(context, doc),
        }
    }

    fn get_query(&self) -> Arc<Query> {
        match self {
            BaseQueryWeight::Term(w) => w.get_query(),
            BaseQueryWeight::MatchAll(w) => w.get_query(),
            BaseQueryWeight::PointRange(w) => w.get_query(),
            BaseQueryWeight::MatchNoDocs(w) => w.get_query(),
            BaseQueryWeight::SortedNumericDocValuesSet(w) => w.get_query(),
            BaseQueryWeight::SortedNumericDocValuesRange(w) => w.get_query(),
            BaseQueryWeight::SortedSetDocValuesRange(w) => w.get_query(),
            BaseQueryWeight::IndexSortSortedNumericDocValuesRange(w) => w.get_query(),
            BaseQueryWeight::FieldExists(w) => w.get_query(),
        }
    }

    fn scorer(
        &self,
        context: &LeafReaderContext<IRC::LeafReader>,
    ) -> Result<Option<<Self::ScorerSupplier as ScorerSupplier<IRC::LeafReader>>::Scorer>> {
        match self {
            BaseQueryWeight::Term(w) => Ok(w.scorer(context)?.map(ScorerEnum9::A)),
            BaseQueryWeight::MatchAll(w) => Ok(w.scorer(context)?.map(ScorerEnum9::B)),
            BaseQueryWeight::PointRange(w) => Ok(w.scorer(context)?.map(ScorerEnum9::C)),
            BaseQueryWeight::MatchNoDocs(w) => Ok(w.scorer(context)?.map(ScorerEnum9::D)),
            BaseQueryWeight::SortedNumericDocValuesSet(w) => {
                Ok(w.scorer(context)?.map(ScorerEnum9::E))
            },
            BaseQueryWeight::SortedNumericDocValuesRange(w) => {
                Ok(w.scorer(context)?.map(ScorerEnum9::F))
            },
            BaseQueryWeight::SortedSetDocValuesRange(w) => {
                Ok(w.scorer(context)?.map(ScorerEnum9::G))
            },
            BaseQueryWeight::IndexSortSortedNumericDocValuesRange(w) => {
                Ok(w.scorer(context)?.map(ScorerEnum9::H))
            },
            BaseQueryWeight::FieldExists(w) => Ok(w.scorer(context)?.map(ScorerEnum9::I)),
        }
    }

    type ScorerSupplier = ScorerSupplierEnum9<
        TermSs<IRC, S>,
        MatchAllSs,
        PointRangeSs<IRC::LeafReader>,
        MatchNoDocsSs,
        SNDVSQSs<IRC::LeafReader>,
        SNDVRQSs<IRC::LeafReader>,
        SSDVRQSs<IRC::LeafReader>,
        ISSNDVRQSs<IRC::LeafReader>,
        FieldExistsESs<IRC::LeafReader>,
    >;

    fn scorer_supplier(
        &self,
        context: &LeafReaderContext<IRC::LeafReader>,
    ) -> Result<Option<Self::ScorerSupplier>> {
        match self {
            BaseQueryWeight::Term(w) => Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum9::A)),
            BaseQueryWeight::MatchAll(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum9::B))
            },
            BaseQueryWeight::PointRange(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum9::C))
            },
            BaseQueryWeight::MatchNoDocs(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum9::D))
            },
            BaseQueryWeight::SortedNumericDocValuesSet(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum9::E))
            },
            BaseQueryWeight::SortedNumericDocValuesRange(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum9::F))
            },
            BaseQueryWeight::SortedSetDocValuesRange(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum9::G))
            },
            BaseQueryWeight::IndexSortSortedNumericDocValuesRange(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum9::H))
            },
            BaseQueryWeight::FieldExists(w) => {
                Ok(w.scorer_supplier(context)?.map(ScorerSupplierEnum9::I))
            },
        }
    }

    fn bulk_scorer(
        &self,
        context: &LeafReaderContext<IRC::LeafReader>,
    ) -> Result<Option<<Self::ScorerSupplier as ScorerSupplier<IRC::LeafReader>>::BulkScorer>> {
        match self {
            BaseQueryWeight::Term(w) => Ok(w.bulk_scorer(context)?.map(BulkScorerEnum9::A)),
            BaseQueryWeight::MatchAll(w) => Ok(w.bulk_scorer(context)?.map(BulkScorerEnum9::B)),
            BaseQueryWeight::PointRange(w) => Ok(w.bulk_scorer(context)?.map(BulkScorerEnum9::C)),
            BaseQueryWeight::MatchNoDocs(w) => Ok(w.bulk_scorer(context)?.map(BulkScorerEnum9::D)),
            BaseQueryWeight::SortedNumericDocValuesSet(w) => {
                Ok(w.bulk_scorer(context)?.map(BulkScorerEnum9::E))
            },
            BaseQueryWeight::SortedNumericDocValuesRange(w) => {
                Ok(w.bulk_scorer(context)?.map(BulkScorerEnum9::F))
            },
            BaseQueryWeight::SortedSetDocValuesRange(w) => {
                Ok(w.bulk_scorer(context)?.map(BulkScorerEnum9::G))
            },
            BaseQueryWeight::IndexSortSortedNumericDocValuesRange(w) => {
                Ok(w.bulk_scorer(context)?.map(BulkScorerEnum9::H))
            },
            BaseQueryWeight::FieldExists(w) => Ok(w.bulk_scorer(context)?.map(BulkScorerEnum9::I)),
        }
    }

    fn count(&self, context: &LeafReaderContext<IRC::LeafReader>) -> Result<i32> {
        match self {
            BaseQueryWeight::Term(w) => w.count(context),
            BaseQueryWeight::MatchAll(w) => w.count(context),
            BaseQueryWeight::PointRange(w) => w.count(context),
            BaseQueryWeight::MatchNoDocs(w) => w.count(context),
            BaseQueryWeight::SortedNumericDocValuesSet(w) => w.count(context),
            BaseQueryWeight::SortedNumericDocValuesRange(w) => w.count(context),
            BaseQueryWeight::SortedSetDocValuesRange(w) => w.count(context),
            BaseQueryWeight::IndexSortSortedNumericDocValuesRange(w) => w.count(context),
            BaseQueryWeight::FieldExists(w) => w.count(context),
        }
    }

    fn default_count(&self, _context: &LeafReaderContext<IRC::LeafReader>) -> Result<i32> {
        match self {
            BaseQueryWeight::Term(w) => w.default_count(_context),
            BaseQueryWeight::MatchAll(w) => w.default_count(_context),
            BaseQueryWeight::PointRange(w) => w.default_count(_context),
            BaseQueryWeight::MatchNoDocs(w) => w.default_count(_context),
            BaseQueryWeight::SortedNumericDocValuesSet(w) => w.default_count(_context),
            BaseQueryWeight::SortedNumericDocValuesRange(w) => w.default_count(_context),
            BaseQueryWeight::SortedSetDocValuesRange(w) => w.default_count(_context),
            BaseQueryWeight::IndexSortSortedNumericDocValuesRange(w) => w.default_count(_context),
            BaseQueryWeight::FieldExists(w) => w.default_count(_context),
        }
    }

    fn is_weight_cacheable(&self) -> bool {
        match self {
            BaseQueryWeight::Term(w) => w.is_weight_cacheable(),
            BaseQueryWeight::MatchAll(w) => w.is_weight_cacheable(),
            BaseQueryWeight::PointRange(w) => w.is_weight_cacheable(),
            BaseQueryWeight::MatchNoDocs(w) => w.is_weight_cacheable(),
            BaseQueryWeight::SortedNumericDocValuesSet(w) => w.is_weight_cacheable(),
            BaseQueryWeight::SortedNumericDocValuesRange(w) => w.is_weight_cacheable(),
            BaseQueryWeight::SortedSetDocValuesRange(w) => w.is_weight_cacheable(),
            BaseQueryWeight::IndexSortSortedNumericDocValuesRange(w) => w.is_weight_cacheable(),
            BaseQueryWeight::FieldExists(w) => w.is_weight_cacheable(),
        }
    }
}

impl Query {
    pub(crate) fn create_weight_no_constant_score<S, IRC, QT, QCP, QC>(
        self,
        searcher: &IndexSearcher<IRC, S, QT, QCP, QC>,
        score_mode: &ScoreMode,
        boost: f32,
        per_reader_term_state: Option<TermStates<IRCTermState<IRC>>>,
    ) -> Result<BaseQueryWeight<S, IRC>>
    where
        IRC: IndexReaderContext,
        S: Similarity,
        QT: QueryTimeout,
        QCP: QueryCachingPolicy,
        QC: QueryCache,
    {
        match self {
            Query::Term(t) => Ok(BaseQueryWeight::Term(t.create_weight(
                searcher,
                score_mode,
                boost,
                per_reader_term_state,
            )?)),
            Query::MatchAll(m) => Ok(BaseQueryWeight::MatchAll(m.create_weight(
                searcher,
                score_mode,
                boost,
                per_reader_term_state,
            )?)),
            Query::PointRange(p) => Ok(BaseQueryWeight::PointRange(p.create_weight(
                searcher,
                score_mode,
                boost,
                per_reader_term_state,
            )?)),
            Query::MatchNoDoc(p) => Ok(BaseQueryWeight::MatchNoDocs(p.create_weight(
                searcher,
                score_mode,
                boost,
                per_reader_term_state,
            )?)),
            Query::SortedNumericDocValuesSet(p) => Ok(BaseQueryWeight::SortedNumericDocValuesSet(
                p.create_weight(searcher, score_mode, boost, per_reader_term_state)?,
            )),
            Query::SortedNumericDocValuesRange(p) => {
                Ok(BaseQueryWeight::SortedNumericDocValuesRange(
                    p.create_weight(searcher, score_mode, boost, per_reader_term_state)?,
                ))
            },
            Query::SortedSetDocValuesRange(p) => Ok(BaseQueryWeight::SortedSetDocValuesRange(
                p.create_weight(searcher, score_mode, boost, per_reader_term_state)?,
            )),
            Query::IndexSortSortedNumericDocValuesRange(p) => {
                Ok(BaseQueryWeight::IndexSortSortedNumericDocValuesRange(
                    p.create_weight(searcher, score_mode, boost, per_reader_term_state)?,
                ))
            },
            Query::FieldExists(p) => Ok(BaseQueryWeight::FieldExists(p.create_weight(
                searcher,
                score_mode,
                boost,
                per_reader_term_state,
            )?)),
            Query::ConstantScore(p) => p.into_inner().create_weight_no_constant_score(
                searcher,
                score_mode,
                boost,
                per_reader_term_state,
            ),
            _ => Err(LuceneError::illegal_argument("")),
        }
    }
}
