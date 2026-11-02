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
use crate::core::geo::geo_utils::WindingOrder;
use crate::core::geo::geometry::Geometry;
use crate::core::geo::polygon2d;
use crate::core::geo::polygon2d::Polygon2D;
use crate::core::geo::xy_encoding_utils::XYEncodingUtils;
use crate::core::geo::xy_geometry::XYGeometry;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::core_helper::CoreHelper;
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;

/// Represents a polygon in cartesian space. You can construct the Polygon directly with `Vec<f32>`,
/// `Vec<f32>` x, y arrays coordinates.
#[derive(Clone, Debug)]
pub struct XYPolygon {
  x: Box<[f32]>,
  y: Box<[f32]>,
  holes: Box<[XYPolygon]>,

  /// minimum x of this polygon's bounding box area
  pub min_x: f32,

  /// maximum x of this polygon's bounding box area
  pub max_x: f32,

  /// minimum y of this polygon's bounding box area
  pub min_y: f32,

  /// maximum y of this polygon's bounding box area
  pub max_y: f32,

  /// winding order of the vertices
  winding_order: WindingOrder,
}

impl XYPolygon {
  /// Creates a new Polygon from the supplied x, y arrays, and optionally any holes.
  pub fn new<A, B, H>(x: A, y: B, holes: H) -> Result<Self>
  where
    A: Into<Vec<f32>>,
    B: Into<Vec<f32>>,
    H: Into<Vec<XYPolygon>>,
  {
    let x = x.into();
    let y = y.into();
    let holes = holes.into();
    if x.len() != y.len() {
      return Err(LuceneError::illegal_argument(
        "x and y must be equal length",
      ));
    }
    if x.len() < 4 {
      return Err(LuceneError::illegal_argument(
        "at least 4 polygon points required",
      ));
    }
    if x[0] != x[x.len() - 1] {
      return Err(LuceneError::illegal_argument(format!(
        "first and last points of the polygon must be the same (it must close itself): x[0]={} x[{}]={}",
        x[0],
        x.len() - 1,
        x[x.len() - 1]
      )));
    }
    if y[0] != y[y.len() - 1] {
      return Err(LuceneError::illegal_argument(format!(
        "first and last points of the polygon must be the same (it must close itself): y[0]={} y[{}]={}",
        y[0],
        y.len() - 1,
        y[y.len() - 1]
      )));
    }
    for inner in &holes {
      if !inner.holes.is_empty() {
        return Err(LuceneError::illegal_argument(
          "holes may not contain holes: polygons may not nest.",
        ));
      }
    }

    let mut min_x = XYEncodingUtils::check_val(x[0])?;
    let mut max_x = x[0];
    let mut min_y = XYEncodingUtils::check_val(y[0])?;
    let mut max_y = y[0];

    let mut winding_sum = 0f64;
    let num_pts = x.len() - 1;
    let mut i = 1usize;
    let mut j = 0usize;
    while i < num_pts {
      min_x = CoreHelper::min_f32(XYEncodingUtils::check_val(x[i])?, min_x);
      max_x = CoreHelper::max_f32(x[i], max_x);
      min_y = CoreHelper::min_f32(XYEncodingUtils::check_val(y[i])?, min_y);
      max_y = CoreHelper::max_f32(y[i], max_y);

      winding_sum += ((x[j] - x[num_pts]) * (y[i] - y[num_pts])
        - (y[j] - y[num_pts]) * (x[i] - x[num_pts])) as f64;

      j = i;
      i += 1;
    }

    let winding_order = if winding_sum < 0f64 {
      WindingOrder::Ccw
    } else {
      WindingOrder::Cw
    };

    Ok(Self {
      x: x.into_boxed_slice(),
      y: y.into_boxed_slice(),
      holes: holes.into_boxed_slice(),
      min_x,
      max_x,
      min_y,
      max_y,
      winding_order,
    })
  }
  /// returns the number of vertex points
  pub fn num_points(&self) -> usize {
    self.x.len()
  }

  /// Borrows the internal x array
  pub fn get_poly_x(&self) -> &[f32] {
    &self.x
  }

  /// Returns x value at given index
  pub fn get_poly_x_at(&self, vertex: usize) -> f32 {
    self.x[vertex]
  }

  /// Borrows the internal y array
  pub fn get_poly_y(&self) -> &[f32] {
    &self.y
  }

  /// Returns y value at given index
  pub fn get_poly_y_at(&self, vertex: usize) -> f32 {
    self.y[vertex]
  }

  /// Borrows the internal holes array
  pub fn get_holes(&self) -> &[XYPolygon] {
    &self.holes
  }

  #[allow(dead_code)]
  pub(crate) fn get_hole(&self, i: usize) -> &XYPolygon {
    &self.holes[i]
  }

  /// Returns the winding order (CW, COLINEAR, CCW) for the polygon shell
  pub fn get_winding_order(&self) -> WindingOrder {
    self.winding_order
  }

  /// returns the number of holes for the polygon
  pub fn num_holes(&self) -> usize {
    self.holes.len()
  }
}
impl PartialEq for XYPolygon {
  fn eq(&self, other: &Self) -> bool {
    self.holes == other.holes
      && CoreHelper::array_equals_f32(&self.x, &other.x)
      && CoreHelper::array_equals_f32(&self.y, &other.y)
  }
}

impl Eq for XYPolygon {}

impl std::hash::Hash for XYPolygon {
  fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
    for hole in &self.holes {
      hole.hash(state);
    }
    for x in &self.x {
      (BitUtil::float_to_int_bits(*x) as u32).hash(state);
    }
    for y in &self.y {
      (BitUtil::float_to_int_bits(*y) as u32).hash(state);
    }
  }
}
impl std::fmt::Display for XYPolygon {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "XYPolygon")?;
    for i in 0..self.x.len() {
      write!(f, "[{}, {}] ", self.x[i], self.y[i])?;
    }
    if !self.holes.is_empty() {
      write!(f, ", holes=[")?;
      for (i, hole) in self.holes.iter().enumerate() {
        if i > 0 {
          write!(f, ", ")?;
        }
        write!(f, "{hole}")?;
      }
      write!(f, "]")?;
    }
    Ok(())
  }
}
impl Geometry for XYPolygon {
  type Component2D = Polygon2D;

  fn to_component2d(&self) -> Result<Self::Component2D> {
    polygon2d::create_from_xy_polygon(self)
  }
}

impl XYGeometry for XYPolygon {}
