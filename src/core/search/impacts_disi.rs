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
use crate::core::index::impacts_enum::ImpactsEnum;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, Either2DocIdSetIterator};
use crate::core::search::max_score_cache::MaxScoreCache;
use crate::core::search::similarities_impl::similarities::SimScorer;
use crate::core::util::error::lucene_error::Result;
/// [`DocIdSetIterator`] that skips non-competitive docs thanks to the indexed impacts.
/// Call [`set_min_competitive_score`](ImpactsDISI::set_min_competitive_score) in order to give this
/// iterator the ability to skip low-scoring documents.
///
/// @lucene.internal
pub struct ImpactsDISI<I, IE, SS>
where
    I: DocIdSetIterator,
    IE: ImpactsEnum,
    SS: SimScorer,
{
    pub(crate) in_: Disi<I, IE>,
    pub(crate) max_score_cache: MaxScoreCache<IE, SS>,
    min_competitive_score: f32,
    up_to: i32,
    max_score: f32,
}
impl<I, IE, SS> ImpactsDISI<I, IE, SS>
where
    I: DocIdSetIterator,
    IE: ImpactsEnum,
    SS: SimScorer,
{
    pub fn new(in_: Disi<I, IE>, max_score_cache: MaxScoreCache<IE, SS>) -> Self {
        Self {
            in_,
            max_score_cache,
            min_competitive_score: 0.0,
            up_to: NO_MORE_DOCS,
            max_score: f32::INFINITY,
        }
    }
    /// Get the [`MaxScoreCache`].
    pub fn max_score_cache(&self) -> &MaxScoreCache<IE, SS> {
        &self.max_score_cache
    }

    /// Set the minimum competitive score.
    ///
    /// See also [`Scorer::set_min_competitive_score`](crate::core::search::scorer::Scorer::set_min_competitive_score).
    pub fn set_min_competitive_score(&mut self, min_competitive_score: f32) {
        debug_assert!(min_competitive_score >= self.min_competitive_score);
        if min_competitive_score > self.min_competitive_score {
            self.min_competitive_score = min_competitive_score;
            // force `up_to` and `max_score` to be recomputed so that we will skip
            // documents if the current block of documents is not competitive
            // only if the min competitive score actually increased
            self.up_to = -1;
        }
    }
    fn advance_target(&mut self, mut target: i32) -> Result<i32> {
        if target <= self.up_to {
            // we are still in the current block, which is considered competitive
            // according to impacts, no skipping
            return Ok(target);
        }
        let (mut impacts_source, max_score_cache) = {
            match self.in_ {
                Either2DocIdSetIterator::A(ref mut t) => (None, &mut self.max_score_cache),
                Either2DocIdSetIterator::B(ref mut s) => (Some(s), &mut self.max_score_cache),
            }
        };
        self.up_to = max_score_cache.advance_shallow(target, &mut impacts_source)?;
        self.max_score = max_score_cache.get_max_score_for_level_zero(&mut impacts_source)?;

        loop {
            debug_assert!(self.up_to >= target);

            if self.max_score >= self.min_competitive_score {
                return Ok(target);
            }

            if self.up_to == NO_MORE_DOCS {
                return Ok(NO_MORE_DOCS);
            }

            let skip_up_to =
                max_score_cache.get_skip_up_to(self.min_competitive_score, &mut impacts_source)?;
            if skip_up_to == -1 {
                // no further skipping
                target = self.up_to + 1;
            } else if skip_up_to == NO_MORE_DOCS {
                return Ok(NO_MORE_DOCS);
            } else {
                target = skip_up_to + 1;
            }

            self.up_to = max_score_cache.advance_shallow(target, &mut impacts_source)?;
            self.max_score = max_score_cache.get_max_score_for_level_zero(&mut impacts_source)?;
        }
    }
}
impl<I, IE, SS> DocIdSetIterator for ImpactsDISI<I, IE, SS>
where
    I: DocIdSetIterator,
    IE: ImpactsEnum,
    SS: SimScorer,
{
    fn doc_id(&self) -> i32 {
        self.in_.doc_id()
    }

    fn next_doc(&mut self) -> Result<i32> {
        let doc = self.in_.doc_id();
        if doc < self.up_to {
            self.in_.next_doc()
        } else {
            self.advance(doc + 1)
        }
    }

    fn advance(&mut self, target: i32) -> Result<i32> {
        self.in_.advance(target)
    }

    fn cost(&self) -> Result<i64> {
        self.in_.cost()
    }
}

type Disi<I, IE> = Either2DocIdSetIterator<I, IE>;
