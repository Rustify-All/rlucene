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

use crate::test_framework::core::util::lucene_test_case::random;
use std::fmt;
use std::rc::Rc;

use rand::Rng;
use rand::RngExt;

use crate::core::codecs::mutable_point_tree::MutablePointTree;
use crate::core::index::BytesRef;
use crate::core::index::point_values::{PointTree, PointTreeEnum};
use crate::core::util::ToInt;
use crate::core::util::bkd::bkd_config::BKDConfig;
use crate::core::util::bkd::mutable_point_tree_reader_utils::MutablePointTreeReaderUtils;
use crate::core::util::clone::TryClone;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::util::test_util::TestUtil;

#[allow(dead_code)] // for quick search
struct TestMutablePointTreeReaderUtils;
#[test]
fn test_sort() -> Result<()> {
  let mut random = random();
  for _ in 0..10 {
    do_test_sort(&mut random, false)?;
  }
  Ok(())
}

#[test]
fn test_sort_with_incremental_doc_id() -> Result<()> {
  let mut random = random();
  for _ in 0..10 {
    do_test_sort(&mut random, true)?;
  }
  Ok(())
}

fn do_test_sort<R>(random: &mut R, is_doc_id_incremental: bool) -> Result<()>
where
  R: Rng + ?Sized,
{
  let bytes_per_dim = TestUtil::next_usize(random, 1, 16);
  let end = 1 << random.random_range(0..30);
  let max_doc = TestUtil::next_int(random, 1, end);
  let config = BKDConfig::new(
    1,
    1,
    bytes_per_dim,
    BKDConfig::DEFAULT_MAX_POINTS_IN_LEAF_NODE,
  )?;
  let mut common_prefix_lengths = [0; 1];
  let points = create_random_points(
    random,
    &config,
    max_doc,
    &mut common_prefix_lengths,
    is_doc_id_incremental,
  );
  let mut reader = DummyPointsReader::new(points.clone());
  MutablePointTreeReaderUtils::sort(&config, max_doc, &mut reader, 0, points.len())?;
  let mut sorted_points = points;
  sorted_points.sort_by(|o1, o2| {
    let cmp = o1.packed_value.cmp(&o2.packed_value);
    if cmp == std::cmp::Ordering::Equal {
      o1.doc.cmp(&o2.doc)
    } else {
      cmp
    }
  });
  assert_ne!(sorted_points.as_ptr(), reader.points.as_ptr());
  assert_eq!(sorted_points.len(), reader.points.len());

  let mut prev_point: Option<&Point> = None;
  for (sorted_point, reader_point) in sorted_points.iter().zip(reader.points.iter()) {
    assert_eq!(sorted_point.packed_value, reader_point.packed_value);
    if let Some(prev) = prev_point
      && reader_point.packed_value == prev.packed_value
    {
      assert!(
        reader_point.doc >= prev.doc,
        "Doc IDs not in ascending order"
      );
    }
    prev_point = Some(reader_point);
  }
  Ok(())
}

#[test]
fn test_sort_by_dim() -> Result<()> {
  let mut random = random();
  for _ in 0..5 {
    do_test_sort_by_dim(&mut random)?;
  }
  Ok(())
}

fn do_test_sort_by_dim<R>(random: &mut R) -> Result<()>
where
  R: Rng + ?Sized,
{
  let config = Rc::new(create_random_config(random)?);
  let end = 1 << random.random_range(0..30);
  let max_doc = TestUtil::next_int(random, 1, end);
  let mut common_prefix_lengths = vec![0; config.num_dims];
  let points = create_random_points(random, &config, max_doc, &mut common_prefix_lengths, false);
  let sorted_dim = random.random_range(0..config.num_index_dims);
  let num_points = points.len();
  let mut reader = DummyPointsReader::new(points);
  MutablePointTreeReaderUtils::sort_by_dim(
    &config,
    sorted_dim,
    &common_prefix_lengths,
    &mut reader,
    0,
    num_points,
    &mut BytesRef::default(),
    &mut BytesRef::default(),
  )?;
  let offset = sorted_dim * config.bytes_per_dim;
  for i in 1..num_points {
    let previous_value = &reader.points[i - 1].packed_value;
    let current_value = &reader.points[i].packed_value;

    let dim_start_prev = previous_value.offset + offset;
    let dim_end_prev = dim_start_prev + config.bytes_per_dim;
    let dim_start_curr = current_value.offset + offset;
    let dim_end_curr = dim_start_curr + config.bytes_per_dim;

    let mut cmp = compare_unsigned(
      &previous_value.bytes[dim_start_prev..dim_end_prev],
      &current_value.bytes[dim_start_curr..dim_end_curr],
    );

    if cmp == 0 {
      let data_dim_offset = config.packed_index_bytes_length();
      let data_dims_length = (config.num_dims - config.num_index_dims) * config.bytes_per_dim;
      let data_start_prev = previous_value.offset + data_dim_offset;
      let data_end_prev = data_start_prev + data_dims_length;
      let data_start_curr = current_value.offset + data_dim_offset;
      let data_end_curr = data_start_curr + data_dims_length;

      cmp = compare_unsigned(
        &previous_value.bytes[data_start_prev..data_end_prev],
        &current_value.bytes[data_start_curr..data_end_curr],
      );
      if cmp == 0 {
        cmp = reader.points[i - 1].doc - reader.points[i].doc;
      }
    }
    assert!(cmp <= 0);
  }
  Ok(())
}

#[test]
fn test_partition() -> Result<()> {
  let mut random = random();
  for _ in 0..5 {
    do_test_partition(&mut random)?;
  }
  Ok(())
}
fn do_test_partition<R>(random: &mut R) -> Result<()>
where
  R: Rng + ?Sized,
{
  let config = Rc::new(create_random_config(random)?);
  let mut common_prefix_lengths = vec![0; config.num_dims];
  let end = 1 << random.random_range(0..30);
  let max_doc = TestUtil::next_int(random, 1, end);
  let points = create_random_points(random, &config, max_doc, &mut common_prefix_lengths, false);
  let split_dim = random.random_range(0..config.num_index_dims);
  let num_points = points.len();
  let mut reader = DummyPointsReader::new(points);
  let pivot = TestUtil::next_usize(random, 0, num_points - 1);

  MutablePointTreeReaderUtils::partition(
    &config,
    max_doc,
    split_dim,
    common_prefix_lengths[split_dim],
    &mut reader,
    0,
    num_points,
    pivot,
    &mut BytesRef::default(),
    &mut BytesRef::default(),
  )?;
  let pivot_point = &reader.points[pivot];
  let pivot_value = &pivot_point.packed_value;
  let offset = split_dim * config.bytes_per_dim;

  for i in 0..num_points {
    let value = &reader.points[i].packed_value;
    let dim_start = value.offset + offset;
    let dim_end = value.offset + (offset + config.bytes_per_dim);
    let pivot_dim_start = pivot_value.offset + offset;
    let pivot_dim_end = pivot_value.offset + (offset + config.bytes_per_dim);

    let mut cmp = compare_unsigned(
      &value.bytes[dim_start..dim_end],
      &pivot_value.bytes[pivot_dim_start..pivot_dim_end],
    );
    if cmp == 0 {
      let data_dim_offset = config.packed_index_bytes_length();
      let data_dims_length = (config.num_dims - config.num_index_dims) * config.bytes_per_dim;
      let data_start = value.offset + data_dim_offset;
      let data_end = data_start + data_dims_length;
      let pivot_data_start = pivot_value.offset + data_dim_offset;
      let pivot_data_end = pivot_data_start + data_dims_length;
      cmp = compare_unsigned(
        &value.bytes[data_start..data_end],
        &pivot_value.bytes[pivot_data_start..pivot_data_end],
      );
      if cmp == 0 {
        cmp = reader.points[i].doc - pivot_point.doc;
      }
    }
    match i.cmp(&pivot) {
      std::cmp::Ordering::Less => {
        assert!(cmp <= 0, "Expected cmp <= 0 for i < pivot, got {}", cmp);
      },
      std::cmp::Ordering::Greater => {
        assert!(cmp >= 0, "Expected cmp >= 0 for i > pivot, got {}", cmp);
      },
      std::cmp::Ordering::Equal => {
        assert_eq!(cmp, 0, "Expected cmp == 0 for the pivot index");
      },
    }
  }
  Ok(())
}

fn compare_unsigned(a: &[u8], b: &[u8]) -> i32 {
  a.cmp(b).to_int()
}

fn create_random_config<R>(random: &mut R) -> Result<BKDConfig>
where
  R: Rng + ?Sized,
{
  let num_index_dims = TestUtil::next_usize(random, 1, BKDConfig::MAX_INDEX_DIMS);
  let num_dims = TestUtil::next_usize(random, num_index_dims, BKDConfig::MAX_DIMS);
  let bytes_per_dim = TestUtil::next_usize(random, 1, 16);
  let max_points_in_leaf_node = TestUtil::next_usize(random, 50, 2000);
  BKDConfig::new(
    num_dims,
    num_index_dims,
    bytes_per_dim,
    max_points_in_leaf_node,
  )
}
fn create_random_points<R>(
  random: &mut R,
  config: &BKDConfig,
  max_doc: i32,
  common_prefix_lengths: &mut [usize],
  is_doc_id_incremental: bool,
) -> Vec<Point>
where
  R: Rng + ?Sized,
{
  assert_eq!(common_prefix_lengths.len(), config.num_dims);
  let num_points = TestUtil::next_int(random, 1, 100000);
  let mut points: Vec<Point> = Vec::with_capacity(num_points as usize);
  if random.random_range(0..10) != 0 {
    for i in 0..num_points {
      let mut value = vec![0u8; config.packed_bytes_length()];
      random.fill_bytes(&mut value);
      let doc = if is_doc_id_incremental {
        i.min(max_doc - 1)
      } else {
        random.random_range(0..max_doc)
      };
      points.push(Point::new(random, &value, doc));
    }
    common_prefix_lengths
      .iter_mut()
      .for_each(|prefix| *prefix = TestUtil::next_usize(random, 0, config.bytes_per_dim));

    let (first, rest) = points.split_at_mut(1);
    let first_value = &first[0].packed_value;
    debug_assert!(first_value.is_valid().is_ok());
    for point in rest.iter_mut() {
      for (dim, &prefix_len) in common_prefix_lengths
        .iter()
        .take(config.num_dims)
        .enumerate()
      {
        let offset = dim * config.bytes_per_dim;
        let src_start = first_value.offset + offset;
        let dst_start = point.packed_value.offset + offset;

        let copy_source = &first_value.bytes[src_start..src_start + prefix_len];
        point.packed_value.bytes[dst_start..][..copy_source.len()].copy_from_slice(copy_source);
      }
    }
  } else {
    let num_data_dims = config.num_dims - config.num_index_dims;
    let mut index_dims = vec![0u8; config.packed_index_bytes_length()];
    random.fill_bytes(&mut index_dims);
    let data_dims_len = num_data_dims * config.bytes_per_dim;
    let mut data_dims = vec![0u8; data_dims_len];

    for i in 0..num_points {
      let mut value = vec![0u8; config.packed_bytes_length()];
      let copy_source = &index_dims;
      value[..copy_source.len()].copy_from_slice(copy_source);
      random.fill_bytes(&mut data_dims);
      let start = config.packed_index_bytes_length();
      let copy_source = &data_dims;
      value[start..][..copy_source.len()].copy_from_slice(copy_source);
      let doc = if is_doc_id_incremental {
        i.min(max_doc - 1)
      } else {
        random.random_range(0..max_doc)
      };
      points.push(Point::new(random, &value, doc));
    }
    common_prefix_lengths
      .iter_mut()
      .take(config.num_index_dims)
      .for_each(|prefix| *prefix = config.bytes_per_dim);

    common_prefix_lengths[config.num_index_dims..config.num_dims]
      .iter_mut()
      .for_each(|prefix| *prefix = TestUtil::next_usize(random, 0, config.bytes_per_dim));

    let (first, rest) = points.split_at_mut(1);
    let first_value = &first[0].packed_value;
    debug_assert!(first_value.is_valid().is_ok());
    for point in rest.iter_mut() {
      for (dim, &prefix_len) in common_prefix_lengths
        .iter()
        .enumerate()
        .skip(config.num_index_dims)
        .take(config.num_dims - config.num_index_dims)
      {
        let offset = dim * config.bytes_per_dim;
        let src_start = first_value.offset + offset;
        let dst_start = point.packed_value.offset + offset;

        let copy_source = &first_value.bytes[src_start..src_start + prefix_len];
        point.packed_value.bytes[dst_start..][..copy_source.len()].copy_from_slice(copy_source);
      }
    }
  }
  points
}
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
struct Point {
  pub packed_value: BytesRef<Vec<u8>>,
  pub doc: i32,
}

impl Point {
  fn new<R>(random: &mut R, packed_value: &[u8], doc: i32) -> Self
  where
    R: Rng + ?Sized,
  {
    let mut vec = vec![0u8; packed_value.len() + 1];
    vec[0] = random.random_range(0..255u8);
    vec[1..][..packed_value.len()].copy_from_slice(packed_value);
    Self {
      packed_value: BytesRef {
        bytes: vec,
        offset: 1,
        length: packed_value.len(),
      },
      doc,
    }
  }
}

impl fmt::Display for Point {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    // Using Debug formatting for BytesRef.
    write!(f, "value={:?} doc={}", self.packed_value, self.doc)
  }
}

#[derive(Clone)]
pub struct DummyPointsReader {
  points: Vec<Point>,
  temp: Vec<Point>,
}
impl TryClone for DummyPointsReader {
  fn try_clone(&self) -> Result<Self> {
    Ok(self.clone())
  }
}

impl DummyPointsReader {
  fn new(points: Vec<Point>) -> Self {
    let length = points.len();
    Self {
      points,
      temp: vec![Point::default(); length],
    }
  }
}
impl PointTree for DummyPointsReader {
  type MutablePointTree = Self;

  fn into_mutable(self) -> PointTreeEnum<Self::MutablePointTree, Self> {
    PointTreeEnum::Mutable(self)
  }

  fn move_to_child(&mut self) -> Result<bool> {
    Ok(false)
  }

  fn move_to_sibling(&mut self) -> Result<bool> {
    Ok(false)
  }

  fn move_to_parent(&mut self) -> Result<bool> {
    Ok(false)
  }
}
impl MutablePointTree for DummyPointsReader {
  fn get_value<'a>(
    &'a self,
    i: usize,
    _spare: &'a mut BytesRef<Vec<u8>>,
  ) -> Result<BytesRef<&'a [u8]>> {
    let point = &self.points[i].packed_value;
    Ok(BytesRef {
      bytes: &point.bytes,
      offset: point.offset,
      length: point.length,
    })
  }

  fn get_byte_at(&self, i: usize, k: usize) -> u8 {
    let packed_value = &self.points[i].packed_value;
    packed_value.bytes[packed_value.offset + k]
  }

  fn get_doc_id(&self, i: usize) -> Result<i32> {
    Ok(self.points[i].doc)
  }

  fn swap(&mut self, i: usize, j: usize) {
    self.points.swap(i, j);
  }

  fn save(&mut self, i: usize, j: usize) {
    self.temp[j] = self.points[i].clone();
  }

  fn restore(&mut self, i: usize, j: usize) {
    self.points[i..j].clone_from_slice(&self.temp[i..j]);
  }
}
