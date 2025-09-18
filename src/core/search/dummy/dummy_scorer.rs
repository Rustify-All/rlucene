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
use crate::core::search::dummy::dummy_doc_id_set_iterator::DummyDocIdSetIterator;
use crate::core::search::dummy::dummy_two_phase_iterator::DummyTwoPhaseIterator;
use crate::core::search::scorable::{ChildScorable, Scorable};
use crate::core::search::scorer::Scorer;
use crate::core::util::error::lucene_error::Result;

pub struct DummyScorer;

impl Scorable for DummyScorer {
    fn score(&mut self) -> Result<f32> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn smoothing_score(&mut self, _doc_id: i32) -> Result<f32> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn set_min_competitive_score(&mut self, _min_score: f32) -> Result<()> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    type Scorable = DummyScorer;

    fn get_children(&self) -> Result<Vec<ChildScorable<Self::Scorable>>> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}

impl Scorer for DummyScorer {
    type DocIdSetIterator = DummyDocIdSetIterator;
    type DocIdSetIteratorRef<'a>
        = DummyDocIdSetIterator
    where
        Self: 'a;

    type TwoPhaseIter = DummyTwoPhaseIterator;

    fn doc_id(&mut self) -> Result<i32> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn iterator(&mut self) -> Self::DocIdSetIteratorRef<'_> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn iterator_take(&mut self) -> Self::DocIdSetIterator {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn two_phase_iterator(&mut self) -> Option<&mut Self::TwoPhaseIter> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn advance_shallow(&mut self, _target: i32) -> Result<i32> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn get_max_score(&mut self, _up_to: i32) -> Result<f32> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}
