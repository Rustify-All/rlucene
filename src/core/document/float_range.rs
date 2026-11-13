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
use crate::core::document::field::FieldDataEnum::Dummy;
use crate::core::document::field::{Field, FieldBase, FieldDataEnum};
use crate::core::document::field_type::FieldType;
use crate::core::document::invertable_field::InvertableType;
use crate::core::document::range_field_query::{QueryType, RangeFieldQuery, RangeFieldQueryBase};
use crate::core::index::BinaryValueEnum;
use crate::core::index::BytesRef;
use crate::core::index::indexable_field::{
  IndexableField, IndexingTokenStream, ReusedIndexingTokenStream,
};
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::util::CoreHelper;
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::number::Number;
use crate::core::util::numeric_utils::NumericUtils;
use std::borrow::Cow;
use std::fmt;
/// An indexed Float Range field.
///
/// This field indexes dimensional ranges defined as min/max pairs. It supports up to a maximum of
/// 4 dimensions (indexed as 8 numeric values). With 1 dimension representing a single float range,
/// 2 dimensions representing a bounding box, 3 dimensions a bounding cube, and 4 dimensions a
/// tesseract.
///
/// Multiple values for the same field in one document is supported, and open ended ranges can be
/// defined using `f32::NEG_INFINITY` and `f32::INFINITY`.
///
/// This field defines the following static factory methods for common search operations over
/// float ranges:
///
/// - [`Self::new_intersects_query`] matches ranges that intersect the defined search range.
/// - [`Self::new_within_query`] matches ranges that are within the defined search range.
/// - [`Self::new_contains_query`] matches ranges that contain the defined search range.
pub struct FloatRange {
  parent_field: Field,
}
pub const BYTES: usize = std::mem::size_of::<f32>();

impl FloatRange {
  /// Lower and upper bounds may independently use arrays, vectors, or slices.
  pub fn new<T, L, U>(name: T, min: L, max: U) -> Result<Self>
  where
    T: Into<Cow<'static, str>>,
    L: AsRef<[f32]>,
    U: AsRef<[f32]>,
  {
    let min = min.as_ref();
    let field_type = Self::get_type(min.len())?;
    let mut parent_field = Field::new(name, Dummy(()), field_type);
    Self::set_range_values_internal(&mut parent_field, min, max.as_ref())?;
    Ok(FloatRange { parent_field })
  }

  fn get_type(dimensions: usize) -> Result<FieldType> {
    if dimensions > 4 {
      return Err(LuceneError::illegal_argument(
        "FloatRange does not support greater than 4 dimensions",
      ));
    }
    let mut ft = FieldType::new();
    ft.set_dimensions(dimensions * 2, BYTES)?;
    ft.freeze();
    Ok(ft)
  }

  /// Updates the range using independently supplied arrays, vectors, or slices.
  pub fn set_range_values<L, U>(&mut self, min: L, max: U) -> Result<()>
  where
    L: AsRef<[f32]>,
    U: AsRef<[f32]>,
  {
    Self::set_range_values_internal(&mut self.parent_field, min.as_ref(), max.as_ref())
  }

  fn set_range_values_internal(parent_field: &mut Field, min: &[f32], max: &[f32]) -> Result<()> {
    check_args(min, max)?;

    let dims = parent_field.field_type().point_dimension_count();
    if min.len() * 2 != dims || max.len() * 2 != dims {
      return Err(LuceneError::illegal_argument(format!(
        "field (name={}) uses {} dimensions; cannot change to (incoming) {} dimensions",
        parent_field.name(),
        dims / 2,
        min.len()
      )));
    }

    let bytes = match &mut parent_field.fields_data {
      FieldDataEnum::Binary(b) => &mut b.bytes,
      FieldDataEnum::Dummy(_) => {
        let new_bytes = vec![0u8; BYTES * 2 * min.len()];
        parent_field.fields_data = BytesRef::from_bytes(new_bytes).into();
        match &mut parent_field.fields_data {
          FieldDataEnum::Binary(b) => &mut b.bytes,
          _ => return Err(LuceneError::illegal_state("should not be here")),
        }
      },
      _ => Err(LuceneError::illegal_state(
        "Unsupported FieldDataEnum variant",
      ))?,
    };

    verify_and_encode(min, max, bytes)
  }

  pub fn get_min(&self, dimension: usize) -> Result<f32> {
    CoreHelper::check_index(
      dimension,
      self.parent_field.field_type().point_dimension_count() / 2,
    )?;
    match &self.parent_field.fields_data {
      FieldDataEnum::Binary(b) => Ok(decode_min(&b.bytes, dimension)),
      _ => Err(LuceneError::illegal_argument(
        "Unsupported FieldDataEnum variant",
      )),
    }
  }

  pub fn get_max(&self, dimension: usize) -> Result<f32> {
    CoreHelper::check_index(
      dimension,
      self.parent_field.field_type().point_dimension_count() / 2,
    )?;
    match &self.parent_field.fields_data {
      FieldDataEnum::Binary(b) => Ok(decode_max(&b.bytes, dimension)),
      _ => Err(LuceneError::illegal_argument(
        "Unsupported FieldDataEnum variant",
      )),
    }
  }

  /// Create a query for matching indexed ranges that intersect the defined range.
  ///
  /// # Parameters
  /// - `field`: field name.
  /// - `min`: array of min values. Accepts `f32::NEG_INFINITY`.
  /// - `max`: array of max values. Accepts `f32::MAX`.
  ///
  /// # Returns
  /// Query for matching intersecting ranges (overlap, within, or contains).
  ///
  /// # Errors
  /// Returns an error if `min` or `max` is invalid.
  pub fn new_intersects_query<T, L, U>(field: T, min: L, max: U) -> Result<RangeFieldQuery>
  where
    T: Into<String>,
    L: AsRef<[f32]>,
    U: AsRef<[f32]>,
  {
    Self::new_relation_query(field, min, max, QueryType::Intersects)
  }

  /// Create a query for matching indexed float ranges that contain the defined range.
  ///
  /// # Parameters
  /// - `field`: field name.
  /// - `min`: array of min values. Accepts `f32::NEG_INFINITY`.
  /// - `max`: array of max values. Accepts `f32::INFINITY`.
  ///
  /// # Returns
  /// Query for matching ranges that contain the defined range.
  ///
  /// # Errors
  /// Returns an error if `min` or `max` is invalid.
  pub fn new_contains_query<T, L, U>(field: T, min: L, max: U) -> Result<RangeFieldQuery>
  where
    T: Into<String>,
    L: AsRef<[f32]>,
    U: AsRef<[f32]>,
  {
    Self::new_relation_query(field, min, max, QueryType::Contains)
  }

  /// Create a query for matching indexed ranges that are within the defined range.
  ///
  /// # Parameters
  /// - `field`: field name.
  /// - `min`: array of min values. Accepts `f32::NEG_INFINITY`.
  /// - `max`: array of max values. Accepts `f32::INFINITY`.
  ///
  /// # Returns
  /// Query for matching ranges within the defined range.
  ///
  /// # Errors
  /// Returns an error if `min` or `max` is invalid.
  pub fn new_within_query<T, L, U>(field: T, min: L, max: U) -> Result<RangeFieldQuery>
  where
    T: Into<String>,
    L: AsRef<[f32]>,
    U: AsRef<[f32]>,
  {
    Self::new_relation_query(field, min, max, QueryType::Within)
  }

  /// Create a query for matching indexed ranges that cross the defined range. A cross relation is
  /// any set of ranges that are not disjoint and not wholly contained by the query. Effectively,
  /// it is the complement of union(WITHIN, DISJOINT).
  ///
  /// # Parameters
  /// - `field`: field name.
  /// - `min`: array of min values. Accepts `f32::NEG_INFINITY`.
  /// - `max`: array of max values. Accepts `f32::INFINITY`.
  ///
  /// # Returns
  /// Query for matching ranges that cross the defined range.
  ///
  /// # Errors
  /// Returns an error if `min` or `max` is invalid.
  pub fn new_crosses_query<T, L, U>(field: T, min: L, max: U) -> Result<RangeFieldQuery>
  where
    T: Into<String>,
    L: AsRef<[f32]>,
    U: AsRef<[f32]>,
  {
    Self::new_relation_query(field, min, max, QueryType::Crosses)
  }

  /// Helper method for creating the desired relational query.
  fn new_relation_query<T, L, U>(
    field: T,
    min: L,
    max: U,
    relation: QueryType,
  ) -> Result<RangeFieldQuery>
  where
    T: Into<String>,
    L: AsRef<[f32]>,
    U: AsRef<[f32]>,
  {
    let min = min.as_ref();
    let max = max.as_ref();
    check_args(min, max)?;
    RangeFieldQuery::new(
      field.into(),
      encode_range(min, max)?,
      min.len(),
      relation,
      FloatRangeFieldQuery,
    )
  }
}

impl FieldBase for FloatRange {}

impl IndexableField for FloatRange {
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
    Err(LuceneError::illegal_argument(
      "cannot convert FloatRange to a single numeric value",
    ))
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

impl fmt::Display for FloatRange {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "FloatRange <{}: ", self.parent_field.name())?;

    let dims = self.parent_field.field_type().point_dimension_count() / 2;

    match &self.parent_field.fields_data {
      FieldDataEnum::Binary(bytes) => {
        for dim in 0..dims {
          if dim > 0 {
            write!(f, " ")?;
          }
          let min = decode_min(&bytes.bytes, dim);
          let max = decode_max(&bytes.bytes, dim);
          write!(f, "[{} : {}]", min, max)?;
        }
      },
      _ => {
        write!(f, "Unsupported FieldDataEnum variant")?;
      },
    }

    write!(f, ">")
  }
}

/// Validates the arguments.
fn check_args(min: &[f32], max: &[f32]) -> Result<()> {
  if min.is_empty() || max.is_empty() {
    return Err(LuceneError::illegal_argument(
      "min/max range values cannot be null or empty",
    ));
  }
  if min.len() != max.len() {
    return Err(LuceneError::illegal_argument("min/max ranges must agree"));
  }
  if min.len() > 4 {
    return Err(LuceneError::illegal_argument(
      "FloatRange does not support greater than 4 dimensions",
    ));
  }
  Ok(())
}

/// Encodes the min, max ranges into a byte array.
pub(crate) fn encode_range(min: &[f32], max: &[f32]) -> Result<Vec<u8>> {
  check_args(min, max)?;
  let mut bytes = vec![0u8; BYTES * 2 * min.len()];
  verify_and_encode(min, max, &mut bytes)?;
  Ok(bytes)
}

/// Encodes the ranges into a sortable byte array (`f32::NAN` not allowed).
///
/// Example for 4 dimensions (4 bytes per dimension value):
/// minD1 ... minD4 | maxD1 ... maxD4
pub fn verify_and_encode(min: &[f32], max: &[f32], bytes: &mut [u8]) -> Result<()> {
  for d in 0..min.len() {
    let i = d * BYTES;
    let j = min.len() * BYTES + d * BYTES;

    if min[d].is_nan() {
      return Err(LuceneError::illegal_argument(
        "invalid min value (NaN) in FloatRange",
      ));
    }
    if max[d].is_nan() {
      return Err(LuceneError::illegal_argument(
        "invalid max value (NaN) in FloatRange",
      ));
    }
    if min[d] > max[d] {
      return Err(LuceneError::illegal_argument(format!(
        "min value ({}) is greater than max value ({})",
        min[d], max[d]
      )));
    }

    encode(min[d], bytes, i);
    encode(max[d], bytes, j);
  }

  Ok(())
}

/// Encodes the given value into the byte array at the defined offset.
fn encode(val: f32, bytes: &mut [u8], offset: usize) {
  NumericUtils::int_to_sortable_bytes(NumericUtils::float_to_sortable_int(val), bytes, offset);
}

fn decode_min(bytes: &[u8], dimension: usize) -> f32 {
  let offset = dimension * BYTES;
  NumericUtils::sortable_int_to_float(NumericUtils::sortable_bytes_to_int(bytes, offset))
}

fn decode_max(bytes: &[u8], dimension: usize) -> f32 {
  let offset = bytes.len() / 2 + dimension * BYTES;
  NumericUtils::sortable_int_to_float(NumericUtils::sortable_bytes_to_int(bytes, offset))
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FloatRangeFieldQuery;

impl RangeFieldQueryBase for FloatRangeFieldQuery {
  fn to_string(&self, value: &[u8], dimension: usize) -> Result<String> {
    Ok(to_string(value, dimension))
  }
}

fn to_string(ranges: &[u8], dimension: usize) -> String {
  format!(
    "[{} : {}]",
    decode_min(ranges, dimension),
    decode_max(ranges, dimension)
  )
}
