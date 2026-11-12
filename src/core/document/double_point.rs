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
use crate::core::index::BinaryValueEnum;
use crate::core::index::BytesRef;
use crate::core::index::indexable_field::{
  IndexableField, IndexingTokenStream, ReusedIndexingTokenStream,
};
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::search::point_in_set_query::{PointInSetBase, PointInSetQuery};
#[cfg(debug_assertions)]
use crate::core::search::point_range_query::check_args;
use crate::core::search::point_range_query::{PointRangeBase, PointRangeQuery};
use crate::core::util::CoreHelper;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::number::Number;
use crate::core::util::numeric_utils::NumericUtils;
use std::borrow::Cow;
use std::fmt;
/// An indexed `f64` field for fast range filters. If you also need to store the value, you
/// should add a separate
/// [`StoredField`](crate::core::document::stored_field::StoredField) instance.
///
/// Finding all documents within an N-dimensional shape or range at search time is efficient.
/// Multiple values for the same field in one document is allowed.
///
/// This field defines static factory methods for creating common queries:
///
/// * [`new_exact_query`](Self::new_exact_query) for matching an exact 1D point.
/// * [`new_set_query`](Self::new_set_query) for matching a set of 1D values.
/// * [`new_range_query`](Self::new_range_query) for matching a 1D range.
/// * [`new_range_query`](Self::new_range_query) for matching points/ranges in
///   n-dimensional space.
///
/// See also [`PointValues`](crate::core::index::point_values::PointValues).
pub struct DoublePoint {
  parent_field: Field,
}
impl DoublePoint {
  /// Creates a new [`DoublePoint`], indexing the provided N-dimensional `f64` point.
  ///
  /// # Arguments
  ///
  /// * `name` - Field name.
  /// * `point` - Double point value.
  pub fn new<T, P>(name: T, point: P) -> Result<DoublePoint>
  where
    T: Into<String>,
    P: AsRef<[f64]>,
  {
    let point = point.as_ref();
    let value = Self::pack(point)?;
    let field_type = Self::get_type(point.len())?;
    let parent_field = Field::from_bytes_ref(name, value, field_type)?;
    Ok(DoublePoint { parent_field })
  }

  /// Return the least double that compares greater than `d` consistently with `f64` comparison.
  /// The only difference with [`f64::next_up`] is that this method returns `+0.0` when the argument
  /// is `-0.0`.
  pub fn next_up(d: f64) -> f64 {
    // -0.0d
    if d.to_bits() == 0x8000_0000_0000_0000u64 {
      0.0
    } else {
      d.next_up()
    }
  }

  /// Return the greatest double that compares less than `d` consistently with `f64` comparison.
  /// The only difference with [`f64::next_down`] is that this method returns `-0.0` when the
  /// argument is `+0.0`.
  pub fn next_down(d: f64) -> f64 {
    if d.to_bits() == 0u64 {
      -0.0
    } else {
      d.next_down()
    }
  }

  fn get_type(num_dims: usize) -> Result<FieldType> {
    let mut field_type = FieldType::new();
    field_type.set_dimensions(num_dims, BitUtil::DOUBLE_BYTES)?;
    field_type.freeze();
    Ok(field_type)
  }
  /// Change the values of this field
  pub fn set_double_values<P>(&mut self, point: P) -> Result<()>
  where
    P: AsRef<[f64]>,
  {
    let point = point.as_ref();
    if self.parent_field.field_type().point_dimension_count() != point.len() {
      return Err(LuceneError::illegal_argument(format!(
        "this field (name={}) uses {} dimensions; cannot change to (incoming) {} dimensions",
        self.parent_field.name(),
        self.parent_field.field_type().point_dimension_count(),
        point.len()
      )));
    }
    if let FieldDataEnum::Binary(value) = &mut self.parent_field.fields_data {
      // Dimensions are fixed, so the existing encoding has the required size.
      for (i, &dim) in point.iter().enumerate() {
        Self::encode_dimension(dim, &mut value.bytes, i * BitUtil::DOUBLE_BYTES);
      }
    } else {
      // take_binary_value() may have transferred the previous allocation.
      self.parent_field.fields_data = Self::pack(point)?.into();
    }
    Ok(())
  }

  /// Packs an `f64` point into a [`BytesRef`](crate::core::index::bytes_ref::BytesRef).
  ///
  /// # Arguments
  ///
  /// * `point` - Double point value.
  ///
  /// # Errors
  ///
  /// Returns an error if the value has zero dimensions.
  fn pack(point: &[f64]) -> Result<BytesRef<Vec<u8>>> {
    if point.is_empty() {
      return Err(LuceneError::illegal_argument(
        "point must not be 0 dimensions".to_string(),
      ));
    }
    let mut packed = Vec::with_capacity(point.len() * BitUtil::DOUBLE_BYTES);
    let mut encoded = [0; BitUtil::DOUBLE_BYTES];
    for &dim in point {
      Self::encode_dimension(dim, &mut encoded, 0);
      packed.extend_from_slice(&encoded);
    }
    Ok(BytesRef::from_bytes(packed))
  }
  /// Encode a single double dimension into byte array
  pub fn encode_dimension(value: f64, dest: &mut [u8], offset: usize) {
    NumericUtils::long_to_sortable_bytes(
      NumericUtils::double_to_sortable_long(value),
      dest,
      offset,
    );
  }

  /// Decode a single double dimension from byte array
  pub fn decode_dimension(value: &[u8], offset: usize) -> f64 {
    NumericUtils::sortable_long_to_double(NumericUtils::sortable_bytes_to_long(value, offset))
  }

  /// Create a query for matching an exact `f64` value.
  ///
  /// This is for simple one-dimension points. For multidimensional points, use
  /// [`new_range_query_n`](Self::new_range_query_n) instead.
  ///
  /// # Arguments
  ///
  /// * `field` - Field name.
  /// * `value` - Double value.
  pub fn new_exact_query<T>(field: T, value: f64) -> Result<PointRangeQuery>
  where
    T: Into<String>,
  {
    Self::new_range_query(field, value, value)
  }

  /// Create a range query for `f64` values.
  ///
  /// This is for simple one-dimension ranges. For multidimensional ranges, use
  /// [`new_range_query_n`](Self::new_range_query_n) instead.
  ///
  /// You can have half-open ranges (which are in fact `</<=` or `>/>=` queries) by setting
  /// `lower_value = f64::NEG_INFINITY` or `upper_value = f64::INFINITY`.
  ///
  /// Ranges are inclusive. For exclusive ranges, pass [`Self::next_up`] with the lower value or
  /// [`Self::next_down`] with the upper value.
  ///
  /// Range comparisons are consistent with `f64` comparison.
  ///
  /// # Arguments
  ///
  /// * `field` - Field name.
  /// * `lower_value` - Lower portion of the range (inclusive).
  /// * `upper_value` - Upper portion of the range (inclusive).
  pub fn new_range_query<T>(field: T, lower_value: f64, upper_value: f64) -> Result<PointRangeQuery>
  where
    T: Into<String>,
  {
    Self::new_range_query_n(field, [lower_value], [upper_value])
  }

  /// Create a query matching any of the specified 1D values. This is the points equivalent of
  /// [`TermInSetQuery`](crate::core::search::term_in_set_query::TermInSetQuery).
  ///
  /// # Arguments
  ///
  /// * `field` - Field name.
  /// * `values` - All values to match.
  ///
  /// Owned vectors are sorted in place; borrowed slices are copied before sorting.
  pub fn new_set_query<T, V>(field: T, values: V) -> Result<PointInSetQuery>
  where
    T: Into<String>,
    V: Into<Vec<f64>>,
  {
    let mut sorted_values = values.into();
    sorted_values.sort_unstable_by(|a, b| CoreHelper::compare_f64(*a, *b));

    PointInSetQuery::new(
      field.into(),
      1,
      BitUtil::DOUBLE_BYTES,
      DoublePointSetBytesRefIterator::new(sorted_values),
      DoublePointInSetQuery,
    )
  }

  /// Create a range query for n-dimensional `f64` values.
  ///
  /// You can have half-open ranges (which are in fact `</<=` or `>/>=` queries) by setting
  /// `lower_value[i] = f64::NEG_INFINITY` or `upper_value[i] = f64::INFINITY`.
  ///
  /// Ranges are inclusive. For exclusive ranges, pass `f64::next_up(lower_value[i])` or
  /// `f64::next_down(upper_value[i])`.
  ///
  /// Range comparisons are consistent with `f64` comparison.
  ///
  /// # Arguments
  ///
  /// * `field` - Field name.
  /// * `lower_value` - Lower portion of the range (inclusive).
  /// * `upper_value` - Upper portion of the range (inclusive).
  ///
  /// Lower and upper bounds may independently use arrays, vectors, or slices.
  pub fn new_range_query_n<T, L, U>(
    field: T,
    lower_value: L,
    upper_value: U,
  ) -> Result<PointRangeQuery>
  where
    T: Into<String>,
    L: AsRef<[f64]>,
    U: AsRef<[f64]>,
  {
    let field = field.into();
    let len = lower_value.as_ref().len();
    let mut lower_point = DoublePoint::pack(lower_value.as_ref())?;
    let mut upper_point = DoublePoint::pack(upper_value.as_ref())?;
    #[cfg(debug_assertions)]
    check_args(&field, &lower_point.bytes, &upper_point.bytes)?;

    PointRangeQuery::new(
      field,
      lower_point.take_bytes(),
      upper_point.take_bytes(),
      len,
      DoublePointRangeQuery,
    )
  }
}

struct DoublePointSetBytesRefIterator {
  sorted_values: Vec<f64>,
  upto: usize,
  encoded: BytesRef<[u8; BitUtil::DOUBLE_BYTES]>,
}

impl DoublePointSetBytesRefIterator {
  fn new(sorted_values: Vec<f64>) -> Self {
    Self {
      sorted_values,
      upto: 0,
      encoded: BytesRef {
        bytes: [0u8; BitUtil::DOUBLE_BYTES],
        offset: 0,
        length: BitUtil::DOUBLE_BYTES,
      },
    }
  }
}

impl BytesRefIterator for DoublePointSetBytesRefIterator {
  type Value<'a>
    = &'a BytesRef<[u8; BitUtil::DOUBLE_BYTES]>
  where
    Self: 'a;

  #[inline]
  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    if self.upto == self.sorted_values.len() {
      Ok(None)
    } else {
      DoublePoint::encode_dimension(self.sorted_values[self.upto], &mut self.encoded.bytes, 0);
      self.upto += 1;
      Ok(Some(&self.encoded))
    }
  }
}
impl FieldBase for DoublePoint {
  fn set_bytes_value<'a, B>(&mut self, _value: B) -> Result<()>
  where
    B: Into<BinaryValueEnum<'a>>,
  {
    Err(LuceneError::illegal_argument(
      "cannot change value type from double to BytesRef",
    ))
  }

  fn set_double_value(&mut self, value: f64) -> Result<()> {
    self.set_double_values([value])
  }
}
impl IndexableField for DoublePoint {
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
    if self.parent_field.field_type().point_dimension_count() != 1 {
      return Err(LuceneError::illegal_state(format!(
        "this field (name={}) uses {} dimensions; cannot convert to a single numeric value",
        self.parent_field.name(),
        self.parent_field.field_type().point_dimension_count()
      )));
    }
    match &self.parent_field.fields_data {
      FieldDataEnum::Binary(bytes) => {
        debug_assert!(bytes.length == BitUtil::DOUBLE_BYTES);
        let value = Self::decode_dimension(&bytes.bytes, bytes.offset);
        Ok(Some(Number::F64(value)))
      },
      _ => {
        debug_assert!(false, "no possible here");
        Ok(None)
      },
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
impl fmt::Display for DoublePoint {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "{} <{}:",
      std::any::type_name::<Self>(),
      self.parent_field.name()
    )?;

    match &self.parent_field.fields_data {
      FieldDataEnum::Binary(bytes) => {
        let dim_count = self.parent_field.field_type().point_dimension_count();
        for dim in 0..dim_count {
          if dim > 0 {
            write!(f, ",")?;
          }
          let value =
            Self::decode_dimension(&bytes.bytes, bytes.offset + dim * BitUtil::DOUBLE_BYTES);
          write!(f, "{value}")?;
        }
      },
      _ => {
        debug_assert!(false, "no possible here");
        write!(f, "Unsupported FieldDataEnum variant")?;
      },
    }

    write!(f, ">")
  }
}

#[derive(Debug, Clone)]
pub struct DoublePointRangeQuery;

impl PointRangeBase for DoublePointRangeQuery {
  fn to_string(&self, _dimension: usize, value: &[u8]) -> Result<String> {
    Ok(DoublePoint::decode_dimension(value, 0).to_string())
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DoublePointInSetQuery;

impl PointInSetBase for DoublePointInSetQuery {
  fn to_string(&self, value: &[u8]) -> Result<String> {
    debug_assert!(value.len() == BitUtil::DOUBLE_BYTES);
    Ok(DoublePoint::decode_dimension(value, 0).to_string())
  }
}
