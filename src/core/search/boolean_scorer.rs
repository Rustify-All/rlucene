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
use crate::core::search::disi_wrapper::DisiWrapper;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_stream::DocIdStream;
use crate::core::search::leaf_collector::LeafCollector;
use crate::core::search::scorable::Scorable;
use crate::core::search::score::Score;
use crate::core::search::scorer::Scorer;
use crate::core::search::scorer_util::ScorerUtil;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::priority_queue::{Compare, PriorityQueue};

pub(crate) const SHIFT: usize = 12;
pub(crate) const SIZE: usize = 1 << SHIFT;
pub(crate) const MASK: usize = SIZE - 1;

pub(crate) const SET_SIZE: usize = 1 << (SHIFT - 6);
pub(crate) const SET_MASK: usize = SET_SIZE - 1;
/// **BulkScorer** that is used for pure disjunctions and disjunctions that have low values of
/// `MinimumNumberShouldMatch` and dense clauses.
///
/// This scorer scores documents by batches of **4,096 docs**.
pub(crate) struct BooleanScorer<S>
where
    S: Scorer,
{
    // One bucket per doc ID in the window, non-null if scores are needed or if frequencies need to be
    // counted
    pub(crate) buckets: Option<Vec<Bucket>>,
    // This is basically an inlined FixedBitSet... seems to help with bound checks
    pub(crate) matching: Vec<u64>,
    pub(crate) head: PriorityQueue<DisiWrapper<S>, HeadPriorityQueueCmp>,
    pub(crate) tail: PriorityQueue<DisiWrapper<S>, TailPriorityQueueCmp>,
    pub(crate) score: Score,
    pub(crate) min_should_match: usize,
    pub(crate) cost: i64,
    pub(crate) needs_scores: bool,
}
impl<S> BooleanScorer<S>
where
    S: Scorer,
{
    pub fn new(scorers: Vec<S>, min_should_match: usize, needs_scores: bool) -> Result<Self> {
        if min_should_match < 1 || min_should_match > scorers.len() {
            return Err(LuceneError::illegal_argument(format!(
                "minShouldMatch should be within 1..num_scorers. Got {}",
                min_should_match
            )));
        }
        if scorers.len() <= 1 {
            return Err(LuceneError::illegal_argument(format!(
                "This scorer can only be used with two scorers or more, got {}",
                scorers.len()
            )));
        }

        let buckets = if needs_scores || min_should_match > 1 {
            let mut v = Vec::with_capacity(SIZE);
            for _ in 0..SIZE {
                v.push(Bucket::new());
            }
            Some(v)
        } else {
            None
        };

        let matching = vec![0u64; SET_SIZE];

        let head_size = scorers.len() - min_should_match + 1;
        let tail_size = min_should_match - 1;

        let mut head = PriorityQueue::new(head_size, HeadPriorityQueueCmp)?;
        let mut tail = PriorityQueue::new(tail_size, TailPriorityQueueCmp)?;

        let mut cost_values: Vec<i64> = Vec::with_capacity(scorers.len());

        for s in scorers {
            let w = DisiWrapper::new(s)?;
            cost_values.push(w.cost);

            if let Some(evicted) = tail.insert_with_overflow(w)? {
                head.add(evicted)?;
            }
        }
        let cost = ScorerUtil::cost_with_min_should_match(
            cost_values.into_iter(),
            head_size + tail_size,
            min_should_match,
        )?;

        Ok(Self {
            buckets,
            matching,
            head,
            tail,
            score: Score::new(0.0),
            min_should_match,
            cost,
            needs_scores,
        })
    }
    fn score_disi_wrapper_into_bitset<B>(
        &mut self,
        w: &mut DisiWrapper<S>,
        accept_docs: Option<&B>,
        min: i32,
        max: i32,
    ) -> Result<()>
    where
        B: Bits,
    {
        let mut doc = {
            let mut doc = w.doc;
            let it = &mut w.iterator_mut();
            if doc < min {
                doc = it.advance(min)?;
            }
            doc
        };
        while doc < max {
            let accepted =
                accept_docs.is_none() || accept_docs.as_ref().unwrap().get(doc as usize)?;
            if accepted {
                let i = doc as usize & MASK;
                let idx = i >> 6;

                self.matching[idx] |= 1u64 << i;
                if let Some(ref mut buckets) = self.buckets {
                    let bucket = &mut buckets[i];
                    bucket.freq += 1;
                    if self.needs_scores {
                        bucket.score += w.score()? as f64;
                    }
                }
            }

            doc = w.iterator_mut().next_doc()?;
        }

        w.doc = doc;
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    fn score_window_into_bitset_and_replay<LC, B>(
        &mut self,
        collector: &mut LC,
        accept_docs: Option<&B>,
        base: i32,
        min: i32,
        max: i32,
        scorers: &mut [DisiWrapper<S>],
        num_scorers: usize,
    ) -> Result<()>
    where
        B: Bits,
        LC: LeafCollector,
    {
        for w in scorers.iter_mut().take(num_scorers) {
            debug_assert!(w.doc < max);
            self.score_disi_wrapper_into_bitset(w, accept_docs, min, max)?;
        }
        let mut stream = DocIdStreamView::new(self, base);
        collector.collect_stream(&mut stream)?;
        for m in self.matching.iter_mut() {
            *m = 0;
        }
        Ok(())
    }
    fn advance(&mut self, min: i32) -> Result<&DisiWrapper<S>> {
        debug_assert!(self.tail.size() == (self.min_should_match - 1));

        match self.head.take_top() {
            None => return Err(LuceneError::illegal_state("head queue is empty")),
            Some(mut head_top) => {
                while head_top.doc < min {
                    match self.tail.take_top() {
                        None => {
                            let v = head_top.iterator_mut().advance(min)?;
                            head_top.doc = v;
                            let _ = self.head.update_top_with_new_top(head_top)?;
                            head_top = self.head.take_top().unwrap();
                        },
                        Some(mut tail_top) => {
                            if head_top.cost <= tail_top.cost {
                                let v = head_top.iterator_mut().advance(min)?;
                                head_top.doc = v;
                                let _ = self.head.update_top_with_new_top(head_top)?;
                                head_top = self.head.take_top().unwrap();
                                // return tail_top back
                                self.tail.update_top_with_new_top(tail_top)?;
                            } else {
                                let v = tail_top.iterator_mut().advance(min)?;
                                tail_top.doc = v;
                                let _ = self.head.update_top_with_new_top(tail_top)?;
                                let _ = self.tail.update_top_with_new_top(head_top)?;
                                head_top = self.head.take_top().unwrap();
                            }
                        },
                    }
                }
                self.head.update_top_with_new_top(head_top)?;
            },
        }
        match self.head.top() {
            None => Err(LuceneError::illegal_state("head queue is empty")),
            Some(top) => Ok(top),
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn score_window_multiple_scorers<B, LC>(
        &mut self,
        collector: &mut LC,
        accept_docs: Option<&B>,
        window_base: i32,
        window_min: i32,
        window_max: i32,
        mut max_freq: usize,
        mut leads: Vec<DisiWrapper<S>>,
    ) -> Result<()>
    where
        B: Bits,
        LC: LeafCollector,
    {
        while max_freq < self.min_should_match
            && max_freq + self.tail.size() >= self.min_should_match
        {
            // a match is still possible
            let mut candidate = self
                .tail
                .pop()?
                .ok_or_else(|| LuceneError::illegal_state("tail.pop returned None"))?;

            if candidate.doc < window_min {
                let new_doc = candidate.iterator_mut().advance(window_min)?;
                candidate.doc = new_doc;
            }

            if candidate.doc < window_max {
                leads[max_freq] = candidate;
                max_freq += 1;
            } else {
                self.head.add(candidate)?;
            }
        }

        if max_freq >= self.min_should_match {
            // There might be matches in other scorers from the tail too
            for x in self.tail.get().into_iter() {
                leads[max_freq] = x;
                max_freq += 1;
            }
            self.tail.clear();

            self.score_window_into_bitset_and_replay(
                collector,
                accept_docs,
                window_base,
                window_min,
                window_max,
                leads.as_mut_slice(),
                max_freq,
            )?;
        }

        for v in leads.into_iter() {
            let evicted = self.head.insert_with_overflow(v)?;
            if let Some(e) = evicted {
                self.tail.add(e)?;
            }
        }

        Ok(())
    }
    pub(crate) fn score_window_single_scorer<B, LC>(
        &mut self,
        w: &mut DisiWrapper<S>,
        collector: &mut LC,
        accept_docs: Option<&B>,
        window_min: i32,
        window_max: i32,
        max_doc: i32,
    ) -> Result<()>
    where
        B: Bits,
        LC: LeafCollector,
    {
        debug_assert!(self.tail.size() == 0);
        let next_window_base = match self.head.top() {
            None => return Err(LuceneError::illegal_state("head queue is empty")),
            Some(top) => top.doc & !(MASK as i32),
        };
        let end = std::cmp::max(window_max, std::cmp::min(max_doc, next_window_base));
        let doc = {
            let mut doc;
            {
                doc = w.doc;
                let mut it = w.iterator_mut();
                if doc < window_min {
                    doc = it.advance(window_min)?;
                }
            }
            collector.set_scorer(&mut w.scorer)?;
            while doc < end {
                let accepted = match accept_docs {
                    None => true,
                    Some(bits) => bits.get(doc as usize)?,
                };
                if accepted {
                    collector.collect(doc, &mut w.scorer)?;
                }
                doc = w.iterator_mut().next_doc()?;
            }
            doc
        };
        w.doc = doc;
        // reset the scorer that should be used for the general case
        collector.set_scorer(&mut self.score)?;
        Ok(())
    }
    pub(crate) fn score_window<B, LC>(
        &mut self,
        top_doc: i32,
        collector: &mut LC,
        accept_docs: Option<&B>,
        min: i32,
        max: i32,
    ) -> Result<&DisiWrapper<S>>
    where
        B: Bits,
        LC: LeafCollector,
    {
        let window_base = top_doc & !(MASK as i32);
        let window_min = std::cmp::max(min, window_base);
        let window_max = std::cmp::min(max, window_base + SIZE as i32);
        let mut leads: Vec<DisiWrapper<S>> = Vec::new();
        let head_top = self
            .head
            .pop()?
            .ok_or_else(|| LuceneError::illegal_state("head's top() returned None"))?;
        leads.push(head_top);
        let mut max_freq = 1usize;

        while self.head.size() > 0 {
            let head_top_doc = self
                .head
                .top()
                .ok_or_else(|| LuceneError::illegal_state("head's top() returned None"))?
                .doc;
            if head_top_doc >= window_max {
                break;
            }
            let w = self
                .head
                .pop()?
                .ok_or_else(|| LuceneError::illegal_state("head's top() returned None"))?;
            leads.push(w);
            max_freq += 1;
        }

        if self.min_should_match == 1 && max_freq == 1 {
            // special case: only one scorer can match in the current window,
            // we can collect directly
            let mut bulk_scorer = leads.remove(0);

            self.score_window_single_scorer(
                &mut bulk_scorer,
                collector,
                accept_docs,
                window_min,
                window_max,
                max,
            )?;
            return self.head.add(bulk_scorer);
        }
        // general case, collect through a bit set first and then replay
        self.score_window_multiple_scorers(
            collector,
            accept_docs,
            window_base,
            window_min,
            window_max,
            max_freq,
            leads,
        )?;

        self.head
            .top()
            .ok_or_else(|| LuceneError::illegal_state("head's top() returned None"))
    }
}
impl<S> BulkScorer for BooleanScorer<S>
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
        collector.set_scorer(&mut self.score)?;
        let mut top = self.advance(min)?;
        let mut doc = top.doc;
        while doc < max {
            top = self.score_window(doc, collector, accept_docs, min, max)?;
            doc = top.doc;
        }
        Ok(top.doc)
    }

    fn cost(&mut self) -> Result<i64> {
        Ok(self.cost)
    }
}

pub struct HeadPriorityQueueCmp;
impl<S> Compare<DisiWrapper<S>> for HeadPriorityQueueCmp
where
    S: Scorer,
{
    fn less_than(&self, a: &DisiWrapper<S>, b: &DisiWrapper<S>) -> Result<bool> {
        Ok(a.doc < b.doc)
    }
}
pub struct TailPriorityQueueCmp;
impl<S> Compare<DisiWrapper<S>> for TailPriorityQueueCmp
where
    S: Scorer,
{
    fn less_than(&self, a: &DisiWrapper<S>, b: &DisiWrapper<S>) -> Result<bool> {
        Ok(a.cost < b.cost)
    }
}
pub struct Bucket {
    score: f64,
    freq: i32,
}
impl Bucket {
    fn new() -> Self {
        Self {
            score: 0.0,
            freq: 0,
        }
    }
}
impl<S> PriorityQueue<DisiWrapper<S>, TailPriorityQueueCmp>
where
    S: Scorer,
{
    fn get(&mut self) -> Vec<DisiWrapper<S>> {
        self.take_heap_array()
    }
}

struct DocIdStreamView<'a, S>
where
    S: Scorer,
{
    scorer: &'a mut BooleanScorer<S>,
    base: i32,
}
impl<'a, S> DocIdStreamView<'a, S>
where
    S: Scorer,
{
    fn new(scorer: &'a mut BooleanScorer<S>, base: i32) -> Self {
        Self { scorer, base }
    }
}
impl<'a, S> DocIdStream for DocIdStreamView<'a, S>
where
    S: Scorer,
{
    type Scorer = Score;

    fn scorer(&mut self) -> &mut Self::Scorer {
        &mut self.scorer.score
    }

    fn for_each<F>(&mut self, mut f: F) -> Result<()>
    where
        F: FnMut(i32, &mut Self::Scorer) -> Result<()>,
    {
        for (idx, bits_ref) in self.scorer.matching.iter_mut().enumerate() {
            let mut bits = *bits_ref;

            while bits != 0 {
                let ntz = bits.trailing_zeros() as usize;
                let index_in_window = (idx << 6) | ntz;
                match self.scorer.buckets {
                    Some(ref mut buckets) => {
                        let bucket = &mut buckets[index_in_window];
                        if bucket.freq as usize >= self.scorer.min_should_match {
                            self.scorer.score.score = bucket.score as f32;
                            f(self.base | index_in_window as i32, &mut self.scorer.score)?;
                        }
                        bucket.freq = 0;
                        bucket.score = 0.0;
                    },
                    None => {
                        f(self.base | index_in_window as i32, &mut self.scorer.score)?;
                    },
                }
                bits ^= 1u64 << ntz;
            }
            *bits_ref = 0;
        }

        Ok(())
    }

    fn count(&mut self) -> Result<i32> {
        if self.scorer.min_should_match > 1 {
            return self.default_count();
        }

        let count: i32 = self
            .scorer
            .matching
            .iter()
            .map(|l| l.count_ones() as i32)
            .sum();
        Ok(count)
    }
}
