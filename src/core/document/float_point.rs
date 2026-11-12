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

/// An indexed `f32` field for fast range filters. If you also need to store the value, you should
/// add a separate [`StoredField`](crate::core::document::stored_field::StoredField) instance.
///
/// Finding all documents within an N-dimensional shape or range at search time is efficient.
/// Multiple values for the same field in one document is allowed.
///
/// This field defines static factory methods for creating common queries:
///
/// * [`new_exact_query`](Self::new_exact_query) for matching an exact 1D point.
/// * [`new_set_query`](Self::new_set_query) for matching a set of 1D values.
/// * [`new_range_query`](Self::new_range_query) for matching a 1D range.
/// * [`new_range_query_n`](Self::new_range_query_n) for matching points/ranges in
///   n-dimensional space.
///
/// See also [`PointValues`](crate::core::index::point_values::PointValues).
pub struct FloatPoint {
  parent_field: Field,
}

impl FloatPoint {
  /// Creates a new [`FloatPoint`], indexing the provided N-dimensional `f32` point.
  ///
  /// # Arguments
  ///
  /// * `name` - Field name.
  /// * `point` - Float point value.
  pub fn new<T, P>(name: T, point: P) -> Result<FloatPoint>
  where
    T: Into<String>,
    P: AsRef<[f32]>,
  {
    let point = point.as_ref();
    let value = Self::pack(point)?;
    let field_type = Self::get_type(point.len())?;
    let parent_field = Field::from_bytes_ref(name, value, field_type)?;
    Ok(FloatPoint { parent_field })
  }

  /// Return the least float that compares greater than `f` consistently with `f32` comparison.
  /// The only difference with [`f32::next_up`] is that this method returns `+0.0` when the argument
  /// is `-0.0`.
  pub fn next_up(f: f32) -> f32 {
    if f.to_bits() == 0x8000_0000u32 {
      0.0
    } else {
      f.next_up()
    }
  }

  /// Return the greatest float that compares less than `f` consistently with `f32` comparison.
  /// The only difference with [`f32::next_down`] is that this method returns `-0.0` when the
  /// argument is `+0.0`.
  pub fn next_down(f: f32) -> f32 {
    if f.to_bits() == 0u32 {
      -0.0
    } else {
      f.next_down()
    }
  }

  fn get_type(num_dims: usize) -> Result<FieldType> {
    let mut field_type = FieldType::new();
    field_type.set_dimensions(num_dims, BitUtil::FLOAT_BYTES)?;
    field_type.freeze();
    Ok(field_type)
  }

  /// Change the values of this field
  pub fn set_float_values<P>(&mut self, point: P) -> Result<()>
  where
    P: AsRef<[f32]>,
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
        Self::encode_dimension(dim, &mut value.bytes, i * BitUtil::FLOAT_BYTES);
      }
    } else {
      // take_binary_value() may have transferred the previous allocation.
      self.parent_field.fields_data = Self::pack(point)?.into();
    }
    Ok(())
  }

  /// Packs an `f32` point into a [`BytesRef`].
  ///
  /// # Arguments
  ///
  /// * `point` - Float point value.
  ///
  /// # Errors
  ///
  /// Returns an error if the value has zero dimensions.
  pub fn pack<P>(point: P) -> Result<BytesRef<Vec<u8>>>
  where
    P: AsRef<[f32]>,
  {
    let point = point.as_ref();
    if point.is_empty() {
      return Err(LuceneError::illegal_argument(
        "point must not be 0 dimensions".to_string(),
      ));
    }
    let mut packed = Vec::with_capacity(point.len() * BitUtil::FLOAT_BYTES);
    let mut encoded = [0; BitUtil::FLOAT_BYTES];
    for &dim in point {
      Self::encode_dimension(dim, &mut encoded, 0);
      packed.extend_from_slice(&encoded);
    }
    Ok(BytesRef::from_bytes(packed))
  }

  /// Encode single float dimension
  pub fn encode_dimension(value: f32, dest: &mut [u8], offset: usize) {
    let sortable = NumericUtils::float_to_sortable_int(value);
    NumericUtils::int_to_sortable_bytes(sortable, dest, offset);
  }

  /// Decode single float dimension
  pub fn decode_dimension(value: &[u8], offset: usize) -> f32 {
    let int_val = NumericUtils::sortable_bytes_to_int(value, offset);
    NumericUtils::sortable_int_to_float(int_val)
  }

  /// Create a query for matching an exact `f32` value.
  ///
  /// This is for simple one-dimension points. For multidimensional points, use
  /// [`new_range_query_n`](Self::new_range_query_n) instead.
  ///
  /// # Arguments
  ///
  /// * `field` - Field name.
  /// * `value` - Float value.
  pub fn new_exact_query<T>(field: T, value: f32) -> Result<PointRangeQuery>
  where
    T: Into<String>,
  {
    Self::new_range_query(field, value, value)
  }

  /// Create a range query for `f32` values.
  ///
  /// This is for simple one-dimension ranges. For multidimensional ranges, use
  /// [`new_range_query_n`](Self::new_range_query_n) instead.
  ///
  /// You can have half-open ranges (which are in fact `</<=` or `>/>=` queries) by setting
  /// `lower_value = f32::NEG_INFINITY` or `upper_value = f32::INFINITY`.
  ///
  /// Ranges are inclusive. For exclusive ranges, pass [`Self::next_up`] with the lower value or
  /// [`Self::next_down`] with the upper value.
  ///
  /// Range comparisons are consistent with `f32` comparison.
  ///
  /// # Arguments
  ///
  /// * `field` - Field name.
  /// * `lower_value` - Lower portion of the range (inclusive).
  /// * `upper_value` - Upper portion of the range (inclusive).
  pub fn new_range_query<T>(field: T, lower_value: f32, upper_value: f32) -> Result<PointRangeQuery>
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
    V: Into<Vec<f32>>,
  {
    let mut sorted_values = values.into();
    sorted_values.sort_unstable_by(|a, b| CoreHelper::compare_f32(*a, *b));

    PointInSetQuery::new(
      field.into(),
      1,
      BitUtil::FLOAT_BYTES,
      FloatPointSetBytesRefIterator::new(sorted_values),
      FloatPointInSetQuery,
    )
  }

  /// Create a range query for n-dimensional `f32` values.
  ///
  /// You can have half-open ranges (which are in fact `</<=` or `>/>=` queries) by setting
  /// `lower_value[i] = f32::NEG_INFINITY` or `upper_value[i] = f32::INFINITY`.
  ///
  /// Ranges are inclusive. For exclusive ranges, pass `f32::next_up(lower_value[i])` or
  /// `f32::next_down(upper_value[i])`.
  ///
  /// Range comparisons are consistent with `f32` comparison.
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
    L: AsRef<[f32]>,
    U: AsRef<[f32]>,
  {
    let field = field.into();
    let len = lower_value.as_ref().len();
    let mut lower_point = FloatPoint::pack(lower_value.as_ref())?;
    let mut upper_point = FloatPoint::pack(upper_value.as_ref())?;

    #[cfg(debug_assertions)]
    check_args(&field, &lower_point.bytes, &upper_point.bytes)?;

    PointRangeQuery::new(
      field,
      lower_point.take_bytes(),
      upper_point.take_bytes(),
      len,
      FloatPointRangeQuery,
    )
  }
}

struct FloatPointSetBytesRefIterator {
  sorted_values: Vec<f32>,
  upto: usize,
  encoded: BytesRef<[u8; BitUtil::FLOAT_BYTES]>,
}

impl FloatPointSetBytesRefIterator {
  fn new(sorted_values: Vec<f32>) -> Self {
    Self {
      sorted_values,
      upto: 0,
      encoded: BytesRef {
        bytes: [0u8; BitUtil::FLOAT_BYTES],
        offset: 0,
        length: BitUtil::FLOAT_BYTES,
      },
    }
  }
}

impl BytesRefIterator for FloatPointSetBytesRefIterator {
  type Value<'a>
    = &'a BytesRef<[u8; BitUtil::FLOAT_BYTES]>
  where
    Self: 'a;

  #[inline]
  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    if self.upto == self.sorted_values.len() {
      Ok(None)
    } else {
      FloatPoint::encode_dimension(self.sorted_values[self.upto], &mut self.encoded.bytes, 0);
      self.upto += 1;
      Ok(Some(&self.encoded))
    }
  }
}

impl FieldBase for FloatPoint {
  fn set_bytes_value<'a, B>(&mut self, _value: B) -> Result<()>
  where
    B: Into<BinaryValueEnum<'a>>,
  {
    Err(LuceneError::illegal_argument(
      "cannot change value type from float to BytesRef".to_string(),
    ))
  }

  fn set_float_value(&mut self, value: f32) -> Result<()> {
    self.set_float_values([value])
  }
}

impl IndexableField for FloatPoint {
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
    todo!()
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
        debug_assert!(bytes.length == BitUtil::FLOAT_BYTES);
        let value = Self::decode_dimension(&bytes.bytes, bytes.offset);
        Ok(Some(Number::F32(value)))
      },
      _ => Err(LuceneError::illegal_argument(
        "Unsupported FieldDataEnum variant",
      )),
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

impl fmt::Display for FloatPoint {
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
            Self::decode_dimension(&bytes.bytes, bytes.offset + dim * BitUtil::FLOAT_BYTES);
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
pub struct FloatPointRangeQuery;

impl PointRangeBase for FloatPointRangeQuery {
  fn to_string(&self, _dimension: usize, value: &[u8]) -> Result<String> {
    Ok(FloatPoint::decode_dimension(value, 0).to_string())
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FloatPointInSetQuery;

impl PointInSetBase for FloatPointInSetQuery {
  fn to_string(&self, value: &[u8]) -> Result<String> {
    debug_assert!(value.len() == BitUtil::FLOAT_BYTES);
    Ok(FloatPoint::decode_dimension(value, 0).to_string())
  }
}
