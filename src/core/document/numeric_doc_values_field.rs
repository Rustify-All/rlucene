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
use crate::core::document::field::{Field, FieldBase, FieldDataEnum};
use crate::core::document::field_type::FieldType;
use crate::core::document::invertable_field::InvertableType;
use crate::core::document::sorted_numeric_doc_values_range_query::SortedNumericDocValuesRangeQuery;
use crate::core::document::sorted_numeric_doc_values_set_query::SortedNumericDocValuesSetQuery;
use crate::core::index::BinaryValueEnum;
use crate::core::index::BytesRef;
use crate::core::index::doc_values_skip_index_type::DocValuesSkipIndexType;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::indexable_field::{
  IndexableField, IndexingTokenStream, ReusedIndexingTokenStream,
};
use crate::core::util::error::lucene_error::Result;
use crate::core::util::number::Number;
use std::borrow::Cow;
use std::fmt::{Display, Formatter};
use std::sync::LazyLock;

/// Type for numeric DocValues.
static TYPE: LazyLock<FieldType> = LazyLock::new(|| {
  let mut ft = FieldType::new();
  expect_invariant!(
    ft.set_doc_values_type(DocValuesType::Numeric),
    "the built-in numeric doc-values field type is mutable and unfrozen"
  );
  ft.freeze();
  ft
});
static INDEXED_TYPE: LazyLock<FieldType> = LazyLock::new(|| {
  let mut ft = expect_invariant!(
    FieldType::from_ref(&*TYPE),
    "the built-in numeric doc-values field type can be copied"
  );
  expect_invariant!(
    ft.set_doc_values_skip_index_type(DocValuesSkipIndexType::Range),
    "the copied numeric doc-values field type is mutable and unfrozen"
  );
  ft.freeze();
  ft
});

/// Field that stores a per-document `i64` value for scoring, sorting or value retrieval.
///
/// If you also need to store the value, you should add a separate
/// [`StoredField`](crate::core::document::stored_field::StoredField) instance.
pub struct NumericDocValuesField {
  pub(crate) parent_field: Field,
}
impl NumericDocValuesField {
  /// Creates a new [`NumericDocValuesField`] with the specified `i64` value that also
  /// creates a skip index.
  ///
  /// # Arguments
  ///
  /// * `name` - Field name.
  /// * `value` - An `i64` value.
  pub fn indexed_field<T>(name: T, value: i64) -> Self
  where
    T: Into<String>,
  {
    Self::with_type(name, value, INDEXED_TYPE.clone())
  }

  /// Creates a new doc-values field with the specified `i64` value.
  ///
  /// # Arguments
  ///
  /// * `name` - Field name.
  /// * `value` - An `i64` value.
  pub fn new<T>(name: T, value: i64) -> Self
  where
    T: Into<String>,
  {
    Self::with_type(name, value, TYPE.clone())
  }

  pub fn with_type<T>(name: T, value: i64, file_type: FieldType) -> Self
  where
    T: Into<String>,
  {
    let parent_field = Field::new(name, value, file_type);
    Self { parent_field }
  }
  /// Create a range query that matches all documents whose value is between `lower_value` and
  /// `upper_value` included.
  ///
  /// You can have half-open ranges (which are in fact `</<=` or `>/>=` queries) by setting
  /// `lower_value = i64::MIN` or `upper_value = i64::MAX`.
  ///
  /// Ranges are inclusive. For exclusive ranges, pass `lower_value + 1` or `upper_value - 1`.
  ///
  /// Note: such queries cannot efficiently advance to the next match, which makes them slow if
  /// they are not ANDed with a selective query. As a consequence, they are best used wrapped in an
  /// [`IndexOrDocValuesQuery`](crate::core::search::index_or_doc_values_query::IndexOrDocValuesQuery), alongside a range query that executes on points, such as
  /// [`LongPoint::new_range_query`](crate::core::document::long_point::LongPoint::new_range_query).
  pub fn new_slow_range_query<T>(
    field: T,
    lower_value: i64,
    upper_value: i64,
  ) -> SortedNumericDocValuesRangeQuery
  where
    T: Into<String>,
  {
    let field = field.into();
    SortedNumericDocValuesRangeQuery::new(field, lower_value, upper_value)
  }

  /// Create a query matching any of the specified values.
  ///
  /// Note: such queries cannot efficiently advance to the next match, which makes them slow if
  /// they are not ANDed with a selective query. As a consequence, they are best used wrapped in an
  /// [`IndexOrDocValuesQuery`](crate::core::search::index_or_doc_values_query::IndexOrDocValuesQuery), alongside a set query that executes on points, such as
  /// [`LongPoint::new_set_query`](crate::core::document::long_point::LongPoint::new_set_query).
  ///
  /// Accepts an owned vector, array, or borrowed slice of values.
  pub fn new_slow_set_query<T, V>(field: T, values: V) -> Result<SortedNumericDocValuesSetQuery>
  where
    T: Into<String>,
    V: Into<Vec<i64>>,
  {
    let field = field.into();
    SortedNumericDocValuesSetQuery::new(field, values.into())
  }
}

impl Display for NumericDocValuesField {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    self.parent_field.fmt(f)
  }
}

impl FieldBase for NumericDocValuesField {
  fn set_long_value(&mut self, value: i64) -> Result<()> {
    self.parent_field.set_long_value(value)
  }
}
impl IndexableField for NumericDocValuesField {
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

  #[inline]
  fn numeric_value(&self) -> Result<Option<Number>> {
    match &self.parent_field.fields_data {
      FieldDataEnum::Number(Number::I64(value)) => Ok(Some(Number::I64(*value))),
      _ => Ok(None),
    }
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
