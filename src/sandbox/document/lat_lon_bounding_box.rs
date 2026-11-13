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
use crate::core::document::lat_lon_point::LatLonPoint;
use crate::core::document::range_field_query::{QueryType, RangeFieldQuery, RangeFieldQueryBase};
use crate::core::geo::geo_encoding_utils::GeoEncodingUtils;
use crate::core::index::BinaryValueEnum;
use crate::core::index::BytesRef;
use crate::core::index::indexable_field::{
  IndexableField, IndexingTokenStream, ReusedIndexingTokenStream,
};
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::number::Number;
use crate::core::util::numeric_utils::NumericUtils;
use std::borrow::Cow;
use std::fmt;
use std::fmt::Write as _;
/// An indexed 2-Dimension Bounding Box field for the Geospatial Lat/Lon Coordinate system.
///
/// This field indexes 2-dimension Latitude, Longitude based Geospatial Bounding Boxes. The
/// bounding boxes are defined as `minLat, minLon, maxLat, maxLon` where min/max lat,lon pairs
/// using double floating point precision.
///
/// Multiple values for the same field in one document is supported.
///
/// This field defines the following static factory methods for common search operations over
/// double ranges:
///
/// - [`new_intersects_query`](Self::new_intersects_query) matches bounding boxes that intersect
///   the defined search bounding box.
/// - [`new_within_query`](Self::new_within_query) matches bounding boxes that are within the
///   defined search bounding box.
/// - [`new_contains_query`](Self::new_contains_query) matches bounding boxes that contain the
///   defined search bounding box.
/// - [`new_crosses_query`](Self::new_crosses_query) matches bounding boxes that cross the defined
///   search bounding box.
///
/// The following Field limitations and restrictions apply:
///
/// - Dateline wrapping is not supported.
/// - Due to an encoding limitation Eastern and Western Hemisphere Bounding Boxes that share the
///   dateline are not supported.
pub struct LatLonBoundingBox {
  parent_field: Field,
}

impl LatLonBoundingBox {
  /// Uses same encoding as [`LatLonPoint`] so numBytes is the same.
  pub const BYTES: usize = LatLonPoint::BYTES;

  /// Create a new 2D GeoBoundingBoxField representing a 2 dimensional geospatial bounding box.
  ///
  /// # Arguments
  ///
  /// - `name` - Field name.
  /// - `min_lat` - Minimum latitude value (in degrees); valid in `[-90.0 : 90.0]`.
  /// - `min_lon` - Minimum longitude value (in degrees); valid in `[-180.0 : 180.0]`.
  /// - `max_lat` - Maximum latitude value (in degrees); valid in `[minLat : 90.0]`.
  /// - `max_lon` - Maximum longitude value (in degrees); valid in `[minLon : 180.0]`.
  pub fn new<T>(name: T, min_lat: f64, min_lon: f64, max_lat: f64, max_lon: f64) -> Result<Self>
  where
    T: Into<Cow<'static, str>>,
  {
    let mut parent_field = Field::new(name, Dummy(()), Self::get_type(2)?);
    Self::set_range_values_internal(&mut parent_field, min_lat, min_lon, max_lat, max_lon)?;
    Ok(Self { parent_field })
  }

  fn get_type(geo_dimensions: usize) -> Result<FieldType> {
    let mut ft = FieldType::new();
    ft.set_dimensions(geo_dimensions * 2, Self::BYTES)?;
    ft.freeze();
    Ok(ft)
  }

  /// Changes the values of the field.
  pub fn set_range_values(
    &mut self,
    min_lat: f64,
    min_lon: f64,
    max_lat: f64,
    max_lon: f64,
  ) -> Result<()> {
    Self::set_range_values_internal(&mut self.parent_field, min_lat, min_lon, max_lat, max_lon)
  }

  fn set_range_values_internal(
    parent_field: &mut Field,
    min_lat: f64,
    min_lon: f64,
    max_lat: f64,
    max_lon: f64,
  ) -> Result<()> {
    check_args(min_lat, min_lon, max_lat, max_lon)?;

    let bytes = match &mut parent_field.fields_data {
      FieldDataEnum::Binary(b) => &mut b.bytes,
      FieldDataEnum::Dummy(_) => {
        let new_bytes = vec![0u8; 4 * Self::BYTES];
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
    encode_point(min_lat, min_lon, bytes, 0)?;
    encode_point(max_lat, max_lon, bytes, 2 * Self::BYTES)
  }

  /// Create a new 2d query that finds all indexed 2d GeoBoundingBoxField values that intersect the
  /// defined 3d bounding ranges.
  ///
  /// # Arguments
  ///
  /// - `field` - Field name.
  /// - `min_lat` - Minimum latitude value (in degrees); valid in `[-90.0 : 90.0]`.
  /// - `min_lon` - Minimum longitude value (in degrees); valid in `[-180.0 : 180.0]`.
  /// - `max_lat` - Maximum latitude value (in degrees); valid in `[minLat : 90.0]`.
  /// - `max_lon` - Maximum longitude value (in degrees); valid in `[minLon : 180.0]`.
  ///
  /// # Returns
  ///
  /// Query for matching intersecting 2d bounding boxes.
  pub fn new_intersects_query<T>(
    field: T,
    min_lat: f64,
    min_lon: f64,
    max_lat: f64,
    max_lon: f64,
  ) -> Result<RangeFieldQuery>
  where
    T: Into<String>,
  {
    Self::new_range_query(
      field,
      min_lat,
      min_lon,
      max_lat,
      max_lon,
      QueryType::Intersects,
    )
  }

  /// Create a new 2d query that finds all indexed 2d GeoBoundingBoxField values that are within the
  /// defined 2d bounding box.
  ///
  /// # Arguments
  ///
  /// - `field` - Field name.
  /// - `min_lat` - Minimum latitude value (in degrees); valid in `[-90.0 : 90.0]`.
  /// - `min_lon` - Minimum longitude value (in degrees); valid in `[-180.0 : 180.0]`.
  /// - `max_lat` - Maximum latitude value (in degrees); valid in `[minLat : 90.0]`.
  /// - `max_lon` - Maximum longitude value (in degrees); valid in `[minLon : 180.0]`.
  ///
  /// # Returns
  ///
  /// Query for matching 3d bounding boxes that are within the defined bounding box.
  pub fn new_within_query<T>(
    field: T,
    min_lat: f64,
    min_lon: f64,
    max_lat: f64,
    max_lon: f64,
  ) -> Result<RangeFieldQuery>
  where
    T: Into<String>,
  {
    Self::new_range_query(field, min_lat, min_lon, max_lat, max_lon, QueryType::Within)
  }

  /// Create a new 2d query that finds all indexed 2d GeoBoundingBoxField values that contain the
  /// defined 2d bounding box.
  ///
  /// # Arguments
  ///
  /// - `field` - Field name.
  /// - `min_lat` - Minimum latitude value (in degrees); valid in `[-90.0 : 90.0]`.
  /// - `min_lon` - Minimum longitude value (in degrees); valid in `[-180.0 : 180.0]`.
  /// - `max_lat` - Maximum latitude value (in degrees); valid in `[minLat : 90.0]`.
  /// - `max_lon` - Maximum longitude value (in degrees); valid in `[minLon : 180.0]`.
  ///
  /// # Returns
  ///
  /// Query for matching 2d bounding boxes that contain the defined bounding box.
  pub fn new_contains_query<T>(
    field: T,
    min_lat: f64,
    min_lon: f64,
    max_lat: f64,
    max_lon: f64,
  ) -> Result<RangeFieldQuery>
  where
    T: Into<String>,
  {
    Self::new_range_query(
      field,
      min_lat,
      min_lon,
      max_lat,
      max_lon,
      QueryType::Contains,
    )
  }

  /// Create a new 2d query that finds all indexed 2d GeoBoundingBoxField values that cross the
  /// defined 3d bounding box.
  ///
  /// # Arguments
  ///
  /// - `field` - Field name.
  /// - `min_lat` - Minimum latitude value (in degrees); valid in `[-90.0 : 90.0]`.
  /// - `min_lon` - Minimum longitude value (in degrees); valid in `[-180.0 : 180.0]`.
  /// - `max_lat` - Maximum latitude value (in degrees); valid in `[minLat : 90.0]`.
  /// - `max_lon` - Maximum longitude value (in degrees); valid in `[minLon : 180.0]`.
  ///
  /// # Returns
  ///
  /// Query for matching 2d bounding boxes that cross the defined bounding box.
  pub fn new_crosses_query<T>(
    field: T,
    min_lat: f64,
    min_lon: f64,
    max_lat: f64,
    max_lon: f64,
  ) -> Result<RangeFieldQuery>
  where
    T: Into<String>,
  {
    Self::new_range_query(
      field,
      min_lat,
      min_lon,
      max_lat,
      max_lon,
      QueryType::Crosses,
    )
  }
  /// helper method to create a two-dimensional geospatial bounding box query
  fn new_range_query<T>(
    field: T,
    min_lat: f64,
    min_lon: f64,
    max_lat: f64,
    max_lon: f64,
    query_type: QueryType,
  ) -> Result<RangeFieldQuery>
  where
    T: Into<String>,
  {
    check_args(min_lat, min_lon, max_lat, max_lon)?;
    RangeFieldQuery::new(
      field.into(),
      encode(min_lat, min_lon, max_lat, max_lon)?,
      2,
      query_type,
      LatLonBoundingBoxFieldQuery,
    )
  }
}

impl FieldBase for LatLonBoundingBox {}

impl IndexableField for LatLonBoundingBox {
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
      "cannot convert LatLonBoundingBox to a single numeric value",
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

impl fmt::Display for LatLonBoundingBox {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "LatLonBoundingBox <{}:", self.parent_field.name())?;
    match &self.parent_field.fields_data {
      FieldDataEnum::Binary(bytes) => {
        let mut buffer = String::new();
        to_string(&bytes.bytes, 0, &mut buffer).map_err(|_| fmt::Error)?;
        let min_end = buffer.len();
        to_string(&bytes.bytes, 1, &mut buffer).map_err(|_| fmt::Error)?;
        write!(f, "[{},{}]", &buffer[..min_end], &buffer[min_end..])?;
      },
      _ => {
        write!(f, "Unsupported FieldDataEnum variant")?;
      },
    }
    write!(f, ">")
  }
}

fn check_args(min_lat: f64, min_lon: f64, max_lat: f64, max_lon: f64) -> Result<()> {
  if min_lon > max_lon {
    return Err(LuceneError::illegal_argument(format!(
      "cannot have minLon [{}] exceed maxLon [{}].",
      min_lon, max_lon
    )));
  }
  if min_lat > max_lat {
    return Err(LuceneError::illegal_argument(format!(
      "cannot have minLat [{}] exceed maxLat [{}].",
      min_lat, max_lat
    )));
  }
  Ok(())
}

pub(crate) fn encode(min_lat: f64, min_lon: f64, max_lat: f64, max_lon: f64) -> Result<Vec<u8>> {
  let mut bytes = vec![0u8; LatLonBoundingBox::BYTES * 4];
  encode_point(min_lat, min_lon, &mut bytes, 0)?;
  encode_point(max_lat, max_lon, &mut bytes, LatLonBoundingBox::BYTES * 2)?;
  Ok(bytes)
}

fn encode_point(lat: f64, lon: f64, result: &mut [u8], offset: usize) -> Result<()> {
  NumericUtils::int_to_sortable_bytes(GeoEncodingUtils::encode_latitude(lat)?, result, offset);
  NumericUtils::int_to_sortable_bytes(
    GeoEncodingUtils::encode_longitude(lon)?,
    result,
    offset + BitUtil::INT_BYTES,
  );
  Ok(())
}

fn to_string(ranges: &[u8], dimension: usize, buffer: &mut String) -> Result<()> {
  let (lat, lon) = match dimension {
    0 => (
      GeoEncodingUtils::decode_latitude_from_bytes(ranges, 0),
      GeoEncodingUtils::decode_longitude_from_bytes(ranges, 4),
    ),
    1 => (
      GeoEncodingUtils::decode_latitude_from_bytes(ranges, 8),
      GeoEncodingUtils::decode_longitude_from_bytes(ranges, 12),
    ),
    _ => {
      return Err(LuceneError::illegal_argument(format!(
        "invalid dimension [{}] in toString",
        dimension
      )));
    },
  };
  write!(buffer, "{:?},{:?}", lat, lon)?;
  Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LatLonBoundingBoxFieldQuery;

impl RangeFieldQueryBase for LatLonBoundingBoxFieldQuery {
  fn to_string(&self, value: &[u8], dimension: usize) -> Result<String> {
    let mut buffer = String::new();
    to_string(value, dimension, &mut buffer)?;
    Ok(buffer)
  }
}
