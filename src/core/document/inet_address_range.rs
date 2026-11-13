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
use crate::core::document::inet_address_point;
use crate::core::document::invertable_field::InvertableType;
use crate::core::document::range_field_query::{QueryType, RangeFieldQuery, RangeFieldQueryBase};
use crate::core::index::BinaryValueEnum;
use crate::core::index::BytesRef;
use crate::core::index::indexable_field::{
  IndexableField, IndexingTokenStream, ReusedIndexingTokenStream,
};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::number::Number;
use std::borrow::Cow;
use std::fmt;
use std::net::IpAddr;

/// An indexed IP-address range field.
///
/// This field indexes an [`IpAddr`] range defined as a min/max pair. It is single
/// dimension only (indexed as two 16 byte paired values).
///
/// Multiple values are supported.
///
/// This field defines the following static factory methods for common search operations over Ip
/// Ranges:
///
/// - [`new_intersects_query`](Self::new_intersects_query) matches ip ranges that intersect the
///   defined search range.
/// - [`new_within_query`](Self::new_within_query) matches ip ranges that are within the defined
///   search range.
/// - [`new_contains_query`](Self::new_contains_query) matches ip ranges that contain the defined
///   search range.
/// - [`new_crosses_query`](Self::new_crosses_query) matches ip ranges that cross the defined search
///   range.
pub struct InetAddressRange {
  parent_field: Field,
}

impl InetAddressRange {
  /// The number of bytes per dimension: sync with InetAddressPoint.
  pub const BYTES: usize = 16;

  /// Create a new [`InetAddressRange`] from minimum and maximum values.
  ///
  /// # Arguments
  ///
  /// - `name` - Field name.
  /// - `min` - Range minimum value, represented as an [`IpAddr`].
  /// - `max` - Range maximum value, represented as an [`IpAddr`].
  pub fn new<T>(name: T, min: IpAddr, max: IpAddr) -> Result<Self>
  where
    T: Into<Cow<'static, str>>,
  {
    let mut parent_field = Field::new(name, Dummy(()), Self::get_type()?);
    Self::set_range_values_internal(&mut parent_field, min, max)?;
    Ok(Self { parent_field })
  }

  fn get_type() -> Result<FieldType> {
    let mut ft = FieldType::new();
    ft.set_dimensions(2, Self::BYTES)?;
    ft.freeze();
    Ok(ft)
  }
  /// Change (or set) the min/max values of the field.
  ///
  /// # Arguments
  ///
  /// - `min` - Range minimum value, represented as an [`IpAddr`].
  /// - `max` - Range maximum value, represented as an [`IpAddr`].
  pub fn set_range_values(&mut self, min: IpAddr, max: IpAddr) -> Result<()> {
    Self::set_range_values_internal(&mut self.parent_field, min, max)
  }

  fn set_range_values_internal(parent_field: &mut Field, min: IpAddr, max: IpAddr) -> Result<()> {
    let bytes = match &mut parent_field.fields_data {
      FieldDataEnum::Binary(b) => &mut b.bytes,
      FieldDataEnum::Dummy(_) => {
        let new_bytes = vec![0u8; Self::BYTES * 2];
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
    encode_range(min, max, bytes)
  }

  /// Create a query for matching indexed ip ranges that `INTERSECT` the defined range.
  ///
  /// # Arguments
  ///
  /// - `field` - Field name.
  /// - `min` - Range minimum value, provided as an [`IpAddr`].
  /// - `max` - Range maximum value, provided as an [`IpAddr`].
  ///
  /// # Returns
  ///
  /// Query for matching intersecting ranges (overlap, within, crosses, or contains).
  ///
  /// # Errors
  ///
  /// Returns an error if `min` or `max` is invalid.
  pub fn new_intersects_query<T>(field: T, min: IpAddr, max: IpAddr) -> Result<RangeFieldQuery>
  where
    T: Into<String>,
  {
    Self::new_relation_query(field, min, max, QueryType::Intersects)
  }

  /// Create a query for matching indexed ip ranges that `CONTAINS` the defined range.
  ///
  /// # Arguments
  ///
  /// - `field` - Field name.
  /// - `min` - Range minimum value, provided as an [`IpAddr`].
  /// - `max` - Range maximum value, provided as an [`IpAddr`].
  ///
  /// # Returns
  ///
  /// Query for matching intersecting ranges (overlap, within, crosses, or contains).
  ///
  /// # Errors
  ///
  /// Returns an error if `min` or `max` is invalid.
  pub fn new_contains_query<T>(field: T, min: IpAddr, max: IpAddr) -> Result<RangeFieldQuery>
  where
    T: Into<String>,
  {
    Self::new_relation_query(field, min, max, QueryType::Contains)
  }

  /// Create a query for matching indexed ip ranges that are `WITHIN` the defined range.
  ///
  /// # Arguments
  ///
  /// - `field` - Field name.
  /// - `min` - Range minimum value, provided as an [`IpAddr`].
  /// - `max` - Range maximum value, provided as an [`IpAddr`].
  ///
  /// # Returns
  ///
  /// Query for matching intersecting ranges (overlap, within, crosses, or contains).
  ///
  /// # Errors
  ///
  /// Returns an error if `min` or `max` is invalid.
  pub fn new_within_query<T>(field: T, min: IpAddr, max: IpAddr) -> Result<RangeFieldQuery>
  where
    T: Into<String>,
  {
    Self::new_relation_query(field, min, max, QueryType::Within)
  }
  /// Create a query for matching indexed ip ranges that `CROSS` the defined range.
  ///
  /// # Arguments
  ///
  /// - `field` - Field name.
  /// - `min` - Range minimum value, provided as an [`IpAddr`].
  /// - `max` - Range maximum value, provided as an [`IpAddr`].
  ///
  /// # Returns
  ///
  /// Query for matching intersecting ranges (overlap, within, crosses, or contains).
  ///
  /// # Errors
  ///
  /// Returns an error if `min` or `max` is invalid.
  pub fn new_crosses_query<T>(field: T, min: IpAddr, max: IpAddr) -> Result<RangeFieldQuery>
  where
    T: Into<String>,
  {
    Self::new_relation_query(field, min, max, QueryType::Crosses)
  }

  fn new_relation_query<T>(
    field: T,
    min: IpAddr,
    max: IpAddr,
    relation: QueryType,
  ) -> Result<RangeFieldQuery>
  where
    T: Into<String>,
  {
    RangeFieldQuery::new(
      field.into(),
      encode(min, max)?,
      1,
      relation,
      InetAddressRangeFieldQuery,
    )
  }
}

impl FieldBase for InetAddressRange {}

impl IndexableField for InetAddressRange {
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
      "cannot convert InetAddressRange to a single numeric value",
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

impl fmt::Display for InetAddressRange {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "InetAddressRange <{}:", self.parent_field.name())?;
    match &self.parent_field.fields_data {
      FieldDataEnum::Binary(bytes) => {
        write!(f, "{}", to_string(&bytes.bytes, 0))?;
      },
      _ => {
        write!(f, "Unsupported FieldDataEnum variant")?;
      },
    }
    write!(f, ">")
  }
}

fn encode_range(min: IpAddr, max: IpAddr, bytes: &mut [u8]) -> Result<()> {
  let min_encoded = inet_address_point::encode_address(min);
  let max_encoded = inet_address_point::encode_address(max);
  if min_encoded[..] > max_encoded[..] {
    return Err(LuceneError::illegal_argument(
      "min value cannot be greater than max value for InetAddressRange field",
    ));
  }
  bytes[..InetAddressRange::BYTES].copy_from_slice(&min_encoded);
  bytes[InetAddressRange::BYTES..InetAddressRange::BYTES * 2].copy_from_slice(&max_encoded);
  Ok(())
}

pub(crate) fn encode(min: IpAddr, max: IpAddr) -> Result<Vec<u8>> {
  let mut bytes = vec![0u8; InetAddressRange::BYTES * 2];
  encode_range(min, max, &mut bytes)?;
  Ok(bytes)
}

fn to_string(ranges: &[u8], _dimension: usize) -> String {
  let min = inet_address_point::decode_address(&ranges[..InetAddressRange::BYTES]);
  let max = inet_address_point::decode_address(
    &ranges[InetAddressRange::BYTES..InetAddressRange::BYTES * 2],
  );
  format!("[{} : {}]", min, max)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct InetAddressRangeFieldQuery;

impl RangeFieldQueryBase for InetAddressRangeFieldQuery {
  fn to_string(&self, value: &[u8], dimension: usize) -> Result<String> {
    Ok(to_string(value, dimension))
  }
}
