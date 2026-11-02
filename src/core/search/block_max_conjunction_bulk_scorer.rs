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

use crate::core::search::bulk_scorer::BulkScorer;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::scorable::{FixedScore, Scorable};
use crate::core::search::scorer::Scorer;
use crate::core::util::TryIntoInt;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::math_util::MathUtil;
use smallvec::SmallVec;

/// BulkScorer implementation of [`BlockMaxConjunctionScorer`](crate::core::search::block_max_conjunction_scorer) that focuses on top-level
/// conjunctions over clauses that do not have two-phase iterators. Use a [`DefaultBulkScorer`](crate::core::search::weight::DefaultBulkScorer)
/// around a [`BlockMaxConjunctionScorer`](crate::core::search::block_max_conjunction_scorer) if you need two-phase support. Another difference with
/// [`BlockMaxConjunctionScorer`](crate::core::search::block_max_conjunction_scorer) is that this scorer computes scores on the fly in order to be
/// able to skip evaluating more clauses if the total score would be under the minimum competitive
/// score anyway. This generally works well because computing a score is cheaper than decoding a
/// block of postings.
pub struct BlockMaxConjunctionBulkScorer<S> {
  scorable: DocAndScore,
  sum_of_other_clauses: Box<[f64]>,
  max_doc: i32,
  scorers: Vec<S>,
}

impl<S> BlockMaxConjunctionBulkScorer<S>
where
  S: Scorer,
{
  pub(crate) fn new(max_doc: i32, mut scorers_list: Vec<S>) -> Result<Self> {
    let mut costs: SmallVec<[(usize, i64); 8]> = SmallVec::with_capacity(scorers_list.len());
    for (index, scorer) in scorers_list.iter().enumerate() {
      costs.push((index, DocIdSetIterator::cost(scorer)?));
    }
    // Use the least costly scorers as leads, preserving input order for equal costs.
    costs.sort_by_key(|entry| entry.1);
    // Map each original position to its sorted destination, then resolve each cycle.
    // Keep Scorers in their original allocation; only costs and indices use inline storage.
    let mut destinations: SmallVec<[usize; 8]> = SmallVec::from_elem(0, scorers_list.len());
    for (destination, &(source, _)) in costs.iter().enumerate() {
      destinations[source] = destination;
    }
    for source in 0..scorers_list.len() {
      while destinations[source] != source {
        let destination = destinations[source];
        scorers_list.swap(source, destination);
        destinations.swap(source, destination);
      }
    }
    let scorers = scorers_list;
    Ok(Self {
      scorable: DocAndScore::new(0.0),
      sum_of_other_clauses: vec![f64::INFINITY; scorers.len()].into_boxed_slice(),
      max_doc,
      scorers,
    })
  }

  fn compute_max_score(&mut self, window_min: i32, window_max: i32) -> Result<f32> {
    for scorer in self.scorers.iter_mut() {
      scorer.advance_shallow(window_min)?;
    }

    let mut max_window_score: f64 = 0.0;

    for (i, scorer) in self.scorers.iter_mut().enumerate() {
      let max_clause_score = scorer.get_max_score(window_max)? as f64;
      self.sum_of_other_clauses[i] = max_clause_score;
      max_window_score += max_clause_score;
    }

    if self.sum_of_other_clauses.len() >= 2 {
      for i in (0..self.sum_of_other_clauses.len() - 1).rev() {
        self.sum_of_other_clauses[i] += self.sum_of_other_clauses[i + 1];
      }
    }

    Ok(max_window_score as f32)
  }
  fn score_window<CLC: crate::core::search::leaf_collector::LeafCollector, AB: Bits + ?Sized>(
    &mut self,
    collector: &mut CLC,
    accept_docs: Option<&AB>,
    min: i32,
    max: i32,
    max_window_score: f32,
  ) -> Result<()> {
    if max_window_score < self.scorable.min_competitive_score {
      // no hits are competitive
      return Ok(());
    }
    let scorers_len: i32 = self.scorers.len().try_convert()?;
    let lead1_doc = {
      let lead1_iter = &mut self.scorers[0];

      if DocIdSetIterator::doc_id(&lead1_iter) < min {
        lead1_iter.advance(min)?;
      }
      DocIdSetIterator::doc_id(&lead1_iter)
    };

    let sum_other_at_1 = self.sum_of_other_clauses[1];

    let mut doc = lead1_doc;
    'advance_head: loop {
      if doc >= max {
        break;
      }
      if let Some(bits) = accept_docs
        && !bits.get(doc as usize)?
      {
        doc = self.scorers[0].next_doc()?;
        continue;
      }
      // Compute the score as we find more matching clauses, in order to skip advancing other
      // clauses if the total score has no chance of being competitive. This works well because
      // computing a score is usually cheaper than decoding a full block of postings and
      // frequencies.
      let has_min_comp = self.scorable.min_competitive_score > 0.0;
      let mut current_score: f64 = if has_min_comp {
        self.scorers[0].score()? as f64
      } else {
        0.0
      };
      // This is the same logic as in the below for loop, specialized for the 2nd least costly
      // clause. Keeping this specialization helps the optimizer.

      // First check if we have a chance of having a match based on max scores
      if has_min_comp
        && (MathUtil::sum_upper_bound(current_score + sum_other_at_1, scorers_len) as f32)
          < self.scorable.min_competitive_score
      {
        doc = self.scorers[0].next_doc()?;
        continue;
      }

      let (head, rest) = self.scorers.split_at_mut(1);
      let lead1 = &mut head[0];

      let (head2, other) = rest.split_at_mut(1);
      let lead2 = &mut head2[0];

      {
        // NOTE: lead2 may be on `doc` already if we `continue`d on the previous loop iteration.
        let lead2_iter = &mut *lead2;
        if DocIdSetIterator::doc_id(&lead2_iter) < doc {
          let next = lead2_iter.advance(doc)?;
          if next != doc {
            doc = lead1.advance(next)?;
            continue 'advance_head;
          }
        }
        debug_assert!(DocIdSetIterator::doc_id(&lead2_iter) == doc);
      }

      if has_min_comp {
        current_score += lead2.score()? as f64;
      }

      for (idx, iter) in other.iter_mut().enumerate() {
        // First check if we have a chance of having a match based on max scores
        if has_min_comp
          && (MathUtil::sum_upper_bound(
            current_score + self.sum_of_other_clauses[idx + 2],
            scorers_len,
          ) as f32)
            < self.scorable.min_competitive_score
        {
          doc = self.scorers[0].next_doc()?;
          continue 'advance_head;
        }

        {
          // NOTE: these iterators may be on `doc` already if we called `continue advanceHead` on the
          // previous loop iteration.
          let it = &mut *iter;
          if DocIdSetIterator::doc_id(&it) < doc {
            let next = it.advance(doc)?;
            if next != doc {
              doc = lead1.advance(next)?;
              continue 'advance_head;
            }
          }
          debug_assert!(DocIdSetIterator::doc_id(&it) == doc);
        }
        if has_min_comp {
          current_score += iter.score()? as f64;
        }
      }

      if !has_min_comp {
        for scorer in &mut self.scorers {
          current_score += scorer.score()? as f64;
        }
      }

      self.scorable.score = current_score as f32;
      collector.collect(doc, &mut self.scorable)?;

      if max_window_score < self.scorable.min_competitive_score {
        // no more hits are competitive
        return Ok(());
      }
      doc = self.scorers[0].next_doc()?;
    }

    Ok(())
  }
}
impl<S> BulkScorer for BlockMaxConjunctionBulkScorer<S>
where
  S: Scorer,
{
  fn score<CLC: crate::core::search::leaf_collector::LeafCollector, AB: Bits + ?Sized>(
    &mut self,
    collector: &mut CLC,
    accept_docs: Option<&AB>,
    min: i32,
    max: i32,
  ) -> Result<i32> {
    collector.set_scorer(&mut self.scorable)?;

    let mut window_min = DocIdSetIterator::doc_id(&self.scorers[0]).max(min);

    while window_min < max {
      let shallow = self.scorers[0].advance_shallow(window_min)?;
      // Use impacts of the least costly scorer to compute windows
      // NOTE: windowMax is inclusive
      let window_max = shallow.min(max - 1);

      let mut max_window_score = f32::INFINITY;
      if self.scorable.min_competitive_score > 0.0 {
        max_window_score = self.compute_max_score(window_min, window_max)?;
      }

      self.score_window(
        collector,
        accept_docs,
        window_min,
        window_max + 1,
        max_window_score,
      )?;
      window_min = DocIdSetIterator::doc_id(&self.scorers[0]).max(window_max + 1);
    }

    Ok(if window_min >= self.max_doc {
      NO_MORE_DOCS
    } else {
      window_min
    })
  }

  fn cost(&mut self) -> Result<i64> {
    DocIdSetIterator::cost(&self.scorers[0])
  }
}

pub struct DocAndScore {
  pub(crate) score: f32,
  pub(crate) min_competitive_score: f32,
}

impl DocAndScore {
  pub fn new(score: f32) -> Self {
    Self {
      score,
      min_competitive_score: f32::NEG_INFINITY,
    }
  }
}

impl Scorable for DocAndScore {
  type Child<'a>
    = crate::core::search::dummy::dummy_scorable::DummyScorable
  where
    Self: 'a;

  fn score(&mut self) -> Result<f32> {
    Ok(self.score)
  }

  fn set_min_competitive_score(&mut self, min_score: f32) -> Result<()> {
    self.min_competitive_score = min_score;
    Ok(())
  }

  fn cost(&self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }
}

impl FixedScore for DocAndScore {}
