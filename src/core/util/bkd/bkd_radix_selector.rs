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

use crate::core::store::IndexOutput;
use crate::core::store::directory::Directory;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bkd::bkd_config::BKDConfig;
use crate::core::util::bkd::heap_point_write::HeapPointWriter;
use crate::core::util::bkd::offline_point_write::OfflinePointWriter;
use crate::core::util::bkd::point_reader::PointReader;
use crate::core::util::bkd::point_value::{PointValue, PointValueEnum};
use crate::core::util::bkd::point_writer::{PointWriter, PointWriterEnum};
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::intro_sorter::IntroSorter;
use crate::core::util::radix_selector::{RadixSelector, RadixSelectorBase};
use crate::core::util::selector::Selector;
use crate::core::util::{
  CoreHelper, IOUtils, IntroSelector, IntroSelectorBase, IntroSelectorBaseDefault, MSBRadixSorter,
  MSBRadixSorterBase, SliceCopyOps, Sorter,
};

/// Offline Radix selector for BKD tree.
pub struct BKDRadixSelector {
  // histogram array
  histogram: Vec<usize>,
  // number of bytes to be sorted: config.bytesPerDim() + Integer.BYTES
  bytes_sorted: usize,
  // flag to when we are moving to sort on heap
  max_points_sort_in_heap: usize,
  // reusable buffer
  offline_buffer: Vec<u8>,
  // holder for partition points
  partition_bucket: Vec<i32>,
  // scratch array to hold temporary data
  scratch: Vec<u8>,
  // prefix for temp files
  temp_file_name_prefix: String,
  // BKD tree configuration
  config: BKDConfig,
}
pub struct SelectorSlice<O>
where
  O: IndexOutput,
{
  pub(crate) partition: Vec<u8>,

  pub(crate) left_writer: Option<PointWriterEnum<O>>,
  pub(crate) left_from: usize,
  pub(crate) left_to: usize,

  pub(crate) right_writer: Option<PointWriterEnum<O>>,
  pub(crate) right_from: usize,
  pub(crate) right_to: usize,
}
impl<O> SelectorSlice<O>
where
  O: IndexOutput,
{
  fn new(
    partition: Vec<u8>,
    left_writer: Option<PointWriterEnum<O>>,
    left_from: usize,
    left_to: usize,

    right_writer: Option<PointWriterEnum<O>>,
    right_from: usize,
    right_to: usize,
  ) -> Self {
    Self {
      partition,
      left_writer,
      left_from,
      left_to,
      right_writer,
      right_from,
      right_to,
    }
  }
}
impl BKDRadixSelector {
  // size of the histogram
  const HISTOGRAM_SIZE: usize = 256;
  // size of the online buffer: 8 KB
  const MAX_SIZE_OFFLINE_BUFFER: usize = 1024 * 8;
  /// Creates a new instance.
  pub fn new(
    config: BKDConfig,
    max_points_sort_in_heap: usize,
    temp_file_name_prefix: &str,
  ) -> Self {
    // Selection and sorting is done in a given dimension. In case the value
    // of the dimension are equal
    // between two points we tie break first using the data-only dimensions
    // and if those are still equal
    // we tie-break on the docID. Here we account for all bytes used in the
    // process.
    let bytes_sorted = config.bytes_per_dim
      + (config.num_dims - config.num_index_dims) * config.bytes_per_dim
      + BitUtil::INT_BYTES;
    let number_of_points_offline = Self::MAX_SIZE_OFFLINE_BUFFER / config.bytes_per_doc();
    let offline_buffer = vec![0u8; number_of_points_offline * config.bytes_per_doc()];
    let partition_bucket = vec![0; bytes_sorted];
    let histogram = vec![0; Self::HISTOGRAM_SIZE];
    let scratch = vec![0u8; bytes_sorted];
    BKDRadixSelector {
      config,
      max_points_sort_in_heap,
      temp_file_name_prefix: temp_file_name_prefix.to_string(),
      bytes_sorted,
      offline_buffer,
      partition_bucket,
      histogram,
      scratch,
    }
  }

  /// It uses the provided `points` from the given `from` to the given `to` to
  /// populate the `partitionSlices` array holder (length > 1) with two path
  /// slices so the path slice at position 0 contains `partition - from`
  /// points where the value of the `dim` is lower or equal to the `to -
  /// from` points on the slice at position 1.
  ///
  /// The `dimCommonPrefix` provides a hint for the length of the common
  /// prefix length for the `dim` where are partitioning the points.
  ///
  /// It return the value of the `dim` at the partition point.
  ///
  /// If the provided `points` is wrapping an `OfflinePointWriter`, the writer
  /// is destroyed in the process to save disk space.
  #[allow(clippy::too_many_arguments)]
  pub fn select<D>(
    &mut self,
    points: &mut PathSlice<D::IndexOutput>,
    from: usize,
    to: usize,
    partition_point: usize,
    dim: usize,
    dim_common_prefix: usize,
    temp_dir: &D,
  ) -> Result<SelectorSlice<D::IndexOutput>>
  where
    D: Directory,
  {
    Self::check_args(from, to, partition_point)?;
    let is_heap = { matches!(points.writer, PointWriterEnum::Heap(_)) };
    if is_heap {
      let partition = self.heap_radix_select(
        points.writer,
        dim,
        from,
        to,
        partition_point,
        dim_common_prefix,
      )?;
      Ok(SelectorSlice::new(
        partition,
        None,
        from,
        partition_point - from,
        None,
        partition_point,
        to - partition_point,
      ))
    } else {
      let mut left_writer =
        self.get_point_writer(partition_point - from, &format!("left{dim}"), temp_dir)?;
      let mut right_writer = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        self.get_point_writer(to - partition_point, &format!("right{dim}"), temp_dir)
      })) {
        Ok(Ok(right_writer)) => right_writer,
        right_result => {
          let close_result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| left_writer.close()));
          return IOUtils::use_or_suppress_caught_result(right_result, close_result)
            .map(|_| unreachable!());
        },
      };
      if let PointWriterEnum::Offline(offline_point_writer) = points.writer {
        let partition_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
          self.build_histogram_and_partition(
            offline_point_writer,
            &mut left_writer,
            &mut right_writer,
            from,
            to,
            partition_point,
            0,
            dim_common_prefix,
            dim,
            temp_dir,
          )
        }));
        let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
          IOUtils::close([&mut right_writer, &mut left_writer], |writer| {
            writer.close()
          })
        }));
        let partition = IOUtils::use_or_suppress_caught_result(partition_result, close_result)?;

        Ok(SelectorSlice::new(
          partition,
          Some(left_writer),
          0,
          partition_point - from,
          Some(right_writer),
          0,
          to - partition_point,
        ))
      } else {
        Err(LuceneError::illegal_state("writer is not Offline"))
      }
    }
  }

  fn check_args(from: usize, to: usize, partition_point: usize) -> Result<()> {
    if partition_point < from {
      return Err(LuceneError::illegal_argument(
        "partitionPoint must be >= from",
      ));
    }
    if partition_point >= to {
      return Err(LuceneError::illegal_argument("partitionPoint must be < to"));
    }
    Ok(())
  }

  fn find_common_prefix_and_histogram<D>(
    &mut self,
    points: &mut OfflinePointWriter<D::IndexOutput>,
    from: usize,
    to: usize,
    dim: usize,
    dim_common_prefix: usize,
    temp_dir: &D,
  ) -> Result<usize>
  where
    D: Directory,
  {
    let mut common_prefix_position = self.bytes_sorted;
    let offset = dim * self.config.bytes_per_dim;
    let mut reader = points.get_reader_with_buffer(
      from,
      to - from,
      std::mem::take(&mut self.offline_buffer),
      temp_dir,
    )?;
    let body_result =
      std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<usize> {
        debug_assert!(common_prefix_position > dim_common_prefix);
        reader.next()?;
        {
          let point_value = reader.point_value()?;
          let (value, packed_value_offset, _) = point_value.packed_value_doc_id_bytes();

          let mut start = packed_value_offset + offset;
          let mut end = start + self.config.bytes_per_dim;
          self.scratch.copy_from(&value[start..end], 0);

          start = packed_value_offset + self.config.packed_index_bytes_length();
          end = start
            + ((self.config.num_dims - self.config.num_index_dims) * self.config.bytes_per_dim)
            + BitUtil::INT_BYTES;
          self
            .scratch
            .copy_from(&value[start..end], self.config.bytes_per_dim);
        }
        let mut histogram_index;
        for i in (from + 1)..to {
          reader.next()?;
          if common_prefix_position == dim_common_prefix {
            {
              let point_value = reader.point_value()?;
              histogram_index =
                self.get_bucket(offset, common_prefix_position, point_value) as usize;
              self.histogram[histogram_index] += 1;
            }
            for _ in (i + 1)..to {
              reader.next()?;
              let point_value = reader.point_value()?;
              histogram_index =
                self.get_bucket(offset, common_prefix_position, point_value) as usize;
              self.histogram[histogram_index] += 1;
            }
            break;
          } else {
            let point_value = reader.point_value()?;
            // Check common prefix and adjust histogram
            let scratch_start_index =
              std::cmp::min(dim_common_prefix, self.config.bytes_per_dim) as usize;
            let scratch_end_index =
              std::cmp::min(common_prefix_position, self.config.bytes_per_dim) as usize;
            let (value, packed_value_offset, _length) = point_value.packed_value_doc_id_bytes();
            let packed_value_start_index = (packed_value_offset + offset) + scratch_start_index;
            let packed_value_end_index = (packed_value_offset + offset) + scratch_end_index;
            let j = CoreHelper::miss_match(
              &self.scratch[scratch_start_index..scratch_end_index],
              &value[packed_value_start_index..packed_value_end_index],
            );
            if j == -1 {
              if common_prefix_position > self.config.bytes_per_dim {
                let start_tie_break = self.config.packed_index_bytes_length();
                let end_tie_break =
                  start_tie_break + common_prefix_position - self.config.bytes_per_dim;
                let k = CoreHelper::miss_match(
                  &self.scratch[self.config.bytes_per_dim..common_prefix_position],
                  &value[(packed_value_offset + start_tie_break)
                    ..(packed_value_offset + end_tie_break)],
                );
                if k != -1 {
                  common_prefix_position = self.config.bytes_per_dim + k as usize;
                  self.histogram.fill(0);
                  self.histogram[self.scratch[common_prefix_position] as usize] = i - from;
                }
              }
            } else {
              common_prefix_position = dim_common_prefix + j as usize;
              self.histogram.fill(0);
              self.histogram[self.scratch[common_prefix_position] as usize] = i - from;
            }
            if common_prefix_position != self.bytes_sorted {
              histogram_index =
                self.get_bucket(offset, common_prefix_position, point_value) as usize;
              self.histogram[histogram_index] += 1;
            }
          }
        }
        Ok(common_prefix_position)
      }));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<usize> {
      match &mut reader.point_value {
        PointValueEnum::Offline(offline_point_value) => {
          self.offline_buffer = std::mem::take(&mut offline_point_value.value);
        },
        _ => {
          debug_assert!(false, "PointValueEnum must be Offline");
        },
      }
      unwrap_caught_result!(body_result)
    }));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reader.close()));
    let common_prefix_position = IOUtils::use_or_suppress_caught_result(result, close_result)?;
    // Build partition buckets up to commonPrefix
    for i in 0..common_prefix_position {
      self.partition_bucket[i] = self.scratch[i] as i32;
    }
    Ok(common_prefix_position)
  }

  fn get_bucket(
    &self,
    offset: usize,
    common_prefix_position: usize,
    point_value: &PointValueEnum,
  ) -> i32 {
    if common_prefix_position < self.config.bytes_per_dim {
      let (packed_value, packed_value_offset, _length) = point_value.packed_value();
      let index = packed_value_offset + offset + common_prefix_position;
      packed_value[index] as i32
    } else {
      let (packed_value, packed_value_offset, _length) = point_value.packed_value_doc_id_bytes();
      let index =
        packed_value_offset + self.config.packed_index_bytes_length() + common_prefix_position
          - self.config.bytes_per_dim;
      packed_value[index] as i32
    }
  }
  #[allow(clippy::too_many_arguments)]
  fn build_histogram_and_partition<D>(
    &mut self,
    points: &mut OfflinePointWriter<D::IndexOutput>,
    left: &mut PointWriterEnum<D::IndexOutput>,
    right: &mut PointWriterEnum<D::IndexOutput>,
    from: usize,
    to: usize,
    partition_point: usize,
    iteration: usize,
    base_common_prefix: usize,
    dim: usize,
    temp_dir: &D,
  ) -> Result<Vec<u8>>
  where
    D: Directory,
  {
    // Find common prefix from baseCommonPrefix and build histogram
    let common_prefix =
      self.find_common_prefix_and_histogram(points, from, to, dim, base_common_prefix, temp_dir)?;
    // If all equals we just partition the points
    if common_prefix == self.bytes_sorted {
      debug_assert!(common_prefix >= 1);
      self.offline_partition(
        points,
        left,
        right,
        None,
        from,
        to,
        dim,
        common_prefix - 1,
        partition_point,
        temp_dir,
      )?;
      return self.partition_point_from_common_prefix();
    }

    let mut left_count = 0usize;
    let mut right_count = 0usize;
    // Count left points and record the partition point
    for i in 0..Self::HISTOGRAM_SIZE {
      let size = self.histogram[i];
      debug_assert!(partition_point >= from);
      if left_count + size > partition_point - from {
        self.partition_bucket[common_prefix] = i as i32;
        break;
      }
      left_count += size;
    }
    // Count right points
    for i in (self.partition_bucket[common_prefix] as usize + 1)..Self::HISTOGRAM_SIZE {
      right_count += self.histogram[i];
    }
    let delta = self.histogram[self.partition_bucket[common_prefix] as usize];
    debug_assert_eq!(
      left_count + right_count + delta,
      to - from,
      "{} / {}",
      left_count + right_count + delta,
      to - from
    );
    // Special case when points are equal except last byte, we can just
    // tie-break
    if common_prefix == self.bytes_sorted - 1 {
      let tie_break_count = partition_point - from - left_count;
      self.offline_partition(
        points,
        left,
        right,
        None,
        from,
        to,
        dim,
        common_prefix,
        tie_break_count,
        temp_dir,
      )?;
      return self.partition_point_from_common_prefix();
    }

    // Create the delta points writer
    let mut delta_points = self.get_delta_point_writer(left, right, delta, iteration, temp_dir)?;
    let partition_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      self.offline_partition(
        points,
        left,
        right,
        Some(&mut delta_points),
        from,
        to,
        dim,
        common_prefix,
        0,
        temp_dir,
      )
    }));
    let close_result =
      std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| delta_points.close()));
    IOUtils::use_or_suppress_caught_result(partition_result, close_result)?;
    let new_partition_point = partition_point - from - left_count;

    // Depending on the concrete type of delta_points, call the appropriate
    // partition method.
    let count = delta_points.count();
    match delta_points {
      PointWriterEnum::Heap(_) => self.heap_partition(
        delta_points,
        left,
        right,
        dim,
        0,
        count,
        new_partition_point,
        common_prefix + 1,
      ),
      PointWriterEnum::Offline(mut offline_writer) => self.build_histogram_and_partition(
        &mut offline_writer,
        left,
        right,
        0,
        count,
        new_partition_point,
        iteration + 1,
        common_prefix + 1,
        dim,
        temp_dir,
      ),
    }
  }

  #[allow(clippy::too_many_arguments)]
  fn offline_partition<D>(
    &mut self,
    points: &mut OfflinePointWriter<D::IndexOutput>,
    left: &mut PointWriterEnum<D::IndexOutput>,
    right: &mut PointWriterEnum<D::IndexOutput>,
    mut delta_points: Option<&mut PointWriterEnum<D::IndexOutput>>,
    from: usize,
    to: usize,
    dim: usize,
    byte_position: usize,
    num_docs_tiebreak: usize,
    temp_dir: &D,
  ) -> Result<()>
  where
    D: Directory,
  {
    debug_assert!(byte_position == self.bytes_sorted - 1 || delta_points.is_some());
    let offset = dim * self.config.bytes_per_dim;
    let mut tiebreak_counter = 0usize;
    let mut reader = points.get_reader_with_buffer(
      from,
      to - from,
      std::mem::take(&mut self.offline_buffer),
      temp_dir,
    )?;
    let body_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      while reader.next()? {
        let point_value = reader.point_value()?;
        let bucket = self.get_bucket(offset, byte_position, point_value);
        if bucket < self.partition_bucket[byte_position] {
          left.append_point_value(point_value)?;
        } else if bucket > self.partition_bucket[byte_position] {
          right.append_point_value(point_value)?;
        } else if byte_position == self.bytes_sorted - 1 {
          if tiebreak_counter < num_docs_tiebreak {
            left.append_point_value(point_value)?;
            tiebreak_counter += 1;
          } else {
            right.append_point_value(point_value)?;
          }
        } else if let Some(dp) = delta_points.as_mut() {
          dp.append_point_value(point_value)?;
        }
      }
      Ok(())
    }));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      match &mut reader.point_value {
        PointValueEnum::Offline(offline_point_value) => {
          self.offline_buffer = std::mem::take(&mut offline_point_value.value);
        },
        _ => {
          debug_assert!(false, "PointValueEnum must be Offline");
        },
      }
      unwrap_caught_result!(body_result)
    }));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reader.close()));
    IOUtils::use_or_suppress_caught_result(result, close_result)?;
    // Delete original file
    points.destroy(temp_dir)?;
    Ok(())
  }

  fn partition_point_from_common_prefix(&self) -> Result<Vec<u8>> {
    let mut partition = vec![0u8; self.config.bytes_per_dim];
    for (i, p) in partition
      .iter_mut()
      .enumerate()
      .take(self.config.bytes_per_dim)
    {
      *p = self.partition_bucket[i] as u8;
    }
    Ok(partition)
  }

  #[allow(clippy::too_many_arguments)]
  fn heap_partition<O>(
    &self,
    mut points: PointWriterEnum<O>,
    left: &mut PointWriterEnum<O>,
    right: &mut PointWriterEnum<O>,
    dim: usize,
    from: usize,
    to: usize,
    partition_point: usize,
    common_prefix: usize,
  ) -> Result<Vec<u8>>
  where
    O: IndexOutput,
  {
    let partition =
      self.heap_radix_select(&mut points, dim, from, to, partition_point, common_prefix)?;
    match points {
      PointWriterEnum::Heap(ref mut heap_writer) => {
        for i in from..to {
          let value = heap_writer.get_packed_value_slice(i)?;
          if i < partition_point {
            left.append_point_value(value)?;
          } else {
            right.append_point_value(value)?;
          }
        }
        Ok(partition)
      },
      _ => {
        debug_assert!(false, "Point writer is not a heap writer");
        Ok(vec![0u8; 0])
      },
    }
  }
  /// Sort the heap writer by the specified dim. It is used to sort the leaves
  /// of the tree/`.
  pub fn heap_radix_select<O>(
    &self,
    points: &mut PointWriterEnum<O>,
    dim: usize,
    from: usize,
    to: usize,
    partition_point: usize,
    common_prefix_length: usize,
  ) -> Result<Vec<u8>>
  where
    O: IndexOutput,
  {
    let bytes_per_dim = self.config.bytes_per_dim;
    let dim_offset = dim * bytes_per_dim + common_prefix_length;
    let max_length = self
      .bytes_sorted
      .checked_sub(common_prefix_length)
      .ok_or_else(|| {
        LuceneError::illegal_argument("common_prefix_length must be <= bytes_sorted")
      })?;
    let dim_cmp_bytes = bytes_per_dim.saturating_sub(common_prefix_length);
    let data_offset = if common_prefix_length < bytes_per_dim {
      self.config.packed_index_bytes_length() - dim_cmp_bytes
    } else {
      self.config.packed_index_bytes_length() + common_prefix_length - bytes_per_dim
    };
    let sub_selector = RadixSelectorImpl {
      points,
      common_prefix_length,
      dim_cmp_bytes,
      dim_offset,
      data_offset,
      dim,
      bytes_per_dim,
      bytes_sorted: self.bytes_sorted,
    };

    debug_assert!(self.bytes_sorted >= common_prefix_length);
    let mut radix_selector = RadixSelector::new(max_length, sub_selector);
    radix_selector.select(from, to, partition_point)?;

    let mut partition = vec![0u8; bytes_per_dim];

    match points {
      PointWriterEnum::Heap(heap_writer) => {
        let point_value = heap_writer.get_packed_value_slice(partition_point)?;
        let (bytes, offset, _length) = point_value.packed_value();

        let start = offset + (dim * bytes_per_dim);
        let end = start + bytes_per_dim;

        partition.copy_from(&bytes[start..end], 0);
        Ok(partition)
      },
      _ => Err(LuceneError::unreachable(
        "Point writer is not a heap writer",
      )),
    }
  }

  /// Sort the heap writer by the specified dim. It is used to sort the leaves
  /// of the tree.
  pub fn heap_radix_sort<O>(
    &self,
    points: &mut PointWriterEnum<O>,
    from: usize,
    to: usize,
    dim: usize,
    common_prefix_length: usize,
  ) -> Result<()>
  where
    O: IndexOutput,
  {
    let bytes_per_dim = self.config.bytes_per_dim;
    let dim_offset = dim * bytes_per_dim + common_prefix_length;
    let max_length = self
      .bytes_sorted
      .checked_sub(common_prefix_length)
      .ok_or_else(|| {
        LuceneError::illegal_argument("common_prefix_length must be <= bytes_sorted")
      })?;
    let dim_cmp_bytes = bytes_per_dim.saturating_sub(common_prefix_length);
    let data_offset = if common_prefix_length < bytes_per_dim {
      self.config.packed_index_bytes_length() - dim_cmp_bytes
    } else {
      self.config.packed_index_bytes_length() + common_prefix_length - bytes_per_dim
    };
    let delegate = MSBRadixSorterImpl {
      points,
      dim_cmp_bytes,
      dim_offset,
      data_offset,
      common_prefix_length,
      dim,
      bytes_per_dim,
      bytes_sorted: self.bytes_sorted,
    };
    let mut msb_radix_sorter = MSBRadixSorter::new(max_length, delegate);
    msb_radix_sorter.sort(from, to)
  }

  fn get_delta_point_writer<D>(
    &mut self,
    left: &mut PointWriterEnum<D::IndexOutput>,
    right: &mut PointWriterEnum<D::IndexOutput>,
    delta: usize,
    iteration: usize,
    temp_dir: &D,
  ) -> Result<PointWriterEnum<D::IndexOutput>>
  where
    D: Directory,
  {
    if delta >= i32::MAX as usize {
      return Err(LuceneError::number_overflow("Delta is too large"));
    }
    if delta <= self.get_max_points_sort_in_heap(left, right) {
      Ok(PointWriterEnum::Heap(HeapPointWriter::new(
        self.config.clone(),
        delta,
      )))
    } else {
      Ok(PointWriterEnum::Offline(OfflinePointWriter::new(
        self.config.clone(),
        temp_dir,
        &self.temp_file_name_prefix,
        &format!("delta{iteration}"),
        delta,
      )?))
    }
  }

  fn get_max_points_sort_in_heap<O>(
    &self,
    left: &mut PointWriterEnum<O>,
    right: &mut PointWriterEnum<O>,
  ) -> usize
  where
    O: IndexOutput,
  {
    let mut points_used = 0;
    if let &mut PointWriterEnum::Heap(ref heap_writer) = left {
      points_used += heap_writer.size;
    }
    if let &mut PointWriterEnum::Heap(ref heap_writer) = right {
      points_used += heap_writer.size;
    }
    debug_assert!(self.max_points_sort_in_heap >= points_used);
    self.max_points_sort_in_heap - points_used
  }

  fn get_point_writer<D>(
    &self,
    count: usize,
    desc: &str,
    temp_dir: &D,
  ) -> Result<PointWriterEnum<D::IndexOutput>>
  where
    D: Directory,
  {
    // As we recurse, we hold two on-heap point writers at any point.
    // Therefore the max size for these objects is half of the total
    // points we can have on-heap.
    if count <= self.max_points_sort_in_heap / 2 {
      let size = count;
      Ok(PointWriterEnum::Heap(HeapPointWriter::new(
        self.config.clone(),
        size,
      )))
    } else {
      Ok(PointWriterEnum::Offline(OfflinePointWriter::new(
        self.config.clone(),
        temp_dir,
        &self.temp_file_name_prefix,
        desc,
        count,
      )?))
    }
  }
}
/// Sliced reference to points in an PointWriter.
pub struct PathSlice<'a, O>
where
  O: IndexOutput,
{
  pub writer: &'a mut PointWriterEnum<O>,
  pub start: usize,
  pub count: usize,
}
impl<'a, O> PathSlice<'a, O>
where
  O: IndexOutput,
{
  pub fn new(writer: &'a mut PointWriterEnum<O>, start: usize, count: usize) -> Self {
    PathSlice {
      writer,
      start,
      count,
    }
  }
}

struct MSBRadixSorterImpl<'a, O>
where
  O: IndexOutput,
{
  points: &'a mut PointWriterEnum<O>,
  dim_cmp_bytes: usize,
  dim_offset: usize,
  data_offset: usize,
  common_prefix_length: usize,
  dim: usize,
  bytes_per_dim: usize,
  bytes_sorted: usize,
}

impl<O> Sorter for MSBRadixSorterImpl<'_, O>
where
  O: IndexOutput,
{
  fn swap(&mut self, i: usize, j: usize) -> Result<()> {
    match self.points {
      PointWriterEnum::Heap(heap_writer) => heap_writer.swap(i, j),
      _ => Err(LuceneError::illegal_state("points is not HeapPointWriter")),
    }
  }
}

impl<O> MSBRadixSorterBase for MSBRadixSorterImpl<'_, O>
where
  O: IndexOutput,
{
  fn byte_at(&mut self, i: usize, k: usize) -> Result<i32> {
    let pos = if k < self.dim_cmp_bytes {
      self.dim_offset + k
    } else {
      self.data_offset + k
    };
    match self.points {
      PointWriterEnum::Heap(heap_writer) => heap_writer.byte_at(i, pos),
      _ => Err(LuceneError::illegal_state("points is not HeapPointWriter")),
    }
  }

  fn get_fallback_sorter(&mut self, k: usize, _length: usize) -> impl Sorter
  where
    Self: Sized,
  {
    let skyped_bytes = k + self.common_prefix_length;
    let dim_start = self.dim * self.bytes_per_dim;
    IntroSorterImpl {
      points: self.points,
      skyped_bytes,
      dim_start,
      scratch: vec![0u8; self.bytes_sorted],
      bytes_per_dim: self.bytes_per_dim,
    }
  }
}

struct IntroSorterImpl<'a, O>
where
  O: IndexOutput,
{
  points: &'a mut PointWriterEnum<O>,
  skyped_bytes: usize,
  dim_start: usize,
  scratch: Vec<u8>,
  bytes_per_dim: usize,
}

impl<O> Sorter for IntroSorterImpl<'_, O>
where
  O: IndexOutput,
{
  fn compare(&mut self, i: usize, j: usize) -> Result<i32> {
    match self.points {
      PointWriterEnum::Heap(heap_writer) => {
        if self.skyped_bytes < self.bytes_per_dim {
          let cmp = heap_writer.compare_dim(i, j, self.dim_start)?;
          if cmp != 0 {
            return Ok(cmp);
          }
        }
        heap_writer.compare_data_dims_and_doc(i, j)
      },
      _ => Err(LuceneError::illegal_state("points is not HeapPointWriter")),
    }
  }

  fn swap(&mut self, i: usize, j: usize) -> Result<()> {
    match self.points {
      PointWriterEnum::Heap(heap_writer) => heap_writer.swap(i, j),
      _ => Err(LuceneError::illegal_state("points is not HeapPointWriter")),
    }
  }

  fn set_pivot(&mut self, i: usize) -> Result<()> {
    match self.points {
      PointWriterEnum::Heap(heap_writer) => {
        if self.skyped_bytes < self.bytes_per_dim {
          heap_writer.copy_dim(i, self.dim_start, &mut self.scratch, 0)?;
        }
        heap_writer.copy_data_dims_and_doc(i, &mut self.scratch, self.bytes_per_dim)
      },
      _ => Err(LuceneError::illegal_state("points is not HeapPointWriter")),
    }
  }

  fn compare_pivot(&mut self, j: usize) -> Result<i32> {
    match self.points {
      PointWriterEnum::Heap(heap_writer) => {
        if self.skyped_bytes < self.bytes_per_dim {
          let cmp = heap_writer.compare_dim_with_scratch(j, &self.scratch, 0, self.dim_start)?;
          if cmp != 0 {
            return Ok(cmp);
          }
        }
        heap_writer.compare_data_dims_and_doc_with(j, &self.scratch, self.bytes_per_dim)
      },
      _ => Err(LuceneError::illegal_state("points is not HeapPointWriter")),
    }
  }
  fn sort(&mut self, from: usize, to: usize) -> Result<()> {
    IntroSorter::sort_range(self, from, to)?;
    Ok(())
  }
}

impl<O> IntroSorter for IntroSorterImpl<'_, O> where O: IndexOutput {}

struct RadixSelectorImpl<'a, O>
where
  O: IndexOutput,
{
  points: &'a mut PointWriterEnum<O>,
  common_prefix_length: usize,
  bytes_per_dim: usize,
  dim_cmp_bytes: usize,
  dim_offset: usize,
  data_offset: usize,
  dim: usize,
  bytes_sorted: usize,
}

impl<O> Selector for RadixSelectorImpl<'_, O>
where
  O: IndexOutput,
{
  fn swap(&mut self, i: usize, j: usize) -> Result<()> {
    match self.points {
      PointWriterEnum::Heap(heap_writer) => heap_writer.swap(i, j),
      _ => Err(LuceneError::illegal_state("points is not HeapPointWriter")),
    }
  }
}

impl<O> RadixSelectorBase for RadixSelectorImpl<'_, O>
where
  O: IndexOutput,
{
  fn byte_at(&mut self, i: usize, k: usize) -> Result<i32> {
    let pos = if k < self.dim_cmp_bytes {
      self.dim_offset + k
    } else {
      self.data_offset + k
    };
    match self.points {
      PointWriterEnum::Heap(heap_writer) => heap_writer.byte_at(i, pos),
      _ => Err(LuceneError::illegal_state("points is not HeapPointWriter")),
    }
  }

  fn get_fallback_selector(&mut self, d: usize, _max_length: usize) -> impl Selector
  where
    Self: Sized,
  {
    let skyped_bytes = d + self.common_prefix_length;
    let dim_start = self.dim * self.bytes_per_dim;
    let sub_selector = IntroSelectorImpl {
      points: self.points,
      skyped_bytes,
      bytes_per_dim: self.bytes_per_dim,
      dim_start,
      scratch: vec![0u8; self.bytes_sorted],
    };
    IntroSelector::new(sub_selector)
  }
}

struct IntroSelectorImpl<'a, O>
where
  O: IndexOutput,
{
  points: &'a mut PointWriterEnum<O>,
  skyped_bytes: usize,
  bytes_per_dim: usize,
  dim_start: usize,
  scratch: Vec<u8>,
}

impl<O> IntroSelectorBaseDefault for IntroSelectorImpl<'_, O>
where
  O: IndexOutput,
{
  fn set_pivot(&mut self, i: usize) -> Result<()> {
    match self.points {
      PointWriterEnum::Heap(heap_writer) => {
        if self.skyped_bytes < self.bytes_per_dim {
          heap_writer.copy_dim(i, self.dim_start, &mut self.scratch, 0)?;
        }
        heap_writer.copy_data_dims_and_doc(i, &mut self.scratch, self.bytes_per_dim)
      },
      _ => Err(LuceneError::illegal_state("points is not HeapPointWriter")),
    }
  }

  fn compare_pivot(&mut self, j: usize) -> Result<i32> {
    match self.points {
      PointWriterEnum::Heap(heap_writer) => {
        if self.skyped_bytes < self.bytes_per_dim {
          let cmp = heap_writer.compare_dim_with_scratch(j, &self.scratch, 0, self.dim_start)?;
          if cmp != 0 {
            return Ok(cmp);
          }
        }
        heap_writer.compare_data_dims_and_doc_with(j, &self.scratch, self.bytes_per_dim)
      },
      _ => Err(LuceneError::illegal_state("points is not HeapPointWriter")),
    }
  }
}

impl<O> Selector for IntroSelectorImpl<'_, O>
where
  O: IndexOutput,
{
  fn swap(&mut self, i: usize, j: usize) -> Result<()> {
    match self.points {
      PointWriterEnum::Heap(heap_writer) => heap_writer.swap(i, j),
      _ => Err(LuceneError::illegal_state("points is not HeapPointWriter")),
    }
  }
}

impl<O> IntroSelectorBase for IntroSelectorImpl<'_, O>
where
  O: IndexOutput,
{
  fn compare(&mut self, i: usize, j: usize) -> Result<i32> {
    match self.points {
      PointWriterEnum::Heap(heap_writer) => {
        if self.skyped_bytes < self.bytes_per_dim {
          let cmp = heap_writer.compare_dim(i, j, self.dim_start)?;
          if cmp != 0 {
            return Ok(cmp);
          }
        }
        heap_writer.compare_data_dims_and_doc(i, j)
      },
      _ => Err(LuceneError::illegal_state("points is not HeapPointWriter")),
    }
  }
}
