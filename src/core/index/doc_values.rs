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
use crate::core::codecs::block_term_state::TermStateEnum;
use crate::core::index::BytesRefValueEnum;
use crate::core::index::binary_doc_values::BinaryDocValues;
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::singleton_sorted_numeric_doc_values::SingletonSortedNumericDocValues;
use crate::core::index::singleton_sorted_set_doc_values::SingletonSortedSetDocValues;
use crate::core::index::sorted_doc_values::SortedDocValues;
use crate::core::index::sorted_doc_values_terms_enum::SortedDocValuesTermsEnum;
use crate::core::index::sorted_numeric_doc_values::SortedNumericDocValues;
use crate::core::index::sorted_set_doc_values::SortedSetDocValues;
use crate::core::index::sorted_set_doc_values_writer::SortedSetDocValuesEnum2;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::index::{BytesRef, BytesRefValue};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::util::access::ByteSource;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::{LuceneError, Result};

pub type EmptySortedSet = SingletonSortedSetDocValues<EmptySorted>;
/// This struct contains utility methods and constants for DocValues
pub struct DocValues;
impl DocValues {
  /// An empty [`BinaryDocValues`] which returns no documents
  pub fn empty_binary() -> EmptyBinary {
    EmptyBinary::new()
  }
  /// An empty [`NumericDocValues`] which returns no documents
  pub fn empty_numeric() -> EmptyNumeric {
    EmptyNumeric::new()
  }
  /// An empty SortedDocValues which returns empty BytesRef for every document
  pub fn empty_sorted() -> EmptySorted {
    EmptySorted::new()
  }
  /// An empty SortedNumericDocValues which returns zero values for every
  /// document.
  pub fn empty_sorted_numeric() -> Result<SingletonSortedNumericDocValues<EmptyNumeric>> {
    Self::singleton_numeric(Self::empty_numeric())
  }
  /// An empty SortedDocValues which returns empty [`BytesRef`] for every
  /// document.
  pub fn empty_sorted_set() -> Result<SingletonSortedSetDocValues<EmptySorted>> {
    Self::singleton_sorted(Self::empty_sorted())
  }

  /// Returns a multi-valued view over the provided SortedDocValues.
  pub fn singleton_sorted<S>(dv: S) -> Result<SingletonSortedSetDocValues<S>>
  where
    S: SortedDocValues,
  {
    SingletonSortedSetDocValues::new(dv)
  }

  /// Returns a single-valued view of the SortedSetDocValues, if it was
  /// previously wrapped with
  /// [`singleton_sorted`](DocValues::singleton_sorted), or None.
  pub fn unwrap_singleton_sorted<S>(dv: &mut S) -> Result<S::SortedDocValues>
  where
    S: SortedSetDocValues,
  {
    dv.get_sorted_doc_values()
  }

  /// Returns a single-valued view of the SortedNumericDocValues, if it was
  /// previously wrapped with
  /// [`singleton_numeric`](DocValues::singleton_numeric), or None.
  pub fn unwrap_singleton_numeric<SN>(dv: &mut SN) -> Result<SN::NumericDocValues>
  where
    SN: SortedNumericDocValues,
  {
    dv.get_numeric_doc_values()
  }
  /// Returns a multi-valued view over the provided NumericDocValues.
  pub fn singleton_numeric<N>(dv: N) -> Result<SingletonSortedNumericDocValues<N>>
  where
    N: NumericDocValues,
  {
    SingletonSortedNumericDocValues::new(dv)
  }

  fn check_field<LR>(reader: &LR, field: &str, expected: &[DocValuesType]) -> Result<()>
  where
    LR: LeafReader,
  {
    if let Some(fi) = reader.get_field_infos()?.field_info_by_name(field)? {
      let actual = *fi.get_doc_values_type();
      if !expected.contains(&actual) {
        let expected_str = if expected.len() == 1 {
          format!("={}", expected[0])
        } else {
          format!("one of {expected:?}")
        };
        return Err(LuceneError::illegal_state(format!(
          "unexpected docvalues type {actual} for field '{field}' (expected {expected_str}). Re-index with correct docvalues type."
        )));
      }
    }
    Ok(())
  }
  /// Returns [`NumericDocValues`] for the field, or [`Self::empty_numeric()`] if it has none.
  ///
  /// # Returns
  ///
  /// A [`NumericDocValues`] instance, or an empty instance if `field` does not exist in this reader.
  ///
  /// # Error
  ///
  /// - [`LuceneErrorKind::IllegalState`](crate::core::util::error::lucene_error::LuceneErrorKind::IllegalState) if `field` exists but was not indexed with doc values.
  /// - [`LuceneErrorKind::IllegalState`](crate::core::util::error::lucene_error::LuceneErrorKind::IllegalState) if `field` has doc values but the type is not [`DocValuesType::Numeric`].
  pub fn get_numeric<LR>(reader: &LR, field: &str) -> Result<Numeric<LR>>
  where
    LR: LeafReader,
  {
    match reader.get_numeric_doc_values(field)? {
      Some(dv) => Ok(NumericDocValuesWithEmpty::A(dv)),
      None => {
        Self::check_field(reader, field, &[DocValuesType::Numeric])?;
        Ok(NumericDocValuesWithEmpty::B(Self::empty_numeric()))
      },
    }
  }
  /// Returns [`BinaryDocValues`] for the field, or [`Self::empty_binary()`] if it has none.
  ///
  /// # Returns
  ///
  /// A [`BinaryDocValues`] instance, or an empty instance if `field` does not exist in this reader.
  ///
  /// # Error
  ///
  /// - [`LuceneErrorKind::IllegalState`](crate::core::util::error::lucene_error::LuceneErrorKind::IllegalState) if `field` exists but was not indexed with doc values.
  /// - [`LuceneErrorKind::IllegalState`](crate::core::util::error::lucene_error::LuceneErrorKind::IllegalState) if `field` has doc values but the type is not [`DocValuesType::Binary`].
  pub fn get_binary<LR>(reader: &LR, field: &str) -> Result<Binary<LR>>
  where
    LR: LeafReader,
  {
    match reader.get_binary_doc_values(field)? {
      Some(dv) => Ok(BinaryDocValuesWithEmpty::A(dv)),
      None => {
        Self::check_field(reader, field, &[DocValuesType::Binary])?;
        Ok(BinaryDocValuesWithEmpty::B(Self::empty_binary()))
      },
    }
  }
  /// Returns [`SortedDocValues`] for the field, or [`Self::empty_sorted()`] if it has none.
  ///
  /// # Returns
  ///
  /// A [`SortedDocValues`] instance, or an empty instance if `field` does not exist in this reader.
  ///
  /// # Error
  ///
  /// - [`LuceneErrorKind::IllegalState`](crate::core::util::error::lucene_error::LuceneErrorKind::IllegalState) if `field` exists but was not indexed with doc values.
  /// - [`LuceneErrorKind::IllegalState`](crate::core::util::error::lucene_error::LuceneErrorKind::IllegalState) if `field` has doc values but the type is not [`DocValuesType::Sorted`].
  pub fn get_sorted<LR>(reader: &LR, field: &str) -> Result<Sorted<LR>>
  where
    LR: LeafReader,
  {
    match reader.get_sorted_doc_values(field)? {
      Some(dv) => Ok(SortedDocValuesWithEmpty::A(dv)),
      None => {
        Self::check_field(reader, field, &[DocValuesType::Sorted])?;
        Ok(SortedDocValuesWithEmpty::B(Self::empty_sorted()))
      },
    }
  }
  /// Returns [`SortedNumericDocValues`] for the field, or [`Self::empty_sorted_numeric()`] if it has none.
  ///
  /// # Returns
  ///
  /// A [`SortedNumericDocValues`] instance, or an empty instance if `field` does not exist in this reader.
  ///
  /// # Error
  ///
  /// - [`LuceneErrorKind::IllegalState`](crate::core::util::error::lucene_error::LuceneErrorKind::IllegalState) if `field` exists but was not indexed with doc values.
  /// - [`LuceneErrorKind::IllegalState`](crate::core::util::error::lucene_error::LuceneErrorKind::IllegalState) if `field` has doc values but the type is not [`DocValuesType::SortedNumeric`] or [`DocValuesType::Numeric`].
  pub fn get_sorted_numeric<LR>(reader: &LR, field: &str) -> Result<SortedNumeric<LR>>
  where
    LR: LeafReader,
  {
    match reader.get_sorted_numeric_doc_values(field)? {
      Some(dv) => Ok(SortedNumericDocValuesEnum3WithEmpty::A(dv)),
      None => match reader.get_numeric_doc_values(field)? {
        Some(single) => Ok(SortedNumericDocValuesEnum3WithEmpty::B(
          Self::singleton_numeric(single)?,
        )),
        None => {
          Self::check_field(reader, field, &[DocValuesType::SortedNumeric])?;
          Ok(SortedNumericDocValuesEnum3WithEmpty::C(
            Self::empty_sorted_numeric()?,
          ))
        },
      },
    }
  }
  /// Returns [`SortedSetDocValues`] for the field, or [`Self::empty_sorted_set()`] if it has none.
  ///
  /// # Returns
  ///
  /// A [`SortedSetDocValues`] instance, or an empty instance if `field` does not exist in this reader.
  ///
  /// # Error
  ///
  /// - [`LuceneErrorKind::IllegalState`](crate::core::util::error::lucene_error::LuceneErrorKind::IllegalState) if `field` exists but was not indexed with doc values.
  /// - [`LuceneErrorKind::IllegalState`](crate::core::util::error::lucene_error::LuceneErrorKind::IllegalState) if `field` has doc values but the type is not [`DocValuesType::SortedSet`] or [`DocValuesType::Sorted`].
  pub fn get_sorted_set<LR>(reader: &LR, field: &str) -> Result<SortedSet<LR>>
  where
    LR: LeafReader,
  {
    match reader.get_sorted_set_doc_values(field)? {
      Some(dv) => Ok(SortedSetDocValuesEnum2::A(dv)),
      None => {
        let sorted = match reader.get_sorted_doc_values(field)? {
          Some(sorted) => SortedDocValuesWithEmpty::A(sorted),
          None => {
            Self::check_field(
              reader,
              field,
              &[DocValuesType::Sorted, DocValuesType::SortedSet],
            )?;
            SortedDocValuesWithEmpty::B(Self::empty_sorted())
          },
        };
        Ok(SortedSetDocValuesEnum2::B(Self::singleton_sorted(sorted)?))
      },
    }
  }

  /// Returns `true` if the specified docvalues fields have not been updated
  pub fn is_cacheable<LR, I, F>(ctx: &LeafReaderContext<LR>, fields: I) -> Result<bool>
  where
    LR: LeafReader,
    I: IntoIterator<Item = F>,
    F: AsRef<str>,
  {
    for field in fields {
      if let Some(fi) = ctx
        .reader()
        .get_field_infos()?
        .field_info_by_name(field.as_ref())?
        && fi.get_doc_values_gen() > -1
      {
        return Ok(false);
      }
    }
    Ok(true)
  }
}
pub type Numeric<LR> = NumericDocValuesWithEmpty<<LR as LeafReader>::NumericDocValues>;
pub type Binary<LR> = BinaryDocValuesWithEmpty<<LR as LeafReader>::BinaryDocValues>;
pub type Sorted<LR> = SortedDocValuesWithEmpty<<LR as LeafReader>::SortedDocValues>;
pub type SortedNumeric<LR> = SortedNumericDocValuesEnum3WithEmpty<
  <LR as LeafReader>::SortedNumericDocValues,
  SingletonSortedNumericDocValues<<LR as LeafReader>::NumericDocValues>,
>;
pub type SortedSet<LR> = SortedSetDocValuesEnum2<
  <LR as LeafReader>::SortedSetDocValues,
  SingletonSortedSetDocValues<SortedDocValuesWithEmpty<<LR as LeafReader>::SortedDocValues>>,
>;
/// An empty [`BinaryDocValues`] which returns no documents  */
pub struct EmptyBinary {
  doc: i32,
  bytes: BytesRef<Vec<u8>>,
}
impl Default for EmptyBinary {
  fn default() -> Self {
    Self::new()
  }
}
impl EmptyBinary {
  fn new() -> Self {
    Self {
      doc: -1,
      bytes: BytesRef::default(),
    }
  }
}

impl crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions for EmptyBinary {}
impl crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for EmptyBinary {}

impl DocIdSetIterator for EmptyBinary {
  fn doc_id(&self) -> i32 {
    self.doc
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.doc = NO_MORE_DOCS;
    Ok(self.doc)
  }

  fn advance(&mut self, _target: i32) -> Result<i32> {
    self.doc = NO_MORE_DOCS;
    Ok(self.doc)
  }

  fn cost(&self) -> Result<i64> {
    Ok(0)
  }
}

impl DocValuesIterator for EmptyBinary {
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.doc = target;
    Ok(false)
  }
}
impl BinaryDocValues for EmptyBinary {
  type Value<'a> = &'a BytesRef<Vec<u8>>;

  fn binary_value(&self) -> Result<Self::Value<'_>> {
    debug_assert!(
      false,
      "EmptyBinary::binary_value() should not be called, as it is an empty iterator"
    );
    Ok(&self.bytes)
  }
}

pub enum BinaryDocValuesWithEmpty<A> {
  A(A),
  B(EmptyBinary),
}

impl<A> DocValuesIterator for BinaryDocValuesWithEmpty<A>
where
  A: DocValuesIterator,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::A(inner) => inner.advance_exact(target),
      Self::B(inner) => inner.advance_exact(target),
    }
  }
}

impl<A> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for BinaryDocValuesWithEmpty<A>
where
  A: DocIdSetIterator,
{
}
impl<A> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for BinaryDocValuesWithEmpty<A>
where
  A: DocIdSetIterator,
{
}

impl<A> DocIdSetIterator for BinaryDocValuesWithEmpty<A>
where
  A: DocIdSetIterator,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::A(inner) => inner.doc_id(),
      Self::B(inner) => inner.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::A(inner) => inner.next_doc(),
      Self::B(inner) => inner.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::A(inner) => inner.advance(target),
      Self::B(inner) => inner.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::A(inner) => inner.slow_advance(target),
      Self::B(inner) => inner.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::A(inner) => inner.cost(),
      Self::B(inner) => inner.cost(),
    }
  }
}

impl<A> BinaryDocValues for BinaryDocValuesWithEmpty<A>
where
  A: BinaryDocValues,
{
  type Value<'a>
    = crate::core::index::BytesRefValueEnum2<A::Value<'a>, &'a BytesRef<Vec<u8>>>
  where
    Self: 'a;

  fn binary_value(&self) -> Result<Self::Value<'_>> {
    match self {
      Self::A(inner) => inner
        .binary_value()
        .map(crate::core::index::BytesRefValueEnum2::A),
      Self::B(inner) => inner
        .binary_value()
        .map(crate::core::index::BytesRefValueEnum2::B),
    }
  }
}
/// An empty [`NumericDocValues`] which returns no documents  */
pub struct EmptyNumeric {
  doc: i32,
}
impl Default for EmptyNumeric {
  fn default() -> Self {
    Self::new()
  }
}

impl EmptyNumeric {
  fn new() -> Self {
    Self { doc: -1 }
  }
}

impl DocValuesIterator for EmptyNumeric {
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.doc = target;
    Ok(false)
  }
}

impl crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions for EmptyNumeric {}
impl crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for EmptyNumeric {}

impl DocIdSetIterator for EmptyNumeric {
  fn doc_id(&self) -> i32 {
    self.doc
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.doc = NO_MORE_DOCS;
    Ok(self.doc)
  }

  fn advance(&mut self, _target: i32) -> Result<i32> {
    self.doc = NO_MORE_DOCS;
    Ok(self.doc)
  }

  fn cost(&self) -> Result<i64> {
    Ok(0)
  }
}

impl NumericDocValues for EmptyNumeric {
  fn long_value(&mut self) -> Result<i64> {
    debug_assert!(false);
    Ok(0)
  }
}

pub enum NumericDocValuesWithEmpty<A> {
  A(A),
  B(EmptyNumeric),
}

impl<A> DocValuesIterator for NumericDocValuesWithEmpty<A>
where
  A: DocValuesIterator,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::A(inner) => inner.advance_exact(target),
      Self::B(inner) => inner.advance_exact(target),
    }
  }
}

impl<A> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for NumericDocValuesWithEmpty<A>
where
  A: DocIdSetIterator,
{
}
impl<A> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for NumericDocValuesWithEmpty<A>
where
  A: DocIdSetIterator,
{
}

impl<A> DocIdSetIterator for NumericDocValuesWithEmpty<A>
where
  A: DocIdSetIterator,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::A(inner) => inner.doc_id(),
      Self::B(inner) => inner.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::A(inner) => inner.next_doc(),
      Self::B(inner) => inner.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::A(inner) => inner.advance(target),
      Self::B(inner) => inner.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::A(inner) => inner.slow_advance(target),
      Self::B(inner) => inner.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::A(inner) => inner.cost(),
      Self::B(inner) => inner.cost(),
    }
  }
}

impl<A> NumericDocValues for NumericDocValuesWithEmpty<A>
where
  A: NumericDocValues,
{
  fn long_value(&mut self) -> Result<i64> {
    match self {
      Self::A(inner) => inner.long_value(),
      Self::B(inner) => inner.long_value(),
    }
  }
}

pub enum NumericDocValuesEnum3WithEmpty<A, B> {
  A(A),
  B(B),
  C(EmptyNumeric),
}

impl<A, B> DocValuesIterator for NumericDocValuesEnum3WithEmpty<A, B>
where
  A: DocValuesIterator,
  B: DocValuesIterator,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::A(inner) => inner.advance_exact(target),
      Self::B(inner) => inner.advance_exact(target),
      Self::C(inner) => inner.advance_exact(target),
    }
  }
}

impl<A, B> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for NumericDocValuesEnum3WithEmpty<A, B>
where
  A: DocIdSetIterator,
  B: DocIdSetIterator,
{
}
impl<A, B> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for NumericDocValuesEnum3WithEmpty<A, B>
where
  A: DocIdSetIterator,
  B: DocIdSetIterator,
{
}

impl<A, B> DocIdSetIterator for NumericDocValuesEnum3WithEmpty<A, B>
where
  A: DocIdSetIterator,
  B: DocIdSetIterator,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::A(inner) => inner.doc_id(),
      Self::B(inner) => inner.doc_id(),
      Self::C(inner) => inner.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::A(inner) => inner.next_doc(),
      Self::B(inner) => inner.next_doc(),
      Self::C(inner) => inner.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::A(inner) => inner.advance(target),
      Self::B(inner) => inner.advance(target),
      Self::C(inner) => inner.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::A(inner) => inner.slow_advance(target),
      Self::B(inner) => inner.slow_advance(target),
      Self::C(inner) => inner.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::A(inner) => inner.cost(),
      Self::B(inner) => inner.cost(),
      Self::C(inner) => inner.cost(),
    }
  }
}

impl<A, B> NumericDocValues for NumericDocValuesEnum3WithEmpty<A, B>
where
  A: NumericDocValues,
  B: NumericDocValues,
{
  fn long_value(&mut self) -> Result<i64> {
    match self {
      Self::A(inner) => inner.long_value(),
      Self::B(inner) => inner.long_value(),
      Self::C(inner) => inner.long_value(),
    }
  }
}

pub enum SortedNumericDocValuesEnum3WithEmpty<A, B> {
  A(A),
  B(B),
  C(SingletonSortedNumericDocValues<EmptyNumeric>),
}

impl<A, B> DocValuesIterator for SortedNumericDocValuesEnum3WithEmpty<A, B>
where
  A: DocValuesIterator,
  B: DocValuesIterator,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::A(inner) => inner.advance_exact(target),
      Self::B(inner) => inner.advance_exact(target),
      Self::C(inner) => inner.advance_exact(target),
    }
  }
}

impl<A, B> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SortedNumericDocValuesEnum3WithEmpty<A, B>
where
  A: DocIdSetIterator,
  B: DocIdSetIterator,
{
}
impl<A, B> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for SortedNumericDocValuesEnum3WithEmpty<A, B>
where
  A: DocIdSetIterator,
  B: DocIdSetIterator,
{
}

impl<A, B> DocIdSetIterator for SortedNumericDocValuesEnum3WithEmpty<A, B>
where
  A: DocIdSetIterator,
  B: DocIdSetIterator,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::A(inner) => inner.doc_id(),
      Self::B(inner) => inner.doc_id(),
      Self::C(inner) => inner.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::A(inner) => inner.next_doc(),
      Self::B(inner) => inner.next_doc(),
      Self::C(inner) => inner.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::A(inner) => inner.advance(target),
      Self::B(inner) => inner.advance(target),
      Self::C(inner) => inner.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::A(inner) => inner.slow_advance(target),
      Self::B(inner) => inner.slow_advance(target),
      Self::C(inner) => inner.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::A(inner) => inner.cost(),
      Self::B(inner) => inner.cost(),
      Self::C(inner) => inner.cost(),
    }
  }
}

impl<A, B> SortedNumericDocValues for SortedNumericDocValuesEnum3WithEmpty<A, B>
where
  A: SortedNumericDocValues,
  B: SortedNumericDocValues,
{
  fn next_value(&mut self) -> Result<i64> {
    match self {
      Self::A(inner) => inner.next_value(),
      Self::B(inner) => inner.next_value(),
      Self::C(inner) => inner.next_value(),
    }
  }

  fn doc_value_count(&mut self) -> Result<i32> {
    match self {
      Self::A(inner) => inner.doc_value_count(),
      Self::B(inner) => inner.doc_value_count(),
      Self::C(inner) => inner.doc_value_count(),
    }
  }

  fn is_single_valued(&self) -> bool {
    match self {
      Self::A(inner) => inner.is_single_valued(),
      Self::B(inner) => inner.is_single_valued(),
      Self::C(inner) => inner.is_single_valued(),
    }
  }

  type NumericDocValues = NumericDocValuesEnum3WithEmpty<A::NumericDocValues, B::NumericDocValues>;

  fn get_numeric_doc_values(&mut self) -> Result<Self::NumericDocValues> {
    match self {
      Self::A(inner) => inner
        .get_numeric_doc_values()
        .map(NumericDocValuesEnum3WithEmpty::A),
      Self::B(inner) => inner
        .get_numeric_doc_values()
        .map(NumericDocValuesEnum3WithEmpty::B),
      Self::C(inner) => inner
        .get_numeric_doc_values()
        .map(NumericDocValuesEnum3WithEmpty::C),
    }
  }
}

pub enum SortedNumericDocValuesWithEmpty<A> {
  A(A),
  B(SingletonSortedNumericDocValues<EmptyNumeric>),
}

impl<A> DocValuesIterator for SortedNumericDocValuesWithEmpty<A>
where
  A: DocValuesIterator,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::A(inner) => inner.advance_exact(target),
      Self::B(inner) => inner.advance_exact(target),
    }
  }
}

impl<A> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SortedNumericDocValuesWithEmpty<A>
where
  A: DocIdSetIterator,
{
}
impl<A> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for SortedNumericDocValuesWithEmpty<A>
where
  A: DocIdSetIterator,
{
}

impl<A> DocIdSetIterator for SortedNumericDocValuesWithEmpty<A>
where
  A: DocIdSetIterator,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::A(inner) => inner.doc_id(),
      Self::B(inner) => inner.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::A(inner) => inner.next_doc(),
      Self::B(inner) => inner.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::A(inner) => inner.advance(target),
      Self::B(inner) => inner.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::A(inner) => inner.slow_advance(target),
      Self::B(inner) => inner.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::A(inner) => inner.cost(),
      Self::B(inner) => inner.cost(),
    }
  }
}

impl<A> SortedNumericDocValues for SortedNumericDocValuesWithEmpty<A>
where
  A: SortedNumericDocValues,
{
  fn next_value(&mut self) -> Result<i64> {
    match self {
      Self::A(inner) => inner.next_value(),
      Self::B(inner) => inner.next_value(),
    }
  }

  fn doc_value_count(&mut self) -> Result<i32> {
    match self {
      Self::A(inner) => inner.doc_value_count(),
      Self::B(inner) => inner.doc_value_count(),
    }
  }

  fn is_single_valued(&self) -> bool {
    match self {
      Self::A(inner) => inner.is_single_valued(),
      Self::B(inner) => inner.is_single_valued(),
    }
  }

  type NumericDocValues = NumericDocValuesWithEmpty<A::NumericDocValues>;

  fn get_numeric_doc_values(&mut self) -> Result<Self::NumericDocValues> {
    match self {
      Self::A(inner) => inner
        .get_numeric_doc_values()
        .map(NumericDocValuesWithEmpty::A),
      Self::B(inner) => inner
        .get_numeric_doc_values()
        .map(NumericDocValuesWithEmpty::B),
    }
  }
}

/// An empty SortedDocValues which returns empty [`BytesRef`] for every
/// document.
pub struct EmptySorted {
  doc: i32,
  empty: BytesRef<Vec<u8>>,
}

impl Default for EmptySorted {
  fn default() -> Self {
    Self::new()
  }
}

impl EmptySorted {
  fn new() -> Self {
    Self {
      doc: -1,
      empty: BytesRef::default(),
    }
  }
}

impl DocValuesIterator for EmptySorted {
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    self.doc = target;
    Ok(false)
  }
}

impl crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions for EmptySorted {}
impl crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for EmptySorted {}

impl DocIdSetIterator for EmptySorted {
  fn doc_id(&self) -> i32 {
    self.doc
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.doc = NO_MORE_DOCS;
    Ok(self.doc)
  }

  fn advance(&mut self, _target: i32) -> Result<i32> {
    self.doc = NO_MORE_DOCS;
    Ok(NO_MORE_DOCS)
  }

  fn cost(&self) -> Result<i64> {
    Ok(0)
  }
}

impl SortedDocValues for EmptySorted {
  type OrdValue<'a>
    = &'a BytesRef<Vec<u8>>
  where
    Self: 'a;

  fn ord_value(&mut self) -> Result<i32> {
    debug_assert!(
      false,
      "EmptySorted should not be called, as it is an empty iterator"
    );
    Ok(-1)
  }

  fn lookup_ord(&mut self, _ord: i32) -> Result<Self::OrdValue<'_>> {
    Ok(&self.empty)
  }

  fn get_value_count(&self) -> Result<i32> {
    Ok(0)
  }

  type TermsEnum<'a> = SortedDocValuesTermsEnum<&'a mut Self>;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    self.default_terms_enum()
  }
}

pub enum SortedDocValuesWithEmpty<A> {
  A(A),
  B(EmptySorted),
}

pub enum SortedDocValuesTermsEnumWithEmpty<'a, T> {
  WithPostingsAndAttributes(T),
  WithoutPostingsAndAttributes(SortedDocValuesTermsEnum<&'a mut EmptySorted>),
}

impl<'a, T> BytesRefIterator for SortedDocValuesTermsEnumWithEmpty<'a, T>
where
  T: TermsEnum,
{
  type Value<'b>
    = BytesRefValueEnum<'b>
  where
    Self: 'b;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms
        .next()
        .map(|value| value.map(BytesRefValue::into_value)),
      Self::WithoutPostingsAndAttributes(terms) => terms
        .next()
        .map(|value| value.map(BytesRefValue::into_value)),
    }
  }

  fn set_next(&mut self) -> Result<bool> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.set_next(),
      Self::WithoutPostingsAndAttributes(terms) => terms.set_next(),
    }
  }
}

impl<'a, T> TermsEnum for SortedDocValuesTermsEnumWithEmpty<'a, T>
where
  T: TermsEnum,
{
  type AttributeSource<'b>
    = T::AttributeSource<'b>
  where
    Self: 'b;
  type AttributeSourceMut<'b>
    = T::AttributeSourceMut<'b>
  where
    Self: 'b;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.attributes(),
      Self::WithoutPostingsAndAttributes(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.attributes_mut(),
      Self::WithoutPostingsAndAttributes(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.seek_exact(term),
      Self::WithoutPostingsAndAttributes(terms) => terms.seek_exact(term),
    }
  }

  fn prepare_seek_exact<BS: ByteSource>(&mut self, text: &BytesRef<BS>) -> Result<Option<()>> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.prepare_seek_exact(text),
      Self::WithoutPostingsAndAttributes(terms) => terms.prepare_seek_exact(text),
    }
  }

  fn get_prepare_seek_exact_status<BS: ByteSource>(
    &mut self,
    target: &BytesRef<BS>,
  ) -> Result<bool> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.get_prepare_seek_exact_status(target),
      Self::WithoutPostingsAndAttributes(terms) => terms.get_prepare_seek_exact_status(target),
    }
  }

  fn seek_ceil<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<SeekStatus> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.seek_ceil(term),
      Self::WithoutPostingsAndAttributes(terms) => terms.seek_ceil(term),
    }
  }

  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.seek_exact_with_ord(ord),
      Self::WithoutPostingsAndAttributes(terms) => terms.seek_exact_with_ord(ord),
    }
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    state: &TermStateEnum,
  ) -> Result<()> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.seek_exact_with_state(term, state),
      Self::WithoutPostingsAndAttributes(terms) => terms.seek_exact_with_state(term, state),
    }
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.term().map(BytesRefValue::into_value),
      Self::WithoutPostingsAndAttributes(terms) => terms.term().map(BytesRefValue::into_value),
    }
  }

  fn ord(&self) -> Result<i64> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.ord(),
      Self::WithoutPostingsAndAttributes(terms) => terms.ord(),
    }
  }

  fn doc_freq(&mut self) -> Result<i32> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.doc_freq(),
      Self::WithoutPostingsAndAttributes(terms) => terms.doc_freq(),
    }
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.total_term_freq(),
      Self::WithoutPostingsAndAttributes(terms) => terms.total_term_freq(),
    }
  }

  type PostingsEnum = T::PostingsEnum;

  fn postings(&mut self, reuse: Option<Self::PostingsEnum>) -> Result<Self::PostingsEnum> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.postings(reuse),
      Self::WithoutPostingsAndAttributes(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.postings_with_flags(reuse, flags),
      Self::WithoutPostingsAndAttributes(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  type ImpactsEnum = T::ImpactsEnum;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.impacts(flags),
      Self::WithoutPostingsAndAttributes(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    match self {
      Self::WithPostingsAndAttributes(terms) => terms.term_state(),
      Self::WithoutPostingsAndAttributes(terms) => terms.term_state(),
    }
  }
}

impl<A> DocValuesIterator for SortedDocValuesWithEmpty<A>
where
  A: DocValuesIterator,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::A(inner) => inner.advance_exact(target),
      Self::B(inner) => inner.advance_exact(target),
    }
  }
}

impl<A> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SortedDocValuesWithEmpty<A>
where
  A: DocIdSetIterator,
{
}
impl<A> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for SortedDocValuesWithEmpty<A>
where
  A: DocIdSetIterator,
{
}

impl<A> DocIdSetIterator for SortedDocValuesWithEmpty<A>
where
  A: DocIdSetIterator,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::A(inner) => inner.doc_id(),
      Self::B(inner) => inner.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::A(inner) => inner.next_doc(),
      Self::B(inner) => inner.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::A(inner) => inner.advance(target),
      Self::B(inner) => inner.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::A(inner) => inner.slow_advance(target),
      Self::B(inner) => inner.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::A(inner) => inner.cost(),
      Self::B(inner) => inner.cost(),
    }
  }
}

impl<A> SortedDocValues for SortedDocValuesWithEmpty<A>
where
  A: SortedDocValues,
{
  type OrdValue<'a>
    = BytesRefValueEnum<'a>
  where
    Self: 'a;

  fn ord_value(&mut self) -> Result<i32> {
    match self {
      Self::A(inner) => inner.ord_value(),
      Self::B(inner) => inner.ord_value(),
    }
  }

  fn lookup_ord(&mut self, ord: i32) -> Result<Self::OrdValue<'_>> {
    match self {
      Self::A(inner) => inner.lookup_ord(ord).map(BytesRefValue::into_value),
      Self::B(inner) => inner.lookup_ord(ord).map(BytesRefValue::into_value),
    }
  }

  fn get_value_count(&self) -> Result<i32> {
    match self {
      Self::A(inner) => inner.get_value_count(),
      Self::B(inner) => inner.get_value_count(),
    }
  }

  fn lookup_term<BS: ByteSource>(&mut self, key: &BytesRef<BS>) -> Result<i32> {
    match self {
      Self::A(inner) => inner.lookup_term(key),
      Self::B(inner) => inner.lookup_term(key),
    }
  }

  type TermsEnum<'a>
    = SortedDocValuesTermsEnumWithEmpty<'a, A::TermsEnum<'a>>
  where
    A: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    match self {
      Self::A(inner) => inner
        .terms_enum()
        .map(SortedDocValuesTermsEnumWithEmpty::WithPostingsAndAttributes),
      Self::B(inner) => inner
        .terms_enum()
        .map(SortedDocValuesTermsEnumWithEmpty::WithoutPostingsAndAttributes),
    }
  }
}
