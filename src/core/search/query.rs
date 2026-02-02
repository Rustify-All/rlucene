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
use crate::core::document::sorted_numeric_doc_values_range_query::SortedNumericDocValuesRangeQuery;
use crate::core::document::sorted_numeric_doc_values_set_query::SortedNumericDocValuesSetQuery;
use crate::core::document::sorted_set_doc_values_range_query::SortedSetDocValuesRangeQuery;
use crate::core::index::index_reader::Identity;
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader::{LRTermState, LeafReader};
use crate::core::index::term_states::TermStates;
use crate::core::search::QueryCache;
use crate::core::search::boolean_query::BooleanQuery;
use crate::core::search::boost_query::BoostQuery;
use crate::core::search::constant_score_query::ConstantScoreQuery;
use crate::core::search::dummy::dummy_query::DummyQuery;
use crate::core::search::field_exists_query::FieldExistsQuery;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::index_sort_sorted_numeric_doc_values_range_query::IndexSortSortedNumericDocValuesRangeQuery;
use crate::core::search::match_all_docs_query::MatchAllDocsQuery;
use crate::core::search::match_no_docs_query::MatchNoDocsQuery;
use crate::core::search::point_range_query::PointRangeQuery;
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::term_query::TermQuery;
use crate::core::search::weight::{BoxWeight, Weight};
use crate::core::util::core_helper::HasIdentity;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::cmp::PartialEq;
use std::fmt::{Debug, Formatter};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

pub trait QueryBase: Eq + Hash + Debug + HasIdentity {
    fn as_string(&self, field: &str) -> String;
    type Weight<LR, QC> = BoxWeight<LR>
    where
        LR: LeafReader,
        QC: QueryCache;
    fn create_weight<IRC, QC>(
        self,
        _searcher: &IndexSearcher<IRC, QC>,
        _score_mode: &ScoreMode,
        _boost: f32,
        _per_reader_term_state: Option<TermStates<LRTermState<IRCLeafReader<IRC>>>>,
    ) -> Result<Self::Weight<IRCLeafReader<IRC>, QC>>
    where
        IRC: IndexReaderContext,
        QC: QueryCache,
        Self: Sized,
    {
        Err(LuceneError::unsupported_operation(format!(
            "Query {} does not implement create_weight",
            std::any::type_name::<Self>()
        )))
    }
    fn rewrite<IRC, QC>(self, _searcher: &IndexSearcher<IRC, QC>) -> Result<Query>
    where
        IRC: IndexReaderContext,
        QC: QueryCache,
        Self: Sized;

    fn visit<QV>(&self, visitor: &QV)
    where
        QV: QueryVisitor;
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum BaseQuery {
    Term(TermQuery),
    MatchAll(MatchAllDocsQuery),
    MatchNoDoc(MatchNoDocsQuery),
    Dummy(DummyQuery),
    Boost(BoostQuery),
    PointRange(PointRangeQuery),
    SortedNumericDocValuesSet(SortedNumericDocValuesSetQuery),
    SortedNumericDocValuesRange(SortedNumericDocValuesRangeQuery),
    SortedSetDocValuesRange(SortedSetDocValuesRangeQuery),
    IndexSortSortedNumericDocValuesRange(IndexSortSortedNumericDocValuesRangeQuery),
    FieldExists(FieldExistsQuery),
}

impl BaseQuery {
    pub fn into_query(self) -> Query {
        self.into()
    }
}

impl TryFrom<Query> for BaseQuery {
    type Error = LuceneError;

    fn try_from(value: Query) -> Result<Self> {
        match value {
            Query::Term(t) => Ok(BaseQuery::Term(t)),
            Query::MatchAll(m) => Ok(BaseQuery::MatchAll(m)),
            Query::MatchNoDoc(m) => Ok(BaseQuery::MatchNoDoc(m)),
            Query::Dummy(d) => Ok(BaseQuery::Dummy(d)),
            Query::Boost(b) => Ok(BaseQuery::Boost(b)),
            Query::PointRange(p) => Ok(BaseQuery::PointRange(p)),
            Query::SortedNumericDocValuesSet(p) => Ok(BaseQuery::SortedNumericDocValuesSet(p)),
            Query::SortedNumericDocValuesRange(p) => Ok(BaseQuery::SortedNumericDocValuesRange(p)),
            Query::SortedSetDocValuesRange(p) => Ok(BaseQuery::SortedSetDocValuesRange(p)),
            Query::IndexSortSortedNumericDocValuesRange(p) => {
                Ok(BaseQuery::IndexSortSortedNumericDocValuesRange(p))
            },
            Query::FieldExists(p) => Ok(BaseQuery::FieldExists(p)),
            Query::ConstantScore(_) | Query::Boolean(_) => Err(LuceneError::unsupported_operation(
                "BaseQuery cannot wrap ConstantScoreQuery or BooleanQuery".to_string(),
            )),
        }
    }
}

impl From<BaseQuery> for Query {
    fn from(value: BaseQuery) -> Self {
        match value {
            BaseQuery::Term(t) => Query::Term(t),
            BaseQuery::MatchAll(m) => Query::MatchAll(m),
            BaseQuery::MatchNoDoc(m) => Query::MatchNoDoc(m),
            BaseQuery::Dummy(d) => Query::Dummy(d),
            BaseQuery::Boost(b) => Query::Boost(b),
            BaseQuery::PointRange(p) => Query::PointRange(p),
            BaseQuery::SortedNumericDocValuesSet(p) => Query::SortedNumericDocValuesSet(p),
            BaseQuery::SortedNumericDocValuesRange(p) => Query::SortedNumericDocValuesRange(p),
            BaseQuery::SortedSetDocValuesRange(p) => Query::SortedSetDocValuesRange(p),
            BaseQuery::IndexSortSortedNumericDocValuesRange(p) => {
                Query::IndexSortSortedNumericDocValuesRange(p)
            },
            BaseQuery::FieldExists(p) => Query::FieldExists(p),
        }
    }
}

macro_rules! impl_from_for_base_query {
    ( $( $ty:ty => $variant:ident ),+ $(,)? ) => {
        $(
            impl From<$ty> for BaseQuery {
                #[inline]
                fn from(value: $ty) -> Self {
                    BaseQuery::$variant(value)
                }
            }
        )+
    };
}

impl_from_for_base_query! {
    TermQuery => Term,
    MatchAllDocsQuery => MatchAll,
    MatchNoDocsQuery => MatchNoDoc,
    DummyQuery => Dummy,
    BoostQuery => Boost,
    PointRangeQuery => PointRange,
    SortedNumericDocValuesSetQuery => SortedNumericDocValuesSet,
    SortedNumericDocValuesRangeQuery => SortedNumericDocValuesRange,
    SortedSetDocValuesRangeQuery => SortedSetDocValuesRange,
    IndexSortSortedNumericDocValuesRangeQuery => IndexSortSortedNumericDocValuesRange,
    FieldExistsQuery => FieldExists,
}

impl HasIdentity for BaseQuery {
    fn identity(&self) -> &Identity {
        match self {
            BaseQuery::Term(t) => t.identity(),
            BaseQuery::MatchAll(m) => m.identity(),
            BaseQuery::MatchNoDoc(m) => m.identity(),
            BaseQuery::Dummy(d) => d.identity(),
            BaseQuery::Boost(b) => b.identity(),
            BaseQuery::PointRange(p) => p.identity(),
            BaseQuery::SortedNumericDocValuesSet(p) => p.identity(),
            BaseQuery::SortedNumericDocValuesRange(p) => p.identity(),
            BaseQuery::SortedSetDocValuesRange(p) => p.identity(),
            BaseQuery::IndexSortSortedNumericDocValuesRange(p) => p.identity(),
            BaseQuery::FieldExists(p) => p.identity(),
        }
    }
}

impl QueryBase for BaseQuery {
    fn as_string(&self, field: &str) -> String {
        match self {
            BaseQuery::Term(t) => t.as_string(field),
            BaseQuery::MatchAll(m) => m.as_string(field),
            BaseQuery::MatchNoDoc(m) => m.as_string(field),
            BaseQuery::Dummy(d) => d.as_string(field),
            BaseQuery::Boost(b) => b.as_string(field),
            BaseQuery::PointRange(p) => p.as_string(field),
            BaseQuery::SortedNumericDocValuesSet(p) => p.as_string(field),
            BaseQuery::SortedNumericDocValuesRange(p) => p.as_string(field),
            BaseQuery::SortedSetDocValuesRange(p) => p.as_string(field),
            BaseQuery::IndexSortSortedNumericDocValuesRange(p) => p.as_string(field),
            BaseQuery::FieldExists(p) => p.as_string(field),
        }
    }

    type Weight<LR, QC> = BoxWeight<LR>
    where
        LR: LeafReader,
        QC: QueryCache;

    fn create_weight<IRC, QC>(
        self,
        searcher: &IndexSearcher<IRC, QC>,
        score_mode: &ScoreMode,
        boost: f32,
        per_reader_term_state: Option<TermStates<LRTermState<IRCLeafReader<IRC>>>>,
    ) -> Result<Self::Weight<IRCLeafReader<IRC>, QC>>
    where
        IRC: IndexReaderContext,
        QC: QueryCache,
        Self: Sized,
    {
        match self {
            BaseQuery::Term(t) => {
                t.create_weight(searcher, score_mode, boost, per_reader_term_state)
            },
            BaseQuery::MatchAll(m) => {
                m.create_weight(searcher, score_mode, boost, per_reader_term_state)
            },
            BaseQuery::PointRange(p) => {
                p.create_weight(searcher, score_mode, boost, per_reader_term_state)
            },
            BaseQuery::MatchNoDoc(p) => {
                p.create_weight(searcher, score_mode, boost, per_reader_term_state)
            },
            BaseQuery::SortedNumericDocValuesSet(p) => {
                p.create_weight(searcher, score_mode, boost, per_reader_term_state)
            },
            BaseQuery::SortedNumericDocValuesRange(p) => {
                p.create_weight(searcher, score_mode, boost, per_reader_term_state)
            },
            BaseQuery::SortedSetDocValuesRange(p) => {
                p.create_weight(searcher, score_mode, boost, per_reader_term_state)
            },
            BaseQuery::IndexSortSortedNumericDocValuesRange(p) => {
                p.create_weight(searcher, score_mode, boost, per_reader_term_state)
            },
            BaseQuery::FieldExists(p) => {
                p.create_weight(searcher, score_mode, boost, per_reader_term_state)
            },
            BaseQuery::Dummy(_) => Err(LuceneError::unsupported_operation(
                "DummyQuery does not support weight creation".to_string(),
            )),
            BaseQuery::Boost(b) => {
                b.create_weight(searcher, score_mode, boost, per_reader_term_state)
            },
        }
    }

    fn rewrite<IRC, QC>(self, searcher: &IndexSearcher<IRC, QC>) -> Result<Query>
    where
        IRC: IndexReaderContext,
        QC: QueryCache,
    {
        match self {
            BaseQuery::Term(t) => t.rewrite(searcher),
            BaseQuery::MatchAll(m) => m.rewrite(searcher),
            BaseQuery::MatchNoDoc(m) => m.rewrite(searcher),
            BaseQuery::Dummy(d) => d.rewrite(searcher),
            BaseQuery::Boost(b) => b.rewrite(searcher),
            BaseQuery::PointRange(c) => c.rewrite(searcher),
            BaseQuery::SortedNumericDocValuesSet(c) => c.rewrite(searcher),
            BaseQuery::SortedNumericDocValuesRange(c) => c.rewrite(searcher),
            BaseQuery::SortedSetDocValuesRange(c) => c.rewrite(searcher),
            BaseQuery::IndexSortSortedNumericDocValuesRange(c) => c.rewrite(searcher),
            BaseQuery::FieldExists(c) => c.rewrite(searcher),
        }
    }

    fn visit<QV>(&self, visitor: &QV)
    where
        QV: QueryVisitor,
    {
        match self {
            BaseQuery::Term(t) => t.visit(visitor),
            BaseQuery::MatchAll(m) => m.visit(visitor),
            BaseQuery::MatchNoDoc(m) => m.visit(visitor),
            BaseQuery::Dummy(d) => d.visit(visitor),
            BaseQuery::Boost(b) => b.visit(visitor),
            BaseQuery::PointRange(c) => c.visit(visitor),
            BaseQuery::SortedNumericDocValuesSet(c) => c.visit(visitor),
            BaseQuery::SortedNumericDocValuesRange(c) => c.visit(visitor),
            BaseQuery::SortedSetDocValuesRange(c) => c.visit(visitor),
            BaseQuery::IndexSortSortedNumericDocValuesRange(c) => c.visit(visitor),
            BaseQuery::FieldExists(c) => c.visit(visitor),
        }
    }
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

    type Weight<LR, QC> = BoxWeight<LR>
    where
        LR: LeafReader,
        QC: QueryCache;

    fn create_weight<IRC, QC>(
        self,
        searcher: &IndexSearcher<IRC, QC>,
        score_mode: &ScoreMode,
        boost: f32,
        per_reader_term_state: Option<TermStates<LRTermState<IRCLeafReader<IRC>>>>,
    ) -> Result<Self::Weight<IRCLeafReader<IRC>, QC>>
    where
        IRC: IndexReaderContext,
        QC: QueryCache,
        Self: Sized,
    {
        match self {
            Query::Term(t) => t.create_weight(searcher, score_mode, boost, per_reader_term_state),
            Query::MatchAll(m) => m.create_weight(searcher, score_mode, boost, per_reader_term_state),
            Query::PointRange(p) => p.create_weight(searcher, score_mode, boost, per_reader_term_state),
            Query::MatchNoDoc(p) => p.create_weight(searcher, score_mode, boost, per_reader_term_state),
            Query::SortedNumericDocValuesSet(p) => {
                p.create_weight(searcher, score_mode, boost, per_reader_term_state)
            },
            Query::SortedNumericDocValuesRange(p) => {
                p.create_weight(searcher, score_mode, boost, per_reader_term_state)
            },
            Query::SortedSetDocValuesRange(p) => {
                p.create_weight(searcher, score_mode, boost, per_reader_term_state)
            },
            Query::IndexSortSortedNumericDocValuesRange(p) => {
                p.create_weight(searcher, score_mode, boost, per_reader_term_state)
            },
            Query::FieldExists(p) => p.create_weight(searcher, score_mode, boost, per_reader_term_state),
            Query::Boost(p) => p.create_weight(searcher, score_mode, boost, per_reader_term_state),
            Query::Dummy(_) => Err(LuceneError::unsupported_operation(
                "DummyQuery does not support weight creation".to_string(),
            )),
            Query::ConstantScore(p) => {
                p.create_weight(searcher, score_mode, boost, per_reader_term_state)
            },
            Query::Boolean(p) => p.create_weight(searcher, score_mode, boost, per_reader_term_state),
        }
    }

    fn rewrite<IRC, QC>(self, searcher: &IndexSearcher<IRC, QC>) -> Result<Query>
    where
        IRC: IndexReaderContext,
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

    type Weight<LR, QC> = BoxWeight<LR>
    where
        LR: LeafReader,
        QC: QueryCache;

    fn create_weight<IRC, QC>(
        self,
        _searcher: &IndexSearcher<IRC, QC>,
        _score_mode: &ScoreMode,
        _boost: f32,
        _per_reader_term_state: Option<TermStates<LRTermState<IRCLeafReader<IRC>>>>,
    ) -> Result<Self::Weight<IRCLeafReader<IRC>, QC>>
    where
        IRC: IndexReaderContext,
        QC: QueryCache,
        Self: Sized,
    {
        Err(LuceneError::unsupported_operation(format!(
            "Arc<QueryBase> cannot be used to create_weight directly: {}",
            std::any::type_name::<Q>()
        )))
    }

    fn rewrite<IRC, QC>(self, _searcher: &IndexSearcher<IRC, QC>) -> Result<Query>
    where
        IRC: IndexReaderContext,
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
