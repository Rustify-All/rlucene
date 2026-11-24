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
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::sorted_numeric_doc_values::SortedNumericDocValues;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::sort_field::SortFieldType;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::numeric_utils::NumericUtils;

#[derive(Clone)]
/// Selects a value from the document’s list to use as the representative value.
///
/// This provides a [`NumericDocValues`] view over the [`SortedNumeric`](crate::core::index::doc_values::SortedNumeric), for use with sorting,
/// expressions, function queries, etc.
pub struct SortedNumericSelector;
impl SortedNumericSelector {
  /// Wraps a multi-valued [`SortedNumericDocValues`] as a single-valued view,
  /// using the specified `selector` and `numeric_type`.
  pub fn wrap<S>(
    mut sorted_numeric: S,
    selector: SortedNumericSelectorType,
    numeric_type: SortFieldType,
  ) -> Result<SortedNumericSelectorWrap<S>>
  where
    S: SortedNumericDocValues,
  {
    match numeric_type {
      SortFieldType::Int | SortFieldType::Long | SortFieldType::Float | SortFieldType::Double => {},
      _ => {
        return Err(LuceneError::illegal_argument(
          "numericType must be a numeric type",
        ));
      },
    }
    let view = if sorted_numeric.is_single_valued() {
      SelectedNumericDocValues::Single(DocValues::unwrap_singleton_numeric(&mut sorted_numeric)?)
    } else {
      match selector {
        SortedNumericSelectorType::Min => {
          SelectedNumericDocValues::Min(MinValue::new(sorted_numeric))
        },
        SortedNumericSelectorType::Max => {
          SelectedNumericDocValues::Max(MaxValue::new(sorted_numeric))
        },
      }
    };

    match numeric_type {
      SortFieldType::Float => Ok(SortedNumericSelectorWrap::Float(
        FilterNumericDocValuesImpl1::new(view),
      )),
      SortFieldType::Double => Ok(SortedNumericSelectorWrap::Double(
        FilterNumericDocValuesImpl2::new(view),
      )),
      _ => Ok(SortedNumericSelectorWrap::Raw(view)),
    }
  }
}
/// Type of selection to perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SortedNumericSelectorType {
  /// Selects the minimum value in the set.
  Min,
  /// Selects the maximum value in the set.
  Max,
}
impl SortedNumericSelectorType {
  pub fn values() -> &'static [Self] {
    &[Self::Min, Self::Max]
  }
}

pub struct MinValue<S> {
  inner: S,
  value: i64,
}
impl<S> MinValue<S> {
  pub fn new(inner: S) -> Self {
    MinValue { inner, value: 0 }
  }
}

impl<S> DocValuesIterator for MinValue<S>
where
  S: SortedNumericDocValues,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    if self.inner.advance_exact(target)? {
      self.value = self.inner.next_value()?;
      Ok(true)
    } else {
      Ok(false)
    }
  }
}
/// Wraps a SortedNumericDocValues and returns the first value (min)
impl<S> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions for MinValue<S> where
  S: SortedNumericDocValues
{
}
impl<S> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for MinValue<S> where
  S: SortedNumericDocValues
{
}

impl<S> DocIdSetIterator for MinValue<S>
where
  S: SortedNumericDocValues,
{
  fn doc_id(&self) -> i32 {
    self.inner.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    let doc = self.inner.next_doc()?;
    if doc != NO_MORE_DOCS {
      self.value = self.inner.next_value()?;
    }
    Ok(doc)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    let doc = self.inner.advance(target)?;
    if doc != NO_MORE_DOCS {
      self.value = self.inner.next_value()?;
    }
    Ok(doc)
  }

  fn cost(&self) -> Result<i64> {
    self.inner.cost()
  }
}

impl<S> NumericDocValues for MinValue<S>
where
  S: SortedNumericDocValues,
{
  fn long_value(&mut self) -> Result<i64> {
    Ok(self.value)
  }
}
/// Wraps a SortedNumericDocValues and returns the last value (max)
pub struct MaxValue<S> {
  inner: S,
  value: i64,
}

impl<S> MaxValue<S> {
  pub fn new(inner: S) -> Self {
    MaxValue { inner, value: 0 }
  }
}

impl<S> MaxValue<S>
where
  S: SortedNumericDocValues,
{
  fn set_value(&mut self) -> Result<()> {
    let count = self.inner.doc_value_count()?;
    for _ in 0..count {
      self.value = self.inner.next_value()?;
    }
    Ok(())
  }
}

impl<S> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions for MaxValue<S> where
  S: SortedNumericDocValues
{
}
impl<S> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for MaxValue<S> where
  S: SortedNumericDocValues
{
}

impl<S> DocIdSetIterator for MaxValue<S>
where
  S: SortedNumericDocValues,
{
  fn doc_id(&self) -> i32 {
    self.inner.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    let doc = self.inner.next_doc()?;
    if doc != NO_MORE_DOCS {
      self.set_value()?;
    }
    Ok(doc)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    let doc = self.inner.advance(target)?;
    if doc != NO_MORE_DOCS {
      self.set_value()?;
    }
    Ok(doc)
  }

  fn cost(&self) -> Result<i64> {
    self.inner.cost()
  }
}

impl<S> DocValuesIterator for MaxValue<S>
where
  S: SortedNumericDocValues,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    if self.inner.advance_exact(target)? {
      self.set_value()?;
      Ok(true)
    } else {
      Ok(false)
    }
  }
}

impl<S> NumericDocValues for MaxValue<S>
where
  S: SortedNumericDocValues,
{
  fn long_value(&mut self) -> Result<i64> {
    Ok(self.value)
  }
}

pub struct FilterNumericDocValuesImpl1<N> {
  pub(crate) inner: N,
}
impl<N> FilterNumericDocValuesImpl1<N> {
  pub fn new(inner: N) -> Self {
    FilterNumericDocValuesImpl1 { inner }
  }
}

impl<N> DocValuesIterator for FilterNumericDocValuesImpl1<N>
where
  N: NumericDocValues,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.inner.advance_exact(target)
  }
}

impl<N> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for FilterNumericDocValuesImpl1<N>
where
  N: NumericDocValues,
{
}
impl<N> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for FilterNumericDocValuesImpl1<N>
where
  N: NumericDocValues,
{
}

impl<N> DocIdSetIterator for FilterNumericDocValuesImpl1<N>
where
  N: NumericDocValues,
{
  fn doc_id(&self) -> i32 {
    self.inner.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.inner.next_doc()
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.inner.advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self.inner.cost()
  }
}

impl<N> NumericDocValues for FilterNumericDocValuesImpl1<N>
where
  N: NumericDocValues,
{
  fn long_value(&mut self) -> Result<i64> {
    let v = self.inner.long_value()? as i32;
    Ok(NumericUtils::sortable_float_bits(v) as i64)
  }
}
pub struct FilterNumericDocValuesImpl2<N> {
  pub(crate) inner: N,
}
impl<N> FilterNumericDocValuesImpl2<N> {
  pub fn new(inner: N) -> Self {
    FilterNumericDocValuesImpl2 { inner }
  }
}

impl<N> DocValuesIterator for FilterNumericDocValuesImpl2<N>
where
  N: NumericDocValues,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.inner.advance_exact(target)
  }
}

impl<N> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for FilterNumericDocValuesImpl2<N>
where
  N: NumericDocValues,
{
}
impl<N> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for FilterNumericDocValuesImpl2<N>
where
  N: NumericDocValues,
{
}

impl<N> DocIdSetIterator for FilterNumericDocValuesImpl2<N>
where
  N: NumericDocValues,
{
  fn doc_id(&self) -> i32 {
    self.inner.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.inner.next_doc()
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.inner.advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self.inner.cost()
  }
}

impl<N> NumericDocValues for FilterNumericDocValuesImpl2<N>
where
  N: NumericDocValues,
{
  fn long_value(&mut self) -> Result<i64> {
    let v = self.inner.long_value()?;
    Ok(NumericUtils::sortable_double_bits(v))
  }
}

pub enum SelectedNumericDocValues<S>
where
  S: SortedNumericDocValues,
{
  Single(S::NumericDocValues),
  Min(MinValue<S>),
  Max(MaxValue<S>),
}

impl<S> DocValuesIterator for SelectedNumericDocValues<S>
where
  S: SortedNumericDocValues,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::Single(inner) => inner.advance_exact(target),
      Self::Min(inner) => inner.advance_exact(target),
      Self::Max(inner) => inner.advance_exact(target),
    }
  }
}

impl<S> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SelectedNumericDocValues<S>
where
  S: SortedNumericDocValues,
{
}
impl<S> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for SelectedNumericDocValues<S>
where
  S: SortedNumericDocValues,
{
}

impl<S> DocIdSetIterator for SelectedNumericDocValues<S>
where
  S: SortedNumericDocValues,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::Single(inner) => inner.doc_id(),
      Self::Min(inner) => inner.doc_id(),
      Self::Max(inner) => inner.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::Single(inner) => inner.next_doc(),
      Self::Min(inner) => inner.next_doc(),
      Self::Max(inner) => inner.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Single(inner) => inner.advance(target),
      Self::Min(inner) => inner.advance(target),
      Self::Max(inner) => inner.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Single(inner) => inner.slow_advance(target),
      Self::Min(inner) => inner.slow_advance(target),
      Self::Max(inner) => inner.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::Single(inner) => inner.cost(),
      Self::Min(inner) => inner.cost(),
      Self::Max(inner) => inner.cost(),
    }
  }
}

impl<S> NumericDocValues for SelectedNumericDocValues<S>
where
  S: SortedNumericDocValues,
{
  fn long_value(&mut self) -> Result<i64> {
    match self {
      Self::Single(inner) => inner.long_value(),
      Self::Min(inner) => inner.long_value(),
      Self::Max(inner) => inner.long_value(),
    }
  }
}

pub enum SortedNumericSelectorWrap<S>
where
  S: SortedNumericDocValues,
{
  Float(FilterNumericDocValuesImpl1<SelectedNumericDocValues<S>>),
  Double(FilterNumericDocValuesImpl2<SelectedNumericDocValues<S>>),
  Raw(SelectedNumericDocValues<S>),
}

impl<S> DocValuesIterator for SortedNumericSelectorWrap<S>
where
  S: SortedNumericDocValues,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::Float(inner) => inner.advance_exact(target),
      Self::Double(inner) => inner.advance_exact(target),
      Self::Raw(inner) => inner.advance_exact(target),
    }
  }
}

impl<S> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SortedNumericSelectorWrap<S>
where
  S: SortedNumericDocValues,
{
}
impl<S> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for SortedNumericSelectorWrap<S>
where
  S: SortedNumericDocValues,
{
}

impl<S> DocIdSetIterator for SortedNumericSelectorWrap<S>
where
  S: SortedNumericDocValues,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::Float(inner) => inner.doc_id(),
      Self::Double(inner) => inner.doc_id(),
      Self::Raw(inner) => inner.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::Float(inner) => inner.next_doc(),
      Self::Double(inner) => inner.next_doc(),
      Self::Raw(inner) => inner.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Float(inner) => inner.advance(target),
      Self::Double(inner) => inner.advance(target),
      Self::Raw(inner) => inner.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Float(inner) => inner.slow_advance(target),
      Self::Double(inner) => inner.slow_advance(target),
      Self::Raw(inner) => inner.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::Float(inner) => inner.cost(),
      Self::Double(inner) => inner.cost(),
      Self::Raw(inner) => inner.cost(),
    }
  }
}

impl<S> NumericDocValues for SortedNumericSelectorWrap<S>
where
  S: SortedNumericDocValues,
{
  fn long_value(&mut self) -> Result<i64> {
    match self {
      Self::Float(inner) => inner.long_value(),
      Self::Double(inner) => inner.long_value(),
      Self::Raw(inner) => inner.long_value(),
    }
  }
}
