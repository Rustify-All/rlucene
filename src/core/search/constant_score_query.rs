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
use crate::core::index::index_reader::Identity;
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::bulk_scorer::BulkScorer;
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::constant_score_weight::ConstantScoreWeight;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_stream::DocIdStream;
use crate::core::search::explanation::Explanation;
use crate::core::search::filter_scorable::FilterScorable;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::leaf_collector::LeafCollector;
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
use crate::core::search::weight::Weight;
use crate::core::util::bits::Bits;
use crate::core::util::core_helper::HasIdentity;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::fmt::{Debug, Display, Formatter};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

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

  pub fn into_inner(self) -> Query {
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
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
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
  fn to_string(&self, field: &str) -> Result<String> {
    let inner = self.query.to_string(field)?;
    Ok(format!("ConstantScore({})", inner))
  }

  fn create_weight<IRC>(
    self,
    searcher: &IndexSearcher<IRC>,
    score_mode: &ScoreMode,
    boost: f32,
  ) -> Result<QueryWeight<IRC>>
  where
    IRC: IndexReaderContext,
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
      Ok(Box::new(ConstantScoreQueryWeight::new(
        boost,
        inner_weight,
        *score_mode,
      )))
    } else {
      Ok(inner_weight)
    }
  }

  fn rewrite<IRC>(mut self, searcher: &IndexSearcher<IRC>) -> Result<Query>
  where
    IRC: IndexReaderContext,
  {
    let query_id = self.query.identity().clone();
    let rewritten = self.query.rewrite(searcher)?;

    let rewritten = match rewritten {
      Query::Boost(b) => b.into_inner(),
      Query::ConstantScore(cs) => cs.into_inner(),
      Query::Boolean(cs) => cs.rewrite_no_scoring()?,
      q => q,
    };

    if let Query::MatchNoDocs(v) = rewritten {
      return Ok(v.into());
    }

    if rewritten.identity() != &query_id {
      return Ok(ConstantScoreQuery::new(rewritten).into());
    }

    self.query = Box::new(rewritten);
    Ok(self.into())
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

pub struct ConstantScoreQueryWeight<IRC> {
  base: ConstantScoreWeight,
  inner_weight: QueryWeight<IRC>,
  score_mode: ScoreMode,
}
impl<IRC> ConstantScoreQueryWeight<IRC>
where
  IRC: IndexReaderContext,
{
  pub fn new(boost: f32, inner_weight: QueryWeight<IRC>, score_mode: ScoreMode) -> Self {
    Self {
      base: ConstantScoreWeight::new(boost),
      inner_weight,
      score_mode,
    }
  }
}
impl<IRC> SegmentCacheable<IRC> for ConstantScoreQueryWeight<IRC>
where
  IRC: IndexReaderContext,
{
  fn is_cacheable(&self, ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    self.inner_weight.is_cacheable(ctx)
  }
}

impl<IRC> Weight<IRC> for ConstantScoreQueryWeight<IRC>
where
  IRC: IndexReaderContext,
{
  fn matches<'a>(
    &'a self,
    context: &'a LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &'a IndexSearcher<IRC>,
  ) -> Result<Option<crate::core::search::query::QueryWeightMatches<'a>>> {
    self.inner_weight.matches(context, doc, searcher)
  }

  fn explain(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Explanation> {
    let scorer = self.scorer(context, searcher)?;
    self
      .base
      .explain(scorer, doc, self.get_query().to_string("")?)
  }

  fn get_query(&self) -> Arc<Query> {
    self.inner_weight.get_query()
  }

  type ScorerSupplier = QueryWeightSs<IRC>;

  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::ScorerSupplier>> {
    match self.inner_weight.scorer_supplier(context, searcher)? {
      Some(inner_scorer_supplier) => Ok(Some(Box::new(ScorerSupplierImpl::new(
        self.score_mode,
        inner_scorer_supplier,
        self.base.score(),
      )))),
      None => Ok(None),
    }
  }

  fn count(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<i32> {
    self.inner_weight.count(context, searcher)
  }
}
pub struct ScorerSupplierImpl<IRC> {
  score_mode: ScoreMode,
  inner_scorer_supplier: QueryWeightSs<IRC>,
  score: f32,
}
impl<IRC> ScorerSupplierImpl<IRC>
where
  IRC: IndexReaderContext,
{
  fn new(score_mode: ScoreMode, inner_scorer_supplier: QueryWeightSs<IRC>, score: f32) -> Self {
    Self {
      score_mode,
      inner_scorer_supplier,
      score,
    }
  }
}
impl<IRC> ScorerSupplier<IRC> for ScorerSupplierImpl<IRC>
where
  IRC: IndexReaderContext,
{
  type Scorer = QueryWeightSsScorer;
  type BulkScorer = QueryWeightSsBulkScorer;

  fn get(
    &mut self,
    lead_cost: i64,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Self::Scorer> {
    let inner_scorer = self
      .inner_scorer_supplier
      .get(lead_cost, context, searcher)?;
    match inner_scorer.has_two_phase_iterator() {
      TwoPhaseState::Yes => {
        let tpi = inner_scorer
          .take_two_phase_iterator()
          .ok_or_else(|| LuceneError::illegal_state("no tpi?"))?;
        let v = ConstantScoreScorer::from_tpi(self.score, self.score_mode, tpi);
        Ok(Box::new(v))
      },
      TwoPhaseState::No => {
        let disi = inner_scorer.take_iterator();
        let v = ConstantScoreScorer::from_disi(self.score, self.score_mode, disi);
        Ok(Box::new(v))
      },
    }
  }

  fn bulk_scorer(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::BulkScorer>> {
    if !self.score_mode.is_exhaustive() {
      let v = self.default_bulk_scorer(context, searcher)?;
      return Ok(Some(Box::new(v)));
    }
    match self.inner_scorer_supplier.bulk_scorer(context, searcher)? {
      Some(v) => {
        let v = ConstantBulkScorer::new(v, self.score);
        Ok(Some(Box::new(v)))
      },
      None => Ok(None),
    }
  }

  fn cost(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<i64> {
    self.inner_scorer_supplier.cost(context, searcher)
  }
}
/// We return this as our BulkScorer so that if the CSQ wraps a query with its own optimized top-level scorer (e.g. BooleanScorer) we can use that top-level scorer.
pub struct ConstantBulkScorer<BS> {
  bulk_scorer: BS,
  the_score: f32,
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
  fn score(
    &mut self,
    collector: &mut dyn LeafCollector,
    accept_docs: Option<&dyn Bits>,
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
  the_score: f32,
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
  fn set_scorer(&mut self, scorer: &mut dyn Scorable) -> Result<()> {
    let mut v = FilterScorableImpl::new(self.the_score, scorer);
    self.in_.set_scorer(&mut v)
  }

  fn collect(&mut self, doc: i32, scorer: &mut dyn Scorable) -> Result<()> {
    let mut v = FilterScorableImpl::new(self.the_score, scorer);
    self.in_.collect(doc, &mut v)
  }

  fn collect_stream(
    &mut self,
    stream: &mut dyn DocIdStream,
    scorer: &mut dyn Scorable,
  ) -> Result<()> {
    let mut v = FilterScorableImpl::new(self.the_score, scorer);
    self.in_.collect_stream(stream, &mut v)
  }

  fn competitive_iterator(&mut self) -> Result<Option<Box<dyn DocIdSetIterator + '_>>> {
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
  the_score: f32,
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
  fn score(&mut self) -> Result<f32> {
    Ok(self.the_score)
  }

  fn smoothing_score(&mut self, doc_id: i32) -> Result<f32> {
    self.base.smoothing_score(doc_id)
  }

  fn set_min_competitive_score(&mut self, min_score: f32) -> Result<()> {
    self.base.set_min_competitive_score(min_score)
  }

  fn get_children(&self) -> Result<Vec<ChildScorable<Box<dyn Scorable>>>> {
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
