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
use crate::core::index::doc_values::DocValues;
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::search::comparators::numeric_comparator::{
    NumericComparator, NumericCompetitiveIterator, NumericLeafComparator,
    NumericLeafComparatorDocValues, ToLong,
};
use crate::core::search::field_comparator::FieldComparator;
use crate::core::search::leaf_field_comparator::LeafFieldComparator;
use crate::core::search::pruning::Pruning;
use crate::core::search::scorable::Scorable;
use crate::core::util::ToInt;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::numeric_utils::NumericUtils;

/// Comparator based on partial_cmp for numHits.
/// This comparator provides a skipping functionality – an iterator that can skip over non-competitive documents.
pub struct LongComparator {
    values: Vec<i64>,
    top_value: i64,
    bottom: i64,
    pub(crate) base: NumericComparator<i64>,
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

    fn value(&self, slot: i32) -> Option<Self::V> {
        Some(self.values[slot as usize])
    }

    type LeafFieldComparator<LR>
        = LongLeafComparator<LR>
    where
        LR: LeafReader;

    fn get_leaf_comparator<LR>(
        &mut self,
        context: &LeafReaderContext<LR>,
    ) -> Result<Self::LeafFieldComparator<LR>>
    where
        LR: LeafReader,
    {
        LongLeafComparator::new(self, context, None, None)
    }

    fn disable_skipping(&mut self) {
        self.base.disable_skipping();
    }
}
pub struct LongLeafComparator<LR>
where
    LR: LeafReader,
{
    base: NumericLeafComparator<LR, NumericLeafComparatorDocValues<LR>, i64, LongConverter>,
}
impl<LR> LongLeafComparator<LR>
where
    LR: LeafReader,
{
    pub fn new(
        comparator: &mut LongComparator,
        context: &LeafReaderContext<LR>,
        doc_values: Option<NumericLeafComparatorDocValues<LR>>,
        candidate: Option<NumericLeafComparatorDocValues<LR>>,
    ) -> Result<Self> {
        let (doc_value, candidate) = match (doc_values, candidate) {
            (Some(v1), Some(v2)) => (v1, v2),
            (None, None) => {
                let v1 = NumericLeafComparatorDocValues::<LR>::B(DocValues::get_numeric(
                    context.reader(),
                    &comparator.base.field,
                )?);
                let v2 = NumericLeafComparatorDocValues::<LR>::B(DocValues::get_numeric(
                    context.reader(),
                    &comparator.base.field,
                )?);
                (v1, v2)
            },
            _ => {
                return Err(LuceneError::illegal_state(
                    "doc_values and candidate must be both Some or None",
                ));
            },
        };
        let top_value = comparator.top_value;
        let base = NumericLeafComparator::new(
            context,
            &mut comparator.base,
            doc_value,
            candidate,
            LongConverter,
            top_value,
        )?;
        Ok(Self { base })
    }
    fn get_value_for_doc(
        &mut self,
        doc: i32,
        comparator: &mut NumericComparator<i64>,
    ) -> Result<i64> {
        let doc_values = &mut self.base.doc_values;
        if doc_values.advance_exact(doc)? {
            Ok(doc_values.long_value()?)
        } else {
            Ok(comparator.missing_value)
        }
    }
}

impl<LR> LeafFieldComparator for LongLeafComparator<LR>
where
    LR: LeafReader,
{
    type FieldComparator = LongComparator;
    fn set_bottom(&mut self, slot: usize, comparator: &mut Self::FieldComparator) -> Result<()> {
        comparator.bottom = comparator.values[slot];
        self.base.set_bottom(
            comparator.bottom,
            comparator.top_value,
            &mut comparator.base,
        )
    }

    fn compare_bottom<S>(
        &mut self,
        doc: i32,
        _scorer: &mut S,
        comparator: &mut Self::FieldComparator,
    ) -> Result<i32>
    where
        S: Scorable,
    {
        let v = self.get_value_for_doc(doc, &mut comparator.base)?;
        Ok(comparator.bottom.cmp(&v).to_int())
    }

    fn compare_top<S>(
        &mut self,
        doc: i32,
        _scorer: &mut S,
        comparator: &mut Self::FieldComparator,
    ) -> Result<i32>
    where
        S: Scorable,
    {
        let v = self.get_value_for_doc(doc, &mut comparator.base)?;
        Ok(comparator.top_value.cmp(&v).to_int())
    }

    fn copy<S>(
        &mut self,
        slot: usize,
        doc: i32,
        _scorer: &mut S,
        comparator: &mut Self::FieldComparator,
    ) -> Result<()>
    where
        S: Scorable,
    {
        let v = self.get_value_for_doc(doc, &mut comparator.base)?;
        comparator.values[slot] = v;
        self.base.copy(doc)
    }

    fn set_scorer<S>(
        &mut self,
        scorer: &mut S,
        comparator: &mut Self::FieldComparator,
    ) -> Result<()>
    where
        S: Scorable,
    {
        self.base.set_scorer(
            scorer,
            comparator.bottom,
            comparator.top_value,
            &mut comparator.base,
        )
    }

    type DocIdSetIteratorRef<'a>
        = &'a mut NumericCompetitiveIterator<LR>
    where
        LR: 'a;

    fn competitive_iterator(
        &mut self,
        _comparator: &mut Self::FieldComparator,
    ) -> Result<Option<Self::DocIdSetIteratorRef<'_>>> {
        Ok(self.base.competitive_iterator())
    }

    fn set_hits_threshold_reached(&mut self, comparator: &mut Self::FieldComparator) -> Result<()> {
        self.base.set_hits_threshold_reached(
            comparator.bottom,
            comparator.top_value,
            &mut comparator.base,
        )
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
