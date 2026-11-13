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
use crate::core::document::document::Document;
use crate::core::document::xy_point_field::XYPointField;
use crate::core::geo::xy_geometry::XYGeometryEnum;
use crate::core::geo::xy_polygon::XYPolygon;
use crate::core::search::query::Query;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::geo::base_xy_point_test_case::BaseXYPointTestCase;
use crate::test_framework::core::util::lucene_test_case::random;
use rand::prelude::StdRng;

pub struct TestXYPointQueries;
fn run_case<F>(f: F) -> Result<()>
where
  F: FnOnce(&TestXYPointQueries, &mut StdRng) -> Result<()>,
{
  let mut random = random();
  let case = TestXYPointQueries;
  f(&case, &mut random)
}
mod base_xy_point_test_case_tests {
  use crate::core::util::error::lucene_error::Result;
  use crate::test::core::search::test_xy_point_queries::run_case;
  use crate::test_framework::core::geo::base_xy_point_test_case::BaseXYPointTestCase;
  #[test]
  fn test_index_extreme_values() -> Result<()> {
    run_case(|case, _random| case.test_index_extreme_values())
  }
  #[test]
  fn test_index_nan_values() -> Result<()> {
    run_case(|case, _random| case.test_index_nan_values())
  }
  #[test]
  fn test_index_inf_values() -> Result<()> {
    run_case(|case, _random| case.test_index_inf_values())
  }
  #[test]
  fn test_box_basics() -> Result<()> {
    run_case(|case, random| case.test_box_basics(random))
  }
  #[test]
  #[ignore = "Java-only: Rust query field parameters are non-null string references"]
  fn test_box_null() -> Result<()> {
    test_not_required_in_rust_lucene!();
  }
  #[test]
  fn test_box_invalid_coordinates() -> Result<()> {
    run_case(|case, _random| case.test_box_invalid_coordinates())
  }
  #[test]
  fn test_distance_basics() -> Result<()> {
    run_case(|case, _random| case.test_distance_basics(_random))
  }
  #[test]
  #[ignore = "Java-only: Rust query field parameters are non-null string references"]
  fn test_distance_null() -> Result<()> {
    test_not_required_in_rust_lucene!();
  }
  #[test]
  fn test_distance_illegal() -> Result<()> {
    run_case(|case, _random| case.test_distance_illegal())
  }
  #[test]
  fn test_distance_negative() -> Result<()> {
    run_case(|case, _random| case.test_distance_negative())
  }
  #[test]
  fn test_distance_nan() -> Result<()> {
    run_case(|case, _random| case.test_distance_nan())
  }
  #[test]
  fn test_distance_inf() -> Result<()> {
    run_case(|case, _random| case.test_distance_inf())
  }
  #[test]
  fn test_polygon_basics() -> Result<()> {
    run_case(|case, random| case.test_polygon_basics(random))
  }
  #[test]
  fn test_polygon_hole() -> Result<()> {
    run_case(|case, random| case.test_polygon_hole(random))
  }
  #[test]
  fn test_polygon_hole_excludes() -> Result<()> {
    run_case(|case, random| case.test_polygon_hole_excludes(random))
  }
  #[test]
  fn test_multi_polygon_basics() -> Result<()> {
    run_case(|case, random| case.test_multi_polygon_basics(random))
  }
  #[test]
  #[ignore = "Java-only: Rust query field parameters are non-null string references"]
  fn test_polygon_null_field() -> Result<()> {
    test_not_required_in_rust_lucene!();
  }
  #[test]
  fn test_same_point_many_times() -> Result<()> {
    run_case(|case, random| case.test_same_point_many_times(random))
  }
  #[test]
  fn test_low_cardinality() -> Result<()> {
    run_case(|case, random| case.test_low_cardinality(random))
  }
  #[test]
  fn test_all_y_equal() -> Result<()> {
    run_case(|case, random| case.test_all_y_equal(random))
  }
  #[test]
  fn test_all_x_equal() -> Result<()> {
    run_case(|case, random| case.test_all_x_equal(random))
  }
  #[test]
  fn test_multi_valued() -> Result<()> {
    run_case(|case, random| case.test_multi_valued(random))
  }
  #[test]
  fn test_random_tiny() -> Result<()> {
    run_case(|case, random| case.test_random_tiny(random))
  }
  #[test]
  fn test_random_medium() -> Result<()> {
    run_case(|case, random| case.test_random_medium(random))
  }
  #[cfg(feature = "nightly")]
  #[test]
  #[ignore = "nightly"]
  fn test_random_big() -> Result<()> {
    run_case(|case, random| case.test_random_big(random))
  }
  #[test]
  fn test_rect_boundaries_are_inclusive() -> Result<()> {
    run_case(|case, random| case.test_rect_boundaries_are_inclusive(random))
  }
  #[test]
  fn test_random_distance() -> Result<()> {
    run_case(|case, random| case.test_random_distance(random))
  }
  #[cfg(feature = "nightly")]
  #[test]
  #[ignore = "nightly"]
  fn test_random_distance_huge() -> Result<()> {
    run_case(|case, random| case.test_random_distance_huge(random))
  }
  #[test]
  fn test_equals() -> Result<()> {
    run_case(|case, random| case.test_equals(random))
  }
  #[test]
  fn test_small_set_rect() -> Result<()> {
    run_case(|case, random| case.test_small_set_rect(random))
  }
  #[test]
  fn test_small_set_rect2() -> Result<()> {
    run_case(|case, random| case.test_small_set_rect2(random))
  }
  #[test]
  fn test_small_set_multi_valued() -> Result<()> {
    run_case(|case, random| case.test_small_set_multi_valued(random))
  }
  #[test]
  fn test_small_set_whole_space() -> Result<()> {
    run_case(|case, random| case.test_small_set_whole_space(random))
  }
  #[test]
  fn test_small_set_poly() -> Result<()> {
    run_case(|case, random| case.test_small_set_poly(random))
  }
  #[test]
  fn test_small_set_poly_whole_space() -> Result<()> {
    run_case(|case, random| case.test_small_set_poly_whole_space(random))
  }
  #[test]
  fn test_small_set_distance() -> Result<()> {
    run_case(|case, random| case.test_small_set_distance(random))
  }
  #[test]
  fn test_small_set_tiny_distance() -> Result<()> {
    run_case(|case, random| case.test_small_set_tiny_distance(random))
  }
  #[test]
  fn test_small_set_huge_distance() -> Result<()> {
    run_case(|case, random| case.test_small_set_huge_distance(random))
  }
}
impl BaseXYPointTestCase for TestXYPointQueries {
  fn add_point_to_doc(&self, field: &str, doc: &mut Document, x: f32, y: f32) -> Result<()> {
    doc.add(XYPointField::new(field.to_owned(), x, y)?);
    Ok(())
  }

  fn new_rect_query(
    &self,
    field: &str,
    min_x: f32,
    max_x: f32,
    min_y: f32,
    max_y: f32,
  ) -> Result<Query> {
    XYPointField::new_box_query(field, min_x, max_x, min_y, max_y)
  }

  fn new_distance_query(
    &self,
    field: &str,
    center_x: f32,
    center_y: f32,
    radius: f32,
  ) -> Result<Query> {
    XYPointField::new_distance_query(field, center_x, center_y, radius)
  }

  fn new_polygon_query(&self, field: &str, polygon: Vec<XYPolygon>) -> Result<Query> {
    XYPointField::new_polygon_query(field, polygon)
  }

  fn new_geometry_query(&self, field: &str, geometries: Vec<XYGeometryEnum>) -> Result<Query> {
    XYPointField::new_geometry_query(field, geometries)
  }
}
