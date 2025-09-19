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
use crate::core::index::impacts_enum::{Either2ImpactsEnum, ImpactsEnum};
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::postings_enum::PostingsEnum;
use crate::core::index::slow_impacts_enum::SlowImpactsEnum;
use crate::core::search::doc_id_set_iterator::{
    DocIdSetIterator, Either2DocIdSetIterator, Either3DocIdSetIterator,
};
use crate::core::search::dummy::dummy_scorable::DummyScorable;
use crate::core::search::dummy::dummy_two_phase_iterator::DummyTwoPhaseIterator;
use crate::core::search::impacts_disi::ImpactsDISI;
use crate::core::search::max_score_cache::MaxScoreCache;
use crate::core::search::scorable::Scorable;
use crate::core::search::scorer::Scorer;
use crate::core::search::similarities_impl::similarities::SimScorer;
use crate::core::util::error::lucene_error::{LuceneError, Result};

/// Expert: A Scorer for documents matching a Term.
pub struct TermScorer<PE, SS, N, IE>
where
    PE: PostingsEnum,
    SS: SimScorer,
    N: NumericDocValues,
    IE: ImpactsEnum,
{
    norms: Option<N>,
    impacts_disi: Option<ImpactsDISI<IE, IE, SS>>,
    max_score_cache: Option<MaxScoreCache<ImpactsEnums<IE, PE>, SS>>,
}

enum TermScorerPostings<'a, IE, PE, SS>
where
    IE: ImpactsEnum,
    PE: PostingsEnum,
    SS: SimScorer,
{
    Disi(&'a mut ImpactsDISI<IE, IE, SS>),
    Cache(&'a mut ImpactsEnums<IE, PE>),
}

impl<'a, IE, PE, SS> TermScorerPostings<'a, IE, PE, SS>
where
    IE: ImpactsEnum,
    PE: PostingsEnum,
    SS: SimScorer,
{
    fn freq(&mut self) -> Result<i32> {
        match self {
            TermScorerPostings::Disi(disi) => match &mut disi.in_ {
                Either2DocIdSetIterator::A(_) => {
                    Err(LuceneError::illegal_state("should not be here"))
                },
                Either2DocIdSetIterator::B(s) => s.freq(),
            },
            TermScorerPostings::Cache(impacts) => impacts.freq(),
        }
    }

    fn doc_id(&mut self) -> Result<i32> {
        match self {
            TermScorerPostings::Disi(disi) => match &mut disi.in_ {
                Either2DocIdSetIterator::A(_) => {
                    Err(LuceneError::illegal_state("should not be here"))
                },
                Either2DocIdSetIterator::B(s) => Ok(s.doc_id()),
            },
            TermScorerPostings::Cache(impacts) => Ok(impacts.doc_id()),
        }
    }
}
impl<PE, SS, N, IE> TermScorer<PE, SS, N, IE>
where
    PE: PostingsEnum,
    SS: SimScorer,
    N: NumericDocValues,
    IE: ImpactsEnum,
{
    /// Construct a [`TermScorer`] that will iterate all documents.
    pub fn with_postings(postings_enum: PE, scorer: SS, norms: Option<N>) -> Self {
        let impacts_enum = SlowImpactsEnum::new(postings_enum);

        let max_score_cache = MaxScoreCache::new(Some(Either2ImpactsEnum::B(impacts_enum)), scorer);

        Self {
            norms,
            impacts_disi: None,
            max_score_cache: Some(max_score_cache),
        }
    }
    /// Construct a [`TermScorer`] that will use impacts to skip blocks of non-competitive documents.
    pub fn new(
        impacts_enum: IE,
        scorer: SS,
        norms: Option<N>,
        top_level_scoring_clause: bool,
    ) -> Self {
        let (impacts_disi, max_score_cache) = if top_level_scoring_clause {
            let max_score_cache = MaxScoreCache::new(None, scorer);
            let disi = ImpactsDISI::new(Either2DocIdSetIterator::B(impacts_enum), max_score_cache);
            (Some(disi), None)
        } else {
            let max_score_cache =
                MaxScoreCache::new(Some(Either2ImpactsEnum::A(impacts_enum)), scorer);
            (None, Some(max_score_cache))
        };

        TermScorer {
            norms,
            impacts_disi,
            max_score_cache,
        }
    }
    /// Returns term frequency in the current document.
    pub fn freq(&mut self) -> Result<i32> {
        let mut postings = self.postings()?;
        postings.freq()
    }

    fn postings(&mut self) -> Result<TermScorerPostings<'_, IE, PE, SS>> {
        if let Some(disi) = self.impacts_disi.as_mut() {
            debug_assert!(self.max_score_cache.is_none());
            Ok(TermScorerPostings::Disi(disi))
        } else {
            let max_score_cache = self.max_score_cache.as_mut().ok_or_else(|| {
                LuceneError::illegal_state(
                    "when max_score_cache is None, impacts_disi must not be None",
                )
            })?;
            debug_assert!(max_score_cache.impacts_source.is_some());
            let impacts_source = max_score_cache.impacts_source.as_mut().unwrap();
            Ok(TermScorerPostings::Cache(impacts_source))
        }
    }

    fn sim_scorer(&self) -> Result<&SS> {
        if let Some(ref max_score_cache) = self.max_score_cache {
            debug_assert!(self.impacts_disi.is_none());
            Ok(&max_score_cache.scorer)
        } else if let Some(ref disi) = self.impacts_disi {
            Ok(&disi.max_score_cache.scorer)
        } else {
            Err(LuceneError::illegal_state(
                "when max_score_cache is None, impacts_disi must not be None",
            ))
        }
    }
}

impl<PE, SS, N, IE> Scorable for TermScorer<PE, SS, N, IE>
where
    IE: ImpactsEnum,
    N: NumericDocValues,
    PE: PostingsEnum,
    SS: SimScorer,
{
    fn score(&mut self) -> Result<f32> {
        let mut norm = 1;
        let (freq, doc_id) = {
            let mut postings = self.postings()?;
            let freq = postings.freq()?;
            let doc_id = postings.doc_id()?;
            (freq, doc_id)
        };
        if let Some(ref mut norms) = self.norms
            && norms.advance_exact(doc_id)?
        {
            norm = norms.long_value()?;
        }
        let scorer = self.sim_scorer()?;
        Ok(scorer.score(freq as f32, norm))
    }

    fn smoothing_score(&mut self, doc_id: i32) -> Result<f32> {
        let mut norm = 1;
        if let Some(ref mut norms) = self.norms
            && norms.advance_exact(doc_id)?
        {
            norm = norms.long_value()?;
        }
        let scorer = self.sim_scorer()?;
        Ok(scorer.score(0f32, norm))
    }

    fn set_min_competitive_score(&mut self, min_score: f32) -> Result<()> {
        if let Some(impacts_disi) = &mut self.impacts_disi {
            impacts_disi.set_min_competitive_score(min_score);
        }
        Ok(())
    }

    type Scorable = DummyScorable;
}

impl<PE, SS, N, IE> Scorer for TermScorer<PE, SS, N, IE>
where
    PE: PostingsEnum + Default,
    SS: SimScorer,
    N: NumericDocValues,
    IE: ImpactsEnum,
{
    type DocIdSetIterator = TermScorerDisi<IE, PE, SS>;
    type DocIdSetIteratorRef<'a>
        = TermScorerDisiRef<'a, IE, PE, SS>
    where
        Self: 'a;

    type TwoPhaseIter = DummyTwoPhaseIterator;

    fn doc_id(&mut self) -> Result<i32> {
        let mut postings = self.postings()?;
        postings.doc_id()
    }

    fn iterator(&mut self) -> Self::DocIdSetIteratorRef<'_> {
        if self.impacts_disi.is_some() {
            debug_assert!(self.max_score_cache.is_none());
            TermScorerDisiRef::C(self.impacts_disi.as_mut().unwrap())
        } else {
            debug_assert!(self.impacts_disi.is_none());
            debug_assert!(
                self.max_score_cache
                    .as_ref()
                    .unwrap()
                    .impacts_source
                    .is_some()
            );
            match self
                .max_score_cache
                .as_mut()
                .unwrap()
                .impacts_source
                .as_mut()
                .unwrap()
            {
                Either2ImpactsEnum::A(t) => TermScorerDisiRef::A(t),
                Either2ImpactsEnum::B(s) => TermScorerDisiRef::B(&mut s.delegate),
            }
        }
    }

    fn iterator_take(&mut self) -> Self::DocIdSetIterator {
        if self.impacts_disi.is_some() {
            debug_assert!(self.max_score_cache.is_none());
            TermScorerDisi::C(std::mem::take(&mut self.impacts_disi).unwrap())
        } else {
            debug_assert!(self.impacts_disi.is_none());
            debug_assert!(
                self.max_score_cache
                    .as_ref()
                    .unwrap()
                    .impacts_source
                    .is_some()
            );
            match self
                .max_score_cache
                .as_mut()
                .unwrap()
                .impacts_source
                .take()
                .unwrap()
            {
                Either2ImpactsEnum::A(t) => TermScorerDisi::A(t),
                Either2ImpactsEnum::B(mut s) => TermScorerDisi::B(std::mem::take(&mut s.delegate)),
            }
        }
    }

    fn advance_shallow(&mut self, target: i32) -> Result<i32> {
        match self.max_score_cache {
            Some(ref mut max_score_cache) => max_score_cache.advance_shallow(target, &mut None),
            None => match self.impacts_disi {
                Some(ref mut disi) => {
                    let (mut impacts_source, max_score_cache) = {
                        match disi.in_ {
                            Either2DocIdSetIterator::A(_) => {
                                return Err(LuceneError::illegal_state("should not be here"));
                            },
                            Either2DocIdSetIterator::B(ref mut s) => {
                                (Some(s), &mut disi.max_score_cache)
                            },
                        }
                    };
                    max_score_cache.advance_shallow(target, &mut impacts_source)
                },
                None => Err(LuceneError::illegal_state(
                    "when max_score_cache is None, impacts_disi must not be None",
                )),
            },
        }
    }

    fn get_max_score(&mut self, up_to: i32) -> Result<f32> {
        match self.max_score_cache {
            Some(ref mut max_score_cache) => max_score_cache.get_max_score(up_to, &mut None),
            None => match self.impacts_disi {
                Some(ref mut disi) => {
                    let (mut impacts_source, max_score_cache) = {
                        match disi.in_ {
                            Either2DocIdSetIterator::A(_) => {
                                return Err(LuceneError::illegal_state("should not be here"));
                            },
                            Either2DocIdSetIterator::B(ref mut s) => {
                                (Some(s), &mut disi.max_score_cache)
                            },
                        }
                    };
                    max_score_cache.get_max_score(up_to, &mut impacts_source)
                },
                None => Err(LuceneError::illegal_state(
                    "when max_score_cache is None, impacts_disi must not be None",
                )),
            },
        }
    }
}
pub type ImpactsEnums<IE, PE> = Either2ImpactsEnum<IE, SlowImpactsEnum<PE>>;

pub type TermScorerDisi<IE, PE, SS> = Either3DocIdSetIterator<IE, PE, ImpactsDISI<IE, IE, SS>>;
pub type TermScorerDisiRef<'a, IE, PE, SS> =
    Either3DocIdSetIterator<&'a mut IE, &'a mut PE, &'a mut ImpactsDISI<IE, IE, SS>>;
