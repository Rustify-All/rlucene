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

use crate::test_framework::core::util::lucene_test_case::{at_least_usize, new_directory, random};
use std::cmp::Ordering::{Greater, Less};

use rand::Rng;
use rand::RngExt;

use crate::core::store::directory::Directory;

use crate::core::util::bit_util::BitUtil;
use crate::core::util::bkd::bkd_config::BKDConfig;
use crate::core::util::bkd::bkd_radix_selector::{BKDRadixSelector, PathSlice};
use crate::core::util::bkd::heap_point_write::HeapPointWriter;
use crate::core::util::bkd::offline_point_write::OfflinePointWriter;
use crate::core::util::bkd::point_reader::PointReader;
use crate::core::util::bkd::point_value::PointValue;
use crate::core::util::bkd::point_writer::{PointWriter, PointWriterEnum};
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::numeric_utils::NumericUtils;
use crate::core::util::{CoreHelper, SliceCopyOps, ToInt};
use crate::test_framework::core::util::test_util::TestUtil;

#[allow(dead_code)] // for quick search
struct TestBKDRadixSelector;

#[test]
fn test_basic() -> Result<()> {
  let mut random = random();
  let values = 4;
  let dir = new_directory(&mut random)?;
  let middle = 2;
  let dimensions = 1;
  let bytes_per_dimensions = BitUtil::INT_BYTES;
  let config = BKDConfig::new(
    dimensions,
    dimensions,
    bytes_per_dimensions,
    BKDConfig::DEFAULT_MAX_POINTS_IN_LEAF_NODE,
  )?;
  let mut points = get_random_point_writer(&mut random, config.clone(), &dir, values)?;
  let mut value = vec![0u8; config.packed_bytes_length()];

  NumericUtils::int_to_sortable_bytes(1, &mut value, 0);
  points.append_bytes(&value, 0)?;

  NumericUtils::int_to_sortable_bytes(2, &mut value, 0);
  points.append_bytes(&value, 1)?;

  NumericUtils::int_to_sortable_bytes(3, &mut value, 0);
  points.append_bytes(&value, 2)?;

  NumericUtils::int_to_sortable_bytes(4, &mut value, 0);
  points.append_bytes(&value, 3)?;
  points.close()?;
  let mut copy = copy_points(&mut random, config.clone(), &dir, &mut points)?;
  verify(&mut random, config, &dir, &mut copy, 0, values, middle, 0)?;
  Ok(())
}

#[test]
fn test_random_binary_tiny() -> Result<()> {
  let mut random = random();
  do_test_random_binary(&mut random, 10)
}

#[test]
fn test_random_binary_medium() -> Result<()> {
  let mut random = random();
  do_test_random_binary(&mut random, 25000)
}

#[cfg(feature = "nightly")]
#[test]
#[ignore = "nightly"]
fn test_random_binary_big() -> Result<()> {
  let mut random = random();
  do_test_random_binary(&mut random, 500000)
}
fn do_test_random_binary<R>(random: &mut R, count: usize) -> Result<()>
where
  R: Rng + ?Sized,
{
  let config = get_random_config(random)?;
  let packed_bytes_length = config.packed_bytes_length();
  let values = TestUtil::next_usize(random, count, count * 2);
  let dir = new_directory(random)?;
  let (start, end) = if random.random_bool(0.5) {
    (0, values)
  } else {
    let start = TestUtil::next_usize(random, 0, values - 3);
    let end = TestUtil::next_usize(random, start + 2, values);
    (start, end)
  };
  let partition_point = TestUtil::next_usize(random, start + 1, end - 1);
  let sorted_on_heap = random.random_range(0..5000);
  let mut points = get_random_point_writer(random, config.clone(), &dir, values)?;
  let mut value = vec![0u8; packed_bytes_length];
  for i in 0..values {
    random.fill(&mut value[..]);
    points.append_bytes(&value, i as i32)?;
  }
  points.close()?;
  verify(
    random,
    config,
    &dir,
    &mut points,
    start,
    end,
    partition_point,
    sorted_on_heap,
  )?;
  Ok(())
}
#[test]
fn test_random_all_dimensions_equals() -> Result<()> {
  let mut random = random();
  let dimensions = TestUtil::next_usize(&mut random, 1, BKDConfig::MAX_INDEX_DIMS);
  let bytes_per_dimensions = TestUtil::next_usize(&mut random, 2, 30);
  let config = BKDConfig::new(
    dimensions,
    dimensions,
    bytes_per_dimensions,
    BKDConfig::DEFAULT_MAX_POINTS_IN_LEAF_NODE,
  )?;
  let values = TestUtil::next_usize(&mut random, 15000, 20000);
  let dir = new_directory(&mut random)?;
  let partition_point = random.random_range(0..values);
  let sorted_on_heap = random.random_range(0..5000);
  let mut points = get_random_point_writer(&mut random, config.clone(), &dir, values)?;
  let mut value = vec![0u8; config.packed_bytes_length()];
  random.fill(&mut value[..]);
  for i in 0..values {
    if random.random_bool(0.5) {
      points.append_bytes(&value, i as i32)?;
    } else {
      points.append_bytes(&value, random.random_range(0..values) as i32)?;
    }
  }
  points.close()?;
  verify(
    &mut random,
    config.clone(),
    &dir,
    &mut points,
    0,
    values,
    partition_point,
    sorted_on_heap,
  )?;
  Ok(())
}

#[test]
fn test_random_last_byte_two_values() -> Result<()> {
  let mut random = random();
  let values = random.random_range(1..=15000);
  let dir = new_directory(&mut random)?;
  let partition_point = random.random_range(0..values);
  let sorted_on_heap = random.random_range(0..5000);
  let config = get_random_config(&mut random)?;
  let mut points = get_random_point_writer(&mut random, config.clone(), &dir, values)?;
  let mut value = vec![0u8; config.packed_bytes_length()];
  random.fill(&mut value[..]);
  for _ in 0..values {
    if random.random_bool(0.5) {
      points.append_bytes(&value, 1)?;
    } else {
      points.append_bytes(&value, 2)?;
    }
  }
  points.close()?;
  verify(
    &mut random,
    config,
    &dir,
    &mut points,
    0,
    values,
    partition_point,
    sorted_on_heap,
  )?;

  Ok(())
}

#[test]
fn test_random_all_docs_equals() -> Result<()> {
  let mut random = random();
  let values = TestUtil::next_usize(&mut random, 1, 15000);
  let dir = new_directory(&mut random)?;
  let partition_point = random.random_range(0..values);
  let sorted_on_heap = random.random_range(0..5000);
  let config = get_random_config(&mut random)?;
  let mut points = get_random_point_writer(&mut random, config.clone(), &dir, values)?;
  let mut value = vec![0u8; config.packed_bytes_length()];
  random.fill(&mut value[..]);
  for _ in 0..values {
    points.append_bytes(&value, 0)?;
  }
  points.close()?;
  verify(
    &mut random,
    config,
    &dir,
    &mut points,
    0,
    values,
    partition_point,
    sorted_on_heap,
  )?;

  Ok(())
}

#[test]
fn test_random_few_different_values() -> Result<()> {
  let mut random = random();
  let config = get_random_config(&mut random)?;
  let values = at_least_usize(&mut random, 15000);
  let dir = new_directory(&mut random)?;
  let partition_point = random.random_range(0..values);
  let sorted_on_heap = random.random_range(0..5000);
  let mut points = get_random_point_writer(&mut random, config.clone(), &dir, values)?;
  let number_values = TestUtil::next_usize(&mut random, 2, 9);
  let mut different_values = Vec::with_capacity(number_values);
  for _ in 0..number_values {
    let mut buf = vec![0u8; config.packed_bytes_length()];
    random.fill(&mut buf[..]);
    different_values.push(buf);
  }
  for i in 0..values {
    let idx = TestUtil::next_usize(&mut random, 0, number_values - 1);
    points.append_bytes(&different_values[idx], i as i32)?;
  }
  points.close()?;
  verify(
    &mut random,
    config,
    &dir,
    &mut points,
    0,
    values,
    partition_point,
    sorted_on_heap,
  )?;
  Ok(())
}

#[test]
fn test_random_data_dim_diff_values() -> Result<()> {
  let mut random = random();
  let config = get_random_config(&mut random)?;
  let values = at_least_usize(&mut random, 15000);
  let dir = new_directory(&mut random)?;
  let partition_point = random.random_range(0..values);
  let sorted_on_heap = random.random_range(0..5000);
  let mut points = get_random_point_writer(&mut random, config.clone(), &dir, values)?;
  let mut value = vec![0u8; config.packed_bytes_length()];
  let data_only_dims = config.num_dims - config.num_index_dims;
  let data_value_len = data_only_dims * config.bytes_per_dim;
  let mut data_value = vec![0u8; data_value_len];
  random.fill(&mut value[..]);
  for i in 0..values {
    random.fill(&mut data_value[..]);
    let start = config.num_index_dims * config.bytes_per_dim;
    value.copy_from(&data_value, start);
    points.append_bytes(&value, i as i32)?;
  }
  points.close()?;
  verify(
    &mut random,
    config,
    &dir,
    &mut points,
    0,
    values,
    partition_point,
    sorted_on_heap,
  )?;

  Ok(())
}
#[allow(clippy::too_many_arguments)]
fn verify<D, R>(
  random: &mut R,
  config: BKDConfig,
  dir: &D,
  points: &mut PointWriterEnum<D::IndexOutput>,
  start: usize,
  end: usize,
  middle: usize,
  sorted_on_heap: usize,
) -> Result<()>
where
  D: Directory,
  R: Rng + ?Sized,
{
  let mut radix_selector = BKDRadixSelector::new(config.clone(), sorted_on_heap, "test");
  let data_only_dims = config.num_dims - config.num_index_dims;

  for split_dim in 0..config.num_index_dims {
    let mut copy = copy_points(random, config.clone(), dir, points)?;
    let mut input_slice = PathSlice::new(&mut copy, 0, points.count());

    let common_prefix_length_input =
      get_random_common_prefix(config.clone(), &mut input_slice, split_dim, random, dir)?;

    let mut select_slice = radix_selector.select(
      &mut input_slice,
      start,
      end,
      middle,
      split_dim,
      common_prefix_length_input,
      dir,
    )?;
    let mut left_slice = match select_slice.left_writer {
      Some(ref mut left_writer) => {
        PathSlice::new(left_writer, select_slice.left_from, select_slice.left_to)
      },
      None => PathSlice::new(
        input_slice.writer,
        select_slice.left_from,
        select_slice.left_to,
      ),
    };
    assert_eq!(
      left_slice.count,
      middle - start,
      "Left slice count does not match"
    );
    let max = get_max(config.clone(), &mut left_slice, split_dim, dir)?;

    let mut right_slice = match select_slice.right_writer {
      Some(ref mut right_writer) => {
        PathSlice::new(right_writer, select_slice.right_from, select_slice.right_to)
      },
      None => PathSlice::new(
        input_slice.writer,
        select_slice.right_from,
        select_slice.right_to,
      ),
    };
    assert_eq!(
      right_slice.count,
      end - middle,
      "Right slice count does not match"
    );
    let min = get_min(config.clone(), &mut right_slice, split_dim, dir)?;

    let cmp = compare_unsigned(&max, config.bytes_per_dim, &min, config.bytes_per_dim);
    assert!(
      cmp <= 0,
      "Expected left slice max to be <= right slice min; got {}",
      cmp
    );

    if cmp == 0 {
      let mut left_slice = match select_slice.left_writer {
        Some(ref mut left_writer) => {
          PathSlice::new(left_writer, select_slice.left_from, select_slice.left_to)
        },
        None => PathSlice::new(
          input_slice.writer,
          select_slice.left_from,
          select_slice.left_to,
        ),
      };
      let max_data_dim =
        get_max_data_dimension(config.clone(), &mut left_slice, &max, split_dim, dir)?;

      let mut right_slice = match select_slice.right_writer {
        Some(ref mut right_writer) => {
          PathSlice::new(right_writer, select_slice.right_from, select_slice.right_to)
        },
        None => PathSlice::new(
          input_slice.writer,
          select_slice.right_from,
          select_slice.right_to,
        ),
      };
      let min_data_dim =
        get_min_data_dimension(config.clone(), &mut right_slice, &min, split_dim, dir)?;
      let cmp = compare_unsigned(
        &max_data_dim,
        data_only_dims * config.bytes_per_dim,
        &min_data_dim,
        data_only_dims * config.bytes_per_dim,
      );
      assert!(
        cmp <= 0,
        "Expected left slice data dims max <= right slice data dims min; got {}",
        cmp
      );
      if cmp == 0 {
        let mut left_slice = match select_slice.left_writer {
          Some(ref mut left_writer) => {
            PathSlice::new(left_writer, select_slice.left_from, select_slice.left_to)
          },
          None => PathSlice::new(
            input_slice.writer,
            select_slice.left_from,
            select_slice.left_to,
          ),
        };
        let max_doc_id = get_max_doc_id(
          config.clone(),
          &mut left_slice,
          split_dim,
          &select_slice.partition,
          &max_data_dim,
          dir,
        )?;
        let mut right_slice = match select_slice.right_writer {
          Some(ref mut right_writer) => {
            PathSlice::new(right_writer, select_slice.right_from, select_slice.right_to)
          },
          None => PathSlice::new(
            input_slice.writer,
            select_slice.right_from,
            select_slice.right_to,
          ),
        };
        let min_doc_id = get_min_doc_id(
          config.clone(),
          &mut right_slice,
          split_dim,
          &select_slice.partition,
          &min_data_dim,
          dir,
        )?;
        assert!(
          min_doc_id >= max_doc_id,
          "Expected min docID {} to be >= max docID {}",
          min_doc_id,
          max_doc_id
        );
      }
    }
    assert_eq!(
      select_slice.partition.as_slice(),
      min.as_slice(),
      "Partition point does not equal the minimum of the right slice"
    );
    let left_slice = match select_slice.left_writer {
      Some(ref mut left_writer) => {
        PathSlice::new(left_writer, select_slice.left_from, select_slice.left_to)
      },
      None => PathSlice::new(
        input_slice.writer,
        select_slice.left_from,
        select_slice.left_to,
      ),
    };
    left_slice.writer.destroy(dir)?;
    let right_slice = match select_slice.right_writer {
      Some(ref mut right_writer) => {
        PathSlice::new(right_writer, select_slice.right_from, select_slice.right_to)
      },
      None => PathSlice::new(
        input_slice.writer,
        select_slice.right_from,
        select_slice.right_to,
      ),
    };
    right_slice.writer.destroy(dir)?;
  }
  points.destroy(dir)?;
  Ok(())
}

fn compare_unsigned(a: &[u8], len_a: usize, b: &[u8], len_b: usize) -> i32 {
  a[..len_a].cmp(&b[..len_b]).to_int()
}

fn copy_points<D, R>(
  random: &mut R,
  config: BKDConfig,
  dir: &D,
  points: &mut PointWriterEnum<D::IndexOutput>,
) -> Result<PointWriterEnum<D::IndexOutput>>
where
  D: Directory,
  R: Rng + ?Sized,
{
  let mut copy = get_random_point_writer(random, config, dir, points.count())?;
  let count = points.count();
  let mut reader = points.get_reader(0, count, dir)?;
  while reader.next()? {
    let point_value_ref = reader.point_value()?;
    copy.append_point_value(&point_value_ref)?
  }
  reader.close()?;
  points.take_data(reader.remove_points());
  copy.close()?;
  Ok(copy)
}

/// returns a common prefix length equal or lower than the current one.
fn get_random_common_prefix<D, R>(
  config: BKDConfig,
  input_slice: &mut PathSlice<D::IndexOutput>,
  split_dim: usize,
  random: &mut R,
  dir: &D,
) -> Result<usize>
where
  D: Directory,
  R: Rng + ?Sized,
{
  let points_max = get_max(config.clone(), input_slice, split_dim, dir)?;
  let points_min = get_min(config.clone(), input_slice, split_dim, dir)?;
  let common_prefix_length = CoreHelper::miss_match_u8(
    &points_max[0..config.bytes_per_dim],
    &points_min[0..config.bytes_per_dim],
  );
  let common_prefix_length = if common_prefix_length == -1 {
    config.bytes_per_dim
  } else {
    common_prefix_length as usize
  };

  if random.random_bool(0.5) {
    Ok(common_prefix_length)
  } else if common_prefix_length == 0 {
    Ok(0)
  } else {
    Ok(random.random_range(0..common_prefix_length))
  }
}

fn get_random_point_writer<D, R>(
  random: &mut R,
  config: BKDConfig,
  dir: &D,
  num_points: usize,
) -> Result<PointWriterEnum<D::IndexOutput>>
where
  D: Directory,
  R: Rng + ?Sized,
{
  assert!(num_points <= i32::MAX as usize);
  if num_points < 4096 && random.random_bool(0.5) {
    Ok(PointWriterEnum::Heap(HeapPointWriter::new(
      config, num_points,
    )))
  } else {
    Ok(PointWriterEnum::Offline(OfflinePointWriter::new(
      config, dir, "test", "test", num_points,
    )?))
  }
}
#[allow(dead_code)]
fn get_directory(_num_points: i32) {
  // TODO
}

fn get_min<D>(
  config: BKDConfig,
  path_slice: &mut PathSlice<D::IndexOutput>,
  dimension: usize,
  dir: &D,
) -> Result<Vec<u8>>
where
  D: Directory,
{
  let size = config.bytes_per_dim;
  let mut min = vec![0xffu8; size];
  let mut reader = path_slice
    .writer
    .get_reader(path_slice.start, path_slice.count, dir)?;
  while reader.next()? {
    let point_value = reader.point_value()?;
    let packed_value = point_value.packed_value();
    let value_ref = packed_value.bytes;
    let packed_value_offset = packed_value.offset;
    let start_idx = packed_value_offset + dimension * config.bytes_per_dim;
    let end_idx = start_idx + size;
    let value = &value_ref[start_idx..end_idx];
    if min.as_slice().cmp(value) == Greater {
      min.copy_from(value, 0);
    }
  }
  reader.close()?;
  path_slice.writer.take_data(reader.remove_points());
  Ok(min)
}

fn get_min_doc_id<D>(
  config: BKDConfig,
  p: &mut PathSlice<D::IndexOutput>,
  dimension: usize,
  partition_point: &[u8],
  data_dim: &[u8],
  dir: &D,
) -> Result<i32>
where
  D: Directory,
{
  let mut doc_id = i32::MAX;
  let mut reader = p.writer.get_reader(p.start, p.count, dir)?;
  while reader.next()? {
    let point_value_ref = reader.point_value()?;
    let packed_value = point_value_ref.packed_value();
    let bytes = packed_value.bytes;
    let packed_value_offset = packed_value.offset;
    let offset = dimension * config.bytes_per_dim;
    let data_offset = config.packed_index_bytes_length();
    let data_length = (config.num_dims - config.num_index_dims) * config.bytes_per_dim;

    let slice1_equal1;
    let slice1_equal2;
    {
      let dim_slice = &bytes
        [(packed_value_offset + offset)..(packed_value_offset + offset + config.bytes_per_dim)];
      let partition_slice = &partition_point[0..config.bytes_per_dim];
      let data_slice = &bytes
        [(packed_value_offset + data_offset)..(packed_value_offset + data_offset + data_length)];
      let data_dim_slice = &data_dim[0..data_length];
      slice1_equal1 = dim_slice == partition_slice;
      slice1_equal2 = data_slice == data_dim_slice;
    }

    if slice1_equal1 && slice1_equal2 {
      let new_doc_id = point_value_ref.doc_id();
      if new_doc_id < doc_id {
        doc_id = new_doc_id;
      }
    }
  }
  reader.close()?;
  p.writer.take_data(reader.remove_points());
  Ok(doc_id)
}

fn get_min_data_dimension<D>(
  config: BKDConfig,
  p: &mut PathSlice<D::IndexOutput>,
  min_dim: &[u8],
  split_dim: usize,
  dir: &D,
) -> Result<Vec<u8>>
where
  D: Directory,
{
  let num_data_dims = config.num_dims - config.num_index_dims;
  let size = num_data_dims * config.bytes_per_dim;
  let mut min = vec![0xffu8; size];
  let offset = split_dim * config.bytes_per_dim;
  let mut reader = p.writer.get_reader(p.start, p.count, dir)?;
  while reader.next()? {
    let point_value_ref = reader.point_value()?;
    let packed_value = point_value_ref.packed_value();
    let value_vec = packed_value.bytes;
    let packed_value_offset = packed_value.offset;
    let start_idx = packed_value_offset + offset;
    let end_idx = packed_value_offset + offset + config.bytes_per_dim;
    let dim_slice = &value_vec[start_idx..end_idx];
    let min_dim_slice = &min_dim[0..config.bytes_per_dim];
    if min_dim_slice == dim_slice {
      let copy_start = packed_value_offset + config.num_index_dims * config.bytes_per_dim;
      let copy_end = copy_start + size;
      let value = &value_vec[copy_start..copy_end];
      if min.as_slice().cmp(value) == Greater {
        min.copy_from(value, 0);
      }
    }
  }
  reader.close()?;
  p.writer.take_data(reader.remove_points());
  Ok(min)
}

fn get_max<D>(
  config: BKDConfig,
  p: &mut PathSlice<D::IndexOutput>,
  dimension: usize,
  dir: &D,
) -> Result<Vec<u8>>
where
  D: Directory,
{
  let size = config.bytes_per_dim;
  let mut max = vec![0u8; size];
  let mut reader = p.writer.get_reader(p.start, p.count, dir)?;
  while reader.next()? {
    let point_value_ref = reader.point_value()?;
    let packed_value = point_value_ref.packed_value();
    let bytes_ref = packed_value.bytes;
    let packed_value_offset = packed_value.offset;
    let start_idx = packed_value_offset + dimension * config.bytes_per_dim;
    let end_idx = start_idx + size;
    let value = &bytes_ref[start_idx..end_idx];
    if max.as_slice().cmp(value) == Less {
      max.copy_from(value, 0);
    }
  }
  reader.close()?;
  p.writer.take_data(reader.remove_points());
  Ok(max)
}

fn get_max_data_dimension<D>(
  config: BKDConfig,
  p: &mut PathSlice<D::IndexOutput>,
  max_dim: &[u8],
  split_dim: usize,
  dir: &D,
) -> Result<Vec<u8>>
where
  D: Directory,
{
  let num_data_dims = config.num_dims - config.num_index_dims;
  let size = num_data_dims * config.bytes_per_dim;
  let mut max = vec![0u8; size];
  let offset = split_dim * config.bytes_per_dim;
  let mut reader = p.writer.get_reader(p.start, p.count, dir)?;
  while reader.next()? {
    let point_value_ref = reader.point_value()?;
    let packed_value = point_value_ref.packed_value();
    let value_vec = packed_value.bytes;
    let packed_value_offset = packed_value.offset;

    let start_idx = packed_value_offset + offset;
    let end_idx = start_idx + config.bytes_per_dim;
    let dim_slice = &value_vec[start_idx..end_idx];
    let max_dim_slice = &max_dim[0..config.bytes_per_dim];
    if max_dim_slice == dim_slice {
      let copy_start = packed_value_offset + config.packed_index_bytes_length();
      let copy_end = copy_start + size;
      let value = &value_vec[copy_start..copy_end];
      if max.as_slice().cmp(value) == Less {
        max.copy_from(value, 0);
      }
    }
  }
  reader.close()?;
  p.writer.take_data(reader.remove_points());
  Ok(max)
}

fn get_max_doc_id<D>(
  config: BKDConfig,
  p: &mut PathSlice<D::IndexOutput>,
  dimension: usize,
  partition_point: &[u8],
  data_dim: &[u8],
  dir: &D,
) -> Result<i32>
where
  D: Directory,
{
  let mut doc_id = i32::MIN;
  let mut reader = p.writer.get_reader(p.start, p.count, dir)?;
  while reader.next()? {
    let point_value_ref = reader.point_value()?;
    let packed_value = point_value_ref.packed_value();
    let value = packed_value.bytes;
    let packed_value_offset = packed_value.offset;
    let offset = dimension * config.bytes_per_dim;
    let data_offset = config.packed_index_bytes_length();
    let data_length = (config.num_dims - config.num_index_dims) * config.bytes_per_dim;
    let slice1_equal1;
    let slice1_equal2;
    {
      let dim_slice = &value
        [(packed_value_offset + offset)..(packed_value_offset + offset + config.bytes_per_dim)];
      let partition_slice = &partition_point[0..config.bytes_per_dim];

      let data_slice = &value
        [(packed_value_offset + data_offset)..(packed_value_offset + data_offset + data_length)];
      let data_dim_slice = &data_dim[0..data_length];
      slice1_equal1 = dim_slice == partition_slice;
      slice1_equal2 = data_slice == data_dim_slice;
    }

    if slice1_equal1 && slice1_equal2 {
      let new_doc_id = point_value_ref.doc_id();
      if new_doc_id > doc_id {
        doc_id = new_doc_id;
      }
    }
  }
  reader.close()?;
  p.writer.take_data(reader.remove_points());
  Ok(doc_id)
}

fn get_random_config<R>(random: &mut R) -> Result<BKDConfig>
where
  R: Rng + ?Sized,
{
  let num_index_dims = TestUtil::next_usize(random, 1, BKDConfig::MAX_INDEX_DIMS);
  let num_dims = TestUtil::next_usize(random, num_index_dims, BKDConfig::MAX_DIMS);
  let bytes_per_dim = TestUtil::next_usize(random, 2, 30);
  let max_points_in_leaf_node = TestUtil::next_usize(random, 50, 2000);
  BKDConfig::new(
    num_dims,
    num_index_dims,
    bytes_per_dim,
    max_points_in_leaf_node,
  )
}
