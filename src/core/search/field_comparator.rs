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
use crate::core::index::BytesRef;
use crate::core::index::binary_doc_values::BinaryDocValues;
use crate::core::index::doc_values::{Binary, DocValues};
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::search::comparators::doc_comparator::DocComparator;
use crate::core::search::comparators::double_comparator::DoubleComparator;
use crate::core::search::comparators::float_comparator::FloatComparator;
use crate::core::search::comparators::int_comparator::IntComparator;
use crate::core::search::comparators::long_comparator::LongComparator;
use crate::core::search::dummy::dummy_doc_id_set_iterator::DummyDocIdSetIterator;
use crate::core::search::leaf_field_comparator::{LeafFieldComparator, LeafFieldComparatorEnum};
use crate::core::search::scorable::{Scorable, ScorerEnum};
use crate::core::search::scorer::Scorer;
use crate::core::util::ToInt;
use crate::core::util::error::lucene_error::Result;
use std::borrow::Cow;
use std::cmp::Ordering;

/// Expert: a `FieldComparator` compares hits so as to determine their sort order when collecting the
/// top results with [`TopFieldCollector`](crate::core::search::top_field_collector::TopFieldCollector).
/// The concrete public `FieldComparator` implementations
/// correspond to the `SortField` types.
///
/// The document IDs passed to these methods must only move forwards, since they are using doc
/// values iterators to retrieve sort values.
///
/// This API is designed to achieve high performance sorting, by exposing a tight interaction with
/// [`FieldValueHitQueue`](crate::core::search::field_value_hit_queue::FieldValueHitQueue) as it visits hits. Whenever a hit is competitive, it's enrolled into a
/// virtual slot, which is an int ranging from 0 to numHits-1. Segment transitions are handled by
/// creating a dedicated per-segment [`LeafFieldComparator`] which also needs to interact with the
/// [`FieldValueHitQueue`](crate::core::search::field_value_hit_queue::FieldValueHitQueue) but can optimize based on the segment to collect.
///
/// The following functions need to be implemented:
/// - `compare` Compare a hit at 'slot a' with hit 'slot b'.
/// - [`Self::set_top_value`] Called by [`TopFieldCollector`](crate::core::search::top_field_collector::TopFieldCollector) to notify the comparator of the top most
///   value, which is used by future calls to [`LeafFieldComparator::compare_top`].
/// - [`get_leaf_comparator`] Invoked when the search is switching to the next segment. You may need
///   to update internal state of the comparator, e.g. retrieving new values from DocValues.
/// - `value` Return the sort value stored in the specified slot. This is only called at the end of
///   the search, in order to populate [`FieldDoc::fields`](crate::core::search::field_doc::FieldDoc) when returning the top results.
///
/// See also:
/// - [`LeafFieldComparator`]
/// - `lucene.experimental`
pub trait FieldComparator {
    // f64 f32 not implement Ord
    type V: PartialOrd;
    /// Compare hit at slot1 with hit at slot2.
    ///
    /// Returns:
    /// - `N < 0` if slot2's value is sorted after slot1
    /// - `N > 0` if slot2's value is sorted before slot1
    /// - `0` if they are equal
    fn compare(&self, slot1: i32, slot2: i32) -> i32;

    /// Record the top value, for future calls to [`LeafFieldComparator::compare_top`].
    /// This is only called for searches that use `search_after` (deep paging),
    /// and is invoked before any calls to [`Self::get_leaf_comparator`].
    fn set_top_value(&mut self, value: Self::V);

    /// Return the actual value in the slot.
    ///
    /// # Parameters
    /// - `slot`: the slot index
    ///
    /// # Returns
    /// The value stored in this slot.
    fn value(&self, slot: i32) -> Self::V;

    type LeafFieldComparator<LR>: LeafFieldComparator
    where
        LR: LeafReader;
    /// Get a per-segment [`LeafFieldComparator`] to collect the given
    /// [`LeafReaderContext`].
    ///
    /// All docIDs supplied to this [`LeafFieldComparator`] are relative to the current reader
    /// (you must add `docBase` if you need to map it to a top-level docID).
    ///
    /// # Parameters
    /// - `context`: current reader context
    ///
    /// # Returns
    /// The comparator to use for this segment.
    ///
    /// # Errors
    /// Returns an error if there is a low-level I/O problem.
    fn get_leaf_comparator<LR>(
        self,
        context: &LeafReaderContext<LR>,
    ) -> Result<Self::LeafFieldComparator<LR>>
    where
        LR: LeafReader;

    /// Returns a negative integer if `first` is less than `second`, `0` if they are equal,
    /// and a positive integer otherwise.
    ///
    /// Default implementation assumes the type implements [`Ord`] (like Java's `Comparable`)
    /// and invokes `.cmp`.
    ///
    /// Be sure to override this method if your `FieldComparator`'s type isn't comparable
    /// or if your values may sometimes be `null` (represented as [`Option::None`] in Rust).
    fn compare_values(&self, first: Option<&Self::V>, second: Option<&Self::V>) -> i32 {
        match (first, second) {
            (None, None) => 0,
            (None, Some(_)) => -1,
            (Some(_), None) => 1,
            (Some(f), Some(s)) => {
                match f.partial_cmp(s) {
                    Some(ord) => ord.to_int(),
                    // In case of NaN for f64 or other non-comparable values
                    None => self.fallback_compare(f, s),
                }
            },
        }
    }
    fn fallback_compare(&self, _first: &Self::V, _second: &Self::V) -> i32 {
        unimplemented!("fallback_compare must be implemented if the type isn't fully comparable");
    }
    /// Informs the comparator that sort is done on this single field.
    /// This is useful to enable some optimizations for skipping non-competitive documents.
    fn set_single_sort(&mut self) {}

    /// Informs the comparator that the skipping of documents should be disabled.
    /// This function is called by [`TopFieldCollector`](crate::core::search::top_field_collector::TopFieldCollector) in cases when the skipping functionality
    /// should not be applied or not necessary.
    ///
    /// An example could be when search sort is a part of the index sort, and can be already efficiently
    /// handled by [`TopFieldCollector`](crate::core::search::top_field_collector::TopFieldCollector), and doing extra work for skipping in the comparator is redundant.
    fn disable_skipping(&mut self) {}
}
/// Sorts by descending relevance.
///
/// NOTE: if you are sorting only by descending relevance and then
/// secondarily by ascending docID, performance is faster using
/// [`TopScoreDocCollector`](crate::core::search::top_score_doc_collector::TopScoreDocCollector) directly (which [`IndexSearcher::search`](crate::core::search::index_searcher::IndexSearcher) uses
/// when no [`Sort`] is specified).
pub struct RelevanceComparator {
    pub(crate) scores: Vec<f32>,
    pub(crate) bottom: f32,
    pub(crate) top_value: f32,
}
impl RelevanceComparator {
    pub fn new(num_hits: i32) -> Self {
        Self {
            scores: vec![0.0; num_hits as usize],
            bottom: 0.0,
            top_value: 0.0,
        }
    }
}
impl FieldComparator for RelevanceComparator {
    type V = f32;

    fn compare(&self, slot1: i32, slot2: i32) -> i32 {
        let slot1_v = self.scores[slot1 as usize];
        let slot2_v = self.scores[slot2 as usize];
        match slot1_v.partial_cmp(&slot2_v) {
            Some(r) => r.to_int(),
            None => self.fallback_compare(&slot1_v, &slot2_v),
        }
    }

    fn set_top_value(&mut self, value: Self::V) {
        self.top_value = value
    }

    fn value(&self, slot: i32) -> Self::V {
        self.scores[slot as usize]
    }

    type LeafFieldComparator<LR>
        = RelevanceLeafComparator
    where
        LR: LeafReader;

    fn get_leaf_comparator<LR>(
        self,
        _context: &LeafReaderContext<LR>,
    ) -> Result<Self::LeafFieldComparator<LR>>
    where
        LR: LeafReader,
    {
        Ok(RelevanceLeafComparator::new(self))
    }

    fn compare_values(&self, first: Option<&Self::V>, second: Option<&Self::V>) -> i32 {
        match (first, second) {
            (Some(&f), Some(&s)) => {
                // Reversed intentionally because relevance by default
                // sorts descending:
                match s.partial_cmp(&f) {
                    Some(r) => r.to_int(),
                    None => self.fallback_compare(&s, &f),
                }
            },
            (None, Some(_)) => 1,
            (Some(_), None) => -1,
            (None, None) => 0,
        }
    }

    fn fallback_compare(&self, first: &Self::V, second: &Self::V) -> i32 {
        if first.is_nan() && second.is_nan() {
            0
        } else if first.is_nan() {
            1
        } else if second.is_nan() {
            -1
        } else {
            0
        }
    }
}
pub struct RelevanceLeafComparator {
    comparator: RelevanceComparator,
}
impl RelevanceLeafComparator {
    pub fn new(comparator: RelevanceComparator) -> Self {
        Self { comparator }
    }
}
impl LeafFieldComparator for RelevanceLeafComparator {
    fn set_bottom(&mut self, slot: usize) -> Result<()> {
        self.comparator.bottom = self.comparator.scores[slot];
        Ok(())
    }

    fn compare_bottom<S1, S2>(&mut self, _doc: i32, scorer: &mut ScorerEnum<S1, S2>) -> Result<i32>
    where
        S1: Scorer,
        S2: Scorable,
    {
        let doc_value = match scorer {
            ScorerEnum::Scorer(s) => s.score()?,
            ScorerEnum::Scorable(s) => s.score()?,
        };
        debug_assert!(!doc_value.is_nan());
        match doc_value.partial_cmp(&self.comparator.bottom) {
            Some(r) => Ok(r.to_int()),
            None => Ok(self
                .comparator
                .fallback_compare(&doc_value, &self.comparator.bottom)),
        }
    }

    fn compare_top<S1, S2>(&mut self, _doc: i32, scorer: &mut ScorerEnum<S1, S2>) -> Result<i32>
    where
        S1: Scorer,
        S2: Scorable,
    {
        let doc_value = match scorer {
            ScorerEnum::Scorer(s) => s.score()?,
            ScorerEnum::Scorable(s) => s.score()?,
        };
        debug_assert!(!doc_value.is_nan());
        match doc_value.partial_cmp(&self.comparator.top_value) {
            Some(r) => Ok(r.to_int()),
            None => Ok(self
                .comparator
                .fallback_compare(&doc_value, &self.comparator.top_value)),
        }
    }

    fn copy<S1, S2>(
        &mut self,
        slot: usize,
        _doc: i32,
        scorer: &mut ScorerEnum<S1, S2>,
    ) -> Result<()>
    where
        S1: Scorer,
        S2: Scorable,
    {
        let score = match scorer {
            ScorerEnum::Scorer(s) => s.score()?,
            ScorerEnum::Scorable(s) => s.score()?,
        };
        self.comparator.scores[slot] = score;
        debug_assert!(!score.is_nan());
        Ok(())
    }

    fn set_scorer<S1, S2>(&mut self, _scorer: &mut ScorerEnum<S1, S2>) -> Result<()>
    where
        S1: Scorer,
        S2: Scorable,
    {
        Ok(())
    }

    type DocIdSetIterator = DummyDocIdSetIterator;
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum FieldComparatorValue {
    #[default]
    Missing,
    Doc(i32),
    Double(f64),
    Float(f32),
    Int(i32),
    Long(i64),
    TermVal(BytesRef<Vec<u8>>),
}

impl FieldComparatorValue {
    fn missing() -> Self {
        FieldComparatorValue::Missing
    }

    fn as_i32(&self) -> Option<&i32> {
        match self {
            FieldComparatorValue::Doc(v) | FieldComparatorValue::Int(v) => Some(v),
            _ => None,
        }
    }

    fn into_i32(self) -> Option<i32> {
        match self {
            FieldComparatorValue::Doc(v) | FieldComparatorValue::Int(v) => Some(v),
            _ => None,
        }
    }

    fn as_i64(&self) -> Option<&i64> {
        match self {
            FieldComparatorValue::Long(v) => Some(v),
            _ => None,
        }
    }

    fn into_i64(self) -> Option<i64> {
        match self {
            FieldComparatorValue::Long(v) => Some(v),
            _ => None,
        }
    }

    fn as_f32(&self) -> Option<&f32> {
        match self {
            FieldComparatorValue::Float(v) => Some(v),
            _ => None,
        }
    }

    fn into_f32(self) -> Option<f32> {
        match self {
            FieldComparatorValue::Float(v) => Some(v),
            _ => None,
        }
    }

    fn as_f64(&self) -> Option<&f64> {
        match self {
            FieldComparatorValue::Double(v) => Some(v),
            _ => None,
        }
    }

    fn into_f64(self) -> Option<f64> {
        match self {
            FieldComparatorValue::Double(v) => Some(v),
            _ => None,
        }
    }

    fn as_term_val(&self) -> Option<&BytesRef<Vec<u8>>> {
        match self {
            FieldComparatorValue::TermVal(v) => Some(v),
            FieldComparatorValue::Missing => None,
            _ => None,
        }
    }

    fn into_term_val(self) -> Option<BytesRef<Vec<u8>>> {
        match self {
            FieldComparatorValue::TermVal(v) => Some(v),
            _ => None,
        }
    }
}

impl PartialOrd for FieldComparatorValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (FieldComparatorValue::Missing, FieldComparatorValue::Missing) => Some(Ordering::Equal),
            (FieldComparatorValue::Doc(a), FieldComparatorValue::Doc(b))
            | (FieldComparatorValue::Int(a), FieldComparatorValue::Int(b))
            | (FieldComparatorValue::Doc(a), FieldComparatorValue::Int(b))
            | (FieldComparatorValue::Int(a), FieldComparatorValue::Doc(b)) => a.partial_cmp(b),
            (FieldComparatorValue::Double(a), FieldComparatorValue::Double(b)) => a.partial_cmp(b),
            (FieldComparatorValue::Float(a), FieldComparatorValue::Float(b)) => a.partial_cmp(b),
            (FieldComparatorValue::Long(a), FieldComparatorValue::Long(b)) => a.partial_cmp(b),
            (FieldComparatorValue::TermVal(a), FieldComparatorValue::TermVal(b)) => Some(a.cmp(b)),
            _ => None,
        }
    }
}

pub enum FieldComparatorEnum {
    Doc(DocComparator),
    Double(DoubleComparator),
    Float(FloatComparator),
    Int(IntComparator),
    Long(LongComparator),
    TermVal(TermValComparator),
}

impl From<DocComparator> for FieldComparatorEnum {
    fn from(comparator: DocComparator) -> Self {
        FieldComparatorEnum::Doc(comparator)
    }
}

impl From<DoubleComparator> for FieldComparatorEnum {
    fn from(comparator: DoubleComparator) -> Self {
        FieldComparatorEnum::Double(comparator)
    }
}

impl From<FloatComparator> for FieldComparatorEnum {
    fn from(comparator: FloatComparator) -> Self {
        FieldComparatorEnum::Float(comparator)
    }
}

impl From<IntComparator> for FieldComparatorEnum {
    fn from(comparator: IntComparator) -> Self {
        FieldComparatorEnum::Int(comparator)
    }
}

impl From<LongComparator> for FieldComparatorEnum {
    fn from(comparator: LongComparator) -> Self {
        FieldComparatorEnum::Long(comparator)
    }
}

impl From<TermValComparator> for FieldComparatorEnum {
    fn from(comparator: TermValComparator) -> Self {
        FieldComparatorEnum::TermVal(comparator)
    }
}

impl FieldComparator for FieldComparatorEnum {
    type V = FieldComparatorValue;

    fn compare(&self, slot1: i32, slot2: i32) -> i32 {
        match self {
            FieldComparatorEnum::Doc(comparator) => comparator.compare(slot1, slot2),
            FieldComparatorEnum::Double(comparator) => comparator.compare(slot1, slot2),
            FieldComparatorEnum::Float(comparator) => comparator.compare(slot1, slot2),
            FieldComparatorEnum::Int(comparator) => comparator.compare(slot1, slot2),
            FieldComparatorEnum::Long(comparator) => comparator.compare(slot1, slot2),
            FieldComparatorEnum::TermVal(comparator) => comparator.compare(slot1, slot2),
        }
    }

    fn set_top_value(&mut self, value: Self::V) {
        match self {
            FieldComparatorEnum::Doc(comparator) => {
                let v = value.into_i32().expect("expected doc comparator value");
                comparator.set_top_value(v);
            },
            FieldComparatorEnum::Double(comparator) => {
                let v = value.into_f64().expect("expected double comparator value");
                comparator.set_top_value(v);
            },
            FieldComparatorEnum::Float(comparator) => {
                let v = value.into_f32().expect("expected float comparator value");
                comparator.set_top_value(v);
            },
            FieldComparatorEnum::Int(comparator) => {
                let v = value.into_i32().expect("expected int comparator value");
                comparator.set_top_value(v);
            },
            FieldComparatorEnum::Long(comparator) => {
                let v = value.into_i64().expect("expected long comparator value");
                comparator.set_top_value(v);
            },
            FieldComparatorEnum::TermVal(comparator) => {
                let v = value
                    .into_term_val()
                    .expect("expected term value comparator value");
                comparator.set_top_value(v);
            },
        }
    }

    fn value(&self, slot: i32) -> Self::V {
        match self {
            FieldComparatorEnum::Doc(comparator) => {
                FieldComparatorValue::Doc(comparator.value(slot))
            },
            FieldComparatorEnum::Double(comparator) => {
                FieldComparatorValue::Double(comparator.value(slot))
            },
            FieldComparatorEnum::Float(comparator) => {
                FieldComparatorValue::Float(comparator.value(slot))
            },
            FieldComparatorEnum::Int(comparator) => {
                FieldComparatorValue::Int(comparator.value(slot))
            },
            FieldComparatorEnum::Long(comparator) => {
                FieldComparatorValue::Long(comparator.value(slot))
            },
            FieldComparatorEnum::TermVal(comparator) => {
                FieldComparatorValue::TermVal(comparator.value(slot))
            },
        }
    }

    type LeafFieldComparator<LR>
        = LeafFieldComparatorEnum<LR>
    where
        LR: LeafReader;

    fn get_leaf_comparator<LR>(
        self,
        context: &LeafReaderContext<LR>,
    ) -> Result<Self::LeafFieldComparator<LR>>
    where
        LR: LeafReader,
    {
        match self {
            FieldComparatorEnum::Doc(comparator) => comparator
                .get_leaf_comparator(context)
                .map(LeafFieldComparatorEnum::Doc),
            FieldComparatorEnum::Double(comparator) => comparator
                .get_leaf_comparator(context)
                .map(LeafFieldComparatorEnum::Double),
            FieldComparatorEnum::Float(comparator) => comparator
                .get_leaf_comparator(context)
                .map(LeafFieldComparatorEnum::Float),
            FieldComparatorEnum::Int(comparator) => comparator
                .get_leaf_comparator(context)
                .map(LeafFieldComparatorEnum::Int),
            FieldComparatorEnum::Long(comparator) => comparator
                .get_leaf_comparator(context)
                .map(LeafFieldComparatorEnum::Long),
            FieldComparatorEnum::TermVal(comparator) => comparator
                .get_leaf_comparator(context)
                .map(LeafFieldComparatorEnum::TermVal),
        }
    }

    fn compare_values(&self, first: Option<&Self::V>, second: Option<&Self::V>) -> i32 {
        match self {
            FieldComparatorEnum::Doc(comparator) => comparator.compare_values(
                first.and_then(FieldComparatorValue::as_i32),
                second.and_then(FieldComparatorValue::as_i32),
            ),
            FieldComparatorEnum::Double(comparator) => comparator.compare_values(
                first.and_then(FieldComparatorValue::as_f64),
                second.and_then(FieldComparatorValue::as_f64),
            ),
            FieldComparatorEnum::Float(comparator) => comparator.compare_values(
                first.and_then(FieldComparatorValue::as_f32),
                second.and_then(FieldComparatorValue::as_f32),
            ),
            FieldComparatorEnum::Int(comparator) => comparator.compare_values(
                first.and_then(FieldComparatorValue::as_i32),
                second.and_then(FieldComparatorValue::as_i32),
            ),
            FieldComparatorEnum::Long(comparator) => comparator.compare_values(
                first.and_then(FieldComparatorValue::as_i64),
                second.and_then(FieldComparatorValue::as_i64),
            ),
            FieldComparatorEnum::TermVal(comparator) => comparator.compare_values(
                first.and_then(FieldComparatorValue::as_term_val),
                second.and_then(FieldComparatorValue::as_term_val),
            ),
        }
    }

    fn fallback_compare(&self, first: &Self::V, second: &Self::V) -> i32 {
        match self {
            FieldComparatorEnum::Double(comparator) => comparator.fallback_compare(
                first.as_f64().expect("expected double comparator value"),
                second.as_f64().expect("expected double comparator value"),
            ),
            FieldComparatorEnum::Float(comparator) => comparator.fallback_compare(
                first.as_f32().expect("expected float comparator value"),
                second.as_f32().expect("expected float comparator value"),
            ),
            _ => 0,
        }
    }

    fn set_single_sort(&mut self) {
        match self {
            FieldComparatorEnum::Doc(comparator) => comparator.set_single_sort(),
            FieldComparatorEnum::Double(comparator) => comparator.set_single_sort(),
            FieldComparatorEnum::Float(comparator) => comparator.set_single_sort(),
            FieldComparatorEnum::Int(comparator) => comparator.set_single_sort(),
            FieldComparatorEnum::Long(comparator) => comparator.set_single_sort(),
            FieldComparatorEnum::TermVal(comparator) => comparator.set_single_sort(),
        }
    }

    fn disable_skipping(&mut self) {
        match self {
            FieldComparatorEnum::Doc(comparator) => comparator.disable_skipping(),
            FieldComparatorEnum::Double(comparator) => comparator.disable_skipping(),
            FieldComparatorEnum::Float(comparator) => comparator.disable_skipping(),
            FieldComparatorEnum::Int(comparator) => comparator.disable_skipping(),
            FieldComparatorEnum::Long(comparator) => comparator.disable_skipping(),
            FieldComparatorEnum::TermVal(comparator) => comparator.disable_skipping(),
        }
    }
}
/// Sorts by field's natural Term sort order.
///
/// All comparisons are done using [`BytesRef::compareTo`],
/// which is slow for medium to large result sets but possibly
/// very fast for very small result sets.
pub struct TermValComparator {
    pub(crate) values: Vec<Option<BytesRef<Vec<u8>>>>,
    pub(crate) field: String,
    pub(crate) bottom: usize,
    pub(crate) top_value: Option<BytesRef<Vec<u8>>>,
    pub(crate) missing_sort_cmp: i32,
}

impl TermValComparator {
    pub fn new(num_hits: i32, field: String, sort_missing_last: bool) -> Self {
        Self {
            values: vec![None; num_hits as usize],
            field,
            bottom: 0,
            top_value: None,
            missing_sort_cmp: if sort_missing_last { 1 } else { -1 },
        }
    }

    fn compare_values(
        &self,
        val1: Option<&BytesRef<Vec<u8>>>,
        val2: Option<&BytesRef<Vec<u8>>>,
    ) -> i32 {
        match (val1, val2) {
            (None, None) => 0,
            (None, Some(_)) => self.missing_sort_cmp,
            (Some(_), None) => -self.missing_sort_cmp,
            (Some(v1), Some(v2)) => v1.cmp(v2).to_int(),
        }
    }
}

impl FieldComparator for TermValComparator {
    type V = BytesRef<Vec<u8>>;

    fn compare(&self, slot1: i32, slot2: i32) -> i32 {
        let val1 = self.values[slot1 as usize].as_ref();
        let val2 = self.values[slot2 as usize].as_ref();
        self.compare_values(val1, val2)
    }

    fn set_top_value(&mut self, value: Self::V) {
        self.top_value = Some(value);
    }

    fn value(&self, slot: i32) -> Self::V {
        // TODO: IMPORTANT - avoid clone here
        self.values[slot as usize]
            .as_ref()
            .expect("value in slot must be present")
            .clone()
    }

    type LeafFieldComparator<LR>
        = TermValLeafComparator<LR>
    where
        LR: LeafReader;

    fn get_leaf_comparator<LR>(
        self,
        context: &LeafReaderContext<LR>,
    ) -> Result<Self::LeafFieldComparator<LR>>
    where
        LR: LeafReader,
    {
        let doc_terms = DocValues::get_binary(context.reader(), &self.field)?;
        Ok(TermValLeafComparator::new(self, doc_terms))
    }

    fn compare_values(&self, first: Option<&Self::V>, second: Option<&Self::V>) -> i32 {
        match (first, second) {
            (Some(f), Some(s)) => f.cmp(s).to_int(),
            (None, Some(_)) => self.missing_sort_cmp,
            (Some(_), None) => -self.missing_sort_cmp,
            (None, None) => 0,
        }
    }
}
pub struct TermValLeafComparator<LR>
where
    LR: LeafReader,
{
    comparator: TermValComparator,
    doc_terms: Binary<LR>,
}

impl<LR> TermValLeafComparator<LR>
where
    LR: LeafReader,
{
    pub fn new(comparator: TermValComparator, doc_terms: Binary<LR>) -> Self {
        Self {
            comparator,
            doc_terms,
        }
    }

    fn get_value_for_doc(
        doc_terms: &mut Binary<LR>,
        doc: i32,
    ) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
        if doc_terms.advance_exact(doc)? {
            Ok(Some(doc_terms.binary_value()?))
        } else {
            Ok(None)
        }
    }
}

impl<LR> LeafFieldComparator for TermValLeafComparator<LR>
where
    LR: LeafReader,
{
    fn set_bottom(&mut self, slot: usize) -> Result<()> {
        self.comparator.bottom = slot;
        Ok(())
    }

    fn compare_bottom<S1, S2>(&mut self, doc: i32, _scorer: &mut ScorerEnum<S1, S2>) -> Result<i32>
    where
        S1: Scorer,
        S2: Scorable,
    {
        let (comparator, doc_terms) = (&self.comparator, &mut self.doc_terms);
        let val = Self::get_value_for_doc(doc_terms, doc)?;
        let bottom_value = match &comparator.values[comparator.bottom] {
            Some(v) => Some(v),
            None => None,
        };
        match val {
            Some(v) => Ok(comparator.compare_values(bottom_value, Some(v.as_ref()))),
            None => Ok(comparator.compare_values(bottom_value, None)),
        }
    }

    fn compare_top<S1, S2>(&mut self, doc: i32, _scorer: &mut ScorerEnum<S1, S2>) -> Result<i32>
    where
        S1: Scorer,
        S2: Scorable,
    {
        let (comparator, doc_terms) = (&self.comparator, &mut self.doc_terms);
        match Self::get_value_for_doc(doc_terms, doc)? {
            None => Ok(comparator.compare_values(comparator.top_value.as_ref(), None)),
            Some(val) => {
                Ok(comparator.compare_values(comparator.top_value.as_ref(), Some(val.as_ref())))
            },
        }
    }

    fn copy<S1, S2>(
        &mut self,
        slot: usize,
        doc: i32,
        _scorer: &mut ScorerEnum<S1, S2>,
    ) -> Result<()>
    where
        S1: Scorer,
        S2: Scorable,
    {
        match Self::get_value_for_doc(&mut self.doc_terms, doc)? {
            None => self.comparator.values[slot] = None,
            Some(val) => self.comparator.values[slot] = Some(val.into_owned()),
        }
        Ok(())
    }

    fn set_scorer<S1, S2>(&mut self, _scorer: &mut ScorerEnum<S1, S2>) -> Result<()>
    where
        S1: Scorer,
        S2: Scorable,
    {
        Ok(())
    }

    type DocIdSetIterator = DummyDocIdSetIterator;
}
