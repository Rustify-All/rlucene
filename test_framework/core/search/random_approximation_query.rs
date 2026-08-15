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
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::explanation::Explanation;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::query::{IntoBoxQuery, Query, QueryBase, QueryWeight, QueryWeightSs};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::scorable::Scorable;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::{Scorer, TwoPhaseState};
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::two_phase_iterator::{
  TwoPhaseIterator, TwoPhaseIteratorAsDocIdSetIterator,
};
use crate::core::search::weight::{DefaultScorerSupplier, Weight};
use crate::core::util::HasIdentity;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::util::lucene_test_case::random_from_seed;
use rand::Rng;
use rand::RngExt;
use rand::prelude::StdRng;
use std::fmt::Debug;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
/// A Query that adds random approximations to its scorers.
#[derive(Clone, Debug)]
pub struct RandomApproximationQuery {
  id: Identity,
  query: Box<Query>,
  random_seed: u64,
}
impl RandomApproximationQuery {
  pub fn new<Q, R>(query: Q, random: &mut R) -> Self
  where
    Q: IntoBoxQuery,
    R: Rng + ?Sized,
  {
    let query = query.into_box_query();
    let random_seed = random.random();
    Self {
      id: Identity::new(),
      query,
      random_seed,
    }
  }
}
impl Hash for RandomApproximationQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.query.hash(state);
  }
}
impl Eq for RandomApproximationQuery {}

impl PartialEq for RandomApproximationQuery {
  fn eq(&self, other: &Self) -> bool {
    self.query.eq(&other.query)
  }
}

impl HasIdentity for RandomApproximationQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl QueryBase for RandomApproximationQuery {
  fn to_string(&self, field: &str) -> Result<String> {
    self.query.to_string(field)
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
    let query = self.clone();
    let weight = self.query.create_weight(searcher, score_mode, boost)?;
    let mut random = random_from_seed(self.random_seed);
    Ok(Box::new(RandomApproximationWeight::new(
      query,
      random.random(),
      weight,
    )))
  }

  fn rewrite<IRC>(self, _searcher: &IndexSearcher<IRC>) -> Result<Query>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    let query_id = self.query.identity().clone();
    let rewritten = self.query.rewrite(_searcher)?;
    if query_id != *rewritten.identity() {
      let mut random = random_from_seed(self.random_seed);
      Ok(RandomApproximationQuery::new(rewritten, &mut random).into())
    } else {
      Ok(rewritten)
    }
  }

  fn visit<QV>(&self, visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    self.query.visit(visitor)
  }
}

pub struct RandomApproximationWeight<LR> {
  query: Arc<Query>,
  random_seed: u64,
  in_: QueryWeight<LR>,
}
impl<LR> RandomApproximationWeight<LR>
where
  LR: IndexReaderContext + 'static,
  IRCLeafReader<LR>: 'static,
{
  fn new(query: RandomApproximationQuery, random_seed: u64, weight: QueryWeight<LR>) -> Self {
    let query = Arc::new(query.into());
    Self {
      query,
      random_seed,
      in_: weight,
    }
  }
}

impl<LR> SegmentCacheable<LR> for RandomApproximationWeight<LR>
where
  LR: IndexReaderContext + 'static,
  IRCLeafReader<LR>: 'static,
{
  fn is_cacheable(&self, ctx: &LeafReaderContext<IRCLeafReader<LR>>) -> Result<bool> {
    self.in_.is_cacheable(ctx)
  }
}

impl<LR> Weight<LR> for RandomApproximationWeight<LR>
where
  LR: IndexReaderContext + 'static,
  IRCLeafReader<LR>: 'static,
{
  fn matches<'a>(
    &'a self,
    context: &'a LeafReaderContext<IRCLeafReader<LR>>,
    doc: i32,
    searcher: &'a IndexSearcher<LR>,
  ) -> Result<Option<crate::core::search::query::QueryWeightMatches<'a>>> {
    self.in_.matches(context, doc, searcher)
  }

  fn explain(
    &self,
    context: &LeafReaderContext<IRCLeafReader<LR>>,
    doc: i32,
    searcher: &IndexSearcher<LR>,
  ) -> Result<Explanation> {
    self.in_.explain(context, doc, searcher)
  }

  fn get_query(&self) -> Arc<Query> {
    self.in_.get_query()
  }

  type ScorerSupplier = QueryWeightSs<LR>;

  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<LR>>,
    searcher: &IndexSearcher<LR>,
  ) -> Result<Option<Self::ScorerSupplier>> {
    let scorer_supplier = self.in_.scorer_supplier(context, searcher)?;

    if let Some(mut scorer_supplier) = scorer_supplier {
      let sub_scorer = scorer_supplier.get(i64::MAX, context, searcher)?;
      let mut random = random_from_seed(self.random_seed);
      let scorer = RandomApproximationScorer::new(random.random(), sub_scorer);

      Ok(Some(Box::new(DefaultScorerSupplier::new(scorer))))
    } else {
      Ok(None)
    }
  }
}

pub struct RandomApproximationScorer<S> {
  random_seed: u64,
  two_phase_view: RandomTwoPhaseView<ScorerDISI<S>>,
}
impl<S> RandomApproximationScorer<S>
where
  S: Scorer,
{
  pub fn new(random_seed: u64, scorer: S) -> Self {
    let disi = ScorerDISI::new(scorer);
    let mut random = random_from_seed(random_seed);
    let two_phase_view = RandomTwoPhaseView::new(&mut random, disi);
    Self {
      random_seed,
      two_phase_view,
    }
  }
}

impl<S> Scorable for RandomApproximationScorer<S>
where
  S: Scorer + 'static,
{
  fn score(&mut self) -> Result<f32> {
    self.two_phase_view.disi_mut().scorer.score()
  }

  fn cost(&self) -> Result<i64> {
    self.iterator().cost()
  }
}

impl<S> crate::core::search::scorable::FixedScore for RandomApproximationScorer<S> where
  S: Scorer + 'static
{
}

impl<S> Scorer for RandomApproximationScorer<S>
where
  S: Scorer + 'static,
{
  fn doc_id(&mut self) -> Result<i32> {
    Ok(self.two_phase_view.approximation().doc_id())
  }

  fn iterator(&self) -> Box<dyn DocIdSetIterator + '_> {
    Box::new(TwoPhaseIteratorAsDocIdSetIterator::new(
      &self.two_phase_view,
    ))
  }

  fn iterator_mut(&mut self) -> Box<dyn DocIdSetIterator + '_> {
    Box::new(TwoPhaseIteratorAsDocIdSetIterator::new(
      &mut self.two_phase_view,
    ))
  }

  fn take_iterator(self: Box<Self>) -> Box<dyn DocIdSetIterator> {
    Box::new(TwoPhaseIteratorAsDocIdSetIterator::new(self.two_phase_view))
  }

  fn two_phase_iterator(&self) -> Option<Box<dyn TwoPhaseIterator + '_>> {
    Some(Box::new(&self.two_phase_view))
  }

  fn two_phase_iterator_mut(&mut self) -> Option<Box<dyn TwoPhaseIterator + '_>> {
    Some(Box::new(&mut self.two_phase_view))
  }

  fn take_two_phase_iterator(self: Box<Self>) -> Option<Box<dyn TwoPhaseIterator>> {
    Some(Box::new(self.two_phase_view))
  }

  fn advance_shallow(&mut self, mut target: i32) -> Result<i32> {
    let scorer_doc = self.two_phase_view.disi().doc_id();
    let approx_doc = self.two_phase_view.approximation().doc_id();
    if scorer_doc > target && approx_doc != scorer_doc {
      // The random approximation can return doc ids that are not present in the underlying
      // scorer. These additional doc ids are always *before* the next matching doc so we
      // cannot use them to shallow advance the main scorer which is already ahead.
      target = scorer_doc;
    }
    self
      .two_phase_view
      .disi_mut()
      .scorer
      .advance_shallow(target)
  }

  fn get_max_score(&mut self, up_to: i32) -> Result<f32> {
    self.two_phase_view.disi_mut().scorer.get_max_score(up_to)
  }

  fn has_two_phase_iterator(&self) -> TwoPhaseState {
    TwoPhaseState::Yes
  }

  fn approximation(&self) -> Box<dyn DocIdSetIterator + '_> {
    self.two_phase_view.approximation()
  }

  fn approximation_mut(&mut self) -> Box<dyn DocIdSetIterator + '_> {
    self.two_phase_view.approximation_mut()
  }
}

pub struct RandomTwoPhaseView<DISI> {
  approximation: RandomApproximation<StdRng, DISI>,
  last_doc: i32,
  random_match_cost: f32,
}
impl<DISI> RandomTwoPhaseView<DISI>
where
  DISI: DocIdSetIterator,
{
  pub fn new<R>(random: &mut R, disi: DISI) -> Self
  where
    R: Rng + ?Sized,
  {
    let seed = random.random();
    let new_random = random_from_seed(seed);
    let random_approximation = RandomApproximation::new(new_random, disi);
    Self {
      approximation: random_approximation,
      last_doc: -1,
      random_match_cost: random.random::<f32>() * 200f32,
    }
  }
  pub fn disi(&self) -> &DISI {
    &self.approximation.disi
  }
  pub fn disi_mut(&mut self) -> &mut DISI {
    &mut self.approximation.disi
  }
}
impl<DISI> TwoPhaseIterator for RandomTwoPhaseView<DISI>
where
  DISI: DocIdSetIterator,
{
  fn approximation_mut(&mut self) -> Box<dyn DocIdSetIterator + '_> {
    Box::new(&mut self.approximation)
  }

  fn approximation(&self) -> Box<dyn DocIdSetIterator + '_> {
    Box::new(&self.approximation)
  }

  fn matches(&mut self) -> Result<bool> {
    let approx_doc = self.approximation.doc_id();

    if approx_doc == -1 || approx_doc == NO_MORE_DOCS {
      return Err(LuceneError::illegal_state(format!(
        "matches() should not be called on doc ID {}",
        approx_doc
      )));
    }

    if self.last_doc == approx_doc {
      return Err(LuceneError::illegal_state(format!(
        "matches() has been called twice on doc ID {}",
        approx_doc
      )));
    }
    self.last_doc = approx_doc;
    Ok(approx_doc == self.approximation.disi.doc_id())
  }

  fn match_cost(&self) -> f32 {
    self.random_match_cost
  }
}
pub struct RandomApproximation<RNG, DISI> {
  random: RNG,
  disi: DISI,
  doc: i32,
}

impl<RNG, DISI> RandomApproximation<RNG, DISI>
where
  RNG: Rng,
  DISI: DocIdSetIterator,
{
  pub fn new(random: RNG, disi: DISI) -> Self {
    Self {
      random,
      disi,
      doc: -1,
    }
  }
}

impl<RNG, DISI> DocIdSetIterator for RandomApproximation<RNG, DISI>
where
  RNG: Rng,
  DISI: DocIdSetIterator,
{
  fn doc_id(&self) -> i32 {
    self.doc
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.advance(self.doc + 1)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    if self.disi.doc_id() < target {
      self.disi.advance(target)?;
    }
    let disi_doc = self.disi.doc_id();
    if disi_doc == NO_MORE_DOCS {
      self.doc = NO_MORE_DOCS;
      return Ok(self.doc);
    }

    let picked = self.random.random_range(target..=disi_doc);
    self.doc = picked;
    Ok(self.doc)
  }

  fn cost(&self) -> Result<i64> {
    self.disi.cost()
  }
}

pub struct ScorerDISI<S> {
  scorer: S,
}
impl<S> ScorerDISI<S>
where
  S: Scorer,
{
  pub fn new(scorer: S) -> Self {
    Self { scorer }
  }
}
impl<S> DocIdSetIterator for ScorerDISI<S>
where
  S: Scorer,
{
  fn doc_id(&self) -> i32 {
    self.scorer.iterator().doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.scorer.iterator().next_doc()
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.scorer.iterator().advance(target)
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    self.scorer.iterator().slow_advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self.scorer.iterator().cost()
  }
}

impl crate::core::util::accountable::Accountable for RandomApproximationQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED)
  }
}
