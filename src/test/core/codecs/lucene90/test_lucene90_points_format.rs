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
use crate::core::codecs::Codecs;
use crate::core::document::binary_point::BinaryPoint;
use crate::core::document::document::Document;
use crate::core::index::directory_reader;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::merge_policy::MergePolicyEnum;
use crate::core::index::point_values::{IntersectVisitor, PointValues, Relation};
use crate::core::util::bkd::bkd_config::BKDConfig;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::codecs::lucene90::test_lucene90_points_format::TestLucene90PointsFormatCodec;
use crate::test_framework::core::index::base_index_file_format_test_case::BaseIndexFileFormatTestCase;
use crate::test_framework::core::index::base_points_format_test_case::BasePointsFormatTestCase;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, get_only_leaf_reader, is_night_mode, new_directory_shared, new_index_writer_config,
  new_merge_policy, random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::Rng;
use rand::RngExt;
use rand::prelude::StdRng;

#[allow(dead_code)] // for quick search
pub struct TestLucene90PointsFormat {
  codec: Codecs,
  max_points_in_leaf_node: usize,
}

impl TestLucene90PointsFormat {
  fn new<R>(random: &mut R) -> Self
  where
    R: Rng + ?Sized,
  {
    // standard issue
    let default_codec = TestUtil::get_default_codec();
    if random.random_bool(0.5) {
      // randomize parameters
      let max_points_in_leaf_node = TestUtil::next_usize(random, 50, 500);
      let max_mb_sort_in_heap = 3.0 + (3.0 * random.random::<f64>());
      if cfg!(feature = "test_log_verbose") {
        println!(
          "TEST: using Lucene60PointsFormat with maxPointsInLeafNode={max_points_in_leaf_node} \
           and maxMBSortInHeap={max_mb_sort_in_heap}"
        );
      }

      // sneaky impersonation!
      Self {
        codec: TestLucene90PointsFormatCodec::new(
          default_codec,
          max_points_in_leaf_node,
          max_mb_sort_in_heap,
        )
        .into(),
        max_points_in_leaf_node,
      }
    } else {
      // standard issue
      Self {
        codec: default_codec.into(),
        max_points_in_leaf_node: BKDConfig::DEFAULT_MAX_POINTS_IN_LEAF_NODE,
      }
    }
  }

  fn test_estimate_point_count<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let mut iwc = new_index_writer_config(random)?;
    // Avoid MockRandomMergePolicy since it may cause non-optimal merges that make the number of
    // points per leaf hard to predict.
    while matches!(&*iwc.get_merge_policy(), MergePolicyEnum::MockRandom(_)) {
      iwc.set_merge_policy(new_merge_policy(random)?);
    }
    let w = IndexWriter::new(dir.clone(), iwc)?;

    let mut point_value = [0u8; 3];
    let mut unique_point_value = [0u8; 3];
    random.fill_bytes(&mut unique_point_value);
    let num_docs = if is_night_mode() {
      at_least(random, 10_000)
    } else {
      at_least(random, 500)
    };
    let multi_values = random.random_bool(0.5);
    let mut total_values = 0usize;

    for i in 0..num_docs {
      let mut doc = Document::new();
      if i == num_docs / 2 {
        total_values += 1;
        doc.add(BinaryPoint::new("f", [&unique_point_value])?);
      } else {
        let num_values = if multi_values {
          TestUtil::next_int(random, 2, 100)
        } else {
          1
        };
        for _ in 0..num_values {
          loop {
            random.fill_bytes(&mut point_value);
            if point_value != unique_point_value {
              break;
            }
          }
          doc.add(BinaryPoint::new("f", [&point_value])?);
          total_values += 1;
        }
      }
      w.add_document(doc)?;
    }

    w.force_merge(1)?;
    w.close()?;

    let r = directory_reader::open(dir)?;
    let lr = get_only_leaf_reader(r)?;
    let points = lr
      .get_point_values("f")?
      .expect("point values should exist");

    let all_points_visitor = AllPointsVisitor;
    assert_eq!(
      total_values as i64,
      points.estimate_point_count(&all_points_visitor)?
    );
    assert_eq!(
      num_docs as i64,
      points.estimate_doc_count(&all_points_visitor)?
    );

    let no_points_visitor = NoPointsVisitor;
    assert_eq!(0, points.estimate_point_count(&no_points_visitor)?);
    assert_eq!(0, points.estimate_doc_count(&no_points_visitor)?);

    let one_point_match_visitor = OnePointMatchVisitor { unique_point_value };
    let point_count = points.estimate_point_count(&one_point_match_visitor)?;
    let last_node_point_count = total_values % self.max_points_in_leaf_node;
    assert!(
      point_count == (self.max_points_in_leaf_node as i64 + 1) / 2
        || point_count == (last_node_point_count as i64 + 1) / 2
        || point_count == 2 * ((self.max_points_in_leaf_node as i64 + 1) / 2)
        || point_count
          == ((self.max_points_in_leaf_node as i64 + 1) / 2)
            + ((last_node_point_count as i64 + 1) / 2),
      "{point_count}"
    );

    let doc_count = points.estimate_doc_count(&one_point_match_visitor)?;
    if multi_values {
      assert_eq!(
        doc_count,
        (doc_count as f64
          * (1.0
            - (((num_docs as i64 - point_count) / points.size()? as i64) as f64)
              .powf((points.size()? as i64 / doc_count) as f64))) as i64
      );
    } else {
      assert_eq!(std::cmp::min(point_count, num_docs as i64), doc_count);
    }

    Ok(())
  }

  fn test_estimate_point_count_2_dims<R>(&self, random: &mut R) -> Result<()>
  where
    R: Rng + ?Sized,
  {
    let dir = new_directory_shared(random)?;
    let iwc = new_index_writer_config(random)?;
    let w = IndexWriter::new(dir.clone(), iwc)?;

    let mut point_value = [[0u8; 3]; 2];
    let mut unique_point_value = [[0u8; 3]; 2];
    random.fill_bytes(&mut unique_point_value[0]);
    random.fill_bytes(&mut unique_point_value[1]);
    let num_docs = if is_night_mode() {
      at_least(random, 10_000)
    } else {
      at_least(random, 1_000)
    };
    let multi_values = random.random_bool(0.5);
    let mut total_values = 0usize;

    for i in 0..num_docs {
      let mut doc = Document::new();
      if i == num_docs / 2 {
        total_values += 1;
        doc.add(BinaryPoint::new("f", unique_point_value)?);
      } else {
        let num_values = if multi_values {
          TestUtil::next_int(random, 2, 100)
        } else {
          1
        };
        for _ in 0..num_values {
          loop {
            random.fill_bytes(&mut point_value[0]);
            random.fill_bytes(&mut point_value[1]);
            if point_value[0] != unique_point_value[0] && point_value[1] != unique_point_value[1] {
              break;
            }
          }
          doc.add(BinaryPoint::new("f", point_value)?);
          total_values += 1;
        }
      }
      w.add_document(doc)?;
    }

    w.force_merge(1)?;
    let r = directory_reader::open_from_writer(&w)?;
    w.close()?;

    let lr = get_only_leaf_reader(r)?;
    let points = lr
      .get_point_values("f")?
      .expect("point values should exist");

    let all_points_visitor = AllPointsVisitor;
    assert_eq!(
      total_values as i64,
      points.estimate_point_count(&all_points_visitor)?
    );
    assert_eq!(
      num_docs as i64,
      points.estimate_doc_count(&all_points_visitor)?
    );

    let no_points_visitor = NoPointsVisitor;
    assert_eq!(0, points.estimate_point_count(&no_points_visitor)?);
    assert_eq!(0, points.estimate_doc_count(&no_points_visitor)?);

    let one_point_match_visitor = OnePointMatchVisitor2Dims { unique_point_value };
    let point_count = points.estimate_point_count(&one_point_match_visitor)?;
    let last_node_point_count = total_values % self.max_points_in_leaf_node;
    let common = (self.max_points_in_leaf_node as i64 + 1) / 2;
    let last = (last_node_point_count as i64 + 1) / 2;
    assert!(
      point_count == common
        || point_count == last
        || point_count == 2 * common
        || point_count == common + last
        || point_count == 4 * common
        || point_count == 3 * common + last,
      "{point_count}"
    );

    let doc_count = points.estimate_doc_count(&one_point_match_visitor)?;
    if multi_values {
      assert_eq!(
        doc_count,
        (doc_count as f64
          * (1.0
            - (((num_docs as i64 - point_count) / points.size()? as i64) as f64)
              .powf((points.size()? as i64 / doc_count) as f64))) as i64
      );
    } else {
      assert_eq!(std::cmp::min(point_count, num_docs as i64), doc_count);
    }

    Ok(())
  }
}

#[derive(Clone, Copy)]
struct AllPointsVisitor;

impl IntersectVisitor for AllPointsVisitor {
  fn visit(&mut self, _doc_id: i32) -> Result<()> {
    Ok(())
  }

  fn visit_with_packed_value(&mut self, _doc_id: i32, _packed_value: &[u8]) -> Result<()> {
    Ok(())
  }

  fn compare(&self, _min_packed_value: &[u8], _max_packed_value: &[u8]) -> Result<Relation> {
    Ok(Relation::CellInsideQuery)
  }
}

#[derive(Clone, Copy)]
struct NoPointsVisitor;

impl IntersectVisitor for NoPointsVisitor {
  fn visit(&mut self, _doc_id: i32) -> Result<()> {
    Ok(())
  }

  fn visit_with_packed_value(&mut self, _doc_id: i32, _packed_value: &[u8]) -> Result<()> {
    Ok(())
  }

  fn compare(&self, _min_packed_value: &[u8], _max_packed_value: &[u8]) -> Result<Relation> {
    Ok(Relation::CellOutsideQuery)
  }
}

#[derive(Clone, Copy)]
struct OnePointMatchVisitor {
  unique_point_value: [u8; 3],
}

impl IntersectVisitor for OnePointMatchVisitor {
  fn visit(&mut self, _doc_id: i32) -> Result<()> {
    Ok(())
  }

  fn visit_with_packed_value(&mut self, _doc_id: i32, _packed_value: &[u8]) -> Result<()> {
    Ok(())
  }

  fn compare(&self, min_packed_value: &[u8], max_packed_value: &[u8]) -> Result<Relation> {
    if self.unique_point_value.as_slice()[0..3] > max_packed_value[0..3]
      || self.unique_point_value.as_slice()[0..3] < min_packed_value[0..3]
    {
      Ok(Relation::CellOutsideQuery)
    } else {
      Ok(Relation::CellCrossesQuery)
    }
  }
}

#[derive(Clone, Copy)]
struct OnePointMatchVisitor2Dims {
  unique_point_value: [[u8; 3]; 2],
}

impl IntersectVisitor for OnePointMatchVisitor2Dims {
  fn visit(&mut self, _doc_id: i32) -> Result<()> {
    Ok(())
  }

  fn visit_with_packed_value(&mut self, _doc_id: i32, _packed_value: &[u8]) -> Result<()> {
    Ok(())
  }

  fn compare(&self, min_packed_value: &[u8], max_packed_value: &[u8]) -> Result<Relation> {
    for dim in 0..2 {
      let offset = dim * 3;
      if self.unique_point_value[dim].as_slice() > &max_packed_value[offset..offset + 3]
        || self.unique_point_value[dim].as_slice() < &min_packed_value[offset..offset + 3]
      {
        return Ok(Relation::CellOutsideQuery);
      }
    }
    Ok(Relation::CellCrossesQuery)
  }
}

#[test]
fn test_estimate_point_count() -> Result<()> {
  run_case(|case, random| case.test_estimate_point_count(random))
}

#[test]
fn test_estimate_point_count_2_dims() -> Result<()> {
  run_case(|case, random| case.test_estimate_point_count_2_dims(random))
}
mod base_points_format_test_case_tests {
  use crate::core::util::error::lucene_error::Result;
  use crate::test::core::codecs::lucene90::test_lucene90_points_format::run_case;
  use crate::test_framework::core::index::base_points_format_test_case::BasePointsFormatTestCase;

  #[test]
  fn test_basic() -> Result<()> {
    run_case(|case, random| case.test_basic(random))
  }

  #[test]
  fn test_merge() -> Result<()> {
    run_case(|case, random| case.test_merge(random))
  }

  #[test]
  fn test_all_point_docs_deleted_in_segment() -> Result<()> {
    run_case(|case, random| case.test_all_point_docs_deleted_in_segment(random))
  }

  #[test]
  fn test_with_exceptions() -> Result<()> {
    run_case(|case, random| case.test_with_exceptions(random))
  }
  #[test]
  fn test_multi_valued() -> Result<()> {
    run_case(|case, random| case.test_multi_valued(random))
  }

  #[test]
  fn test_all_equal() -> Result<()> {
    run_case(|case, random| case.test_all_equal(random))
  }

  #[test]
  fn test_one_dim_equal() -> Result<()> {
    run_case(|case, random| case.test_one_dim_equal(random))
  }
  #[test]
  fn test_one_dim_two_values() -> Result<()> {
    run_case(|case, random| case.test_one_dim_two_values(random))
  }
  #[test]
  fn test_big_int_n_dims() -> Result<()> {
    run_case(|case, random| case.test_big_int_n_dims(random))
  }

  #[test]
  fn test_random_binary_tiny() -> Result<()> {
    run_case(|case, random| case.test_random_binary_tiny(random))
  }

  #[test]
  fn test_random_binary_medium() -> Result<()> {
    run_case(|case, random| case.test_random_binary_medium(random))
  }
  #[cfg(feature = "nightly")]
  #[test]
  #[ignore = "nightly"]
  fn test_random_binary_big() -> Result<()> {
    run_case(|case, random| case.test_random_binary_big(random))
  }
  #[test]
  fn test_add_indexes() -> Result<()> {
    run_case(|case, random| case.test_add_indexes(random))
  }
  #[test]
  fn test_merge_missing() -> Result<()> {
    run_case(|case, random| case.test_merge_missing(random))
  }

  #[test]
  fn test_doc_count_edge_cases() -> Result<()> {
    run_case(|case, _random| case.test_doc_count_edge_cases())
  }

  #[test]
  fn test_random_doc_count() -> Result<()> {
    run_case(|case, random| case.test_random_doc_count(random))
  }
  #[test]
  fn test_mismatched_fields() -> Result<()> {
    run_case(|case, random| case.test_mismatched_fields(random))
  }
}

impl BaseIndexFileFormatTestCase for TestLucene90PointsFormat {
  type Defaults = crate::test_framework::core::index::base_points_format_test_case::BasePointsFormatTestCaseDefaults;

  fn get_codec(&self) -> Result<Codecs> {
    Ok(self.codec.clone())
  }
}

impl BasePointsFormatTestCase for TestLucene90PointsFormat {}

fn run_case<F>(f: F) -> Result<()>
where
  F: FnOnce(&TestLucene90PointsFormat, &mut StdRng) -> Result<()>,
{
  let mut random = random();
  let case = TestLucene90PointsFormat::new(&mut random);
  let codec_guard = case.set_up()?;
  let result = f(&case, &mut random);
  case.tear_down(codec_guard);
  result
}

mod base_index_file_format_test_case_test {
  use super::run_case;
  use crate::core::util::error::lucene_error::Result;
  use crate::test_framework::core::index::base_index_file_format_test_case::BaseIndexFileFormatTestCase;

  #[test]
  fn test_merge_stability() -> Result<()> {
    run_case(|case, random| case.test_merge_stability(random))
  }

  #[test]
  fn test_multi_close() -> Result<()> {
    run_case(|case, random| case.test_multi_close(random))
  }

  #[test]
  fn test_random_exceptions() -> Result<()> {
    run_case(|case, random| case.test_random_exceptions(random))
  }

  #[test]
  fn test_check_integrity_reads_all_bytes() -> Result<()> {
    run_case(|case, random| case.test_check_integrity_reads_all_bytes(random))
  }
}
