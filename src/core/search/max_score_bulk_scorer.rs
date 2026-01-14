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
use crate::core::search::disi_priority_queue::DisiPriorityQueue;
use crate::core::search::disi_wrapper::DisiWrapper;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::search::dummy::dummy_scorable::DummyScorable;
use crate::core::search::leaf_collector::LeafCollector;
use crate::core::search::scorable::Scorable;
use crate::core::search::scorer::Scorer;
use crate::core::util::TryIntoInt;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::math_util::MathUtil;

const INNER_WINDOW_SIZE: i32 = 1 << 12;
pub struct MaxScoreBulkScorer<S>
where
    S: Scorer,
{
    max_doc: i32,
    all_scorers: Vec<DisiWrapper<S>>,
    // All scorers, sorted by increasing max score.
    pub(crate) all_scorers_idx: Vec<usize>,
    scratch: Vec<usize>,
    // These are the last scorers from `allScorers` that are "essential", ie. required for a match to
    // have a competitive score.
    essential_queue: DisiPriorityQueue,
    // Index of the first essential scorer, ie. essentialQueue contains all scorers from
    // allScorers[firstEssentialScorer:]. All scorers below this index are non-essential.
    pub(crate) first_essential_scorer: usize,
    pub(crate) first_required_scorer: usize,
    // The minimum value of minCompetitiveScore that would produce a more favorable partitioning.
    pub(crate) next_min_competitive_score: f32,
    cost: i64,
    pub(crate) scorable: Score,
    pub(crate) max_score_sums: Vec<f64>,
    filter: Option<DisiWrapper<S>>,
    window_matches: Vec<u64>,
    window_scores: Vec<f64>,
    // Number of outer windows that have been evaluated
    num_outer_windows: usize,
    // Number of candidate matches so far
    num_candidates: usize,
    // Minimum window size. See #computeOuterWindowMax where we have heuristics that adjust the
    // minimum window size based on the average number of candidate matches per outer window, to keep
    // the per-window overhead under control.
    min_window_size: usize,
}
impl<S> MaxScoreBulkScorer<S>
where
    S: Scorer,
{
    pub fn new(max_doc: i32, scorers: Vec<S>, filter: Option<S>) -> Result<Self> {
        let filter = match filter {
            None => None,
            Some(f) => Some(DisiWrapper::new(f)?),
        };
        let mut all_scorers: Vec<DisiWrapper<S>> = Vec::with_capacity(scorers.len());
        let mut all_scorers_idx = Vec::with_capacity(scorers.len());
        let mut cost: i64 = 0;

        for (i, scorer) in scorers.into_iter().enumerate() {
            let w = DisiWrapper::new(scorer)?;
            cost += w.cost;
            all_scorers.push(w);
            all_scorers_idx.push(i);
        }
        let scratch = vec![0usize; all_scorers_idx.len()];
        let essential_queue = DisiPriorityQueue::new(all_scorers_idx.len() as i32);
        let max_score_sums = vec![0f64; all_scorers_idx.len()];
        let window_matches = vec![0u64; FixedBitSet::bits2words(INNER_WINDOW_SIZE as usize)];
        let window_scores = vec![0f64; INNER_WINDOW_SIZE as usize];

        Ok(Self {
            max_doc,
            all_scorers,
            all_scorers_idx,
            scratch,
            essential_queue,
            first_essential_scorer: 0,
            first_required_scorer: 0,
            next_min_competitive_score: 0.0,
            cost,
            scorable: Score::new(),
            max_score_sums,
            filter,
            window_matches,
            window_scores,

            num_outer_windows: 0,
            num_candidates: 0,
            min_window_size: 1,
        })
    }
    fn score_inner_window<LC, B>(
        &mut self,
        collector: &mut LC,
        accept_docs: Option<&B>,
        max: i32,
    ) -> Result<()>
    where
        LC: LeafCollector,
        B: Bits,
    {
        if self.filter.is_some() {
            let mut filter = self.filter.take().unwrap();
            self.score_inner_window_with_filter(collector, accept_docs, max, &mut filter)?;
            self.filter = Some(filter);
        } else if self.all_scorers_idx.len() - self.first_required_scorer >= 2 {
            self.score_inner_window_as_conjunction(collector, accept_docs, max)?;
        } else {
            let top_index = self.essential_queue.top().expect("top ie empty");
            let top2_index_opt = self.essential_queue.top2(&self.all_scorers);

            match top2_index_opt {
                Some(top2_index) => {
                    let top = &self.all_scorers[top_index];
                    let top2 = &self.all_scorers[top2_index];

                    if top2.doc - (INNER_WINDOW_SIZE / 2) >= top.doc {
                        self.score_inner_window_single_essential_clause(
                            collector,
                            accept_docs,
                            max.min(top2.doc),
                        )?;
                    } else {
                        self.score_inner_window_multiple_essential_clauses(
                            collector,
                            accept_docs,
                            max,
                        )?;
                    }
                },
                None => {
                    self.score_inner_window_single_essential_clause(collector, accept_docs, max)?;
                },
            }
        }

        Ok(())
    }

    fn score_inner_window_with_filter<LC, B>(
        &mut self,
        collector: &mut LC,
        accept_docs: Option<&B>,
        max: i32,
        filter: &mut DisiWrapper<S>,
    ) -> Result<()>
    where
        LC: LeafCollector,
        B: Bits,
    {
        let mut top_index = self.essential_queue.top().expect("top ie empty");
        {
            let top = &self.all_scorers[top_index];
            debug_assert!(top.doc < max);
        }

        let filter_doc = filter.doc;
        {
            let top = &mut self.all_scorers[top_index];
            if top.doc < filter_doc {
                let v = top.iterator_mut().advance(filter_doc)?;
                top.doc = v;
            }
        }

        let inner_window_min = self.all_scorers[top_index].doc;
        let inner_window_max = std::cmp::min(max, inner_window_min + INNER_WINDOW_SIZE);
        while self.all_scorers[top_index].doc < inner_window_max {
            let top_doc = self.all_scorers[top_index].doc;
            debug_assert!(filter.doc <= top_doc);

            if filter.doc < top_doc {
                let v = filter.iterator_mut().advance(top_doc)?;
                filter.doc = v;
            }

            if filter.doc != self.all_scorers[top_index].doc {
                loop {
                    let fdoc = filter.doc;
                    {
                        let top = &mut self.all_scorers[top_index];
                        let v = top.iterator_mut().advance(fdoc)?;
                        top.doc = v;
                    }
                    top_index = self.essential_queue.update_top(&self.all_scorers);
                    if self.all_scorers[top_index].doc >= filter.doc {
                        break;
                    }
                }
            } else {
                let doc = self.all_scorers[top_index].doc;
                let m = {
                    let accepted = match accept_docs {
                        None => true,
                        Some(bits) => bits.get(doc as usize)?,
                    };
                    accepted && filter.matches_may_none()?
                };

                let mut score = 0f64;
                loop {
                    if m {
                        let s = {
                            let top = &mut self.all_scorers[top_index];
                            top.score()? as f64
                        };
                        score += s;
                    }

                    {
                        let top = &mut self.all_scorers[top_index];
                        let v = top.iterator_mut().next_doc()?;
                        top.doc = v;
                    }
                    top_index = self.essential_queue.update_top(&self.all_scorers);

                    if self.all_scorers[top_index].doc != doc {
                        break;
                    }
                }

                if m {
                    self.score_non_essential_clauses(
                        collector,
                        doc,
                        score,
                        self.first_essential_scorer,
                    )?;
                }
            }
        }

        Ok(())
    }
    fn score_inner_window_single_essential_clause<LC, B>(
        &mut self,
        collector: &mut LC,
        accept_docs: Option<&B>,
        up_to: i32,
    ) -> Result<()>
    where
        LC: LeafCollector,
        B: Bits,
    {
        let top_index = self.essential_queue.top().expect("top ie empty");
        let (mut doc, mut score) = {
            let top = &mut self.all_scorers[top_index];
            // single essential clause in this window, we can iterate it directly and skip the bitset.
            // this is a common case for 2-clauses queries
            (top.doc, top.score()? as f64)
        };

        while doc < up_to {
            let accepted = match accept_docs {
                None => true,
                Some(bits) => bits.get(doc as usize)?,
            };
            if accepted {
                self.score_non_essential_clauses(
                    collector,
                    doc,
                    score,
                    self.first_essential_scorer,
                )?;
            }
            let top = &mut self.all_scorers[top_index];
            doc = top.iterator_mut().next_doc()?;
            score = top.score()? as f64;
        }
        let top = &mut self.all_scorers[top_index];
        let v = top.iterator_mut().doc_id();
        top.doc = v;
        self.essential_queue.update_top(&self.all_scorers);

        Ok(())
    }

    /// allScorers = [ w0, w1, w2, ..., w(n-3), w(n-2), w(n-1) ]
    ///                                   ^       ^       ^
    ///                                   |       |       |
    ///                                block B  lead2   lead1
    fn score_inner_window_as_conjunction<LC, B>(
        &mut self,
        collector: &mut LC,
        accept_docs: Option<&B>,
        max: i32,
    ) -> Result<()>
    where
        LC: LeafCollector,
        B: Bits,
    {
        debug_assert!(self.first_essential_scorer == self.all_scorers_idx.len() - 1);
        debug_assert!(self.first_required_scorer <= self.all_scorers_idx.len() - 2);

        let n = self.all_scorers.len();

        let last = n - 1;
        let second_last = n - 2;
        let (mut doc, max_score_sum_at_lead2) = {
            let (other_and_lead2, lead1_slice) = self.all_scorers.split_at_mut(last);
            let lead1 = &mut lead1_slice[0];

            let (_, lead2_slice) = other_and_lead2.split_at_mut(second_last);
            let lead2 = &mut lead2_slice[0];

            debug_assert!(self.essential_queue.size() == 1);
            debug_assert!(self.essential_queue.top().expect("top ie empty") == last);

            if lead1.doc < lead2.doc {
                let v = lead1.iterator_mut().advance(lead2.doc.min(max))?;
                lead1.doc = v;
            }
            (lead1.doc, self.max_score_sums[n - 2])
        };
        // TODO IMPORTANT能否降低iterator()方法的调用次数
        'outer: while doc < max {
            let (v, score) = {
                let (other_and_lead2, lead1_slice) = self.all_scorers.split_at_mut(last);
                let lead1 = &mut lead1_slice[0];

                let (other, lead2_slice) = other_and_lead2.split_at_mut(second_last);
                let lead2 = &mut lead2_slice[0];

                let accepted = match accept_docs {
                    None => true,
                    Some(bits) => bits.get(lead1.doc as usize)?,
                };

                if !accepted {
                    let v = lead1.iterator_mut().next_doc()?;
                    lead1.doc = v;
                    continue;
                }

                let mut score = lead1.score()? as f64;

                if (MathUtil::sum_upper_bound(score + max_score_sum_at_lead2, n as i32) as f32)
                    < self.scorable.min_competitive_score
                {
                    let v = lead1.iterator_mut().next_doc()?;
                    lead1.doc = v;
                    continue;
                }

                if lead2.doc < lead1.doc {
                    let v = lead2.iterator_mut().advance(lead1.doc)?;
                    lead2.doc = v;
                }
                if lead2.doc != lead1.doc {
                    let v = lead1.iterator_mut().advance(lead2.doc.min(max))?;
                    lead1.doc = v;
                    continue;
                }

                score += lead2.score()? as f64;

                for j in (self.all_scorers_idx.len() - 3..=self.first_required_scorer).rev() {
                    if (MathUtil::sum_upper_bound(score + self.max_score_sums[j], n as i32) as f32)
                        < self.scorable.min_competitive_score
                    {
                        let v = lead1.iterator_mut().next_doc()?;
                        lead1.doc = v;
                        continue 'outer;
                    }

                    let w_index = self.all_scorers_idx[j];
                    debug_assert!(w_index < second_last);
                    let w = &mut other[w_index];

                    if w.doc < lead1.doc {
                        let v = w.iterator_mut().advance(lead1.doc)?;
                        w.doc = v;
                    }
                    if w.doc != lead1.doc {
                        let v = lead1.iterator_mut().advance(w.doc.min(max))?;
                        lead1.doc = v;
                        continue 'outer;
                    }

                    score += w.score()? as f64;
                }
                (lead1.doc, score)
            };

            self.score_non_essential_clauses(collector, v, score, self.first_required_scorer)?;
            let lead1 = &mut self.all_scorers[last];
            let v = lead1.iterator_mut().next_doc()?;
            doc = v;
            lead1.doc = v;
        }

        Ok(())
    }

    fn score_inner_window_multiple_essential_clauses<LC, B>(
        &mut self,
        collector: &mut LC,
        accept_docs: Option<&B>,
        max: i32,
    ) -> Result<()>
    where
        LC: LeafCollector,
        B: Bits,
    {
        let top_index = self.essential_queue.top().expect("top ie empty");
        let mut top = &mut self.all_scorers[top_index];

        let inner_window_min = top.doc;
        let inner_window_max = std::cmp::min(max, inner_window_min + INNER_WINDOW_SIZE);
        // Collect matches of essential clauses into a bitset
        loop {
            let mut doc = top.doc;
            while doc < inner_window_max {
                let accepted = match accept_docs {
                    None => true,
                    Some(bits) => bits.get(doc as usize)?,
                };

                if accepted {
                    let i = (doc - inner_window_min) as usize;
                    self.window_matches[i >> 6] |= 1u64 << i;
                    self.window_scores[i] += top.score()? as f64;
                }
                doc = top.iterator_mut().next_doc()?;
            }

            let doc_id = top.iterator_mut().doc_id();
            top.doc = doc_id;
            let next_index = self.essential_queue.update_top(&self.all_scorers);
            top = &mut self.all_scorers[next_index];

            if top.doc >= inner_window_max {
                break;
            }
        }

        for word_index in 0..self.window_matches.len() {
            let mut bits = self.window_matches[word_index];
            self.window_matches[word_index] = 0;

            while bits != 0 {
                let ntz = bits.trailing_zeros() as usize;
                bits ^= 1u64 << ntz;

                let index = (word_index << 6) | ntz;
                let v: i32 = index.try_convert()?;
                let doc = inner_window_min + v;

                let score = self.window_scores[index];
                self.window_scores[index] = 0.0;

                self.score_non_essential_clauses(
                    collector,
                    doc,
                    score,
                    self.first_essential_scorer,
                )?;
            }
        }

        Ok(())
    }

    /// Only use essential scorers to compute the window's max doc ID, in order to avoid constantly
    /// recomputing max scores over small windows
    fn compute_outer_window_max(&mut self, window_min: i32) -> Result<i32> {
        let n = self.all_scorers_idx.len();
        let first_window_lead = self.first_essential_scorer.min(n - 1);

        let mut window_max = NO_MORE_DOCS;

        for i in first_window_lead..n {
            let index = self.all_scorers_idx[i];
            let scorer = &mut self.all_scorers[index];

            if self.filter.is_none() || scorer.cost >= self.filter.as_ref().unwrap().cost {
                let up_to = scorer.advance_shallow(scorer.doc.max(window_min))?;
                window_max = window_max.min(up_to + 1); // upTo is inclusive
            }
        }

        if n - first_window_lead > 1 {
            // The more clauses we consider to compute outer windows, the higher chances that one of these
            // clauses has a block boundary in the next few doc IDs. This situation can result in more
            // time spent computing maximum scores per outer window than evaluating hits. To avoid such
            // situations, we target at least 32 candidate matches per clause per outer window on average,
            // to make sure we amortize the cost of computing maximum scores.
            let threshold = self.num_outer_windows * 32 * n;
            if (self.num_candidates) < threshold {
                self.min_window_size = (self.min_window_size << 1).min(INNER_WINDOW_SIZE as usize);
            } else {
                self.min_window_size = 1;
            }
            let v: i32 = self.min_window_size.try_convert()?;
            let min_window_max = window_min + v;
            window_max = window_max.max(min_window_max);
        }
        Ok(window_max)
    }
    fn update_max_window_scores(&mut self, window_min: i32, window_max: i32) -> Result<()> {
        for &idx in &self.all_scorers_idx {
            let w = &mut self.all_scorers[idx];

            if w.doc < window_max {
                if w.doc < window_min {
                    // Make sure to advance shallow if necessary to get as good score upper bounds as
                    // possible.
                    w.advance_shallow(window_min)?;
                }
                w.max_window_score = w.get_max_score(window_max - 1)?;
            } else {
                // This scorer has no documents in the considered window.
                w.max_window_score = 0.0;
            }
        }
        Ok(())
    }
    fn score_non_essential_clauses<LC>(
        &mut self,
        collector: &mut LC,
        doc: i32,
        essential_score: f64,
        num_non_essential_clauses: usize,
    ) -> Result<()>
    where
        LC: LeafCollector,
    {
        self.num_candidates += 1;

        let mut score = essential_score;
        for i in (0..num_non_essential_clauses).rev() {
            let max_possible_score = MathUtil::sum_upper_bound(
                score + self.max_score_sums[i],
                self.all_scorers_idx.len() as i32,
            ) as f32;

            if max_possible_score < self.scorable.min_competitive_score {
                // Hit is not competitive.
                return Ok(());
            }

            let index = self.all_scorers_idx[i];
            let w = &mut self.all_scorers[index];

            if w.doc < doc {
                let v = w.iterator_mut().advance(doc)?;
                w.doc = v;
            }
            if w.doc == doc {
                score += w.score()? as f64;
            }
        }

        self.scorable.score = score as f32;
        collector.collect(doc, &mut self.scorable)?;
        Ok(())
    }

    /// Partitioning scorers is an optimization problem: the optimal set of non-essential scorers is
    /// the subset of scorers whose sum of max window scores is less than the minimum competitive
    /// score that maximizes the sum of costs.
    /// Computing the optimal solution to this problem would take O(2^num_clauses). As a first
    /// approximation, we take the first scorers sorted by max_window_score / cost whose sum of max
    /// scores is less than the minimum competitive scores. In the common case, maximum scores are
    /// inversely correlated with document frequency so this is the same as only sorting by maximum
    /// score, as described in the MAXSCORE paper and gives the optimal solution. However, this can
    /// make a difference when using custom scores (like FuzzyQuery), high query-time boosts, or
    /// scoring based on wacky weights.
    fn partition_scorers(&mut self) -> Result<bool> {
        for i in 0..self.all_scorers_idx.len() {
            self.scratch[i] = i;
        }

        self.scratch.sort_by(|&i1, &i2| {
            let w1 = &self.all_scorers[i1];
            let w2 = &self.all_scorers[i2];
            let s1 = w1.max_window_score as f64 / (w1.cost.max(1) as f64);
            let s2 = w2.max_window_score as f64 / (w2.cost.max(1) as f64);
            // s2 never be zero  so we could use `total_cmp` directly on the division result.
            s1.total_cmp(&s2)
        });

        let mut max_score_sum: f64 = 0.0;
        self.first_essential_scorer = 0;
        self.next_min_competitive_score = f32::INFINITY;

        let n = self.all_scorers_idx.len();

        for idx in 0..n {
            let index = self.scratch[idx];
            let w = &self.all_scorers[index];
            let new_max_score_sum = max_score_sum + w.max_window_score as f64;
            let v: i32 = self.first_essential_scorer.try_convert()?;
            let max_score_sum_float = MathUtil::sum_upper_bound(new_max_score_sum, v + 1) as f32;

            if max_score_sum_float < self.scorable.min_competitive_score {
                max_score_sum = new_max_score_sum;
                self.all_scorers_idx[self.first_essential_scorer] = index;
                self.max_score_sums[self.first_essential_scorer] = max_score_sum;
                self.first_essential_scorer += 1;
            } else {
                let pos = n - 1 - (idx - self.first_essential_scorer);
                self.all_scorers_idx[pos] = index;
                self.next_min_competitive_score =
                    self.next_min_competitive_score.min(max_score_sum_float);
            }
        }

        self.first_required_scorer = n;

        if self.first_essential_scorer == n {
            return Ok(false);
        }

        self.essential_queue.clear();
        for i in self.first_essential_scorer..n {
            self.essential_queue
                .add(self.all_scorers_idx[i], &self.all_scorers);
        }

        if self.first_essential_scorer == n - 1 {
            // single essential clause
            // If there is a single essential clause and matching it plus all non-essential clauses but
            // the best one is not enough to yield a competitive match, the we know that hits must match
            // both the essential clause and the best non-essential clause. Here are some examples when
            // this optimization would kick in:
            //   `quick fox`  when maxscore(quick) = 1, maxscore(fox) = 1, minCompetitiveScore = 1.5
            //   `the quick fox` when maxscore (the) = 0.1, maxscore(quick) = 1, maxscore(fox) = 1,
            //       minCompetitiveScore = 1.5
            self.first_required_scorer = n - 1;
            let mut max_required_score = self.all_scorers
                [self.all_scorers_idx[self.first_essential_scorer]]
                .max_window_score as f64;

            while self.first_required_scorer > 0 {
                let mut max_possible_score_without_previous = max_required_score;

                if self.first_required_scorer > 1 {
                    max_possible_score_without_previous +=
                        self.max_score_sums[self.first_required_scorer - 2];
                }

                if (max_possible_score_without_previous as f32)
                    >= self.scorable.min_competitive_score
                {
                    break;
                }
                // The sum of maximum scores ignoring the previous clause is less than the minimum
                // competitive
                self.first_required_scorer -= 1;
                max_required_score += self.all_scorers
                    [self.all_scorers_idx[self.first_required_scorer]]
                    .max_window_score as f64;
            }
        }

        Ok(true)
    }

    /// Return the next candidate on or after `rangeEnd`.
    fn next_candidate(&self, range_end: i32) -> i32 {
        if range_end >= self.max_doc {
            return NO_MORE_DOCS;
        }

        let mut next = NO_MORE_DOCS;
        for w in &self.all_scorers_idx {
            let w = &self.all_scorers[*w];
            if w.doc < range_end {
                return range_end;
            } else {
                next = next.min(w.doc);
            }
        }
        next
    }
}
impl<S> BulkScorer for MaxScoreBulkScorer<S>
where
    S: Scorer,
{
    fn score<LC, B>(
        &mut self,
        collector: &mut LC,
        accept_docs: Option<&B>,
        min: i32,
        max: i32,
    ) -> Result<i32>
    where
        LC: LeafCollector,
        B: Bits,
    {
        collector.set_scorer(&mut self.scorable)?;

        // This scorer computes outer windows based on impacts that are stored in the index. These outer
        // windows should be small enough to provide good upper bounds of scores, and big enough to make
        // sure we spend more time collecting docs than recomputing windows.
        // Then within these outer windows, it creates inner windows of size WINDOW_SIZE that help
        // collect matches into a bitset and save the overhead of rebalancing the priority queue on
        // every match.

        let mut outer_window_min = min;

        'outer: while outer_window_min < max {
            let mut outer_window_max = self.compute_outer_window_max(outer_window_min)?;
            outer_window_max = outer_window_max.min(max);

            loop {
                self.update_max_window_scores(outer_window_min, outer_window_max)?;

                if !self.partition_scorers()? {
                    // No matches in this window
                    outer_window_min = outer_window_max;
                    continue 'outer;
                }

                // There is a dependency between windows and maximum scores, as we compute windows based on
                // maximum scores and maximum scores based on windows.
                // So the approach consists of starting by computing a window based on the set of essential
                // scorers from the _previous_ window and then iteratively recompute maximum scores and
                // windows as long as the window size decreases.
                // In general the set of essential scorers is rather stable over time so this would exit
                // after a single iteration, but there is a change that some scorers got swapped between the
                // set of essential and non-essential scorers, in which case there may be multiple
                // iterations of this loop.

                let new_outer_window_max = self.compute_outer_window_max(outer_window_min)?;
                if new_outer_window_max >= outer_window_max {
                    break;
                }
                outer_window_max = new_outer_window_max;
            }

            let mut top_index = self.essential_queue.top().expect("top ie empty");
            {
                let mut doc = self.all_scorers[top_index].doc;
                while doc < outer_window_min {
                    {
                        let top = &mut self.all_scorers[top_index];
                        let v = top.iterator_mut().advance(outer_window_min)?;
                        top.doc = v;
                        doc = v;
                    }
                    top_index = self.essential_queue.update_top(&self.all_scorers);
                }
            }

            let mut top_doc = self.all_scorers[top_index].doc;

            while top_doc < outer_window_max {
                self.score_inner_window(collector, accept_docs, outer_window_max)?;
                top_index = self.essential_queue.top().expect("top ie empty");
                top_doc = self.all_scorers[top_index].doc;

                if self.scorable.min_competitive_score >= self.next_min_competitive_score {
                    // The minimum competitive score increased substantially, so we can now partition scorers
                    // in a more favorable way.
                    break;
                }
            }
            outer_window_min = std::cmp::min(top_doc, outer_window_max);
            self.num_outer_windows += 1;
        }

        Ok(self.next_candidate(max))
    }

    fn cost(&mut self) -> Result<i64> {
        Ok(self.cost)
    }
}

pub struct Score {
    score: f32,
    min_competitive_score: f32,
}
impl Score {
    fn new() -> Self {
        Self {
            score: 0.0,
            min_competitive_score: 0.0,
        }
    }
}
impl Scorable for Score {
    fn score(&mut self) -> Result<f32> {
        Ok(self.score)
    }

    fn set_min_competitive_score(&mut self, min_score: f32) -> Result<()> {
        self.min_competitive_score = min_score;
        Ok(())
    }

    type Scorable = DummyScorable;
}
#[cfg(test)]
mod test {
    use crate::core::document::document::Document;
    use crate::core::document::field::Store;
    use crate::core::document::string_field::StringField;
    use crate::core::index::index_writer::IndexWriter;
    use crate::core::index::index_writer_config::IndexWriterConfig;
    use crate::core::search::doc_id_set_iterator::AllDISI;
    use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
    use crate::core::search::dummy::dummy_scorable::DummyScorable;
    use crate::core::search::dummy::dummy_two_phase_iterator::DummyTwoPhaseIterator;
    use crate::core::search::max_score_bulk_scorer::{INNER_WINDOW_SIZE, MaxScoreBulkScorer};
    use crate::core::search::scorable::Scorable;
    use crate::core::search::scorer::TwoPhaseState::No;
    use crate::core::search::scorer::{Scorer, TwoPhaseState};
    use crate::core::store::directory::Directory;
    use crate::core::util::error::lucene_error::{LuceneError, Result};
    use crate::test::util::lucene_test_case::lucene_test_case_util::random;
    use rand::prelude::SliceRandom;
    use std::sync::Arc;

    #[allow(dead_code)] // for quick search
    struct TestMaxScoreBulkScorer;

    fn write_documents<D>(dir: Arc<D>) -> Result<()>
    where
        D: Directory,
    {
        let iwc = IndexWriterConfig::new();
        // TODO newLogMergePolicy 未实现
        // iwc.set_merge_policy(new_log_merge_policy());

        let writer = IndexWriter::new(dir.clone(), iwc)?;

        let docs: Vec<Vec<&str>> = vec![
            vec!["A", "B"],      // 0
            vec!["A"],           // 1
            vec![],              // 2
            vec!["A", "B", "C"], // 3
            vec!["B"],           // 4
            vec!["B", "C"],      // 5
        ];

        for values in docs {
            let mut doc = Document::new();
            for value in values {
                doc.add(StringField::with_string("foo", value, Store::No)?);
            }
            writer.add_document(doc)?;

            for _i in 1..INNER_WINDOW_SIZE {
                writer.add_document(Document::new())?;
            }
        }
        // TODO force_merge 未实现
        // writer.force_merge(1)?;
        writer.close()?;
        Ok(())
    }
    // TODO BoostQuery跟ConstantScoreQuery未实现
    #[test]
    fn test_partition() -> Result<()> {
        let mut random = random();
        let mut the = FakeScorer::new("the".to_string());
        the.cost = 9000;
        the.max_score = 0.1;
        the.doc_id = 4;
        the.max_score_up_to = 130;

        let mut quick = FakeScorer::new("quick".to_string());
        quick.cost = 1000;
        quick.max_score = 1.0;
        quick.doc_id = 4;
        quick.max_score_up_to = 999;

        let mut fox = FakeScorer::new("fox".to_string());
        fox.cost = 900;
        fox.max_score = 1.1;
        fox.doc_id = 10;
        fox.max_score_up_to = 1200;

        let scorers = vec![the, quick, fox];
        let mut scorer = MaxScoreBulkScorer::new(10_000, scorers, None)?;
        scorer.all_scorers_idx.shuffle(&mut random);
        scorer.update_max_window_scores(4, 100)?;
        assert!(scorer.partition_scorers()?);
        assert_eq!(0, scorer.first_essential_scorer);
        assert_eq!(3, scorer.first_required_scorer);

        // less than the minimum score of every clause
        scorer.scorable.min_competitive_score = 0.09;
        scorer.all_scorers_idx.shuffle(&mut random);
        scorer.update_max_window_scores(4, 100)?;
        assert!(scorer.partition_scorers()?);
        assert_eq!(0, scorer.first_essential_scorer);
        assert_eq!(3, scorer.first_required_scorer);

        // equal to the maximum score of `the`
        scorer.scorable.min_competitive_score = 0.1;
        scorer.all_scorers_idx.shuffle(&mut random);
        scorer.update_max_window_scores(4, 100)?;
        assert!(scorer.partition_scorers()?);
        assert_eq!(0, scorer.first_essential_scorer);
        assert_eq!(3, scorer.first_required_scorer);

        // gt than the minimum score of `the`
        scorer.scorable.min_competitive_score = 0.11;
        scorer.all_scorers_idx.shuffle(&mut random);
        scorer.update_max_window_scores(4, 100)?;
        assert!(scorer.partition_scorers()?);
        assert_eq!(1, scorer.first_essential_scorer);
        assert_eq!(3, scorer.first_required_scorer);
        assert_eq!(0, scorer.all_scorers_idx[0]); // the

        // equal to the sum of the max scores of the and quick
        scorer.scorable.min_competitive_score = 1.1;
        scorer.all_scorers_idx.shuffle(&mut random);
        scorer.update_max_window_scores(4, 100)?;
        assert!(scorer.partition_scorers()?);
        assert_eq!(1, scorer.first_essential_scorer);
        assert_eq!(3, scorer.first_required_scorer);
        assert_eq!(0, scorer.all_scorers_idx[0]); // the

        // greater than the sum of the max scores of the and quick
        scorer.scorable.min_competitive_score = 1.11;
        scorer.all_scorers_idx.shuffle(&mut random);
        scorer.update_max_window_scores(4, 100)?;
        assert!(scorer.partition_scorers()?);
        assert_eq!(2, scorer.first_essential_scorer);
        assert_eq!(2, scorer.first_required_scorer);
        assert_eq!(0, scorer.all_scorers_idx[0]); // the
        assert_eq!(1, scorer.all_scorers_idx[1]); // quick
        assert_eq!(2, scorer.all_scorers_idx[2]); // fox

        // equal to the sum of the max scores of the and fox
        scorer.scorable.min_competitive_score = 1.2;
        scorer.all_scorers_idx.shuffle(&mut random);
        scorer.update_max_window_scores(4, 100)?;
        assert!(scorer.partition_scorers()?);
        assert_eq!(2, scorer.first_essential_scorer);
        assert_eq!(2, scorer.first_required_scorer);
        assert_eq!(0, scorer.all_scorers_idx[0]);
        assert_eq!(1, scorer.all_scorers_idx[1]);
        assert_eq!(2, scorer.all_scorers_idx[2]);

        // greater than the sum of the max scores of the and fox
        scorer.scorable.min_competitive_score = 1.21;
        scorer.all_scorers_idx.shuffle(&mut random);
        scorer.update_max_window_scores(4, 100)?;
        assert!(scorer.partition_scorers()?);
        assert_eq!(2, scorer.first_essential_scorer);
        assert_eq!(1, scorer.first_required_scorer);
        assert_eq!(0, scorer.all_scorers_idx[0]);
        assert_eq!(1, scorer.all_scorers_idx[1]);
        assert_eq!(2, scorer.all_scorers_idx[2]);

        // equal to the sum of the max scores of quick and fox
        scorer.scorable.min_competitive_score = 2.1;
        scorer.all_scorers_idx.shuffle(&mut random);
        scorer.update_max_window_scores(4, 100)?;
        assert!(scorer.partition_scorers()?);
        assert_eq!(2, scorer.first_essential_scorer);
        assert_eq!(1, scorer.first_required_scorer);
        assert_eq!(0, scorer.all_scorers_idx[0]);
        assert_eq!(1, scorer.all_scorers_idx[1]);
        assert_eq!(2, scorer.all_scorers_idx[2]);

        // greater than the sum of the max scores of quick and fox
        scorer.scorable.min_competitive_score = 2.11;
        scorer.all_scorers_idx.shuffle(&mut random);
        scorer.update_max_window_scores(4, 100)?;
        assert!(scorer.partition_scorers()?);
        assert_eq!(2, scorer.first_essential_scorer);
        assert_eq!(0, scorer.first_required_scorer);
        assert_eq!(0, scorer.all_scorers_idx[0]);
        assert_eq!(1, scorer.all_scorers_idx[1]);
        assert_eq!(2, scorer.all_scorers_idx[2]);

        // equal to the sum of the max scores of all terms
        scorer.scorable.min_competitive_score = 2.2;
        scorer.all_scorers_idx.shuffle(&mut random);
        scorer.update_max_window_scores(4, 100)?;
        assert!(scorer.partition_scorers()?);
        assert_eq!(2, scorer.first_essential_scorer);
        assert_eq!(0, scorer.first_required_scorer);
        assert_eq!(0, scorer.all_scorers_idx[0]);
        assert_eq!(1, scorer.all_scorers_idx[1]);
        assert_eq!(2, scorer.all_scorers_idx[2]);

        // greater than the sum of the max scores of all terms
        scorer.scorable.min_competitive_score = 2.21;
        scorer.update_max_window_scores(4, 100)?;
        assert!(!scorer.partition_scorers()?);

        Ok(())
    }

    struct FakeScorer {
        to_string: String,
        doc_id: i32,
        max_score_up_to: i32,
        max_score: f32,
        cost: i32,
        disi: AllDISI,
    }
    impl FakeScorer {
        fn new(to_string: String) -> Self {
            let cost = 10;
            let disi = AllDISI::new(cost);
            Self {
                to_string,
                doc_id: -1,
                max_score_up_to: NO_MORE_DOCS,
                max_score: 1.0,
                cost: 10,
                disi,
            }
        }
    }

    impl Scorable for FakeScorer {
        fn score(&mut self) -> Result<f32> {
            Err(LuceneError::unsupported_operation(""))
        }

        type Scorable = DummyScorable;
    }

    impl Scorer for FakeScorer {
        type DocIdSetIterator = AllDISI;
        type DocIdSetIteratorRef<'a>
            = &'a AllDISI
        where
            Self: 'a;
        type DocIdSetIteratorMut<'a>
            = &'a mut AllDISI
        where
            Self: 'a;
        type TwoPhaseIter = DummyTwoPhaseIterator;
        type TwoPhaseIterRef<'a>
            = DummyTwoPhaseIterator
        where
            Self: 'a;
        type TwoPhaseIterMut<'a>
            = DummyTwoPhaseIterator
        where
            Self: 'a;

        fn doc_id(&mut self) -> Result<i32> {
            Ok(self.doc_id)
        }

        fn iterator(&self) -> Self::DocIdSetIteratorRef<'_> {
            &self.disi
        }

        fn iterator_mut(&mut self) -> Self::DocIdSetIteratorMut<'_> {
            &mut self.disi
        }

        fn take_iterator(self) -> Self::DocIdSetIterator {
            unreachable!("")
        }

        fn two_phase_iterator(&self) -> Result<Option<Self::TwoPhaseIterRef<'_>>> {
            Ok(None)
        }

        fn advance_shallow(&mut self, _target: i32) -> Result<i32> {
            Ok(self.max_score_up_to)
        }

        fn get_max_score(&mut self, _up_to: i32) -> Result<f32> {
            Ok(self.max_score)
        }

        fn has_two_phase_iterator(&self) -> TwoPhaseState {
            No
        }
    }
}
