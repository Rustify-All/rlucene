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
use crate::core::document::field::{Field, FieldBase, FieldDataEnum, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::invertable_field::InvertableType;
use crate::core::document::long_point::LongPoint;

use crate::core::document::sorted_numeric_doc_values_field::SortedNumericDocValuesField;
use crate::core::index::BinaryValueEnum;
use crate::core::index::BytesRef;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::indexable_field::{
  IndexableField, IndexingTokenStream, ReusedIndexingTokenStream,
};
use crate::core::search::index_or_doc_values_query::IndexOrDocValuesQuery;
use crate::core::search::index_sort_sorted_numeric_doc_values_range_query::IndexSortSortedNumericDocValuesRangeQuery;
use crate::core::search::sort_field::SortFieldType;
use crate::core::search::sorted_numeric_selector::SortedNumericSelectorType;
use crate::core::search::sorted_numeric_sort_field::SortedNumericSortField;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::number::Number;
use crate::core::util::numeric_utils::NumericUtils;
use std::borrow::Cow;
use std::fmt;
use std::sync::LazyLock;

/// Indexed as SortedNumeric DocValue, not stored.
pub static FIELD_TYPE: LazyLock<FieldType> = LazyLock::new(|| {
  let mut ft = FieldType::new();
  expect_invariant!(
    ft.set_dimensions(1, BitUtil::LONG_BYTES),
    "the built-in long field dimensions are valid"
  );
  expect_invariant!(
    ft.set_doc_values_type(DocValuesType::SortedNumeric),
    "the built-in long field type is mutable and unfrozen"
  );
  ft.freeze();
  ft
});
/// Indexed as SortedNumeric DocValue, and stored.
pub static FIELD_TYPE_STORED: LazyLock<FieldType> = LazyLock::new(|| {
  let mut ft = expect_invariant!(
    FieldType::from_ref(&*FIELD_TYPE),
    "the built-in long field type can be copied"
  );
  expect_invariant!(
    ft.set_stored(true),
    "the copied long field type is mutable and unfrozen"
  );
  ft.freeze();
  ft
});

/// Create a new sort field for `i64` values.
///
/// # Arguments
///
/// * `field` - Field name.
/// * `reverse` - `true` if natural order should be reversed.
/// * `selector` - Custom selector type for choosing the sort value from the set.
pub fn new_sort_field<S>(
  field: S,
  reverse: bool,
  selector: SortedNumericSelectorType,
) -> Result<SortedNumericSortField>
where
  S: Into<String>,
{
  SortedNumericSortField::with_selector(field, SortFieldType::Long, reverse, selector)
}

/// Field that stores a per-document `i64` value for scoring, sorting or value retrieval and
/// indexes the field for fast range filters. If you need more fine-grained control, use
/// [`LongPoint`], [`NumericDocValuesField`](crate::core::document::numeric_doc_values_field::NumericDocValuesField) or [`SortedNumericDocValuesField`], and [`StoredField`](crate::core::document::stored_field::StoredField).
///
/// This field defines static factory methods for creating common queries:
///
/// * [`new_exact_query`](Self::new_exact_query) for matching an exact 1D point.
/// * [`new_range_query`](Self::new_range_query) for matching a 1D range.
/// * [`new_set_query`](Self::new_set_query) for matching a 1D set.
///
/// See also [`PointValues`](crate::core::index::point_values::PointValues).
pub struct LongField {
  parent_field: Field,
  stored_value: Option<FieldDataEnum>,
}

impl LongField {
  /// Creates a new [`LongField`], indexing the provided value,
  /// storing it as a DocValue, and optionally as a stored field.
  ///
  /// # Arguments
  ///
  /// * `name` - Field name.
  /// * `value` - The `i64` value.
  /// * `stored` - Whether to store the field.
  pub fn new<T>(name: T, value: i64, stored: Store) -> Result<LongField>
  where
    T: Into<Cow<'static, str>>,
  {
    let stored = stored.into();
    let (field_type, stored_value) = if stored {
      (FIELD_TYPE_STORED.clone(), Some(value.into()))
    } else {
      (FIELD_TYPE.clone(), None)
    };
    let parent_field = Field::new(name, value, field_type);
    Ok(LongField {
      parent_field,
      stored_value,
    })
  }

  /// Create a query for matching an exact `i64` value.
  ///
  /// # Arguments
  ///
  /// * `field` - Field name.
  /// * `value` - Exact value.
  pub fn new_exact_query<T>(
    field: T,
    value: i64,
  ) -> Result<IndexSortSortedNumericDocValuesRangeQuery>
  where
    T: Into<String>,
  {
    Self::new_range_query(field, value, value)
  }

  /// Create a range query for `i64` values.
  ///
  /// You can have half-open ranges (which are in fact `</<=` or `>/>=` queries) by setting
  /// `lower_value = i64::MIN` or `upper_value = i64::MAX`.
  ///
  /// Ranges are inclusive. For exclusive ranges, pass `lower_value + 1` or `upper_value - 1`.
  ///
  /// # Arguments
  ///
  /// * `field` - Field name.
  /// * `lower_value` - Lower portion of the range (inclusive).
  /// * `upper_value` - Upper portion of the range (inclusive).
  pub fn new_range_query<T>(
    field: T,
    lower_value: i64,
    upper_value: i64,
  ) -> Result<IndexSortSortedNumericDocValuesRangeQuery>
  where
    T: Into<String>,
  {
    let field = field.into();
    let fallback_query = IndexOrDocValuesQuery::new(
      LongPoint::new_range_query(field.clone(), lower_value, upper_value)?,
      SortedNumericDocValuesField::new_slow_range_query(field.clone(), lower_value, upper_value),
    );

    Ok(IndexSortSortedNumericDocValuesRangeQuery::new(
      field,
      lower_value,
      upper_value,
      fallback_query,
    ))
  }

  /// Create a query matching values in a supplied set.
  ///
  /// Accepts a vector, array or slice. Owned vectors are moved; borrowed slices are copied.
  ///
  /// # Arguments
  ///
  /// * `field` - Field name.
  /// * `values` - Long values.
  pub fn new_set_query<T, V>(field: T, values: V) -> Result<IndexOrDocValuesQuery>
  where
    T: Into<String>,
    V: Into<Vec<i64>>,
  {
    let values = values.into();
    let field = field.into();
    let point_query = LongPoint::new_set_query(field.clone(), values.as_slice())?;
    let dv_query = SortedNumericDocValuesField::new_slow_set_query(field, values)?;
    Ok(IndexOrDocValuesQuery::new(point_query, dv_query))
  }
}

impl FieldBase for LongField {
  fn set_long_value(&mut self, value: i64) -> Result<()> {
    self.parent_field.set_long_value(value)?;
    if self.stored_value.is_some() {
      self.stored_value = Some(value.into());
    }
    Ok(())
  }
}

impl IndexableField for LongField {
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
    match &self.parent_field.fields_data {
      FieldDataEnum::Number(Number::I64(v)) => {
        let mut bytes = [0u8; BitUtil::LONG_BYTES];
        NumericUtils::long_to_sortable_bytes(*v, &mut bytes, 0);
        Ok(Some(BinaryValueEnum::Inline {
          bytes,
          length: BitUtil::LONG_BYTES,
        }))
      },
      _ => Err(LuceneError::illegal_state(
        "parent_field's fields_data does not contain an i64 value",
      )),
    }
  }

  fn take_binary_value(&mut self) -> Result<Option<BytesRef<Vec<u8>>>> {
    // Keep the direct allocation path for callers requiring Vec-backed storage.
    match &self.parent_field.fields_data {
      FieldDataEnum::Number(Number::I64(v)) => {
        let mut bytes = vec![0u8; BitUtil::LONG_BYTES];
        NumericUtils::long_to_sortable_bytes(*v, &mut bytes, 0);
        Ok(Some(BytesRef::from_bytes(bytes)))
      },
      _ => Err(LuceneError::illegal_state(
        "parent_field's fields_data does not contain an i64 value",
      )),
    }
  }

  fn string_value(&self) -> Result<Option<Cow<'_, str>>> {
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
    Ok(self.stored_value.as_ref())
  }

  fn invertable_type(&self) -> &InvertableType {
    self.parent_field.invertable_type()
  }
}

impl fmt::Display for LongField {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "{} <{}:{}>",
      std::any::type_name::<Self>(),
      self.parent_field.name(),
      self.parent_field.fields_data
    )
  }
}
