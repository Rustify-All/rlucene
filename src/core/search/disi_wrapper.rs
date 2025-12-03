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
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, Either2DocIdSetIterator};
use crate::core::search::scorable::{ChildScorable, Scorable};
use crate::core::search::scorer::Scorer;
use crate::core::search::two_phase_iterator::TwoPhaseIterator;
use crate::core::util::error::lucene_error::Result;
/// Diff to Java Lucene, Compile-time polymorphism makes it unnecessary to wrap `likelyTermScorer`
/// or `likelyImpactsEnum`.
pub struct DisiWrapper<S>
where
    S: Scorer,
{
    scorer: S,
    pub(crate) next: Option<usize>,
    pub(crate) doc: i32,
    pub(crate) cost: i64,
    // the match cost for two-phase iterators, 0 otherwise
    pub(crate) match_cost: f32,
    // for MaxScoreBulkScorer
    scaled_max_score: i64,
    // for MaxScoreBulkScorer
    pub(crate) max_window_score: f32,
}

pub type DisiWrapperDocIdSetIterator<'a, S> = Either2DocIdSetIterator<
    <S as Scorer>::DocIdSetIteratorRef<'a>,
    <<S as Scorer>::TwoPhaseIterRef<'a> as TwoPhaseIterator>::DocIdSetIteratorMut<'a>,
>;
impl<S> DisiWrapper<S>
where
    S: Scorer,
{
    pub fn new(mut scorer: S) -> Result<Self> {
        let cost = scorer.iterator().cost()?;
        let match_cost = match scorer.two_phase_iterator() {
            Some(tpi) => tpi.match_cost(),
            None => 0.0,
        };
        Ok(Self {
            scorer,
            next: None,
            doc: -1,
            cost,
            match_cost,
            scaled_max_score: 0,
            max_window_score: 0.0,
        })
    }

    pub fn advance(&mut self, doc: i32) -> Result<i32> {
        let mut disi = self.doc_id_set_iterator();
        disi.advance(doc)
    }
    pub fn next_doc(&mut self) -> Result<i32> {
        let mut disi = self.doc_id_set_iterator();
        disi.next_doc()
    }
    pub fn matches(&mut self) -> Result<bool> {
        match self.scorer.two_phase_iterator() {
            Some(mut tpi) => tpi.matches(),
            None => Ok(true),
        }
    }

    pub fn doc_id_set_iterator(&mut self) -> DisiWrapperDocIdSetIterator<'_, S> {
        match self.scorer.two_phase_iterator() {
            Some(mut tpi) => DisiWrapperDocIdSetIterator::B(tpi.approximation_mut()),
            None => DisiWrapperDocIdSetIterator::A(self.scorer.iterator()),
        }
    }
}

impl<S> Scorable for DisiWrapper<S>
where
    S: Scorer,
{
    fn score(&mut self) -> Result<f32> {
        self.scorer.score()
    }

    fn smoothing_score(&mut self, doc_id: i32) -> Result<f32> {
        self.scorer.smoothing_score(doc_id)
    }

    fn set_min_competitive_score(&mut self, min_score: f32) -> Result<()> {
        self.scorer.set_min_competitive_score(min_score)
    }

    type Scorable = S::Scorable;

    fn get_children(&self) -> Result<Vec<ChildScorable<Self::Scorable>>> {
        self.scorer.get_children()
    }

    fn cost(&mut self) -> Result<i64> {
        self.scorer.cost()
    }
}

impl<S> Scorer for DisiWrapper<S>
where
    S: Scorer,
{
    type DocIdSetIterator = S::DocIdSetIterator;
    type DocIdSetIteratorRef<'a>
        = S::DocIdSetIteratorRef<'a>
    where
        Self: 'a;
    type TwoPhaseIter = S::TwoPhaseIter;
    type TwoPhaseIterRef<'a>
        = S::TwoPhaseIterRef<'a>
    where
        Self: 'a;

    fn doc_id(&mut self) -> Result<i32> {
        self.scorer.doc_id()
    }

    fn iterator(&mut self) -> Self::DocIdSetIteratorRef<'_> {
        self.scorer.iterator()
    }

    fn take_iterator(&mut self) -> Self::DocIdSetIterator {
        self.scorer.take_iterator()
    }

    fn two_phase_iterator(&mut self) -> Option<Self::TwoPhaseIterRef<'_>> {
        self.scorer.two_phase_iterator()
    }

    fn take_two_phase_iterator(&mut self) -> Option<Self::TwoPhaseIter> {
        self.scorer.take_two_phase_iterator()
    }

    fn advance_shallow(&mut self, target: i32) -> Result<i32> {
        self.scorer.advance_shallow(target)
    }

    fn get_max_score(&mut self, up_to: i32) -> Result<f32> {
        self.scorer.get_max_score(up_to)
    }

    fn default_cost(&mut self) -> Result<i64> {
        self.scorer.default_cost()
    }
}
