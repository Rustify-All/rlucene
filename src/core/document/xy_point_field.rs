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
use crate::core::document::xy_point_in_geometry_query::XYPointInGeometryQuery;
use crate::core::geo::xy_circle::XYCircle;
use crate::core::geo::xy_encoding_utils::XYEncodingUtils;
use crate::core::geo::xy_geometry::{XYGeometry, XYGeometryEnum};
use crate::core::geo::xy_polygon::XYPolygon;
use crate::core::geo::xy_rectangle::XYRectangle;
use crate::core::index::BinaryValueEnum;
use crate::core::index::BytesRef;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::indexable_field::{
  IndexableField, IndexingTokenStream, ReusedIndexingTokenStream,
};
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::search::query::Query;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::number::Number;
use crate::core::util::numeric_utils::NumericUtils;
use std::borrow::Cow;
use std::fmt::{Display, Formatter};
use std::sync::LazyLock;

/// XYPoint is encoded as integer values so number of bytes is 4
pub const BYTES: usize = BitUtil::INT_BYTES;

/// Type for an indexed XYPoint
///
/// Each point stores two dimensions with 4 bytes per dimension.
pub(crate) static TYPE: LazyLock<FieldType> = LazyLock::new(|| {
  let mut ft = FieldType::new();
  expect_invariant!(
    ft.set_dimensions(2, BitUtil::INT_BYTES),
    "the built-in XY point dimensions are valid"
  );
  ft.freeze();
  ft
});

/// An indexed XY position field.
///
/// Finding all documents within a range at search time is efficient. Multiple values for the same
/// field in one document is allowed.
///
/// This field defines static factory methods for common operations:
///
/// - `new_box_query` for matching points within a bounding box.
/// - `new_distance_query` for matching points within a specified
///   distance.
/// - `new_polygon_query` for matching points within an arbitrary polygon.
/// - `new_geometry_query` for matching points within an arbitrary
///   geometry collection.
///
/// If you also need per-document operations such as sort by distance, add a separate
/// [`XYDocValuesField`](crate::core::document::xy_doc_values_field::XYDocValuesField) instance. If you also need to store the value, you should add a separate
/// [`StoredField`](crate::core::document::stored_field::StoredField) instance.
///
/// See [`PointValues`](crate::core::index::point_values::PointValues).
///
/// See [`XYDocValuesField`](crate::core::document::xy_doc_values_field::XYDocValuesField).
pub struct XYPointField {
  parent_field: Field,
}

impl XYPointField {
  /// Change the values of this field
  ///
  /// # Parameters
  ///
  /// - `x`: x value.
  /// - `y`: y value.
  pub fn set_location_value(&mut self, x: f32, y: f32) -> Result<()> {
    let x_encoded = XYEncodingUtils::encode(x)?;
    let y_encoded = XYEncodingUtils::encode(y)?;

    match &mut self.parent_field.fields_data {
      FieldDataEnum::Binary(bytes) => {
        let encoded = &mut bytes.bytes[..2 * BitUtil::INT_BYTES];
        NumericUtils::int_to_sortable_bytes(x_encoded, encoded, 0);
        NumericUtils::int_to_sortable_bytes(y_encoded, encoded, BitUtil::INT_BYTES);
        bytes.offset = 0;
        bytes.length = bytes.bytes.len();
      },
      _ => {
        let mut encoded = vec![0u8; 2 * BitUtil::INT_BYTES];
        NumericUtils::int_to_sortable_bytes(x_encoded, &mut encoded, 0);
        NumericUtils::int_to_sortable_bytes(y_encoded, &mut encoded, BitUtil::INT_BYTES);
        self.parent_field.fields_data = BytesRef::from_bytes(encoded).into();
      },
    }
    Ok(())
  }

  /// Creates a new XYPoint with the specified x and y
  ///
  /// # Parameters
  ///
  /// - `name`: field name
  /// - `x`: x value.
  /// - `y`: y value.
  pub fn new<T>(name: T, x: f32, y: f32) -> Result<Self>
  where
    T: Into<String>,
  {
    let mut field = Self {
      parent_field: Field::new(
        name.into(),
        BytesRef::from_bytes(vec![0u8; 2 * BitUtil::INT_BYTES]),
        TYPE.clone(),
      ),
    };
    field.set_location_value(x, y)?;
    Ok(field)
  }

  /// Checks field information and returns an error if it is definitely not an [`XYPoint`](crate::core::geo::xy_point::XYPoint).
  pub(crate) fn check_compatible(field_info: &FieldInfo) -> Result<()> {
    if field_info.get_point_dimension_count() != 0
      && field_info.get_point_dimension_count() != TYPE.point_dimension_count()
    {
      return Err(LuceneError::illegal_argument(format!(
        "field=\"{}\" was indexed with numDims={} but this point type has numDims={}, is the field really a XYPoint?",
        field_info.name,
        field_info.get_point_dimension_count(),
        TYPE.point_dimension_count()
      )));
    }

    if field_info.get_point_num_bytes() != 0
      && field_info.get_point_num_bytes() != TYPE.point_num_bytes()
    {
      return Err(LuceneError::illegal_argument(format!(
        "field=\"{}\" was indexed with bytesPerDim={} but this point type has bytesPerDim={}, is the field really a XYPoint?",
        field_info.name,
        field_info.get_point_num_bytes(),
        TYPE.point_num_bytes()
      )));
    }

    Ok(())
  }

  /// Create a query for matching a bounding box.
  ///
  /// # Parameters
  ///
  /// - `field`: field name.
  /// - `min_x`: x lower bound.
  /// - `max_x`: x upper bound.
  /// - `min_y`: y lower bound.
  /// - `max_y`: y upper bound.
  ///
  /// # Returns
  ///
  /// Query matching points within this box
  ///
  /// # Errors
  ///
  /// Returns an error if the box has invalid coordinates.
  pub fn new_box_query<T>(field: T, min_x: f32, max_x: f32, min_y: f32, max_y: f32) -> Result<Query>
  where
    T: Into<String>,
  {
    let rectangle = XYRectangle::new(min_x, max_x, min_y, max_y)?;
    Ok(XYPointInGeometryQuery::new(field.into(), vec![rectangle.into()])?.into())
  }

  /// Create a query for matching points within the specified distance of the supplied location.
  ///
  /// # Parameters
  ///
  /// - `field`: field name.
  /// - `x`: x at the center.
  /// - `y`: y at the center.
  /// - `radius`: maximum distance from the center in cartesian units: must be non-negative and
  ///   finite.
  ///
  /// # Returns
  ///
  /// Query matching points within this distance
  ///
  /// # Errors
  ///
  /// Returns an error if the location or radius is invalid.
  pub fn new_distance_query<T>(field: T, x: f32, y: f32, radius: f32) -> Result<Query>
  where
    T: Into<String>,
  {
    let circle = XYCircle::new(x, y, radius)?;
    Ok(XYPointInGeometryQuery::new(field.into(), vec![circle.into()])?.into())
  }

  /// Create a query for matching one or more polygons.
  ///
  /// # Parameters
  ///
  /// - `field`: field name.
  /// - `polygons`: array of polygons. must not be empty
  ///
  /// # Returns
  ///
  /// Query matching points within this polygon
  ///
  /// # Errors
  ///
  /// Returns an error if `polygons` is empty.
  ///
  /// See [`Polygon`](crate::core::geo::polygon::Polygon).
  pub fn new_polygon_query<T, I>(field: T, polygons: I) -> Result<Query>
  where
    T: Into<String>,
    I: IntoIterator<Item = XYPolygon>,
  {
    Self::new_geometry_query(field, polygons)
  }

  /// create a query to find all indexed shapes that intersect a provided geometry collection.
  /// XYLine geometries are not supported.
  ///
  /// # Parameters
  ///
  /// - `field`: field name.
  /// - `xy_geometries`: array of geometries. must not be empty.
  ///
  /// # Returns
  ///
  /// Query matching points within this geometry collection.
  ///
  /// # Errors
  ///
  /// Returns an error if `xy_geometries` is empty or contains an unsupported [`XYLine`](crate::core::geo::xy_line::XYLine) geometry.
  ///
  /// See [`XYGeometry`].
  pub fn new_geometry_query<S, T, I>(field: S, xy_geometries: I) -> Result<Query>
  where
    S: Into<String>,
    T: XYGeometry + Into<XYGeometryEnum>,
    I: IntoIterator<Item = T>,
  {
    let xy_geometries: Vec<XYGeometryEnum> = xy_geometries.into_iter().map(Into::into).collect();
    Ok(XYPointInGeometryQuery::new(field.into(), xy_geometries)?.into())
  }
}

impl Display for XYPointField {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    let bytes = match self.parent_field.fields_data {
      FieldDataEnum::Binary(ref bytes) => bytes,
      _ => {
        return Err(std::fmt::Error);
      },
    };

    let x = XYEncodingUtils::decode_bytes(bytes.bytes.as_slice(), 0);
    let y = XYEncodingUtils::decode_bytes(bytes.bytes.as_slice(), BitUtil::INT_BYTES);

    write!(f, "XYPointField <{}:{},{}>", self.parent_field.name, x, y)
  }
}

impl IndexableField for XYPointField {
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

impl FieldBase for XYPointField {}
