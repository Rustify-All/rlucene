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
use crate::core::search::dummy::dummy_disi::DummyDISI;
use crate::core::search::leaf_field_comparator::LeafFieldComparator;
use crate::core::search::scorable::{Scorable, ScorerEnum};
use crate::core::search::scorer::Scorer;
use crate::core::util::error::lucene_error::Result;

pub struct DummyLeafFieldComparator;
impl LeafFieldComparator for DummyLeafFieldComparator {
    fn set_bottom(&mut self, _slot: usize) -> Result<()> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn compare_bottom<S1, S2>(&mut self, _doc: i32, _scorer: &ScorerEnum<S1, S2>) -> Result<i32>
    where
        S1: Scorer,
        S2: Scorable,
    {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn compare_top(&mut self, _doc: i32) -> Result<i32> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn copy<S1, S2>(&mut self, _slot: usize, _doc: i32, _scorer: &ScorerEnum<S1, S2>) -> Result<()>
    where
        S1: Scorer,
        S2: Scorable,
    {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn set_scorer<S1, S2>(&mut self, _scorer: &ScorerEnum<S1, S2>) -> Result<()>
    where
        S1: Scorer,
        S2: Scorable,
    {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    type DocIdSetIterator = DummyDISI;

    fn competitive_iterator(&mut self) -> Option<Self::DocIdSetIterator> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn set_hits_threshold_reached(&mut self) -> Result<()> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}
