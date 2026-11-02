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
use crate::core::geo::geo_utils::GeoUtils;
use crate::core::geo::rectangle::Rectangle;
use crate::core::util::core_helper::CoreHelper;
use crate::core::util::error::lucene_error::{LuceneError, Result};
/// Internal tree node: represents geometry edge from `[x1, y1]` to `[x2, y2]`.
/// The sort value is `low`, which is the minimum y of the edge.
/// `max` stores the maximum y of this edge or any children.
///
/// Construction takes `O(n log n)` time for sorting and tree construction.
/// Methods are `O(n)`, but for most practical lines and polygons are much faster
/// than brute force.
#[derive(Default)]
pub struct EdgeTree {
  pub(crate) y1: f64,
  pub(crate) y2: f64,
  pub(crate) x1: f64,
  pub(crate) x2: f64,

  /// Min Y of this edge.
  pub(crate) low: f64,

  /// Max Y of this edge or any children.
  pub(crate) max: f64,
  /// left child edge, or None
  pub(crate) left: Option<Box<EdgeTree>>,
  /// right child edge, or None
  pub(crate) right: Option<Box<EdgeTree>>,
}
impl EdgeTree {
  /// helper bytes to signal if a point is on an edge, it is within the edge tree or disjoint
  pub(crate) const FALSE: u8 = 0x00;
  pub(crate) const TRUE: u8 = 0x01;
  pub(crate) const ON_EDGE: u8 = 0x02;
  fn new(x1: f64, y1: f64, x2: f64, y2: f64, low: f64, max: f64) -> Self {
    Self {
      y1,
      y2,
      x1,
      x2,
      low,
      max,
      left: None,
      right: None,
    }
  }
  pub(crate) fn contains(&self, x: f64, y: f64) -> bool {
    self.contains_pn_poly(x, y) > Self::FALSE
  }

  /**
   * Returns byte 0x00 if the point crosses this edge subtree an even number of times. Returns byte
   * 0x01 if the point crosses this edge subtree an odd number of times. Returns byte 0x02 if the
   * point is on one of the edges.
   *
   * See the [PNPOLY description](https://www.ecse.rpi.edu/~wrf/Research/Short_Notes/pnpoly.html)
   * for more information.
   */
  // Ported from https://www.ecse.rpi.edu/~wrf/Research/Short_Notes/pnpoly.html.
  // original code under the BSD license
  // (https://www.ecse.rpi.edu/~wrf/Research/Short_Notes/pnpoly.html#License%20to%20Use)
  //
  // Copyright (c) 1970-2003, Wm. Randolph Franklin
  //
  // Permission is hereby granted, free of charge, to any person obtaining a copy of this software
  // and associated
  // documentation files (the "Software"), to deal in the Software without restriction, including
  // without limitation
  // the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of
  // the Software, and
  // to permit persons to whom the Software is furnished to do so, subject to the following
  // conditions:
  //
  // 1. Redistributions of source code must retain the above copyright
  //    notice, this list of conditions and the following disclaimers.
  // 2. Redistributions in binary form must reproduce the above copyright
  //    notice in the documentation and/or other materials provided with
  //    the distribution.
  // 3. The name of W. Randolph Franklin may not be used to endorse or
  //    promote products derived from this Software without specific
  //    prior written permission.
  //
  // THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING
  // BUT NOT LIMITED
  // TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN
  // NO EVENT SHALL
  // THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER
  // IN AN ACTION OF
  // CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE
  // OR OTHER DEALINGS
  // IN THE SOFTWARE.
  pub(crate) fn contains_pn_poly(&self, x: f64, y: f64) -> u8 {
    let mut res = Self::FALSE;
    if y <= self.max {
      if (y == self.y1 && y == self.y2)
        || ((y <= self.y1 && y >= self.y2) != (y >= self.y1 && y <= self.y2))
      {
        if (x == self.x1 && x == self.x2)
          || (((x <= self.x1 && x >= self.x2) != (x >= self.x1 && x <= self.x2))
            && GeoUtils::orient(self.x1, self.y1, self.x2, self.y2, x, y) == 0)
        {
          return Self::ON_EDGE;
        } else if (self.y1 > y) != (self.y2 > y) {
          res = if x < (self.x2 - self.x1) * (y - self.y1) / (self.y2 - self.y1) + self.x1 {
            Self::TRUE
          } else {
            Self::FALSE
          };
        }
      }

      if let Some(left) = &self.left {
        res ^= left.contains_pn_poly(x, y);
        if (res & 0x02) == 0x02 {
          return Self::ON_EDGE;
        }
      }

      if let Some(right) = &self.right
        && y >= self.low
      {
        res ^= right.contains_pn_poly(x, y);
        if (res & 0x02) == 0x02 {
          return Self::ON_EDGE;
        }
      }
    }
    debug_assert!((Self::FALSE..=Self::ON_EDGE).contains(&res));
    res
  }
  /// returns true if the provided x, y point lies on the line
  pub(crate) fn is_point_online(&self, x: f64, y: f64) -> bool {
    if y <= self.max {
      let a1x = self.x1;
      let a1y = self.y1;
      let b1x = self.x2;
      let b1y = self.y2;
      let outside = (a1y < y && b1y < y)
        || (a1y > y && b1y > y)
        || (a1x < x && b1x < x)
        || (a1x > x && b1x > x);
      if !outside && GeoUtils::orient(a1x, a1y, b1x, b1y, x, y) == 0 {
        return true;
      }
      if let Some(left) = &self.left
        && left.is_point_online(x, y)
      {
        return true;
      }
      if let Some(right) = &self.right
        && y >= self.low
        && right.is_point_online(x, y)
      {
        return true;
      }
    }
    false
  }
  #[allow(clippy::too_many_arguments)]
  /// Returns true if the triangle crosses any edge in this edge subtree
  pub(crate) fn crosses_triangle(
    &self,
    min_x: f64,
    max_x: f64,
    min_y: f64,
    max_y: f64,
    ax: f64,
    ay: f64,
    bx: f64,
    by: f64,
    cx: f64,
    cy: f64,
    include_boundary: bool,
  ) -> bool {
    if min_y <= self.max {
      let dy = self.y1;
      let ey = self.y2;
      let dx = self.x1;
      let ex = self.x2;
      // optimization: see if the rectangle is outside of the "bounding box" of the polyline at all
      // if not, don't waste our time trying more complicated stuff
      let outside = (dy < min_y && ey < min_y)
        || (dy > max_y && ey > max_y)
        || (dx < min_x && ex < min_x)
        || (dx > max_x && ex > max_x);

      if !outside {
        if include_boundary {
          if GeoUtils::line_crosses_line_with_boundary(dx, dy, ex, ey, ax, ay, bx, by)
            || GeoUtils::line_crosses_line_with_boundary(dx, dy, ex, ey, bx, by, cx, cy)
            || GeoUtils::line_crosses_line_with_boundary(dx, dy, ex, ey, cx, cy, ax, ay)
          {
            return true;
          }
        } else if GeoUtils::line_crosses_line(dx, dy, ex, ey, ax, ay, bx, by)
          || GeoUtils::line_crosses_line(dx, dy, ex, ey, bx, by, cx, cy)
          || GeoUtils::line_crosses_line(dx, dy, ex, ey, cx, cy, ax, ay)
        {
          return true;
        }
      }

      if let Some(left) = &self.left
        && left.crosses_triangle(
          min_x,
          max_x,
          min_y,
          max_y,
          ax,
          ay,
          bx,
          by,
          cx,
          cy,
          include_boundary,
        )
      {
        return true;
      }

      if let Some(right) = &self.right
        && max_y >= self.low
        && right.crosses_triangle(
          min_x,
          max_x,
          min_y,
          max_y,
          ax,
          ay,
          bx,
          by,
          cx,
          cy,
          include_boundary,
        )
      {
        return true;
      }
    }
    false
  }
  /// Returns true if the box crosses any edge in this edge subtree
  pub(crate) fn crosses_box(
    &self,
    min_x: f64,
    max_x: f64,
    min_y: f64,
    max_y: f64,
    include_boundary: bool,
  ) -> bool {
    // we just have to cross one edge to answer the question, so we descend the tree and return when
    // we do.
    if min_y <= self.max {
      let cy = self.y1;
      let dy = self.y2;
      let cx = self.x1;
      let dx = self.x2;

      if Rectangle::contains_point(cy, cx, min_y, max_y, min_x, max_x)
        || Rectangle::contains_point(dy, dx, min_y, max_y, min_x, max_x)
      {
        return true;
      }
      // optimization: see if either end of the line segment is contained by the rectangle
      let outside = (cy < min_y && dy < min_y)
        || (cy > max_y && dy > max_y)
        || (cx < min_x && dx < min_x)
        || (cx > max_x && dx > max_x);
      // optimization: see if the rectangle is outside of the "bounding box" of the polyline at all
      // if not, don't waste our time trying more complicated stuff
      if !outside {
        if include_boundary {
          if GeoUtils::line_crosses_line_with_boundary(cx, cy, dx, dy, min_x, min_y, max_x, min_y)
            || GeoUtils::line_crosses_line_with_boundary(cx, cy, dx, dy, max_x, min_y, max_x, max_y)
            || GeoUtils::line_crosses_line_with_boundary(cx, cy, dx, dy, max_x, max_y, min_x, max_y)
            || GeoUtils::line_crosses_line_with_boundary(cx, cy, dx, dy, min_x, max_y, min_x, min_y)
          {
            // include boundaries: ensures box edges that terminate on the polygon are included
            return true;
          }
        } else if GeoUtils::line_crosses_line(cx, cy, dx, dy, min_x, min_y, max_x, min_y)
          || GeoUtils::line_crosses_line(cx, cy, dx, dy, max_x, min_y, max_x, max_y)
          || GeoUtils::line_crosses_line(cx, cy, dx, dy, max_x, max_y, min_x, max_y)
          || GeoUtils::line_crosses_line(cx, cy, dx, dy, min_x, max_y, min_x, min_y)
        {
          return true;
        }
      }

      if let Some(left) = &self.left
        && left.crosses_box(min_x, max_x, min_y, max_y, include_boundary)
      {
        return true;
      }

      if let Some(right) = &self.right
        && max_y >= self.low
        && right.crosses_box(min_x, max_x, min_y, max_y, include_boundary)
      {
        return true;
      }
    }
    false
  }
  /// Returns true if the line crosses any edge in this edge subtree
  #[allow(clippy::too_many_arguments)]
  pub(crate) fn crosses_line(
    &self,
    min_x: f64,
    max_x: f64,
    min_y: f64,
    max_y: f64,
    a2x: f64,
    a2y: f64,
    b2x: f64,
    b2y: f64,
    include_boundary: bool,
  ) -> bool {
    if min_y <= self.max {
      let a1x = self.x1;
      let a1y = self.y1;
      let b1x = self.x2;
      let b1y = self.y2;

      let outside = (a1y < min_y && b1y < min_y)
        || (a1y > max_y && b1y > max_y)
        || (a1x < min_x && b1x < min_x)
        || (a1x > max_x && b1x > max_x);
      if !outside {
        if include_boundary {
          if GeoUtils::line_crosses_line_with_boundary(a1x, a1y, b1x, b1y, a2x, a2y, b2x, b2y) {
            return true;
          }
        } else if GeoUtils::line_crosses_line(a1x, a1y, b1x, b1y, a2x, a2y, b2x, b2y) {
          return true;
        }
      }
      if let Some(left) = &self.left
        && left.crosses_line(
          min_x,
          max_x,
          min_y,
          max_y,
          a2x,
          a2y,
          b2x,
          b2y,
          include_boundary,
        )
      {
        return true;
      }
      if let Some(right) = &self.right
        && max_y >= self.low
        && right.crosses_line(
          min_x,
          max_x,
          min_y,
          max_y,
          a2x,
          a2y,
          b2x,
          b2y,
          include_boundary,
        )
      {
        return true;
      }
    }
    false
  }
}
/// Creates an edge interval tree from a set of geometry vertices.
pub(crate) fn create_tree(x: &[f64], y: &[f64]) -> Result<EdgeTree> {
  if x.len() != y.len() {
    return Err(LuceneError::illegal_argument(
      "x and y must be equal length",
    ));
  }
  if x.len() < 2 {
    return Err(LuceneError::illegal_argument(
      "at least 2 geometry points required",
    ));
  }

  if x.len() == 2 {
    return Ok(EdgeTree::new(
      x[0],
      y[0],
      x[1],
      y[1],
      CoreHelper::min_f64(y[0], y[1]),
      CoreHelper::max_f64(y[0], y[1]),
    ));
  }

  let mut edges = Vec::with_capacity(x.len() - 1);
  for i in 1..x.len() {
    let x1 = x[i - 1];
    let y1 = y[i - 1];
    let x2 = x[i];
    let y2 = y[i];
    edges.push(EdgeTree::new(
      x1,
      y1,
      x2,
      y2,
      CoreHelper::min_f64(y1, y2),
      CoreHelper::max_f64(y1, y2),
    ));
  }

  edges.sort_by(|left, right| {
    let ret = CoreHelper::compare_f64(left.low, right.low);
    if ret.is_eq() {
      CoreHelper::compare_f64(left.max, right.max)
    } else {
      ret
    }
  });

  let high = edges.len() - 1;
  let root = create_tree_from_edges(&mut edges, 0, high)
    .ok_or_else(|| LuceneError::illegal_state("edge tree root is missing"))?;
  Ok(root)
}
/// Creates tree from sorted edges (with range low and high inclusive)
fn create_tree_from_edges(edges: &mut [EdgeTree], low: usize, high: usize) -> Option<EdgeTree> {
  if low > high {
    return None;
  }

  let mid = low + ((high - low) >> 1);
  let mut new_node = std::mem::take(&mut edges[mid]);

  new_node.left = if low < mid {
    create_tree_from_edges(edges, low, mid - 1).map(Box::new)
  } else {
    None
  };
  new_node.right = if mid < high {
    create_tree_from_edges(edges, mid + 1, high).map(Box::new)
  } else {
    None
  };

  if let Some(left) = &new_node.left {
    new_node.max = CoreHelper::max_f64(new_node.max, left.max);
  }
  if let Some(right) = &new_node.right {
    new_node.max = CoreHelper::max_f64(new_node.max, right.max);
  }

  Some(new_node)
}
