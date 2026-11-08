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
use crate::core::util::bkd::offline_point_reader::OfflinePointReader;
use crate::core::util::bkd::offline_point_write::OfflinePointWriter;
use crate::core::util::bkd::point_reader::PointReader;
use crate::core::util::bkd::point_value::PointValue;
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
use std::sync::Arc;

/// Offline Radix selector for BKD tree.
pub struct BKDRadixSelector<const N: usize = 0> {
  // histogram array
  histogram: [usize; 256],
  // Number of bytes to sort: `config.bytes_per_dim() + INT_BYTES`.
  bytes_sorted: usize,
  // flag to when we are moving to sort on heap
  max_points_sort_in_heap: usize,
  // reusable buffer
  offline_buffer: Vec<u8>,
  // holder for partition points
  partition_bucket: smallvec::SmallVec<[usize; 8]>,
  // scratch array to hold temporary data
  scratch: smallvec::SmallVec<[u8; 16]>,
  // prefix for temp files
  temp_file_name_prefix: Arc<str>,
  // BKD tree configuration
  config: BKDConfig,
}
pub struct SelectorSlice<O, const N: usize = 0>
where
  O: IndexOutput,
{
  pub(crate) partition: smallvec::SmallVec<[u8; 16]>,

  pub(crate) left_writer: Option<PointWriterEnum<O, N>>,
  pub(crate) left_from: usize,
  pub(crate) left_to: usize,

  pub(crate) right_writer: Option<PointWriterEnum<O, N>>,
  pub(crate) right_from: usize,
  pub(crate) right_to: usize,
}
impl<O, const N: usize> SelectorSlice<O, N>
where
  O: IndexOutput,
{
  fn new(
    partition: smallvec::SmallVec<[u8; 16]>,
    left_writer: Option<PointWriterEnum<O, N>>,
    left_from: usize,
    left_to: usize,

    right_writer: Option<PointWriterEnum<O, N>>,
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
#[cfg(test)]
impl BKDRadixSelector<0> {
  pub fn new<P: Into<Arc<str>>>(
    config: BKDConfig,
    max_points_sort_in_heap: usize,
    prefix: P,
  ) -> Self {
    Self::new_with_bytes(config, max_points_sort_in_heap, prefix)
  }
}
impl<const N: usize> BKDRadixSelector<N> {
  // size of the histogram
  const HISTOGRAM_SIZE: usize = 256;
  // size of the online buffer: 8 KB
  const MAX_SIZE_OFFLINE_BUFFER: usize = 1024 * 8;
  /// Creates a new instance.
  pub fn new_with_bytes<P: Into<Arc<str>>>(
    config: BKDConfig,
    max_points_sort_in_heap: usize,
    temp_file_name_prefix: P,
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
    let partition_bucket = smallvec::smallvec![0; bytes_sorted];
    let histogram = [0; 256];
    let scratch = smallvec::smallvec![0u8; bytes_sorted];
    BKDRadixSelector {
      config,
      max_points_sort_in_heap,
      temp_file_name_prefix: temp_file_name_prefix.into(),
      bytes_sorted,
      offline_buffer: Vec::new(),
      partition_bucket,
      histogram,
      scratch,
    }
  }

  fn take_offline_buffer(&mut self) -> Vec<u8> {
    if self.offline_buffer.is_empty() {
      let bytes_per_doc = self.config.bytes_per_doc();
      let capacity = (Self::MAX_SIZE_OFFLINE_BUFFER / bytes_per_doc) * bytes_per_doc;
      self.offline_buffer.resize(capacity, 0);
    }
    std::mem::take(&mut self.offline_buffer)
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
    points: &mut PathSlice<D::IndexOutput, N>,
    from: usize,
    to: usize,
    partition_point: usize,
    dim: usize,
    dim_common_prefix: usize,
    temp_dir: &D,
  ) -> Result<SelectorSlice<D::IndexOutput, N>>
  where
    D: Directory,
  {
    Self::check_args(from, to, partition_point)?;
    if let PointWriterEnum::Heap(heap_writer) = points.writer {
      let partition = self.heap_radix_select(
        heap_writer,
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
        self.get_point_writer(partition_point - from, format_args!("left{dim}"), temp_dir)?;
      let mut right_writer = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        self.get_point_writer(to - partition_point, format_args!("right{dim}"), temp_dir)
      })) {
        Ok(Ok(right_writer)) => right_writer,
        right_result => {
          let close_result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| left_writer.close()));
          IOUtils::use_or_suppress_caught_result(right_result, close_result)?;
          return Err(LuceneError::illegal_state(
            "right point writer creation unexpectedly succeeded after an error",
          ));
        },
      };
      if let PointWriterEnum::Offline(offline_point_writer) = points.writer {
        let partition_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
          let heap_sizes = (
            match &left_writer {
              PointWriterEnum::Heap(writer) => writer.size,
              PointWriterEnum::Offline(_) => 0,
            },
            match &right_writer {
              PointWriterEnum::Heap(writer) => writer.size,
              PointWriterEnum::Offline(_) => 0,
            },
          );
          let args = (
            heap_sizes,
            from,
            to,
            partition_point,
            0,
            dim_common_prefix,
            dim,
            temp_dir,
          );
          match (&mut left_writer, &mut right_writer) {
            (PointWriterEnum::Heap(left), PointWriterEnum::Heap(right)) => {
              self.build_histogram_and_partition(offline_point_writer, (left, right), args)
            },
            (PointWriterEnum::Heap(left), PointWriterEnum::Offline(right)) => {
              self.build_histogram_and_partition(offline_point_writer, (left, right), args)
            },
            (PointWriterEnum::Offline(left), PointWriterEnum::Heap(right)) => {
              self.build_histogram_and_partition(offline_point_writer, (left, right), args)
            },
            (PointWriterEnum::Offline(left), PointWriterEnum::Offline(right)) => {
              self.build_histogram_and_partition(offline_point_writer, (left, right), args)
            },
          }
        }));
        let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
          IOUtils::close([&mut right_writer, &mut left_writer])
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

  #[allow(clippy::too_many_arguments)]
  fn histogram_from_reader<R: PointReader>(
    &mut self,
    reader: &mut R,
    from: usize,
    to: usize,
    dim_common_prefix: usize,
    offset: usize,
    mut common_prefix_position: usize,
  ) -> Result<usize> {
    debug_assert!(common_prefix_position > dim_common_prefix);
    reader.next()?;
    {
      let point_value = reader.point_value()?;
      let value = point_value.packed_value_doc_id_bytes();

      let mut start = value.offset + offset;
      let mut end = start + self.config.bytes_per_dim;
      self.scratch.copy_from(&value.bytes[start..end], 0);

      start = value.offset + self.config.packed_index_bytes_length();
      end = start
        + ((self.config.num_dims - self.config.num_index_dims) * self.config.bytes_per_dim)
        + BitUtil::INT_BYTES;
      self
        .scratch
        .copy_from(&value.bytes[start..end], self.config.bytes_per_dim);
    }
    let mut histogram_index;
    for i in (from + 1)..to {
      reader.next()?;
      if common_prefix_position == dim_common_prefix {
        {
          let point_value = reader.point_value()?;
          histogram_index = self.get_bucket(offset, common_prefix_position, &point_value);
          self.histogram[histogram_index] += 1;
        }
        for _ in (i + 1)..to {
          reader.next()?;
          let point_value = reader.point_value()?;
          histogram_index = self.get_bucket(offset, common_prefix_position, &point_value);
          self.histogram[histogram_index] += 1;
        }
        break;
      } else {
        let point_value = reader.point_value()?;
        // Check common prefix and adjust histogram
        let scratch_start_index = std::cmp::min(dim_common_prefix, self.config.bytes_per_dim);
        let scratch_end_index = std::cmp::min(common_prefix_position, self.config.bytes_per_dim);
        let value = point_value.packed_value_doc_id_bytes();
        let packed_value_start_index = (value.offset + offset) + scratch_start_index;
        let packed_value_end_index = (value.offset + offset) + scratch_end_index;
        let j = CoreHelper::miss_match_u8(
          &self.scratch[scratch_start_index..scratch_end_index],
          &value.bytes[packed_value_start_index..packed_value_end_index],
        );
        if j == -1 {
          if common_prefix_position > self.config.bytes_per_dim {
            let start_tie_break = self.config.packed_index_bytes_length();
            let end_tie_break =
              start_tie_break + common_prefix_position - self.config.bytes_per_dim;
            let k = CoreHelper::miss_match_u8(
              &self.scratch[self.config.bytes_per_dim..common_prefix_position],
              &value.bytes[(value.offset + start_tie_break)..(value.offset + end_tie_break)],
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
          histogram_index = self.get_bucket(offset, common_prefix_position, &point_value);
          self.histogram[histogram_index] += 1;
        }
      }
    }
    Ok(common_prefix_position)
  }
  #[allow(clippy::too_many_arguments)]
  fn partition_from_reader<RD: PointReader, L: PointWriter, R: PointWriter, W: PointWriter>(
    &self,
    reader: &mut RD,
    left: &mut L,
    right: &mut R,
    mut delta_points: Option<&mut W>,
    offset: usize,
    byte_position: usize,
    num_docs_tiebreak: usize,
    mut tiebreak_counter: usize,
  ) -> Result<()> {
    while reader.next()? {
      let point_value = reader.point_value()?;
      let bucket = self.get_bucket(offset, byte_position, &point_value);
      if bucket < self.partition_bucket[byte_position] {
        left.append_point_value(&point_value)?;
      } else if bucket > self.partition_bucket[byte_position] {
        right.append_point_value(&point_value)?;
      } else if byte_position == self.bytes_sorted - 1 {
        if tiebreak_counter < num_docs_tiebreak {
          left.append_point_value(&point_value)?;
          tiebreak_counter += 1;
        } else {
          right.append_point_value(&point_value)?;
        }
      } else if let Some(dp) = delta_points.as_mut() {
        dp.append_point_value(&point_value)?;
      }
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
    let common_prefix_position = self.bytes_sorted;
    let offset = dim * self.config.bytes_per_dim;
    let mut reader =
      points.get_reader_with_buffer(from, to - from, self.take_offline_buffer(), temp_dir)?;
    let body_result =
      std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<usize> {
        match &mut reader {
          OfflinePointReader::Plain(reader) => self.histogram_from_reader(
            reader,
            from,
            to,
            dim_common_prefix,
            offset,
            common_prefix_position,
          ),
          OfflinePointReader::Checksum(reader) => self.histogram_from_reader(
            reader,
            from,
            to,
            dim_common_prefix,
            offset,
            common_prefix_position,
          ),
        }
      }));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<usize> {
      {
        self.offline_buffer = reader.take_buffer();
      }
      unwrap_caught_result!(body_result)
    }));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reader.close()));
    let common_prefix_position = IOUtils::use_or_suppress_caught_result(result, close_result)?;
    // Build partition buckets up to commonPrefix
    for i in 0..common_prefix_position {
      self.partition_bucket[i] = self.scratch[i] as usize;
    }
    Ok(common_prefix_position)
  }

  fn get_bucket<V: PointValue + ?Sized>(
    &self,
    offset: usize,
    common_prefix_position: usize,
    point_value: &V,
  ) -> usize {
    if common_prefix_position < self.config.bytes_per_dim {
      let packed_value = point_value.packed_value();
      let index = packed_value.offset + offset + common_prefix_position;
      packed_value.bytes[index] as usize
    } else {
      let packed_value = point_value.packed_value_doc_id_bytes();
      let index =
        packed_value.offset + self.config.packed_index_bytes_length() + common_prefix_position
          - self.config.bytes_per_dim;
      packed_value.bytes[index] as usize
    }
  }
  fn build_histogram_and_partition<D, L, R>(
    &mut self,
    points: &mut OfflinePointWriter<D::IndexOutput>,
    writers: (&mut L, &mut R),
    args: ((usize, usize), usize, usize, usize, usize, usize, usize, &D),
  ) -> Result<smallvec::SmallVec<[u8; 16]>>
  where
    D: Directory,
    L: PointWriter,
    R: PointWriter,
  {
    let (left, right) = writers;
    let (heap_sizes, from, to, partition_point, iteration, base_common_prefix, dim, temp_dir) =
      args;
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
        None::<&mut HeapPointWriter<N>>,
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
        self.partition_bucket[common_prefix] = i;
        break;
      }
      left_count += size;
    }
    // Count right points
    for i in (self.partition_bucket[common_prefix] + 1)..Self::HISTOGRAM_SIZE {
      right_count += self.histogram[i];
    }
    let delta = self.histogram[self.partition_bucket[common_prefix]];
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
        None::<&mut HeapPointWriter<N>>,
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
    let mut delta_points = self.get_delta_point_writer(heap_sizes, delta, iteration, temp_dir)?;
    let partition_result =
      std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &mut delta_points {
        PointWriterEnum::Heap(delta) => self.offline_partition(
          points,
          left,
          right,
          Some(delta),
          from,
          to,
          dim,
          common_prefix,
          0,
          temp_dir,
        ),
        PointWriterEnum::Offline(delta) => self.offline_partition(
          points,
          left,
          right,
          Some(delta),
          from,
          to,
          dim,
          common_prefix,
          0,
          temp_dir,
        ),
      }));
    let close_result =
      std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| delta_points.close()));
    IOUtils::use_or_suppress_caught_result(partition_result, close_result)?;
    let new_partition_point = partition_point - from - left_count;

    // Depending on the concrete type of delta_points, call the appropriate
    // partition method.
    let count = delta_points.count();
    match delta_points {
      PointWriterEnum::Heap(heap_writer) => self.heap_partition(
        heap_writer,
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
        (left, right),
        (
          heap_sizes,
          0,
          count,
          new_partition_point,
          iteration + 1,
          common_prefix + 1,
          dim,
          temp_dir,
        ),
      ),
    }
  }

  #[allow(clippy::too_many_arguments)]
  fn offline_partition<D, L, R, W>(
    &mut self,
    points: &mut OfflinePointWriter<D::IndexOutput>,
    left: &mut L,
    right: &mut R,
    delta_points: Option<&mut W>,
    from: usize,
    to: usize,
    dim: usize,
    byte_position: usize,
    num_docs_tiebreak: usize,
    temp_dir: &D,
  ) -> Result<()>
  where
    D: Directory,
    L: PointWriter,
    R: PointWriter,
    W: PointWriter,
  {
    debug_assert!(byte_position == self.bytes_sorted - 1 || delta_points.is_some());
    let offset = dim * self.config.bytes_per_dim;
    let tiebreak_counter = 0usize;
    let mut reader =
      points.get_reader_with_buffer(from, to - from, self.take_offline_buffer(), temp_dir)?;
    let body_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      match &mut reader {
        OfflinePointReader::Plain(reader) => self.partition_from_reader(
          reader,
          left,
          right,
          delta_points,
          offset,
          byte_position,
          num_docs_tiebreak,
          tiebreak_counter,
        ),
        OfflinePointReader::Checksum(reader) => self.partition_from_reader(
          reader,
          left,
          right,
          delta_points,
          offset,
          byte_position,
          num_docs_tiebreak,
          tiebreak_counter,
        ),
      }
    }));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      {
        self.offline_buffer = reader.take_buffer();
      }
      unwrap_caught_result!(body_result)
    }));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reader.close()));
    IOUtils::use_or_suppress_caught_result(result, close_result)?;
    // Delete original file
    points.destroy(temp_dir)?;
    Ok(())
  }

  fn partition_point_from_common_prefix(&self) -> Result<smallvec::SmallVec<[u8; 16]>> {
    Ok(
      self.partition_bucket[..self.config.bytes_per_dim]
        .iter()
        .map(|&byte| byte as u8)
        .collect(),
    )
  }

  #[allow(clippy::too_many_arguments)]
  fn heap_partition<L, R>(
    &mut self,
    mut points: HeapPointWriter<N>,
    left: &mut L,
    right: &mut R,
    dim: usize,
    from: usize,
    to: usize,
    partition_point: usize,
    common_prefix: usize,
  ) -> Result<smallvec::SmallVec<[u8; 16]>>
  where
    L: PointWriter,
    R: PointWriter,
  {
    let partition =
      self.heap_radix_select(&mut points, dim, from, to, partition_point, common_prefix)?;
    {
      for i in from..to {
        let value = points.get_packed_value_slice(i)?;
        if i < partition_point {
          left.append_point_value(value)?;
        } else {
          right.append_point_value(value)?;
        }
      }
      Ok(partition)
    }
  }
  /// Sort the heap writer by the specified dim. It is used to sort the leaves
  /// of the tree/`.
  pub fn heap_radix_select(
    &mut self,
    points: &mut HeapPointWriter<N>,
    dim: usize,
    from: usize,
    to: usize,
    partition_point: usize,
    common_prefix_length: usize,
  ) -> Result<smallvec::SmallVec<[u8; 16]>> {
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
      scratch: &mut self.scratch,
    };

    debug_assert!(self.bytes_sorted >= common_prefix_length);
    let mut radix_selector = RadixSelector::new(max_length, sub_selector);
    radix_selector.select(from, to, partition_point)?;

    let mut partition = smallvec::SmallVec::with_capacity(bytes_per_dim);

    {
      let point_value = points.get_packed_value_slice(partition_point)?;
      let bytes = point_value.packed_value();

      let start = bytes.offset + (dim * bytes_per_dim);
      let end = start + bytes_per_dim;

      partition.extend_from_slice(&bytes.bytes[start..end]);
      Ok(partition)
    }
  }

  /// Sort the heap writer by the specified dim. It is used to sort the leaves
  /// of the tree.
  pub fn heap_radix_sort(
    &mut self,
    points: &mut HeapPointWriter<N>,
    from: usize,
    to: usize,
    dim: usize,
    common_prefix_length: usize,
  ) -> Result<()> {
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
      scratch: &mut self.scratch,
    };
    let mut msb_radix_sorter = MSBRadixSorter::new(max_length, delegate);
    msb_radix_sorter.sort(from, to)
  }

  fn get_delta_point_writer<D>(
    &mut self,
    heap_sizes: (usize, usize),
    delta: usize,
    iteration: usize,
    temp_dir: &D,
  ) -> Result<PointWriterEnum<D::IndexOutput, N>>
  where
    D: Directory,
  {
    if delta >= i32::MAX as usize {
      return Err(LuceneError::number_overflow("Delta is too large"));
    }
    if delta <= self.get_max_points_sort_in_heap(heap_sizes) {
      Ok(PointWriterEnum::Heap(HeapPointWriter::<N>::new_with_bytes(
        self.config.clone(),
        delta,
      )))
    } else {
      Ok(PointWriterEnum::Offline(OfflinePointWriter::new(
        self.config.clone(),
        temp_dir,
        &self.temp_file_name_prefix,
        format_args!("delta{iteration}"),
        delta,
      )?))
    }
  }

  fn get_max_points_sort_in_heap(&self, heap_sizes: (usize, usize)) -> usize {
    let points_used = heap_sizes.0 + heap_sizes.1;
    debug_assert!(self.max_points_sort_in_heap >= points_used);
    self.max_points_sort_in_heap - points_used
  }

  fn get_point_writer<D>(
    &self,
    count: usize,
    desc: std::fmt::Arguments<'_>,
    temp_dir: &D,
  ) -> Result<PointWriterEnum<D::IndexOutput, N>>
  where
    D: Directory,
  {
    // As we recurse, we hold two on-heap point writers at any point.
    // Therefore the max size for these objects is half of the total
    // points we can have on-heap.
    if count <= self.max_points_sort_in_heap / 2 {
      let size = count;
      Ok(PointWriterEnum::Heap(HeapPointWriter::<N>::new_with_bytes(
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
pub struct PathSlice<'a, O, const N: usize = 0>
where
  O: IndexOutput,
{
  pub writer: &'a mut PointWriterEnum<O, N>,
  pub start: usize,
  pub count: usize,
}
impl<'a, O, const N: usize> PathSlice<'a, O, N>
where
  O: IndexOutput,
{
  pub fn new(writer: &'a mut PointWriterEnum<O, N>, start: usize, count: usize) -> Self {
    PathSlice {
      writer,
      start,
      count,
    }
  }
}

struct MSBRadixSorterImpl<'a, const N: usize> {
  points: &'a mut HeapPointWriter<N>,
  dim_cmp_bytes: usize,
  dim_offset: usize,
  data_offset: usize,
  common_prefix_length: usize,
  dim: usize,
  bytes_per_dim: usize,
  scratch: &'a mut [u8],
}

impl<const N: usize> Sorter for MSBRadixSorterImpl<'_, N> {
  fn swap(&mut self, i: usize, j: usize) -> Result<()> {
    self.points.swap(i, j)
  }
}

impl<const N: usize> MSBRadixSorterBase for MSBRadixSorterImpl<'_, N> {
  fn byte_at(&mut self, i: usize, k: usize) -> Result<i32> {
    let pos = if k < self.dim_cmp_bytes {
      self.dim_offset + k
    } else {
      self.data_offset + k
    };
    self.points.byte_at(i, pos)
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
      scratch: self.scratch,
      bytes_per_dim: self.bytes_per_dim,
    }
  }
}

struct IntroSorterImpl<'a, const N: usize> {
  points: &'a mut HeapPointWriter<N>,
  skyped_bytes: usize,
  dim_start: usize,
  scratch: &'a mut [u8],
  bytes_per_dim: usize,
}

impl<const N: usize> Sorter for IntroSorterImpl<'_, N> {
  fn compare(&mut self, i: usize, j: usize) -> Result<i32> {
    if self.skyped_bytes < self.bytes_per_dim {
      let cmp = self.points.compare_dim(i, j, self.dim_start)?;
      if cmp != 0 {
        return Ok(cmp);
      }
    }
    self.points.compare_data_dims_and_doc(i, j)
  }

  fn swap(&mut self, i: usize, j: usize) -> Result<()> {
    self.points.swap(i, j)
  }

  fn set_pivot(&mut self, i: usize) -> Result<()> {
    if self.skyped_bytes < self.bytes_per_dim {
      self.points.copy_dim(i, self.dim_start, self.scratch, 0)?;
    }
    self
      .points
      .copy_data_dims_and_doc(i, self.scratch, self.bytes_per_dim)
  }

  fn compare_pivot(&mut self, j: usize) -> Result<i32> {
    if self.skyped_bytes < self.bytes_per_dim {
      let cmp = self
        .points
        .compare_dim_with_scratch(j, self.scratch, 0, self.dim_start)?;
      if cmp != 0 {
        return Ok(cmp);
      }
    }
    self
      .points
      .compare_data_dims_and_doc_with(j, self.scratch, self.bytes_per_dim)
  }
  fn sort(&mut self, from: usize, to: usize) -> Result<()> {
    IntroSorter::sort_range(self, from, to)?;
    Ok(())
  }
}

impl<const N: usize> IntroSorter for IntroSorterImpl<'_, N> {}

struct RadixSelectorImpl<'a, const N: usize> {
  points: &'a mut HeapPointWriter<N>,
  common_prefix_length: usize,
  bytes_per_dim: usize,
  dim_cmp_bytes: usize,
  dim_offset: usize,
  data_offset: usize,
  dim: usize,
  scratch: &'a mut [u8],
}

impl<const N: usize> Selector for RadixSelectorImpl<'_, N> {
  fn swap(&mut self, i: usize, j: usize) -> Result<()> {
    self.points.swap(i, j)
  }
}

impl<const N: usize> RadixSelectorBase for RadixSelectorImpl<'_, N> {
  fn byte_at(&mut self, i: usize, k: usize) -> Result<i32> {
    let pos = if k < self.dim_cmp_bytes {
      self.dim_offset + k
    } else {
      self.data_offset + k
    };
    self.points.byte_at(i, pos)
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
      scratch: self.scratch,
    };
    IntroSelector::new(sub_selector)
  }
}

struct IntroSelectorImpl<'a, const N: usize> {
  points: &'a mut HeapPointWriter<N>,
  skyped_bytes: usize,
  bytes_per_dim: usize,
  dim_start: usize,
  scratch: &'a mut [u8],
}

impl<const N: usize> IntroSelectorBaseDefault for IntroSelectorImpl<'_, N> {
  fn set_pivot(&mut self, i: usize) -> Result<()> {
    {
      if self.skyped_bytes < self.bytes_per_dim {
        self.points.copy_dim(i, self.dim_start, self.scratch, 0)?;
      }
      self
        .points
        .copy_data_dims_and_doc(i, self.scratch, self.bytes_per_dim)
    }
  }

  fn compare_pivot(&mut self, j: usize) -> Result<i32> {
    if self.skyped_bytes < self.bytes_per_dim {
      let cmp = self
        .points
        .compare_dim_with_scratch(j, self.scratch, 0, self.dim_start)?;
      if cmp != 0 {
        return Ok(cmp);
      }
    }
    self
      .points
      .compare_data_dims_and_doc_with(j, self.scratch, self.bytes_per_dim)
  }
}

impl<const N: usize> Selector for IntroSelectorImpl<'_, N> {
  fn swap(&mut self, i: usize, j: usize) -> Result<()> {
    self.points.swap(i, j)
  }
}

impl<const N: usize> IntroSelectorBase for IntroSelectorImpl<'_, N> {
  fn compare(&mut self, i: usize, j: usize) -> Result<i32> {
    if self.skyped_bytes < self.bytes_per_dim {
      let cmp = self.points.compare_dim(i, j, self.dim_start)?;
      if cmp != 0 {
        return Ok(cmp);
      }
    }
    self.points.compare_data_dims_and_doc(i, j)
  }
}
