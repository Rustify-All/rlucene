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
use crate::core::document::lat_lon_point_distance_feature_query::LatLonPointDistanceFeatureQuery;
use crate::core::document::lat_lon_point_distance_query::LatLonPointDistanceQuery;
use crate::core::document::lat_lon_point_query::lat_lon_point_query;
use crate::core::document::nearest_neighbor;
use crate::core::document::shape_field::QueryRelation;
use crate::core::geo::geo_encoding_utils::GeoEncodingUtils;
use crate::core::geo::geo_utils::GeoUtils;
use crate::core::geo::lat_lon_geometry::LatLonGeometry;
use crate::core::geo::lat_lon_geometry::LatLonGeometryEnum;
use crate::core::geo::polygon::Polygon;
use crate::core::index::BinaryValueEnum;
use crate::core::index::BytesRef;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::indexable_field::{
  IndexableField, IndexingTokenStream, ReusedIndexingTokenStream,
};
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::point_values::PointValues;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::boolean_query::Builder;
use crate::core::search::boost_query::BoostQuery;
use crate::core::search::constant_score_query::ConstantScoreQuery;
use crate::core::search::field_doc::FieldDoc;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::match_no_docs_query::MatchNoDocsQuery;
use crate::core::search::point_range_query::{PointRangeBase, PointRangeQuery};
use crate::core::search::query::Query;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::top_field_docs::TopFieldDocs;
use crate::core::search::total_hits::Relation::EqualTo;
use crate::core::search::total_hits::TotalHits;
use crate::core::util::SloppyMath;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::number::Number;
use crate::core::util::numeric_utils::NumericUtils;
use std::borrow::Cow;
use std::fmt::{Display, Formatter, Write as _};
use std::sync::LazyLock;

/// Type for an indexed [`LatLonPoint`](crate::core::document::lat_lon_point::LatLonPoint).
///
/// Each point stores two dimensions with 4 bytes per dimension.
pub(crate) static TYPE_: LazyLock<FieldType> = LazyLock::new(|| {
  let mut ft = FieldType::new();
  expect_invariant!(
    ft.set_dimensions(2, BitUtil::INT_BYTES),
    "the built-in latitude-longitude point dimensions are valid"
  );
  ft.freeze();
  ft
});
/// An indexed location field.
///
/// Finding all documents within a range at search time is efficient. Multiple values for the same
/// field in one document is allowed.
///
/// This field defines static factory methods for common operations:
///
/// * [`Self::new_box_query`] for matching points within a bounding box.
/// * [`Self::new_distance_query`] for matching points within a specified distance.
/// * [`Self::new_polygon_query`] for matching points within an arbitrary polygon.
/// * [`Self::new_geometry_query`] for matching points within an arbitrary geometry collection.
///
/// If you also need per-document operations such as sort by distance, add a separate
/// [`LatLonDocValuesField`](crate::core::document::lat_lon_doc_values_field::LatLonDocValuesField) instance. If you also need to store the value, you should add a
/// separate [`StoredField`](crate::core::document::stored_field::StoredField) instance.
///
/// **WARNING**: Values are indexed with some loss of precision from the original `f64` values
/// (`4.190951585769653E-8` for the latitude component and `8.381903171539307E-8` for
/// longitude).
///
/// See also [`PointValues`].
/// See also [`LatLonDocValuesField`](crate::core::document::lat_lon_doc_values_field::LatLonDocValuesField).
pub struct LatLonPoint {
  parent_field: Field,
}

impl LatLonPoint {
  /// [`LatLonPoint`] is encoded as integer values, so the number of bytes is 4.
  pub const BYTES: usize = BitUtil::INT_BYTES;
  /// Creates a new LatLonPoint with the specified latitude and longitude
  ///
  /// * `name` - field name
  /// * `latitude` - latitude value: must be within standard +/-90 coordinate bounds.
  /// * `longitude` - longitude value: must be within standard +/-180 coordinate bounds.
  pub fn new(name: &str, latitude: f64, longitude: f64) -> Result<Self> {
    let mut point = Self {
      parent_field: Field::new(name.to_owned(), Dummy(()), TYPE_.clone()),
    };
    point.set_location_value(latitude, longitude)?;
    Ok(point)
  }
  /// Change the values of this field
  ///
  /// * `latitude` - latitude value: must be within standard +/-90 coordinate bounds.
  /// * `longitude` - longitude value: must be within standard +/-180 coordinate bounds.
  ///
  /// # Errors
  ///
  /// Returns [`LuceneErrorKind::IllegalArgument`] if latitude or longitude are out of bounds.
  pub fn set_location_value(&mut self, latitude: f64, longitude: f64) -> Result<()> {
    if matches!(self.parent_field.fields_data, FieldDataEnum::Dummy(_)) {
      self.parent_field.fields_data = FieldDataEnum::Binary(BytesRef::from_bytes(vec![0u8; 8]));
    }

    let bytes = match &mut self.parent_field.fields_data {
      FieldDataEnum::Binary(v) => v.bytes.as_mut_slice(),
      _ => {
        return Err(LuceneError::illegal_state("should not be here"));
      },
    };
    let latitude_encoded = GeoEncodingUtils::encode_latitude(latitude)?;
    let longitude_encoded = GeoEncodingUtils::encode_longitude(longitude)?;
    NumericUtils::int_to_sortable_bytes(latitude_encoded, bytes, 0);
    NumericUtils::int_to_sortable_bytes(longitude_encoded, bytes, BitUtil::INT_BYTES);
    Ok(())
  }
  /// sugar encodes a single point as a byte array
  fn encode(latitude: f64, longitude: f64) -> Result<Vec<u8>> {
    let mut bytes = vec![0u8; 2 * BitUtil::INT_BYTES];
    NumericUtils::int_to_sortable_bytes(
      GeoEncodingUtils::encode_latitude(latitude)?,
      &mut bytes,
      0,
    );
    NumericUtils::int_to_sortable_bytes(
      GeoEncodingUtils::encode_longitude(longitude)?,
      &mut bytes,
      BitUtil::INT_BYTES,
    );
    Ok(bytes)
  }
  /// sugar encodes a single point as a byte array, rounding values up
  fn encode_ceil(latitude: f64, longitude: f64) -> Result<Vec<u8>> {
    let mut bytes = vec![0u8; 2 * BitUtil::INT_BYTES];
    NumericUtils::int_to_sortable_bytes(
      GeoEncodingUtils::encode_latitude_ceil(latitude)?,
      &mut bytes,
      0,
    );
    NumericUtils::int_to_sortable_bytes(
      GeoEncodingUtils::encode_longitude_ceil(longitude)?,
      &mut bytes,
      BitUtil::INT_BYTES,
    );
    Ok(bytes)
  }

  /// Checks field information and returns an error if it is definitely not a [`LatLonPoint`](crate::core::document::lat_lon_point::LatLonPoint).
  pub(crate) fn check_compatible(field_info: &FieldInfo) -> Result<()> {
    // point/dv properties could be "unset", if you e.g. used only StoredField with this same name
    // in the segment.
    if field_info.get_point_dimension_count() != 0
      && field_info.get_point_dimension_count() != TYPE_.point_dimension_count()
    {
      return Err(LuceneError::illegal_argument(format!(
        "field=\"{}\" was indexed with numDims={} but this point type has numDims={}, is the field really a LatLonPoint?",
        field_info.name,
        field_info.get_point_dimension_count(),
        TYPE_.point_dimension_count()
      )));
    }

    if field_info.get_point_num_bytes() != 0
      && field_info.get_point_num_bytes() != TYPE_.point_num_bytes()
    {
      return Err(LuceneError::illegal_argument(format!(
        "field=\"{}\" was indexed with bytesPerDim={} but this point type has bytesPerDim={}, is the field really a LatLonPoint?",
        field_info.name,
        field_info.get_point_num_bytes(),
        TYPE_.point_num_bytes()
      )));
    }

    Ok(())
  }
  /// Create a query for matching a bounding box.
  ///
  /// The box may cross over the dateline.
  ///
  /// * `field` - field name.
  /// * `min_latitude` - latitude lower bound: must be within standard +/-90 coordinate bounds.
  /// * `max_latitude` - latitude upper bound: must be within standard +/-90 coordinate bounds.
  /// * `min_longitude` - longitude lower bound: must be within standard +/-180 coordinate bounds.
  /// * `max_longitude` - longitude upper bound: must be within standard +/-180 coordinate bounds.
  ///
  /// Returns query matching points within this box.
  ///
  /// # Errors
  ///
  /// Returns [`LuceneErrorKind::IllegalArgument`] if the box has invalid coordinates.
  pub fn new_box_query(
    field: &str,
    min_latitude: f64,
    max_latitude: f64,
    min_longitude: f64,
    max_longitude: f64,
  ) -> Result<Query> {
    let mut min_longitude = min_longitude;
    if min_latitude == 90.0 {
      return Ok(
        MatchNoDocsQuery::with_reason("LatLonPoint.newBoxQuery with minLatitude=90.0").into(),
      );
    }
    if min_longitude == 180.0 {
      if max_longitude == 180.0 {
        return Ok(
          MatchNoDocsQuery::with_reason(
            "LatLonPoint.newBoxQuery with minLongitude=maxLongitude=180.0",
          )
          .into(),
        );
      } else if max_longitude < min_longitude {
        min_longitude = -180.0;
      }
    }

    let lower = LatLonPoint::encode_ceil(min_latitude, min_longitude)?;
    let upper = LatLonPoint::encode(max_latitude, max_longitude)?;

    if max_longitude < min_longitude {
      let mut q = Builder::new();

      let mut left_open = lower.clone();
      NumericUtils::int_to_sortable_bytes(i32::MIN, &mut left_open, BitUtil::INT_BYTES);
      let left = Self::new_box_internal(field, left_open, upper.clone())?;
      q.add(left, Occur::Should)?;

      let mut right_open = upper;
      NumericUtils::int_to_sortable_bytes(i32::MAX, &mut right_open, BitUtil::INT_BYTES);
      let right = Self::new_box_internal(field, lower, right_open)?;
      q.add(right, Occur::Should)?;

      Ok(ConstantScoreQuery::new(q.build()).into())
    } else {
      Ok(Self::new_box_internal(field, lower, upper)?.into())
    }
  }
  fn new_box_internal(field: &str, min: Vec<u8>, max: Vec<u8>) -> Result<PointRangeQuery> {
    PointRangeQuery::new(field.to_string(), min, max, 2, LatLonPointRangeQuery)
  }
  /// Create a query for matching points within the specified distance of the supplied location.
  ///
  /// * `field` - field name.
  /// * `latitude` - latitude at the center: must be within standard +/-90 coordinate bounds.
  /// * `longitude` - longitude at the center: must be within standard +/-180 coordinate bounds.
  /// * `radius_meters` - maximum distance from the center in meters: must be non-negative and
  ///   finite.
  ///
  /// Returns query matching points within this distance.
  ///
  /// # Errors
  ///
  /// Returns [`LuceneErrorKind::IllegalArgument`] if the location has invalid coordinates or radius is invalid.
  pub fn new_distance_query(
    field: &str,
    latitude: f64,
    longitude: f64,
    radius_meters: f64,
  ) -> Result<LatLonPointDistanceQuery> {
    LatLonPointDistanceQuery::new(field.to_string(), latitude, longitude, radius_meters)
  }
  /// Create a query for matching one or more polygons.
  ///
  /// * `field` - field name.
  /// * `polygons` - array of polygons.
  ///
  /// Returns query matching points within this polygon.
  ///
  /// # Errors
  ///
  /// Returns [`LuceneErrorKind::IllegalArgument`] if `polygons` is empty.
  ///
  /// See also [`Polygon`].
  pub fn new_polygon_query<I>(field: &str, polygons: I) -> Result<Query>
  where
    I: IntoIterator<Item = Polygon>,
  {
    Self::new_geometry_query(field, QueryRelation::Intersects, polygons)
  }

  /// Create a query for matching one or more geometries against the provided
  /// [`QueryRelation`]. Line geometries are not supported for WITHIN relationship.
  ///
  /// * `field` - field name.
  /// * `query_relation` - The relation the points needs to satisfy with the provided geometries,
  ///   
  /// * `lat_lon_geometries` - array of LatLonGeometries.
  ///
  /// Returns query matching points within at least one geometry.
  ///
  /// # Errors
  ///
  /// Returns [`LuceneErrorKind::IllegalArgument`] if `lat_lon_geometries` is empty.
  ///
  /// See also [`LatLonGeometry`].
  pub fn new_geometry_query<T, I>(
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

    if query_relation == QueryRelation::Intersects && lat_lon_geometries.len() == 1 {
      match &lat_lon_geometries[0] {
        LatLonGeometryEnum::Rectangle(rect) => {
          return Self::new_box_query(
            field,
            rect.min_lat,
            rect.max_lat,
            rect.min_lon,
            rect.max_lon,
          );
        },
        LatLonGeometryEnum::Circle(circle) => {
          return Ok(
            Self::new_distance_query(
              field,
              circle.get_lat(),
              circle.get_lon(),
              circle.get_radius(),
            )?
            .into(),
          );
        },
        _ => {},
      }
    }

    if query_relation == QueryRelation::Contains {
      return Self::make_contains_geometry_query(field, lat_lon_geometries);
    }

    Ok(lat_lon_point_query(field.to_string(), query_relation, lat_lon_geometries)?.into())
  }

  fn make_contains_geometry_query<T>(field: &str, lat_lon_geometries: Vec<T>) -> Result<Query>
  where
    T: LatLonGeometry + Into<LatLonGeometryEnum>,
  {
    let mut builder = Builder::new();

    for geometry in lat_lon_geometries {
      let geometry = geometry.into();
      if !matches!(geometry, LatLonGeometryEnum::Point(_)) {
        return Ok(
          MatchNoDocsQuery::with_reason(
            "Contains LatLonPoint.newGeometryQuery with non-point geometries",
          )
          .into(),
        );
      }
      builder.add(
        lat_lon_point_query(field.to_string(), QueryRelation::Contains, vec![geometry])?,
        Occur::Must,
      )?;
    }

    Ok(ConstantScoreQuery::new(builder.build()).into())
  }

  /// Given a field that indexes point values into a [`LatLonPoint`] and doc values into
  /// [`LatLonDocValuesField`](crate::core::document::lat_lon_doc_values_field::LatLonDocValuesField), this returns a query that scores documents based on their haversine
  /// distance in meters to `(origin_lat, origin_lon)`: `score = weight * pivot_distance_meters /
  /// (pivot_distance_meters + distance)`, ie. score is in the `[0, weight]` range, is equal to
  /// `weight` when the document's value is equal to `(origin_lat, origin_lon)` and is equal to
  /// `weight / 2` when the document's value is distant of `pivot_distance_meters` from
  /// `(origin_lat, origin_lon)`. In case of multi-valued fields, only the closest point to
  /// `(origin_lat, origin_lon)` will be considered. This query is typically useful to boost results
  /// based on distance by adding this query to a [`Occur::Should`] clause of a [`BooleanQuery`](crate::core::search::boolean_query::BooleanQuery).
  pub fn new_distance_feature_query(
    field: &str,
    weight: f32,
    origin_lat: f64,
    origin_lon: f64,
    pivot_distance_meters: f64,
  ) -> Result<Query> {
    let mut query: Query = LatLonPointDistanceFeatureQuery::new(
      field.to_string(),
      origin_lat,
      origin_lon,
      pivot_distance_meters,
    )?
    .into();
    if weight != 1f32 {
      query = BoostQuery::new(query, weight)?.into();
    }
    Ok(query)
  }

  /// Finds the `n` nearest indexed points to the provided point, according to Haversine distance.
  ///
  /// This is functionally equivalent to running [`MatchAllDocsQuery`](crate::core::search::match_all_docs_query::MatchAllDocsQuery) with a
  /// [`LatLonDocValuesField::new_distance_sort`](crate::core::document::lat_lon_doc_values_field::LatLonDocValuesField::new_distance_sort), but is far more efficient since it takes
  /// advantage of properties the indexed BKD tree. Multi-valued fields are currently not
  /// de-duplicated, so if a document had multiple instances of the specified field that make it
  /// into the top n, that document will appear more than once.
  ///
  /// Documents are ordered by ascending distance from the location. The value returned in
  /// [`FieldDoc`] for the hits contains a `Double` instance with the distance in meters.
  ///
  /// * `searcher` - IndexSearcher to find nearest points from.
  /// * `field` - field name.
  /// * `latitude` - latitude at the center: must be within standard +/-90 coordinate bounds.
  /// * `longitude` - longitude at the center: must be within standard +/-180 coordinate bounds.
  /// * `n` - the number of nearest neighbors to retrieve.
  ///
  /// Returns [`TopFieldDocs`] containing documents ordered by distance, where the field value for each
  /// [`FieldDoc`] is the distance in meters.
  ///
  /// # Errors
  ///
  /// Returns [`LuceneErrorKind::IllegalArgument`] if `latitude`, `longitude` or `n` are out-of-bounds.
  ///
  /// Returns an error if an I/O error occurs while finding the points.
  pub fn nearest<IRC, SIM>(
    searcher: &IndexSearcher<IRC, SIM>,
    field: &str,
    latitude: f64,
    longitude: f64,
    n: i32,
  ) -> Result<TopFieldDocs>
  where
    SIM: Similarity,
    IRC: IndexReaderContext,
    IndexSearcher<IRC, SIM>: Sync,
  {
    GeoUtils::check_latitude(latitude)?;
    GeoUtils::check_longitude(longitude)?;

    if n < 1 {
      return Err(LuceneError::illegal_argument(format!(
        "n must be at least 1; got {}",
        n
      )));
    }

    let mut readers = Vec::new();
    let mut doc_bases = Vec::new();
    let mut live_docs = Vec::new();
    let mut total_hits = 0;

    for leaf in searcher.get_leaf_contexts()? {
      let reader = leaf.reader();
      let points = reader.get_point_values(field)?;
      if let Some(points) = points {
        total_hits += points.get_doc_count()?;
        readers.push(points);
        doc_bases.push(leaf.doc_base as i32);
        live_docs.push(reader.get_live_docs()?);
      }
    }

    let hits = nearest_neighbor::nearest(
      latitude, longitude, &readers, &live_docs, &doc_bases, n as usize,
    )?;

    let mut score_docs = Vec::with_capacity(hits.len());
    for hit in hits {
      let hit_distance = SloppyMath::haversin_meters_from_sort_key(hit.distance_sort_key);
      score_docs.push(FieldDoc::with_fields(hit.doc_id, 0.0, vec![hit_distance.into()]).into());
    }

    Ok(TopFieldDocs::new(
      TotalHits::new(total_hits as usize, EqualTo),
      score_docs,
      vec![],
    ))
  }
}

impl Display for LatLonPoint {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    let mut result = String::new();
    result.push_str(
      std::any::type_name::<Self>()
        .rsplit("::")
        .next()
        .unwrap_or("LatLonPoint"),
    );
    result.push_str(" <");
    result.push_str(&self.parent_field.name);
    result.push(':');

    let bytes = match self.parent_field.fields_data {
      FieldDataEnum::Binary(ref v) => v.bytes.as_slice(),
      _ => {
        return Err(std::fmt::Error);
      },
    };
    write!(
      result,
      "{}",
      GeoEncodingUtils::decode_latitude_from_bytes(bytes, 0)
    )?;
    result.push(',');
    write!(
      result,
      "{}",
      GeoEncodingUtils::decode_longitude_from_bytes(bytes, BitUtil::INT_BYTES)
    )?;

    result.push('>');
    write!(f, "{result}")
  }
}

impl IndexableField for LatLonPoint {
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
impl FieldBase for LatLonPoint {}

#[derive(Debug, Clone)]
pub struct LatLonPointRangeQuery;
impl PointRangeBase for LatLonPointRangeQuery {
  fn to_string(&self, dimension: usize, value: &[u8]) -> Result<String> {
    match dimension {
      0 => Ok(GeoEncodingUtils::decode_latitude_from_bytes(value, 0).to_string()),
      1 => Ok(GeoEncodingUtils::decode_longitude_from_bytes(value, 0).to_string()),
      _ => Err(LuceneError::illegal_state(format!(
        "invalid dimension {} for LatLonPoint, must be 0 or 1",
        dimension
      ))),
    }
  }
}
