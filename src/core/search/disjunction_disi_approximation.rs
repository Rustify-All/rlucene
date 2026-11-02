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
use crate::core::search::disi_priority_queue::DisiPriorityQueue;
use crate::core::search::disi_wrapper::DisiWrapper;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::scorer::Scorer;
use crate::core::util::error::lucene_error::{LuceneError, Result};

pub struct DisjunctionDISIApproximation<S> {
  all_scores: Box<[DisiWrapper<S>]>,
  sub_iterators: DisiPriorityQueue,
  cost: i64,
  #[cfg(debug_assertions)]
  initial_all_scores_len: usize,
  #[cfg(debug_assertions)]
  initial_sub_iterator_count: usize,
}
impl<S> DisjunctionDISIApproximation<S> {
  pub fn new(sub_iterators: DisiPriorityQueue, all_scores: Vec<DisiWrapper<S>>) -> Result<Self> {
    if sub_iterators.size() == 0 {
      return Err(LuceneError::illegal_argument(
        "sub_iterators must not be empty",
      ));
    }

    let mut cost = 0i64;
    for idx in sub_iterators.iter() {
      let score = all_scores.get(idx).ok_or_else(|| {
        LuceneError::illegal_argument(format!(
          "sub-iterator index {idx} is out of bounds for {} scores",
          all_scores.len()
        ))
      })?;
      cost += score.cost;
    }
    #[cfg(debug_assertions)]
    let initial_all_scores_len = all_scores.len();
    #[cfg(debug_assertions)]
    let initial_sub_iterator_count = sub_iterators.size();
    Ok(Self {
      all_scores: all_scores.into_boxed_slice(),
      sub_iterators,
      cost,
      #[cfg(debug_assertions)]
      initial_all_scores_len,
      #[cfg(debug_assertions)]
      initial_sub_iterator_count,
    })
  }

  pub(crate) fn all_scores(&self) -> &[DisiWrapper<S>] {
    #[cfg(debug_assertions)]
    self.debug_assert_structure_unchanged();
    &self.all_scores
  }

  pub(crate) fn all_scores_mut(&mut self) -> &mut [DisiWrapper<S>] {
    #[cfg(debug_assertions)]
    self.debug_assert_structure_unchanged();
    &mut self.all_scores
  }

  pub(crate) fn sub_iterators_and_all_scores_mut(
    &mut self,
  ) -> (&DisiPriorityQueue, &mut [DisiWrapper<S>]) {
    #[cfg(debug_assertions)]
    self.debug_assert_structure_unchanged();
    (&self.sub_iterators, &mut self.all_scores)
  }

  pub(crate) fn top_list_root(&mut self) -> usize
  where
    S: Scorer,
  {
    #[cfg(debug_assertions)]
    self.debug_assert_structure_unchanged();
    let root = self.sub_iterators.top_list_root(&mut self.all_scores);
    #[cfg(debug_assertions)]
    self.debug_assert_structure_unchanged();
    root
  }

  fn top_idx(&self) -> usize {
    #[cfg(debug_assertions)]
    self.debug_assert_structure_unchanged();
    expect_invariant!(
      self.sub_iterators.top(),
      "a disjunction approximation retains its non-empty iterator queue"
    )
  }

  #[cfg(debug_assertions)]
  fn debug_assert_structure_unchanged(&self) {
    debug_assert_eq!(self.initial_all_scores_len, self.all_scores.len());
    debug_assert_eq!(self.initial_sub_iterator_count, self.sub_iterators.size());
  }
}
impl<S> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for DisjunctionDISIApproximation<S>
where
  S: Scorer,
{
}
impl<S> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for DisjunctionDISIApproximation<S>
where
  S: Scorer,
{
}

impl<S> DocIdSetIterator for DisjunctionDISIApproximation<S>
where
  S: Scorer,
{
  fn doc_id(&self) -> i32 {
    let top_idx = self.top_idx();
    self.all_scores[top_idx].doc
  }

  fn next_doc(&mut self) -> Result<i32> {
    let mut top_idx = self.top_idx();
    let old_doc = self.all_scores[top_idx].doc;

    loop {
      let top = &mut self.all_scores[top_idx];
      let v = top.scorer.approx_next_doc()?;
      top.doc = v;
      top_idx = self.sub_iterators.update_top(&self.all_scores);
      #[cfg(debug_assertions)]
      self.debug_assert_structure_unchanged();
      if self.all_scores[top_idx].doc != old_doc {
        break;
      }
    }
    Ok(self.all_scores[top_idx].doc)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    let mut top_idx = self.top_idx();
    loop {
      let top = &mut self.all_scores[top_idx];
      let v = top.scorer.approx_advance(target)?;
      top.doc = v;
      top_idx = self.sub_iterators.update_top(&self.all_scores);
      #[cfg(debug_assertions)]
      self.debug_assert_structure_unchanged();
      if self.all_scores[top_idx].doc >= target {
        break;
      }
    }

    Ok(self.all_scores[top_idx].doc)
  }

  fn cost(&self) -> Result<i64> {
    Ok(self.cost)
  }
}
