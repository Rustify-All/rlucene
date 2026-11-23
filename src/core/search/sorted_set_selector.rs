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
use crate::core::index::BytesRefValueEnum;
use crate::core::util::access::ByteSource;

use crate::core::index::doc_values::DocValues;
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::sorted_doc_values::{SortedDocValues, SortedDocValuesVisitor};
use crate::core::index::sorted_doc_values_terms_enum::SortedDocValuesTermsEnum;
use crate::core::index::sorted_set_doc_values::SortedSetDocValues;
use crate::core::index::{BytesRef, BytesRefValue};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::util::error::lucene_error::{LuceneError, Result};

/// Selects a value from the document's set to use as the representative value.
pub struct SortedSetSelector;
impl SortedSetSelector {
  /// Wraps a multi-valued SortedSetDocValues as a single-valued view, using
  /// the specified selector.
  pub fn wrap<S>(
    mut sorted_set: S,
    selector: SortedSetSelectorType,
  ) -> Result<SortedDocValuesWrap<S>>
  where
    S: SortedSetDocValues,
  {
    if sorted_set.get_value_count()? >= i32::MAX as i64 {
      return Err(LuceneError::unsupported_operation(format!(
        "fields containing more than {} unique terms are unsupported",
        i32::MAX - 1
      )));
    }
    if sorted_set.is_single_valued() {
      let singleton = DocValues::unwrap_singleton_sorted(&mut sorted_set)?;
      Ok(SortedDocValuesWrap::Singleton(singleton))
    } else {
      let v = match selector {
        SortedSetSelectorType::Min => SortedDocValuesWrap::Min(MinValue::new(sorted_set)),
        SortedSetSelectorType::Max => SortedDocValuesWrap::Max(MaxValue::new(sorted_set)),
        SortedSetSelectorType::MiddleMin => {
          SortedDocValuesWrap::MiddleMin(MiddleMinValue::new(sorted_set))
        },
        SortedSetSelectorType::MiddleMax => {
          SortedDocValuesWrap::MiddleMax(MiddleMaxValue::new(sorted_set))
        },
      };
      Ok(v)
    }
  }
}
/// Type of selection to perform.
///
/// # Limitations
/// - Fields containing `i32::MAX` or more unique values are unsupported.
/// - Selectors other than [`SortedSetSelectorType::Min`] require optional codec
///   support. However, several codecs provided by Lucene, including the current
///   default codec, support this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SortedSetSelectorType {
  /// Selects the minimum value in the set.
  Min,
  /// Selects the maximum value in the set.
  Max,
  /// Selects the middle value in the set.
  ///
  /// If the set has an even number of values, the lower of the middle two is
  /// chosen.
  MiddleMin,
  /// Selects the middle value in the set.
  ///
  /// If the set has an even number of values, the higher of the middle two
  /// is chosen.
  MiddleMax,
}
impl SortedSetSelectorType {
  pub fn values() -> &'static [Self] {
    &[Self::Min, Self::Max, Self::MiddleMin, Self::MiddleMax]
  }
}
/// Wraps a SortedSetDocValues and returns the first ordinal (min)
pub struct MinValue<S> {
  inner: S,
  ord: i32,
}

impl<S> MinValue<S> {
  fn new(inner: S) -> Self {
    Self { inner, ord: 0 }
  }
}

impl<S> MinValue<S>
where
  S: SortedSetDocValues,
{
  fn set_ord(&mut self) -> Result<()> {
    if self.doc_id() != NO_MORE_DOCS {
      self.ord = self.inner.next_ord()? as i32
    }
    Ok(())
  }
}

impl<S> DocValuesIterator for MinValue<S>
where
  S: SortedSetDocValues,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    if self.inner.advance_exact(target)? {
      self.set_ord()?;
      return Ok(true);
    }
    Ok(false)
  }
}

impl<S> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions for MinValue<S> where
  S: SortedSetDocValues
{
}
impl<S> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for MinValue<S> where
  S: SortedSetDocValues
{
}

impl<S> DocIdSetIterator for MinValue<S>
where
  S: SortedSetDocValues,
{
  fn doc_id(&self) -> i32 {
    self.inner.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.inner.next_doc()?;
    self.set_ord()?;
    Ok(self.doc_id())
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.inner.advance(target)?;
    self.set_ord()?;
    Ok(self.doc_id())
  }

  fn cost(&self) -> Result<i64> {
    self.inner.cost()
  }
}

impl<S> SortedDocValues for MinValue<S>
where
  S: SortedSetDocValues,
{
  type OrdValue<'a>
    = S::OrdValue<'a>
  where
    Self: 'a;

  fn ord_value(&mut self) -> Result<i32> {
    Ok(self.ord)
  }

  fn lookup_ord(&mut self, ord: i32) -> Result<Self::OrdValue<'_>> {
    self.inner.lookup_ord(ord as i64)
  }

  fn get_value_count(&self) -> Result<i32> {
    Ok(self.inner.get_value_count()? as i32)
  }

  fn lookup_term<BS: ByteSource>(&mut self, key: &BytesRef<BS>) -> Result<i32> {
    Ok(self.inner.lookup_term(key)? as i32)
  }
  type TermsEnum<'a>
    = SortedDocValuesTermsEnum<&'a mut Self>
  where
    S: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    self.default_terms_enum()
  }
}
/// Wraps a SortedSetDocValues and returns the last ordinal (max)
pub struct MaxValue<S> {
  inner: S,
  ord: i32,
}

impl<S> MaxValue<S> {
  fn new(inner: S) -> Self {
    Self { inner, ord: 0 }
  }
}

impl<S> MaxValue<S>
where
  S: SortedSetDocValues,
{
  fn set_ord(&mut self) -> Result<()> {
    if self.doc_id() != NO_MORE_DOCS {
      let doc_value_count = self.inner.doc_value_count()?;
      for _ in 0..(doc_value_count - 1) {
        self.inner.next_ord()?;
      }
      self.ord = self.inner.next_ord()? as i32;
    }
    Ok(())
  }
}

impl<S> DocValuesIterator for MaxValue<S>
where
  S: SortedSetDocValues,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    if self.inner.advance_exact(target)? {
      self.set_ord()?;
      return Ok(true);
    }
    Ok(false)
  }
}

impl<S> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions for MaxValue<S> where
  S: SortedSetDocValues
{
}
impl<S> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for MaxValue<S> where
  S: SortedSetDocValues
{
}

impl<S> DocIdSetIterator for MaxValue<S>
where
  S: SortedSetDocValues,
{
  fn doc_id(&self) -> i32 {
    self.inner.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.inner.next_doc()?;
    self.set_ord()?;
    Ok(self.doc_id())
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.inner.advance(target)?;
    self.set_ord()?;
    Ok(self.doc_id())
  }

  fn cost(&self) -> Result<i64> {
    self.inner.cost()
  }
}

impl<S> SortedDocValues for MaxValue<S>
where
  S: SortedSetDocValues,
{
  type OrdValue<'a>
    = S::OrdValue<'a>
  where
    Self: 'a;

  fn ord_value(&mut self) -> Result<i32> {
    Ok(self.ord)
  }

  fn lookup_ord(&mut self, ord: i32) -> Result<Self::OrdValue<'_>> {
    self.inner.lookup_ord(ord as i64)
  }

  fn get_value_count(&self) -> Result<i32> {
    Ok(self.inner.get_value_count()? as i32)
  }

  fn lookup_term<BS: ByteSource>(&mut self, key: &BytesRef<BS>) -> Result<i32> {
    Ok(self.inner.lookup_term(key)? as i32)
  }

  type TermsEnum<'a>
    = SortedDocValuesTermsEnum<&'a mut Self>
  where
    S: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    self.default_terms_enum()
  }
}
/// Wraps a SortedSetDocValues and returns the middle ordinal (or min of the
/// two)
pub struct MiddleMinValue<S> {
  inner: S,
  ord: i32,
}

impl<S> MiddleMinValue<S> {
  fn new(inner: S) -> Self {
    Self { inner, ord: 0 }
  }
}

impl<S> MiddleMinValue<S>
where
  S: SortedSetDocValues,
{
  fn set_ord(&mut self) -> Result<()> {
    if self.doc_id() != NO_MORE_DOCS {
      let doc_value_count = self.inner.doc_value_count()?;
      let target_idx = (doc_value_count - 1) >> 1;
      for _ in 0..target_idx {
        self.inner.next_ord()?;
      }
      self.ord = self.inner.next_ord()? as i32;
    }
    Ok(())
  }
}

impl<S> DocValuesIterator for MiddleMinValue<S>
where
  S: SortedSetDocValues,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    if self.inner.advance_exact(target)? {
      self.set_ord()?;
      return Ok(true);
    }
    Ok(false)
  }
}

impl<S> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions for MiddleMinValue<S> where
  S: SortedSetDocValues
{
}
impl<S> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for MiddleMinValue<S> where
  S: SortedSetDocValues
{
}

impl<S> DocIdSetIterator for MiddleMinValue<S>
where
  S: SortedSetDocValues,
{
  fn doc_id(&self) -> i32 {
    self.inner.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.inner.next_doc()?;
    self.set_ord()?;
    Ok(self.doc_id())
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.inner.advance(target)?;
    self.set_ord()?;
    Ok(self.doc_id())
  }

  fn cost(&self) -> Result<i64> {
    self.inner.cost()
  }
}

impl<S> SortedDocValues for MiddleMinValue<S>
where
  S: SortedSetDocValues,
{
  type OrdValue<'a>
    = S::OrdValue<'a>
  where
    Self: 'a;

  fn ord_value(&mut self) -> Result<i32> {
    Ok(self.ord)
  }

  fn lookup_ord(&mut self, ord: i32) -> Result<Self::OrdValue<'_>> {
    self.inner.lookup_ord(ord as i64)
  }

  fn get_value_count(&self) -> Result<i32> {
    Ok(self.inner.get_value_count()? as i32)
  }

  fn lookup_term<BS: ByteSource>(&mut self, key: &BytesRef<BS>) -> Result<i32> {
    Ok(self.inner.lookup_term(key)? as i32)
  }
  type TermsEnum<'a>
    = SortedDocValuesTermsEnum<&'a mut Self>
  where
    S: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    self.default_terms_enum()
  }
}
/// Wraps a SortedSetDocValues and returns the middle ordinal (or max of the
/// two)
pub struct MiddleMaxValue<S> {
  inner: S,
  ord: i32,
}

impl<S> MiddleMaxValue<S> {
  fn new(inner: S) -> Self {
    Self { inner, ord: 0 }
  }
}

impl<S> MiddleMaxValue<S>
where
  S: SortedSetDocValues,
{
  fn set_ord(&mut self) -> Result<()> {
    if self.doc_id() != NO_MORE_DOCS {
      let count = self.inner.doc_value_count()?;
      let target_idx = ((count as u64) >> 1) as i32;
      for _ in 0..target_idx {
        self.inner.next_ord()?;
      }
      self.ord = self.inner.next_ord()? as i32;
    }
    Ok(())
  }
}

impl<S> DocValuesIterator for MiddleMaxValue<S>
where
  S: SortedSetDocValues,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    if self.inner.advance_exact(target)? {
      self.set_ord()?;
      return Ok(true);
    }
    Ok(false)
  }
}

impl<S> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions for MiddleMaxValue<S> where
  S: SortedSetDocValues
{
}
impl<S> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for MiddleMaxValue<S> where
  S: SortedSetDocValues
{
}

impl<S> DocIdSetIterator for MiddleMaxValue<S>
where
  S: SortedSetDocValues,
{
  fn doc_id(&self) -> i32 {
    self.inner.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.inner.next_doc()?;
    self.set_ord()?;
    Ok(self.doc_id())
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.inner.advance(target)?;
    self.set_ord()?;
    Ok(self.doc_id())
  }

  fn cost(&self) -> Result<i64> {
    self.inner.cost()
  }
}

impl<S> SortedDocValues for MiddleMaxValue<S>
where
  S: SortedSetDocValues,
{
  type OrdValue<'a>
    = S::OrdValue<'a>
  where
    Self: 'a;

  fn ord_value(&mut self) -> Result<i32> {
    Ok(self.ord)
  }

  fn lookup_ord(&mut self, ord: i32) -> Result<Self::OrdValue<'_>> {
    self.inner.lookup_ord(ord as i64)
  }

  fn get_value_count(&self) -> Result<i32> {
    Ok(self.inner.get_value_count()? as i32)
  }

  fn lookup_term<BS: ByteSource>(&mut self, key: &BytesRef<BS>) -> Result<i32> {
    Ok(self.inner.lookup_term(key)? as i32)
  }
  type TermsEnum<'a>
    = SortedDocValuesTermsEnum<&'a mut Self>
  where
    S: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    self.default_terms_enum()
  }
}

pub enum SortedDocValuesWrap<S>
where
  S: SortedSetDocValues,
{
  Singleton(S::SortedDocValues),
  Min(MinValue<S>),
  Max(MaxValue<S>),
  MiddleMin(MiddleMinValue<S>),
  MiddleMax(MiddleMaxValue<S>),
}

impl<S> DocValuesIterator for SortedDocValuesWrap<S>
where
  S: SortedSetDocValues,
{
  #[inline(always)]
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      SortedDocValuesWrap::Singleton(single) => single.advance_exact(target),
      SortedDocValuesWrap::Min(min) => min.advance_exact(target),
      SortedDocValuesWrap::Max(max) => max.advance_exact(target),
      SortedDocValuesWrap::MiddleMin(middle_min) => middle_min.advance_exact(target),
      SortedDocValuesWrap::MiddleMax(middle_max) => middle_max.advance_exact(target),
    }
  }
}

impl<S> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SortedDocValuesWrap<S>
where
  S: SortedSetDocValues,
{
}
impl<S> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for SortedDocValuesWrap<S> where
  S: SortedSetDocValues
{
}

impl<S> DocIdSetIterator for SortedDocValuesWrap<S>
where
  S: SortedSetDocValues,
{
  fn doc_id(&self) -> i32 {
    match self {
      SortedDocValuesWrap::Singleton(single) => single.doc_id(),
      SortedDocValuesWrap::Min(min) => min.doc_id(),
      SortedDocValuesWrap::Max(max) => max.doc_id(),
      SortedDocValuesWrap::MiddleMin(middle_min) => middle_min.doc_id(),
      SortedDocValuesWrap::MiddleMax(middle_max) => middle_max.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      SortedDocValuesWrap::Singleton(single) => single.next_doc(),
      SortedDocValuesWrap::Min(min) => min.next_doc(),
      SortedDocValuesWrap::Max(max) => max.next_doc(),
      SortedDocValuesWrap::MiddleMin(middle_min) => middle_min.next_doc(),
      SortedDocValuesWrap::MiddleMax(middle_max) => middle_max.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      SortedDocValuesWrap::Singleton(single) => single.advance(target),
      SortedDocValuesWrap::Min(min) => min.advance(target),
      SortedDocValuesWrap::Max(max) => max.advance(target),
      SortedDocValuesWrap::MiddleMin(middle_min) => middle_min.advance(target),
      SortedDocValuesWrap::MiddleMax(middle_max) => middle_max.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      SortedDocValuesWrap::Singleton(single) => single.slow_advance(target),
      SortedDocValuesWrap::Min(min) => min.slow_advance(target),
      SortedDocValuesWrap::Max(max) => max.slow_advance(target),
      SortedDocValuesWrap::MiddleMin(middle_min) => middle_min.slow_advance(target),
      SortedDocValuesWrap::MiddleMax(middle_max) => middle_max.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      SortedDocValuesWrap::Singleton(single) => single.cost(),
      SortedDocValuesWrap::Min(min) => min.cost(),
      SortedDocValuesWrap::Max(max) => max.cost(),
      SortedDocValuesWrap::MiddleMin(middle_min) => middle_min.cost(),
      SortedDocValuesWrap::MiddleMax(middle_max) => middle_max.cost(),
    }
  }
}

impl<S> SortedDocValues for SortedDocValuesWrap<S>
where
  S: SortedSetDocValues,
{
  type OrdValue<'a>
    = BytesRefValueEnum<'a>
  where
    Self: 'a;

  fn ord_value(&mut self) -> Result<i32> {
    match self {
      SortedDocValuesWrap::Singleton(single) => single.ord_value(),
      SortedDocValuesWrap::Min(min) => min.ord_value(),
      SortedDocValuesWrap::Max(max) => max.ord_value(),
      SortedDocValuesWrap::MiddleMin(middle_min) => middle_min.ord_value(),
      SortedDocValuesWrap::MiddleMax(middle_max) => middle_max.ord_value(),
    }
  }

  fn visit<V: SortedDocValuesVisitor>(&mut self, visitor: V) -> V::Output {
    match self {
      Self::Singleton(inner) => visitor.visit(inner),
      Self::Min(inner) => visitor.visit(inner),
      Self::Max(inner) => visitor.visit(inner),
      Self::MiddleMin(inner) => visitor.visit(inner),
      Self::MiddleMax(inner) => visitor.visit(inner),
    }
  }

  fn lookup_ord(&mut self, ord: i32) -> Result<Self::OrdValue<'_>> {
    match self {
      SortedDocValuesWrap::Singleton(single) => {
        single.lookup_ord(ord).map(BytesRefValue::into_value)
      },
      SortedDocValuesWrap::Min(min) => min.lookup_ord(ord).map(BytesRefValue::into_value),
      SortedDocValuesWrap::Max(max) => max.lookup_ord(ord).map(BytesRefValue::into_value),
      SortedDocValuesWrap::MiddleMin(middle_min) => {
        middle_min.lookup_ord(ord).map(BytesRefValue::into_value)
      },
      SortedDocValuesWrap::MiddleMax(middle_max) => {
        middle_max.lookup_ord(ord).map(BytesRefValue::into_value)
      },
    }
  }

  fn get_value_count(&self) -> Result<i32> {
    match self {
      SortedDocValuesWrap::Singleton(single) => single.get_value_count(),
      SortedDocValuesWrap::Min(min) => min.get_value_count(),
      SortedDocValuesWrap::Max(max) => max.get_value_count(),
      SortedDocValuesWrap::MiddleMin(middle_min) => middle_min.get_value_count(),
      SortedDocValuesWrap::MiddleMax(middle_max) => middle_max.get_value_count(),
    }
  }

  fn lookup_term<BS: ByteSource>(&mut self, key: &BytesRef<BS>) -> Result<i32> {
    match self {
      SortedDocValuesWrap::Singleton(single) => single.lookup_term(key),
      SortedDocValuesWrap::Min(min) => min.lookup_term(key),
      SortedDocValuesWrap::Max(max) => max.lookup_term(key),
      SortedDocValuesWrap::MiddleMin(middle_min) => middle_min.lookup_term(key),
      SortedDocValuesWrap::MiddleMax(middle_max) => middle_max.lookup_term(key),
    }
  }

  type TermsEnum<'a>
    = SortedDocValuesTermsEnum<&'a mut Self>
  where
    S: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    self.default_terms_enum()
  }
}
