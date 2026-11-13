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
use crate::core::document::lat_lon_doc_values_box_query::LatLonDocValuesBoxQuery;
use crate::core::document::lat_lon_doc_values_query::LatLonDocValuesQuery;
use crate::core::document::lat_lon_point_sort_field::LatLonPointSortField;
use crate::core::document::shape_field::QueryRelation;
use crate::core::geo::circle::Circle;
use crate::core::geo::geo_encoding_utils::GeoEncodingUtils;
use crate::core::geo::lat_lon_geometry::{LatLonGeometry, LatLonGeometryEnum};
use crate::core::geo::polygon::Polygon;
use crate::core::index::BinaryValueEnum;
use crate::core::index::BytesRef;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::indexable_field::{
  IndexableField, IndexingTokenStream, ReusedIndexingTokenStream,
};
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::search::match_no_docs_query::MatchNoDocsQuery;
use crate::core::search::query::Query;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::number::Number;
use std::borrow::Cow;
use std::fmt::{Display, Formatter};
use std::sync::LazyLock;

/// Type for a [`LatLonDocValuesField`](crate::core::document::lat_lon_doc_values_field::LatLonDocValuesField).
///
/// Each value stores an `i64` where the upper 32 bits are the encoded latitude, and the
/// lower 32 bits are the encoded longitude.
///
/// # See also
///
/// - [`GeoEncodingUtils::decode_latitude`]
/// - [`GeoEncodingUtils::decode_longitude`]
pub(crate) static TYPE: LazyLock<FieldType> = LazyLock::new(|| {
  let mut ft = FieldType::new();
  expect_invariant!(
    ft.set_doc_values_type(DocValuesType::SortedNumeric),
    "the built-in latitude-longitude doc-values field type is mutable and unfrozen"
  );
  ft.freeze();
  ft
});
/// A per-document location field.
///
/// Sorting by distance is efficient. Multiple values for the same field in one document are
/// allowed.
///
/// This field defines static factory methods for common operations:
///
/// - [`new_distance_sort`](Self::new_distance_sort) for ordering documents by distance from a specified
///   location.
///
/// If you also need query operations, you should add a separate [`LatLonPoint`](crate::core::document::lat_lon_point::LatLonPoint) instance. If you
/// also need to store the value, you should add a separate [`StoredField`](crate::core::document::stored_field::StoredField) instance.
///
/// **WARNING**: Values are indexed with some loss of precision from the original `f64` values
/// (4.190951585769653E-8 for the latitude component and 8.381903171539307E-8 for longitude).
///
/// # See also
///
/// [`LatLonPoint`](crate::core::document::lat_lon_point::LatLonPoint)
pub struct LatLonDocValuesField {
  parent_field: Field,
}

impl LatLonDocValuesField {
  /// Creates a new [`LatLonDocValuesField`] with the specified latitude and longitude.
  ///
  /// # Parameters
  ///
  /// - `name`: field name
  /// - `latitude`: latitude value; must be within standard +/-90 coordinate bounds.
  /// - `longitude`: longitude value; must be within standard +/-180 coordinate bounds.
  ///
  /// # Errors
  ///
  /// Returns an error if latitude or longitude are out of bounds.
  pub fn new(name: &str, latitude: f64, longitude: f64) -> Result<Self> {
    let mut field = Self {
      parent_field: Field::new(name.to_owned(), 0i64, TYPE.clone()),
    };
    field.set_location_value(latitude, longitude)?;
    Ok(field)
  }

  /// Change the values of this field.
  ///
  /// # Parameters
  ///
  /// - `latitude`: latitude value; must be within standard +/-90 coordinate bounds.
  /// - `longitude`: longitude value; must be within standard +/-180 coordinate bounds.
  ///
  /// # Errors
  ///
  /// Returns an error if latitude or longitude are out of bounds.
  pub fn set_location_value(&mut self, latitude: f64, longitude: f64) -> Result<()> {
    let latitude_encoded = GeoEncodingUtils::encode_latitude(latitude)?;
    let longitude_encoded = GeoEncodingUtils::encode_longitude(longitude)?;
    let value = ((latitude_encoded as i64) << 32) | (longitude_encoded as u32 as i64);
    self.parent_field.fields_data = value.into();
    Ok(())
  }

  /// Checks field information and returns an error if it is definitely not a [`LatLonDocValuesField`](crate::core::document::lat_lon_doc_values_field::LatLonDocValuesField).
  pub(crate) fn check_compatible(field_info: &FieldInfo) -> Result<()> {
    if *field_info.get_doc_values_type() != DocValuesType::None
      && field_info.get_doc_values_type() != TYPE.doc_values_type()
    {
      return Err(LuceneError::illegal_argument(format!(
        "field=\"{}\" was indexed with docValuesType={:?} but this type has docValuesType={:?}, is the field really a LatLonDocValuesField?",
        field_info.name,
        field_info.get_doc_values_type(),
        TYPE.doc_values_type()
      )));
    }
    Ok(())
  }
  /// Creates a [`SortField`](crate::core::search::sort_field::SortField) for sorting by distance from a location.
  ///
  /// This sort orders documents by ascending distance from the location. The value returned in
  /// [`FieldDoc`](crate::core::search::field_doc::FieldDoc) for the hits contains an `f64` distance in meters.
  ///
  /// If a document is missing the field, then by default it is treated as having
  /// [`f64::INFINITY`] distance (missing values sort last).
  ///
  /// If a document contains multiple values for the field, the *closest* distance to the location is
  /// used.
  ///
  /// # Parameters
  ///
  /// - `field`: field name;
  /// - `latitude`: latitude at the center; must be within standard +/-90 coordinate bounds.
  /// - `longitude`: longitude at the center; must be within standard +/-180 coordinate bounds.
  ///
  /// # Returns
  ///
  /// A [`SortField`](crate::core::search::sort_field::SortField) ordering documents by distance.
  ///
  /// # Errors
  ///
  /// Returns an error if the location has invalid coordinates.
  pub fn new_distance_sort<T>(
    field: T,
    latitude: f64,
    longitude: f64,
  ) -> Result<LatLonPointSortField>
  where
    T: Into<String>,
  {
    LatLonPointSortField::new(field, latitude, longitude)
  }
  /// Create a query for matching a bounding box using doc values. This query is usually slow as it
  /// does not use an index structure and needs to verify documents one-by-one in order to know
  /// whether they match. It is best used wrapped in an [`IndexOrDocValuesQuery`](crate::core::search::index_or_doc_values_query::IndexOrDocValuesQuery) alongside a
  /// [`LatLonPoint::new_box_query`](crate::core::document::lat_lon_point::LatLonPoint::new_box_query).
  pub fn new_slow_box_query(
    field: &str,
    min_latitude: f64,
    max_latitude: f64,
    mut min_longitude: f64,
    max_longitude: f64,
  ) -> Result<Query> {
    if min_latitude == 90.0 {
      return Ok(
        MatchNoDocsQuery::with_reason("LatLonDocValuesField.newBoxQuery with minLatitude=90.0")
          .into(),
      );
    }
    if min_longitude == 180.0 {
      if max_longitude == 180.0 {
        return Ok(
          MatchNoDocsQuery::with_reason(
            "LatLonDocValuesField.newBoxQuery with minLongitude=maxLongitude=180.0",
          )
          .into(),
        );
      } else if max_longitude < min_longitude {
        min_longitude = -180.0;
      }
    }
    Ok(
      LatLonDocValuesBoxQuery::new(
        field.to_string(),
        min_latitude,
        max_latitude,
        min_longitude,
        max_longitude,
      )?
      .into(),
    )
  }
  /// Create a query for matching points within the specified distance of the supplied location. This
  /// query is usually slow as it does not use an index structure and needs to verify documents
  /// one-by-one in order to know whether they match. It is best used wrapped in an
  /// [`IndexOrDocValuesQuery`](crate::core::search::index_or_doc_values_query::IndexOrDocValuesQuery) alongside a [`LatLonPoint::new_distance_query`](crate::core::document::lat_lon_point::LatLonPoint::new_distance_query).
  ///
  /// # Parameters
  ///
  /// - `field`: field name.
  /// - `latitude`: latitude at the center; must be within standard +/-90 coordinate bounds.
  /// - `longitude`: longitude at the center; must be within standard +/-180 coordinate bounds.
  /// - `radius_meters`: maximum distance from the center in meters; must be non-negative and finite.
  ///
  /// # Returns
  ///
  /// A query matching points within this distance.
  ///
  /// # Errors
  ///
  /// Returns an error if the location has invalid coordinates, or the radius is
  /// invalid.
  pub fn new_slow_distance_query(
    field: &str,
    latitude: f64,
    longitude: f64,
    radius_meters: f64,
  ) -> Result<Query> {
    let circle = Circle::new(latitude, longitude, radius_meters)?;
    Self::new_slow_geometry_query(field, QueryRelation::Intersects, vec![circle])
  }
  /// Create a query for matching points within the supplied polygons. This query is usually slow as
  /// it does not use an index structure and needs to verify documents one-by-one in order to know
  /// whether they match. It is best used wrapped in an [`IndexOrDocValuesQuery`](crate::core::search::index_or_doc_values_query::IndexOrDocValuesQuery) alongside a
  /// [`LatLonPoint::new_polygon_query`](crate::core::document::lat_lon_point::LatLonPoint::new_polygon_query).
  ///
  /// # Parameters
  ///
  /// - `field`: field name.
  /// - `polygons`: array of polygons. must not be empty.
  ///
  /// # Returns
  ///
  /// A query matching points within the given polygons.
  ///
  /// # Errors
  ///
  /// Returns an error if `polygons` is empty.
  pub fn new_slow_polygon_query<I>(field: &str, polygons: I) -> Result<Query>
  where
    I: IntoIterator<Item = Polygon>,
  {
    Self::new_slow_geometry_query(field, QueryRelation::Intersects, polygons)
  }
  /// Create a query for matching one or more geometries against the provided
  /// [`QueryRelation`]. Line geometries are not supported for the `WITHIN` relationship.
  /// This query is usually slow as it does not use an index structure and needs to verify documents
  /// one-by-one in order to know whether they match. It is best used wrapped in an
  /// [`IndexOrDocValuesQuery`](crate::core::search::index_or_doc_values_query::IndexOrDocValuesQuery) alongside a [`LatLonPoint::new_geometry_query`](crate::core::document::lat_lon_point::LatLonPoint::new_geometry_query).
  ///
  /// # Parameters
  ///
  /// - `field`: field name.
  /// - `query_relation`: relation the points must satisfy with the provided geometries.
  /// - `lat_lon_geometries`: array of [`LatLonGeometry`] values. must not be empty.
  ///
  /// # Returns
  ///
  /// A query matching points within the given polygons.
  ///
  /// # Errors
  ///
  /// Returns an error if `lat_lon_geometries` is empty or contains an unsupported line geometry.
  pub fn new_slow_geometry_query<T, I>(
    field: &str,
    query_relation: QueryRelation,
    lat_lon_geometries: I,
  ) -> Result<Query>
  where
    T: LatLonGeometry + Into<LatLonGeometryEnum>,
    I: IntoIterator<Item = T>,
  {
    let lat_lon_geometries: Vec<LatLonGeometryEnum> =
      lat_lon_geometries.into_iter().map(Into::into).collect();

    if query_relation == QueryRelation::Intersects
      && lat_lon_geometries.len() == 1
      && let LatLonGeometryEnum::Rectangle(rect) = &lat_lon_geometries[0]
    {
      return Self::new_slow_box_query(
        field,
        rect.min_lat,
        rect.max_lat,
        rect.min_lon,
        rect.max_lon,
      );
    }

    if query_relation == QueryRelation::Contains {
      for geometry in &lat_lon_geometries {
        if !matches!(geometry, LatLonGeometryEnum::Point(_)) {
          return Ok(
            MatchNoDocsQuery::with_reason(
              "Contains LatLonDocValuesField.newSlowGeometryQuery with non-point geometries",
            )
            .into(),
          );
        }
      }
    }

    Ok(LatLonDocValuesQuery::new(field.to_string(), query_relation, lat_lon_geometries)?.into())
  }
}

impl Display for LatLonDocValuesField {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    let current_value = match self.parent_field.fields_data {
      FieldDataEnum::Number(Number::I64(v)) => v as u64,
      _ => {
        return Err(std::fmt::Error);
      },
    };
    let lat = GeoEncodingUtils::decode_latitude((current_value >> 32) as i32);
    let lon = GeoEncodingUtils::decode_longitude(current_value as u32 as i32);
    write!(
      f,
      "LatLonDocValuesField <{}:{},{}>",
      self.parent_field.name, lat, lon
    )
  }
}

impl IndexableField for LatLonDocValuesField {
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

impl FieldBase for LatLonDocValuesField {}
