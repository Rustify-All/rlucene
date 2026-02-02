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
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::term_states::TermStates;
use crate::core::search::QueryCache;
use crate::core::search::bulk_scorer::{BulkScorer, BulkScorerEnum, BulkScorerEnum2, BulkScorerEnum9};
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::constant_score_weight::ConstantScoreWeight;
use crate::core::search::doc_id_stream::DocIdStream;

use crate::core::document::sorted_numeric_doc_values_range_query::{
    SNDVRQSs, SortedNumericDocValuesRangeQueryWeight,
};
use crate::core::document::sorted_numeric_doc_values_set_query::{
    SNDVSQSs, SortedNumericDocValuesSetQueryWeight,
};
use crate::core::document::sorted_set_doc_values_range_query::{
    SSDVRQSs, SortedSetDocValuesRangeQueryWeight,
};
use crate::core::index::index_reader::Identity;
use crate::core::index::leaf_reader::{LRTermState, LeafReader};
use crate::core::search::dummy::dummy_disi::DummyDISI;
use crate::core::search::dummy::dummy_matches::DummyMatches;
use crate::core::search::dummy::dummy_two_phase_iterator::DummyTwoPhaseIterator;
use crate::core::search::explanation::Explanation;
use crate::core::search::field_exists_query::{FieldExistsESs, FieldExistsWeight};
use crate::core::search::filter_leaf_collector::FilterLeafCollector;
use crate::core::search::filter_scorable::FilterScorable;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::index_sort_sorted_numeric_doc_values_range_query::{
    ISSNDVRQSs, IndexSortSortedNumericDocValuesRangeQueryWeight,
};
use crate::core::search::leaf_collector::LeafCollector;
use crate::core::search::match_all_docs_query::{MatchAllSs, MatchAllWeight};
use crate::core::search::match_no_docs_query::{MatchNoDocsSs, MatchNoDocsWeight};
use crate::core::search::matches_utils::MatchWithNoTerms;
use crate::core::search::point_range_query::{PointRangeSs, PointRangeWeight};
use crate::core::search::query::{BaseQuery, Query, QueryBase};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::scorable::{ChildScorable, Scorable};
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::{Scorer, ScorerEnum, ScorerEnum2, ScorerEnum9, TwoPhaseState};
use crate::core::search::scorer_supplier::{ScorerSupplier, ScorerSupplierEnum, ScorerSupplierEnum9};
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::term_query::{TermSs, TermWeight};
use crate::core::search::weight::{BoxWeight, DefaultBulkScorer, Weight};
use crate::core::util::bits::Bits;
use crate::core::util::core_helper::HasIdentity;
use crate::core::util::error::lucene_error::Result;
use std::fmt::{Debug, Display, Formatter};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// A query that wraps another query and simply returns a constant score equal to 1 for every document that matches the query.
/// It therefore simply strips of all scores and always returns 1.
#[derive(Debug, Clone)]
pub struct ConstantScoreQuery {
    id: Identity,
    query: Box<BaseQuery>,
}
impl ConstantScoreQuery {
    /// Strips off scores from the passed in Query. The hits will get a constant score of 1.
    pub fn new<T>(query: T) -> Self
    where
        T: Into<Box<BaseQuery>>,
    {
        let query = query.into();
        Self {
            id: Identity::new(),
            query,
        }
    }

    pub(crate) fn into_inner(self) -> BaseQuery {
        *self.query
    }
}
impl Eq for ConstantScoreQuery {}

impl PartialEq<Self> for ConstantScoreQuery {
    fn eq(&self, other: &Self) -> bool {
        self.query == other.query
    }
}

impl Hash for ConstantScoreQuery {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::any::type_name::<Self>().to_string().hash(state);
        self.query.hash(state);
    }
}

impl HasIdentity for ConstantScoreQuery {
    fn identity(&self) -> &Identity {
        &self.id
    }
}
impl QueryBase for ConstantScoreQuery {
    fn as_string(&self, field: &str) -> String {
        let inner = self.query.as_string(field);
        format!("ConstantScore({})", inner)
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
        let inner_score_mode = if score_mode.is_exhaustive() {
            ScoreMode::CompleteNoScores
        } else {
            ScoreMode::TopDocs
        };
        let query = *self.query;
        let inner_weight =
            query.create_weight(searcher, &inner_score_mode, 1.0, per_reader_term_state)?;
        let inner_weight = searcher.wrap_weight(inner_weight, inner_score_mode);
        if score_mode.needs_scores() {
            Ok(Box::new(ConstantScoreQueryWeight::new(
                boost,
                inner_weight,
                *score_mode,
            )))
        } else {
            Ok(inner_weight)
        }
    }

    fn rewrite<IRC, QC>(mut self, searcher: &IndexSearcher<IRC, QC>) -> Result<Query>
    where
        IRC: IndexReaderContext,
        QC: QueryCache,
    {
        let query_id = self.query.identity().clone();
        let rewritten = self.query.rewrite(searcher)?;

        let rewritten = match rewritten {
            Query::Boost(b) => *b.query,
            Query::ConstantScore(cs) => cs.into_inner(),
            q => BaseQuery::try_from(q)?,
        };

        if let BaseQuery::MatchNoDoc(v) = rewritten {
            return Ok(v.into());
        }

        if rewritten.identity() != &query_id {
            return Ok(ConstantScoreQuery::new(rewritten).into());
        }

        self.query = Box::new(rewritten);
        Ok(self.into())
    }

    fn visit<QV>(&self, _visitor: &QV)
    where
        QV: QueryVisitor,
    {
        todo!()
    }
}

pub struct ConstantScoreQueryWeight<LR>
where
    LR: LeafReader,
{
    base: ConstantScoreWeight,
    inner_weight: BoxWeight<LR>,
    score_mode: ScoreMode,
}

impl<LR> ConstantScoreQueryWeight<LR>
where
    LR: LeafReader,
{
    pub fn new(boost: f32, inner_weight: BoxWeight<LR>, score_mode: ScoreMode) -> Self {
        Self {
            base: ConstantScoreWeight::new(boost),
            inner_weight,
            score_mode,
        }
    }
}

impl<LR> SegmentCacheable<LR> for ConstantScoreQueryWeight<LR>
where
    LR: LeafReader,
{
    fn is_cacheable(&self, ctx: &LeafReaderContext<LR>) -> Result<bool> {
        self.inner_weight.is_cacheable(ctx)
    }
}

impl<LR> Weight<LR> for ConstantScoreQueryWeight<LR>
where
    LR: LeafReader,
{
    type Matches = MatchWithNoTerms;

    fn matches(&self, context: &LeafReaderContext<LR>, doc: i32) -> Result<Option<Self::Matches>> {
        self.default_matches(context, doc)
    }

    fn explain(&self, context: &LeafReaderContext<LR>, doc: i32) -> Result<Explanation> {
        let scorer = self.scorer(context)?;
        self.base
            .explain(scorer, doc, self.get_query().as_string(""))
    }

    fn get_query(&self) -> Arc<Query> {
        self.inner_weight.get_query()
    }

    type ScorerSupplier = ScorerSupplierEnum<LR>;

    fn scorer_supplier(
        &self,
        context: &LeafReaderContext<LR>,
    ) -> Result<Option<Self::ScorerSupplier>> {
        match self.inner_weight.scorer_supplier(context)? {
            Some(inner_scorer_supplier) => Ok(Some(ScorerSupplierEnum::ConstantScore(
                ConstantScoreScorerSupplier::new(
                    self.score_mode,
                    inner_scorer_supplier,
                    self.base.score(),
                ),
            ))),
            None => Ok(None),
        }
    }

    fn count(&self, context: &LeafReaderContext<LR>) -> Result<i32> {
        self.inner_weight.count(context)
    }
}

pub type ConstantScoreScorerEnum<LR> = ScorerEnum2<
    ConstantScoreScorer<<ScorerEnum<LR> as Scorer>::DocIdSetIterator, DummyTwoPhaseIterator>,
    ConstantScoreScorer<DummyDISI, <ScorerEnum<LR> as Scorer>::TwoPhaseIter>,
>;
pub type ConstantScoreBulkScorer<LR> = BulkScorerEnum2<
    DefaultBulkScorer<ConstantScoreScorerEnum<LR>>,
    ConstantBulkScorer<BulkScorerEnum<LR>>,
>;

pub struct ConstantScoreScorerSupplier<LR>
where
    LR: LeafReader,
{
    score_mode: ScoreMode,
    inner_scorer_supplier: ScorerSupplierEnum<LR>,
    score: f32,
}
impl<LR> ConstantScoreScorerSupplier<LR>
where
    LR: LeafReader,
{
    fn new(score_mode: ScoreMode, inner_scorer_supplier: ScorerSupplierEnum<LR>, score: f32) -> Self {
        Self {
            score_mode,
            inner_scorer_supplier,
            score,
        }
    }
}
impl<LR> ScorerSupplier<LR> for ConstantScoreScorerSupplier<LR>
where
    LR: LeafReader,
{
    type Scorer = ConstantScoreScorerEnum<LR>;
    type BulkScorer = ConstantScoreBulkScorer<LR>;

    fn get(&mut self, lead_cost: i64, context: &LeafReaderContext<LR>) -> Result<Self::Scorer> {
        let inner_scorer = self.inner_scorer_supplier.get(lead_cost, context)?;
        let has_tpi = inner_scorer.has_two_phase_iterator() == TwoPhaseState::Yes
            || inner_scorer.two_phase_iterator()?.is_some();
        match has_tpi {
            true => {
                let tpi = inner_scorer.take_two_phase_iterator()?.unwrap();
                let v = ConstantScoreScorer::with_tpi(self.score, self.score_mode, tpi);
                Ok(ConstantScoreScorerEnum::<LR>::B(v))
            },
            false => {
                let disi = inner_scorer.take_iterator();
                let v = ConstantScoreScorer::with_disi(self.score, self.score_mode, disi);
                Ok(ConstantScoreScorerEnum::<LR>::A(v))
            },
        }
    }

    fn bulk_scorer(&mut self, context: &LeafReaderContext<LR>) -> Result<Option<Self::BulkScorer>> {
        if !self.score_mode.is_exhaustive() {
            let v = self.default_bulk_scorer(context)?;
            return Ok(Some(ConstantScoreBulkScorer::<LR>::A(v)));
        }
        match self.inner_scorer_supplier.bulk_scorer(context)? {
            Some(v) => {
                let v = ConstantBulkScorer::new(v, self.score);
                Ok(Some(ConstantScoreBulkScorer::<LR>::B(v)))
            },
            None => Ok(None),
        }
    }

    fn cost(&mut self, context: &LeafReaderContext<LR>) -> Result<i64> {
        self.inner_scorer_supplier.cost(context)
    }
}
/// We return this as our BulkScorer so that if the CSQ wraps a query with its own optimized top-level scorer (e.g. BooleanScorer) we can use that top-level scorer.
pub struct ConstantBulkScorer<BS>
where
    BS: BulkScorer,
{
    bulk_scorer: BS,
    the_score: f32,
}
impl<BS> ConstantBulkScorer<BS>
where
    BS: BulkScorer,
{
    pub fn new(bulk_scorer: BS, the_score: f32) -> Self {
        Self {
            bulk_scorer,
            the_score,
        }
    }
    fn wrap_collector<LC>(collector: LC, the_score: f32) -> FilterLeafCollectorImpl<LC>
    where
        LC: LeafCollector,
    {
        FilterLeafCollectorImpl::new(collector, the_score)
    }
}
impl<BS> BulkScorer for ConstantBulkScorer<BS>
where
    BS: BulkScorer,
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
        self.bulk_scorer.score(
            &mut Self::wrap_collector(collector, self.the_score),
            accept_docs,
            min,
            max,
        )
    }

    fn cost(&mut self) -> Result<i64> {
        self.bulk_scorer.cost()
    }
}

pub struct FilterLeafCollectorImpl<LC>
where
    LC: LeafCollector,
{
    in_: FilterLeafCollector<LC>,
    the_score: f32,
}

impl<LC> FilterLeafCollectorImpl<LC>
where
    LC: LeafCollector,
{
    pub fn new(in_: LC, the_score: f32) -> Self {
        let base = FilterLeafCollector::new(in_);
        Self {
            in_: base,
            the_score,
        }
    }
}

impl<LC> Display for FilterLeafCollectorImpl<LC>
where
    LC: LeafCollector + Display,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", std::any::type_name::<Self>(), self.in_)
    }
}

impl<LC> LeafCollector for FilterLeafCollectorImpl<LC>
where
    LC: LeafCollector,
{
    fn set_scorer<S>(&mut self, scorer: &mut S) -> Result<()>
    where
        S: Scorable,
    {
        let mut v = FilterScorableImpl::new(self.the_score, scorer);
        self.in_.set_scorer(&mut v)
    }

    fn collect<S>(&mut self, doc: i32, scorer: &mut S) -> Result<()>
    where
        S: Scorable,
    {
        self.in_.collect(doc, scorer)
    }

    fn collect_stream<DS>(&mut self, stream: &mut DS) -> Result<()>
    where
        DS: DocIdStream,
    {
        self.in_.collect_stream(stream)
    }

    type DocIdSetIteratorRef<'b>
        = <FilterLeafCollector<LC> as LeafCollector>::DocIdSetIteratorRef<'b>
    where
        Self: 'b;

    fn competitive_iterator(&mut self) -> Result<Option<Self::DocIdSetIteratorRef<'_>>> {
        self.in_.competitive_iterator()
    }

    fn finish(&mut self) -> Result<()> {
        self.in_.finish()
    }
}

pub struct FilterScorableImpl<'a, S>
where
    S: Scorable,
{
    the_score: f32,
    base: FilterScorable<'a, S>,
}
impl<'a, S> FilterScorableImpl<'a, S>
where
    S: Scorable,
{
    pub(crate) fn new(the_score: f32, s: &'a mut S) -> Self {
        let base = FilterScorable::new(s);
        Self { the_score, base }
    }
}
impl<'a, S> Scorable for FilterScorableImpl<'a, S>
where
    S: Scorable,
{
    fn score(&mut self) -> Result<f32> {
        Ok(self.the_score)
    }

    fn smoothing_score(&mut self, doc_id: i32) -> Result<f32> {
        self.base.smoothing_score(doc_id)
    }

    fn set_min_competitive_score(&mut self, min_score: f32) -> Result<()> {
        self.base.set_min_competitive_score(min_score)
    }

    type Scorable = <FilterScorable<'a, S> as Scorable>::Scorable;

    fn get_children(&self) -> Result<Vec<ChildScorable<Self::Scorable>>> {
        self.base.get_children()
    }

    fn cost(&mut self) -> Result<i64> {
        self.base.cost()
    }
}

pub type QueryBaseWeight<Q, LR, QC> = <Q as QueryBase>::Weight<LR, QC>;

impl<LR> SegmentCacheable<LR> for BaseQueryWeight<LR>
where
    LR: LeafReader,
{
    fn is_cacheable(&self, ctx: &LeafReaderContext<LR>) -> Result<bool> {
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

pub enum BaseQueryWeight<LR>
where
    LR: LeafReader,
{
    Term(TermWeight<LR>),
    MatchAll(MatchAllWeight<LR>),
    PointRange(PointRangeWeight<LR>),
    MatchNoDocs(MatchNoDocsWeight<LR>),
    SortedNumericDocValuesSet(SortedNumericDocValuesSetQueryWeight<LR>),
    SortedNumericDocValuesRange(SortedNumericDocValuesRangeQueryWeight<LR>),
    SortedSetDocValuesRange(SortedSetDocValuesRangeQueryWeight<LR>),
    IndexSortSortedNumericDocValuesRange(IndexSortSortedNumericDocValuesRangeQueryWeight<LR>),
    FieldExists(FieldExistsWeight<LR>),
}

impl<LR> Weight<LR> for BaseQueryWeight<LR>
where
    LR: LeafReader,
{
    type Matches = DummyMatches;

    fn matches(
        &self,
        _context: &LeafReaderContext<LR>,
        _doc: i32,
    ) -> Result<Option<Self::Matches>> {
        todo!()
    }

    fn default_matches(
        &self,
        context: &LeafReaderContext<LR>,
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

    fn explain(&self, context: &LeafReaderContext<LR>, doc: i32) -> Result<Explanation> {
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
        context: &LeafReaderContext<LR>,
    ) -> Result<Option<<Self::ScorerSupplier as ScorerSupplier<LR>>::Scorer>> {
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
        TermSs<LR>,
        MatchAllSs,
        PointRangeSs<LR>,
        MatchNoDocsSs,
        SNDVSQSs<LR>,
        SNDVRQSs<LR>,
        SSDVRQSs<LR>,
        ISSNDVRQSs<LR>,
        FieldExistsESs<LR>,
    >;

    fn scorer_supplier(
        &self,
        context: &LeafReaderContext<LR>,
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
        context: &LeafReaderContext<LR>,
    ) -> Result<Option<<Self::ScorerSupplier as ScorerSupplier<LR>>::BulkScorer>> {
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

    fn count(&self, context: &LeafReaderContext<LR>) -> Result<i32> {
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

    fn default_count(&self, _context: &LeafReaderContext<LR>) -> Result<i32> {
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
#[cfg(test)]
mod tests {
    use crate::core::index::multi_reader::MultiReader;
    use crate::core::search::constant_score_query::ConstantScoreQuery;
    use crate::core::search::match_no_docs_query::MatchNoDocsQuery;
    use crate::core::search::query::{BaseQuery, Query};
    use crate::core::util::error::lucene_error::Result;
    use crate::test::util::lucene_test_case::lucene_test_case_util::new_searcher_with_reader;

    #[test]
    fn test_csq() -> Result<()> {
        // TODO TermRangeQuery未实现
        Ok(())
    }
    #[test]
    fn test_wrapped_2_times() -> Result<()> {
        // TODO BooleanQuery未实现
        Ok(())
    }

    #[test]
    fn test_constant_score_query_and_filter() -> Result<()> {
        // TODO BooleanQuery未实现
        Ok(())
    }
    #[test]
    fn test_propagates_approximations() -> Result<()> {
        // TODO PhraseQuery未实现
        Ok(())
    }
    #[test]
    fn test_rewrite_bubbles_up_match_no_docs_query() -> Result<()> {
        let searcher = new_searcher_with_reader(MultiReader::empty()?)?;
        let query: BaseQuery = MatchNoDocsQuery::new().into();
        let query = ConstantScoreQuery::new(query);
        let rewritten = searcher.rewrite(query.into())?;
        assert_eq!(rewritten, Query::MatchNoDoc(MatchNoDocsQuery::new()));
        Ok(())
    }
}
