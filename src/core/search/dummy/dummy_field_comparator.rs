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
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::search::dummy::dummy_leaf_field_comparator::DummyLeafFieldComparator;
use crate::core::search::field_comparator::FieldComparator;

pub struct DummyFieldComparator;
impl FieldComparator for DummyFieldComparator {
    type V = i32;

    fn compare(&self, _slot1: i32, _slot2: i32) -> i32 {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn set_top_value(&mut self, _value: Self::V) {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn value(&self, _slot: i32) -> Option<Self::V> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    type LeafFieldComparator<LR>
        = DummyLeafFieldComparator
    where
        LR: LeafReader;

    fn get_leaf_comparator<LR>(
        &mut self,
        _context: &LeafReaderContext<LR>,
    ) -> crate::core::util::error::lucene_error::Result<Self::LeafFieldComparator<LR>>
    where
        LR: LeafReader,
    {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn compare_values(&self, _first: Option<&Self::V>, _second: Option<&Self::V>) -> i32 {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn fallback_compare(&self, _first: &Self::V, _second: &Self::V) -> i32 {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn set_single_sort(&mut self) {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn disable_skipping(&mut self) {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}
