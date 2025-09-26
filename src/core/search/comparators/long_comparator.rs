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
use crate::core::index::doc_values::{DocValues, Numeric};
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::search::comparators::numeric_comparator::{
    CompetitiveIterator, CompetitiveIteratorType, NumericComparator, NumericLeafComparator, ToLong,
};
use crate::core::search::field_comparator::FieldComparator;
use crate::core::search::leaf_field_comparator::LeafFieldComparator;
use crate::core::search::pruning::Pruning;
use crate::core::search::scorable::{Scorable, ScorerEnum};
use crate::core::search::scorer::Scorer;
use crate::core::util::ToInt;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::numeric_utils::NumericUtils;

/// Comparator based on partial_cmp for numHits.
/// This comparator provides a skipping functionality – an iterator that can skip over non-competitive documents.
pub struct LongComparator {
    values: Vec<i64>,
    top_value: i64,
    bottom: i64,
    base: NumericComparator<i64>,
}

impl LongComparator {
    pub fn new(
        field: String,
        num_hits: usize,
        missing_value: Option<i64>,
        reverse: bool,
        pruning: Pruning,
    ) -> Self {
        let missing_value = missing_value.unwrap_or(0);
        let base = NumericComparator::new(
            field,
            missing_value,
            reverse,
            pruning,
            BitUtil::LONG_BYTES as i32,
            missing_value,
        );
        Self {
            values: vec![0; num_hits],
            top_value: 0,
            bottom: 0,
            base,
        }
    }
}

impl FieldComparator for LongComparator {
    type V = i64;

    fn compare(&self, slot1: i32, slot2: i32) -> i32 {
        self.values[slot1 as usize].cmp(&self.values[slot2 as usize]) as i32
    }

    fn set_top_value(&mut self, value: Self::V) {
        self.base.set_top_value();
        self.top_value = value;
    }

    fn value(&self, slot: i32) -> &Self::V {
        &self.values[slot as usize]
    }

    type LeafFieldComparator<LR>
        = LongLeafComparator<LR>
    where
        LR: LeafReader;

    fn get_leaf_comparator<LR>(
        mut self,
        context: &LeafReaderContext<LR>,
    ) -> Result<Self::LeafFieldComparator<LR>>
    where
        LR: LeafReader,
    {
        let v = std::mem::take(&mut self.base);
        LongLeafComparator::new(self, context, v)
    }
}
pub struct LongLeafComparator<LR>
where
    LR: LeafReader,
{
    comparator: LongComparator,
    base: NumericLeafComparator<LR, Numeric<LR>, i64, LongConverter>,
}
impl<LR> LongLeafComparator<LR>
where
    LR: LeafReader,
{
    pub fn new(
        comparator: LongComparator,
        context: &LeafReaderContext<LR>,
        nc: NumericComparator<i64>,
    ) -> Result<Self> {
        let doc_value = DocValues::get_numeric(context.reader(), &comparator.base.field)?;
        let candidate = DocValues::get_numeric(context.reader(), &comparator.base.field)?;
        let top_value = comparator.top_value;
        let base = NumericLeafComparator::new(
            context,
            nc,
            doc_value,
            candidate,
            LongConverter,
            top_value,
        )?;
        Ok(Self { comparator, base })
    }
    fn get_value_for_doc(&mut self, doc: i32) -> Result<i64> {
        let doc_values = &mut self.base.doc_values;
        if doc_values.advance_exact(doc)? {
            Ok(doc_values.long_value()?)
        } else {
            Ok(self.base.parent.missing_value)
        }
    }
}

impl<LR> LeafFieldComparator for LongLeafComparator<LR>
where
    LR: LeafReader,
{
    fn set_bottom(&mut self, slot: usize) -> Result<()> {
        self.comparator.bottom = self.comparator.values[slot];
        self.base
            .set_bottom(self.comparator.bottom, self.comparator.top_value)
    }

    fn compare_bottom(&mut self, doc: i32) -> Result<i32> {
        let v = self.get_value_for_doc(doc)?;
        Ok(self.comparator.bottom.cmp(&v).to_int())
    }

    fn compare_top(&mut self, doc: i32) -> Result<i32> {
        let v = self.get_value_for_doc(doc)?;
        Ok(self.comparator.top_value.cmp(&v).to_int())
    }

    fn copy(&mut self, slot: usize, doc: i32) -> Result<()> {
        let v = self.get_value_for_doc(doc)?;
        self.comparator.values[slot] = v;
        self.base.copy(doc)
    }

    fn set_scorer<S1, S2>(&mut self, scorer: &ScorerEnum<S1, S2>) -> Result<()>
    where
        S1: Scorer,
        S2: Scorable,
    {
        self.base
            .set_scorer(scorer, self.comparator.bottom, self.comparator.top_value)
    }

    type DocIdSetIterator = CompetitiveIterator<CompetitiveIteratorType<Numeric<LR>>>;

    fn competitive_iterator(&mut self) -> Option<Self::DocIdSetIterator> {
        self.base.competitive_iterator()
    }

    fn set_hits_threshold_reached(&mut self) -> Result<()> {
        self.base
            .set_hits_threshold_reached(self.comparator.bottom, self.comparator.top_value)
    }
}
pub(crate) struct LongConverter;
impl ToLong for LongConverter {
    type V = i64;

    fn value_to_long(&self, v: Self::V) -> i64 {
        v
    }

    fn bytes_to_long(&self, bytes: &[u8]) -> i64 {
        NumericUtils::sortable_bytes_to_long(bytes, 0)
    }
}
