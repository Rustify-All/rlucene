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
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::search::comparators::numeric_comparator::{
  NumericComparator, NumericCompetitiveIterator, NumericLeafComparator,
  NumericLeafComparatorDocValues, ToLong, prepare_numeric_doc_values,
};
use crate::core::search::field_comparator::FieldComparator;
use crate::core::search::leaf_field_comparator::LeafFieldComparator;
use crate::core::search::pruning::Pruning;
use crate::core::search::scorable::Scorable;
use crate::core::util::ToInt;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::numeric_utils::NumericUtils;

/// Comparator based on i32 for numHits.
/// This comparator provides a skipping functionality – an iterator that can skip over non-competitive documents.
pub struct IntComparator {
  values: smallvec::SmallVec<[i32; 2]>,
  top_value: i32,
  bottom: i32,
  pub(crate) base: NumericComparator<i32>,
}

impl IntComparator {
  pub fn new<FName>(
    field: FName,
    num_hits: usize,
    missing_value: Option<i32>,
    reverse: bool,
    pruning: Pruning,
  ) -> Self
  where
    FName: Into<String>,
  {
    let field = field.into();
    let missing_value = missing_value.unwrap_or(0);
    let base = NumericComparator::new(
      field,
      missing_value,
      reverse,
      pruning,
      BitUtil::INT_BYTES,
      missing_value as i64,
    );
    Self {
      values: smallvec::smallvec![0; num_hits],
      top_value: 0,
      bottom: 0,
      base,
    }
  }
}

impl FieldComparator for IntComparator {
  type V = i32;
  fn compare(&self, slot1: usize, slot2: usize) -> i32 {
    self.values[slot1].cmp(&self.values[slot2]) as i32
  }

  fn set_top_value(&mut self, value: Self::V) -> Result<()> {
    self.base.set_top_value();
    self.top_value = value;
    Ok(())
  }

  fn value(&self, slot: usize) -> Option<Self::V> {
    Some(self.values[slot])
  }

  type LeafFieldComparator<LR>
    = IntLeafComparator<LR>
  where
    LR: LeafReader;

  fn get_leaf_comparator<LR>(
    &mut self,
    context: &LeafReaderContext<LR>,
  ) -> Result<Self::LeafFieldComparator<LR>>
  where
    LR: LeafReader,
  {
    let (doc_values, candidate, context_reader) =
      prepare_numeric_doc_values!(self.base, context, None, None);
    IntLeafComparator::new(self, context, doc_values, candidate, context_reader)
  }

  fn disable_skipping(&mut self) {
    self.base.disable_skipping();
  }

  fn set_single_sort(&mut self) {
    self.base.set_single_sort();
  }
}

pub struct IntLeafComparator<LR, D = NumericLeafComparatorDocValues<LR>>
where
  LR: LeafReader,
  D: NumericDocValues,
{
  context_reader: Option<LR>,
  base: NumericLeafComparator<
    <LR as LeafReader>::PointValues,
    D,
    IntConverter,
    NumericLeafComparatorDocValues<LR>,
  >,
}
impl<LR, D> IntLeafComparator<LR, D>
where
  LR: LeafReader,
  D: NumericDocValues,
{
  pub fn new(
    comparator: &mut IntComparator,
    context: &LeafReaderContext<LR>,
    doc_values: D,
    candidate: Option<NumericLeafComparatorDocValues<LR>>,
    context_reader: Option<LR>,
  ) -> Result<Self> {
    let top_value = comparator.top_value;
    let base = NumericLeafComparator::with_candidate(
      context,
      &mut comparator.base,
      doc_values,
      candidate,
      IntConverter,
      top_value,
    )?;
    Ok(Self {
      context_reader,
      base,
    })
  }
  fn get_value_for_doc(
    &mut self,
    doc: i32,
    comparator: &mut NumericComparator<i32>,
  ) -> Result<i32> {
    let doc_values = &mut self.base.doc_values;
    if doc_values.advance_exact(doc)? {
      Ok(doc_values.long_value()? as i32)
    } else {
      Ok(comparator.missing_value)
    }
  }
}
impl<LR, D> LeafFieldComparator for IntLeafComparator<LR, D>
where
  LR: LeafReader,
  D: NumericDocValues,
{
  type FieldComparator = IntComparator;
  fn set_bottom(&mut self, slot: usize, comparator: &mut Self::FieldComparator) -> Result<()> {
    comparator.bottom = comparator.values[slot];
    self.base.set_bottom(
      comparator.bottom,
      comparator.top_value,
      &mut comparator.base,
      |comparator| comparator.get_numeric_doc_values(self.context_reader.as_ref()),
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
    Ok(comparator.bottom.cmp(&v).to_int())
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
      |comparator| comparator.get_numeric_doc_values(self.context_reader.as_ref()),
    )
  }

  type DocIdSetIteratorRef<'a>
    = &'a mut NumericCompetitiveIterator<LR>
  where
    LR: 'a,
    D: 'a;

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
      |comparator| comparator.get_numeric_doc_values(self.context_reader.as_ref()),
    )
  }
}
pub(crate) struct IntConverter;
impl ToLong for IntConverter {
  type V = i32;

  fn value_to_long(&self, v: Self::V) -> i64 {
    v as i64
  }

  fn bytes_to_long(&self, bytes: &[u8]) -> i64 {
    NumericUtils::sortable_bytes_to_int(bytes, 0) as i64
  }
}
