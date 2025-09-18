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
use crate::core::index::impact::Impact;
use crate::core::index::impacts::Impacts;
use crate::core::index::impacts_source::ImpactsSource;
use crate::core::search::similarities_impl::similarities::SimScorer;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::error::lucene_error::Result;
/// Compute maximum scores based on [`Impacts`] and keep them in a cache
/// in order not to run expensive similarity score computations multiple times
/// on the same data.
///
/// @lucene.internal
pub(crate) struct MaxScoreCache<IS, SS>
where
    IS: ImpactsSource,
    SS: SimScorer,
{
    pub(crate) impacts_source: Option<IS>,
    pub(crate) scorer: SS,
    global_max_score: f32,
    max_score_cache: Vec<f32>,
    max_score_cache_upto: Vec<i32>,
}

impl<IS, SS> MaxScoreCache<IS, SS>
where
    IS: ImpactsSource,
    SS: SimScorer,
{
    pub fn new(impacts_source: Option<IS>, scorer: SS) -> Self {
        let global_max_score = scorer.score(f32::MAX, 1);

        Self {
            impacts_source,
            scorer,
            global_max_score,
            max_score_cache: Vec::new(),
            max_score_cache_upto: Vec::new(),
        }
    }
    /// Implement the contract of [`Scorer::advance_shallow`](ImpactsSource::advance_shallow) based on the wrapped [`ImpactsSource`].
    ///
    /// See also [`Scorer::advance_shallow`].
    pub fn advance_shallow(
        &mut self,
        target: i32,
        impacts_source: &mut Option<&mut IS>,
    ) -> Result<i32> {
        let impacts_source = match self.impacts_source {
            Some(ref mut is) => {
                debug_assert!(impacts_source.is_none());
                is
            },
            None => impacts_source.as_mut().unwrap(),
        };
        impacts_source.advance_shallow(target)?;
        let impacts = impacts_source.get_impacts()?;
        Ok(impacts.get_doc_id_upto(0))
    }

    fn ensure_cache_size(&mut self, size: usize) -> Result<()> {
        if self.max_score_cache.len() < size {
            let old_len = self.max_score_cache.len();
            ArrayUtil::grow_with_len(&mut self.max_score_cache, size);
            let len = self.max_score_cache.len();
            ArrayUtil::grow_exact(&mut self.max_score_cache_upto, len)?;
            self.max_score_cache_upto[old_len..].fill(-1);
        }
        Ok(())
    }

    fn compute_max_score(&self, impacts: &[Impact]) -> f32 {
        let mut max_score = 0.0;
        for impact in impacts {
            let score = self.scorer.score(impact.freq as f32, impact.norm);
            if score > max_score {
                max_score = score;
            }
        }
        max_score
    }
    /// Return the maximum score up to upTo included.
    pub fn get_max_score(
        &mut self,
        up_to: i32,
        impacts_source: &mut Option<&mut IS>,
    ) -> Result<f32> {
        let level = self.get_level(up_to, impacts_source)?;
        if level == -1 {
            Ok(self.global_max_score)
        } else {
            self.get_max_score_for_level(level, impacts_source)
        }
    }

    /// Return the first level that includes all doc IDs up to `up_to`,
    /// or -1 if there is no such level.
    fn get_level(&mut self, up_to: i32, impacts_source: &mut Option<&mut IS>) -> Result<i32> {
        let impacts_source = match self.impacts_source {
            Some(ref mut is) => {
                debug_assert!(impacts_source.is_none());
                is
            },
            None => impacts_source.as_mut().unwrap(),
        };
        let impacts = impacts_source.get_impacts()?;
        let num_levels = impacts.num_levels();
        for level in 0..num_levels {
            let impacts_up_to = impacts.get_doc_id_upto(level);
            if up_to <= impacts_up_to {
                return Ok(level);
            }
        }
        Ok(-1)
    }

    pub fn get_max_score_for_level_zero(
        &mut self,
        impacts_source: &mut Option<&mut IS>,
    ) -> Result<f32> {
        self.get_max_score_for_level(0, impacts_source)
    }
    /// Return the maximum score for the given `level`.
    fn get_max_score_for_level(
        &mut self,
        level: i32,
        impacts_source: &mut Option<&mut IS>,
    ) -> Result<f32> {
        let impacts_source = match self.impacts_source {
            Some(ref mut is) => {
                debug_assert!(impacts_source.is_none());
                is
            },
            None => impacts_source.as_mut().unwrap(),
        };
        debug_assert!(level >= 0, "level must not be negative; got {}", level);
        let mut impacts = impacts_source.get_impacts()?;
        self.ensure_cache_size((level + 1) as usize)?;

        let level_up_to = impacts.get_doc_id_upto(level);
        if self.max_score_cache_upto[level as usize] < level_up_to {
            let max_score = self.compute_max_score(impacts.get_impacts(level)?.as_ref());
            self.max_score_cache[level as usize] = max_score;
            self.max_score_cache_upto[level as usize] = level_up_to;
        }
        Ok(self.max_score_cache[level as usize])
    }

    /// Return the maximum level at which scores are all less than `min_score`,
    /// or -1 if none.
    fn get_skip_level<I>(
        &mut self,
        impacts: &I,
        min_score: f32,
        impacts_source: &mut Option<&mut IS>,
    ) -> Result<i32>
    where
        I: Impacts,
    {
        let num_levels = impacts.num_levels();
        for level in 0..num_levels {
            if self.get_max_score_for_level(level, impacts_source)? >= min_score {
                return Ok(level - 1);
            }
        }
        Ok(num_levels - 1)
    }

    /// Return an inclusive upper bound of documents that all have a score less than `min_score`,
    /// or -1 if the current document may be competitive.
    pub fn get_skip_up_to(
        &mut self,
        min_score: f32,
        impacts_source: &mut Option<&mut IS>,
    ) -> Result<i32> {
        let impacts = impacts_source.as_mut().unwrap().get_impacts()?;
        let level = self.get_skip_level(&impacts, min_score, impacts_source)?;
        if level == -1 {
            Ok(-1)
        } else {
            Ok(impacts.get_doc_id_upto(level))
        }
    }
}
