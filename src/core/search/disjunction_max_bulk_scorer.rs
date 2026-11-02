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
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::leaf_collector::LeafCollector;
use crate::core::search::scorable::Scorable;
use crate::core::search::simple_scorable::SimpleScorable;
use crate::core::util::CoreHelper;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::priority_queue::{Compare, PriorityQueue};
use std::fmt::{Display, Formatter};

const WINDOW_SIZE: usize = 4096;
/// Bulk scorer for DisjunctionMaxQuery when the tie-break multiplier is zero.
pub struct DisjunctionMaxBulkScorer<BS> {
  window_matches: FixedBitSet,
  window_scores: [f32; WINDOW_SIZE],
  scorers: PriorityQueue<usize, BulkScorerAndNextCmp<BS>>,
  top_level_scorable: SimpleScorable,
}
impl<BS> DisjunctionMaxBulkScorer<BS>
where
  BS: BulkScorer,
{
  pub fn new(scorers: Vec<BS>) -> Result<Self> {
    let len = scorers.len();
    if len < 2 {
      return Err(LuceneError::illegal_argument("scorers.len() must be >= 2"));
    }

    let mut bulk_scorer_and_nexts = Vec::with_capacity(len);
    for scorer in scorers {
      bulk_scorer_and_nexts.push(BulkScorerAndNext::new(scorer));
    }

    let cmp = BulkScorerAndNextCmp {
      bulk_scorer_and_nexts: bulk_scorer_and_nexts.into_boxed_slice(),
    };
    let mut queue = PriorityQueue::new(len, cmp)?;
    for i in 0..queue.compare.bulk_scorer_and_nexts.len() {
      queue.add(i)?;
    }

    Ok(Self {
      window_matches: FixedBitSet::new(WINDOW_SIZE + 1),
      window_scores: [0.0; WINDOW_SIZE],
      scorers: queue,
      top_level_scorable: SimpleScorable::default(),
    })
  }
}
impl<BS> BulkScorer for DisjunctionMaxBulkScorer<BS>
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
    loop {
      let (window_min, window_max) = {
        let top = self
          .scorers
          .top()
          .ok_or_else(|| LuceneError::illegal_state("scorers is empty"))?;
        let top_next = self.scorers.compare.bulk_scorer_and_nexts[*top].next;
        if top_next >= max {
          break;
        }

        let window_min = std::cmp::max(top_next, min);
        let window_max = (max as i64).min(window_min as i64 + WINDOW_SIZE as i64) as i32;
        (window_min, window_max)
      };

      let mut top;
      loop {
        {
          top = *self
            .scorers
            .top()
            .ok_or_else(|| LuceneError::illegal_state("scorers is empty"))?;

          let scorer = &mut self.scorers.compare.bulk_scorer_and_nexts[top];
          let mut leaf_collector = LeafCollectorImpl::new(
            &mut self.window_matches,
            &mut self.top_level_scorable,
            window_min,
            &mut self.window_scores,
          );
          scorer.next =
            scorer
              .scorer
              .score(&mut leaf_collector, accept_docs, window_min, window_max)?;
        }

        top = *self.scorers.update_top()?;
        let top_next = self.scorers.compare.bulk_scorer_and_nexts[top].next;
        if top_next >= window_max {
          break;
        }
      }

      collector.set_scorer(&mut self.top_level_scorable)?;

      let mut window_doc = self.window_matches.next_set_bit(0);
      while window_doc != NO_MORE_DOCS as usize {
        let doc = window_min + window_doc as i32;
        self.top_level_scorable.score = self.window_scores[window_doc];
        collector.collect(doc, &mut self.top_level_scorable)?;
        window_doc = self.window_matches.next_set_bit(window_doc + 1);
      }

      self.window_matches.clear()?;
      self.window_scores.fill(0.0);
    }

    let top = self
      .scorers
      .top()
      .ok_or_else(|| LuceneError::illegal_state("scorers is empty"))?;
    Ok(self.scorers.compare.bulk_scorer_and_nexts[*top].next)
  }

  fn cost(&mut self) -> Result<i64> {
    let mut cost = 0i64;
    for slot in 1..=self.scorers.size() {
      if let Some(index) = self.scorers.heap()[slot] {
        cost += self.scorers.compare.bulk_scorer_and_nexts[index]
          .scorer
          .cost()?;
      }
    }
    Ok(cost)
  }
}
struct BulkScorerAndNext<BS> {
  scorer: BS,
  next: i32,
}
impl<BS> BulkScorerAndNext<BS> {
  fn new(scorer: BS) -> Self {
    Self { scorer, next: 0 }
  }
}
struct BulkScorerAndNextCmp<BS> {
  bulk_scorer_and_nexts: Box<[BulkScorerAndNext<BS>]>,
}
impl<BS> Compare<usize> for BulkScorerAndNextCmp<BS>
where
  BS: BulkScorer,
{
  fn less_than(&self, a: &usize, b: &usize) -> Result<bool> {
    Ok(self.bulk_scorer_and_nexts[*a].next < self.bulk_scorer_and_nexts[*b].next)
  }
}

struct LeafCollectorImpl<'a> {
  window_matches: &'a mut FixedBitSet,
  top_level_scorable: &'a mut SimpleScorable,
  window_min: i32,
  window_scores: &'a mut [f32; WINDOW_SIZE],
}
impl<'a> LeafCollectorImpl<'a> {
  fn new(
    window_matches: &'a mut FixedBitSet,
    top_level_scorable: &'a mut SimpleScorable,
    window_min: i32,
    window_scores: &'a mut [f32; WINDOW_SIZE],
  ) -> Self {
    Self {
      window_matches,
      top_level_scorable,
      window_min,
      window_scores,
    }
  }
}

impl Display for LeafCollectorImpl<'_> {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl<'a> LeafCollector for LeafCollectorImpl<'a> {
  type CompetitiveIterator<'view>
    = crate::core::search::doc_id_set_iterator::EmptyDISI
  where
    Self: 'view;

  fn set_scorer<SC: Scorable + ?Sized>(&mut self, scorer: &mut SC) -> Result<()> {
    if self.top_level_scorable.min_competitive_score != 0.0 {
      scorer.set_min_competitive_score(self.top_level_scorable.min_competitive_score)?
    }
    Ok(())
  }

  fn collect<SC: Scorable + ?Sized>(&mut self, doc: i32, scorer: &mut SC) -> Result<()> {
    let delta = (doc - self.window_min) as usize;
    self.window_matches.set(delta)?;
    self.window_scores[delta] = CoreHelper::max_f32(self.window_scores[delta], scorer.score()?);
    Ok(())
  }
}
