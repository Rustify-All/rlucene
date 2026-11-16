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
use rand::Rng;
use rand::RngExt;

use crate::core::store::IndexOutput;
use crate::core::store::dummy::dummy_index_output::DummyIndexOutput;
use crate::core::util::bkd::bkd_config::BKDConfig;
use crate::core::util::bkd::bkd_radix_selector::BKDRadixSelector;
use crate::core::util::bkd::heap_point_write::HeapPointWriter;
use crate::core::util::bkd::point_value::PointValue;
use crate::core::util::bkd::point_writer::{PointWriter, PointWriterEnum};
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::{CoreHelper, ToInt};
use crate::test_framework::core::util::test_util::TestUtil;
#[allow(dead_code)] // for quick search
struct TestBKDRadixSort;
#[test]
fn test_random() -> Result<()> {
  let mut random = random();
  let config = get_random_config(&mut random)?;
  let num_points = TestUtil::next_usize(&mut random, 1, BKDConfig::DEFAULT_MAX_POINTS_IN_LEAF_NODE);
  let mut heap_points = HeapPointWriter::new(config.clone(), num_points);
  let mut value = vec![0u8; config.packed_bytes_length()];
  for i in 0..num_points {
    random.fill(&mut value[..]);
    heap_points.append_bytes(&value, i as i32)?;
  }
  heap_points.close()?;
  let mut points = PointWriterEnum::<DummyIndexOutput>::Heap(heap_points);
  verify_sort(&mut random, config, &mut points, 0, num_points)?;
  Ok(())
}
#[test]
fn test_random_all_equals() -> Result<()> {
  let mut random = random();
  let config = get_random_config(&mut random)?;
  let num_points = TestUtil::next_usize(&mut random, 1, BKDConfig::DEFAULT_MAX_POINTS_IN_LEAF_NODE);
  let mut heap_points = HeapPointWriter::new(config.clone(), num_points);
  let mut value = vec![0u8; config.packed_bytes_length()];
  random.fill(&mut value[..]);
  for _ in 0..num_points {
    let doc_id = random.random_range(0..num_points);
    heap_points.append_bytes(&value, doc_id as i32)?;
  }
  heap_points.close()?;
  let mut points = PointWriterEnum::<DummyIndexOutput>::Heap(heap_points);
  verify_sort(&mut random, config, &mut points, 0, num_points)?;
  Ok(())
}
#[test]
fn test_random_last_byte_two_values() -> Result<()> {
  let mut random = random();
  let config = get_random_config(&mut random)?;
  let num_points = TestUtil::next_usize(&mut random, 1, BKDConfig::DEFAULT_MAX_POINTS_IN_LEAF_NODE);
  let mut heap_points = HeapPointWriter::new(config.clone(), num_points);
  let mut value = vec![0u8; config.packed_bytes_length()];
  random.fill(&mut value[..]);
  for _ in 0..num_points {
    if random.random_bool(0.5) {
      heap_points.append_bytes(&value, 1)?;
    } else {
      heap_points.append_bytes(&value, 2)?;
    }
  }
  heap_points.close()?;
  let mut points = PointWriterEnum::<DummyIndexOutput>::Heap(heap_points);
  verify_sort(&mut random, config, &mut points, 0, num_points)?;
  Ok(())
}

#[test]
fn test_random_few_different_values() -> Result<()> {
  let mut random = random();
  let config = get_random_config(&mut random)?;
  let num_points = TestUtil::next_usize(&mut random, 1, BKDConfig::DEFAULT_MAX_POINTS_IN_LEAF_NODE);
  let mut heap_points = HeapPointWriter::new(config.clone(), num_points);
  let number_values = TestUtil::next_usize(&mut random, 2, 9);
  let mut different_values: Vec<Vec<u8>> = Vec::with_capacity(number_values);
  for _ in 0..number_values {
    let mut buf = vec![0u8; config.packed_bytes_length()];
    random.fill(&mut buf[..]);
    different_values.push(buf);
  }
  for i in 0..num_points {
    let index = TestUtil::next_usize(&mut random, 0, number_values - 1);
    heap_points.append_bytes(&different_values[index], i as i32)?;
  }
  heap_points.close()?;
  let mut points = PointWriterEnum::<DummyIndexOutput>::Heap(heap_points);
  verify_sort(&mut random, config, &mut points, 0, num_points)?;
  Ok(())
}

#[test]
fn test_random_data_dim_different() -> Result<()> {
  let mut random = random();
  let config = get_random_config(&mut random)?;
  let num_points = TestUtil::next_usize(&mut random, 1, BKDConfig::DEFAULT_MAX_POINTS_IN_LEAF_NODE);
  let mut heap_points = HeapPointWriter::new(config.clone(), num_points);
  let total_data_dimension = config.num_dims - config.num_index_dims;
  let data_dim_length = total_data_dimension * config.bytes_per_dim;
  let mut data_dimension_values = vec![0u8; data_dim_length];
  let mut value = vec![0u8; config.packed_bytes_length()];
  random.fill(&mut value[..]);
  for _ in 0..num_points {
    random.fill(&mut data_dimension_values[..]);
    let start = config.packed_index_bytes_length();
    let copy_source = &data_dimension_values;
    value[start..][..copy_source.len()].copy_from_slice(copy_source);
    let doc_id = random.random_range(0..num_points);
    heap_points.append_bytes(&value, doc_id as i32)?;
  }
  heap_points.close()?;
  let mut points = PointWriterEnum::<DummyIndexOutput>::Heap(heap_points);
  verify_sort(&mut random, config, &mut points, 0, num_points)?;
  Ok(())
}

fn verify_sort<O, R>(
  random: &mut R,
  config: BKDConfig,
  points: &mut PointWriterEnum<O>,
  start: usize,
  end: usize,
) -> Result<()>
where
  O: IndexOutput,
  R: Rng + ?Sized,
{
  let mut radix_selector = BKDRadixSelector::new(config.clone(), 1000, "test");
  // we check for each dimension
  for split_dim in 0..config.num_dims {
    let common_prefix_length;
    {
      common_prefix_length =
        get_random_common_prefix(config.clone(), points, start, end, split_dim, random)?;
    }

    match points {
      PointWriterEnum::Heap(heap) => {
        radix_selector.heap_radix_sort(heap, start, end, split_dim, common_prefix_length)?;
      },
      PointWriterEnum::Offline(_) => unreachable!("sort fixtures construct heap writers"),
    }

    let mut previous = vec![0u8; config.packed_bytes_length()];
    let mut previous_doc_id = -1;
    previous.fill(0);

    let dim_offset = split_dim * config.bytes_per_dim;

    match points {
      PointWriterEnum::Heap(heap_writer) => {
        for j in start..end {
          let point_value = heap_writer.get_packed_value_slice(j)?;
          let mut cmp;
          let packed_value = point_value.packed_value();
          let bytes_ref = packed_value.bytes;
          let packed_value_offset = packed_value.offset;
          {
            cmp = bytes_ref[packed_value_offset + dim_offset
              ..packed_value_offset + dim_offset + config.bytes_per_dim]
              .cmp(&previous[dim_offset..dim_offset + config.bytes_per_dim])
              .to_int();
            assert!(
              cmp >= 0,
              "Sorting validation failed for split_dim {}, cmp: {}",
              split_dim,
              cmp
            );

            if cmp == 0 {
              let data_offset = config.num_index_dims * config.bytes_per_dim;
              cmp = bytes_ref[packed_value_offset + data_offset
                ..packed_value_offset + config.packed_bytes_length()]
                .cmp(&previous[data_offset..config.packed_bytes_length()])
                .to_int();
              assert!(cmp >= 0, "Data dimension sorting validation failed");
            }
          }

          if cmp == 0 {
            let doc_id = point_value.doc_id();
            assert!(
              doc_id >= previous_doc_id,
              "DocID order validation failed: {} < {}",
              doc_id,
              previous_doc_id
            );
          }

          {
            let copy_source =
              &bytes_ref[packed_value_offset..packed_value_offset + config.packed_bytes_length()];
            previous[..copy_source.len()].copy_from_slice(copy_source);
          }
          previous_doc_id = point_value.doc_id();
        }
      },
      _ => {
        unreachable!()
      },
    }
  }

  Ok(())
}
fn get_random_common_prefix<O, R>(
  config: BKDConfig,
  points: &mut PointWriterEnum<O>,
  start: usize,
  end: usize,
  sort_dim: usize,
  random: &mut R,
) -> Result<usize>
where
  O: IndexOutput,
  R: Rng + ?Sized,
{
  match points {
    PointWriterEnum::Heap(heap_writer) => {
      let mut common_prefix_length = config.bytes_per_dim;
      let point_value = heap_writer.get_packed_value_slice(start)?;
      let packed_value = point_value.packed_value();
      let bytes_ref = packed_value.bytes;
      let packed_value_offset = packed_value.offset;
      let mut first_value = vec![0u8; config.bytes_per_dim];
      let offset = sort_dim * config.bytes_per_dim;
      let copy_source = &bytes_ref
        [packed_value_offset + offset..packed_value_offset + offset + config.bytes_per_dim];
      first_value[..copy_source.len()].copy_from_slice(copy_source);
      for i in (start + 1)..end {
        let point_value = heap_writer.get_packed_value_slice(i)?;
        let packed_value = point_value.packed_value();
        let bytes_ref = packed_value.bytes;
        let packed_value_offset = packed_value.offset;
        let diff = CoreHelper::miss_match_u8(
          &bytes_ref
            [packed_value_offset + offset..packed_value_offset + offset + config.bytes_per_dim],
          &first_value,
        );
        if diff != -1 {
          let diff = diff as usize;
          if common_prefix_length > diff {
            if diff == 0 {
              return Ok(diff);
            }
            common_prefix_length = diff;
          }
        }
      }

      if random.random_bool(0.5) {
        Ok(common_prefix_length)
      } else {
        Ok(random.random_range(0..common_prefix_length))
      }
    },
    _ => {
      unreachable!("should not be here");
    },
  }
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
