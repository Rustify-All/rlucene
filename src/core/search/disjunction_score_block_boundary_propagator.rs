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
use crate::core::search::disi_wrapper::DisiWrapper;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::scorer::Scorer;
use crate::core::util::core_helper::CoreHelper;
use crate::core::util::error::lucene_error::Result;
/// A helper to propagate block boundaries for disjunctions. Because a disjunction matches if any of
/// its sub clauses matches, it is tempting to return the minimum block boundary across all clauses.
/// The problem is that it might then make the query slow when the minimum competitive score is high
/// and low-scoring clauses no longer drive iteration. This struct computes block boundaries
/// only across clauses whose maximum score is greater than or equal to the minimum competitive
/// score, or the maximum scoring clause if there is no such clause.
pub struct DisjunctionScoreBlockBoundaryPropagator {
  cost: smallvec::SmallVec<[Cost; 4]>,
  lead_index: usize,
}
impl DisjunctionScoreBlockBoundaryPropagator {
  pub(crate) fn new<S>(scorers: &mut [S]) -> Result<Self>
  where
    S: Scorer,
  {
    let mut cost = smallvec::SmallVec::<[Cost; 4]>::with_capacity(scorers.len());
    for (i, scorer) in scorers.iter_mut().enumerate() {
      scorer.advance_shallow(0)?;
      let max_score = scorer.get_max_score(NO_MORE_DOCS)?;
      let iter_cost = scorer.cost()?;
      cost.push(Cost::new(max_score, i, iter_cost));
    }
    cost.sort_by(|a, b| {
      CoreHelper::compare_f32(a.max_score, b.max_score).then_with(|| a.iter_cost.cmp(&b.iter_cost))
    });
    Ok(Self {
      cost,
      lead_index: 0,
    })
  }
  /// Advances all scorers shallowly to `target`.
  pub(crate) fn advance_shallow<S>(
    &mut self,
    target: i32,
    scorers: &mut [DisiWrapper<S>],
  ) -> Result<i32>
  where
    S: Scorer,
  {
    // For scorers that are below the lead index, just propagate.
    for i in 0..self.lead_index {
      let s = &mut scorers[self.cost[i].idx].scorer;
      if (s).scoring_doc_id()? < target {
        s.advance_shallow(target)?;
      }
    }

    // For scorers above the lead index, we take the minimum boundary.
    let lead_idx = self.lead_index;
    let lead_scorer = &mut scorers[self.cost[lead_idx].idx];
    let doc_id = (lead_scorer.scorer).scoring_doc_id()?;
    let mut upto = lead_scorer
      .scorer
      .advance_shallow(std::cmp::max(doc_id, target))?;

    for i in (lead_idx + 1)..self.cost.len() {
      let scorer = &mut scorers[self.cost[i].idx];
      if (scorer.scorer).scoring_doc_id()? <= target {
        let v = scorer.scorer.advance_shallow(target)?;
        upto = std::cmp::min(v, upto);
      }
    }

    // If the maximum scoring clauses are beyond `target`, then we use their
    // docID as a boundary. It helps not consider them when computing the
    // maximum score and get a lower score upper bound.
    let mut i = self.cost.len() - 1;
    while i > self.lead_index {
      let scorer = &mut scorers[self.cost[i].idx];
      let doc = (scorer.scorer).scoring_doc_id()?;
      if doc > target {
        upto = std::cmp::min(upto, doc - 1);
      } else {
        break;
      }
      i -= 1;
    }

    Ok(upto)
  }

  /// Set the minimum competitive score to filter out clauses that score less than this threshold.
  pub(crate) fn set_min_competitive_score(&mut self, min_score: f32) {
    // Update the lead index if necessary
    while (self.lead_index + 1 < self.cost.len())
      && min_score > self.cost[self.lead_index].max_score
    {
      self.lead_index += 1;
    }
  }
}
struct Cost {
  max_score: f32,
  idx: usize,
  iter_cost: i64,
}
impl Cost {
  fn new(max_score: f32, idx: usize, iter_cost: i64) -> Self {
    Self {
      max_score,
      idx,
      iter_cost,
    }
  }
}
