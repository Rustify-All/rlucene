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
use crate::core::search::comparators::numeric_comparator::NumericComparator;
use crate::core::search::dummy::dummy_leaf_field_comparator::DummyLeafFieldComparator;
use crate::core::search::field_comparator::FieldComparator;
use crate::core::search::pruning::Pruning;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::numeric_utils::NumericUtils;
/// Comparator based partial_cmp on for numHits.
/// This comparator provides a skipping functionality – an iterator that can skip over non-competitive documents.
pub struct FloatComparator {
    values: Vec<f32>,
    top_value: f32,
    bottom: f32,
    missing_value: f32,
    base: NumericComparator<f32>,
}

impl FloatComparator {
    pub fn new(
        field: String,
        num_hits: usize,
        missing_value: Option<f32>,
        reverse: bool,
        pruning: Pruning,
    ) -> Self {
        let missing_value = missing_value.unwrap_or(0f32);
        let base = NumericComparator::new(
            field,
            missing_value,
            reverse,
            pruning,
            BitUtil::FLOAT_BYTES as i32,
            NumericUtils::float_to_sortable_int(missing_value) as i64,
        );
        Self {
            values: vec![0.0; num_hits],
            top_value: 0.0,
            bottom: 0.0,
            missing_value,
            base,
        }
    }
}

impl FieldComparator for FloatComparator {
    type V = f32;

    fn compare(&self, slot1: i32, slot2: i32) -> i32 {
        self.values[slot1 as usize]
            .partial_cmp(&self.values[slot2 as usize])
            .unwrap_or(std::cmp::Ordering::Equal) as i32
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

    fn fallback_compare(&self, first: &Self::V, second: &Self::V) -> Result<i32> {
        if first.is_nan() && second.is_nan() {
            Ok(0)
        } else if first.is_nan() {
            Ok(1)
        } else if second.is_nan() {
            Ok(-1)
        } else {
            Ok(0)
        }
    }
}
