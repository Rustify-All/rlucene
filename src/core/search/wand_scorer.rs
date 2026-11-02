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
use crate::core::search::disi_priority_queue::DisiPriorityQueue;
use crate::core::search::disi_wrapper::DisiWrapper;
use crate::core::search::doc_id_set_iterator::BitSetIteratorAccess;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::scorable::{ChildScorable, Scorable};
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::{Scorer, TwoPhaseState};
use crate::core::search::scorer_util::ScorerUtil;
use crate::core::search::two_phase_iterator::{
  TwoPhaseIterator, TwoPhaseIteratorAsDocIdSetIterator,
};
use crate::core::util::TryIntoInt;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::math_util::MathUtil;
use smallvec::SmallVec;

/// Implements the WAND (Weak AND) algorithm for dynamic pruning as described in
/// “Efficient Query Evaluation using a Two-Level Retrieval Process”
/// by Broder, Carmel, Herscovici, Soffer and Zien.
///
/// It is enhanced with techniques from
/// “Faster Top-k Document Retrieval Using Block-Max Indexes” by Ding and Suel.
///
/// For [`ScoreMode::TopScores`], this scorer maintains a feedback loop with the
/// collector in order to know, at any time, the minimum score that is required
/// for a hit to be competitive.
///
///
/// The implementation supports both:
/// - `minCompetitiveScore` by enforcing `∑ max_score >= minCompetitiveScore`
/// - `minShouldMatch` by enforcing `freq >= minShouldMatch`
///
/// It keeps sub-scorers in three different places:
/// - **tail**: a heap that contains scorers that are *behind* the desired doc ID,
///   ordered by cost so that the least costly ones can be advanced first.
/// - **lead**: a linked list of scorers that are positioned on the desired doc ID
/// - **head**: a heap that contains scorers which are *beyond* the desired doc ID,
///   ordered by doc ID in order to move quickly to the next candidate.
///
///
/// When `score_mode == ScoreMode::TopScores`, it leverages
/// [`Scorer::get_max_score`] from each scorer in order to know when it may call
/// [`DocIdSetIterator::advance`] rather than [`DocIdSetIterator::next_doc`] to move
/// to the next competitive hit.
///
/// When `score_mode != ScoreMode::TopScores`, block-max scoring related logic is skipped.
///
/// Finding the next match consists of:
/// 1. Setting the desired doc ID to the least entry in `head`.
/// 2. Advancing `tail` until there is a match, by meeting the configured
///    constraints:
///    - `freq >= minShouldMatch`
///    - and/or `∑ max_score >= minCompetitiveScore`.
///
/// `TOP_SCORES` is selected from the score mode before document iteration starts.
pub struct WANDScorer<S, const TOP_SCORES: bool> {
  disi: TwoPhaseIteratorAsDocIdSetIterator<TwoPhaseIteratorImpl<S, TOP_SCORES>>,
}

impl<S, const TOP_SCORES: bool> WANDScorer<S, TOP_SCORES>
where
  S: Scorer,
{
  pub(crate) fn new(
    scorers: Vec<S>,
    min_should_match: i32,
    score_mode: ScoreMode,
    lead_cost: i64,
  ) -> Result<WANDScorer<S, TOP_SCORES>> {
    let v = DocIdSetIteratorImpl::new(scorers, min_should_match, score_mode, lead_cost)?;
    let tpi = TwoPhaseIteratorImpl::new(v);
    Ok(WANDScorer {
      disi: TwoPhaseIteratorAsDocIdSetIterator::new(tpi),
    })
  }
}

impl<S, const TOP_SCORES: bool> Scorable for WANDScorer<S, TOP_SCORES>
where
  S: Scorer + 'static,
{
  type Child<'a>
    = &'a mut S
  where
    Self: 'a;

  fn score(&mut self) -> Result<f32> {
    let disi = &mut self.disi.two_phase_iterator.approximation;
    // we need to know about all matches
    disi.advance_all_tail()?;
    let mut lead_score = disi.lead_score;

    if !TOP_SCORES {
      // With TOP_SCORES, the score was already computed on the fly.
      let mut cur = disi.lead;
      while let Some(idx) = cur {
        lead_score += disi.all_scorers[idx].scorer.score()? as f64;
        cur = disi.all_scorers[idx].next;
      }
    }

    Ok(lead_score as f32)
  }

  fn set_min_competitive_score(&mut self, min_score: f32) -> Result<()> {
    // Let this disjunction know about the new min score so that it can skip
    // over clauses that produce low scores.
    let disi = &mut self.disi.two_phase_iterator.approximation;
    debug_assert_eq!(
      disi.score_mode,
      ScoreMode::TopScores,
      "minCompetitiveScore can only be set for ScoreMode.TOP_SCORES, but got: {:?}",
      disi.score_mode
    );
    debug_assert!(min_score >= 0f32);
    let scaled_min_score = scale_min_score(min_score, disi.scaling_factor);
    debug_assert!(scaled_min_score >= disi.min_competitive_score);
    disi.min_competitive_score = scaled_min_score;
    Ok(())
  }

  fn get_children(&mut self) -> Result<Vec<ChildScorable<Self::Child<'_>>>> {
    let disi = &mut self.disi.two_phase_iterator.approximation;
    disi.advance_all_tail()?;
    let mut current = disi.lead;
    let mut scorers: Vec<_> = disi.all_scorers.iter_mut().map(Some).collect();
    let mut children = Vec::with_capacity(disi.freq);
    while let Some(idx) = current {
      let wrapper = scorers[idx]
        .take()
        .ok_or_else(|| LuceneError::illegal_state("duplicate WAND lead"))?;
      current = wrapper.next;
      children.push(ChildScorable::new(&mut wrapper.scorer, "SHOULD"));
    }
    Ok(children)
  }

  fn cost(&self) -> Result<i64> {
    self.disi.cost()
  }
}

impl<S, const TOP_SCORES: bool> crate::core::search::scorable::FixedScore
  for WANDScorer<S, TOP_SCORES>
{
}

impl<S, const TOP_SCORES: bool> DocIdSetIterator for WANDScorer<S, TOP_SCORES>
where
  S: Scorer + 'static,
{
  fn doc_id(&self) -> i32 {
    DocIdSetIterator::doc_id(&self.disi)
  }
  fn next_doc(&mut self) -> Result<i32> {
    DocIdSetIterator::next_doc(&mut self.disi)
  }
  fn advance(&mut self, target: i32) -> Result<i32> {
    DocIdSetIterator::advance(&mut self.disi, target)
  }
  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    DocIdSetIterator::slow_advance(&mut self.disi, target)
  }
  fn cost(&self) -> Result<i64> {
    DocIdSetIterator::cost(&self.disi)
  }
}
impl<S, const TOP_SCORES: bool> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for WANDScorer<S, TOP_SCORES>
where
  S: Scorer + 'static,
{
}
impl<S, const TOP_SCORES: bool> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for WANDScorer<S, TOP_SCORES>
where
  S: Scorer + 'static,
{
}
impl<S, const TOP_SCORES: bool> ApproximationDISI for WANDScorer<S, TOP_SCORES>
where
  S: Scorer + 'static,
{
  fn approx_doc_id(&self) -> i32 {
    self.disi.two_phase_iterator.approx_doc_id()
  }
  fn approx_next_doc(&mut self) -> Result<i32> {
    self.disi.two_phase_iterator.approx_next_doc()
  }
  fn approx_advance(&mut self, target: i32) -> Result<i32> {
    self.disi.two_phase_iterator.approx_advance(target)
  }
  fn approx_slow_advance(&mut self, target: i32) -> Result<i32> {
    self.disi.two_phase_iterator.approx_slow_advance(target)
  }
  fn approx_cost(&self) -> Result<i64> {
    self.disi.two_phase_iterator.approx_cost()
  }
}
impl<S, const TOP_SCORES: bool> ApproximationBitSetAccess for WANDScorer<S, TOP_SCORES>
where
  S: Scorer + 'static,
{
  fn approx_is_bit_iter(&self) -> bool {
    self.disi.two_phase_iterator.approx_is_bit_iter()
  }
  fn approx_get(&self, index: usize) -> Result<bool> {
    self.disi.two_phase_iterator.approx_get(index)
  }
  fn approx_set_doc_id(&mut self, doc: i32) -> Result<()> {
    self.disi.two_phase_iterator.approx_set_doc_id(doc)
  }
  fn approx_bit_set_length(&self) -> Result<usize> {
    self.disi.two_phase_iterator.approx_bit_set_length()
  }
}

impl<S, const TOP_SCORES: bool> TwoPhaseIterator for WANDScorer<S, TOP_SCORES>
where
  S: Scorer + 'static,
{
  fn matches(&mut self) -> Result<bool> {
    self.disi.two_phase_iterator.matches()
  }
  fn match_cost(&self) -> f32 {
    self.disi.two_phase_iterator.match_cost()
  }
}

impl<S, const TOP_SCORES: bool> Scorer for WANDScorer<S, TOP_SCORES>
where
  S: Scorer + 'static,
{
  fn scoring_doc_id(&mut self) -> Result<i32> {
    Ok(self.disi.two_phase_iterator.approximation.doc)
  }

  fn advance_shallow(&mut self, target: i32) -> Result<i32> {
    let disi = &mut self.disi.two_phase_iterator.approximation;
    // Propagate to improve score bounds.
    for w in &mut disi.all_scorers {
      if w.scorer.scoring_doc_id()? < target {
        w.scorer.advance_shallow(target)?;
      }
    }
    if target <= disi.upto {
      return Ok(disi.upto);
    }
    Ok(NO_MORE_DOCS)
  }

  fn get_max_score(&mut self, upto: i32) -> Result<f32> {
    let all_scorers = self
      .disi
      .two_phase_iterator
      .approximation
      .all_scorers
      .as_mut();

    let mut max_score_sum = 0f64;
    let len = all_scorers.len();
    for w in all_scorers {
      let scorer = &mut w.scorer;

      if scorer.scoring_doc_id()? <= upto {
        max_score_sum += scorer.get_max_score(upto)? as f64;
      }
    }
    Ok(MathUtil::sum_upper_bound(max_score_sum, len.try_convert()?) as f32)
  }

  fn has_two_phase_iterator(&self) -> TwoPhaseState {
    TwoPhaseState::Yes
  }
}

pub struct DocIdSetIteratorImpl<S, const TOP_SCORES: bool> {
  all_scorers: Box<[DisiWrapper<S>]>,
  doc: i32,
  score_mode: ScoreMode,
  upto: i32,
  /// priority queue of scorers that are too advanced compared to the current
  /// doc. Ordered by doc ID.
  pub(crate) head: DisiPriorityQueue,
  /// list of scorers which 'lead' the iteration and are currently
  /// positioned on 'doc'. This is sometimes called the 'pivot' in
  /// some descriptions of WAND (Weak AND).
  pub(crate) lead: Option<usize>,
  pub(crate) freq: usize,
  pub(crate) lead_cost: i64,
  /// score of the leads
  pub(crate) lead_score: f64,
  /// priority queue of scorers which are behind the current doc.
  /// Ordered by maxScore.
  pub(crate) tail: Vec<usize>,
  /// sum of max scores of scorers in tail
  pub(crate) tail_max_score: i64,
  pub(crate) tail_size: usize,
  /// scaled min competitive score
  min_competitive_score: i64,
  pub(crate) min_should_match: usize,
  /// cost from Lucene
  pub(crate) cost: i64,
  /// scalingFactor in Lucene
  scaling_factor: i32,
}
impl<S, const TOP_SCORES: bool> DocIdSetIteratorImpl<S, TOP_SCORES>
where
  S: Scorer,
{
  fn new(
    mut scorers: Vec<S>,
    min_should_match: i32,
    score_mode: ScoreMode,
    lead_cost: i64,
  ) -> Result<DocIdSetIteratorImpl<S, TOP_SCORES>> {
    debug_assert_eq!(TOP_SCORES, score_mode == ScoreMode::TopScores);
    let num_scorers = scorers.len();

    if i64::from(min_should_match) >= num_scorers as i64 {
      return Err(LuceneError::illegal_argument(
        "minShouldMatch should be < the number of scorers",
      ));
    }

    debug_assert!(
      min_should_match >= 0,
      "minShouldMatch should not be negative, but got {}",
      min_should_match
    );

    let scaling_factor = if TOP_SCORES {
      // To avoid accuracy issues with floating-point numbers, this scorer operates on scaled longs.
      // How do you choose the scaling factor? The thing is that we want to retain as many
      // significant bits as possible, but not too many, otherwise operations on longs would be more
      // precise than the equivalent operations on their unscaled counterparts and we might skip too
      // many hits. So we compute the maximum possible score produced by this scorer, which is the
      // sum of the maximum scores of each clause, and compute a scaling factor that would preserve
      // 24 bits of accuracy - the number of mantissa bits of single-precision floating-point
      // numbers.
      let mut max_score_sum_double = 0f64;
      for scorer in scorers.iter_mut() {
        scorer.advance_shallow(0)?;
        let max_score = scorer.get_max_score(NO_MORE_DOCS)?;
        max_score_sum_double += max_score as f64;
      }
      let max_score_sum = MathUtil::sum_upper_bound(max_score_sum_double, num_scorers as i32);
      scaling_factor(max_score_sum as f32)?
    } else {
      0
    };

    let mut all_scorers = Vec::with_capacity(num_scorers);
    let mut cost: SmallVec<[i64; 8]> = SmallVec::with_capacity(num_scorers);
    for scorer in scorers {
      cost.push(DocIdSetIterator::cost(&scorer)?);
      all_scorers.push(DisiWrapper::new(scorer)?);
    }

    let head = DisiPriorityQueue::new(num_scorers);
    let tail = vec![0usize; num_scorers];

    let min_should_match: usize = min_should_match.try_convert()?;
    let mut this = Self {
      all_scorers: all_scorers.into_boxed_slice(),
      doc: -1,
      score_mode,
      upto: -1,
      head,
      lead: None,
      freq: 0,
      lead_cost,
      lead_score: 0.0,
      tail,
      tail_max_score: 0,
      tail_size: 0,
      min_competitive_score: 0,
      min_should_match,
      cost: 0,
      scaling_factor,
    };

    for idx in 0..num_scorers {
      // Ideally we would pass true when scoreMode == TOP_SCORES and false otherwise, but this would
      // break the optimization as there could then be 3 different impls of DocIdSetIterator
      // (ImpactsEnum, PostingsEnum and <Else>). So we pass true to favor disjunctions sorted by
      // descending score as opposed to non-scoring disjunctions whose minShouldMatch is greater
      // than 1.
      this.add_unpositioned_lead(idx);
    }

    this.cost = ScorerUtil::cost_with_min_should_match(cost, num_scorers, min_should_match)?;

    Ok(this)
  }
  /// Add a disi to the linked list of leads.
  fn add_lead(&mut self, lead_idx: usize) -> Result<()> {
    self.all_scorers[lead_idx].next = self.lead;
    self.lead = Some(lead_idx);
    self.freq += 1;
    if TOP_SCORES {
      let scorer = self.all_scorers[lead_idx].scorer.score()?;
      self.lead_score += scorer as f64;
    }
    Ok(())
  }
  /// Add a disi to the linked list of leads.
  fn add_unpositioned_lead(&mut self, lead_idx: usize) {
    debug_assert!(self.all_scorers[lead_idx].doc == -1);
    self.all_scorers[lead_idx].next = self.lead;
    self.lead = Some(lead_idx);
    self.freq += 1;
  }
  /// Make sure all disis in 'head' are on or after 'target'.
  fn push_back_leads(&mut self, target: i32) -> Result<()> {
    let mut cur = self.lead;
    while let Some(idx) = cur {
      let evicted = self.insert_tail_with_overflow(idx);

      if let Some(evicted_idx) = evicted {
        let new_doc = self.all_scorers[evicted_idx].scorer.advance(target)?;
        self.all_scorers[evicted_idx].doc = new_doc;
        self.head.add(evicted_idx, &self.all_scorers);
      }
      cur = self.all_scorers[idx].next;
    }
    self.lead = None;
    Ok(())
  }

  /// Make sure all disis in 'head' are on or after 'target'.
  fn advance_head(&mut self, target: i32) -> Result<Option<usize>> {
    let mut head_top = self.head.top();

    while let Some(ref top_idx) = head_top {
      let top_idx = *top_idx;
      if self.all_scorers[top_idx].doc >= target {
        break;
      }
      let evicted = self.insert_tail_with_overflow(top_idx);

      if let Some(evicted_idx) = evicted {
        let new_doc = self.all_scorers[evicted_idx].scorer.advance(target)?;
        self.all_scorers[evicted_idx].doc = new_doc;
        head_top = Some(self.head.update_top_with(evicted_idx, &self.all_scorers));
      } else {
        self.head.pop(&self.all_scorers);
        head_top = self.head.top();
      }
    }

    Ok(head_top)
  }

  fn advance_tail(&mut self, idx: usize) -> Result<()> {
    let new_doc = self.all_scorers[idx].scorer.advance(self.doc)?;
    self.all_scorers[idx].doc = new_doc;

    if new_doc == self.doc {
      self.add_lead(idx)?;
    } else {
      self.head.add(idx, &self.all_scorers);
    }

    Ok(())
  }
  /// Pop the entry from the 'tail' that has the greatest score contribution,
  /// advance it to the current doc and then add it to 'lead' or 'head' depending on whether it matches.
  fn advance_tail_top(&mut self) -> Result<()> {
    let top = self.pop_tail();
    self.advance_tail(top)
  }
  fn update_max_scores(&mut self, target: i32) -> Result<()> {
    let mut new_upto = NO_MORE_DOCS;
    // If we have entries in 'head', we treat them all as leads and take the minimum of their next
    // block boundaries as a next boundary.
    // We don't take entries in 'tail' into account on purpose: 'tail' is supposed to contain the
    // least score contributors, and taking them into account might not move the boundary fast
    // enough, so we'll waste CPU re-computing the next boundary all the time.
    // Likewise, we ignore clauses whose cost is greater than the lead cost to avoid recomputing
    // per-window max scores over and over again. In the event when this makes us compute upTo as
    // NO_MORE_DOCS, this scorer will effectively implement WAND rather than block-max WAND.
    {
      let iter = self.head.iter();
      for idx in iter {
        let w = &mut self.all_scorers[idx];

        if w.doc <= new_upto && w.cost <= self.lead_cost {
          let shallow = w.scorer.advance_shallow(w.doc)?;
          new_upto = new_upto.min(shallow);
        }
      }
    }
    // Only look at the tail if none of the `head` clauses had a block we could reuse and if its
    // cost is less than or equal to the lead cost.
    if new_upto == NO_MORE_DOCS
      && self.tail_size > 0
      && self.all_scorers[self.tail[0]].cost <= self.lead_cost
    {
      let top_idx = self.tail[0];

      new_upto = self.all_scorers[top_idx].scorer.advance_shallow(target)?;
      // upTo must be on or after the least `head` doc
      if let Some(ht) = self.head.top() {
        new_upto = new_upto.max(self.all_scorers[ht].doc);
      }
    }

    self.upto = new_upto;

    {
      // Now update the max scores of clauses that are before upTo.
      let iter = self.head.iter();
      for idx in iter {
        let w = &mut self.all_scorers[idx];
        if w.doc <= self.upto {
          let v = w.scorer.get_max_score(new_upto)?;
          w.scaled_max_score = scale_max_score(v, self.scaling_factor);
        }
      }
    }

    self.tail_max_score = 0;

    for i in 0..self.tail_size {
      let idx = self.tail[i];
      let w = &mut self.all_scorers[idx];
      w.scorer.advance_shallow(target)?;
      let v = w.scorer.get_max_score(self.upto)?;

      w.scaled_max_score = scale_max_score(v, self.scaling_factor);

      Self::up_heap_max_score(&mut self.tail, i, &self.all_scorers); // the heap might need to be reordered

      self.tail_max_score += self.all_scorers[idx].scaled_max_score;
    }
    // We need to make sure that entries in 'tail' alone cannot match
    // a competitive hit.
    while self.tail_size > 0 && self.tail_max_score >= self.min_competitive_score {
      let idx = self.pop_tail();

      let new_doc = self.all_scorers[idx].scorer.advance(target)?;
      self.all_scorers[idx].doc = new_doc;
      self.head.add(idx, &self.all_scorers);
    }

    Ok(())
  }

  /// Update upTo and maximum scores of sub scorers so that upTo is greater than or equal to the next candidate after target,
  /// i.e. the top of `head`.
  fn move_to_next_block(&mut self, mut target: i32) -> Result<()> {
    debug_assert!(self.lead.is_none());

    while self.upto < NO_MORE_DOCS {
      if self.head.size() == 0 {
        // All clauses could fit in the tail, which means that the sum of the
        // maximum scores of sub clauses is less than the minimum competitive score.
        // Move to the next block until this condition becomes false.
        target = target.max(self.upto + 1);
        self.update_max_scores(target)?;
      } else {
        let top_idx = self
          .head
          .top()
          .ok_or_else(|| LuceneError::illegal_state("no top available"))?;

        let top_doc = self.all_scorers[top_idx].doc;
        // We have a next candidate but it's not in the current block. We need to
        // move to the next block in order to not miss any potential hits between
        // `target` and `head.top().doc`.
        if top_doc > self.upto {
          debug_assert!(top_doc >= target);
          self.update_max_scores(target)?;
          break;
        } else {
          break;
        }
      }
    }

    debug_assert!(
      self
        .head
        .top()
        .is_none_or(|idx| self.all_scorers[idx].doc <= self.upto)
    );
    debug_assert!(self.upto >= target);

    Ok(())
  }
  /// Set 'doc' to the next potential match, and move all disis of 'head' that are on this doc into 'lead'.
  fn move_to_next_candidate(&mut self) -> Result<()> {
    // The top of `head` defines the next potential match
    // pop all documents which are on this doc
    let lead_idx = self.head.pop(&self.all_scorers);
    self.lead = Some(lead_idx);
    debug_assert!(self.doc == self.all_scorers[lead_idx].doc);
    self.all_scorers[lead_idx].next = None;
    self.freq = 1;

    if TOP_SCORES {
      self.lead_score = self.all_scorers[lead_idx].scorer.score()? as f64;
    }
    while self.head.size() > 0 {
      let top_idx = match self.head.top() {
        Some(idx) => idx,
        None => return Err(LuceneError::illegal_state("head top is empty")),
      };

      if self.all_scorers[top_idx].doc != self.doc {
        break;
      }

      let popped = self.head.pop(&self.all_scorers);
      self.add_lead(popped)?;
    }

    Ok(())
  }

  /// Advance all entries from the tail to know about all matches on the current doc.
  fn advance_all_tail(&mut self) -> Result<()> {
    // we return the next doc when the sum of the scores of the potential
    // matching clauses is high enough but some of the clauses in 'tail' might
    // match as well
    // since we are advancing all clauses in tail, we just iterate the array
    // without reorganizing the PQ
    for i in (0..self.tail_size).rev() {
      self.advance_tail(self.tail[i])?;
    }
    self.tail_size = 0;
    self.tail_max_score = 0;
    debug_assert!(self.ensure_consistent()?);
    Ok(())
  }

  /// Insert an entry in 'tail' and evict the least-costly scorer if full.
  fn insert_tail_with_overflow(&mut self, s: usize) -> Option<usize> {
    let s_score = self.all_scorers[s].scaled_max_score;

    if self.tail_max_score + s_score < self.min_competitive_score
      || self.tail_size + 1 < self.min_should_match
    {
      // we have free room for this new entry
      self.add_tail(s);
      self.tail_max_score += s_score;
      None
    } else if self.tail_size == 0 {
      Some(s)
    } else {
      let top = self.tail[0];
      if !Self::greater_max_score(&self.all_scorers[top], &self.all_scorers[s]) {
        Some(s)
      } else {
        // swap top and s
        self.tail[0] = s;
        Self::down_heap_max_score(&mut self.tail, self.tail_size, &self.all_scorers);
        self.tail_max_score =
          self.tail_max_score - self.all_scorers[top].scaled_max_score + s_score;
        Some(top)
      }
    }
  }

  /// Add an entry to 'tail'. Fails if over capacity.
  fn add_tail(&mut self, idx: usize) {
    self.tail[self.tail_size] = idx;
    Self::up_heap_max_score(&mut self.tail, self.tail_size, &self.all_scorers);
    self.tail_size += 1;
  }

  /// Pop the least-costly scorer from 'tail'.
  fn pop_tail(&mut self) -> usize {
    debug_assert!(self.tail_size > 0);
    let result = self.tail[0];
    self.tail_size -= 1;
    self.tail[0] = self.tail[self.tail_size];
    Self::down_heap_max_score(&mut self.tail, self.tail_size, &self.all_scorers);
    self.tail_max_score -= self.all_scorers[result].scaled_max_score;

    result
  }

  /// Heap helpers
  fn up_heap_max_score(heap: &mut [usize], mut i: usize, all: &[DisiWrapper<S>]) {
    let node = heap[i];

    while let Some(j) = DisiPriorityQueue::parent_node(i) {
      if !Self::greater_max_score(&all[node], &all[heap[j]]) {
        break;
      }
      heap[i] = heap[j];
      i = j;
    }

    heap[i] = node;
  }

  fn down_heap_max_score(heap: &mut [usize], size: usize, all: &[DisiWrapper<S>]) {
    let mut i = 0;
    let node = heap[0];
    let mut j = DisiPriorityQueue::left_node(i);

    if j < size {
      let mut k = DisiPriorityQueue::right_node(j);

      if k < size && Self::greater_max_score(&all[heap[k]], &all[heap[j]]) {
        j = k;
      }

      if Self::greater_max_score(&all[heap[j]], &all[node]) {
        loop {
          heap[i] = heap[j];
          i = j;
          j = DisiPriorityQueue::left_node(i);
          k = DisiPriorityQueue::right_node(j);

          if k < size && Self::greater_max_score(&all[heap[k]], &all[heap[j]]) {
            j = k;
          }

          if j >= size || !Self::greater_max_score(&all[heap[j]], &all[node]) {
            break;
          }
        }
        heap[i] = node;
      }
    }
  }

  /// In the tail, we want to get first entries that produce the maximum scores and in case of ties
  /// (eg. constant-score queries), those that have the least cost so that they are likely to advance further.
  fn greater_max_score(w1: &DisiWrapper<S>, w2: &DisiWrapper<S>) -> bool {
    if w1.scaled_max_score > w2.scaled_max_score {
      true
    } else if w1.scaled_max_score < w2.scaled_max_score {
      false
    } else {
      w1.cost < w2.cost
    }
  }
  // Returns a `bool` so it can be called from an assertion.
  // the return value is useless: it always returns true
  fn ensure_consistent(&mut self) -> Result<bool> {
    if TOP_SCORES {
      let mut max_score_sum: i64 = 0;

      for i in 0..self.tail_size {
        let idx = self.tail[i];
        let w = &self.all_scorers[idx];

        debug_assert!(w.doc < self.doc);
        max_score_sum = max_score_sum
          .checked_add(w.scaled_max_score)
          .ok_or_else(|| LuceneError::number_overflow("scaled maximum score sum overflowed"))?;
      }

      debug_assert!(
        max_score_sum == self.tail_max_score,
        "tailMaxScore mismatch: {max_score_sum} vs {}",
        self.tail_max_score
      );

      let mut lead_scores: Vec<f32> = Vec::with_capacity(self.freq);

      let mut cur = self.lead;
      while let Some(idx) = cur {
        let w = &mut self.all_scorers[idx];
        debug_assert!(w.doc == self.doc);
        lead_scores.push(w.scorer.score()?);
        cur = w.next;
      }
      // Make sure to recompute the sum in the same order to get the same floating point rounding
      // errors.
      lead_scores.reverse();

      let mut recomputed_lead_score = 0f64;
      for score in &lead_scores {
        recomputed_lead_score += *score as f64;
      }

      debug_assert!(
        (recomputed_lead_score == self.lead_score),
        "leadScore mismatch: recomputed={recomputed_lead_score} stored={}",
        self.lead_score
      );

      debug_assert!(
        self.min_competitive_score == 0
          || self.tail_max_score < self.min_competitive_score
          || self.tail_size < self.min_should_match,
      );

      debug_assert!(self.doc <= self.upto);
    }

    let head_iter = self.head.iter();
    for idx in head_iter {
      let w = &self.all_scorers[idx];

      if self.lead.is_none() {
        debug_assert!(w.doc >= self.doc);
      } else {
        debug_assert!(w.doc > self.doc);
      }
    }

    Ok(true)
  }
}
impl<S, const TOP_SCORES: bool> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for DocIdSetIteratorImpl<S, TOP_SCORES>
where
  S: Scorer,
{
}
impl<S, const TOP_SCORES: bool> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for DocIdSetIteratorImpl<S, TOP_SCORES>
where
  S: Scorer,
{
}

impl<S, const TOP_SCORES: bool> DocIdSetIterator for DocIdSetIteratorImpl<S, TOP_SCORES>
where
  S: Scorer,
{
  fn doc_id(&self) -> i32 {
    self.doc
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.advance(self.doc + 1)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    // Move 'lead' iterators back to the tail
    self.push_back_leads(target)?;
    // Make sure `head` is also on or beyond `target`
    let mut head_top = self.advance_head(target)?;

    if TOP_SCORES && head_top.is_none_or(|idx| self.all_scorers[idx].doc > self.upto) {
      // Update score bounds if necessary
      self.move_to_next_block(target)?;
      debug_assert!(self.upto >= target);

      head_top = self.head.top();
    }
    if let Some(idx) = head_top {
      self.doc = self.all_scorers[idx].doc;
      Ok(self.doc)
    } else {
      self.doc = NO_MORE_DOCS;
      Ok(self.doc)
    }
  }

  fn cost(&self) -> Result<i64> {
    Ok(self.cost)
  }
}

pub struct TwoPhaseIteratorImpl<S, const TOP_SCORES: bool> {
  approximation: DocIdSetIteratorImpl<S, TOP_SCORES>,
}
impl<S, const TOP_SCORES: bool> TwoPhaseIteratorImpl<S, TOP_SCORES> {
  pub fn new(approximation: DocIdSetIteratorImpl<S, TOP_SCORES>) -> Self {
    Self { approximation }
  }
}
impl<S, const TOP_SCORES: bool> ApproximationDISI for TwoPhaseIteratorImpl<S, TOP_SCORES>
where
  S: Scorer,
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
impl<S, const TOP_SCORES: bool> ApproximationBitSetAccess for TwoPhaseIteratorImpl<S, TOP_SCORES>
where
  S: Scorer,
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

impl<S, const TOP_SCORES: bool> TwoPhaseIterator for TwoPhaseIteratorImpl<S, TOP_SCORES>
where
  S: Scorer,
{
  fn matches(&mut self) -> Result<bool> {
    let approx = &mut self.approximation;

    debug_assert!(approx.lead.is_none());

    approx.move_to_next_candidate()?;

    let mut scaled_lead_score: i64 = 0;

    if TOP_SCORES {
      scaled_lead_score = scale_max_score(
        MathUtil::sum_upper_bound(approx.lead_score, FLOAT_MANTISSA_BITS) as f32,
        approx.scaling_factor,
      );
    }

    while scaled_lead_score < approx.min_competitive_score || approx.freq < approx.min_should_match
    {
      debug_assert!(approx.ensure_consistent()?);

      if scaled_lead_score + approx.tail_max_score < approx.min_competitive_score
        || approx.freq + approx.tail_size < approx.min_should_match
      {
        return Ok(false);
      } else {
        // a match on doc is still possible, try to
        // advance scorers from the tail
        let prev_lead = approx.lead;
        approx.advance_tail_top()?;

        if TOP_SCORES && approx.lead != prev_lead {
          debug_assert!(match approx.lead {
            Some(lead) => prev_lead == approx.all_scorers[lead].next,
            None => false,
          });

          scaled_lead_score = scale_max_score(
            MathUtil::sum_upper_bound(approx.lead_score, FLOAT_MANTISSA_BITS) as f32,
            approx.scaling_factor,
          );
        }
      }
    }
    debug_assert!(approx.ensure_consistent()?);
    Ok(true)
  }

  fn match_cost(&self) -> f32 {
    self.approximation.tail.len() as f32
  }
}

pub(crate) const FLOAT_MANTISSA_BITS: i32 = 24;
const MAX_SCALED_SCORE: i64 = (1_i64 << 24) - 1;

/// Return a scaling factor for the given float such that
/// `f * 2^scaling_factor` falls within the interval `[2^23, 2^24)`.
///
/// Special cases:
/// - `scalingFactor(0) = scalingFactor(MIN_VALUE) + 1`
/// - `scalingFactor(+∞) = scalingFactor(MAX_VALUE) - 1`
///
/// This ensures that values are scaled so that their significant bits fit within
/// the 24-bit mantissa of a single-precision floating-point number.
pub(crate) fn scaling_factor(f: f32) -> Result<i32> {
  if f < 0.0 {
    Err(LuceneError::illegal_argument(
      "Scores must be positive or null",
    ))
  } else if f == 0.0 {
    Ok(scaling_factor(BitUtil::F32_MIN_VALUE)? + 1)
  } else if f.is_infinite() {
    Ok(scaling_factor(f32::MAX)? - 1)
  } else {
    // Widen to f64 so even subnormal f32 values have a normal exponent.
    let d = f as f64;
    let exp = ((d.to_bits() >> 52) & 0x7ff) as i32 - 1023;
    debug_assert!(d == 0.0 || exp >= -1022);
    Ok(FLOAT_MANTISSA_BITS - 1 - exp)
  }
}
/// Scale maximum scores into an unsigned integer in order to avoid overflows
/// (only the lower 32 bits of the `u64` are used) and to prevent
/// floating-point arithmetic errors.
///
/// Values are rounded **up** to ensure that no competitive matches are missed.
pub(crate) fn scale_max_score(max_score: f32, scaling_factor: i32) -> i64 {
  debug_assert!(!max_score.is_nan());
  debug_assert!(max_score >= 0.0);
  let scaled = (max_score as f64) * (2f64.powi(scaling_factor));

  if scaled > MAX_SCALED_SCORE as f64 {
    // This happens if one scorer returns +Infty as a max score, or if the scorer returns greater
    // max scores locally than globally - which shouldn't happen with well-behaved scorers
    return MAX_SCALED_SCORE;
  }
  scaled.ceil() as i64
}
/// Scale minimum competitive scores in the same way as maximum scores,
/// except values are rounded **down** in order to ensure that no matches
/// are missed during pruning.
fn scale_min_score(min_score: f32, scaling_factor: i32) -> i64 {
  debug_assert!(min_score.is_finite());
  debug_assert!(min_score >= 0.0);
  let scaled = (min_score as f64) * (2f64.powi(scaling_factor));
  scaled.floor() as i64
}
