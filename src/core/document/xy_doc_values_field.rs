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
use crate::core::document::xy_doc_values_point_in_geometry_query::XYDocValuesPointInGeometryQuery;
use crate::core::document::xy_point_sort_field::XYPointSortField;
use crate::core::geo::xy_circle::XYCircle;
use crate::core::geo::xy_encoding_utils::XYEncodingUtils;
use crate::core::geo::xy_geometry::{XYGeometry, XYGeometryEnum};
use crate::core::geo::xy_polygon::XYPolygon;
use crate::core::geo::xy_rectangle::XYRectangle;
use crate::core::index::BinaryValueEnum;
use crate::core::index::BytesRef;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::indexable_field::{
  IndexableField, IndexingTokenStream, ReusedIndexingTokenStream,
};
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::search::query::Query;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::number::Number;
use std::borrow::Cow;
use std::fmt::{Display, Formatter};
use std::sync::LazyLock;

/// Type for a XYDocValuesField
///
/// Each value stores an `i64` where the upper 32 bits are the encoded x value, and the
/// lower 32 bits are the encoded y value.
///
/// See [`XYEncodingUtils::decode`].
pub(crate) static TYPE: LazyLock<FieldType> = LazyLock::new(|| {
  let mut ft = FieldType::new();
  expect_invariant!(
    ft.set_doc_values_type(DocValuesType::SortedNumeric),
    "the built-in XY doc-values field type is mutable and unfrozen"
  );
  ft.freeze();
  ft
});

/// An per-document location field.
///
/// Sorting by distance is efficient. Multiple values for the same field in one document is
/// allowed.
///
/// This field defines static factory methods for common operations:
///
/// - `new_slow_box_query` for matching points within a bounding box.
/// - `new_slow_distance_query` for matching points within a specified
///   distance.
/// - `new_slow_polygon_query` for matching points within an arbitrary
///   polygon.
/// - `new_slow_geometry_query` for matching points within an
///   arbitrary geometry.
/// - `new_distance_sort` for ordering documents by distance from a
///   specified location.
///
/// If you also need query operations, you should add a separate [`XYPointField`](crate::core::document::xy_point_field::XYPointField) instance. If
/// you also need to store the value, you should add a separate [`StoredField`](crate::core::document::stored_field::StoredField) instance.
///
/// See [`XYPointField`](crate::core::document::xy_point_field::XYPointField).
pub struct XYDocValuesField {
  parent_field: Field,
}

impl XYDocValuesField {
  /// Creates a new XYDocValuesField with the specified x and y
  ///
  /// # Parameters
  ///
  /// - `name`: field name
  /// - `x`: x value.
  /// - `y`: y values.
  ///
  /// # Errors
  ///
  /// Returns an error if `x` or `y` is infinite or NaN.
  pub fn new<T>(name: T, x: f32, y: f32) -> Result<Self>
  where
    T: Into<Cow<'static, str>>,
  {
    let mut field = Self {
      parent_field: Field::new(name.into(), 0i64, TYPE.clone()),
    };
    field.set_location_value(x, y)?;
    Ok(field)
  }

  /// Change the values of this field
  ///
  /// # Parameters
  ///
  /// - `x`: x value.
  /// - `y`: y value.
  ///
  /// # Errors
  ///
  /// Returns an error if x or y are infinite or NaN.
  pub fn set_location_value(&mut self, x: f32, y: f32) -> Result<()> {
    let x_encoded = XYEncodingUtils::encode(x)?;
    let y_encoded = XYEncodingUtils::encode(y)?;
    let value = ((x_encoded as i64) << 32) | (y_encoded as u32 as i64);
    self.parent_field.fields_data = value.into();
    Ok(())
  }

  /// Checks field information and returns an error if it is definitely not an [`XYDocValuesField`](crate::core::document::xy_doc_values_field::XYDocValuesField).
  pub(crate) fn check_compatible(field_info: &FieldInfo) -> Result<()> {
    if *field_info.get_doc_values_type() != DocValuesType::None
      && field_info.get_doc_values_type() != TYPE.doc_values_type()
    {
      return Err(LuceneError::illegal_argument(format!(
        "field=\"{}\" was indexed with docValuesType={:?} but this type has docValuesType={:?}, is the field really a XYDocValuesField?",
        field_info.name,
        field_info.get_doc_values_type(),
        TYPE.doc_values_type()
      )));
    }
    Ok(())
  }

  /// Creates a SortField for sorting by distance from a location.
  ///
  /// This sort orders documents by ascending distance from the location. The value returned in
  /// [`FieldDoc`](crate::core::search::field_doc::FieldDoc) for the hits contains a Double instance with the distance in meters.
  ///
  /// If a document is missing the field, then by default it is treated as having
  /// [`f64::INFINITY`] distance (missing values sort last).
  ///
  /// If a document contains multiple values for the field, the *closest* distance to the
  /// location is used.
  ///
  /// # Parameters
  ///
  /// - `field`: field name.
  /// - `x`: x at the center.
  /// - `y`: y at the center.
  ///
  /// # Returns
  ///
  /// SortField ordering documents by distance
  ///
  /// # Errors
  ///
  /// Returns an error if the location has invalid coordinates.
  pub fn new_distance_sort<T>(field: T, x: f32, y: f32) -> Result<XYPointSortField>
  where
    T: Into<String>,
  {
    XYPointSortField::new(field, x, y)
  }

  /// Create a query for matching a bounding box using doc values. This query is usually slow as it
  /// does not use an index structure and needs to verify documents one-by-one in order to know
  /// whether they match. It is best used wrapped in an [`IndexOrDocValuesQuery`](crate::core::search::index_or_doc_values_query::IndexOrDocValuesQuery) alongside a
  /// [`XYPointField::new_box_query`](crate::core::document::xy_point_field::XYPointField::new_box_query).
  pub fn new_slow_box_query<T>(
    field: T,
    min_x: f32,
    max_x: f32,
    min_y: f32,
    max_y: f32,
  ) -> Result<Query>
  where
    T: Into<String>,
  {
    let rectangle = XYRectangle::new(min_x, max_x, min_y, max_y)?;
    Ok(XYDocValuesPointInGeometryQuery::new(field.into(), vec![rectangle.into()])?.into())
  }

  /// Create a query for matching points within the specified distance of the supplied location. This
  /// query is usually slow as it does not use an index structure and needs to verify documents
  /// one-by-one in order to know whether they match. It is best used wrapped in an
  /// [`IndexOrDocValuesQuery`](crate::core::search::index_or_doc_values_query::IndexOrDocValuesQuery) alongside a [`XYPointField::new_distance_query`](crate::core::document::xy_point_field::XYPointField::new_distance_query).
  ///
  /// # Parameters
  ///
  /// - `field`: field name.
  /// - `x`: x at the center.
  /// - `y`: y at the center: must be within standard +/-180 coordinate bounds.
  /// - `radius`: maximum distance from the center in cartesian distance: must be non-negative and
  ///   finite.
  ///
  /// # Returns
  ///
  /// Query matching points within this distance
  ///
  /// # Errors
  ///
  /// Returns an error if the location or radius is invalid.
  pub fn new_slow_distance_query<T>(field: T, x: f32, y: f32, radius: f32) -> Result<Query>
  where
    T: Into<String>,
  {
    let circle = XYCircle::new(x, y, radius)?;
    Ok(XYDocValuesPointInGeometryQuery::new(field.into(), vec![circle.into()])?.into())
  }

  /// Create a query for matching points within the supplied polygons. This query is usually slow as
  /// it does not use an index structure and needs to verify documents one-by-one in order to know
  /// whether they match. It is best used wrapped in an [`IndexOrDocValuesQuery`](crate::core::search::index_or_doc_values_query::IndexOrDocValuesQuery) alongside a
  /// [`XYPointField::new_polygon_query`](crate::core::document::xy_point_field::XYPointField::new_polygon_query).
  ///
  /// # Parameters
  ///
  /// - `field`: field name.
  /// - `polygons`: array of polygons. must not be empty.
  ///
  /// # Returns
  ///
  /// Query matching points within the given polygons.
  ///
  /// # Errors
  ///
  /// Returns an error if `polygons` is empty.
  pub fn new_slow_polygon_query<T, I>(field: T, polygons: I) -> Result<Query>
  where
    T: Into<String>,
    I: IntoIterator<Item = XYPolygon>,
  {
    Self::new_slow_geometry_query(field, polygons)
  }

  /// Create a query for matching points within the supplied geometries. XYLine geometries are not
  /// supported. This query is usually slow as it does not use an index structure and needs to verify
  /// documents one-by-one in order to know whether they match. It is best used wrapped in an
  /// [`IndexOrDocValuesQuery`](crate::core::search::index_or_doc_values_query::IndexOrDocValuesQuery) alongside a [`XYPointField::new_geometry_query`](crate::core::document::xy_point_field::XYPointField::new_geometry_query).
  ///
  /// # Parameters
  ///
  /// - `field`: field name.
  /// - `geometries`: array of XY geometries. must not be empty.
  ///
  /// # Returns
  ///
  /// Query matching points within the given geometries.
  ///
  /// # Errors
  ///
  /// Returns an error if `geometries` is empty or contains an unsupported [`XYLine`](crate::core::geo::xy_line::XYLine) geometry.
  pub fn new_slow_geometry_query<S, T, I>(field: S, geometries: I) -> Result<Query>
  where
    S: Into<String>,
    T: XYGeometry + Into<XYGeometryEnum>,
    I: IntoIterator<Item = T>,
  {
    let geometries: Vec<XYGeometryEnum> = geometries.into_iter().map(Into::into).collect();
    Ok(XYDocValuesPointInGeometryQuery::new(field.into(), geometries)?.into())
  }
}

impl Display for XYDocValuesField {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    let current_value = match self.parent_field.fields_data {
      FieldDataEnum::Number(Number::I64(v)) => v as u64,
      _ => {
        return Err(std::fmt::Error);
      },
    };
    let x = XYEncodingUtils::decode((current_value >> 32) as i32);
    let y = XYEncodingUtils::decode(current_value as u32 as i32);
    write!(
      f,
      "XYDocValuesField <{}:{},{}>",
      self.parent_field.name, x, y
    )
  }
}

impl IndexableField for XYDocValuesField {
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

impl FieldBase for XYDocValuesField {}
