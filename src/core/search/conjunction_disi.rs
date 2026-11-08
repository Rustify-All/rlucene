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
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, DocIdSetIteratorEnum2};
use crate::core::search::scorable::{ChildScorable, Scorable};
use crate::core::search::scorer::Scorer;
use crate::core::search::two_phase_iterator::TwoPhaseIterator;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bit_set_iterator::BitSetIterator;
use crate::core::util::collection_util::CollectionUtil;
use crate::core::util::core_helper::CoreHelper;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{Comparator, ToInt, TryIntoInt};
use smallvec::SmallVec;

/// The two conjunction implementations selected by iterator cost.
pub type ConjunctionDISIEnum<D> =
  DocIdSetIteratorEnum2<ConjunctionDISI<D>, BitSetConjunctionDISI<D>>;

pub type ConjunctionDISIWithBitSet<D, B> =
  ConjunctionDISIEnum<DocIdSetIteratorEnum2<D, BitSetIterator<B>>>;

impl<D> ConjunctionDISIEnum<D> {
  pub(crate) fn len(&self) -> usize {
    match self {
      Self::A(conjunction) => conjunction.all_disi.len(),
      Self::B(conjunction) => conjunction.iterator_locations.len(),
    }
  }

  pub(crate) fn iterator_at(&self, index: usize) -> &D {
    match self {
      Self::A(conjunction) => &conjunction.all_disi[index],
      Self::B(conjunction) => match conjunction.iterator_locations[index] {
        IteratorLocation::Lead(index) => match &conjunction.lead {
          DocIdSetIteratorEnum2::A(iterator) => iterator,
          DocIdSetIteratorEnum2::B(lead) => &lead.all_disi[index],
        },
        IteratorLocation::BitSet(index) => &conjunction.bit_set_iterators[index],
      },
    }
  }

  pub(crate) fn iterator_at_mut(&mut self, index: usize) -> &mut D {
    match self {
      Self::A(conjunction) => &mut conjunction.all_disi[index],
      Self::B(conjunction) => match conjunction.iterator_locations[index] {
        IteratorLocation::Lead(index) => match &mut conjunction.lead {
          DocIdSetIteratorEnum2::A(iterator) => iterator,
          DocIdSetIteratorEnum2::B(lead) => &mut lead.all_disi[index],
        },
        IteratorLocation::BitSet(index) => &mut conjunction.bit_set_iterators[index],
      },
    }
  }
}

impl<D, B> ConjunctionDISIWithBitSet<D, B> {
  pub(crate) fn non_bit_set_iterator(&self) -> Result<&D> {
    match self.iterator_at(0) {
      DocIdSetIteratorEnum2::A(iterator) => Ok(iterator),
      DocIdSetIteratorEnum2::B(_) => Err(LuceneError::illegal_state(
        "Expected the non-bit-set iterator first in the conjunction",
      )),
    }
  }
}

pub struct ConjunctionDISI<D> {
  lead1: usize,
  lead2: usize,
  others: Vec<usize>,
  pub(crate) all_disi: Vec<D>,
}
impl<S> ConjunctionDISI<ScorerDisi<S>>
where
  S: Scorer,
{
  pub(crate) fn from_scorer(scorers: Vec<S>) -> Result<ConjunctionDISIEnum<ScorerDisi<S>>> {
    let iters = scorers.into_iter().map(ScorerDisi::new).collect();
    Self::create_conjunction(iters)
  }
}
impl<D> ConjunctionDISI<D>
where
  D: DocIdSetIterator,
{
  /// Selects the conjunction implementation by cost.
  /// Minimum-cost bit sets remain eligible to lead iteration.
  pub(crate) fn create_conjunction(iterators: Vec<D>) -> Result<ConjunctionDISIEnum<D>> {
    debug_assert!(iterators.len() >= 2);
    let cur_doc = iterators[0].doc_id();
    let mut min_cost = i64::MAX;
    for iterator in &iterators {
      if iterator.doc_id() != cur_doc {
        return Err(LuceneError::illegal_argument(
          "Sub-iterators of ConjunctionDISI are not on the same document!",
        ));
      }
      min_cost = min_cost.min(iterator.cost()?);
    }

    let mut first_bit_set = None;
    for (index, iterator) in iterators.iter().enumerate() {
      if iterator.is_bit_iter() && iterator.cost()? > min_cost {
        first_bit_set = Some(index);
        break;
      }
    }
    let Some(first_bit_set) = first_bit_set else {
      return Ok(ConjunctionDISIEnum::A(Self::new(iterators)?));
    };

    let input_count = iterators.len();
    let mut remaining = iterators.into_iter();
    let mut ordinary = Vec::with_capacity(input_count);
    ordinary.extend(remaining.by_ref().take(first_bit_set));
    let first = remaining
      .next()
      .ok_or_else(|| LuceneError::illegal_state("First bit set iterator is missing"))?;
    let mut bit_sets = vec![first];
    let mut iterator_locations = Vec::with_capacity(input_count);
    iterator_locations.extend((0..first_bit_set).map(IteratorLocation::Lead));
    iterator_locations.push(IteratorLocation::BitSet(0));
    for iterator in remaining {
      if iterator.is_bit_iter() && iterator.cost()? > min_cost {
        iterator_locations.push(IteratorLocation::BitSet(bit_sets.len()));
        bit_sets.push(iterator);
      } else {
        iterator_locations.push(IteratorLocation::Lead(ordinary.len()));
        ordinary.push(iterator);
      }
    }

    let lead = if ordinary.len() == 1 {
      DocIdSetIteratorEnum2::A(ordinary.remove(0))
    } else {
      DocIdSetIteratorEnum2::B(Self::new(ordinary)?)
    };
    Ok(ConjunctionDISIEnum::B(BitSetConjunctionDISI::new(
      lead,
      bit_sets,
      iterator_locations,
    )?))
  }
}

impl<D> ConjunctionDISI<D>
where
  D: DocIdSetIterator,
{
  pub(crate) fn new(iterators: Vec<D>) -> Result<Self> {
    debug_assert!(iterators.len() >= 2);
    let mut cost: SmallVec<[usize; 8]> = SmallVec::with_capacity(iterators.len());
    for idx in 0..iterators.len() {
      cost.push(idx);
    }
    let cmp = DisiCmp::new(iterators.as_ref());
    CollectionUtil::tim_sort_with_comparator(&mut cost, cmp)?;
    let lead1 = cost[0];
    let lead2 = cost[1];
    // Short conjunctions only need heap storage for the remaining indices.
    let others = if cost.spilled() {
      cost.drain(..2);
      cost.into_vec()
    } else {
      cost[2..].to_vec()
    };
    let v = Self {
      lead1,
      lead2,
      others,
      all_disi: iterators,
    };
    Ok(v)
  }
  fn do_next(&mut self, mut doc: i32) -> Result<i32> {
    'advance_head: loop {
      debug_assert_eq!(doc, self.all_disi[self.lead1].doc_id());
      // find agreement between the two iterators with the lower costs
      // we special case them because they do not need the
      // `other.doc_id() < doc` check that the remaining iterators need.
      let next2 = self.all_disi[self.lead2].advance(doc)?;
      if next2 != doc {
        doc = self.all_disi[self.lead1].advance(next2)?;
        if doc != next2 {
          continue;
        }
      }
      // then find agreement with other iterators
      for other_idx in self.others.iter() {
        let other_doc = self.all_disi[*other_idx].doc_id();

        // other.doc may already be equal to doc if we "continued advanceHead"
        // on the previous iteration and the advance on the lead scorer exactly matched.
        if other_doc < doc {
          let next = self.all_disi[*other_idx].advance(doc)?;

          if next > doc {
            // iterator beyond the current doc - advance lead and continue to the new highest doc.
            doc = self.all_disi[self.lead1].advance(next)?;
            continue 'advance_head;
          }
        }
      }
      return Ok(doc);
    }
  }
  // Returns `true` if all sub-iterators are on the same document ID, `false` otherwise.
  fn assert_iters_on_same_doc(&self) -> bool {
    let cur_doc = self.all_disi[self.lead1].doc_id();
    let mut iterators_on_the_same_doc = self.all_disi[self.lead2].doc_id() == cur_doc;
    let mut i = 0;
    while i < self.others.len() && iterators_on_the_same_doc {
      iterators_on_the_same_doc =
        iterators_on_the_same_doc && (self.all_disi[self.others[i]].doc_id() == cur_doc);
      i += 1;
    }
    iterators_on_the_same_doc
  }
}
impl<D> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions for ConjunctionDISI<D> where
  D: DocIdSetIterator
{
}
impl<D> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for ConjunctionDISI<D> where
  D: DocIdSetIterator
{
}

impl<D> DocIdSetIterator for ConjunctionDISI<D>
where
  D: DocIdSetIterator,
{
  fn doc_id(&self) -> i32 {
    self.all_disi[self.lead1].doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    debug_assert!(
      self.assert_iters_on_same_doc(),
      "Sub-iterators of ConjunctionDISI are not on the same document!"
    );
    let doc = self.all_disi[self.lead1].next_doc()?;
    self.do_next(doc)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    debug_assert!(
      self.assert_iters_on_same_doc(),
      "Sub-iterators of ConjunctionDISI are not on the same document!"
    );
    let doc = self.all_disi[self.lead1].advance(target)?;
    self.do_next(doc)
  }

  fn cost(&self) -> Result<i64> {
    self.all_disi[self.lead1].cost()
  }
}
struct DisiCmp<'a, D> {
  disi: &'a [D],
}
impl<'a, D> DisiCmp<'a, D> {
  fn new(disi: &'a [D]) -> Self {
    DisiCmp { disi }
  }
}
impl<D> Comparator<usize> for DisiCmp<'_, D>
where
  D: DocIdSetIterator,
{
  const TYPE: &'static str = "DisiCmp";

  fn compare(&self, a: &usize, b: &usize) -> Result<i32> {
    let lf = self.disi[*a].cost()?;
    let rg = self.disi[*b].cost()?;
    Ok(lf.cmp(&rg).to_int())
  }
}
#[derive(Clone, Copy)]
enum IteratorLocation {
  Lead(usize),
  BitSet(usize),
}

/// Conjunction between a [`DocIdSetIterator`] and one or more BitSetIterators.
pub struct BitSetConjunctionDISI<D> {
  lead: DocIdSetIteratorEnum2<D, ConjunctionDISI<D>>,
  bit_set_iterators: Vec<D>,
  min_length: i32,
  iterator_locations: Vec<IteratorLocation>,
}
impl<D> BitSetConjunctionDISI<D>
where
  D: DocIdSetIterator,
{
  fn new(
    lead: DocIdSetIteratorEnum2<D, ConjunctionDISI<D>>,
    mut bit_set_iterators: Vec<D>,
    mut iterator_locations: Vec<IteratorLocation>,
  ) -> Result<Self> {
    debug_assert!(!bit_set_iterators.is_empty());
    let mut cost: SmallVec<[usize; 8]> = SmallVec::with_capacity(bit_set_iterators.len());
    for idx in 0..bit_set_iterators.len() {
      cost.push(idx);
    }
    let cmp = BitSetIteratorCmp::new(bit_set_iterators.as_ref());
    ArrayUtil::tim_sort_with_comparator(&mut cost, cmp)?;

    let mut sorted_indices: SmallVec<[usize; 8]> = SmallVec::from_elem(0, cost.len());
    for (sorted_idx, idx) in cost.into_iter().enumerate() {
      sorted_indices[idx] = sorted_idx;
    }
    for location in &mut iterator_locations {
      if let IteratorLocation::BitSet(index) = location {
        *index = sorted_indices[*index];
      }
    }
    // Keep the iterators in their original allocation and resolve the sorted permutation.
    for source in 0..bit_set_iterators.len() {
      while sorted_indices[source] != source {
        let destination = sorted_indices[source];
        bit_set_iterators.swap(source, destination);
        sorted_indices.swap(source, destination);
      }
    }
    let mut min_length = i32::MAX;
    for iter in &bit_set_iterators {
      min_length = min_length.min(iter.bit_set_length()?.try_convert()?);
    }

    Ok(Self {
      lead,
      bit_set_iterators,
      min_length,
      iterator_locations,
    })
  }
  fn do_next(&mut self, mut doc: i32) -> Result<i32> {
    'advance_lead: loop {
      if doc >= self.min_length {
        if doc != NO_MORE_DOCS {
          self.lead.advance(NO_MORE_DOCS)?;
        }
        return Ok(NO_MORE_DOCS);
      }

      for bs_iter in &self.bit_set_iterators {
        if !bs_iter.get(doc as usize)? {
          doc = self.lead.next_doc()?;
          continue 'advance_lead;
        }
      }

      for iter in &mut self.bit_set_iterators {
        iter.set_doc_id(doc)?;
      }

      return Ok(doc);
    }
  }
  fn assert_iters_on_same_doc(&self) -> bool {
    let cur_doc = self.lead.doc_id();
    for iter in &self.bit_set_iterators {
      if iter.doc_id() != cur_doc {
        return false;
      }
    }
    true
  }
}
impl<D> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for BitSetConjunctionDISI<D>
where
  D: DocIdSetIterator,
{
}
impl<D> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for BitSetConjunctionDISI<D> where
  D: DocIdSetIterator
{
}

impl<D> DocIdSetIterator for BitSetConjunctionDISI<D>
where
  D: DocIdSetIterator,
{
  fn doc_id(&self) -> i32 {
    self.lead.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    debug_assert!(
      self.assert_iters_on_same_doc(),
      "Sub-iterators of BitSetConjunctionDISI are not on the same document!"
    );
    let doc = self.lead.next_doc()?;
    self.do_next(doc)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    debug_assert!(
      self.assert_iters_on_same_doc(),
      "Sub-iterators of BitSetConjunctionDISI are not on the same document!"
    );
    let doc = self.lead.advance(target)?;
    self.do_next(doc)
  }

  fn cost(&self) -> Result<i64> {
    self.lead.cost()
  }
}
struct BitSetIteratorCmp<'a, B> {
  disi: &'a [B],
}
impl<'a, B> BitSetIteratorCmp<'a, B> {
  fn new(disi: &'a [B]) -> Self {
    BitSetIteratorCmp { disi }
  }
}
impl<B> Comparator<usize> for BitSetIteratorCmp<'_, B>
where
  B: DocIdSetIterator,
{
  const TYPE: &'static str = "BitSetIteratorCmp";

  fn compare(&self, a: &usize, b: &usize) -> Result<i32> {
    let left = &self.disi[*a];
    let right = &self.disi[*b];
    Ok(left.cost()?.cmp(&right.cost()?).to_int())
  }
}
/// Access to sub-scorers in their original logical order.
#[doc(hidden)]
pub trait ConjunctionScorers: DocIdSetIterator {
  type SubScorer: Scorer;
  fn iterator_count(&self) -> usize;
  fn iterator_at(&self, index: usize) -> &ScorerDisi<Self::SubScorer>;
  fn iterator_at_mut(&mut self, index: usize) -> &mut ScorerDisi<Self::SubScorer>;
}
impl<S: Scorer> ConjunctionScorers for ConjunctionDISI<ScorerDisi<S>> {
  type SubScorer = S;
  fn iterator_count(&self) -> usize {
    self.all_disi.len()
  }
  fn iterator_at(&self, index: usize) -> &ScorerDisi<S> {
    &self.all_disi[index]
  }
  fn iterator_at_mut(&mut self, index: usize) -> &mut ScorerDisi<S> {
    &mut self.all_disi[index]
  }
}
impl<S: Scorer> ConjunctionScorers for BitSetConjunctionDISI<ScorerDisi<S>> {
  type SubScorer = S;
  fn iterator_count(&self) -> usize {
    self.iterator_locations.len()
  }
  fn iterator_at(&self, index: usize) -> &ScorerDisi<S> {
    match self.iterator_locations[index] {
      IteratorLocation::Lead(index) => match &self.lead {
        DocIdSetIteratorEnum2::A(iterator) => iterator,
        DocIdSetIteratorEnum2::B(lead) => &lead.all_disi[index],
      },
      IteratorLocation::BitSet(index) => &self.bit_set_iterators[index],
    }
  }
  fn iterator_at_mut(&mut self, index: usize) -> &mut ScorerDisi<S> {
    match self.iterator_locations[index] {
      IteratorLocation::Lead(index) => match &mut self.lead {
        DocIdSetIteratorEnum2::A(iterator) => iterator,
        DocIdSetIteratorEnum2::B(lead) => &mut lead.all_disi[index],
      },
      IteratorLocation::BitSet(index) => &mut self.bit_set_iterators[index],
    }
  }
}
impl<S: Scorer> ConjunctionScorers for ConjunctionDISIEnum<ScorerDisi<S>> {
  type SubScorer = S;
  fn iterator_count(&self) -> usize {
    self.len()
  }
  fn iterator_at(&self, index: usize) -> &ScorerDisi<S> {
    self.iterator_at(index)
  }
  fn iterator_at_mut(&mut self, index: usize) -> &mut ScorerDisi<S> {
    self.iterator_at_mut(index)
  }
}

/// [`TwoPhaseIterator`] implementing a conjunction.
pub struct ConjunctionTwoPhaseIterator<C> {
  two_phase_iterator_idx: Vec<usize>,
  pub(crate) approximation: C,
  match_cost: f32,
}
impl<C> ConjunctionTwoPhaseIterator<C>
where
  C: ConjunctionScorers,
{
  pub(crate) fn new(approximation: C) -> Result<Self> {
    debug_assert!(
      {
        let mut has_tpi = false;
        for idx in 0..approximation.iterator_count() {
          let x = approximation.iterator_at(idx);
          if x.has_two_phase_iterator() == crate::core::search::scorer::TwoPhaseState::Yes {
            has_tpi = true;
            break;
          }
        }
        has_tpi
      },
      "ConjunctionTwoPhaseIterator requires at least one TwoPhaseIterator"
    );
    let (two_phase_iterator_idx, total_match_cost) = {
      let mut two_phase_iterator_idx = Vec::with_capacity(approximation.iterator_count());
      let mut total_match_cost = 0.0;
      for idx in 0..approximation.iterator_count() {
        let scorer = approximation.iterator_at(idx);
        if scorer.has_two_phase_iterator() == crate::core::search::scorer::TwoPhaseState::Yes {
          two_phase_iterator_idx.push(idx);
          total_match_cost += scorer.match_cost();
        }
      }
      let cmp = TwoPhaseIteratorCmp {
        approximation: &approximation,
      };
      ArrayUtil::tim_sort_with_comparator(&mut two_phase_iterator_idx, cmp)?;
      (two_phase_iterator_idx, total_match_cost)
    };

    Ok(ConjunctionTwoPhaseIterator {
      two_phase_iterator_idx,
      approximation,
      match_cost: total_match_cost,
    })
  }
}
impl<C> ApproximationDISI for ConjunctionTwoPhaseIterator<C>
where
  C: ConjunctionScorers,
{
  fn approx_doc_id(&self) -> i32 {
    self.approximation.doc_id()
  }

  fn approx_next_doc(&mut self) -> Result<i32> {
    self.approximation.next_doc()
  }

  fn approx_advance(&mut self, target: i32) -> Result<i32> {
    self.approximation.advance(target)
  }

  fn approx_slow_advance(&mut self, target: i32) -> Result<i32> {
    self.approximation.slow_advance(target)
  }

  fn approx_cost(&self) -> Result<i64> {
    self.approximation.cost()
  }
}
impl<C> ApproximationBitSetAccess for ConjunctionTwoPhaseIterator<C>
where
  C: ConjunctionScorers,
{
  fn approx_is_bit_iter(&self) -> bool {
    self.approximation.is_bit_iter()
  }

  fn approx_get(&self, index: usize) -> Result<bool> {
    self.approximation.get(index)
  }

  fn approx_set_doc_id(&mut self, doc: i32) -> Result<()> {
    self.approximation.set_doc_id(doc)
  }

  fn approx_bit_set_length(&self) -> Result<usize> {
    self.approximation.bit_set_length()
  }
}

impl<C> TwoPhaseIterator for ConjunctionTwoPhaseIterator<C>
where
  C: ConjunctionScorers,
{
  fn matches(&mut self) -> Result<bool> {
    for idx in self.two_phase_iterator_idx.iter() {
      if !self.approximation.iterator_at_mut(*idx).matches()? {
        return Ok(false);
      }
    }
    Ok(true)
  }

  fn match_cost(&self) -> f32 {
    self.match_cost
  }
}

struct TwoPhaseIteratorCmp<'a, C> {
  approximation: &'a C,
}
impl<C: ConjunctionScorers> Comparator<usize> for TwoPhaseIteratorCmp<'_, C> {
  const TYPE: &'static str = "TwoPhaseIteratorCmp";
  fn compare(&self, a: &usize, b: &usize) -> Result<i32> {
    Ok(
      CoreHelper::compare_f32(
        self.approximation.iterator_at(*a).match_cost(),
        self.approximation.iterator_at(*b).match_cost(),
      )
      .to_int(),
    )
  }
}

pub struct ScorerDisi<S> {
  scorer: S,
}
impl<S> ScorerDisi<S> {
  pub fn new(scorer: S) -> Self {
    Self { scorer }
  }
}
impl<S> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions for ScorerDisi<S> where
  S: Scorer
{
}
impl<S> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for ScorerDisi<S>
where
  S: Scorer,
{
  fn is_bit_iter(&self) -> bool {
    self.scorer.approx_is_bit_iter()
  }
  fn get(&self, index: usize) -> Result<bool> {
    self.scorer.approx_get(index)
  }
  fn set_doc_id(&mut self, doc: i32) -> Result<()> {
    self.scorer.approx_set_doc_id(doc)
  }
  fn bit_set_length(&self) -> Result<usize> {
    self.scorer.approx_bit_set_length()
  }
}

impl<S> DocIdSetIterator for ScorerDisi<S>
where
  S: Scorer,
{
  fn doc_id(&self) -> i32 {
    self.scorer.approx_doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.scorer.approx_next_doc()
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.scorer.approx_advance(target)
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    self.scorer.approx_slow_advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self.scorer.approx_cost()
  }
}

impl<S> Scorable for ScorerDisi<S>
where
  S: Scorer,
{
  type Child<'a>
    = S::Child<'a>
  where
    Self: 'a;

  fn score(&mut self) -> Result<f32> {
    self.scorer.score()
  }

  fn smoothing_score(&mut self, doc_id: i32) -> Result<f32> {
    self.scorer.smoothing_score(doc_id)
  }

  fn set_min_competitive_score(&mut self, min_score: f32) -> Result<()> {
    self.scorer.set_min_competitive_score(min_score)
  }

  fn get_children(&mut self) -> Result<Vec<ChildScorable<Self::Child<'_>>>> {
    self.scorer.get_children()
  }

  fn cost(&self) -> Result<i64> {
    Scorable::cost(&self.scorer)
  }
}

impl<S> crate::core::search::scorable::FixedScore for ScorerDisi<S> {}

impl<S: Scorer> ScorerDisi<S> {
  pub(crate) fn scoring_doc_id(&mut self) -> Result<i32> {
    (self.scorer).scoring_doc_id()
  }
  pub(crate) fn advance_shallow(&mut self, target: i32) -> Result<i32> {
    self.scorer.advance_shallow(target)
  }
  pub(crate) fn get_max_score(&mut self, upto: i32) -> Result<f32> {
    self.scorer.get_max_score(upto)
  }
  fn has_two_phase_iterator(&self) -> crate::core::search::scorer::TwoPhaseState {
    self.scorer.has_two_phase_iterator()
  }
  fn match_cost(&self) -> f32 {
    self.scorer.match_cost()
  }

  fn matches(&mut self) -> Result<bool> {
    self.scorer.matches()
  }
}
