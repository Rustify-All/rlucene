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
use crate::core::index::field_invert_state::FieldInvertState;
use crate::core::search::collection_statistics::CollectionStatistics;
use crate::core::search::dummy::dumy_sim_scorer::DummySimScorer;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::term_statistics::TermStatistics;
use std::fmt::{Display, Formatter};

pub struct DummySimilarity;

impl Display for DummySimilarity {
    fn fmt(&self, _f: &mut Formatter<'_>) -> std::fmt::Result {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}

impl Similarity for DummySimilarity {
    fn get_discount_overlaps(&self) -> bool {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn compute_norm(
        &self,
        _state: &FieldInvertState,
    ) -> crate::core::util::error::lucene_error::Result<i64> {
        Ok(1)
    }

    type SimScorer = DummySimScorer;

    fn scorer(
        &self,
        _boost: f32,
        _collection_stats: &CollectionStatistics,
        _term_stats: &[TermStatistics],
    ) -> Self::SimScorer {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}
