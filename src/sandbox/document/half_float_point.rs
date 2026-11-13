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
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::number::Number;
use std::borrow::Cow;
use std::fmt;

/// An indexed `half-float` field for fast range filters.
///
/// The API takes `f32` values, but they will be encoded to half-floats before being indexed. In
/// case the provided floats cannot be represented accurately as a half float, they will be rounded
/// to the closest value that can be represented as a half float. In case of tie, values will be
/// rounded to the value that has a zero as its least significant bit.
///
/// Finding all documents within an N-dimensional point at search time is efficient. Multiple values
/// for the same field in one document is allowed.
///
/// This field defines static factory methods for creating common queries:
///
/// - [`new_exact_query`](Self::new_exact_query) for matching an exact 1D point.
/// - [`new_range_query`](Self::new_range_query) for matching a 1D range.
/// - [`new_range_query_n`](Self::new_range_query_n) for matching points/ranges in
///   n-dimensional space.
pub struct HalfFloatPoint {
  parent_field: Field,
}

impl HalfFloatPoint {
  /// The number of bytes used to represent a half-float value.
  pub const BYTES: usize = 2;

  /// Return the first half float which is immediately greater than `v`. If the argument is
  /// `f32::NAN` then the return value is `f32::NAN`. If the argument is `f32::INFINITY` then the
  /// return value is `f32::INFINITY`.
  pub fn next_up(v: f32) -> f32 {
    if v.is_nan() || v == f32::INFINITY {
      return v;
    }
    let s = Self::half_float_to_sortable_short(v);
    let mut r = Self::sortable_short_to_half_float(s);
    if r <= v {
      r = Self::sortable_short_to_half_float(s.wrapping_add(1));
    }
    r
  }

  /// Return the first half float which is immediately smaller than `v`. If the argument is
  /// `f32::NAN` then the return value is `f32::NAN`. If the argument is `f32::NEG_INFINITY` then
  /// the return value is `f32::NEG_INFINITY`.
  pub fn next_down(v: f32) -> f32 {
    if v.is_nan() || v == f32::NEG_INFINITY {
      return v;
    }
    let s = Self::half_float_to_sortable_short(v);
    let mut r = Self::sortable_short_to_half_float(s);
    if r >= v {
      r = Self::sortable_short_to_half_float(s.wrapping_sub(1));
    }
    r
  }

  /// Convert a half-float to an `i16` value that maintains ordering.
  pub fn half_float_to_sortable_short(v: f32) -> i16 {
    Self::sortable_short_bits(Self::half_float_to_short_bits(v))
  }

  /// Convert `i16` bits to a half-float value that maintains ordering.
  pub fn sortable_short_to_half_float(bits: i16) -> f32 {
    Self::short_bits_to_half_float(Self::sortable_short_bits(bits))
  }

  fn sortable_short_bits(s: i16) -> i16 {
    (s as u16 ^ (((s >> 15) as u16) & 0x7fff)) as i16
  }

  pub(crate) fn half_float_to_short_bits(v: f32) -> i16 {
    let float_bits = if v.is_nan() { 0x7fc00000 } else { v.to_bits() };
    let sign = float_bits >> 31;
    let mut exp = ((float_bits >> 23) & 0xff) as i32;
    let mut mantissa = (float_bits & 0x7fffff) as i32;

    if exp == 0xff {
      exp = 0x1f;
      mantissa >>= 23 - 10;
    } else if exp == 0x00 {
      mantissa = 0;
    } else {
      exp = exp - 127 + 15;
      if exp >= 0x1f {
        exp = 0x1f;
        mantissa = 0;
      } else if exp <= 0 {
        let shift = 23 - 10 - exp + 1;
        if shift >= 32 {
          exp = 0;
          mantissa = 0;
        } else {
          mantissa |= 0x800000;
          mantissa = Self::round_shift(mantissa, shift);
          exp = mantissa >> 10;
          mantissa &= 0x3ff;
        }
      } else {
        mantissa = Self::round_shift((exp << 23) | mantissa, 23 - 10);
        exp = mantissa >> 10;
        mantissa &= 0x3ff;
      }
    }
    (((sign << 15) | ((exp as u32) << 10) | mantissa as u32) & 0xffff) as u16 as i16
  }

  // divide by 2^shift and round to the closest int
  // round to even in case of tie
  pub(crate) fn round_shift(mut i: i32, shift: i32) -> i32 {
    debug_assert!(shift > 0);
    i += 1 << (shift - 1);
    i -= (i >> shift) & 1;
    ((i as u32) >> shift) as i32
  }

  pub(crate) fn short_bits_to_half_float(s: i16) -> f32 {
    let s = s as u16;
    let sign = (s >> 15) as u32;
    let mut exp = ((s >> 10) & 0x1f) as i32;
    let mut mantissa = (s & 0x3ff) as i32;
    if exp == 0x1f {
      exp = 0xff;
      mantissa <<= 23 - 10;
    } else if mantissa == 0 && exp == 0 {
    } else {
      if exp == 0 {
        let shift = mantissa.leading_zeros() as i32 - (32 - 11);
        mantissa = (mantissa << shift) & 0x3ff;
        exp = exp - shift + 1;
      }
      exp = exp + 127 - 15;
      mantissa <<= 23 - 10;
    }

    f32::from_bits((sign << 31) | ((exp as u32) << 23) | mantissa as u32)
  }

  pub(crate) fn short_to_sortable_bytes(value: i16, result: &mut [u8], offset: usize) {
    let value = (value as u16) ^ 0x8000;
    result[offset] = (value >> 8) as u8;
    result[offset + 1] = value as u8;
  }

  pub(crate) fn sortable_bytes_to_short(encoded: &[u8], offset: usize) -> i16 {
    let x = u16::from_be_bytes([encoded[offset], encoded[offset + 1]]);
    (x ^ 0x8000) as i16
  }

  fn get_type(num_dims: usize) -> Result<FieldType> {
    let mut ty = FieldType::new();
    ty.set_dimensions(num_dims, Self::BYTES)?;
    ty.freeze();
    Ok(ty)
  }

  /// Change the values of this field.
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
    self.parent_field.fields_data = FieldDataEnum::Binary(Self::pack(point)?);
    Ok(())
  }

  fn pack<P>(point: P) -> Result<BytesRef<Vec<u8>>>
  where
    P: AsRef<[f32]>,
  {
    let point = point.as_ref();
    if point.is_empty() {
      return Err(LuceneError::illegal_argument(
        "point must not be 0 dimensions".to_string(),
      ));
    }
    let mut packed = vec![0u8; point.len() * Self::BYTES];
    for (dim, value) in point.iter().enumerate() {
      Self::encode_dimension(*value, &mut packed, dim * Self::BYTES);
    }
    Ok(BytesRef::from_bytes(packed))
  }

  /// Creates a new [`HalfFloatPoint`], indexing the provided N-dimensional float point.
  ///
  /// # Arguments
  ///
  /// * `name` - Field name.
  /// * `point` - Float point value.
  pub fn new<T, P>(name: T, point: P) -> Result<Self>
  where
    T: Into<Cow<'static, str>>,
    P: AsRef<[f32]>,
  {
    let point = point.as_ref();
    let packed = Self::pack(point)?;
    let parent_field = Field::from_bytes_ref(name, packed, Self::get_type(point.len())?)?;
    Ok(Self { parent_field })
  }

  /// Encode single float dimension.
  pub fn encode_dimension(value: f32, dest: &mut [u8], offset: usize) {
    Self::short_to_sortable_bytes(Self::half_float_to_sortable_short(value), dest, offset);
  }

  /// Decode single float dimension.
  pub fn decode_dimension(value: &[u8], offset: usize) -> f32 {
    Self::sortable_short_to_half_float(Self::sortable_bytes_to_short(value, offset))
  }

  /// Create a query for matching an exact half-float value. It will be rounded to the closest
  /// half-float if `value` cannot be represented accurately as a half-float.
  ///
  /// This is for simple one-dimension points. For multidimensional points, use
  /// [`new_range_query_n`](Self::new_range_query_n) instead.
  ///
  /// # Arguments
  ///
  /// * `field` - Field name.
  /// * `value` - Half-float value.
  pub fn new_exact_query<T>(field: T, value: f32) -> Result<PointRangeQuery>
  where
    T: Into<String>,
  {
    Self::new_range_query(field, value, value)
  }

  /// Create a range query for half-float values. Bounds will be rounded to the closest half-float
  /// if they cannot be represented accurately as a half-float.
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

  /// Create a range query for n-dimensional half-float values. Bounds will be rounded to the
  /// closest half-float if they cannot be represented accurately as a half-float.
  ///
  /// You can have half-open ranges (which are in fact `</<=` or `>/>=` queries) by setting
  /// `lower_value[i] = f32::NEG_INFINITY` or `upper_value[i] = f32::INFINITY`.
  ///
  /// Ranges are inclusive. For exclusive ranges, pass [`Self::next_up`] with `lower_value[i]` or
  /// [`Self::next_down`] with `upper_value[i]`.
  ///
  /// Range comparisons are consistent with `f32` comparison.
  ///
  /// # Arguments
  ///
  /// * `field` - Field name.
  /// * `lower_value` - Lower portion of the range (inclusive).
  /// * `upper_value` - Upper portion of the range (inclusive).
  pub fn new_range_query_n<T, LV, UV>(
    field: T,
    lower_value: LV,
    upper_value: UV,
  ) -> Result<PointRangeQuery>
  where
    T: Into<String>,
    LV: AsRef<[f32]>,
    UV: AsRef<[f32]>,
  {
    let field = field.into();
    let lower_value = lower_value.as_ref();
    let upper_value = upper_value.as_ref();
    if lower_value.len() != upper_value.len() {
      return Err(LuceneError::illegal_argument(
        "lowerValue.length != upperValue.length".to_string(),
      ));
    }

    let mut lower_point = Self::pack(lower_value)?;
    let mut upper_point = Self::pack(upper_value)?;
    #[cfg(debug_assertions)]
    check_args(&field, &lower_point.bytes, &upper_point.bytes)?;
    PointRangeQuery::new(
      field,
      lower_point.take_bytes(),
      upper_point.take_bytes(),
      lower_value.len(),
      HalfFloatPointRangeQuery,
    )
  }

  /// Create a query matching any of the specified 1D values. This is the points equivalent of
  /// [`TermInSetQuery`](crate::core::search::term_in_set_query::TermInSetQuery). Values will be
  /// rounded to the closest half-float if they cannot be represented
  /// accurately as a half-float.
  ///
  /// # Arguments
  ///
  /// * `field` - Field name.
  /// * `values` - All values to match.
  pub fn new_set_query<T, V>(field: T, values: V) -> Result<PointInSetQuery>
  where
    T: Into<String>,
    V: Into<Vec<f32>>,
  {
    let mut sorted_values = values.into();
    sorted_values.sort_by(|a, b| CoreHelper::compare_f32(*a, *b));

    PointInSetQuery::new(
      field.into(),
      1,
      Self::BYTES,
      HalfFloatPointSetBytesRefIterator::new(sorted_values),
      HalfFloatPointInSetQuery,
    )
  }
}

struct HalfFloatPointSetBytesRefIterator {
  sorted_values: Vec<f32>,
  upto: usize,
  encoded: BytesRef<Vec<u8>>,
}

impl HalfFloatPointSetBytesRefIterator {
  fn new(sorted_values: Vec<f32>) -> Self {
    Self {
      sorted_values,
      upto: 0,
      encoded: BytesRef::from_bytes(vec![0u8; HalfFloatPoint::BYTES]),
    }
  }
}

impl BytesRefIterator for HalfFloatPointSetBytesRefIterator {
  type Value<'a>
    = &'a BytesRef<Vec<u8>>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    if self.upto == self.sorted_values.len() {
      Ok(None)
    } else {
      HalfFloatPoint::encode_dimension(self.sorted_values[self.upto], &mut self.encoded.bytes, 0);
      self.upto += 1;
      Ok(Some(&self.encoded))
    }
  }
}

impl FieldBase for HalfFloatPoint {
  fn set_bytes_value<'a, B>(&mut self, _value: B) -> Result<()>
  where
    B: Into<BinaryValueEnum<'a>>,
  {
    Err(LuceneError::illegal_argument(
      "cannot change value type from float to BytesRef",
    ))
  }

  fn set_float_value(&mut self, value: f32) -> Result<()> {
    self.set_float_values([value])
  }
}

impl IndexableField for HalfFloatPoint {
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
        debug_assert!(bytes.length == Self::BYTES);
        Ok(Some(Number::F32(Self::decode_dimension(
          &bytes.bytes,
          bytes.offset,
        ))))
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

impl fmt::Display for HalfFloatPoint {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "HalfFloatPoint <{}:", self.parent_field.name())?;
    let bytes = match &self.parent_field.fields_data {
      FieldDataEnum::Binary(bytes) => bytes,
      _ => {
        debug_assert!(false, "HalfFloatPoint fieldsData must be BytesRef");
        return Err(fmt::Error);
      },
    };
    for dim in 0..self.parent_field.field_type().point_dimension_count() {
      if dim > 0 {
        write!(f, ",")?;
      }
      let value = Self::decode_dimension(&bytes.bytes, bytes.offset + dim * Self::BYTES);
      write!(f, "{value}")?;
    }
    write!(f, ">")
  }
}

#[derive(Debug, Clone)]
pub struct HalfFloatPointRangeQuery;

impl PointRangeBase for HalfFloatPointRangeQuery {
  fn to_string(&self, _dimension: usize, value: &[u8]) -> Result<String> {
    Ok(HalfFloatPoint::decode_dimension(value, 0).to_string())
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HalfFloatPointInSetQuery;

impl PointInSetBase for HalfFloatPointInSetQuery {
  fn to_string(&self, value: &[u8]) -> Result<String> {
    Ok(HalfFloatPoint::decode_dimension(value, 0).to_string())
  }
}
