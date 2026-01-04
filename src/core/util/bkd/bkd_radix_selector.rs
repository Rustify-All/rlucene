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
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::intro_sorter::IntroSorter;
use crate::core::util::radix_selector::{RadixSelector, RadixSelectorBase};
use crate::core::util::selector::Selector;
use crate::core::util::{
    CoreHelper, IntroSelector, IntroSelectorBase, IntroSelectorBaseDefault, MSBRadixSorter,
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
pub struct SelectorSlice<D>
where
    D: Directory,
{
    pub(crate) partition: Vec<u8>,

    pub(crate) left_writer: Option<PointWriterEnum<D::IndexOutput>>,
    pub(crate) left_from: usize,
    pub(crate) left_to: usize,

    pub(crate) right_writer: Option<PointWriterEnum<D::IndexOutput>>,
    pub(crate) right_from: usize,
    pub(crate) right_to: usize,
}
impl<D> SelectorSlice<D>
where
    D: Directory,
{
    fn new(
        partition: Vec<u8>,
        left_writer: Option<PointWriterEnum<D::IndexOutput>>,
        left_from: usize,
        left_to: usize,

        right_writer: Option<PointWriterEnum<D::IndexOutput>>,
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
    /// Sole constructor.
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
    pub fn select<D: Directory>(
        &mut self,
        points: &mut PathSlice<D::IndexOutput>,
        from: usize,
        to: usize,
        partition_point: usize,
        dim: usize,
        dim_common_prefix: usize,
        temp_dir: &D,
    ) -> Result<SelectorSlice<D>> {
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
            let mut right_writer =
                self.get_point_writer(to - partition_point, &format!("right{dim}"), temp_dir)?;
            if let PointWriterEnum::Offline(offline_point_writer) = points.writer {
                let partition = self.build_histogram_and_partition(
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
                )?;
                left_writer.close();
                right_writer.close();

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

    fn find_common_prefix_and_histogram<D: Directory>(
        &mut self,
        points: &mut OfflinePointWriter<D::IndexOutput>,
        from: usize,
        to: usize,
        dim: usize,
        dim_common_prefix: usize,
        temp_dir: &D,
    ) -> Result<usize> {
        let mut common_prefix_position = self.bytes_sorted;
        let offset = dim * self.config.bytes_per_dim;
        let mut reader = points.get_reader_with_buffer(
            from,
            to - from,
            std::mem::take(&mut self.offline_buffer),
            temp_dir,
        )?;
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
            self.scratch
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
                            self.histogram[self.scratch[common_prefix_position] as usize] =
                                i - from;
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
        match &mut reader.point_value {
            PointValueEnum::Offline(offline_point_value) => {
                self.offline_buffer = std::mem::take(&mut offline_point_value.value);
            },
            _ => {
                debug_assert!(false, "PointValueEnum must be Offline");
            },
        }
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
            let (packed_value, packed_value_offset, _length) =
                point_value.packed_value_doc_id_bytes();
            let index = packed_value_offset
                + self.config.packed_index_bytes_length()
                + common_prefix_position
                - self.config.bytes_per_dim;
            packed_value[index] as i32
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn build_histogram_and_partition<D: Directory>(
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
    ) -> Result<Vec<u8>> {
        // Find common prefix from baseCommonPrefix and build histogram
        let common_prefix = self.find_common_prefix_and_histogram(
            points,
            from,
            to,
            dim,
            base_common_prefix,
            temp_dir,
        )?;
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
        let mut delta_points =
            self.get_delta_point_writer(left, right, delta, iteration, temp_dir)?;
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
        )?;
        delta_points.close();
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
    fn offline_partition<D: Directory>(
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
    ) -> Result<()> {
        debug_assert!(byte_position == self.bytes_sorted - 1 || delta_points.is_some());
        let offset = dim * self.config.bytes_per_dim;
        let mut tiebreak_counter = 0usize;
        let mut reader = points.get_reader_with_buffer(
            from,
            to - from,
            std::mem::take(&mut self.offline_buffer),
            temp_dir,
        )?;
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
        match &mut reader.point_value {
            PointValueEnum::Offline(offline_point_value) => {
                self.offline_buffer = std::mem::take(&mut offline_point_value.value);
            },
            _ => {
                debug_assert!(false, "PointValueEnum must be Offline");
            },
        }
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
    fn heap_partition<O: IndexOutput>(
        &self,
        mut points: PointWriterEnum<O>,
        left: &mut PointWriterEnum<O>,
        right: &mut PointWriterEnum<O>,
        dim: usize,
        from: usize,
        to: usize,
        partition_point: usize,
        common_prefix: usize,
    ) -> Result<Vec<u8>> {
        let partition =
            self.heap_radix_select(&mut points, dim, from, to, partition_point, common_prefix)?;
        match points {
            PointWriterEnum::Heap(ref mut heap_writer) => {
                for i in from..to {
                    let value = heap_writer.get_packed_value_slice(i);
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
    pub fn heap_radix_select<O: IndexOutput>(
        &self,
        points: &mut PointWriterEnum<O>,
        dim: usize,
        from: usize,
        to: usize,
        partition_point: usize,
        common_prefix_length: usize,
    ) -> Result<Vec<u8>> {
        let bytes_per_dim = self.config.bytes_per_dim;
        let dim_offset = dim * bytes_per_dim + common_prefix_length;
        let dim_cmp_bytes = bytes_per_dim - common_prefix_length;
        let data_offset = self.config.packed_index_bytes_length() - dim_cmp_bytes;
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
        let mut radix_selector = RadixSelector::new(
            (self.bytes_sorted - common_prefix_length).try_into()?,
            sub_selector,
        );
        radix_selector.select(
            from.try_into()?,
            to.try_into()?,
            partition_point.try_into()?,
        )?;

        let mut partition = vec![0u8; bytes_per_dim];

        match points {
            PointWriterEnum::Heap(heap_writer) => {
                let point_value = heap_writer.get_packed_value_slice(partition_point);
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
    pub fn heap_radix_sort<O: IndexOutput>(
        &self,
        points: &mut PointWriterEnum<O>,
        from: usize,
        to: usize,
        dim: usize,
        common_prefix_length: usize,
    ) -> Result<()> {
        let bytes_per_dim = self.config.bytes_per_dim;
        let dim_offset = dim * bytes_per_dim + common_prefix_length;
        let dim_cmp_bytes = bytes_per_dim - common_prefix_length;
        let data_offset = self.config.packed_index_bytes_length() - dim_cmp_bytes;
        let max_length = self.bytes_sorted - common_prefix_length;
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
        let mut msb_radix_sorter = MSBRadixSorter::new(max_length.try_into()?, delegate);
        msb_radix_sorter.sort(from.try_into()?, to.try_into()?)
    }

    fn get_delta_point_writer<D: Directory>(
        &mut self,
        left: &mut PointWriterEnum<D::IndexOutput>,
        right: &mut PointWriterEnum<D::IndexOutput>,
        delta: usize,
        iteration: usize,
        temp_dir: &D,
    ) -> Result<PointWriterEnum<D::IndexOutput>> {
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

    fn get_max_points_sort_in_heap<O: IndexOutput>(
        &self,
        left: &mut PointWriterEnum<O>,
        right: &mut PointWriterEnum<O>,
    ) -> usize {
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

    fn get_point_writer<D: Directory>(
        &self,
        count: usize,
        desc: &str,
        temp_dir: &D,
    ) -> Result<PointWriterEnum<D::IndexOutput>> {
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
    fn byte_at(&mut self, i: i32, k: i32) -> Result<i32> {
        let i = i as usize;
        debug_assert!(k >= 0, "negative prefix {k}");
        let k = k as usize;
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

    fn get_fallback_sorter(&mut self, k: i32, _length: i32) -> impl Sorter
    where
        Self: Sized,
    {
        let k = k as usize;
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

    fn set_pivot(&mut self, i: i32) -> Result<()> {
        let i = i as usize;
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

    fn compare_pivot(&mut self, j: i32) -> Result<i32> {
        let j = j as usize;
        match self.points {
            PointWriterEnum::Heap(heap_writer) => {
                if self.skyped_bytes < self.bytes_per_dim {
                    let cmp = heap_writer.compare_dim_with_scratch(
                        j,
                        &self.scratch,
                        0,
                        self.dim_start,
                    )?;
                    if cmp != 0 {
                        return Ok(cmp);
                    }
                }
                heap_writer.compare_data_dims_and_doc_with(j, &self.scratch, self.bytes_per_dim)
            },
            _ => Err(LuceneError::illegal_state("points is not HeapPointWriter")),
        }
    }
    fn sort(&mut self, from: i32, to: i32) -> Result<()> {
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
    fn swap(&mut self, i: i32, j: i32) -> Result<()> {
        let i = i as usize;
        let j = j as usize;
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
    fn byte_at(&mut self, i: i32, k: i32) -> Result<i32> {
        debug_assert!(k >= 0, "negative prefix {k}");
        let i = i as usize;
        let k = k as usize;
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

    fn get_fallback_selector(&mut self, d: i32, _max_length: i32) -> impl Selector
    where
        Self: Sized,
    {
        let d = d as usize;
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
    fn set_pivot(&mut self, i: i32) -> Result<()> {
        let i = i as usize;
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

    fn compare_pivot(&mut self, j: i32) -> Result<i32> {
        let j = j as usize;
        match self.points {
            PointWriterEnum::Heap(heap_writer) => {
                if self.skyped_bytes < self.bytes_per_dim {
                    let cmp = heap_writer.compare_dim_with_scratch(
                        j,
                        &self.scratch,
                        0,
                        self.dim_start,
                    )?;
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
    fn swap(&mut self, i: i32, j: i32) -> Result<()> {
        let i = i as usize;
        let j = j as usize;
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
    fn compare(&mut self, i: i32, j: i32) -> Result<i32> {
        let i = i as usize;
        let j = j as usize;
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

#[cfg(test)]
mod tests {
    mod test_bkd_radix_selector {

        use std::cmp::Ordering::{Greater, Less};

        use rand::Rng;

        use crate::core::store::directory::Directory;

        use crate::core::util::bit_util::BitUtil;
        use crate::core::util::bkd::bkd_config::BKDConfig;
        use crate::core::util::bkd::bkd_radix_selector::{BKDRadixSelector, PathSlice};
        use crate::core::util::bkd::heap_point_write::HeapPointWriter;
        use crate::core::util::bkd::offline_point_write::OfflinePointWriter;
        use crate::core::util::bkd::point_reader::PointReader;
        use crate::core::util::bkd::point_value::PointValue;
        use crate::core::util::bkd::point_writer::{PointWriter, PointWriterEnum};
        use crate::core::util::error::lucene_error::Result;
        use crate::core::util::numeric_utils::NumericUtils;
        use crate::core::util::{CoreHelper, SliceCopyOps, ToInt};
        use crate::test::util::lucene_test_case::lucene_test_case_util::{
            at_least, new_directory, random,
        };
        use crate::test::util::test_util::TestUtil;

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
            points.close();
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

        #[test]
        #[ignore]
        fn test_random_binary_big() -> Result<()> {
            let mut random = random();
            do_test_random_binary(&mut random, 500000)
        }
        fn do_test_random_binary<R: Rng + ?Sized>(random: &mut R, count: i32) -> Result<()> {
            let config = get_random_config(random)?;
            let packed_bytes_length = config.packed_bytes_length();
            let values = TestUtil::next_int(random, count, count * 2) as usize;
            let dir = new_directory(random)?;
            let (start, end) = if random.random_bool(0.5) {
                (0, values)
            } else {
                let start = TestUtil::next_int(random, 0, values as i32 - 3) as usize;
                let end = TestUtil::next_int(random, start as i32 + 2, values as i32) as usize;
                (start, end)
            };
            let partition_point =
                TestUtil::next_int(random, start as i32 + 1, end as i32 - 1) as usize;
            let sorted_on_heap = random.random_range(0..5000);
            let mut points = get_random_point_writer(random, config.clone(), &dir, values)?;
            let mut value = vec![0u8; packed_bytes_length as usize];
            for i in 0..values {
                random.fill(&mut value[..]);
                points.append_bytes(&value, i as i32)?;
            }
            points.close();
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
            let dimensions =
                TestUtil::next_int(&mut random, 1, BKDConfig::MAX_INDEX_DIMS as i32) as usize;
            let bytes_per_dimensions = TestUtil::next_int(&mut random, 2, 30) as usize;
            let config = BKDConfig::new(
                dimensions,
                dimensions,
                bytes_per_dimensions,
                BKDConfig::DEFAULT_MAX_POINTS_IN_LEAF_NODE,
            )?;
            let values = TestUtil::next_int(&mut random, 15000, 20000) as usize;
            let dir = new_directory(&mut random)?;
            let partition_point = random.random_range(0..values);
            let sorted_on_heap = random.random_range(0..5000);
            let mut points = get_random_point_writer(&mut random, config.clone(), &dir, values)?;
            let mut value = vec![0u8; config.packed_bytes_length() as usize];
            random.fill(&mut value[..]);
            for i in 0..values {
                if random.random_bool(0.5) {
                    points.append_bytes(&value, i as i32)?;
                } else {
                    points.append_bytes(&value, random.random_range(0..values) as i32)?;
                }
            }
            points.close();
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
            let mut value = vec![0u8; config.packed_bytes_length() as usize];
            random.fill(&mut value[..]);
            for _ in 0..values {
                if random.random_bool(0.5) {
                    points.append_bytes(&value, 1)?;
                } else {
                    points.append_bytes(&value, 2)?;
                }
            }
            points.close();
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
            let values = random.random_range(1..=15000) as usize;
            let dir = new_directory(&mut random)?;
            let partition_point = random.random_range(0..values);
            let sorted_on_heap = random.random_range(0..5000);
            let config = get_random_config(&mut random)?;
            let mut points = get_random_point_writer(&mut random, config.clone(), &dir, values)?;
            let mut value = vec![0u8; config.packed_bytes_length() as usize];
            random.fill(&mut value[..]);
            for _ in 0..values {
                points.append_bytes(&value, 0)?;
            }
            points.close();
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
            let values = at_least(&mut random, 15000) as usize;
            let dir = new_directory(&mut random)?;
            let partition_point = random.random_range(0..values);
            let sorted_on_heap = random.random_range(0..5000);
            let mut points = get_random_point_writer(&mut random, config.clone(), &dir, values)?;
            let number_values = random.random_range(2..=9);
            let mut different_values = Vec::with_capacity(number_values as usize);
            for _ in 0..number_values {
                let mut buf = vec![0u8; config.packed_bytes_length() as usize];
                random.fill(&mut buf[..]);
                different_values.push(buf);
            }
            for i in 0..values {
                let idx = random.random_range(0..number_values) as usize;
                points.append_bytes(&different_values[idx], i as i32)?;
            }
            points.close();
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
            let values = at_least(&mut random, 15000) as usize;
            let dir = new_directory(&mut random)?;
            let partition_point = random.random_range(0..values);
            let sorted_on_heap = random.random_range(0..5000);
            let mut points = get_random_point_writer(&mut random, config.clone(), &dir, values)?;
            let mut value = vec![0u8; config.packed_bytes_length() as usize];
            let data_only_dims = config.num_dims - config.num_index_dims;
            let data_value_len = (data_only_dims * config.bytes_per_dim) as usize;
            let mut data_value = vec![0u8; data_value_len];
            random.fill(&mut value[..]);
            for i in 0..values {
                random.fill(&mut data_value[..]);
                let start = (config.num_index_dims * config.bytes_per_dim) as usize;
                value.copy_from(&data_value, start);
                points.append_bytes(&value, i as i32)?;
            }
            points.close();
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
        fn verify<D: Directory, R: Rng + ?Sized>(
            random: &mut R,
            config: BKDConfig,
            dir: &D,
            points: &mut PointWriterEnum<D::IndexOutput>,
            start: usize,
            end: usize,
            middle: usize,
            sorted_on_heap: usize,
        ) -> Result<()> {
            let mut radix_selector = BKDRadixSelector::new(config.clone(), sorted_on_heap, "test");
            let data_only_dims = config.num_dims - config.num_index_dims;

            for split_dim in 0..config.num_index_dims {
                let mut copy = copy_points(random, config.clone(), dir, points)?;
                let mut input_slice = PathSlice::new(&mut copy, 0, points.count());

                let common_prefix_length_input = get_random_common_prefix(
                    config.clone(),
                    &mut input_slice,
                    split_dim,
                    random,
                    dir,
                )?;

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
                        Some(ref mut left_writer) => PathSlice::new(
                            left_writer,
                            select_slice.left_from,
                            select_slice.left_to,
                        ),
                        None => PathSlice::new(
                            input_slice.writer,
                            select_slice.left_from,
                            select_slice.left_to,
                        ),
                    };
                    let max_data_dim = get_max_data_dimension(
                        config.clone(),
                        &mut left_slice,
                        &max,
                        split_dim,
                        dir,
                    )?;

                    let mut right_slice = match select_slice.right_writer {
                        Some(ref mut right_writer) => PathSlice::new(
                            right_writer,
                            select_slice.right_from,
                            select_slice.right_to,
                        ),
                        None => PathSlice::new(
                            input_slice.writer,
                            select_slice.right_from,
                            select_slice.right_to,
                        ),
                    };
                    let min_data_dim = get_min_data_dimension(
                        config.clone(),
                        &mut right_slice,
                        &min,
                        split_dim,
                        dir,
                    )?;
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
                            Some(ref mut left_writer) => PathSlice::new(
                                left_writer,
                                select_slice.left_from,
                                select_slice.left_to,
                            ),
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
                            Some(ref mut right_writer) => PathSlice::new(
                                right_writer,
                                select_slice.right_from,
                                select_slice.right_to,
                            ),
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
                    select_slice.partition, min,
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

        fn copy_points<D: Directory, R: Rng + ?Sized>(
            random: &mut R,
            config: BKDConfig,
            dir: &D,
            points: &mut PointWriterEnum<D::IndexOutput>,
        ) -> Result<PointWriterEnum<D::IndexOutput>> {
            let mut copy = get_random_point_writer(random, config, dir, points.count())?;
            let count = points.count();
            let mut reader = points.get_reader(0, count, dir)?;
            while reader.next()? {
                let point_value_ref = reader.point_value()?;
                copy.append_point_value(point_value_ref)?
            }
            points.take_data(reader.remove_points());
            copy.close();
            Ok(copy)
        }

        /// returns a common prefix length equal or lower than the current one.
        fn get_random_common_prefix<D: Directory, R: Rng + ?Sized>(
            config: BKDConfig,
            input_slice: &mut PathSlice<D::IndexOutput>,
            split_dim: usize,
            random: &mut R,
            dir: &D,
        ) -> Result<usize> {
            let points_max = get_max(config.clone(), input_slice, split_dim, dir)?;
            let points_min = get_min(config.clone(), input_slice, split_dim, dir)?;
            let mut common_prefix_length = CoreHelper::miss_match(
                &points_max[0..config.bytes_per_dim],
                &points_min[0..config.bytes_per_dim],
            );
            if common_prefix_length == -1 {
                common_prefix_length = config.bytes_per_dim as i32;
            }

            if random.random_bool(0.5) {
                Ok(common_prefix_length as usize)
            } else if common_prefix_length == 0 {
                Ok(0)
            } else {
                Ok(random.random_range(0..common_prefix_length) as usize)
            }
        }

        fn get_random_point_writer<D: Directory, R: Rng + ?Sized>(
            random: &mut R,
            config: BKDConfig,
            dir: &D,
            num_points: usize,
        ) -> Result<PointWriterEnum<D::IndexOutput>> {
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

        fn get_min<D: Directory>(
            config: BKDConfig,
            path_slice: &mut PathSlice<D::IndexOutput>,
            dimension: usize,
            dir: &D,
        ) -> Result<Vec<u8>> {
            let size = config.bytes_per_dim;
            let mut min = vec![0xffu8; size];
            let mut reader =
                path_slice
                    .writer
                    .get_reader(path_slice.start, path_slice.count, dir)?;
            let mut value = vec![0u8; size];
            while reader.next()? {
                let point_value = reader.point_value()?;
                let (value_ref, packed_value_offset, _) = point_value.packed_value();
                let start_idx = packed_value_offset + dimension * config.bytes_per_dim;
                let end_idx = start_idx + size;
                value.copy_from(&value_ref[start_idx..end_idx], 0);
                if min.cmp(&value) == Greater {
                    min.copy_from(&value, 0);
                }
            }
            path_slice.writer.take_data(reader.remove_points());
            Ok(min)
        }

        fn get_min_doc_id<D: Directory>(
            config: BKDConfig,
            p: &mut PathSlice<D::IndexOutput>,
            dimension: usize,
            partition_point: &[u8],
            data_dim: &[u8],
            dir: &D,
        ) -> Result<i32> {
            let mut doc_id = i32::MAX;
            let mut reader = p.writer.get_reader(p.start, p.count, dir)?;
            while reader.next()? {
                let point_value_ref = reader.point_value()?;
                let (bytes, packed_value_offset, _) = point_value_ref.packed_value();
                let offset = dimension * config.bytes_per_dim;
                let data_offset = config.packed_index_bytes_length();
                let data_length = (config.num_dims - config.num_index_dims) * config.bytes_per_dim;

                let slice1_equal1;
                let slice1_equal2;
                {
                    let dim_slice = &bytes[(packed_value_offset + offset)
                        ..(packed_value_offset + offset + config.bytes_per_dim)];
                    let partition_slice = &partition_point[0..config.bytes_per_dim];
                    let data_slice = &bytes[(packed_value_offset + data_offset)
                        ..(packed_value_offset + data_offset + data_length)];
                    let data_dim_slice = &data_dim[0..data_length];
                    slice1_equal1 = data_slice == partition_slice;
                    slice1_equal2 = dim_slice == data_dim_slice;
                }

                if slice1_equal1 && slice1_equal2 {
                    let new_doc_id = point_value_ref.doc_id();
                    if new_doc_id < doc_id {
                        doc_id = new_doc_id;
                    }
                }
            }
            p.writer.take_data(reader.remove_points());
            Ok(doc_id)
        }

        fn get_min_data_dimension<D: Directory>(
            config: BKDConfig,
            p: &mut PathSlice<D::IndexOutput>,
            min_dim: &[u8],
            split_dim: usize,
            dir: &D,
        ) -> Result<Vec<u8>> {
            let num_data_dims = config.num_dims - config.num_index_dims;
            let size = num_data_dims * config.bytes_per_dim;
            let mut min = vec![0xffu8; size];
            let offset = split_dim * config.bytes_per_dim;
            let mut reader = p.writer.get_reader(p.start, p.count, dir)?;
            let mut value = vec![0u8; size];
            while reader.next()? {
                let point_value_ref = reader.point_value()?;
                let (value_vec, packed_value_offset, _) = point_value_ref.packed_value();
                let start_idx = packed_value_offset + offset;
                let end_idx = packed_value_offset + offset + config.bytes_per_dim;
                let dim_slice = &value_vec[start_idx..end_idx];
                let min_dim_slice = &min_dim[0..config.bytes_per_dim];
                if min_dim_slice.cmp(dim_slice) == Less {
                    let copy_start =
                        packed_value_offset + config.num_index_dims * config.bytes_per_dim;
                    let copy_end = copy_start + size;
                    value.copy_from(&value_vec[copy_start..copy_end], 0);
                    if min_dim_slice.cmp(&value) == Greater {
                        min.copy_from(&value, 0);
                    }
                }
            }
            p.writer.take_data(reader.remove_points());
            Ok(min)
        }

        fn get_max<D: Directory>(
            config: BKDConfig,
            p: &mut PathSlice<D::IndexOutput>,
            dimension: usize,
            dir: &D,
        ) -> Result<Vec<u8>> {
            let size = config.bytes_per_dim;
            let mut max = vec![0u8; size];
            let mut reader = p.writer.get_reader(p.start, p.count, dir)?;
            let mut value = vec![0u8; size];
            while reader.next()? {
                let point_value_ref = reader.point_value()?;
                let (bytes_ref, packed_value_offset, _) = point_value_ref.packed_value();
                let start_idx = packed_value_offset + dimension * config.bytes_per_dim;
                let end_idx = start_idx + size;
                value.copy_from(&bytes_ref[start_idx..end_idx], 0);
                if max.cmp(&value) == Less {
                    max.copy_from(&value, 0);
                }
            }
            p.writer.take_data(reader.remove_points());
            Ok(max)
        }

        fn get_max_data_dimension<D: Directory>(
            config: BKDConfig,
            p: &mut PathSlice<D::IndexOutput>,
            max_dim: &[u8],
            split_dim: usize,
            dir: &D,
        ) -> Result<Vec<u8>> {
            let num_data_dims = config.num_dims - config.num_index_dims;
            let size = num_data_dims * config.bytes_per_dim;
            let mut max = vec![0u8; size];
            let offset = split_dim * config.bytes_per_dim;
            let mut reader = p.writer.get_reader(p.start, p.count, dir)?;
            let mut value = vec![0u8; size];
            while reader.next()? {
                let point_value_ref = reader.point_value()?;
                let (value_vec, packed_value_offset, _) = point_value_ref.packed_value();

                let start_idx = packed_value_offset + offset;
                let end_idx = start_idx + config.bytes_per_dim;
                let dim_slice = &value_vec[start_idx..end_idx];
                let max_dim_slice = &max_dim[0..config.bytes_per_dim];
                if max_dim_slice.cmp(dim_slice) == Less {
                    let copy_start = packed_value_offset + config.packed_index_bytes_length();
                    let copy_end = copy_start + size;
                    value.copy_from(&value_vec[copy_start..copy_end], 0);
                    if max.cmp(&value) == Less {
                        max.copy_from(&value, 0);
                    }
                }
            }
            p.writer.take_data(reader.remove_points());
            Ok(max)
        }

        fn get_max_doc_id<D: Directory>(
            config: BKDConfig,
            p: &mut PathSlice<D::IndexOutput>,
            dimension: usize,
            partition_point: &[u8],
            data_dim: &[u8],
            dir: &D,
        ) -> Result<i32> {
            let mut doc_id = i32::MIN;
            let mut reader = p.writer.get_reader(p.start, p.count, dir)?;
            while reader.next()? {
                let point_value_ref = reader.point_value()?;
                let (value, packed_value_offset, _) = point_value_ref.packed_value();
                let offset = dimension * config.bytes_per_dim;
                let data_offset = config.packed_index_bytes_length();
                let data_length = (config.num_dims - config.num_index_dims) * config.bytes_per_dim;
                let slice1_equal1;
                let slice1_equal2;
                {
                    let dim_slice = &value[(packed_value_offset + offset)
                        ..(packed_value_offset + offset + config.bytes_per_dim)];
                    let partition_slice = &partition_point[0..config.bytes_per_dim];

                    let data_slice = &value[(packed_value_offset + data_offset)
                        ..(packed_value_offset + data_offset + data_length)];
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
            p.writer.take_data(reader.remove_points());
            Ok(doc_id)
        }

        fn get_random_config<R: Rng + ?Sized>(random: &mut R) -> Result<BKDConfig> {
            let num_index_dims =
                TestUtil::next_int(random, 1, BKDConfig::MAX_INDEX_DIMS as i32) as usize;
            let num_dims =
                TestUtil::next_int(random, num_index_dims as i32, BKDConfig::MAX_DIMS as i32)
                    as usize;
            let bytes_per_dim = TestUtil::next_int(random, 2, 30) as usize;
            let max_points_in_leaf_node = TestUtil::next_int(random, 50, 2000) as usize;
            BKDConfig::new(
                num_dims,
                num_index_dims,
                bytes_per_dim,
                max_points_in_leaf_node,
            )
        }
    }
    mod test_bkd_radix_sort {

        use rand::Rng;

        use crate::core::store::IndexOutput;
        use crate::core::store::dummy::dummy_index_output::DummyIndexOutput;
        use crate::core::util::bkd::bkd_config::BKDConfig;
        use crate::core::util::bkd::bkd_radix_selector::BKDRadixSelector;
        use crate::core::util::bkd::heap_point_write::HeapPointWriter;
        use crate::core::util::bkd::point_value::PointValue;
        use crate::core::util::bkd::point_writer::{PointWriter, PointWriterEnum};
        use crate::core::util::error::lucene_error::Result;
        use crate::core::util::{CoreHelper, SliceCopyOps, ToInt};
        use crate::test::util::lucene_test_case::lucene_test_case_util::random;
        use crate::test::util::test_util::TestUtil;
        #[allow(dead_code)] // for quick search
        struct TestBKDRadixSort;
        #[test]
        fn test_random() -> Result<()> {
            let mut random = random();
            let config = get_random_config(&mut random)?;
            let num_points = TestUtil::next_int(
                &mut random,
                1,
                BKDConfig::DEFAULT_MAX_POINTS_IN_LEAF_NODE as i32,
            ) as usize;
            let mut heap_points = HeapPointWriter::new(config.clone(), num_points);
            let mut value = vec![0u8; config.packed_bytes_length() as usize];
            for i in 0..num_points {
                random.fill(&mut value[..]);
                heap_points.append_bytes(&value, i as i32)?;
            }
            heap_points.close();
            let mut points = PointWriterEnum::<DummyIndexOutput>::Heap(heap_points);
            verify_sort(&mut random, config, &mut points, 0, num_points)?;
            Ok(())
        }
        #[test]
        fn test_random_all_equals() -> Result<()> {
            let mut random = random();
            let config = get_random_config(&mut random)?;
            let num_points = TestUtil::next_int(
                &mut random,
                1,
                BKDConfig::DEFAULT_MAX_POINTS_IN_LEAF_NODE as i32,
            ) as usize;
            let mut heap_points = HeapPointWriter::new(config.clone(), num_points);
            let mut value = vec![0u8; config.packed_bytes_length() as usize];
            random.fill(&mut value[..]);
            for _ in 0..num_points {
                let doc_id = random.random_range(0..num_points);
                heap_points.append_bytes(&value, doc_id as i32)?;
            }
            heap_points.close();
            let mut points = PointWriterEnum::<DummyIndexOutput>::Heap(heap_points);
            verify_sort(&mut random, config, &mut points, 0, num_points)?;
            Ok(())
        }
        #[test]
        fn test_random_last_byte_two_values() -> Result<()> {
            let mut random = random();
            let config = get_random_config(&mut random)?;
            let num_points = TestUtil::next_int(
                &mut random,
                1,
                BKDConfig::DEFAULT_MAX_POINTS_IN_LEAF_NODE as i32,
            ) as usize;
            let mut heap_points = HeapPointWriter::new(config.clone(), num_points);
            let mut value = vec![0u8; config.packed_bytes_length() as usize];
            random.fill(&mut value[..]);
            for _ in 0..num_points {
                if random.random_bool(0.5) {
                    heap_points.append_bytes(&value, 1)?;
                } else {
                    heap_points.append_bytes(&value, 2)?;
                }
            }
            heap_points.close();
            let mut points = PointWriterEnum::<DummyIndexOutput>::Heap(heap_points);
            verify_sort(&mut random, config, &mut points, 0, num_points)?;
            Ok(())
        }

        #[test]
        fn test_random_few_different_values() -> Result<()> {
            let mut random = random();
            let config = get_random_config(&mut random)?;
            let num_points = TestUtil::next_int(
                &mut random,
                1,
                BKDConfig::DEFAULT_MAX_POINTS_IN_LEAF_NODE as i32,
            ) as usize;
            let mut heap_points = HeapPointWriter::new(config.clone(), num_points);
            let number_values = random.random_range(0..8) + 2; // [2, 9)
            let mut different_values: Vec<Vec<u8>> = Vec::with_capacity(number_values as usize);
            for _ in 0..number_values {
                let mut buf = vec![0u8; config.packed_bytes_length() as usize];
                random.fill(&mut buf[..]);
                different_values.push(buf);
            }
            for i in 0..num_points {
                let index = random.random_range(0..number_values);
                heap_points.append_bytes(&different_values[index as usize], i as i32)?;
            }
            heap_points.close();
            let mut points = PointWriterEnum::<DummyIndexOutput>::Heap(heap_points);
            verify_sort(&mut random, config, &mut points, 0, num_points)?;
            Ok(())
        }

        #[test]
        fn test_random_data_dim_different() -> Result<()> {
            let mut random = random();
            let config = get_random_config(&mut random)?;
            let num_points = TestUtil::next_int(
                &mut random,
                1,
                BKDConfig::DEFAULT_MAX_POINTS_IN_LEAF_NODE as i32,
            ) as usize;
            let mut heap_points = HeapPointWriter::new(config.clone(), num_points);
            let total_data_dimension = config.num_dims - config.num_index_dims;
            let data_dim_length = total_data_dimension * config.bytes_per_dim;
            let mut data_dimension_values = vec![0u8; data_dim_length as usize];
            let mut value = vec![0u8; config.packed_bytes_length() as usize];
            random.fill(&mut value[..]);
            for _ in 0..num_points {
                random.fill(&mut data_dimension_values[..]);
                let start = config.packed_index_bytes_length() as usize;
                value.copy_from(&data_dimension_values, start);
                let doc_id = random.random_range(0..num_points);
                heap_points.append_bytes(&value, doc_id as i32)?;
            }
            heap_points.close();
            let mut points = PointWriterEnum::<DummyIndexOutput>::Heap(heap_points);
            verify_sort(&mut random, config, &mut points, 0, num_points)?;
            Ok(())
        }

        fn verify_sort<O: IndexOutput, R: Rng + ?Sized>(
            random: &mut R,
            config: BKDConfig,
            points: &mut PointWriterEnum<O>,
            start: usize,
            end: usize,
        ) -> Result<()> {
            let radix_selector = BKDRadixSelector::new(config.clone(), 1000, "test");
            // we check for each dimension
            for split_dim in 0..config.num_dims {
                let common_prefix_length;
                {
                    common_prefix_length = get_random_common_prefix(
                        config.clone(),
                        points,
                        start,
                        end,
                        split_dim,
                        random,
                    );
                }

                radix_selector.heap_radix_sort(
                    points,
                    start,
                    end,
                    split_dim,
                    common_prefix_length,
                )?;

                let mut previous = vec![0u8; config.packed_bytes_length()];
                let mut previous_doc_id = -1;
                previous.fill(0);

                let dim_offset = split_dim * config.bytes_per_dim;

                match points {
                    PointWriterEnum::Heap(heap_writer) => {
                        for j in start..end {
                            let point_value = heap_writer.get_packed_value_slice(j);
                            let mut cmp;
                            let (bytes_ref, packed_value_offset, _) = point_value.packed_value();
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
                                previous.copy_from(
                                    &bytes_ref[packed_value_offset
                                        ..packed_value_offset + config.packed_bytes_length()],
                                    0,
                                );
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
        fn get_random_common_prefix<O: IndexOutput, R: Rng + ?Sized>(
            config: BKDConfig,
            points: &mut PointWriterEnum<O>,
            start: usize,
            end: usize,
            sort_dim: usize,
            random: &mut R,
        ) -> usize {
            match points {
                PointWriterEnum::Heap(heap_writer) => {
                    let mut common_prefix_length = config.bytes_per_dim;
                    let point_value = heap_writer.get_packed_value_slice(start);
                    let (bytes_ref, packed_value_offset, _length) = point_value.packed_value();
                    let mut first_value = vec![0u8; config.bytes_per_dim];
                    let offset = sort_dim * config.bytes_per_dim;
                    first_value.copy_from(
                        &bytes_ref[packed_value_offset + offset
                            ..packed_value_offset + offset + config.bytes_per_dim],
                        0,
                    );
                    for i in (start + 1)..end {
                        let point_value = heap_writer.get_packed_value_slice(i);
                        let (bytes_ref, packed_value_offset, _length) = point_value.packed_value();
                        let diff = CoreHelper::miss_match(
                            &bytes_ref[packed_value_offset + offset
                                ..packed_value_offset + offset + config.bytes_per_dim],
                            &first_value,
                        );
                        if diff != -1 && common_prefix_length > diff as usize {
                            if diff == 0 {
                                return diff as usize;
                            }
                            common_prefix_length = diff as usize;
                        }
                    }

                    if random.random_bool(0.5) {
                        common_prefix_length
                    } else {
                        random.random_range(0..common_prefix_length)
                    }
                },
                _ => {
                    unreachable!("should not be here");
                },
            }
        }

        fn get_random_config<R: Rng + ?Sized>(random: &mut R) -> Result<BKDConfig> {
            let num_index_dims =
                TestUtil::next_int(random, 1, BKDConfig::MAX_INDEX_DIMS as i32) as usize;
            let num_dims =
                TestUtil::next_int(random, num_index_dims as i32, BKDConfig::MAX_DIMS as i32)
                    as usize;
            let bytes_per_dim = TestUtil::next_int(random, 2, 30) as usize;
            let max_points_in_leaf_node = TestUtil::next_int(random, 50, 2000) as usize;
            BKDConfig::new(
                num_dims,
                num_index_dims,
                bytes_per_dim,
                max_points_in_leaf_node,
            )
        }
    }
}
