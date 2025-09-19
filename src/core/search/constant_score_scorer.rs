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
use crate::core::search::doc_id_set_iterator::{
    DocIdSetIterator, Either2DocIdSetIterator, EitherEmpty, EmptyDISI,
};
use crate::core::search::dummy::dummy_scorable::DummyScorable;
use crate::core::search::scorable::Scorable;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::Scorer;
use crate::core::search::two_phase_iterator::{
    Either2TwoPhaseIterator, TwoPhaseIterator, TwoPhaseIteratorAsDocIdSetIterator,
};
use crate::core::util::error::lucene_error::{LuceneError, Result};
/// A constant-scoring Scorer.
pub struct ConstantScoreScorer<DISI, TPI>
where
    DISI: DocIdSetIterator,
    TPI: TwoPhaseIterator,
{
    score: f32,
    score_mode: ScoreMode,
    disi: ConstantDISI_<DISI, TPI>,
}
impl<DISI, TPI> ConstantScoreScorer<DISI, TPI>
where
    DISI: DocIdSetIterator,
    TPI: TwoPhaseIterator,
{
    /// Constructor based on a [`DocIdSetIterator`] used to drive iteration. Two-phase
    /// iteration is not supported.
    ///
    /// # Parameters
    /// - `score`: the score to return on each document.
    /// - `score_mode`: the score mode.
    /// - `disi`: the iterator that defines matching documents.
    pub fn with_disi(score: f32, score_mode: ScoreMode, disi: DISI) -> Self {
        let approximation = match score_mode {
            ScoreMode::TopScores => {
                ConstantDISI::A(DocIdSetIteratorWrapper::new(EitherEmpty::A(disi)))
            },
            _ => ConstantDISI::B(disi),
        };
        Self {
            score,
            score_mode,
            disi: Either2DocIdSetIterator::A(approximation),
        }
    }
    /// Constructor based on a [`TwoPhaseIterator`]. In this case the [`Scorer`] will
    /// support two-phase iteration.
    ///
    /// # Parameters
    /// - `score`: the score to return on each document.
    /// - `score_mode`: the score mode.
    /// - `two_phase_iterator`: the iterator that defines matching documents.
    pub fn with_tpi(score: f32, score_mode: ScoreMode, two_phase_iterator: TPI) -> Self {
        let two_phase_iterator = match score_mode {
            ScoreMode::TopScores => ConstantTPI::A(TwoPhaseIteratorImpl::new(two_phase_iterator)),
            _ => ConstantTPI::B(two_phase_iterator),
        };
        Self {
            score,
            score_mode,
            disi: Either2DocIdSetIterator::B(TwoPhaseIteratorAsDocIdSetIterator::new(
                two_phase_iterator,
            )),
        }
    }
}

impl<DISI, TPI> Scorable for ConstantScoreScorer<DISI, TPI>
where
    DISI: DocIdSetIterator,
    TPI: TwoPhaseIterator,
{
    fn score(&mut self) -> Result<f32> {
        Ok(self.score)
    }

    fn set_min_competitive_score(&mut self, min_score: f32) -> Result<()> {
        if min_score > self.score && matches!(self.score_mode, ScoreMode::TopScores) {
            match self.disi {
                ConstantDISI_::A(ref mut v) => match v {
                    Either2DocIdSetIterator::A(v) => {
                        v.delegate = EitherEmpty::B(EmptyDISI::new());
                    },
                    Either2DocIdSetIterator::B(_) => {
                        return Err(LuceneError::illegal_state("not DocIdSetIteratorWrapper"));
                    },
                },
                ConstantDISI_::B(ref mut v) => v.two_phase_iterator.set_empty(),
            }
        }
        Ok(())
    }

    type Scorable = DummyScorable;
}

impl<DISI, TPI> Scorer for ConstantScoreScorer<DISI, TPI>
where
    DISI: DocIdSetIterator,
    TPI: TwoPhaseIterator,
{
    type DocIdSetIterator = ConstantDISI_<DISI, TPI>;
    type DocIdSetIteratorRef<'a>
        = EitherEmpty<&'a mut ConstantDISI_<DISI, TPI>>
    where
        Self: 'a;

    type TwoPhaseIter = ConstantTPI<TPI>;

    fn doc_id(&mut self) -> Result<i32> {
        Ok(self.disi.doc_id())
    }

    fn iterator(&mut self) -> Self::DocIdSetIteratorRef<'_> {
        EitherEmpty::A(&mut self.disi)
    }

    fn iterator_take(&mut self) -> Self::DocIdSetIterator {
        std::mem::replace(
            &mut self.disi,
            Either2DocIdSetIterator::A(ConstantDISI::A(DocIdSetIteratorWrapper::new(
                EitherEmpty::B(EmptyDISI::new()),
            ))),
        )
    }

    fn two_phase_iterator(&mut self) -> Option<&mut Self::TwoPhaseIter> {
        match self.disi {
            ConstantDISI_::A(_) => None,
            ConstantDISI_::B(ref mut v) => Some(&mut v.two_phase_iterator),
        }
    }

    fn get_max_score(&mut self, _up_to: i32) -> Result<f32> {
        Ok(self.score)
    }
}

pub struct TwoPhaseIteratorImpl<TPI>
where
    TPI: TwoPhaseIterator,
{
    two_phase_iterator: TPI,
}
impl<TPI> TwoPhaseIteratorImpl<TPI>
where
    TPI: TwoPhaseIterator,
{
    pub fn new(two_phase_iterator: TPI) -> Self {
        Self { two_phase_iterator }
    }
}
impl<TPI> TwoPhaseIterator for TwoPhaseIteratorImpl<TPI>
where
    TPI: TwoPhaseIterator,
{
    type DocIdSetIterator = TPI::DocIdSetIterator;

    fn approximation_mut(&mut self) -> &mut Self::DocIdSetIterator {
        self.two_phase_iterator.approximation_mut()
    }

    fn approximation(&self) -> &Self::DocIdSetIterator {
        self.two_phase_iterator.approximation()
    }

    fn take_approximation(&mut self) -> Self::DocIdSetIterator {
        self.two_phase_iterator.take_approximation()
    }

    fn set_empty(&mut self) {
        self.two_phase_iterator.set_empty()
    }

    fn matches(&mut self) -> Result<bool> {
        self.two_phase_iterator.matches()
    }

    fn match_cost(&self) -> f32 {
        self.two_phase_iterator.match_cost()
    }
}
// used for Constructor from DISI
pub type ConstantDISI<DISI> =
    Either2DocIdSetIterator<DocIdSetIteratorWrapper<EitherEmpty<DISI>>, DISI>;
// used Constructor from TwoPhaseIterator
pub type ConstantTPI<TPI> = Either2TwoPhaseIterator<TwoPhaseIteratorImpl<TPI>, TPI>;

pub type ConstantDISI_<DISI, TPI> = Either2DocIdSetIterator<
    ConstantDISI<DISI>,
    TwoPhaseIteratorAsDocIdSetIterator<ConstantTPI<TPI>>,
>;

pub struct DocIdSetIteratorWrapper<D>
where
    D: DocIdSetIterator,
{
    doc: i32,
    delegate: D,
}

impl<D> DocIdSetIteratorWrapper<D>
where
    D: DocIdSetIterator,
{
    pub fn new(delegate: D) -> Self {
        Self { doc: -1, delegate }
    }
}

impl<D> DocIdSetIterator for DocIdSetIteratorWrapper<D>
where
    D: DocIdSetIterator,
{
    fn doc_id(&self) -> i32 {
        self.doc
    }

    fn next_doc(&mut self) -> Result<i32> {
        self.doc = self.delegate.next_doc()?;
        Ok(self.doc)
    }

    fn advance(&mut self, target: i32) -> Result<i32> {
        self.doc = self.delegate.advance(target)?;
        Ok(self.doc)
    }

    fn cost(&self) -> Result<i64> {
        self.delegate.cost()
    }
}
