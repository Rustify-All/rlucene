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
use crate::core::analysis::analyzer::Analyzer;
use crate::core::analysis::reader::ReaderEnum;
use crate::core::document::field::{Field, FieldDataEnum};
use crate::core::document::field_type::FieldType;
use crate::core::document::invertable_field::InvertableType;
use crate::core::document::sorted_set_doc_values_range_query::SortedSetDocValuesRangeQuery;
use crate::core::index::BinaryValueEnum;
use crate::core::index::BytesRef;
use crate::core::index::doc_values_skip_index_type::DocValuesSkipIndexType;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::indexable_field::{
  IndexableField, IndexingTokenStream, ReusedIndexingTokenStream,
};
use crate::core::search::multi_term_query::{DOC_VALUES_REWRITE, MultiTermQuerySet};
use crate::core::search::term_in_set_query::TermInSetQuery;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::number::Number;
use std::borrow::Cow;
use std::fmt::{Display, Formatter};
use std::sync::LazyLock;

static TYPE: LazyLock<FieldType> = LazyLock::new(|| {
  let mut ft = FieldType::new();
  expect_invariant!(
    ft.set_doc_values_type(DocValuesType::SortedSet),
    "the built-in sorted-set doc-values field type is mutable and unfrozen"
  );
  ft.freeze();
  ft
});

static INDEXED_TYPE: LazyLock<FieldType> = LazyLock::new(|| {
  let mut ft = expect_invariant!(
    FieldType::from_ref(&*TYPE),
    "the built-in sorted-set doc-values field type can be copied"
  );
  expect_invariant!(
    ft.set_doc_values_skip_index_type(DocValuesSkipIndexType::Range),
    "the copied sorted-set doc-values field type is mutable and unfrozen"
  );
  ft.freeze();
  ft
});

/// Field that stores a set of per-document [`BytesRef`] values, indexed for
/// faceting, grouping, and joining.
///
/// Example usage:
///
/// ```text
/// document.add(SortedSetDocValuesField::new(name, BytesRef::from_string("hello")));
/// document.add(SortedSetDocValuesField::new(name, BytesRef::from_string("world")));
/// ```
///
/// If you also need to store the value, you should add a separate
/// [`StoredField`](crate::core::document::stored_field::StoredField) instance.
///
/// Each value can be at most **32766 bytes** long.
pub struct SortedSetDocValuesField {
  parent_field: Field,
}

impl SortedSetDocValuesField {
  /// Create a new sorted DocValues field.
  pub fn new<T, B>(name: T, bytes: B) -> Self
  where
    T: Into<Cow<'static, str>>,
    B: Into<BytesRef<Vec<u8>>>,
  {
    Self::with_type(name, bytes, TYPE.clone())
  }

  /// Creates a new [`SortedSetDocValuesField`] that also creates a
  /// doc-values skip index configured on its [`FieldType`].
  pub fn indexed_field<T, B>(name: T, bytes: B) -> Self
  where
    T: Into<Cow<'static, str>>,
    B: Into<BytesRef<Vec<u8>>>,
  {
    Self::with_type(name, bytes, INDEXED_TYPE.clone())
  }

  pub fn with_type<T, B>(name: T, bytes: B, field_type: FieldType) -> Self
  where
    T: Into<Cow<'static, str>>,
    B: Into<BytesRef<Vec<u8>>>,
  {
    let bytes = bytes.into();
    let parent_field = Field::new(name, bytes, field_type);
    Self { parent_field }
  }

  /// Create a range query that matches all documents whose value is between
  /// `lower_value` and `upper_value`.
  ///
  /// This query also works with fields that have indexed [`SortedDocValuesField`](crate::core::document::sorted_doc_values_field::SortedDocValuesField)s.
  ///
  /// **NOTE**: Such queries cannot efficiently advance to the next match,
  /// which makes them slow if they are not ANDed with a selective query. As a
  /// consequence, they are best used wrapped in an
  /// [`IndexOrDocValuesQuery`](crate::core::search::index_or_doc_values_query::IndexOrDocValuesQuery),
  /// alongside a range query that executes on points, such as
  /// [`BinaryPoint::new_range_query`](crate::core::document::binary_point::BinaryPoint::new_range_query).
  pub fn new_slow_range_query<T>(
    field: T,
    lower_value: Option<BytesRef<Vec<u8>>>,
    upper_value: Option<BytesRef<Vec<u8>>>,
    lower_inclusive: bool,
    upper_inclusive: bool,
  ) -> SortedSetDocValuesRangeQuery
  where
    T: Into<String>,
  {
    SortedSetDocValuesRangeQuery::new(
      field.into(),
      lower_value,
      upper_value,
      lower_inclusive,
      upper_inclusive,
    )
  }

  /// Create a query for matching an exact [`BytesRef`] value.
  ///
  /// This query also works with fields that have indexed [`SortedDocValuesField`](crate::core::document::sorted_doc_values_field::SortedDocValuesField)s.
  ///
  /// **NOTE**: Such queries cannot efficiently advance to the next match,
  /// which makes them slow if they are not ANDed with a selective query. As a
  /// consequence, they are best used wrapped in an
  /// [`IndexOrDocValuesQuery`](crate::core::search::index_or_doc_values_query::IndexOrDocValuesQuery),
  /// alongside a range query that executes on points, such as
  /// [`BinaryPoint::new_exact_query`](crate::core::document::binary_point::BinaryPoint::new_exact_query).
  pub fn new_slow_exact_query<T>(
    field: T,
    value: Option<BytesRef<Vec<u8>>>,
  ) -> SortedSetDocValuesRangeQuery
  where
    T: Into<String>,
  {
    SortedSetDocValuesRangeQuery::new(field.into(), value.clone(), value, true, true)
  }

  /// Create a query matching any of the specified values.
  ///
  /// This query also works with fields that have indexed [`SortedDocValuesField`](crate::core::document::sorted_doc_values_field::SortedDocValuesField)s.
  ///
  /// **NOTE**: Such queries cannot efficiently advance to the next match,
  /// which makes them slow if they are not ANDed with a selective query. As a
  /// consequence, they are best used wrapped in an
  /// [`IndexOrDocValuesQuery`](crate::core::search::index_or_doc_values_query::IndexOrDocValuesQuery),
  /// alongside a set query that executes on postings, such as [`TermInSetQuery`].
  ///
  /// Accepts arrays, vectors, or iterators of text, bytes, or `BytesRef` values.
  /// For an empty set, pass `std::iter::empty::<&str>()`.
  pub fn new_slow_set_query<T, I, B>(field: T, values: I) -> Result<MultiTermQuerySet>
  where
    T: Into<String>,
    I: IntoIterator<Item = B>,
    B: Into<BytesRef<Vec<u8>>>,
  {
    Ok(TermInSetQuery::new_with_rewrite_method(DOC_VALUES_REWRITE, field, values)?.into())
  }
}

impl Display for SortedSetDocValuesField {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    self.parent_field.fmt(f)
  }
}

impl IndexableField for SortedSetDocValuesField {
  fn name(&self) -> &str {
    self.parent_field.name()
  }

  type FieldType<'a>
    = &'a FieldType
  where
    Self: 'a;

  fn field_type(&self) -> Self::FieldType<'_> {
    self.parent_field.field_type()
  }
  fn token_stream<'a, A>(
    &'a mut self,
    analyzer: &'a A,
    reuse_token_stream: &'a mut Option<ReusedIndexingTokenStream>,
  ) -> Result<IndexingTokenStream<'a>>
  where
    A: Analyzer,
  {
    self.parent_field.token_stream(analyzer, reuse_token_stream)
  }

  fn binary_value(&self) -> Result<Option<BinaryValueEnum<'_>>> {
    self.parent_field.binary_value()
  }

  fn take_binary_value(&mut self) -> Result<Option<BytesRef<Vec<u8>>>> {
    self.parent_field.take_binary_value()
  }

  fn string_value(&self) -> Result<Option<Cow<'_, String>>> {
    self.parent_field.string_value()
  }

  fn take_string_value(&mut self) -> Result<Option<String>> {
    self.parent_field.take_string_value()
  }

  fn take_reader_value(&mut self) -> Result<Option<ReaderEnum>> {
    self.parent_field.take_reader_value()
  }

  fn numeric_value(&self) -> Result<Option<Number>> {
    self.parent_field.numeric_value()
  }

  type StoredValue<'a>
    = &'a FieldDataEnum
  where
    Self: 'a;

  fn stored_value(&self) -> Result<Option<Self::StoredValue<'_>>> {
    self.parent_field.stored_value()
  }

  fn invertable_type(&self) -> &InvertableType {
    self.parent_field.invertable_type()
  }
}
