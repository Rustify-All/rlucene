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
use crate::core::search::conjunction_disi::{ConjunctionDISI, ConjunctionTwoPhaseIterator};
use crate::core::search::conjunction_scorer::{ConjunctionScorer, ConjunctionScorerDisi};
use crate::test_framework::core::util::lucene_test_case::{at_least, random};
use rand::{Rng, RngExt};
use std::sync::Arc;

use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::doc_id_set::DocIdSet;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::doc_id_set_iterator::{
  AllDISI, DocIdSetIterator, DocIdSetIteratorEnum2, RangeDISI,
};

use crate::core::search::scorable::{FixedScore, Scorable};
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::{Scorer, ScorerEnum2, ScorerEnum3, TwoPhaseState};
use crate::core::search::two_phase_iterator::{
  TwoPhaseIterator, TwoPhaseIteratorAsDocIdSetIterator,
};
use crate::core::util::bit_doc_id_set::BitDocIdSet;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::test_framework::core::util::test_util::TestUtil;

#[allow(dead_code)] // for quick search
struct TestConjunctionDISI;

fn approximation<D, R>(
  random: &mut R,
  iterator: D,
  confirmed: Arc<FixedBitSet>,
) -> TwoPhaseIteratorImpl<DocIdSetIteratorEnum2<DocIdSetIteratorImpl<D>, D>>
where
  D: DocIdSetIterator,
  R: Rng + ?Sized,
{
  let v = if random.random_bool(0.5) {
    DocIdSetIteratorEnum2::A(anonymize_iterator(iterator))
  } else {
    DocIdSetIteratorEnum2::B(iterator)
  };
  TwoPhaseIteratorImpl::new(v, confirmed)
}

/// Returns an anonymous implementation so `ConjunctionDISI` cannot optimize it as a `BitSetIterator`.
fn anonymize_iterator<D>(it: D) -> DocIdSetIteratorImpl<D>
where
  D: DocIdSetIterator,
{
  DocIdSetIteratorImpl { it }
}

fn scorer<TPI>(two_phase_iterator: TPI) -> ScorerImpl<TPI>
where
  TPI: TwoPhaseIterator,
{
  ScorerImpl::new(two_phase_iterator)
}
fn random_set<R>(random: &mut R, max_doc: usize) -> FixedBitSet
where
  R: Rng + ?Sized,
{
  let step = TestUtil::next_usize(random, 1, 10);
  let mut set = FixedBitSet::new(max_doc);

  let mut doc = random.random_range(0..step);
  while doc < max_doc {
    set.set(doc);
    doc += TestUtil::next_usize(random, 1, step);
  }

  set
}
fn clear_random_bits<R>(random: &mut R, other: &FixedBitSet) -> FixedBitSet
where
  R: Rng + ?Sized,
{
  let mut set = FixedBitSet::new(other.length());

  set.or(other);

  for i in 0..set.length() {
    if random.random_bool(0.5) {
      set.clear_with_index(i);
    }
  }

  set
}
fn intersect(bit_sets: &[Arc<FixedBitSet>]) -> FixedBitSet {
  let mut intersection = FixedBitSet::new(bit_sets[0].length());

  intersection.or(&bit_sets[0]);

  for bs in &bit_sets[1..] {
    intersection.and(bs);
  }

  intersection
}
#[test]
fn test_conjunction() -> Result<()> {
  let mut random = random();

  let iters = at_least(&mut random, 100);

  for _ in 0..iters {
    let max_doc = TestUtil::next_usize(&mut random, 100, 10000);
    let num_iterators = TestUtil::next_usize(&mut random, 2, 5);

    let mut sets = Vec::with_capacity(num_iterators);
    let mut iterators = Vec::with_capacity(num_iterators);

    for _ in 0..num_iterators {
      let set = Arc::new(random_set(&mut random, max_doc));

      match random.random_range(0..3) {
        0 => {
          sets.push(set.clone());

          let it = BitDocIdSet::new(Some(set))?.iterator()?;
          let scorer =
            ConstantScoreScorer::from_disi(0f32, ScoreMode::TopScores, anonymize_iterator(it));

          iterators.push(ScorerEnum3::A(scorer));
        },
        1 => {
          // bitset iterator
          sets.push(set.clone());

          let it = BitDocIdSet::new(Some(set))?.iterator()?;
          let scorer = ConstantScoreScorer::from_disi(0f32, ScoreMode::TopScores, it);

          iterators.push(ScorerEnum3::B(scorer));
        },
        _ => {
          // two-phase iterator
          let confirmed = Arc::new(clear_random_bits(&mut random, &set));
          sets.push(confirmed.clone());

          let approx = approximation(
            &mut random,
            BitDocIdSet::new(Some(set))?.iterator()?,
            confirmed,
          );

          let scorer = scorer(approx);
          iterators.push(ScorerEnum3::C(scorer));
        },
      }
    }
    let mut has_tpi = false;
    for x in &iterators {
      if x.has_two_phase_iterator() == TwoPhaseState::Yes {
        has_tpi = true;
        break;
      }
    }
    let conjunction = ConjunctionDISI::from_scorer(iterators)?;
    let mut disi = match has_tpi {
      false => ConjunctionScorerDisi::A(conjunction),
      true => {
        let v =
          TwoPhaseIteratorAsDocIdSetIterator::new(ConjunctionTwoPhaseIterator::new(conjunction)?);
        ConjunctionScorerDisi::B(v)
      },
    };

    let actual = to_bit_set(max_doc, &mut disi)?;
    let expected = intersect(&sets);

    assert_eq!(expected, actual);
  }

  Ok(())
}
#[test]
fn test_conjunction_approximation() -> Result<()> {
  let mut random = random();

  let iters = at_least(&mut random, 100);

  for _ in 0..iters {
    let max_doc = TestUtil::next_usize(&mut random, 100, 10000);
    let num_iterators = TestUtil::next_usize(&mut random, 2, 5);

    let mut sets = Vec::with_capacity(num_iterators);
    let mut iterators = Vec::with_capacity(num_iterators);

    let mut has_approximation = false;

    for _ in 0..num_iterators {
      let set = Arc::new(random_set(&mut random, max_doc));

      if random.random_bool(0.5) {
        // simple iterator
        sets.push(set.clone());

        let it = BitDocIdSet::new(Some(set))?.iterator()?;
        let scorer = ConstantScoreScorer::from_disi(0f32, ScoreMode::CompleteNoScores, it);

        iterators.push(ScorerEnum2::A(scorer));
      } else {
        // scorer with approximation
        let confirmed = Arc::new(clear_random_bits(&mut random, &set));
        sets.push(confirmed.clone());

        let approx = approximation(
          &mut random,
          BitDocIdSet::new(Some(set))?.iterator()?,
          confirmed,
        );

        let scorer = scorer(approx);
        iterators.push(ScorerEnum2::B(scorer));

        has_approximation = true;
      }
    }

    let mut has_tpi = false;
    for x in &iterators {
      if x.has_two_phase_iterator() == TwoPhaseState::Yes {
        has_tpi = true;
        break;
      }
    }
    assert_eq!(has_approximation, has_tpi);
    let v = ConjunctionDISI::from_scorer(iterators)?;
    let mut disi = match has_tpi {
      false => ConjunctionScorerDisi::A(v),
      true => {
        let v = TwoPhaseIteratorAsDocIdSetIterator::new(ConjunctionTwoPhaseIterator::new(v)?);
        ConjunctionScorerDisi::B(v)
      },
    };
    assert_eq!(intersect(&sets), to_bit_set(max_doc, &mut disi)?);
  }

  Ok(())
}
#[test]
fn test_recursive_conjunction_approximation() -> Result<()> {
  let mut random = random();
  let iters = at_least(&mut random, 100);

  for _ in 0..iters {
    let max_doc = TestUtil::next_usize(&mut random, 100, 10000);
    let num_iterators = TestUtil::next_usize(&mut random, 2, 5);
    let mut sets = Vec::with_capacity(num_iterators);
    let mut conjunction: Option<Box<dyn Scorer>> = None;
    let mut has_approximation = false;

    for _ in 0..num_iterators {
      let set = Arc::new(random_set(&mut random, max_doc));
      let new_iterator: Box<dyn Scorer> = match random.random_range(0..3) {
        0 => {
          sets.push(set.clone());
          let it = BitDocIdSet::new(Some(set))?.iterator()?;
          Box::new(ConstantScoreScorer::from_disi(
            0f32,
            ScoreMode::TopScores,
            anonymize_iterator(it),
          ))
        },
        1 => {
          sets.push(set.clone());
          let it = BitDocIdSet::new(Some(set))?.iterator()?;
          Box::new(ConstantScoreScorer::from_disi(
            0f32,
            ScoreMode::TopScores,
            it,
          ))
        },
        _ => {
          let confirmed = Arc::new(clear_random_bits(&mut random, &set));
          sets.push(confirmed.clone());
          let approximation = approximation(
            &mut random,
            BitDocIdSet::new(Some(set))?.iterator()?,
            confirmed,
          );
          has_approximation = true;
          Box::new(scorer(approximation))
        },
      };

      conjunction = Some(match conjunction {
        None => new_iterator,
        Some(conjunction) => {
          let conjunction = ConjunctionDISI::from_scorer(vec![conjunction, new_iterator])?;
          if has_approximation {
            Box::new(ConstantScoreScorer::from_tpi(
              0f32,
              ScoreMode::TopScores,
              ConjunctionTwoPhaseIterator::new(conjunction)?,
            ))
          } else {
            Box::new(ConstantScoreScorer::from_disi(
              0f32,
              ScoreMode::TopScores,
              conjunction,
            ))
          }
        },
      });
    }

    let mut conjunction = conjunction.unwrap();
    assert_eq!(
      has_approximation,
      conjunction.two_phase_iterator().is_some()
    );
    assert_eq!(
      intersect(&sets),
      to_bit_set(max_doc, conjunction.iterator_mut().as_mut())?
    );
  }

  Ok(())
}
fn to_bit_set<I>(max_doc: usize, iterator: &mut I) -> Result<FixedBitSet>
where
  I: DocIdSetIterator + ?Sized,
{
  let mut set = FixedBitSet::new(max_doc);

  let mut doc = iterator.next_doc()?;
  while doc != NO_MORE_DOCS {
    set.set(doc as usize);
    doc = iterator.next_doc()?;
  }

  Ok(set)
}
#[test]
fn test_collapse_sub_conjunction_disis() -> Result<()> {
  test_collapse_sub_conjunctions(false)
}

#[test]
fn test_collapse_sub_conjunction_scorers() -> Result<()> {
  test_collapse_sub_conjunctions(true)
}
fn test_collapse_sub_conjunctions(wrap_with_scorer: bool) -> Result<()> {
  let mut random = random();
  let iters = at_least(&mut random, 100);

  for _ in 0..iters {
    let max_doc = TestUtil::next_usize(&mut random, 100, 10000);
    let num_iterators = TestUtil::next_usize(&mut random, 5, 10);
    let mut sets = Vec::with_capacity(num_iterators);
    let mut scorers: Vec<Box<dyn Scorer>> = Vec::with_capacity(num_iterators);

    for _ in 0..num_iterators {
      let set = Arc::new(random_set(&mut random, max_doc));
      if random.random_bool(0.5) {
        sets.push(set.clone());
        scorers.push(Box::new(ConstantScoreScorer::from_disi(
          0f32,
          ScoreMode::TopScores,
          BitDocIdSet::new(Some(set))?.iterator()?,
        )));
      } else {
        let confirmed = Arc::new(clear_random_bits(&mut random, &set));
        sets.push(confirmed.clone());
        scorers.push(Box::new(scorer(approximation(
          &mut random,
          BitDocIdSet::new(Some(set))?.iterator()?,
          confirmed,
        ))));
      }
    }

    let sub_iters = at_least(&mut random, 3);
    for _ in 0..sub_iters {
      if scorers.len() <= 3 {
        break;
      }
      let sub_seq_start = TestUtil::next_usize(&mut random, 0, scorers.len() - 2);
      let sub_seq_end = TestUtil::next_usize(&mut random, sub_seq_start + 2, scorers.len());
      let sub_scorers: Vec<Box<dyn Scorer>> = scorers.drain(sub_seq_start..sub_seq_end).collect();

      let sub_conjunction: Box<dyn Scorer> = if wrap_with_scorer {
        Box::new(ConjunctionScorer::new(sub_scorers, vec![])?)
      } else {
        let has_two_phase = sub_scorers
          .iter()
          .any(|scorer| scorer.has_two_phase_iterator() == TwoPhaseState::Yes);
        let conjunction = ConjunctionDISI::from_scorer(sub_scorers)?;
        if has_two_phase {
          Box::new(ConstantScoreScorer::from_tpi(
            0f32,
            ScoreMode::TopScores,
            ConjunctionTwoPhaseIterator::new(conjunction)?,
          ))
        } else {
          Box::new(ConstantScoreScorer::from_disi(
            0f32,
            ScoreMode::TopScores,
            conjunction,
          ))
        }
      };
      scorers.insert(sub_seq_start, sub_conjunction);
    }

    if scorers.len() == 1 {
      scorers.push(Box::new(ConstantScoreScorer::from_disi(
        0f32,
        ScoreMode::TopScores,
        AllDISI::new(max_doc as i32),
      )));
    }

    let has_two_phase = scorers
      .iter()
      .any(|scorer| scorer.has_two_phase_iterator() == TwoPhaseState::Yes);
    let conjunction = ConjunctionDISI::from_scorer(scorers)?;
    let mut conjunction: Box<dyn Scorer> = if has_two_phase {
      Box::new(ConstantScoreScorer::from_tpi(
        0f32,
        ScoreMode::TopScores,
        ConjunctionTwoPhaseIterator::new(conjunction)?,
      ))
    } else {
      Box::new(ConstantScoreScorer::from_disi(
        0f32,
        ScoreMode::TopScores,
        conjunction,
      ))
    };

    assert_eq!(
      intersect(&sets),
      to_bit_set(max_doc, conjunction.iterator_mut().as_mut())?
    );
  }

  Ok(())
}
#[test]
fn test_illegal_advancement_of_sub_iterators_trips_assertion() -> Result<()> {
  if !cfg!(debug_assertions) {
    return Ok(());
  }

  let mut random = random();

  let max_doc = 100;
  let num_iterators = TestUtil::next_usize(&mut random, 2, 5);

  let set = Arc::new(random_set(&mut random, max_doc));

  let mut iterators = Vec::with_capacity(num_iterators);
  for _ in 0..num_iterators {
    iterators.push(BitDocIdSet::new(Some(set.clone()))?.iterator()?);
  }
  let len = iterators.len();
  let mut conjunction = ConjunctionDISI::from_disi(iterators)?;

  let idx = TestUtil::next_usize(&mut random, 0, len - 1);
  let rogue = &mut conjunction.all_disi[idx];
  let _ = rogue.next_doc()?;
  let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
    let _ = conjunction.next_doc();
  }));
  if let Err(err) = result {
    let msg = if let Some(s) = err.downcast_ref::<&str>() {
      *s
    } else if let Some(s) = err.downcast_ref::<String>() {
      s.as_str()
    } else {
      ""
    };
    assert!(msg.contains("Sub-iterators of ConjunctionDISI are not on the same document!"));
  }

  Ok(())
}
#[test]
fn test_bit_set_conjunction_disi_doc_id_on_exhaust() -> Result<()> {
  let mut random = random();

  let num_bitset_iterators = TestUtil::next_usize(&mut random, 2, 5);
  let mut iterators = Vec::with_capacity(num_bitset_iterators + 1);

  let max_bitset_length = 1000;
  let min_bitset_length = 2;
  let lead_max_doc = max_bitset_length + 1;

  iterators.push(DocIdSetIteratorEnum2::A(RangeDISI::new(
    lead_max_doc as i32,
    (lead_max_doc + 1) as i32,
  )?));

  for _ in 0..num_bitset_iterators {
    let bitset_length = TestUtil::next_usize(&mut random, min_bitset_length, max_bitset_length);

    let mut bitset = FixedBitSet::new(bitset_length);
    bitset.set_with_range(0, bitset_length - 1);

    let it = BitDocIdSet::new(Some(Arc::new(bitset)))?.iterator()?;
    iterators.push(DocIdSetIteratorEnum2::B(it));
  }

  let mut conjunction = ConjunctionDISI::from_disi(iterators)?;

  assert_eq!(NO_MORE_DOCS, conjunction.next_doc()?);
  assert_eq!(NO_MORE_DOCS, conjunction.doc_id());

  Ok(())
}

struct ScorerImpl<TPI> {
  tpi_disi: TwoPhaseIteratorAsDocIdSetIterator<TPI>,
}
impl<TPI> ScorerImpl<TPI>
where
  TPI: TwoPhaseIterator,
{
  fn new(two_phase_iterator: TPI) -> Self {
    Self {
      tpi_disi: TwoPhaseIteratorAsDocIdSetIterator::new(two_phase_iterator),
    }
  }
}

impl<TPI> Scorable for ScorerImpl<TPI>
where
  TPI: TwoPhaseIterator,
{
  fn score(&mut self) -> Result<f32> {
    Ok(0f32)
  }
}

impl<TPI> FixedScore for ScorerImpl<TPI> where TPI: TwoPhaseIterator {}

impl<TPI> Scorer for ScorerImpl<TPI>
where
  TPI: TwoPhaseIterator + 'static,
{
  fn doc_id(&mut self) -> Result<i32> {
    Ok(self.tpi_disi.doc_id())
  }

  fn iterator(&self) -> Box<dyn DocIdSetIterator + '_> {
    Box::new(&self.tpi_disi)
  }

  fn iterator_mut(&mut self) -> Box<dyn DocIdSetIterator + '_> {
    Box::new(&mut self.tpi_disi)
  }

  fn take_iterator(self: Box<Self>) -> Box<dyn DocIdSetIterator> {
    let ScorerImpl { tpi_disi, .. } = *self;
    Box::new(tpi_disi)
  }

  fn two_phase_iterator(&self) -> Option<Box<dyn TwoPhaseIterator + '_>> {
    Some(Box::new(&self.tpi_disi.two_phase_iterator))
  }

  fn two_phase_iterator_mut(&mut self) -> Option<Box<dyn TwoPhaseIterator + '_>> {
    Some(Box::new(&mut self.tpi_disi.two_phase_iterator))
  }

  fn take_two_phase_iterator(self: Box<Self>) -> Option<Box<dyn TwoPhaseIterator>> {
    let ScorerImpl { tpi_disi, .. } = *self;
    Some(Box::new(tpi_disi.two_phase_iterator))
  }

  fn get_max_score(&mut self, _upto: i32) -> Result<f32> {
    Ok(0f32)
  }

  fn has_two_phase_iterator(&self) -> TwoPhaseState {
    TwoPhaseState::Yes
  }

  fn approximation(&self) -> Box<dyn DocIdSetIterator + '_> {
    self.tpi_disi.two_phase_iterator.approximation()
  }

  fn approximation_mut(&mut self) -> Box<dyn DocIdSetIterator + '_> {
    self.tpi_disi.two_phase_iterator.approximation_mut()
  }
}

struct TwoPhaseIteratorImpl<D> {
  approximation: D,
  confirmed: Arc<FixedBitSet>,
}
impl<D> TwoPhaseIteratorImpl<D>
where
  D: DocIdSetIterator,
{
  fn new(approximation: D, confirmed: Arc<FixedBitSet>) -> Self {
    Self {
      approximation,
      confirmed,
    }
  }
}
impl<D> TwoPhaseIterator for TwoPhaseIteratorImpl<D>
where
  D: DocIdSetIterator,
{
  fn approximation_mut(&mut self) -> Box<dyn DocIdSetIterator + '_> {
    Box::new(&mut self.approximation)
  }

  fn approximation(&self) -> Box<dyn DocIdSetIterator + '_> {
    Box::new(&self.approximation)
  }

  fn matches(&mut self) -> Result<bool> {
    self.confirmed.get(self.approximation.doc_id() as usize)
  }

  fn match_cost(&self) -> f32 {
    5f32
  }
}

struct DocIdSetIteratorImpl<D> {
  it: D,
}
impl<D> DocIdSetIterator for DocIdSetIteratorImpl<D>
where
  D: DocIdSetIterator,
{
  fn doc_id(&self) -> i32 {
    self.it.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.it.next_doc()
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.it.advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self.it.cost()
  }
}
