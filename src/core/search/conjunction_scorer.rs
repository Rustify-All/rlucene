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

use crate::core::search::approximation_disi::ApproximationBitSetAccess;
use crate::core::search::approximation_disi::ApproximationDISI;
use crate::core::search::conjunction_disi::{
  BitSetConjunctionDISI, ConjunctionDISI, ConjunctionDISIEnum, ConjunctionScorers,
  ConjunctionTwoPhaseIterator, ScorerDisi,
};
use crate::core::search::doc_id_set_iterator::BitSetIteratorAccess;
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, DocIdSetIteratorEnum4};
use crate::core::search::scorable::{ChildScorable, Scorable};
#[cfg(test)]
use crate::core::search::scorer::ScorerKind;
use crate::core::search::scorer::{Scorer, TwoPhaseState};
use crate::core::search::two_phase_iterator::TwoPhaseIterator;
use crate::core::search::two_phase_iterator::TwoPhaseIteratorAsDocIdSetIterator;
use crate::core::util::error::lucene_error::Result;

pub type ConjunctionScorerDisi<S> = DocIdSetIteratorEnum4<
  ConjunctionDISI<ScorerDisi<S>>,
  BitSetConjunctionDISI<ScorerDisi<S>>,
  TwoPhaseIteratorAsDocIdSetIterator<ConjunctionTwoPhaseIterator<ConjunctionDISI<ScorerDisi<S>>>>,
  TwoPhaseIteratorAsDocIdSetIterator<
    ConjunctionTwoPhaseIterator<BitSetConjunctionDISI<ScorerDisi<S>>>,
  >,
>;
// TODO IMPORTANT: Review conjunction performance; it is worse in some scenarios.
/// Scorer for conjunctions, sets of queries, all of which are required.
pub struct ConjunctionScorer<D: DocIdSetIterator> {
  disi: D,
  scoring_idx: Vec<usize>,
}
impl<S: Scorer> ConjunctionScorer<ConjunctionDISIEnum<ScorerDisi<S>>> {
  /// Select the iterator before entering a document loop.
  pub(crate) fn prepare(required: Vec<S>, scorers: &[usize]) -> Result<ConjunctionScorerDisi<S>> {
    debug_assert!({ scorers.iter().all(|v| *v < required.len()) });
    let mut has_tpi = false;
    for v in required.iter() {
      if v.has_two_phase_iterator() == crate::core::search::scorer::TwoPhaseState::Yes {
        has_tpi = true;
        break;
      }
    }
    let v = ConjunctionDISI::from_scorer(required)?;
    match (has_tpi, v) {
      (false, ConjunctionDISIEnum::A(disi)) => Ok(ConjunctionScorerDisi::A(disi)),
      (false, ConjunctionDISIEnum::B(disi)) => Ok(ConjunctionScorerDisi::B(disi)),
      (true, ConjunctionDISIEnum::A(disi)) => Ok(ConjunctionScorerDisi::C(
        TwoPhaseIteratorAsDocIdSetIterator::new(ConjunctionTwoPhaseIterator::new(disi)?),
      )),
      (true, ConjunctionDISIEnum::B(disi)) => Ok(ConjunctionScorerDisi::D(
        TwoPhaseIteratorAsDocIdSetIterator::new(ConjunctionTwoPhaseIterator::new(disi)?),
      )),
    }
  }
}
impl<C: ConjunctionScorers> ConjunctionScorer<C> {
  pub(crate) fn from_disi(disi: C, scoring_idx: Vec<usize>) -> Self {
    Self { disi, scoring_idx }
  }
}
impl<C: ConjunctionScorers>
  ConjunctionScorer<TwoPhaseIteratorAsDocIdSetIterator<ConjunctionTwoPhaseIterator<C>>>
{
  pub(crate) fn from_tpi(
    disi: TwoPhaseIteratorAsDocIdSetIterator<ConjunctionTwoPhaseIterator<C>>,
    scoring_idx: Vec<usize>,
  ) -> Self {
    Self { disi, scoring_idx }
  }
}

/// Access to the conjunction owned by the selected iterator.
#[doc(hidden)]
pub trait ConjunctionState:
  DocIdSetIterator + ApproximationDISI + ApproximationBitSetAccess + 'static
{
  fn match_cost(&self) -> f32;

  fn matches(&mut self) -> Result<bool>;

  type Conjunction: ConjunctionScorers + 'static;
  fn conjunction_mut(&mut self) -> &mut Self::Conjunction;

  fn has_two_phase_iterator(&self) -> TwoPhaseState;
}
impl<S: Scorer> ApproximationDISI for ConjunctionDISI<ScorerDisi<S>> {
  fn approx_doc_id(&self) -> i32 {
    DocIdSetIterator::doc_id(self)
  }
  fn approx_next_doc(&mut self) -> Result<i32> {
    DocIdSetIterator::next_doc(self)
  }
  fn approx_advance(&mut self, target: i32) -> Result<i32> {
    DocIdSetIterator::advance(self, target)
  }
  fn approx_slow_advance(&mut self, target: i32) -> Result<i32> {
    DocIdSetIterator::slow_advance(self, target)
  }
  fn approx_cost(&self) -> Result<i64> {
    DocIdSetIterator::cost(self)
  }
}
impl<S: Scorer> ApproximationBitSetAccess for ConjunctionDISI<ScorerDisi<S>> {
  fn approx_is_bit_iter(&self) -> bool {
    BitSetIteratorAccess::is_bit_iter(self)
  }
  fn approx_get(&self, index: usize) -> Result<bool> {
    BitSetIteratorAccess::get(self, index)
  }
  fn approx_set_doc_id(&mut self, doc: i32) -> Result<()> {
    BitSetIteratorAccess::set_doc_id(self, doc)
  }
  fn approx_bit_set_length(&self) -> Result<usize> {
    BitSetIteratorAccess::bit_set_length(self)
  }
}
impl<S: Scorer> ApproximationDISI for BitSetConjunctionDISI<ScorerDisi<S>> {
  fn approx_doc_id(&self) -> i32 {
    DocIdSetIterator::doc_id(self)
  }
  fn approx_next_doc(&mut self) -> Result<i32> {
    DocIdSetIterator::next_doc(self)
  }
  fn approx_advance(&mut self, target: i32) -> Result<i32> {
    DocIdSetIterator::advance(self, target)
  }
  fn approx_slow_advance(&mut self, target: i32) -> Result<i32> {
    DocIdSetIterator::slow_advance(self, target)
  }
  fn approx_cost(&self) -> Result<i64> {
    DocIdSetIterator::cost(self)
  }
}
impl<S: Scorer> ApproximationBitSetAccess for BitSetConjunctionDISI<ScorerDisi<S>> {
  fn approx_is_bit_iter(&self) -> bool {
    BitSetIteratorAccess::is_bit_iter(self)
  }
  fn approx_get(&self, index: usize) -> Result<bool> {
    BitSetIteratorAccess::get(self, index)
  }
  fn approx_set_doc_id(&mut self, doc: i32) -> Result<()> {
    BitSetIteratorAccess::set_doc_id(self, doc)
  }
  fn approx_bit_set_length(&self) -> Result<usize> {
    BitSetIteratorAccess::bit_set_length(self)
  }
}
impl<C: ConjunctionScorers> ApproximationDISI
  for TwoPhaseIteratorAsDocIdSetIterator<ConjunctionTwoPhaseIterator<C>>
{
  fn approx_doc_id(&self) -> i32 {
    self.two_phase_iterator.approx_doc_id()
  }
  fn approx_next_doc(&mut self) -> Result<i32> {
    self.two_phase_iterator.approx_next_doc()
  }
  fn approx_advance(&mut self, target: i32) -> Result<i32> {
    self.two_phase_iterator.approx_advance(target)
  }
  fn approx_slow_advance(&mut self, target: i32) -> Result<i32> {
    self.two_phase_iterator.approx_slow_advance(target)
  }
  fn approx_cost(&self) -> Result<i64> {
    self.two_phase_iterator.approx_cost()
  }
}
impl<C: ConjunctionScorers> ApproximationBitSetAccess
  for TwoPhaseIteratorAsDocIdSetIterator<ConjunctionTwoPhaseIterator<C>>
{
  fn approx_is_bit_iter(&self) -> bool {
    self.two_phase_iterator.approx_is_bit_iter()
  }
  fn approx_get(&self, index: usize) -> Result<bool> {
    self.two_phase_iterator.approx_get(index)
  }
  fn approx_set_doc_id(&mut self, doc: i32) -> Result<()> {
    self.two_phase_iterator.approx_set_doc_id(doc)
  }
  fn approx_bit_set_length(&self) -> Result<usize> {
    self.two_phase_iterator.approx_bit_set_length()
  }
}
impl<S: Scorer + 'static> ConjunctionState for ConjunctionDISI<ScorerDisi<S>> {
  fn match_cost(&self) -> f32 {
    0.0
  }

  fn matches(&mut self) -> Result<bool> {
    Ok(true)
  }

  type Conjunction = Self;
  fn conjunction_mut(&mut self) -> &mut Self {
    self
  }

  fn has_two_phase_iterator(&self) -> TwoPhaseState {
    TwoPhaseState::No
  }
}
impl<S: Scorer + 'static> ConjunctionState for BitSetConjunctionDISI<ScorerDisi<S>> {
  fn match_cost(&self) -> f32 {
    0.0
  }

  fn matches(&mut self) -> Result<bool> {
    Ok(true)
  }

  type Conjunction = Self;
  fn conjunction_mut(&mut self) -> &mut Self {
    self
  }

  fn has_two_phase_iterator(&self) -> TwoPhaseState {
    TwoPhaseState::No
  }
}
impl<C: ConjunctionScorers + 'static> ConjunctionState
  for TwoPhaseIteratorAsDocIdSetIterator<ConjunctionTwoPhaseIterator<C>>
{
  fn match_cost(&self) -> f32 {
    self.two_phase_iterator.match_cost()
  }

  fn matches(&mut self) -> Result<bool> {
    self.two_phase_iterator.matches()
  }

  type Conjunction = C;
  fn conjunction_mut(&mut self) -> &mut C {
    &mut self.two_phase_iterator.approximation
  }

  fn has_two_phase_iterator(&self) -> TwoPhaseState {
    TwoPhaseState::Yes
  }
}

impl<D: ConjunctionState> Scorable for ConjunctionScorer<D> {
  type Child<'a>
    = crate::core::search::dummy::dummy_scorable::DummyScorable
  where
    Self: 'a;
  #[inline]
  fn score(&mut self) -> Result<f32> {
    let mut sum = 0f64;
    for x in self.scoring_idx.iter() {
      let score = self.disi.conjunction_mut().iterator_at_mut(*x).score()?;
      sum += score as f64;
    }
    Ok(sum as f32)
  }
  fn set_min_competitive_score(&mut self, min_score: f32) -> Result<()> {
    // This scorer is only used for TOP_SCORES when there is a single scoring clause
    if self.scoring_idx.len() == 1 {
      let i = self.scoring_idx[0];
      self
        .disi
        .conjunction_mut()
        .iterator_at_mut(i)
        .set_min_competitive_score(min_score)?;
    }
    Ok(())
  }
  fn get_children(&mut self) -> Result<Vec<ChildScorable<Self::Child<'_>>>> {
    todo!()
  }
  fn cost(&self) -> Result<i64> {
    self.disi.cost()
  }
}
impl<D: DocIdSetIterator> crate::core::search::scorable::FixedScore for ConjunctionScorer<D> {}
impl<D: DocIdSetIterator> DocIdSetIterator for ConjunctionScorer<D> {
  fn doc_id(&self) -> i32 {
    self.disi.doc_id()
  }
  fn next_doc(&mut self) -> Result<i32> {
    self.disi.next_doc()
  }
  fn advance(&mut self, target: i32) -> Result<i32> {
    self.disi.advance(target)
  }
  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    self.disi.slow_advance(target)
  }
  fn cost(&self) -> Result<i64> {
    self.disi.cost()
  }
}
impl<D: DocIdSetIterator> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for ConjunctionScorer<D>
{
}
impl<D: DocIdSetIterator> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for ConjunctionScorer<D>
{
}
impl<D: ConjunctionState> ApproximationDISI for ConjunctionScorer<D> {
  fn approx_doc_id(&self) -> i32 {
    self.disi.approx_doc_id()
  }
  fn approx_next_doc(&mut self) -> Result<i32> {
    self.disi.approx_next_doc()
  }
  fn approx_advance(&mut self, target: i32) -> Result<i32> {
    self.disi.approx_advance(target)
  }
  fn approx_slow_advance(&mut self, target: i32) -> Result<i32> {
    self.disi.approx_slow_advance(target)
  }
  fn approx_cost(&self) -> Result<i64> {
    self.disi.approx_cost()
  }
}
impl<D: ConjunctionState> ApproximationBitSetAccess for ConjunctionScorer<D> {
  fn approx_is_bit_iter(&self) -> bool {
    self.disi.approx_is_bit_iter()
  }
  fn approx_get(&self, index: usize) -> Result<bool> {
    self.disi.approx_get(index)
  }
  fn approx_set_doc_id(&mut self, doc: i32) -> Result<()> {
    self.disi.approx_set_doc_id(doc)
  }
  fn approx_bit_set_length(&self) -> Result<usize> {
    self.disi.approx_bit_set_length()
  }
}

impl<D: ConjunctionState> TwoPhaseIterator for ConjunctionScorer<D> {
  fn matches(&mut self) -> Result<bool> {
    self.disi.matches()
  }
  fn match_cost(&self) -> f32 {
    self.disi.match_cost()
  }
}

impl<D: ConjunctionState> Scorer for ConjunctionScorer<D> {
  fn scoring_doc_id(&mut self) -> Result<i32> {
    Ok(self.disi.doc_id())
  }

  fn advance_shallow(&mut self, target: i32) -> Result<i32> {
    if self.scoring_idx.len() == 1 {
      let i = self.scoring_idx[0];
      return self
        .disi
        .conjunction_mut()
        .iterator_at_mut(i)
        .advance_shallow(target);
    }
    for &idx in &self.scoring_idx {
      self
        .disi
        .conjunction_mut()
        .iterator_at_mut(idx)
        .advance_shallow(target)?;
    }
    self.default_advance_shallow(target)
  }
  fn get_max_score(&mut self, upto: i32) -> Result<f32> {
    let mut max_score = 0f64;
    for &idx in &self.scoring_idx {
      let s = self.disi.conjunction_mut().iterator_at_mut(idx);
      if s.scoring_doc_id()? <= upto {
        max_score += s.get_max_score(upto)? as f64;
      }
    }
    Ok(max_score as f32)
  }
  fn has_two_phase_iterator(&self) -> TwoPhaseState {
    self.disi.has_two_phase_iterator()
  }

  #[cfg(test)]
  fn kind(&self) -> ScorerKind {
    ScorerKind::Conjunction
  }
}
