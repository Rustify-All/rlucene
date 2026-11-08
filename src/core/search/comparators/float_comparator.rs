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
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::numeric_utils::NumericUtils;
use crate::core::util::{CoreHelper, ToInt};

/// Comparator for `num_hits` that treats all NaNs as equal and greater than
/// positive infinity, and sorts negative zero before positive zero.
///
/// Provides an iterator that can skip over non-competitive documents.
pub struct FloatComparator {
  values: smallvec::SmallVec<[f32; 2]>,
  top_value: f32,
  bottom: f32,
  pub(crate) base: NumericComparator<f32>,
}

impl FloatComparator {
  pub fn new<FName>(
    field: FName,
    num_hits: usize,
    missing_value: Option<f32>,
    reverse: bool,
    pruning: Pruning,
  ) -> Self
  where
    FName: Into<String>,
  {
    let field = field.into();
    let missing_value = missing_value.unwrap_or(0f32);
    let base = NumericComparator::new(
      field,
      missing_value,
      reverse,
      pruning,
      BitUtil::FLOAT_BYTES,
      NumericUtils::float_to_sortable_int(missing_value) as i64,
    );
    Self {
      values: smallvec::smallvec![0.0; num_hits],
      top_value: 0.0,
      bottom: 0.0,
      base,
    }
  }
}

impl FieldComparator for FloatComparator {
  type V = f32;

  fn compare(&self, slot1: usize, slot2: usize) -> i32 {
    let slot1_v = self.values[slot1];
    let slot2_v = self.values[slot2];
    CoreHelper::compare_f32(slot1_v, slot2_v).to_int()
  }

  fn set_top_value(&mut self, value: Self::V) -> Result<()> {
    self.base.set_top_value();
    self.top_value = value;
    Ok(())
  }

  fn value(&self, slot: usize) -> Option<Self::V> {
    Some(self.values[slot])
  }

  fn compare_values(&self, first: Option<&Self::V>, second: Option<&Self::V>) -> Result<i32> {
    match (first, second) {
      (None, None) => Ok(0),
      (None, Some(_)) => Ok(-1),
      (Some(_), None) => Ok(1),
      (Some(&first), Some(&second)) => Ok(CoreHelper::compare_f32(first, second).to_int()),
    }
  }

  type LeafFieldComparator<LR>
    = FloatLeafComparator<LR>
  where
    LR: LeafReader;

  fn get_leaf_comparator<LR>(
    &mut self,
    context: &LeafReaderContext<LR>,
  ) -> Result<Self::LeafFieldComparator<LR>>
  where
    LR: LeafReader,
  {
    FloatLeafComparator::new(self, context, None, None)
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

  fn disable_skipping(&mut self) {
    self.base.disable_skipping()
  }

  fn set_single_sort(&mut self) {
    self.base.set_single_sort();
  }
}
pub struct FloatLeafComparator<LR>
where
  LR: LeafReader,
{
  base: NumericLeafComparator<
    <LR as LeafReader>::PointValues,
    NumericLeafComparatorDocValues<LR>,
    FloatConverter,
  >,
}

impl<LR> FloatLeafComparator<LR>
where
  LR: LeafReader,
{
  pub fn new(
    comparator: &mut FloatComparator,
    context: &LeafReaderContext<LR>,
    doc_values: Option<NumericLeafComparatorDocValues<LR>>,
    candidate: Option<NumericLeafComparatorDocValues<LR>>,
  ) -> Result<Self> {
    let (doc_value, candidate) = match (doc_values, candidate) {
      (Some(v1), Some(v2)) => (v1, v2),
      (None, None) => {
        let v1 = NumericLeafComparatorDocValues::B(DocValues::get_numeric(
          context.reader(),
          &comparator.base.field,
        )?);
        let v2 = NumericLeafComparatorDocValues::B(DocValues::get_numeric(
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
      FloatConverter,
      top_value,
    )?;
    Ok(Self { base })
  }
  fn get_value_for_doc(
    &mut self,
    doc: i32,
    comparator: &mut NumericComparator<f32>,
  ) -> Result<f32> {
    let doc_values = &mut self.base.doc_values;
    if doc_values.advance_exact(doc)? {
      let bits = doc_values.long_value()? as u32;
      Ok(f32::from_bits(bits))
    } else {
      Ok(comparator.missing_value)
    }
  }
}

impl<LR> LeafFieldComparator for FloatLeafComparator<LR>
where
  LR: LeafReader,
{
  type FieldComparator = FloatComparator;
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
    S: Scorable + ?Sized,
  {
    let v = self.get_value_for_doc(doc, &mut comparator.base)?;
    Ok(CoreHelper::compare_f32(comparator.bottom, v).to_int())
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
    let v = self.get_value_for_doc(doc, &mut comparator.base)?;
    Ok(CoreHelper::compare_f32(comparator.top_value, v).to_int())
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
    let v = self.get_value_for_doc(doc, &mut comparator.base)?;
    comparator.values[slot] = v;
    self.base.copy(doc)
  }

  fn set_scorer<S>(&mut self, scorer: &mut S, comparator: &mut Self::FieldComparator) -> Result<()>
  where
    S: Scorable + ?Sized,
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
pub(crate) struct FloatConverter;
impl ToLong for FloatConverter {
  type V = f32;

  fn value_to_long(&self, v: Self::V) -> i64 {
    NumericUtils::float_to_sortable_int(v) as i64
  }

  fn bytes_to_long(&self, bytes: &[u8]) -> i64 {
    NumericUtils::sortable_bytes_to_int(bytes, 0) as i64
  }
}
