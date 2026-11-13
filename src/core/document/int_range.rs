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
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::number::Number;
use crate::core::util::numeric_utils::NumericUtils;
use std::borrow::Cow;
use std::fmt;
/// An indexed Integer Range field.
///
/// This field indexes dimensional ranges defined as min/max pairs. It supports up to a maximum of
/// 4 dimensions (indexed as 8 numeric values). With 1 dimension representing a single integer range,
/// 2 dimensions representing a bounding box, 3 dimensions a bounding cube, and 4 dimensions a
/// tesseract.
///
/// Multiple values for the same field in one document is supported, and open ended ranges can be
/// defined using `i32::MIN` and `i32::MAX`.
///
/// This field defines the following static factory methods for common search operations over
/// integer ranges:
///
/// - [`IntRange::new_intersects_query`] matches ranges that intersect the defined search range.
/// - [`IntRange::new_within_query`] matches ranges that are within the defined search range.
/// - [`IntRange::new_contains_query`] matches ranges that contain the defined search range.
pub const BYTES: usize = std::mem::size_of::<i32>();
pub struct IntRange {
  parent_field: Field,
}

impl IntRange {
  /// Lower and upper bounds may independently use arrays, vectors, or slices.
  pub fn new<T, L, U>(name: T, min: L, max: U) -> Result<Self>
  where
    T: Into<Cow<'static, str>>,
    L: AsRef<[i32]>,
    U: AsRef<[i32]>,
  {
    let min = min.as_ref();
    let field_type = Self::get_type(min.len())?;
    let mut parent_field = Field::new(name, Dummy(()), field_type);
    Self::set_range_values_internal(&mut parent_field, min, max.as_ref())?;
    Ok(IntRange { parent_field })
  }

  fn get_type(dimensions: usize) -> Result<FieldType> {
    if dimensions > 4 {
      return Err(LuceneError::illegal_argument(
        "IntRange does not support greater than 4 dimensions".to_string(),
      ));
    }
    let mut ft = FieldType::new();
    ft.set_dimensions(dimensions * 2, BitUtil::INT_BYTES)?;
    ft.freeze();
    Ok(ft)
  }

  /// Updates the range using independently supplied arrays, vectors, or slices.
  pub fn set_range_values<L, U>(&mut self, min: L, max: U) -> Result<()>
  where
    L: AsRef<[i32]>,
    U: AsRef<[i32]>,
  {
    Self::set_range_values_internal(&mut self.parent_field, min.as_ref(), max.as_ref())
  }

  fn set_range_values_internal(parent_field: &mut Field, min: &[i32], max: &[i32]) -> Result<()> {
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
        let new_bytes = vec![0u8; BitUtil::INT_BYTES * 2 * min.len()];
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

  pub fn get_min(&self, dimension: usize) -> Result<i32> {
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

  pub fn get_max(&self, dimension: usize) -> Result<i32> {
    CoreHelper::check_index(
      dimension,
      self.parent_field.field_type().point_dimension_count() / 2,
    )?;
    match &self.parent_field.fields_data {
      FieldDataEnum::Binary(b) => Ok(decode_max(&b.bytes, dimension)),
      _ => Err(LuceneError::illegal_argument(
        "Unsupported FieldDataEnum variant".to_string(),
      )),
    }
  }

  /// Create a query for matching indexed ranges that intersect the defined range.
  ///
  /// # Parameters
  /// - `field`: field name.
  /// - `min`: array of min values. Accepts `i32::MIN`.
  /// - `max`: array of max values. Accepts `i32::MAX`.
  ///
  /// # Returns
  /// Query for matching intersecting ranges (overlap, within, or contains).
  ///
  /// # Errors
  /// Returns an error if `min` or `max` is invalid.
  pub fn new_intersects_query<T, L, U>(field: T, min: L, max: U) -> Result<RangeFieldQuery>
  where
    T: Into<String>,
    L: AsRef<[i32]>,
    U: AsRef<[i32]>,
  {
    Self::new_relation_query(field, min, max, QueryType::Intersects)
  }

  /// Create a query for matching indexed ranges that contain the defined range.
  ///
  /// # Parameters
  /// - `field`: field name.
  /// - `min`: array of min values. Accepts `i32::MIN`.
  /// - `max`: array of max values. Accepts `i32::MAX`.
  ///
  /// # Returns
  /// Query for matching ranges that contain the defined range.
  ///
  /// # Errors
  /// Returns an error if `min` or `max` is invalid.
  pub fn new_contains_query<T, L, U>(field: T, min: L, max: U) -> Result<RangeFieldQuery>
  where
    T: Into<String>,
    L: AsRef<[i32]>,
    U: AsRef<[i32]>,
  {
    Self::new_relation_query(field, min, max, QueryType::Contains)
  }

  /// Create a query for matching indexed ranges that are within the defined range.
  ///
  /// # Parameters
  /// - `field`: field name.
  /// - `min`: array of min values. Accepts `i32::MIN`.
  /// - `max`: array of max values. Accepts `i32::MAX`.
  ///
  /// # Returns
  /// Query for matching ranges within the defined range.
  ///
  /// # Errors
  /// Returns an error if `min` or `max` is invalid.
  pub fn new_within_query<T, L, U>(field: T, min: L, max: U) -> Result<RangeFieldQuery>
  where
    T: Into<String>,
    L: AsRef<[i32]>,
    U: AsRef<[i32]>,
  {
    Self::new_relation_query(field, min, max, QueryType::Within)
  }

  /// Create a query for matching indexed ranges that cross the defined range. A cross relation is
  /// any set of ranges that are not disjoint and not wholly contained by the query. Effectively,
  /// it is the complement of union(WITHIN, DISJOINT).
  ///
  /// # Parameters
  /// - `field`: field name.
  /// - `min`: array of min values. Accepts `i32::MIN`.
  /// - `max`: array of max values. Accepts `i32::MAX`.
  ///
  /// # Returns
  /// Query for matching ranges that cross the defined range.
  ///
  /// # Errors
  /// Returns an error if `min` or `max` is invalid.
  pub fn new_crosses_query<T, L, U>(field: T, min: L, max: U) -> Result<RangeFieldQuery>
  where
    T: Into<String>,
    L: AsRef<[i32]>,
    U: AsRef<[i32]>,
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
    L: AsRef<[i32]>,
    U: AsRef<[i32]>,
  {
    let min = min.as_ref();
    let max = max.as_ref();
    check_args(min, max)?;
    RangeFieldQuery::new(
      field.into(),
      encode(min, max)?,
      min.len(),
      relation,
      IntRangeFieldQuery,
    )
  }
}

impl FieldBase for IntRange {}

impl IndexableField for IntRange {
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

  fn string_value(&self) -> Result<Option<Cow<'_, str>>> {
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
      "cannot convert IntRange to a single numeric value".to_string(),
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

impl fmt::Display for IntRange {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "IntRange <{}: ", self.parent_field.name())?;

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

fn check_args(min: &[i32], max: &[i32]) -> Result<()> {
  if min.is_empty() || max.is_empty() {
    return Err(LuceneError::illegal_argument(
      "min/max range values cannot be null or empty".to_string(),
    ));
  }
  if min.len() != max.len() {
    return Err(LuceneError::illegal_argument(
      "min/max ranges must agree".to_string(),
    ));
  }
  if min.len() > 4 {
    return Err(LuceneError::illegal_argument(
      "IntRange does not support greater than 4 dimensions".to_string(),
    ));
  }
  Ok(())
}

pub fn encode(min: &[i32], max: &[i32]) -> Result<Vec<u8>> {
  check_args(min, max)?;
  let mut b = vec![0u8; BitUtil::INT_BYTES * 2 * min.len()];
  verify_and_encode(min, max, &mut b)?;
  Ok(b)
}

fn verify_and_encode(min: &[i32], max: &[i32], bytes: &mut [u8]) -> Result<()> {
  let n = min.len();
  let mut i = 0;
  let mut j = min.len() * BitUtil::INT_BYTES;
  for d in 0..n {
    if (min[d] as f64).is_nan() {
      return Err(LuceneError::illegal_argument(format!(
        "invalid min value ({}) in IntRange",
        f64::NAN
      )));
    }
    if (max[d] as f64).is_nan() {
      return Err(LuceneError::illegal_argument(format!(
        "invalid max value ({}) in IntRange",
        f64::NAN
      )));
    }
    if min[d] > max[d] {
      return Err(LuceneError::illegal_argument(format!(
        "min value ({}) is greater than max value ({})",
        min[d], max[d]
      )));
    }
    encode_dimension(min[d], bytes, i);
    encode_dimension(max[d], bytes, j);
    i += BitUtil::INT_BYTES;
    j += BitUtil::INT_BYTES;
  }
  Ok(())
}

fn encode_dimension(value: i32, dest: &mut [u8], offset: usize) {
  NumericUtils::int_to_sortable_bytes(value, dest, offset)
}

fn decode_min(bytes: &[u8], dimension: usize) -> i32 {
  let offset = dimension * BitUtil::INT_BYTES;
  NumericUtils::sortable_bytes_to_int(bytes, offset)
}

fn decode_max(bytes: &[u8], dimension: usize) -> i32 {
  let offset = bytes.len() / 2 + dimension * BitUtil::INT_BYTES;
  NumericUtils::sortable_bytes_to_int(bytes, offset)
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct IntRangeFieldQuery;
impl RangeFieldQueryBase for IntRangeFieldQuery {
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
