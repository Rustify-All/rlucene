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
use crate::core::geo::geo_utils::GeoUtils;
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
use std::fmt::{Display, Formatter};
use std::sync::LazyLock;
/// Shape utilities used for both LatLon (spherical) and XY (cartesian) shape fields.
///
/// [`Polygon`](crate::core::geo::polygon::Polygon)'s and [`Line`](crate::core::geo::line::Line)'s are decomposed into a triangular mesh using the [`Tessellator`](crate::core::geo::tessellator::Tessellator)
/// utility. Each [`Triangle`] is encoded by this type and indexed as a seven-dimensional
/// multi-value field.
///
/// Finding all shapes that intersect a range (e.g., bounding box), or target shape, at search
/// time is efficient.
///
/// This struct defines associated methods for encoding the three vertices of tessellated
/// triangles as a seven dimension point. The coordinates are converted from double precision
/// values into 32 bit integers so they are sortable at index time.
pub struct ShapeField;
impl ShapeField {
  pub const BYTES: usize = BitUtil::INT_BYTES;
}

/// tessellated triangles are seven dimensions; the first four are the bounding box index dimensions
pub(crate) static TYPE_: LazyLock<FieldType> = LazyLock::new(|| {
  let mut ft = FieldType::new();
  expect_invariant!(
    ft.set_dimensions_with_index(7, 4, ShapeField::BYTES),
    "the built-in tessellated shape dimensions are valid"
  );
  ft.freeze();
  ft
});
/// polygons are decomposed into tessellated triangles using Tessellator these triangles are encoded and inserted as separate indexed POINT fields
pub struct Triangle {
  parent_field: Field,
}

impl Triangle {
  /// Creates a triangle field for points and lines.
  pub(crate) fn new(
    name: &str,
    a_x_encoded: i32,
    a_y_encoded: i32,
    b_x_encoded: i32,
    b_y_encoded: i32,
    c_x_encoded: i32,
    c_y_encoded: i32,
  ) -> Result<Self> {
    let mut triangle = Self {
      parent_field: Field::new(name.to_owned(), Dummy(()), TYPE_.clone()),
    };
    triangle.set_triangle_value(
      a_x_encoded,
      a_y_encoded,
      true,
      b_x_encoded,
      b_y_encoded,
      true,
      c_x_encoded,
      c_y_encoded,
      true,
    )?;
    Ok(triangle)
  }

  // TODO IMPORTANT Tessellator 未实现
  // /// xtor from a given Tessellated Triangle object
  // pub fn from_tessellated_triangle(name: &str, t: &TessellatorTriangle) -> Self {
  //     let mut triangle = Self {
  //         parent_field: Field::new(name.to_owned(), TYPE),
  //     };
  //     triangle.set_triangle_value(
  //         t.get_encoded_x(0),
  //         t.get_encoded_y(0),
  //         t.is_edge_from_polygon(0),
  //         t.get_encoded_x(1),
  //         t.get_encoded_y(1),
  //         t.is_edge_from_polygon(1),
  //         t.get_encoded_x(2),
  //         t.get_encoded_y(2),
  //         t.is_edge_from_polygon(2),
  //     );
  //     triangle
  // }

  /// sets the vertices of the triangle as integer encoded values
  #[allow(clippy::too_many_arguments)]
  pub(crate) fn set_triangle_value(
    &mut self,
    a_x: i32,
    a_y: i32,
    ab_from_shape: bool,
    b_x: i32,
    b_y: i32,
    bc_from_shape: bool,
    c_x: i32,
    c_y: i32,
    ca_from_shape: bool,
  ) -> Result<()> {
    if matches!(self.parent_field.fields_data, FieldDataEnum::Dummy(_)) {
      self.parent_field.fields_data =
        FieldDataEnum::Binary(BytesRef::from_bytes(vec![0u8; 7 * ShapeField::BYTES]));
    }

    let bytes = match &mut self.parent_field.fields_data {
      FieldDataEnum::Binary(v) => v.bytes.as_mut_slice(),
      _ => {
        return Err(LuceneError::illegal_state("should not be here"));
      },
    };
    encode_triangle(
      bytes,
      a_y,
      a_x,
      ab_from_shape,
      b_y,
      b_x,
      bc_from_shape,
      c_y,
      c_x,
      ca_from_shape,
    )
  }
}
impl FieldBase for Triangle {}

impl Display for Triangle {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    self.parent_field.fmt(f)
  }
}

impl IndexableField for Triangle {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum QueryRelation {
  /// used for INTERSECT Queries
  Intersects,
  /// used for WITHIN Queries
  Within,
  /// used for DISJOINT Queries
  Disjoint,
  /// used for CONTAINS Queries
  Contains,
}

const MINY_MINX_MAXY_MAXX_Y_X: i32 = 0;
const MINY_MINX_Y_X_MAXY_MAXX: i32 = 1;
const MAXY_MINX_Y_X_MINY_MAXX: i32 = 2;
const MAXY_MINX_MINY_MAXX_Y_X: i32 = 3;
const Y_MINX_MINY_X_MAXY_MAXX: i32 = 4;
const Y_MINX_MINY_MAXX_MAXY_X: i32 = 5;
const MAXY_MINX_MINY_X_Y_MAXX: i32 = 6;
const MINY_MINX_Y_MAXX_MAXY_X: i32 = 7;

/// A triangle is encoded using 6 points and an extra point with encoded information in three bits
/// of how to reconstruct it. Triangles are encoded with CCW orientation and might be rotated to
/// limit the number of possible reconstructions to 2^3. Reconstruction always happens from west to
/// east.
#[allow(clippy::too_many_arguments)]
pub fn encode_triangle(
  bytes: &mut [u8],
  mut a_y: i32,
  mut a_x: i32,
  mut ab: bool,
  mut b_y: i32,
  mut b_x: i32,
  mut bc: bool,
  mut c_y: i32,
  mut c_x: i32,
  mut ca: bool,
) -> Result<()> {
  debug_assert_eq!(bytes.len(), 7 * ShapeField::BYTES);

  if b_x < a_x || c_x < a_x {
    let temp_x = a_x;
    let temp_y = a_y;
    let temp_bool = ab;
    if b_x < c_x {
      a_x = b_x;
      a_y = b_y;
      ab = bc;
      b_x = c_x;
      b_y = c_y;
      bc = ca;
      c_x = temp_x;
      c_y = temp_y;
      ca = temp_bool;
    } else {
      a_x = c_x;
      a_y = c_y;
      ab = ca;
      c_x = b_x;
      c_y = b_y;
      ca = bc;
      b_x = temp_x;
      b_y = temp_y;
      bc = temp_bool;
    }
  } else if a_x == b_x && a_x == c_x && (b_y < a_y || c_y < a_y) {
    let temp_x = a_x;
    let temp_y = a_y;
    let temp_bool = ab;
    if b_y < c_y {
      a_x = b_x;
      a_y = b_y;
      ab = bc;
      b_x = c_x;
      b_y = c_y;
      bc = ca;
      c_x = temp_x;
      c_y = temp_y;
      ca = temp_bool;
    } else {
      a_x = c_x;
      a_y = c_y;
      ab = ca;
      c_x = b_x;
      c_y = b_y;
      ca = bc;
      b_x = temp_x;
      b_y = temp_y;
      bc = temp_bool;
    }
  }

  if GeoUtils::orient(
    a_x as f64, a_y as f64, b_x as f64, b_y as f64, c_x as f64, c_y as f64,
  ) == -1
  {
    let temp_x = b_x;
    let temp_y = b_y;
    let temp_bool = ab;
    ab = bc;
    b_x = c_x;
    b_y = c_y;
    c_x = temp_x;
    c_y = temp_y;
    ca = temp_bool;
  }

  let min_x = a_x;
  let min_y = a_y.min(b_y.min(c_y));
  let max_x = a_x.max(b_x.max(c_x));
  let max_y = a_y.max(b_y.max(c_y));

  let (mut bits, x, y): (i32, i32, i32);
  if min_y == a_y {
    if max_y == b_y && max_x == b_x {
      y = c_y;
      x = c_x;
      bits = MINY_MINX_MAXY_MAXX_Y_X;
    } else if max_y == c_y && max_x == c_x {
      y = b_y;
      x = b_x;
      bits = MINY_MINX_Y_X_MAXY_MAXX;
    } else {
      y = b_y;
      x = c_x;
      bits = MINY_MINX_Y_MAXX_MAXY_X;
    }
  } else if max_y == a_y {
    if min_y == b_y && max_x == b_x {
      y = c_y;
      x = c_x;
      bits = MAXY_MINX_MINY_MAXX_Y_X;
    } else if min_y == c_y && max_x == c_x {
      y = b_y;
      x = b_x;
      bits = MAXY_MINX_Y_X_MINY_MAXX;
    } else {
      y = c_y;
      x = b_x;
      bits = MAXY_MINX_MINY_X_Y_MAXX;
    }
  } else if max_x == b_x && min_y == b_y {
    y = a_y;
    x = c_x;
    bits = Y_MINX_MINY_MAXX_MAXY_X;
  } else if max_x == c_x && max_y == c_y {
    y = a_y;
    x = b_x;
    bits = Y_MINX_MINY_X_MAXY_MAXX;
  } else {
    return Err(LuceneError::illegal_argument(
      "Could not encode the provided triangle",
    ));
  }

  bits |= if ab { 1 << 3 } else { 0 };
  bits |= if bc { 1 << 4 } else { 0 };
  bits |= if ca { 1 << 5 } else { 0 };

  NumericUtils::int_to_sortable_bytes(min_y, bytes, 0);
  NumericUtils::int_to_sortable_bytes(min_x, bytes, ShapeField::BYTES);
  NumericUtils::int_to_sortable_bytes(max_y, bytes, 2 * ShapeField::BYTES);
  NumericUtils::int_to_sortable_bytes(max_x, bytes, 3 * ShapeField::BYTES);
  NumericUtils::int_to_sortable_bytes(y, bytes, 4 * ShapeField::BYTES);
  NumericUtils::int_to_sortable_bytes(x, bytes, 5 * ShapeField::BYTES);
  NumericUtils::int_to_sortable_bytes(bits, bytes, 6 * ShapeField::BYTES);
  Ok(())
}

/// Decode a triangle encoded by [`encode_triangle`].
pub fn decode_triangle(t: &[u8], triangle: &mut DecodedTriangle) -> Result<()> {
  let bits = NumericUtils::sortable_bytes_to_int(t, 6 * ShapeField::BYTES);
  let t_code = ((1 << 3) - 1) & bits;

  let (a_x, a_y, b_x, b_y, c_x, c_y) = match t_code {
    MINY_MINX_MAXY_MAXX_Y_X => (
      NumericUtils::sortable_bytes_to_int(t, ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 0),
      NumericUtils::sortable_bytes_to_int(t, 3 * ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 2 * ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 5 * ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 4 * ShapeField::BYTES),
    ),
    MINY_MINX_Y_X_MAXY_MAXX => (
      NumericUtils::sortable_bytes_to_int(t, ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 0),
      NumericUtils::sortable_bytes_to_int(t, 5 * ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 4 * ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 3 * ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 2 * ShapeField::BYTES),
    ),
    MAXY_MINX_Y_X_MINY_MAXX => (
      NumericUtils::sortable_bytes_to_int(t, ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 2 * ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 5 * ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 4 * ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 3 * ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 0),
    ),
    MAXY_MINX_MINY_MAXX_Y_X => (
      NumericUtils::sortable_bytes_to_int(t, ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 2 * ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 3 * ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 0),
      NumericUtils::sortable_bytes_to_int(t, 5 * ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 4 * ShapeField::BYTES),
    ),
    Y_MINX_MINY_X_MAXY_MAXX => (
      NumericUtils::sortable_bytes_to_int(t, ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 4 * ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 5 * ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 0),
      NumericUtils::sortable_bytes_to_int(t, 3 * ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 2 * ShapeField::BYTES),
    ),
    Y_MINX_MINY_MAXX_MAXY_X => (
      NumericUtils::sortable_bytes_to_int(t, ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 4 * ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 3 * ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 0),
      NumericUtils::sortable_bytes_to_int(t, 5 * ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 2 * ShapeField::BYTES),
    ),
    MAXY_MINX_MINY_X_Y_MAXX => (
      NumericUtils::sortable_bytes_to_int(t, ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 2 * ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 5 * ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 0),
      NumericUtils::sortable_bytes_to_int(t, 3 * ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 4 * ShapeField::BYTES),
    ),
    MINY_MINX_Y_MAXX_MAXY_X => (
      NumericUtils::sortable_bytes_to_int(t, ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 0),
      NumericUtils::sortable_bytes_to_int(t, 3 * ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 4 * ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 5 * ShapeField::BYTES),
      NumericUtils::sortable_bytes_to_int(t, 2 * ShapeField::BYTES),
    ),
    _ => {
      return Err(LuceneError::illegal_argument(
        "Could not decode the provided triangle",
      ));
    },
  };

  debug_assert!(
    GeoUtils::orient(
      a_x as f64, a_y as f64, b_x as f64, b_y as f64, c_x as f64, c_y as f64
    ) >= 0
  );

  let ab = (bits & (1 << 3)) == (1 << 3);
  let bc = (bits & (1 << 4)) == (1 << 4);
  let ca = (bits & (1 << 5)) == (1 << 5);

  triangle.set_values(a_x, a_y, ab, b_x, b_y, bc, c_x, c_y, ca);
  resolve_triangle_type(triangle);
  Ok(())
}

pub fn resolve_triangle_type(triangle: &mut DecodedTriangle) {
  if triangle.a_x == triangle.b_x && triangle.a_y == triangle.b_y {
    if triangle.a_x == triangle.c_x && triangle.a_y == triangle.c_y {
      triangle.type_ = DecodedTriangleType::Point;
    } else {
      triangle.ab = triangle.bc || triangle.ca;
      triangle.b_x = triangle.c_x;
      triangle.b_y = triangle.c_y;
      triangle.c_x = triangle.a_x;
      triangle.c_y = triangle.a_y;
      triangle.type_ = DecodedTriangleType::Line;
    }
  } else if triangle.a_x == triangle.c_x && triangle.a_y == triangle.c_y {
    triangle.ab = triangle.ab || triangle.bc;
    triangle.type_ = DecodedTriangleType::Line;
  } else if triangle.b_x == triangle.c_x && triangle.b_y == triangle.c_y {
    triangle.ab = triangle.ab || triangle.ca;
    triangle.c_x = triangle.a_x;
    triangle.c_y = triangle.a_y;
    triangle.type_ = DecodedTriangleType::Line;
  } else {
    triangle.type_ = DecodedTriangleType::Triangle;
  }
}
/// Represents a encoded triangle using [`decode_triangle`].
#[derive(Clone, Debug, Default)]
pub struct DecodedTriangle {
  /// x coordinate, vertex one
  pub a_x: i32,

  /// y coordinate, vertex one
  pub a_y: i32,

  /// x coordinate, vertex two
  pub b_x: i32,

  /// y coordinate, vertex two
  pub b_y: i32,

  /// x coordinate, vertex three
  pub c_x: i32,

  /// y coordinate, vertex three
  pub c_y: i32,

  /// represent if edge ab belongs to original shape
  pub ab: bool,

  /// represent if edge bc belongs to original shape
  pub bc: bool,

  /// represent if edge ca belongs to original shape
  pub ca: bool,

  /// triangle type
  pub type_: DecodedTriangleType,
}

/// type of triangle
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum DecodedTriangleType {
  /// all coordinates are equal
  Point,
  /// first and third coordinates are equal
  Line,
  /// all coordinates are different
  #[default]
  Triangle,
}

impl DecodedTriangle {
  /// default xtor
  pub fn new() -> Self {
    Self::default()
  }

  /// Sets the values of the DecodedTriangle
  #[allow(clippy::too_many_arguments)]
  pub(crate) fn set_values(
    &mut self,
    a_x: i32,
    a_y: i32,
    ab: bool,
    b_x: i32,
    b_y: i32,
    bc: bool,
    c_x: i32,
    c_y: i32,
    ca: bool,
  ) {
    self.a_x = a_x;
    self.a_y = a_y;
    self.ab = ab;
    self.b_x = b_x;
    self.b_y = b_y;
    self.bc = bc;
    self.c_x = c_x;
    self.c_y = c_y;
    self.ca = ca;
  }
}

impl PartialEq for DecodedTriangle {
  fn eq(&self, other: &Self) -> bool {
    (self.a_x == other.a_x && self.b_x == other.b_x && self.c_x == other.c_x)
      && (self.a_y == other.a_y && self.b_y == other.b_y && self.c_y == other.c_y)
      && (self.ab == other.ab && self.bc == other.bc && self.ca == other.ca)
  }
}

impl Eq for DecodedTriangle {}

impl std::hash::Hash for DecodedTriangle {
  fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
    self.a_x.hash(state);
    self.a_y.hash(state);
    self.b_x.hash(state);
    self.b_y.hash(state);
    self.c_x.hash(state);
    self.c_y.hash(state);
    self.ab.hash(state);
    self.bc.hash(state);
    self.ca.hash(state);
  }
}

impl std::fmt::Display for DecodedTriangle {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "{}, {} {}, {} {}, {} [{},{},{}]",
      self.a_x, self.a_y, self.b_x, self.b_y, self.c_x, self.c_y, self.ab, self.bc, self.ca
    )
  }
}
