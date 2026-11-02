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
use crate::core::search::query::QueryRef;

use crate::core::index::index_reader::Identity;
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::bulk_scorer::BulkScorer;
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::constant_score_weight::ConstantScoreWeight;
use crate::core::search::doc_id_stream::DocIdStream;
use crate::core::search::explanation::Explanation;
use crate::core::search::filter_scorable::FilterScorable;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::leaf_collector::LeafCollector;
use crate::core::search::query::QueryWeightMatches;
use crate::core::search::query::{
  IntoBoxQuery, Query, QueryBase, QueryWeight, QueryWeightSs, QueryWeightSsBulkScorer,
  QueryWeightSsScorer,
};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::scorable::{ChildScorable, Scorable};
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::{Scorer, TwoPhaseState};
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::weight::Weight;
use crate::core::util::bits::Bits;
use crate::core::util::core_helper::HasIdentity;
use crate::core::util::error::lucene_error::Result;
use std::borrow::Cow;
use std::fmt::{Debug, Display, Formatter};
use std::hash::{Hash, Hasher};

/// A query that wraps another query and simply returns a constant score equal to 1 for every document that matches the query.
/// It therefore simply strips of all scores and always returns 1.
#[derive(Debug, Clone)]
pub struct ConstantScoreQuery {
  id: Identity,
  query: Box<Query>,
}
impl ConstantScoreQuery {
  /// Strips off scores from the passed in Query. The hits will get a constant score of 1.
  pub fn new<T>(query: T) -> Self
  where
    T: IntoBoxQuery,
  {
    let query = query.into_box_query();
    Self {
      id: Identity::new(),
      query,
    }
  }

  pub fn get_query(&self) -> &Query {
    &self.query
  }

  pub fn into_inner(self) -> Query {
    *self.query
  }

  pub(crate) fn rewrite_inner<IRC, SIM>(
    query: &Query,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Option<Query>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync,
  {
    let rewritten = query.rewrite(searcher);
    // Inspect the borrowed result before moving its large Query payload.
    if matches!(&rewritten, Ok(None)) && matches!(query, Query::Term(_)) {
      return Ok(None);
    }
    let rewritten = match rewritten? {
      Some(rewritten) => Cow::Owned(rewritten),
      None => Cow::Borrowed(query),
    };

    // Do some extra simplifications that are legal since scores are not needed on the wrapped
    // query.
    let rewritten = match rewritten {
      Cow::Owned(Query::Boost(boost_query)) => Cow::Owned(boost_query.into_inner()),
      Cow::Borrowed(Query::Boost(boost_query)) => Cow::Borrowed(boost_query.get_query()),

      Cow::Owned(Query::ConstantScore(constant_score_query)) => {
        Cow::Owned(constant_score_query.into_inner())
      },
      Cow::Borrowed(Query::ConstantScore(constant_score_query)) => {
        Cow::Borrowed(constant_score_query.get_query())
      },

      Cow::Owned(Query::Boolean(boolean_query)) => match boolean_query.rewrite_no_scoring()? {
        Some(rewritten) => Cow::Owned(rewritten),
        None => Cow::Owned(Query::Boolean(boolean_query)),
      },
      Cow::Borrowed(Query::Boolean(boolean_query)) => match boolean_query.rewrite_no_scoring()? {
        Some(rewritten) => Cow::Owned(rewritten),
        None => Cow::Borrowed(query),
      },
      rewritten => rewritten,
    };

    if matches!(rewritten.as_ref(), Query::MatchNoDocs(_)) {
      // bubble up MatchNoDocsQuery
      return Ok(Some(rewritten.into_owned()));
    }

    if rewritten.identity() != query.identity() {
      return Ok(Some(ConstantScoreQuery::new(rewritten.into_owned()).into()));
    }

    if matches!(rewritten.as_ref(), Query::ConstantScore(_)) {
      return Ok(Some(rewritten.into_owned()));
    }

    if let Query::Boost(boost_query) = rewritten.as_ref() {
      return Ok(Some(
        ConstantScoreQuery::new(boost_query.get_query().clone()).into(),
      ));
    }

    Ok(None)
  }
}
impl Eq for ConstantScoreQuery {}

impl PartialEq<Self> for ConstantScoreQuery {
  fn eq(&self, other: &Self) -> bool {
    self.query == other.query
  }
}

impl Hash for ConstantScoreQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    std::any::type_name::<Self>().hash(state);
    self.query.hash(state);
  }
}

impl HasIdentity for ConstantScoreQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}
impl QueryBase for ConstantScoreQuery {
  type Weight<IRC, SIM>
    = ConstantScoreQueryWeightResult<IRC, SIM>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static;

  fn to_string(&self, field: &str) -> Result<String> {
    let mut inner = self.query.to_string(field)?;
    inner.reserve("ConstantScore()".len());
    inner.insert_str(0, "ConstantScore(");
    inner.push(')');
    Ok(inner)
  }

  fn create_weight<IRC, SIM>(
    self,
    searcher: &IndexSearcher<IRC, SIM>,
    score_mode: &ScoreMode,
    boost: f32,
  ) -> Result<Self::Weight<IRC, SIM>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync,
    Self: Sized,
  {
    let inner_score_mode = if score_mode.is_exhaustive() {
      ScoreMode::CompleteNoScores
    } else {
      ScoreMode::TopDocs
    };
    let query = *self.query;
    let inner_weight = searcher.create_weight(query, inner_score_mode, 1.0)?;
    if score_mode.needs_scores() {
      Ok(ConstantScoreQueryWeightResult::Scoring(
        ConstantScoreQueryWeight::new(boost, inner_weight, *score_mode),
      ))
    } else {
      Ok(ConstantScoreQueryWeightResult::Inner(inner_weight))
    }
  }

  fn rewrite<IRC, SIM>(&self, searcher: &IndexSearcher<IRC, SIM>) -> Result<Option<Query>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync,
  {
    Self::rewrite_inner(self.get_query(), searcher)
  }

  fn visit<QV>(&self, visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    let query = self.into();
    let mut visitor = visitor.get_sub_visitor(Occur::Filter, query);
    self.query.visit(&mut visitor)
  }
}

pub struct ConstantScoreQueryWeight<
  IRC: IndexReaderContext + 'static,
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> where
  SIM: Similarity,
{
  base: ConstantScoreWeight,
  inner_weight: QueryWeight<IRC, SIM>,
  score_mode: ScoreMode,
}
impl<IRC, SIM> ConstantScoreQueryWeight<IRC, SIM>
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
{
  pub fn new(boost: f32, inner_weight: QueryWeight<IRC, SIM>, score_mode: ScoreMode) -> Self {
    Self {
      base: ConstantScoreWeight::new(boost),
      inner_weight,
      score_mode,
    }
  }
}
impl<IRC, SIM> SegmentCacheable<IRC> for ConstantScoreQueryWeight<IRC, SIM>
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
{
  fn is_cacheable(&self, ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    self.inner_weight.is_cacheable(ctx)
  }
}

impl<IRC, SIM> Weight<IRC, SIM> for ConstantScoreQueryWeight<IRC, SIM>
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
{
  fn matches<'a>(
    &'a self,
    context: &'a LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &'a IndexSearcher<IRC, SIM>,
  ) -> Result<Option<crate::core::search::query::QueryWeightMatches<'a, IRC, SIM>>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    self.inner_weight.matches(context, doc, searcher)
  }

  fn explain(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Explanation>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let scorer = self.scorer(context, searcher)?;
    self
      .base
      .explain(scorer, doc, self.get_query().to_string("")?)
  }

  fn get_query(&self) -> QueryRef<'_> {
    self.inner_weight.get_query()
  }

  type ScorerSupplier = ScorerSupplierImpl<IRC, SIM>;

  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Option<Self::ScorerSupplier>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    match self.inner_weight.scorer_supplier(context, searcher)? {
      Some(inner_scorer_supplier) => Ok(Some(ScorerSupplierImpl::new(
        self.score_mode,
        inner_scorer_supplier,
        self.base.score(),
      ))),
      None => Ok(None),
    }
  }

  fn count(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<i32>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    self.inner_weight.count(context, searcher)
  }
}
pub struct ScorerSupplierImpl<
  IRC: IndexReaderContext + 'static,
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> where
  SIM: Similarity,
{
  score_mode: ScoreMode,
  inner_scorer_supplier: QueryWeightSs<IRC, SIM>,
  score: f32,
}
impl<IRC, SIM> ScorerSupplierImpl<IRC, SIM>
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
{
  fn new(
    score_mode: ScoreMode,
    inner_scorer_supplier: QueryWeightSs<IRC, SIM>,
    score: f32,
  ) -> Self {
    Self {
      score_mode,
      inner_scorer_supplier,
      score,
    }
  }
}
impl<IRC, SIM> ScorerSupplier<IRC, SIM> for ScorerSupplierImpl<IRC, SIM>
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
{
  type Scorer = QueryWeightSsScorer<IRC, SIM>;
  type BulkScorer = crate::core::search::bulk_scorer::BulkScorerEnum2<
    crate::core::search::weight::DefaultBulkScorer<QueryWeightSsScorer<IRC, SIM>>,
    ConstantBulkScorer<QueryWeightSsBulkScorer<IRC, SIM>>,
  >;

  fn get(
    &mut self,
    lead_cost: i64,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Self::Scorer>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let inner_scorer = self
      .inner_scorer_supplier
      .get(lead_cost, context, searcher)?;
    match inner_scorer.has_two_phase_iterator() {
      TwoPhaseState::Yes => {
        let tpi = inner_scorer;
        let v = ConstantScoreScorer::from_tpi(self.score, self.score_mode, tpi);
        Ok(QueryWeightSsScorer::<IRC, SIM>::new(v))
      },
      TwoPhaseState::No => {
        let disi = inner_scorer;
        let v = ConstantScoreScorer::from_disi(self.score, self.score_mode, disi);
        Ok(QueryWeightSsScorer::<IRC, SIM>::new(v))
      },
    }
  }

  fn bulk_scorer(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Option<Self::BulkScorer>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    if !self.score_mode.is_exhaustive() {
      let v = self.default_bulk_scorer(context, searcher)?;
      return Ok(Some(crate::core::search::bulk_scorer::BulkScorerEnum2::A(
        v,
      )));
    }
    match self.inner_scorer_supplier.bulk_scorer(context, searcher)? {
      Some(v) => {
        let v = ConstantBulkScorer::new(v, self.score);
        Ok(Some(crate::core::search::bulk_scorer::BulkScorerEnum2::B(
          v,
        )))
      },
      None => Ok(None),
    }
  }

  fn cost(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<i64>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    self.inner_scorer_supplier.cost(context, searcher)
  }
}
/// We return this as our BulkScorer so that if the CSQ wraps a query with its own optimized top-level scorer (e.g. BooleanScorer) we can use that top-level scorer.
pub struct ConstantBulkScorer<BS> {
  pub(crate) bulk_scorer: BS,
  pub(crate) the_score: f32,
}
impl<BS> ConstantBulkScorer<BS> {
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
  fn score<CLC: crate::core::search::leaf_collector::LeafCollector, AB: Bits + ?Sized>(
    &mut self,
    collector: &mut CLC,
    accept_docs: Option<&AB>,
    min: i32,
    max: i32,
  ) -> Result<i32> {
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

pub struct FilterLeafCollectorImpl<LC> {
  in_: LC,
  pub(crate) the_score: f32,
}

impl<LC> FilterLeafCollectorImpl<LC> {
  pub fn new(in_: LC, the_score: f32) -> Self {
    Self { in_, the_score }
  }
}

impl<LC> Display for FilterLeafCollectorImpl<LC>
where
  LC: Display,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{} ({})", std::any::type_name::<Self>(), self.in_)
  }
}

impl<LC> LeafCollector for FilterLeafCollectorImpl<LC>
where
  LC: LeafCollector,
{
  type CompetitiveIterator<'a>
    = LC::CompetitiveIterator<'a>
  where
    Self: 'a;

  fn set_scorer<SC: Scorable + ?Sized>(&mut self, scorer: &mut SC) -> Result<()> {
    let mut v = FilterScorableImpl::new(self.the_score, scorer);
    self.in_.set_scorer(&mut v)
  }

  fn collect<SC: Scorable + ?Sized>(&mut self, doc: i32, scorer: &mut SC) -> Result<()> {
    let mut v = FilterScorableImpl::new(self.the_score, scorer);
    self.in_.collect(doc, &mut v)
  }

  fn collect_stream<DS: DocIdStream + ?Sized, SC: Scorable + ?Sized>(
    &mut self,
    stream: &mut DS,
    scorer: &mut SC,
  ) -> Result<()> {
    let mut v = FilterScorableImpl::new(self.the_score, scorer);
    self.in_.collect_stream(stream, &mut v)
  }

  fn competitive_iterator(&mut self) -> Result<Option<Self::CompetitiveIterator<'_>>> {
    self.in_.competitive_iterator()
  }

  fn finish(&mut self) -> Result<()> {
    self.in_.finish()
  }
}

pub struct FilterScorableImpl<'a, S>
where
  S: ?Sized,
{
  pub(crate) the_score: f32,
  base: FilterScorable<'a, S>,
}
impl<'a, S> FilterScorableImpl<'a, S>
where
  S: ?Sized,
{
  pub(crate) fn new(the_score: f32, s: &'a mut S) -> Self {
    let base = FilterScorable::new(s);
    Self { the_score, base }
  }
}
impl<'a, S> Scorable for FilterScorableImpl<'a, S>
where
  S: Scorable + ?Sized,
{
  type Child<'b>
    = <FilterScorable<'a, S> as Scorable>::Child<'b>
  where
    Self: 'b;

  fn score(&mut self) -> Result<f32> {
    Ok(self.the_score)
  }

  fn smoothing_score(&mut self, doc_id: i32) -> Result<f32> {
    self.base.smoothing_score(doc_id)
  }

  fn set_min_competitive_score(&mut self, min_score: f32) -> Result<()> {
    self.base.set_min_competitive_score(min_score)
  }

  fn get_children(&mut self) -> Result<Vec<ChildScorable<Self::Child<'_>>>> {
    self.base.get_children()
  }

  fn cost(&self) -> Result<i64> {
    self.base.cost()
  }
}

impl<S> crate::core::search::scorable::FixedScore for FilterScorableImpl<'_, S> where S: ?Sized {}
impl crate::core::util::accountable::Accountable for ConstantScoreQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED)
  }
}

/// Weight returned by the actual branches of this query factory.
pub enum ConstantScoreQueryWeightResult<
  IRC: IndexReaderContext + 'static,
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> where
  SIM: Similarity,
{
  Scoring(ConstantScoreQueryWeight<IRC, SIM>),
  Inner(QueryWeight<IRC, SIM>),
}
impl<IRC: IndexReaderContext + 'static, SIM> SegmentCacheable<IRC>
  for ConstantScoreQueryWeightResult<IRC, SIM>
where
  SIM: Similarity,
{
  fn is_cacheable(&self, context: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    match self {
      Self::Scoring(inner) => SegmentCacheable::<IRC>::is_cacheable(inner, context),
      Self::Inner(inner) => SegmentCacheable::<IRC>::is_cacheable(inner, context),
    }
  }
}
impl<IRC: IndexReaderContext + 'static, SIM> Weight<IRC, SIM>
  for ConstantScoreQueryWeightResult<IRC, SIM>
where
  SIM: Similarity,
{
  type ScorerSupplier = QueryWeightSs<IRC, SIM>;
  fn matches<'a>(
    &'a self,
    context: &'a LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &'a IndexSearcher<IRC, SIM>,
  ) -> Result<Option<QueryWeightMatches<'a, IRC, SIM>>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    match self {
      Self::Scoring(inner) => Weight::<IRC, SIM>::matches(inner, context, doc, searcher),
      Self::Inner(inner) => Weight::<IRC, SIM>::matches(inner, context, doc, searcher),
    }
  }
  fn explain(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Explanation>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    match self {
      Self::Scoring(inner) => Weight::<IRC, SIM>::explain(inner, context, doc, searcher),
      Self::Inner(inner) => Weight::<IRC, SIM>::explain(inner, context, doc, searcher),
    }
  }
  fn get_query(&self) -> QueryRef<'_> {
    match self {
      Self::Scoring(inner) => Weight::<IRC, SIM>::get_query(inner),
      Self::Inner(inner) => Weight::<IRC, SIM>::get_query(inner),
    }
  }
  fn scorer(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Option<QueryWeightSsScorer<IRC, SIM>>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    match self {
      Self::Scoring(inner) => Weight::<IRC, SIM>::scorer(inner, context, searcher),
      Self::Inner(inner) => Weight::<IRC, SIM>::scorer(inner, context, searcher),
    }
  }
  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Option<Self::ScorerSupplier>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    match self {
      Self::Scoring(inner) => Weight::<IRC, SIM>::scorer_supplier(inner, context, searcher)
        .map(|supplier| supplier.map(Into::into)),
      Self::Inner(inner) => Weight::<IRC, SIM>::scorer_supplier(inner, context, searcher),
    }
  }
  fn bulk_scorer(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Option<QueryWeightSsBulkScorer<IRC, SIM>>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    match self {
      Self::Scoring(inner) => Weight::<IRC, SIM>::bulk_scorer(inner, context, searcher)
        .map(|scorer| scorer.map(QueryWeightSsBulkScorer::<IRC, SIM>::new)),
      Self::Inner(inner) => Weight::<IRC, SIM>::bulk_scorer(inner, context, searcher),
    }
  }
  fn count(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<i32>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    match self {
      Self::Scoring(inner) => Weight::<IRC, SIM>::count(inner, context, searcher),
      Self::Inner(inner) => Weight::<IRC, SIM>::count(inner, context, searcher),
    }
  }
  fn default_count(&self, context: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<i32> {
    match self {
      Self::Scoring(inner) => Weight::<IRC, SIM>::default_count(inner, context),
      Self::Inner(inner) => Weight::<IRC, SIM>::default_count(inner, context),
    }
  }
  fn is_cache_wrapper(&self) -> bool {
    match self {
      Self::Scoring(inner) => Weight::<IRC, SIM>::is_cache_wrapper(inner),
      Self::Inner(inner) => Weight::<IRC, SIM>::is_cache_wrapper(inner),
    }
  }
  fn into_inner_weight(self: Box<Self>) -> Option<QueryWeight<IRC, SIM>> {
    match *self {
      Self::Scoring(_) => None,
      Self::Inner(inner) => inner.into_inner_weight(),
    }
  }
  #[cfg(test)]
  fn as_any(&mut self) -> &mut dyn std::any::Any {
    match self {
      Self::Scoring(inner) => Weight::<IRC, SIM>::as_any(inner),
      Self::Inner(inner) => Weight::<IRC, SIM>::as_any(inner),
    }
  }
}
impl<IRC: IndexReaderContext + 'static, SIM> From<ConstantScoreQueryWeightResult<IRC, SIM>>
  for QueryWeight<IRC, SIM>
where
  SIM: Similarity,
{
  fn from(value: ConstantScoreQueryWeightResult<IRC, SIM>) -> Self {
    match value {
      ConstantScoreQueryWeightResult::Scoring(inner) => inner.into(),
      ConstantScoreQueryWeightResult::Inner(inner) => inner,
    }
  }
}
