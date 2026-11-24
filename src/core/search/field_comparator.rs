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
use crate::core::document::lat_lon_point_distance_comparator::LatLonPointDistanceComparator;
use crate::core::document::xy_point_distance_comparator::XYPointDistanceComparator;
use crate::core::index::binary_doc_values::BinaryDocValues;
use crate::core::index::doc_values::{Binary, DocValues, SortedNumericDocValuesEnum3WithEmpty};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::{BytesRef, BytesRefValue};
use crate::core::search::comparators::doc_comparator::DocComparator;
use crate::core::search::comparators::double_comparator::{DoubleComparator, DoubleLeafComparator};
use crate::core::search::comparators::float_comparator::{FloatComparator, FloatLeafComparator};
use crate::core::search::comparators::int_comparator::IntComparator;
use crate::core::search::comparators::long_comparator::LongComparator;
use crate::core::search::comparators::numeric_comparator::{
  NumericLeafComparatorDocValues, prepare_numeric_doc_values,
};
use crate::core::search::comparators::term_ord_val_comparator::TermOrdValComparator;
use crate::core::search::dummy::dummy_disi::DummyDISI;
use crate::core::search::dummy::dummy_field_comparator::DummyFieldComparator;
use crate::core::search::field_doc::FieldDoc;
use crate::core::search::leaf_field_comparator::{LeafFieldComparator, LeafFieldComparatorEnum};
use crate::core::search::scorable::Scorable;
use crate::core::search::sorted_numeric_selector::{
  FilterNumericDocValuesImpl1, FilterNumericDocValuesImpl2, MaxValue, MinValue,
  SelectedNumericDocValues, SortedNumericSelectorWrap,
};
use crate::core::search::sorted_numeric_sort_field::{
  SortedNumericDoubleComparator, SortedNumericFloatComparator, SortedNumericIntComparator,
  SortedNumericLongComparator,
};
use crate::core::search::sorted_set_sort_field::SortedDocValuesTermOrdValComparator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{CoreHelper, ToInt};
use crate::impl_from_for_enum;
use std::cmp::Ordering;
use std::sync::Arc;

/// Immutable paging term, either owned or retained by a shared `search_after` document.
pub(crate) enum TermTopValue {
  Owned(BytesRef<Vec<u8>>),
  After {
    doc: Arc<FieldDoc>,
    field_index: usize,
  },
}

impl TermTopValue {
  pub(crate) fn as_ref(&self) -> Result<&BytesRef<Vec<u8>>> {
    match self {
      Self::Owned(value) => Ok(value),
      Self::After { doc, field_index } => doc
        .fields
        .get(*field_index)
        .and_then(FieldComparatorValue::as_term_val)
        .ok_or_else(|| LuceneError::illegal_state("expected term value comparator value")),
    }
  }
}

/// Expert: a [`FieldComparator`] compares hits so as to determine their sort order when collecting the
/// top results with [`TopFieldCollector`](crate::core::search::top_field_collector::TopFieldCollector).
/// The concrete public [`FieldComparator`] implementations
/// correspond to the [`SortField`](crate::core::search::sort_field::SortField) types.
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
/// - `get_leaf_comparator` Invoked when the search is switching to the next segment. You may need
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
  fn compare(&self, slot1: usize, slot2: usize) -> i32;

  /// Record the top value, for future calls to [`LeafFieldComparator::compare_top`].
  /// This is only called for searches that use `search_after` (deep paging),
  /// and is invoked before any calls to [`Self::get_leaf_comparator`].
  fn set_top_value(&mut self, value: Self::V) -> Result<()>;

  /// Return the actual value in the slot.
  ///
  /// # Parameters
  /// - `slot`: the slot index
  ///
  /// # Returns
  /// The value stored in this slot if it exists, otherwise [`None`].
  fn value(&self, slot: usize) -> Option<Self::V>;

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
    &mut self,
    context: &LeafReaderContext<LR>,
  ) -> Result<Self::LeafFieldComparator<LR>>
  where
    LR: LeafReader;

  /// Returns a negative integer if `first` is less than `second`, `0` if they are equal,
  /// and a positive integer otherwise.
  ///
  /// The default implementation requires [`Ord`] and invokes [`Ord::cmp`].
  ///
  /// Provide this method if the [`FieldComparator`] value type does not implement ordering.
  /// or if values may sometimes be absent ([`Option::None`]).
  fn compare_values(&self, first: Option<&Self::V>, second: Option<&Self::V>) -> Result<i32> {
    match (first, second) {
      (None, None) => Ok(0),
      (None, Some(_)) => Ok(-1),
      (Some(_), None) => Ok(1),
      (Some(f), Some(s)) => {
        match f.partial_cmp(s) {
          Some(ord) => Ok(ord.to_int()),
          // In case of NaN for f64 or other non-comparable values
          None => self.fallback_compare(f, s),
        }
      },
    }
  }
  fn fallback_compare(&self, _first: &Self::V, _second: &Self::V) -> Result<i32> {
    Err(LuceneError::unsupported_operation(
      "fallback_compare must be implemented if the value type is not fully comparable",
    ))
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
/// `TopScoreDocCollector` directly (which [`IndexSearcher::search`](crate::core::search::index_searcher::IndexSearcher) uses
/// when no [`Sort`](crate::core::search::sort::Sort) is specified).
pub struct RelevanceComparator {
  pub(crate) scores: Vec<f32>,
  pub(crate) bottom: f32,
  pub(crate) top_value: f32,
}
impl RelevanceComparator {
  pub fn new(num_hits: usize) -> Self {
    Self {
      scores: vec![0.0; num_hits],
      bottom: 0.0,
      top_value: 0.0,
    }
  }
}
impl FieldComparator for RelevanceComparator {
  type V = f32;

  fn compare(&self, slot1: usize, slot2: usize) -> i32 {
    let slot1_v = self.scores[slot2];
    let slot2_v = self.scores[slot1];
    CoreHelper::compare_f32(slot1_v, slot2_v).to_int()
  }

  fn set_top_value(&mut self, value: Self::V) -> Result<()> {
    self.top_value = value;
    Ok(())
  }

  fn value(&self, slot: usize) -> Option<Self::V> {
    Some(self.scores[slot])
  }

  type LeafFieldComparator<LR>
    = RelevanceLeafComparator
  where
    LR: LeafReader;

  fn get_leaf_comparator<LR>(
    &mut self,
    _context: &LeafReaderContext<LR>,
  ) -> Result<Self::LeafFieldComparator<LR>>
  where
    LR: LeafReader,
  {
    Ok(RelevanceLeafComparator::new())
  }

  fn compare_values(&self, first: Option<&Self::V>, second: Option<&Self::V>) -> Result<i32> {
    match (first, second) {
      // Reversed intentionally because relevance by default sorts descending.
      (Some(&f), Some(&s)) => Ok(CoreHelper::compare_f32(s, f).to_int()),
      (None, Some(_)) => Ok(1),
      (Some(_), None) => Ok(-1),
      (None, None) => Ok(0),
    }
  }

  fn fallback_compare(&self, first: &Self::V, second: &Self::V) -> Result<i32> {
    Ok(if first.is_nan() && second.is_nan() {
      0
    } else if first.is_nan() {
      1
    } else if second.is_nan() {
      -1
    } else {
      0
    })
  }
}
pub struct RelevanceLeafComparator;
impl Default for RelevanceLeafComparator {
  fn default() -> Self {
    Self::new()
  }
}

impl RelevanceLeafComparator {
  pub fn new() -> Self {
    Self
  }
}
impl LeafFieldComparator for RelevanceLeafComparator {
  type FieldComparator = RelevanceComparator;
  fn set_bottom(&mut self, slot: usize, comparator: &mut Self::FieldComparator) -> Result<()> {
    comparator.bottom = comparator.scores[slot];
    Ok(())
  }

  fn compare_bottom<S>(
    &mut self,
    _doc: i32,
    scorer: &mut S,
    comparator: &mut Self::FieldComparator,
  ) -> Result<i32>
  where
    S: Scorable + ?Sized,
  {
    let doc_value = scorer.score()?;
    debug_assert!(!doc_value.is_nan());
    Ok(CoreHelper::compare_f32(doc_value, comparator.bottom).to_int())
  }

  fn compare_top<S>(
    &mut self,
    _doc: i32,
    scorer: &mut S,
    comparator: &mut Self::FieldComparator,
  ) -> Result<i32>
  where
    S: Scorable + ?Sized,
  {
    let doc_value = scorer.score()?;
    debug_assert!(!doc_value.is_nan());
    Ok(CoreHelper::compare_f32(doc_value, comparator.top_value).to_int())
  }

  fn copy<S>(
    &mut self,
    slot: usize,
    _doc: i32,
    scorer: &mut S,
    comparator: &mut Self::FieldComparator,
  ) -> Result<()>
  where
    S: Scorable + ?Sized,
  {
    let score = scorer.score()?;
    comparator.scores[slot] = score;
    debug_assert!(!score.is_nan());
    Ok(())
  }

  fn set_scorer<S>(
    &mut self,
    _scorer: &mut S,
    _comparator: &mut Self::FieldComparator,
  ) -> Result<()>
  where
    S: Scorable + ?Sized,
  {
    Ok(())
  }

  type DocIdSetIteratorRef<'a> = &'a mut DummyDISI;
}

#[derive(Debug, Clone, Default)]
pub enum FieldComparatorValue {
  #[default]
  Missing,
  Double(f64),
  Float(f32),
  Int(i32),
  Long(i64),
  TermVal(BytesRef<Vec<u8>>),
}

impl PartialEq for FieldComparatorValue {
  fn eq(&self, other: &Self) -> bool {
    match (self, other) {
      (Self::Missing, Self::Missing) => true,
      (Self::Double(a), Self::Double(b)) => CoreHelper::compare_f64(*a, *b).is_eq(),
      (Self::Float(a), Self::Float(b)) => CoreHelper::compare_f32(*a, *b).is_eq(),
      (Self::Int(a), Self::Int(b)) => a == b,
      (Self::Long(a), Self::Long(b)) => a == b,
      (Self::TermVal(a), Self::TermVal(b)) => a == b,
      _ => false,
    }
  }
}
impl_from_for_enum!(
    FieldComparatorValue,
    i32 => Int,
    i64 => Long,
    f32 => Float,
    f64 => Double,
    BytesRef<Vec<u8>> => TermVal,
);
impl FieldComparatorValue {
  pub fn missing() -> Self {
    FieldComparatorValue::Missing
  }

  pub fn as_i32(&self) -> Option<&i32> {
    match self {
      FieldComparatorValue::Int(v) => Some(v),
      _ => None,
    }
  }

  pub fn into_i32(self) -> Option<i32> {
    match self {
      FieldComparatorValue::Int(v) => Some(v),
      _ => None,
    }
  }

  pub fn as_i64(&self) -> Option<&i64> {
    match self {
      FieldComparatorValue::Long(v) => Some(v),
      _ => None,
    }
  }

  pub fn into_i64(self) -> Option<i64> {
    match self {
      FieldComparatorValue::Long(v) => Some(v),
      _ => None,
    }
  }

  pub fn as_f32(&self) -> Option<&f32> {
    match self {
      FieldComparatorValue::Float(v) => Some(v),
      _ => None,
    }
  }

  pub fn into_f32(self) -> Option<f32> {
    match self {
      FieldComparatorValue::Float(v) => Some(v),
      _ => None,
    }
  }

  pub fn as_f64(&self) -> Option<&f64> {
    match self {
      FieldComparatorValue::Double(v) => Some(v),
      _ => None,
    }
  }

  pub fn into_f64(self) -> Option<f64> {
    match self {
      FieldComparatorValue::Double(v) => Some(v),
      _ => None,
    }
  }

  pub fn as_term_val(&self) -> Option<&BytesRef<Vec<u8>>> {
    match self {
      FieldComparatorValue::TermVal(v) => Some(v),
      FieldComparatorValue::Missing => None,
      _ => None,
    }
  }

  pub fn into_term_val(self) -> Option<BytesRef<Vec<u8>>> {
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
      (FieldComparatorValue::Int(a), FieldComparatorValue::Int(b)) => a.partial_cmp(b),
      (FieldComparatorValue::Double(a), FieldComparatorValue::Double(b)) => {
        Some(CoreHelper::compare_f64(*a, *b))
      },
      (FieldComparatorValue::Float(a), FieldComparatorValue::Float(b)) => {
        Some(CoreHelper::compare_f32(*a, *b))
      },
      (FieldComparatorValue::Long(a), FieldComparatorValue::Long(b)) => a.partial_cmp(b),
      (FieldComparatorValue::TermVal(a), FieldComparatorValue::TermVal(b)) => Some(a.cmp(b)),
      _ => None,
    }
  }
}

pub enum FieldComparatorEnum {
  Relevance(RelevanceComparator),
  Doc(DocComparator),
  Double(DoubleComparator),
  Float(FloatComparator),
  Int(IntComparator),
  LatLonPointDistance(LatLonPointDistanceComparator),
  Long(LongComparator),
  TermVal(TermValComparator),
  TermOrdValue(TermOrdValComparator),
  SortedNumericInt(SortedNumericIntComparator),
  SortedNumericLong(SortedNumericLongComparator),
  SortedNumericFloat(SortedNumericFloatComparator),
  SortedNumericDouble(SortedNumericDoubleComparator),
  SortedDocValuesTermOrdVal(SortedDocValuesTermOrdValComparator),
  XYPointDistance(XYPointDistanceComparator),
  Dummy(DummyFieldComparator),
}
// for std::mem::take
impl Default for FieldComparatorEnum {
  fn default() -> Self {
    FieldComparatorEnum::Dummy(DummyFieldComparator)
  }
}
impl_from_for_enum!(
    FieldComparatorEnum,
    RelevanceComparator => Relevance,
    DocComparator => Doc,
    DoubleComparator => Double,
    FloatComparator => Float,
    IntComparator => Int,
    LongComparator => Long,
    TermValComparator => TermVal,
    TermOrdValComparator => TermOrdValue,
    SortedNumericIntComparator => SortedNumericInt,
    SortedNumericLongComparator => SortedNumericLong,
    SortedNumericFloatComparator => SortedNumericFloat,
    SortedNumericDoubleComparator => SortedNumericDouble,
    SortedDocValuesTermOrdValComparator => SortedDocValuesTermOrdVal,
    LatLonPointDistanceComparator => LatLonPointDistance,
    XYPointDistanceComparator => XYPointDistance,
    DummyFieldComparator => Dummy,
);

impl FieldComparatorEnum {
  pub(crate) fn set_top_value_from_after(
    &mut self,
    after: &Arc<FieldDoc>,
    field_index: usize,
  ) -> Result<()> {
    let value = after
      .fields
      .get(field_index)
      .ok_or_else(|| LuceneError::illegal_state("missing search_after comparator value"))?;
    match self {
      Self::TermVal(comparator) => {
        value
          .as_term_val()
          .ok_or_else(|| LuceneError::illegal_state("expected term value comparator value"))?;
        comparator.top_value = Some(TermTopValue::After {
          doc: Arc::clone(after),
          field_index,
        });
        Ok(())
      },
      Self::TermOrdValue(comparator) => {
        value
          .as_term_val()
          .ok_or_else(|| LuceneError::illegal_state("expected term ord value comparator value"))?;
        comparator.top_value = Some(TermTopValue::After {
          doc: Arc::clone(after),
          field_index,
        });
        Ok(())
      },
      Self::SortedDocValuesTermOrdVal(comparator) => {
        value.as_term_val().ok_or_else(|| {
          LuceneError::illegal_state("expected sorted doc values term ord val comparator value")
        })?;
        comparator.base.top_value = Some(TermTopValue::After {
          doc: Arc::clone(after),
          field_index,
        });
        Ok(())
      },
      _ => self.set_top_value(value.clone()),
    }
  }
}

impl FieldComparator for FieldComparatorEnum {
  type V = FieldComparatorValue;

  fn compare(&self, slot1: usize, slot2: usize) -> i32 {
    match self {
      FieldComparatorEnum::Relevance(comparator) => comparator.compare(slot1, slot2),
      FieldComparatorEnum::Doc(comparator) => comparator.compare(slot1, slot2),
      FieldComparatorEnum::Double(comparator) => comparator.compare(slot1, slot2),
      FieldComparatorEnum::Float(comparator) => comparator.compare(slot1, slot2),
      FieldComparatorEnum::Int(comparator) => comparator.compare(slot1, slot2),
      FieldComparatorEnum::Long(comparator) => comparator.compare(slot1, slot2),
      FieldComparatorEnum::TermVal(comparator) => comparator.compare(slot1, slot2),
      FieldComparatorEnum::TermOrdValue(comparator) => comparator.compare(slot1, slot2),
      FieldComparatorEnum::SortedNumericInt(comparator) => comparator.compare(slot1, slot2),
      FieldComparatorEnum::SortedNumericLong(comparator) => comparator.compare(slot1, slot2),
      FieldComparatorEnum::SortedNumericFloat(comparator) => comparator.compare(slot1, slot2),
      FieldComparatorEnum::SortedNumericDouble(comparator) => comparator.compare(slot1, slot2),
      FieldComparatorEnum::SortedDocValuesTermOrdVal(comparator) => {
        comparator.compare(slot1, slot2)
      },
      FieldComparatorEnum::LatLonPointDistance(comparator) => comparator.compare(slot1, slot2),
      FieldComparatorEnum::XYPointDistance(comparator) => comparator.compare(slot1, slot2),
      FieldComparatorEnum::Dummy(comparator) => comparator.compare(slot1, slot2),
    }
  }

  fn set_top_value(&mut self, value: Self::V) -> Result<()> {
    match self {
      FieldComparatorEnum::Relevance(comparator) => {
        let v = value
          .into_f32()
          .ok_or_else(|| LuceneError::illegal_state("expected relevance comparator value"))?;
        comparator.set_top_value(v)?;
      },
      FieldComparatorEnum::Doc(comparator) => {
        let v = value
          .into_i32()
          .ok_or_else(|| LuceneError::illegal_state("expected doc comparator value"))?;
        comparator.set_top_value(v)?;
      },
      FieldComparatorEnum::Double(comparator) => {
        let v = value
          .into_f64()
          .ok_or_else(|| LuceneError::illegal_state("expected double comparator value"))?;
        comparator.set_top_value(v)?;
      },
      FieldComparatorEnum::Float(comparator) => {
        let v = value
          .into_f32()
          .ok_or_else(|| LuceneError::illegal_state("expected float comparator value"))?;
        comparator.set_top_value(v)?;
      },
      FieldComparatorEnum::Int(comparator) => {
        let v = value
          .into_i32()
          .ok_or_else(|| LuceneError::illegal_state("expected int comparator value"))?;
        comparator.set_top_value(v)?;
      },
      FieldComparatorEnum::Long(comparator) => {
        let v = value
          .into_i64()
          .ok_or_else(|| LuceneError::illegal_state("expected long comparator value"))?;
        comparator.set_top_value(v)?;
      },
      FieldComparatorEnum::TermVal(comparator) => {
        let v = value
          .into_term_val()
          .ok_or_else(|| LuceneError::illegal_state("expected term value comparator value"))?;
        comparator.set_top_value(v)?;
      },
      FieldComparatorEnum::TermOrdValue(comparator) => {
        let v = value
          .into_term_val()
          .ok_or_else(|| LuceneError::illegal_state("expected term ord value comparator value"))?;
        comparator.set_top_value(v)?;
      },
      FieldComparatorEnum::SortedNumericInt(comparator) => {
        let v = value.into_i32().ok_or_else(|| {
          LuceneError::illegal_state("expected sorted numeric int comparator value")
        })?;
        comparator.set_top_value(v)?;
      },
      FieldComparatorEnum::SortedNumericLong(comparator) => {
        let v = value.into_i64().ok_or_else(|| {
          LuceneError::illegal_state("expected sorted numeric long comparator value")
        })?;
        comparator.set_top_value(v)?;
      },
      FieldComparatorEnum::SortedNumericFloat(comparator) => {
        let v = value.into_f32().ok_or_else(|| {
          LuceneError::illegal_state("expected sorted numeric float comparator value")
        })?;
        comparator.set_top_value(v)?;
      },
      FieldComparatorEnum::SortedNumericDouble(comparator) => {
        let v = value.into_f64().ok_or_else(|| {
          LuceneError::illegal_state("expected sorted numeric double comparator value")
        })?;
        comparator.set_top_value(v)?;
      },
      FieldComparatorEnum::SortedDocValuesTermOrdVal(comparator) => {
        let v = value.into_term_val().ok_or_else(|| {
          LuceneError::illegal_state("expected sorted doc values term ord val comparator value")
        })?;
        comparator.set_top_value(v)?;
      },
      FieldComparatorEnum::LatLonPointDistance(comparator) => {
        let v = value.into_f64().ok_or_else(|| {
          LuceneError::illegal_state("expected lat lon point distance comparator value")
        })?;
        comparator.set_top_value(v)?;
      },
      FieldComparatorEnum::XYPointDistance(comparator) => {
        let v = value.into_f64().ok_or_else(|| {
          LuceneError::illegal_state("expected xy point distance comparator value")
        })?;
        comparator.set_top_value(v)?;
      },
      FieldComparatorEnum::Dummy(_comparator) => {
        dummy_unreachable!()
      },
    }
    Ok(())
  }

  fn value(&self, slot: usize) -> Option<Self::V> {
    match self {
      FieldComparatorEnum::Relevance(comparator) => {
        comparator.value(slot).map(FieldComparatorValue::Float)
      },
      FieldComparatorEnum::Doc(comparator) => comparator.value(slot).map(FieldComparatorValue::Int),
      FieldComparatorEnum::Double(comparator) => {
        comparator.value(slot).map(FieldComparatorValue::Double)
      },
      FieldComparatorEnum::Float(comparator) => {
        comparator.value(slot).map(FieldComparatorValue::Float)
      },
      FieldComparatorEnum::Int(comparator) => comparator.value(slot).map(FieldComparatorValue::Int),
      FieldComparatorEnum::Long(comparator) => {
        comparator.value(slot).map(FieldComparatorValue::Long)
      },
      FieldComparatorEnum::TermVal(comparator) => {
        comparator.value(slot).map(FieldComparatorValue::TermVal)
      },
      FieldComparatorEnum::TermOrdValue(comparator) => {
        comparator.value(slot).map(FieldComparatorValue::TermVal)
      },
      FieldComparatorEnum::SortedNumericInt(comparator) => {
        comparator.value(slot).map(FieldComparatorValue::Int)
      },
      FieldComparatorEnum::SortedNumericLong(comparator) => {
        comparator.value(slot).map(FieldComparatorValue::Long)
      },
      FieldComparatorEnum::SortedNumericFloat(comparator) => {
        comparator.value(slot).map(FieldComparatorValue::Float)
      },
      FieldComparatorEnum::SortedNumericDouble(comparator) => {
        comparator.value(slot).map(FieldComparatorValue::Double)
      },
      FieldComparatorEnum::SortedDocValuesTermOrdVal(comparator) => {
        comparator.value(slot).map(FieldComparatorValue::TermVal)
      },
      FieldComparatorEnum::LatLonPointDistance(comparator) => {
        comparator.value(slot).map(FieldComparatorValue::Double)
      },
      FieldComparatorEnum::XYPointDistance(comparator) => {
        comparator.value(slot).map(FieldComparatorValue::Double)
      },
      FieldComparatorEnum::Dummy(_comparator) => {
        dummy_unreachable!()
      },
    }
  }

  type LeafFieldComparator<LR>
    = LeafFieldComparatorEnum<LR>
  where
    LR: LeafReader;

  fn get_leaf_comparator<LR>(
    &mut self,
    context: &LeafReaderContext<LR>,
  ) -> Result<Self::LeafFieldComparator<LR>>
  where
    LR: LeafReader,
  {
    // Select the cursor once before the collector starts reading documents.
    macro_rules! floating_leaf {
      ($parent:ident, $leaf:ident, $wrapper:ident, $decoder:ident,
       $single:ident, $min:ident, $max:ident) => {{
        let base = &mut $parent.base;
        let (values, candidate, context_reader) =
          prepare_numeric_doc_values!(base.base, context, None, None);
        macro_rules! selected {
          ($values:expr, $kind:ident) => {
            $leaf::with_doc_values(
              base,
              context,
              $decoder::new($values),
              candidate,
              context_reader,
            )
            .map(LeafFieldComparatorEnum::$kind)
          };
        }
        let values = match values {
          NumericLeafComparatorDocValues::A(SortedNumericSelectorWrap::$wrapper(view)) => {
            match view.inner {
              SelectedNumericDocValues::Single(values) => return selected!(values, $single),
              SelectedNumericDocValues::Min(MinValue {
                inner: SortedNumericDocValuesEnum3WithEmpty::A(values),
                ..
              }) => return selected!(MinValue::new(values), $min),
              SelectedNumericDocValues::Max(MaxValue {
                inner: SortedNumericDocValuesEnum3WithEmpty::A(values),
                ..
              }) => return selected!(MaxValue::new(values), $max),
              view => NumericLeafComparatorDocValues::A(SortedNumericSelectorWrap::$wrapper(
                $decoder::new(view),
              )),
            }
          },
          values => values,
        };
        $leaf::with_doc_values(base, context, values, candidate, context_reader)
          .map(LeafFieldComparatorEnum::$wrapper)
      }};
    }
    match self {
      FieldComparatorEnum::Relevance(comparator) => comparator
        .get_leaf_comparator(context)
        .map(LeafFieldComparatorEnum::Relevance),
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
      FieldComparatorEnum::TermOrdValue(comparator) => comparator
        .get_leaf_comparator(context)
        .map(LeafFieldComparatorEnum::TermOrdSorted),
      FieldComparatorEnum::SortedNumericInt(comparator) => comparator.get_leaf_comparator(context),
      FieldComparatorEnum::SortedNumericLong(comparator) => comparator.get_leaf_comparator(context),
      FieldComparatorEnum::SortedNumericFloat(comparator) => floating_leaf!(
        comparator,
        FloatLeafComparator,
        Float,
        FilterNumericDocValuesImpl1,
        FloatSelectedSingle,
        FloatSelectedMin,
        FloatSelectedMax
      ),
      FieldComparatorEnum::SortedNumericDouble(comparator) => floating_leaf!(
        comparator,
        DoubleLeafComparator,
        Double,
        FilterNumericDocValuesImpl2,
        DoubleSelectedSingle,
        DoubleSelectedMin,
        DoubleSelectedMax
      ),
      FieldComparatorEnum::SortedDocValuesTermOrdVal(comparator) => {
        comparator.get_leaf_comparator(context)
      },
      FieldComparatorEnum::LatLonPointDistance(comparator) => comparator
        .get_leaf_comparator(context)
        .map(LeafFieldComparatorEnum::LatLonPointDistance),
      FieldComparatorEnum::XYPointDistance(comparator) => comparator
        .get_leaf_comparator(context)
        .map(LeafFieldComparatorEnum::XYPointDistance),
      FieldComparatorEnum::Dummy(_) => {
        dummy_unreachable!()
      },
    }
  }

  fn compare_values(&self, first: Option<&Self::V>, second: Option<&Self::V>) -> Result<i32> {
    match self {
      FieldComparatorEnum::Relevance(comparator) => comparator.compare_values(
        first.and_then(FieldComparatorValue::as_f32),
        second.and_then(FieldComparatorValue::as_f32),
      ),
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
      FieldComparatorEnum::TermVal(comparator) => {
        <TermValComparator as FieldComparator>::compare_values(
          comparator,
          first.and_then(FieldComparatorValue::as_term_val),
          second.and_then(FieldComparatorValue::as_term_val),
        )
      },
      FieldComparatorEnum::TermOrdValue(comparator) => comparator.compare_values(
        first.and_then(FieldComparatorValue::as_term_val),
        second.and_then(FieldComparatorValue::as_term_val),
      ),
      FieldComparatorEnum::SortedNumericInt(comparator) => comparator.compare_values(
        first.and_then(FieldComparatorValue::as_i32),
        second.and_then(FieldComparatorValue::as_i32),
      ),
      FieldComparatorEnum::SortedNumericLong(comparator) => comparator.compare_values(
        first.and_then(FieldComparatorValue::as_i64),
        second.and_then(FieldComparatorValue::as_i64),
      ),
      FieldComparatorEnum::SortedNumericFloat(comparator) => comparator.compare_values(
        first.and_then(FieldComparatorValue::as_f32),
        second.and_then(FieldComparatorValue::as_f32),
      ),
      FieldComparatorEnum::SortedNumericDouble(comparator) => comparator.compare_values(
        first.and_then(FieldComparatorValue::as_f64),
        second.and_then(FieldComparatorValue::as_f64),
      ),
      FieldComparatorEnum::SortedDocValuesTermOrdVal(comparator) => comparator.compare_values(
        first.and_then(FieldComparatorValue::as_term_val),
        second.and_then(FieldComparatorValue::as_term_val),
      ),
      FieldComparatorEnum::LatLonPointDistance(comparator) => comparator.compare_values(
        first.and_then(FieldComparatorValue::as_f64),
        second.and_then(FieldComparatorValue::as_f64),
      ),
      FieldComparatorEnum::XYPointDistance(comparator) => comparator.compare_values(
        first.and_then(FieldComparatorValue::as_f64),
        second.and_then(FieldComparatorValue::as_f64),
      ),
      FieldComparatorEnum::Dummy(_) => {
        dummy_unreachable!()
      },
    }
  }

  fn fallback_compare(&self, first: &Self::V, second: &Self::V) -> Result<i32> {
    match self {
      FieldComparatorEnum::Double(comparator) => match (first.as_f64(), second.as_f64()) {
        (Some(first), Some(second)) => comparator.fallback_compare(first, second),
        _ => Err(LuceneError::illegal_state(
          "double fallback comparison received non-double values",
        )),
      },
      FieldComparatorEnum::Float(comparator) => match (first.as_f32(), second.as_f32()) {
        (Some(first), Some(second)) => comparator.fallback_compare(first, second),
        _ => Err(LuceneError::illegal_state(
          "float fallback comparison received non-float values",
        )),
      },
      FieldComparatorEnum::SortedNumericDouble(comparator) => {
        match (first.as_f64(), second.as_f64()) {
          (Some(first), Some(second)) => comparator.fallback_compare(first, second),
          _ => Err(LuceneError::illegal_state(
            "sorted numeric double fallback comparison received non-double values",
          )),
        }
      },
      FieldComparatorEnum::SortedNumericFloat(comparator) => {
        match (first.as_f32(), second.as_f32()) {
          (Some(first), Some(second)) => comparator.fallback_compare(first, second),
          _ => Err(LuceneError::illegal_state(
            "sorted numeric float fallback comparison received non-float values",
          )),
        }
      },
      FieldComparatorEnum::Dummy(_) => {
        dummy_unreachable!()
      },
      _ => Err(LuceneError::unsupported_operation(
        "fallback comparison is not supported for this field comparator",
      )),
    }
  }

  fn set_single_sort(&mut self) {
    match self {
      FieldComparatorEnum::Relevance(comparator) => comparator.set_single_sort(),
      FieldComparatorEnum::Doc(comparator) => comparator.set_single_sort(),
      FieldComparatorEnum::Double(comparator) => comparator.set_single_sort(),
      FieldComparatorEnum::Float(comparator) => comparator.set_single_sort(),
      FieldComparatorEnum::Int(comparator) => comparator.set_single_sort(),
      FieldComparatorEnum::Long(comparator) => comparator.set_single_sort(),
      FieldComparatorEnum::TermVal(comparator) => comparator.set_single_sort(),
      FieldComparatorEnum::TermOrdValue(comparator) => comparator.set_single_sort(),
      FieldComparatorEnum::SortedNumericInt(comparator) => comparator.set_single_sort(),
      FieldComparatorEnum::SortedNumericLong(comparator) => comparator.set_single_sort(),
      FieldComparatorEnum::SortedNumericFloat(comparator) => comparator.set_single_sort(),
      FieldComparatorEnum::SortedNumericDouble(comparator) => comparator.set_single_sort(),
      FieldComparatorEnum::SortedDocValuesTermOrdVal(comparator) => comparator.set_single_sort(),
      FieldComparatorEnum::LatLonPointDistance(comparator) => comparator.set_single_sort(),
      FieldComparatorEnum::XYPointDistance(comparator) => comparator.set_single_sort(),
      FieldComparatorEnum::Dummy(comparator) => comparator.set_single_sort(),
    }
  }

  fn disable_skipping(&mut self) {
    match self {
      FieldComparatorEnum::Relevance(comparator) => comparator.disable_skipping(),
      FieldComparatorEnum::Doc(comparator) => comparator.disable_skipping(),
      FieldComparatorEnum::Double(comparator) => comparator.disable_skipping(),
      FieldComparatorEnum::Float(comparator) => comparator.disable_skipping(),
      FieldComparatorEnum::Int(comparator) => comparator.disable_skipping(),
      FieldComparatorEnum::Long(comparator) => comparator.disable_skipping(),
      FieldComparatorEnum::TermVal(comparator) => comparator.disable_skipping(),
      FieldComparatorEnum::TermOrdValue(comparator) => comparator.disable_skipping(),
      FieldComparatorEnum::SortedNumericInt(comparator) => comparator.disable_skipping(),
      FieldComparatorEnum::SortedNumericLong(comparator) => comparator.disable_skipping(),
      FieldComparatorEnum::SortedNumericFloat(comparator) => comparator.disable_skipping(),
      FieldComparatorEnum::SortedNumericDouble(comparator) => comparator.disable_skipping(),
      FieldComparatorEnum::SortedDocValuesTermOrdVal(comparator) => comparator.disable_skipping(),
      FieldComparatorEnum::LatLonPointDistance(comparator) => comparator.disable_skipping(),
      FieldComparatorEnum::XYPointDistance(comparator) => comparator.disable_skipping(),
      FieldComparatorEnum::Dummy(comparator) => comparator.disable_skipping(),
    }
  }
}
/// Sorts by field's natural Term sort order.
///
/// All comparisons are done using [`BytesRef`],
/// which is slow for medium to large result sets but possibly
/// very fast for very small result sets.
pub struct TermValComparator {
  pub(crate) values: Vec<Option<BytesRef<Vec<u8>>>>,
  pub(crate) field: String,
  pub(crate) bottom: usize,
  pub(crate) top_value: Option<TermTopValue>,
  pub(crate) missing_sort_cmp: i32,
}

impl TermValComparator {
  pub fn new<FName>(field: FName, num_hits: usize, sort_missing_last: bool) -> Self
  where
    FName: Into<String>,
  {
    let field = field.into();
    Self {
      values: vec![None; num_hits],
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

  fn compare_value_bytes(&self, val1: Option<&[u8]>, val2: Option<&[u8]>) -> i32 {
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

  fn compare(&self, slot1: usize, slot2: usize) -> i32 {
    let val1 = self.values[slot1].as_ref();
    let val2 = self.values[slot2].as_ref();
    self.compare_values(val1, val2)
  }

  fn set_top_value(&mut self, value: Self::V) -> Result<()> {
    self.top_value = Some(TermTopValue::Owned(value));
    Ok(())
  }

  fn value(&self, slot: usize) -> Option<Self::V> {
    self.values[slot].clone()
  }

  type LeafFieldComparator<LR>
    = TermValLeafComparator<Binary<LR>>
  where
    LR: LeafReader;

  fn get_leaf_comparator<LR>(
    &mut self,
    context: &LeafReaderContext<LR>,
  ) -> Result<Self::LeafFieldComparator<LR>>
  where
    LR: LeafReader,
  {
    let doc_terms = DocValues::get_binary(context.reader(), &self.field)?;
    Ok(TermValLeafComparator::new(doc_terms))
  }

  fn compare_values(&self, first: Option<&Self::V>, second: Option<&Self::V>) -> Result<i32> {
    Ok(match (first, second) {
      (Some(f), Some(s)) => f.cmp(s).to_int(),
      (None, Some(_)) => self.missing_sort_cmp,
      (Some(_), None) => -self.missing_sort_cmp,
      (None, None) => 0,
    })
  }
}
pub struct TermValLeafComparator<B> {
  doc_terms: B,
}

impl<B> TermValLeafComparator<B> {
  pub fn new(doc_terms: B) -> Self {
    Self { doc_terms }
  }
}

impl<B> TermValLeafComparator<B>
where
  B: BinaryDocValues,
{
  fn get_value_for_doc(doc_terms: &mut B, doc: i32) -> Result<Option<B::Value<'_>>> {
    if doc_terms.advance_exact(doc)? {
      Ok(Some(doc_terms.binary_value()?))
    } else {
      Ok(None)
    }
  }
}

impl<B> LeafFieldComparator for TermValLeafComparator<B>
where
  B: BinaryDocValues,
{
  type FieldComparator = TermValComparator;
  fn set_bottom(&mut self, slot: usize, comparator: &mut Self::FieldComparator) -> Result<()> {
    comparator.bottom = slot;
    Ok(())
  }

  fn compare_bottom<S>(
    &mut self,
    doc: i32,
    _scorer: &mut S,
    comparator: &mut Self::FieldComparator,
  ) -> Result<i32>
  where
    S: Scorable + ?Sized,
  {
    let (comparator, doc_terms) = (&comparator, &mut self.doc_terms);
    let val = Self::get_value_for_doc(doc_terms, doc)?;
    let bottom_value = comparator.values[comparator.bottom]
      .as_ref()
      .map(BytesRefValue::as_bytes);
    Ok(comparator.compare_value_bytes(bottom_value, val.as_ref().map(BytesRefValue::as_bytes)))
  }

  fn compare_top<S>(
    &mut self,
    doc: i32,
    _scorer: &mut S,
    comparator: &mut Self::FieldComparator,
  ) -> Result<i32>
  where
    S: Scorable + ?Sized,
  {
    let (comparator, doc_terms) = (&comparator, &mut self.doc_terms);
    let doc_value = Self::get_value_for_doc(doc_terms, doc)?;
    let top_value = comparator
      .top_value
      .as_ref()
      .map(TermTopValue::as_ref)
      .transpose()?;
    Ok(comparator.compare_value_bytes(
      top_value.map(BytesRefValue::as_bytes),
      doc_value.as_ref().map(BytesRefValue::as_bytes),
    ))
  }

  fn copy<S>(
    &mut self,
    slot: usize,
    doc: i32,
    _scorer: &mut S,
    comparator: &mut Self::FieldComparator,
  ) -> Result<()>
  where
    S: Scorable + ?Sized,
  {
    match Self::get_value_for_doc(&mut self.doc_terms, doc)? {
      None => comparator.values[slot] = None,
      Some(value) => match comparator.values[slot].as_mut() {
        Some(buffer) => {
          buffer.copy_from_slice(value.as_bytes());
        },
        None => comparator.values[slot] = Some(value.into_owned()),
      },
    }
    Ok(())
  }

  fn set_scorer<S>(
    &mut self,
    _scorer: &mut S,
    _comparator: &mut Self::FieldComparator,
  ) -> Result<()>
  where
    S: Scorable + ?Sized,
  {
    Ok(())
  }

  type DocIdSetIteratorRef<'a>
    = &'a mut DummyDISI
  where
    B: 'a;
}
