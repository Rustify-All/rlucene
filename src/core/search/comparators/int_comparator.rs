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
use crate::core::search::comparators::numeric_comparator::{
    NumericComparator, NumericComparatorBase,
};
use crate::core::search::dummy::dummy_leaf_field_comparator::DummyLeafFieldComparator;
use crate::core::search::field_comparator::FieldComparator;
use crate::core::util::numeric_utils::NumericUtils;

/// Comparator based on i32 for numHits.
/// This comparator provides a skipping functionality – an iterator that can skip over non-competitive documents.
pub struct IntComparator {
    values: Vec<i32>,
    top_value: i32,
    bottom: i32,
    missing_value: i32,
    base: NumericComparator<i32>,
}

impl IntComparator {
    pub fn new(num_hits: usize, missing_value: i32, base: NumericComparator<i32>) -> Self {
        Self {
            values: vec![0; num_hits],
            top_value: 0,
            bottom: 0,
            missing_value,
            base,
        }
    }
}

impl NumericComparatorBase for IntComparator {
    fn missing_value_as_comparable_long(&self) -> i64 {
        self.missing_value as i64
    }

    fn sortable_bytes_to_long(&self, bytes: &[u8]) -> i64 {
        NumericUtils::sortable_bytes_to_int(bytes, 0) as i64
    }
}

impl FieldComparator for IntComparator {
    type V = i32;
    fn compare(&self, slot1: i32, slot2: i32) -> i32 {
        self.values[slot1 as usize].cmp(&self.values[slot2 as usize]) as i32
    }

    fn set_top_value(&mut self, value: Self::V) {
        self.base.set_top_value(value);
        self.top_value = value;
    }

    fn value(&self, slot: i32) -> &Self::V {
        &self.values[slot as usize]
    }

    type LeafFieldComparator<LR> = DummyLeafFieldComparator;

    fn get_leaf_comparator<LR>(
        self,
        _context: &LeafReaderContext<LR>,
    ) -> Self::LeafFieldComparator<LR>
    where
        LR: LeafReader,
    {
        todo!()
    }
}
