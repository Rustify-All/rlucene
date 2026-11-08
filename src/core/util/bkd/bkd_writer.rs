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
use crate::core::codecs::CodecUtil;
use crate::core::codecs::mutable_point_tree::MutablePointTree;
use crate::core::index::merge_state::DocMap;
use crate::core::index::point_values::{
  IntersectVisitor, MAX_NUM_BYTES, PointTree, PointValues, Relation,
};
use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::store::directory::Directory;
use crate::core::store::tracking_directory_wrapper::TrackingDirectoryWrapper;
use crate::core::store::{ByteBuffersDataOutput, DataOutput, IndexOutput};
use crate::core::util::array_util::{ArrayUtil, ByteArrayComparator};
use crate::core::util::bit_set::BitSet;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bkd::bkd_bytes::{BKDBytes, BKDEquals, BKDPrefix};
use crate::core::util::bkd::bkd_config::BKDConfig;
use crate::core::util::bkd::bkd_radix_selector::{BKDRadixSelector, PathSlice};
use crate::core::util::bkd::bkd_util::ByteArrayPredicate;
use crate::core::util::bkd::doc_ids_writer::DocIdsWriter;
use crate::core::util::bkd::heap_point_write::HeapPointWriter;
use crate::core::util::bkd::mutable_point_tree_reader_utils::MutablePointTreeReaderUtils;
use crate::core::util::bkd::offline_point_reader::OfflinePointReader;
use crate::core::util::bkd::offline_point_write::OfflinePointWriter;
use crate::core::util::bkd::point_reader::{PointReader, PointReaderEnum};
use crate::core::util::bkd::point_value::PointValue;
use crate::core::util::bkd::point_writer::{PointWriter, PointWriterEnum};
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{CaughtResult, LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::numeric_utils::NumericUtils;
use crate::core::util::priority_queue::{Compare, PriorityQueue};
use crate::core::util::{IOUtils, SliceCopyOps, ToInt, TryIntoInt};
use std::sync::Arc;

/// Recursively builds a block KD-tree to assign all incoming points in N-dim
/// space to smaller and smaller N-dim rectangles (cells) until the number of
/// points in a given rectangle is <= `config.max_points_in_leaf_node()`. The
/// tree is partially balanced, which means the leaf nodes will have the
/// requested `config.max_points_in_leaf_node()` except one that might have
/// less. Leaf nodes may straddle the two bottom levels of the binary tree.
/// Values that fall exactly on a cell boundary may be in either cell.
///
/// The number of dimensions can be 1 to 8, but every byte-slice value is fixed
/// length.
///
/// This consumes heap during writing: it allocates a `Vec<i64>` (`num_leaves`
/// size), a `Vec<u8>` (`num_leaves * (1 + config.bytes_per_dim)`) and then uses
/// up to the specified `max_mb_sort_in_heap` heap space for writing.
///
/// **NOTE**: This can write at most `i32::MAX * config.max_points_in_leaf_node
/// / config.bytes_per_dim` total points.
pub struct BKDWriter<D, const N: usize = 0>
where
  D: Directory,
{
  config: BKDConfig,
  comparator: BKDBytes<N>,
  common_prefix_comparator: BKDPrefix<N>,
  temp_dir: TrackingDirectoryWrapper<D>,
  temp_file_name_prefix: Arc<str>,

  #[allow(dead_code)]
  max_mb_sort_in_heap: f64,
  scratch_diff: smallvec::SmallVec<[u8; 16]>,
  scratch: smallvec::SmallVec<[u8; 32]>,
  scratch_bytes_ref1: BytesRef<Vec<u8>>,
  scratch_bytes_ref2: BytesRef<Vec<u8>>,
  common_prefix_lengths: smallvec::SmallVec<[usize; 4]>,
  used_bytes: smallvec::SmallVec<[Option<FixedBitSet>; 4]>,
  docs_seen: FixedBitSet,
  point_writer: Option<PointWriterEnum<<TrackingDirectoryWrapper<D> as Directory>::IndexOutput, N>>,
  finished: bool,
  max_points_sort_in_heap: usize,
  /// Minimum per-dim values, packed.
  min_packed_value: smallvec::SmallVec<[u8; 32]>,
  /// Maximum per-dim values, packed.
  max_packed_value: smallvec::SmallVec<[u8; 32]>,
  point_count: i64,
  /// An upper bound on how many points the caller will add (includes
  /// deletions).
  total_point_count: i64,
  equals_predicate: BKDEquals<N>,
  max_doc: i32,
  doc_ids_writer: DocIdsWriter,
}

#[cfg(test)]
impl<D: Directory> BKDWriter<D> {
  pub fn new(
    max_doc: i32,
    temp_dir: D,
    prefix: &str,
    config: BKDConfig,
    max_mb: f64,
    total: i64,
  ) -> Result<Self> {
    Self::new_with_bytes(max_doc, temp_dir, prefix, config, max_mb, total)
  }
}

impl<D, const N: usize> BKDWriter<D, N>
where
  D: Directory,
{
  pub fn new_with_bytes(
    max_doc: i32,
    temp_dir: D,
    temp_file_name_prefix: &str,
    config: BKDConfig,
    max_mb_sort_in_heap: f64,
    total_point_count: i64,
  ) -> Result<Self> {
    Self::verify_params(max_mb_sort_in_heap, total_point_count)?;

    let bytes_per_dim = config.bytes_per_dim;
    let packed_bytes_length = config.packed_bytes_length();
    let packed_index_bytes_length = config.packed_index_bytes_length();
    let temp_dir = TrackingDirectoryWrapper::new(temp_dir);
    let comparator = BKDBytes::new(bytes_per_dim);
    let equals_predicate = BKDEquals(BKDBytes::new(bytes_per_dim));
    let common_prefix_comparator = BKDPrefix(BKDBytes::new(bytes_per_dim));
    let docs_seen = FixedBitSet::new(max_doc as usize);

    let scratch_diff = smallvec::smallvec![0u8; config.bytes_per_dim];
    let scratch = smallvec::smallvec![0u8; packed_bytes_length];
    let common_prefix_lengths = smallvec::smallvec![0; config.num_dims];

    let min_packed_value = smallvec::smallvec![0u8; packed_index_bytes_length];
    let max_packed_value = smallvec::smallvec![0u8; packed_index_bytes_length];

    // Maximum number of points we hold in memory at any time
    let max_points_sort_in_heap =
      ((max_mb_sort_in_heap * 1024.0 * 1024.0) / config.bytes_per_doc() as f64) as usize;
    // Only the reader uses the packed document ID decoding buffer.
    let doc_ids_writer = DocIdsWriter::new(0);
    // Finally, we must be able to hold at least the leaf node in heap
    // during build:
    if max_points_sort_in_heap < config.max_points_in_leaf_node {
      return Err(LuceneError::illegal_argument(format!(
        "maxMBSortInHeap={} only allows for maxPointsSortInHeap={}, but this is less than maxPointsInLeafNode={}; \
                either increase maxMBSortInHeap or decrease maxPointsInLeafNode",
        max_mb_sort_in_heap, max_points_sort_in_heap, config.max_points_in_leaf_node
      )));
    }

    Ok(Self {
      config,
      comparator,
      equals_predicate,
      common_prefix_comparator,
      temp_dir,
      temp_file_name_prefix: Arc::from(temp_file_name_prefix),
      max_mb_sort_in_heap,
      scratch_diff,
      scratch,
      scratch_bytes_ref1: BytesRef::default(),
      scratch_bytes_ref2: BytesRef::default(),
      common_prefix_lengths,
      used_bytes: smallvec::SmallVec::new(),
      docs_seen,
      point_writer: None,
      finished: false,
      max_points_sort_in_heap,
      min_packed_value,
      max_packed_value,
      point_count: 0,
      total_point_count,
      max_doc,
      doc_ids_writer,
    })
  }
  fn verify_params(max_mb_sort_in_heap: f64, total_point_count: i64) -> Result<()> {
    if max_mb_sort_in_heap < 0.0 {
      return Err(LuceneError::illegal_argument(format!(
        "maxMBSortInHeap must be >= 0.0 (got: {max_mb_sort_in_heap})"
      )));
    }
    if total_point_count < 0 {
      return Err(LuceneError::illegal_argument(format!(
        "totalPointCount must be >= 0 (got: {total_point_count})"
      )));
    }
    Ok(())
  }
  fn init_point_writer(&mut self) -> Result<()> {
    debug_assert!(
      self.point_writer.is_none(),
      "Point writer is already initialized"
    );

    // Total point count is an estimation but the final point count must be
    // equal or lower to that number.
    if self.total_point_count > self.max_points_sort_in_heap as i64 {
      let writer = OfflinePointWriter::new(
        self.config.clone(),
        &self.temp_dir,
        &self.temp_file_name_prefix,
        "spill",
        0,
      )?;
      self.point_writer = Some(PointWriterEnum::Offline(writer));
    } else {
      self.point_writer = Some(PointWriterEnum::Heap(HeapPointWriter::<N>::new_with_bytes(
        self.config.clone(),
        self.total_point_count.try_convert()?,
      )));
    }

    Ok(())
  }
  pub fn add(&mut self, packed_value: &[u8], doc_id: i32) -> Result<()> {
    if packed_value.len() != self.config.packed_bytes_length() {
      return Err(LuceneError::illegal_argument(format!(
        "packedValue should be length={} (got: {})",
        self.config.packed_bytes_length(),
        packed_value.len()
      )));
    }

    if self.point_count >= self.total_point_count {
      return Err(LuceneError::illegal_state(format!(
        "totalPointCount={} was passed when we were created, but we just hit {} values",
        self.total_point_count,
        self.point_count + 1
      )));
    }

    if self.point_count == 0 {
      self.init_point_writer()?;
      let length = self.config.packed_index_bytes_length();
      self.min_packed_value.copy_from(&packed_value[..length], 0);
      self.max_packed_value.copy_from(&packed_value[..length], 0);
    } else {
      let bytes_per_dim = self.config.bytes_per_dim;
      for dim in 0..self.config.num_index_dims {
        let offset = dim * bytes_per_dim;
        if self
          .comparator
          .compare(packed_value, offset, &self.min_packed_value, offset)
          < 0
        {
          self
            .min_packed_value
            .copy_from(&packed_value[offset..offset + bytes_per_dim], offset);
        } else if self
          .comparator
          .compare(packed_value, offset, &self.max_packed_value, offset)
          > 0
        {
          self
            .max_packed_value
            .copy_from(&packed_value[offset..offset + bytes_per_dim], offset);
        }
      }
    }
    match self.point_writer.as_mut() {
      None => {
        return Err(LuceneError::illegal_state(
          "point writer is not initialized",
        ));
      },
      Some(ref mut writer) => {
        writer.append_bytes(packed_value, doc_id)?;
      },
    }
    self.point_count += 1;
    self.docs_seen.set(doc_id as usize)?;
    Ok(())
  }
  /// Write a field from a [`MutablePointTree`](crate::core::codecs::mutable_point_tree::MutablePointTree). This way of writing points is
  /// faster than regular writes with `BKDWriter::add` since there is
  /// opportunity for reordering points before writing them to disk. This
  /// method does not use transient disk in order to reorder points.
  pub fn write_field<M, IO>(
    &mut self,
    data_out: &mut IO,
    reader: &mut M,
    filename: &str,
  ) -> Result<Option<IORunnable>>
  where
    M: MutablePointTree,
    IO: IndexOutput,
  {
    if self.config.num_dims == 1 {
      self.write_field_1dim(data_out, filename, reader)
    } else {
      self.write_field_n_dims(data_out, reader)
    }
  }

  fn compute_packed_value_bounds_with_tree<M, const B: usize>(
    &mut self,
    values: &M,
    from: usize,
    to: usize,
    min_packed_value: &mut [u8],
    max_packed_value: &mut [u8],
  ) -> Result<()>
  where
    M: MutablePointTree,
  {
    if from == to {
      return Ok(());
    }
    let value = values.get_value(from, &mut self.scratch_bytes_ref1)?;
    min_packed_value.copy_from(
      &value.bytes[value.offset..value.offset + self.config.packed_index_bytes_length()],
      0,
    );
    max_packed_value.copy_from(
      &value.bytes[value.offset..value.offset + self.config.packed_index_bytes_length()],
      0,
    );

    let comparator = BKDBytes::<B>::new(self.config.bytes_per_dim);
    for i in from + 1..to {
      let value = values.get_value(i, &mut self.scratch_bytes_ref1)?;
      let offset = value.offset;
      for dim in 0..self.config.num_index_dims {
        let start_offset = dim * self.config.bytes_per_dim;
        let end_offset = start_offset + self.config.bytes_per_dim;

        if comparator.compare(
          &value.bytes[offset + start_offset..offset + end_offset],
          0,
          &min_packed_value[start_offset..end_offset],
          0,
        ) < 0
        {
          min_packed_value.copy_from(
            &value.bytes[offset + start_offset..offset + end_offset],
            start_offset,
          );
        } else if comparator.compare(
          &value.bytes[offset + start_offset..offset + end_offset],
          0,
          &max_packed_value[start_offset..end_offset],
          0,
        ) > 0
        {
          max_packed_value.copy_from(
            &value.bytes[offset + start_offset..offset + end_offset],
            start_offset,
          );
        }
      }
    }

    Ok(())
  }
  /// In the 2+D case, we recursively pick the split dimension, compute the
  /// median value and partition other values around it.
  pub fn write_field_n_dims<M, IO>(
    &mut self,
    data_out: &mut IO,
    values: &mut M,
  ) -> Result<Option<IORunnable>>
  where
    M: MutablePointTree,
    IO: IndexOutput,
  {
    if self.point_count != 0 {
      return Err(LuceneError::illegal_state("cannot mix add and write_field"));
    }

    // Catch user silliness:
    if self.finished {
      return Err(LuceneError::illegal_state("already finished"));
    }

    // Mark that we already finished:
    self.finished = true;

    let point_count = values.size()?;
    self.point_count = point_count.try_convert()?;

    if self.point_count == 0 {
      return Ok(None);
    }

    let num_leaves = point_count.div_ceil(self.config.max_points_in_leaf_node);
    let num_splits = num_leaves - 1;

    self.check_max_leaf_node_count(num_leaves)?;

    let mut split_packed_values = vec![0u8; num_splits * self.config.bytes_per_dim];
    let mut split_dimension_values = smallvec::smallvec![0u8; num_splits];
    let mut leaf_block_fps = vec![0usize; num_leaves];

    let mut min_packed_value = smallvec::smallvec![0u8; self.min_packed_value.len()];
    let mut max_packed_value = smallvec::smallvec![0u8; self.max_packed_value.len()];
    // MutablePointTree accesses are limited to signed i32 positions.
    let _: i32 = point_count.try_convert()?;
    // Compute the min/max for this slice
    match self.config.bytes_per_dim {
      4 => self.compute_packed_value_bounds_with_tree::<_, 4>(
        values,
        0,
        point_count,
        &mut min_packed_value,
        &mut max_packed_value,
      )?,
      8 => self.compute_packed_value_bounds_with_tree::<_, 8>(
        values,
        0,
        point_count,
        &mut min_packed_value,
        &mut max_packed_value,
      )?,
      _ => self.compute_packed_value_bounds_with_tree::<_, 0>(
        values,
        0,
        point_count,
        &mut min_packed_value,
        &mut max_packed_value,
      )?,
    }
    self.min_packed_value = min_packed_value;
    self.max_packed_value = max_packed_value;

    for i in 0..point_count {
      self.docs_seen.set(values.get_doc_id(i)? as usize)?;
    }

    let data_start_fp = data_out.get_file_pointer()?;
    stack_or_heap_buffer!(
      parent_splits,
      i32,
      self.config.num_index_dims,
      BKDConfig::MAX_INDEX_DIMS,
      0
    );

    stack_or_heap_buffer!(
      spare_doc_ids,
      i32,
      point_count.min(self.config.max_points_in_leaf_node),
      64,
      0
    );
    self.build_with_reader(
      0,
      num_leaves,
      values,
      0,
      point_count,
      data_out,
      smallvec::SmallVec::from_slice(&self.min_packed_value),
      smallvec::SmallVec::from_slice(&self.max_packed_value),
      parent_splits,
      &mut split_packed_values,
      &mut split_dimension_values,
      &mut leaf_block_fps,
      &mut vec![0; self.config.max_points_in_leaf_node],
    )?;

    debug_assert!(
      parent_splits.iter().all(|&x| x == 0),
      "parent_splits should be all zeros at the end"
    );

    let split_packed_values = BytesRef::from_bytes(split_packed_values);

    self.make_writer(
      split_packed_values,
      split_dimension_values,
      leaf_block_fps,
      data_start_fp,
    )
  }

  /// In the 1D case, we can simply sort points in ascending order and use the
  /// same writing logic as we use at merge time.
  fn write_field_1dim<M, O>(
    &mut self,
    data_out: &mut O,
    _field_name: &str,
    reader: &mut M,
  ) -> Result<Option<IORunnable>>
  where
    M: MutablePointTree,
    O: IndexOutput,
  {
    let size = reader.size()?;
    MutablePointTreeReaderUtils::sort(&self.config, self.max_doc, reader, 0, size)?;

    let one_dim_writer = OneDimensionBKDWriter::new(data_out, self)?;
    let mut intersect_visitor = IntersectVisitorImpl { one_dim_writer };
    reader.visit_doc_values(&mut intersect_visitor)?;
    intersect_visitor.one_dim_writer.finish()
  }
  /// More efficient bulk-add for incoming implementations of [`PointValues`](crate::core::index::point_values::PointValues).
  /// This does a merge sort of the already sorted values and currently only
  /// works when num_dims==1. This returns `None` if all documents
  /// containing dimensional values were deleted.
  pub fn merge<S, DM, IO>(
    &mut self,
    data_out: &mut IO,
    doc_maps: Option<Vec<DM>>,
    readers: Vec<S>,
  ) -> Result<Option<IORunnable>>
  where
    S: PointValues,
    DM: DocMap,
    IO: IndexOutput,
  {
    let readers_len = readers.len();
    debug_assert!(match doc_maps {
      None => true,
      Some(ref doc_maps) => doc_maps.len() == readers_len,
    });
    match doc_maps {
      Some(doc_maps) => self.merge_with_doc_map(data_out, readers, |i| {
        let doc_map = &doc_maps[i];
        move |doc_id| doc_map.get(doc_id)
      }),
      None => self.merge_with_doc_map(data_out, readers, |_| |doc_id| Ok(doc_id)),
    }
  }
  fn merge_with_doc_map<S, DM, IO, F>(
    &mut self,
    data_out: &mut IO,
    readers: Vec<S>,
    make_doc_map: F,
  ) -> Result<Option<IORunnable>>
  where
    S: PointValues,
    DM: Fn(i32) -> Result<i32>,
    IO: IndexOutput,
    F: Fn(usize) -> DM,
  {
    let readers_len = readers.len();
    let mut queue = PriorityQueue::new(
      readers_len,
      MergeReaderCmp::<N>::new(self.config.bytes_per_dim),
    )?;

    for (i, mut point_values) in readers.into_iter().enumerate() {
      debug_assert_eq!(point_values.get_num_dimensions()?, self.config.num_dims);
      debug_assert_eq!(
        point_values.get_bytes_per_dimension()?,
        self.config.bytes_per_dim
      );
      debug_assert_eq!(
        point_values.get_num_index_dimensions()?,
        self.config.num_index_dims
      );
      let doc_map = make_doc_map(i);
      let mut reader = MergeReader::new(&mut point_values, doc_map)?;
      if reader.next()? {
        queue.add(Box::new(reader))?;
      }
    }

    let mut one_dim_writer = OneDimensionBKDWriter::new(data_out, self)?;

    while queue.size() != 0 {
      let reader = queue
        .top_mut()
        .ok_or_else(|| LuceneError::illegal_state("no top available"))?;
      let start = reader.packed_value_offset;
      let end = start + reader.packed_bytes_length;
      one_dim_writer.add(
        &reader.merge_intersects_visitor.packed_values[start..end],
        reader.doc_id,
      )?;

      if reader.next()? {
        queue.update_top()?;
      } else {
        // This segment was exhausted
        queue.pop_unchecked()?;
      }
    }

    one_dim_writer.finish()
  }
  fn get_num_left_leaf_nodes(&self, num_leaves: usize) -> usize {
    debug_assert!(
      num_leaves > 1,
      "get_num_left_leaf_nodes() called with {num_leaves}"
    );
    // return the level that can be filled with this number of leaves
    let last_full_level = usize::BITS - 1 - num_leaves.leading_zeros();
    // how many leaf nodes are in the full level
    let leaves_full_level = 1 << last_full_level;
    // half of the leaf nodes from the full level goes to the left
    let mut num_left_leaf_nodes = leaves_full_level / 2;
    // leaf nodes that do not fit in the full level
    let unbalanced_leaf_nodes = num_leaves - leaves_full_level;
    // distribute unbalanced leaf nodes
    num_left_leaf_nodes += unbalanced_leaf_nodes.min(num_left_leaf_nodes);
    // we should always place unbalanced leaf nodes on the left
    debug_assert!(
      num_left_leaf_nodes >= num_leaves - num_left_leaf_nodes
        && num_left_leaf_nodes <= 2 * (num_leaves - num_left_leaf_nodes)
    );

    num_left_leaf_nodes
  }

  fn check_max_leaf_node_count(&self, num_leaves: usize) -> Result<()> {
    if (self.config.bytes_per_dim as u64) * (num_leaves as u64) > ArrayUtil::MAX_ARRAY_LENGTH as u64
    {
      return Err(LuceneError::illegal_state(format!(
        "too many nodes; increase config.max_points_in_leaf_node() (currently {}) and reindex",
        self.config.max_points_in_leaf_node
      )));
    }
    Ok(())
  }
  /// Writes the BKD tree to the provided [`IndexOutput`](crate::core::store::index_output::IndexOutput)s and returns an
  /// `IORunnable` that writes the index of the tree if at least one point
  /// has been added, or `None` otherwise.
  pub fn finish<IO>(&mut self, data_out: &mut IO) -> Result<Option<IORunnable>>
  where
    IO: IndexOutput,
  {
    if self.finished {
      return Err(LuceneError::illegal_state("already finished"));
    }

    if self.point_count == 0 {
      return Ok(None);
    }

    // Mark as finished
    self.finished = true;

    match self.point_writer.as_mut() {
      None => return Err(LuceneError::illegal_state("point writer is None")),
      Some(writer) => writer.close()?,
    }
    let mut point_writer = expect_invariant!(
      self.point_writer.take(),
      "closing the initialized point writer does not remove its owner"
    );
    let mut points = PathSlice::new(&mut point_writer, 0, self.point_count as usize);

    let max_points_in_leaf_node = self.config.max_points_in_leaf_node;
    let num_leaves = (self.point_count as usize).div_ceil(max_points_in_leaf_node);
    let num_splits = num_leaves - 1;

    self.check_max_leaf_node_count(num_leaves)?;

    // NOTE: we could save the `1+` here, to use a bit less heap at search
    // time, but then we'd need a somewhat costly check at each step
    // of the recursion to recompute the split dim:

    // Indexed by nodeID, but first (root) nodeID is 1.
    // We do `1+` because the lead byte at each recursion says which dim we
    // split on.
    let mut split_packed_values = vec![0u8; num_splits * self.config.bytes_per_dim];
    let mut split_dimension_values = smallvec::smallvec![0u8; num_splits];

    // +1 because leaf count is power of 2 (e.g. 8), and innerNodeCount is
    // power of 2 minus 1 (e.g. 7)
    let mut leaf_block_fps = vec![0usize; num_leaves];

    // Make sure the math above "worked":
    debug_assert!(
      self.point_count / num_leaves as i64 <= self.config.max_points_in_leaf_node as i64,
      "point_count={} num_leaves={} config.max_points_in_leaf_node()={}",
      self.point_count,
      num_leaves,
      self.config.max_points_in_leaf_node
    );

    // We re-use the selector so we do not need to create an object every
    // time.
    let mut radix_selector = BKDRadixSelector::<N>::new_with_bytes(
      self.config.clone(),
      self.max_points_sort_in_heap,
      Arc::clone(&self.temp_file_name_prefix),
    );

    let data_start_fp = data_out.get_file_pointer()?;

    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      stack_or_heap_buffer!(
        parent_splits,
        i32,
        self.config.num_index_dims,
        BKDConfig::MAX_INDEX_DIMS,
        0
      );
      stack_or_heap_buffer!(
        min_packed_value,
        u8,
        self.min_packed_value.len(),
        MAX_NUM_BYTES * BKDConfig::MAX_INDEX_DIMS,
        0
      );
      min_packed_value.copy_from_slice(&self.min_packed_value);
      stack_or_heap_buffer!(
        max_packed_value,
        u8,
        self.max_packed_value.len(),
        MAX_NUM_BYTES * BKDConfig::MAX_INDEX_DIMS,
        0
      );
      max_packed_value.copy_from_slice(&self.max_packed_value);
      stack_or_heap_buffer!(
        spare_doc_ids,
        i32,
        (self.point_count as usize).min(self.config.max_points_in_leaf_node),
        64,
        0
      );
      self.build(
        0,
        num_leaves,
        &mut points,
        data_out,
        &mut radix_selector,
        min_packed_value,
        max_packed_value,
        parent_splits,
        &mut split_packed_values,
        &mut split_dimension_values,
        &mut leaf_block_fps,
        &mut vec![0; self.config.max_points_in_leaf_node],
      )?;

      debug_assert!(
        parent_splits.iter().all(|&x| x == 0),
        "parentSplits should be all zeros at the end"
      );
      // If no error, we should have cleaned everything up:
      debug_assert!(
        !self.temp_dir.has_created_files(),
        "Temp directory should be empty"
      );
      success = true;
      Ok(())
    }));
    if !success {
      let created_files = self.temp_dir.get_created_files();
      IOUtils::delete_files_ignoring_exceptions(&self.temp_dir, &created_files);
    }
    unwrap_caught_result!(result)?;

    let split_packed_values = BytesRef::from_bytes(split_packed_values);

    self.make_writer(
      split_packed_values,
      split_dimension_values,
      leaf_block_fps,
      data_start_fp,
    )
  }

  fn make_writer(
    &self,
    split_packed_values: BytesRef<Vec<u8>>,
    split_dimension_values: smallvec::SmallVec<[u8; 16]>,
    leaf_block_fps: Vec<usize>,
    data_start_fp: usize,
  ) -> Result<Option<IORunnable>> {
    let num_leaves = leaf_block_fps.len();
    let leaf_nodes = BKDTreeLeafNodesEnum::MultiDimensions(BKDTreeLeafNodesImpl {
      scratch_bytes_ref1: split_packed_values,
      leaf_block_fps,
      split_dimension_values,
      bytes_per_dim: self.config.bytes_per_dim,
    });
    Ok(Option::from(IORunnable {
      leaf_nodes,
      num_leaves,
      count_per_leaf: self.config.max_points_in_leaf_node,
      data_start_fp,
    }))
  }
  /// Packs the two arrays, representing a semi-balanced binary tree, into a
  /// compact byte structure.
  fn pack_index<T: BKDTreeLeafNodes>(&self, leaf_nodes: &T) -> Result<Vec<u8>> {
    // Reused while packing the index
    let mut write_buffer = ByteBuffersDataOutput::new_resettable_instance();

    // This is the "file" to which the bytes are appended.
    let mut blocks =
      smallvec::SmallVec::<[Option<Vec<u8>>; 1]>::with_capacity(leaf_nodes.num_leaves());
    stack_or_heap_buffer!(
      last_split_values,
      u8,
      self.config.bytes_per_dim * self.config.num_index_dims,
      MAX_NUM_BYTES * BKDConfig::MAX_INDEX_DIMS,
      0
    );

    stack_or_heap_buffer!(
      negative_deltas,
      bool,
      self.config.num_index_dims,
      BKDConfig::MAX_INDEX_DIMS,
      false
    );
    let total_size = self.recurse_pack_index(
      &mut write_buffer,
      leaf_nodes,
      0,
      &mut blocks,
      last_split_values,
      negative_deltas,
      false,
      0,
      leaf_nodes.num_leaves(),
    )?;

    let mut blocks = blocks.into_iter();
    let mut index = blocks
      .next()
      .flatten()
      .ok_or_else(|| LuceneError::illegal_state("block should not be None"))?;
    index.reserve_exact(total_size - index.len());
    for block in blocks {
      let block = block.ok_or_else(|| LuceneError::illegal_state("block should not be None"))?;
      index.extend_from_slice(&block);
    }
    debug_assert_eq!(index.len(), total_size);

    Ok(index)
  }

  /// Appends the current contents of writeBuffer as another block on the
  /// growing in-memory file.
  fn append_block(
    &self,
    write_buffer: &mut ByteBuffersDataOutput,
    blocks: &mut smallvec::SmallVec<[Option<Vec<u8>>; 1]>,
  ) -> usize {
    // The first block becomes the final index buffer.
    let block = if blocks.is_empty() {
      write_buffer.try_get_array_ownership()
    } else {
      write_buffer.get_array_copy()
    };
    let block_len = block.len();
    if blocks.spilled() && blocks.len() == blocks.capacity() {
      blocks.reserve_exact(blocks.len());
    }
    blocks.push(Option::from(block));
    write_buffer.reset();
    block_len
  }
  /// lastSplitValues is per-dimension split value previously seen; we use
  /// this to prefix-code the split bytes on each inner node
  #[allow(clippy::too_many_arguments)]
  fn recurse_pack_index<T: BKDTreeLeafNodes>(
    &self,
    write_buffer: &mut ByteBuffersDataOutput,
    leaf_nodes: &T,
    min_block_fp: usize,
    blocks: &mut smallvec::SmallVec<[Option<Vec<u8>>; 1]>,
    last_split_values: &mut [u8],
    negative_deltas: &mut [bool],
    is_left: bool,
    leaves_offset: usize,
    num_leaves: usize,
  ) -> Result<usize> {
    if num_leaves == 1 {
      if is_left {
        debug_assert!(leaf_nodes.get_leaf_lp(leaves_offset) - min_block_fp == 0);
        Ok(0)
      } else {
        let delta = leaf_nodes.get_leaf_lp(leaves_offset) - min_block_fp;
        debug_assert!(
          leaf_nodes.num_leaves() == num_leaves || delta > 0,
          "expected delta > 0; got numLeaves = {num_leaves} and delta={delta}"
        );
        write_buffer.write_vlong(delta as i64)?;
        Ok(self.append_block(write_buffer, blocks))
      }
    } else {
      let left_block_fp;
      if is_left {
        // The left tree's left most leaf block FP is always the minimal
        // FP:
        debug_assert!(leaf_nodes.get_leaf_lp(leaves_offset) == min_block_fp);
        left_block_fp = min_block_fp;
      } else {
        left_block_fp = leaf_nodes.get_leaf_lp(leaves_offset);
        let delta = left_block_fp - min_block_fp;
        debug_assert!(
          leaf_nodes.num_leaves() == num_leaves || delta > 0,
          "expected delta > 0; got numLeaves = {num_leaves} and delta={delta}"
        );
        write_buffer.write_vlong(delta as i64)?;
      }

      let num_left_leaf_nodes = self.get_num_left_leaf_nodes(num_leaves);
      let right_offset = leaves_offset + num_left_leaf_nodes;
      let split_offset = right_offset - 1;

      let split_dim = leaf_nodes.get_split_dimension(split_offset);
      let split_value = leaf_nodes.get_split_value(split_offset);
      let split_bytes = split_value.bytes;
      let address = split_value.offset;

      let prefix: usize = self
        .common_prefix_comparator
        .compare(
          split_bytes,
          address,
          last_split_values,
          split_dim * self.config.bytes_per_dim,
        )
        .try_convert()?;

      let first_diff_byte_delta: usize = if prefix < self.config.bytes_per_dim {
        let diff = (split_bytes[address + prefix] as i32)
          - (last_split_values[split_dim * self.config.bytes_per_dim + prefix] as i32);
        if negative_deltas[split_dim] {
          debug_assert!(diff < 0);
          (-diff) as usize
        } else {
          debug_assert!(diff > 0);
          diff as usize
        }
      } else {
        0
      };
      // pack the prefix, splitDim and delta first diff byte into a single
      // vInt:
      let code = (first_diff_byte_delta * (1 + self.config.bytes_per_dim) + prefix)
        * self.config.num_index_dims
        + split_dim;

      write_buffer.write_vint(code as i32)?;

      let suffix = self.config.bytes_per_dim - prefix;
      stack_or_heap_buffer!(sav_split_value, u8, suffix, MAX_NUM_BYTES, 0);

      if suffix > 1 {
        write_buffer.write_bytes_range(split_bytes, address + prefix + 1, suffix - 1)?;
      }

      #[cfg(debug_assertions)]
      let cmp = last_split_values.to_vec();
      let last_split_values_start = split_dim * self.config.bytes_per_dim + prefix;
      sav_split_value.copy_from(
        &last_split_values[last_split_values_start..last_split_values_start + suffix],
        0,
      );
      // copy our split value into lastSplitValues for our children to
      // prefix-code against
      let split_bytes_start = address + prefix;
      last_split_values.copy_from(
        &split_bytes[split_bytes_start..split_bytes_start + suffix],
        last_split_values_start,
      );

      let num_bytes = self.append_block(write_buffer, blocks);

      let idx_sav = blocks.len();
      if num_left_leaf_nodes != 1 {
        if blocks.spilled() && blocks.len() == blocks.capacity() {
          blocks.reserve_exact(blocks.len());
        }
        blocks.push(None);
      }

      let sav_negative_delta = negative_deltas[split_dim];
      negative_deltas[split_dim] = true;

      let left_num_bytes = self.recurse_pack_index(
        write_buffer,
        leaf_nodes,
        left_block_fp,
        blocks,
        last_split_values,
        negative_deltas,
        true,
        leaves_offset,
        num_left_leaf_nodes,
      )?;

      let bytes2_len = if num_left_leaf_nodes != 1 {
        write_buffer.write_vint(left_num_bytes.try_convert()?)?;
        let bytes = write_buffer.get_array_copy();
        let len = bytes.len();
        debug_assert!(len <= i32::MAX as usize);
        write_buffer.reset();
        blocks[idx_sav] = Some(bytes);
        len
      } else {
        debug_assert!(left_num_bytes == 0, "leftNumBytes={left_num_bytes}");
        0
      };

      negative_deltas[split_dim] = false;
      let right_num_bytes = self.recurse_pack_index(
        write_buffer,
        leaf_nodes,
        left_block_fp,
        blocks,
        last_split_values,
        negative_deltas,
        false,
        right_offset,
        num_leaves - num_left_leaf_nodes,
      )?;

      negative_deltas[split_dim] = sav_negative_delta;

      let start = split_dim * self.config.bytes_per_dim + prefix;
      last_split_values.copy_from(&sav_split_value[..suffix], start);

      #[cfg(debug_assertions)]
      debug_assert!(last_split_values == &cmp[..]);

      Ok(num_bytes + bytes2_len + left_num_bytes + right_num_bytes)
    }
  }
  pub fn write_index<O>(
    &self,
    meta_out: &mut O,
    index_out: Option<&mut O>,
    data: &IORunnable,
  ) -> Result<()>
  where
    O: IndexOutput,
  {
    let packed_index = match &data.leaf_nodes {
      BKDTreeLeafNodesEnum::OneDimension(leaf) => self.pack_index(leaf)?,
      BKDTreeLeafNodesEnum::MultiDimensions(leaf) => self.pack_index(leaf)?,
    };
    self.write_index_with_packed_index(meta_out, index_out, &packed_index, data)
  }
  pub fn write_index_with_packed_index<IO, IO2>(
    &self,
    meta_out: &mut IO,
    index_out: Option<&mut IO2>,
    packed_index: &[u8],
    data: &IORunnable,
  ) -> Result<()>
  where
    IO: IndexOutput,
    IO2: IndexOutput,
  {
    let packed_index_len = packed_index.len();
    CodecUtil::write_header(&mut *meta_out, CODEC_NAME, VERSION_CURRENT)?;
    meta_out.write_vint(self.config.num_dims as i32)?;
    meta_out.write_vint(self.config.num_index_dims as i32)?;
    meta_out.write_vint(data.count_per_leaf as i32)?;
    meta_out.write_vint(self.config.bytes_per_dim as i32)?;

    let num_leaves = data.num_leaves.try_convert()?;
    meta_out.write_vint(num_leaves)?;
    meta_out.write_bytes_range(
      &self.min_packed_value,
      0,
      self.config.packed_index_bytes_length(),
    )?;
    meta_out.write_bytes_range(
      &self.max_packed_value,
      0,
      self.config.packed_index_bytes_length(),
    )?;

    meta_out.write_vlong(self.point_count)?;
    meta_out.write_vint(self.docs_seen.cardinality() as i32)?;
    meta_out.write_vint(packed_index_len as i32)?;
    meta_out.write_long(data.data_start_fp as i64)?;
    let (file_pointer, v) = match &index_out {
      // If metaOut and indexOut are the same file, we account for the fact
      // that writing a long makes the index start 8 bytes later.
      None => (meta_out.get_file_pointer()?, BitUtil::LONG_BYTES),
      Some(io) => (io.get_file_pointer()?, 0),
    };

    meta_out.write_long((file_pointer + v) as i64)?;
    match index_out {
      None => {
        // metaOut and indexOut are the same file
        meta_out.write_bytes_range(packed_index, 0, packed_index_len)?;
      },
      Some(index_out) => {
        index_out.write_bytes_range(packed_index, 0, packed_index_len)?;
      },
    }
    Ok(())
  }
  fn write_leaf_block_docs<IO>(
    &mut self,
    out: &mut IO,
    doc_ids: &[i32],
    start: usize,
    count: usize,
  ) -> Result<()>
  where
    IO: IndexOutput,
  {
    debug_assert!(
      count > 0,
      "config.max_points_in_leaf_node()={}",
      self.config.max_points_in_leaf_node
    );
    out.write_vint(count.try_convert()?)?;
    self
      .doc_ids_writer
      .write_doc_ids(doc_ids, start, count, out)?;
    Ok(())
  }
  fn write_leaf_block_packed_values<DO, T>(
    &mut self,
    out: &mut DO,
    count: usize,
    sorted_dim: usize,
    packed_values: &mut T,
    leaf_cardinality: usize,
  ) -> Result<()>
  where
    DO: DataOutput,
    T: PackedValues,
  {
    let prefix_len_sum: usize = self.common_prefix_lengths.iter().sum();
    if prefix_len_sum == self.config.packed_bytes_length() {
      // all values in this block are equal
      out.write_byte(-1i8 as u8)?;
    } else {
      debug_assert!(self.common_prefix_lengths[sorted_dim] < self.config.bytes_per_dim);

      // estimate if storing the values with cardinality is cheaper than
      // storing all values.
      let compressed_byte_offset =
        sorted_dim * self.config.bytes_per_dim + self.common_prefix_lengths[sorted_dim];

      let (high_cardinality_cost, low_cardinality_cost) = if count == leaf_cardinality {
        // all values in this block are different
        (0, 1)
      } else {
        // compute cost of runLen compression
        let mut num_run_lens = 0;
        let mut i = 0;
        while i < count {
          // do run-length compression on the byte at
          // compressed_byte_offset
          let run_len = Self::run_len(
            packed_values,
            i,
            std::cmp::min(i + 0xff, count),
            compressed_byte_offset,
          )?;
          debug_assert!(run_len <= 0xff);
          num_run_lens += 1;
          i += run_len;
        }

        // Add cost of runLen compression
        let packed_len = self.config.packed_bytes_length();

        let value = packed_len
          .checked_sub(prefix_len_sum)
          .and_then(|v| v.checked_sub(1))
          .ok_or_else(|| {
            LuceneError::illegal_state("packed_bytes_length - prefix_len_sum - 1 < 0")
          })?;
        let high_cardinality_cost = count * value + 2 * num_run_lens;

        // +1 is the byte needed for storing the cardinality
        let low_cardinality_cost =
          leaf_cardinality * (self.config.packed_bytes_length() - prefix_len_sum + 1);

        (high_cardinality_cost, low_cardinality_cost)
      };

      if low_cardinality_cost <= high_cardinality_cost {
        out.write_byte(-2i8 as u8)?;
        self.write_low_cardinality_leaf_block_packed_values(out, count, packed_values)?;
      } else {
        out.write_byte(sorted_dim as u8)?;
        self.write_high_cardinality_leaf_block_packed_values(
          out,
          count,
          sorted_dim,
          packed_values,
          compressed_byte_offset,
        )?;
      }
    }
    Ok(())
  }
  fn write_low_cardinality_leaf_block_packed_values<DO, T>(
    &mut self,
    out: &mut DO,
    count: usize,
    packed_values: &mut T,
  ) -> Result<()>
  where
    DO: DataOutput,
    T: PackedValues,
  {
    if self.config.num_index_dims != 1 {
      self.write_actual_bounds(out, count, packed_values)?;
    }

    let value = packed_values.get_value(0)?;
    let bytes_ref = value.bytes;
    let offset = value.offset;
    self.scratch.copy_from(
      &bytes_ref[offset..(offset + self.config.packed_bytes_length())],
      0,
    );

    let mut cardinality = 1;
    for i in 1..count {
      let value = packed_values.get_value(i)?;
      let bytes_ref = value.bytes;
      let offset = value.offset;
      for dim in 0..self.config.num_dims {
        let start = dim * self.config.bytes_per_dim;
        if !self
          .equals_predicate
          .test(bytes_ref, offset + start, &self.scratch, start)
        {
          out.write_vint(cardinality)?;
          for j in 0..self.config.num_dims {
            out.write_bytes_range(
              &self.scratch,
              j * self.config.bytes_per_dim + self.common_prefix_lengths[j],
              self.config.bytes_per_dim - self.common_prefix_lengths[j],
            )?;
          }
          self.scratch.copy_from(
            &bytes_ref[offset..(offset + self.config.packed_bytes_length())],
            0,
          );
          cardinality = 1;
          break;
        } else if dim == self.config.num_dims - 1 {
          cardinality += 1;
        }
      }
    }

    out.write_vint(cardinality)?;
    for i in 0..self.config.num_dims {
      out.write_bytes_range(
        &self.scratch,
        i * self.config.bytes_per_dim + self.common_prefix_lengths[i],
        self.config.bytes_per_dim - self.common_prefix_lengths[i],
      )?;
    }

    Ok(())
  }
  fn write_high_cardinality_leaf_block_packed_values<DO, T>(
    &mut self,
    out: &mut DO,
    count: usize,
    sorted_dim: usize,
    packed_values: &mut T,
    compressed_byte_offset: usize,
  ) -> Result<()>
  where
    DO: DataOutput,
    T: PackedValues,
  {
    if self.config.num_index_dims != 1 {
      self.write_actual_bounds(out, count, packed_values)?;
    }

    self.common_prefix_lengths[sorted_dim] += 1;

    let mut i = 0;
    while i < count {
      // Do run-length compression on the byte at compressedByteOffset
      let run_len = Self::run_len(
        packed_values,
        i,
        std::cmp::min(i + 0xff, count),
        compressed_byte_offset,
      )?;
      debug_assert!(run_len <= 0xff);

      let value = packed_values.get_value(i)?;
      let bytes_ref = value.bytes;
      let offset = value.offset;
      let prefix_byte = bytes_ref[offset + compressed_byte_offset];

      out.write_byte(prefix_byte)?;
      out.write_byte(run_len as u8)?;

      self.write_leaf_block_packed_values_range(out, i, i + run_len, packed_values)?;
      i += run_len;
      debug_assert!(i <= count);
    }

    Ok(())
  }
  fn write_actual_bounds<DO, T>(
    &self,
    out: &mut DO,
    count: usize,
    packed_values: &mut T,
  ) -> Result<()>
  where
    DO: DataOutput,
    T: PackedValues,
  {
    for dim in 0..self.config.num_index_dims {
      let common_prefix_length = self.common_prefix_lengths[dim];
      let suffix_length = self.config.bytes_per_dim - common_prefix_length;

      if suffix_length > 0 {
        let (min, max) = self.compute_min_max(
          count,
          packed_values,
          dim * self.config.bytes_per_dim + common_prefix_length,
          suffix_length,
        )?;

        out.write_bytes_range(&min.bytes, min.offset, min.length)?;
        out.write_bytes_range(&max.bytes, max.offset, max.length)?;
      }
    }
    Ok(())
  }
  /// Return an array that contains the min and max values for the [offset,
  ///
  /// `offset..offset + length` interval of the given [`BytesRef`] values.
  #[allow(clippy::type_complexity)]
  fn compute_min_max<T>(
    &self,
    count: usize,
    packed_values: &mut T,
    offset: usize,
    length: usize,
  ) -> Result<(BytesRef<Vec<u8>>, BytesRef<Vec<u8>>)>
  where
    T: PackedValues,
  {
    debug_assert!(length > 0);
    let value = packed_values.get_value(0)?;
    let bytes_ref = value.bytes;
    let first_offset = value.offset;
    let mut min: BytesRefBuilder<Vec<u8>> = BytesRefBuilder::new();
    let mut max: BytesRefBuilder<Vec<u8>> = BytesRefBuilder::new();
    let bytes = bytes_ref;
    min.copy_bytes_from_vec(bytes, first_offset + offset, length)?;
    max.copy_bytes_from_vec(bytes, first_offset + offset, length)?;

    let length_usize = length;
    let offset_usize = offset;
    for i in 1..count {
      let value = packed_values.get_value(i)?;
      let bytes_ref = value.bytes;
      let candidate_offset = value.offset;
      let candidate_offset_usize = candidate_offset;
      let candidate_bytes = bytes_ref;
      if min.bytes().bytes[0..length_usize]
        .cmp(
          &candidate_bytes[candidate_offset_usize + offset_usize
            ..candidate_offset_usize + offset_usize + length_usize],
        )
        .to_int()
        > 0
      {
        min.copy_bytes_from_vec(candidate_bytes, candidate_offset + offset, length)?;
      } else if max.bytes().bytes[0..length_usize]
        .cmp(
          &candidate_bytes[candidate_offset_usize + offset_usize
            ..candidate_offset_usize + offset_usize + length_usize],
        )
        .to_int()
        < 0
      {
        max.copy_bytes_from_vec(candidate_bytes, candidate_offset + offset, length)?;
      }
    }
    Ok((min.get_bytes_owner(), max.get_bytes_owner()))
  }
  fn write_leaf_block_packed_values_range<DO, T>(
    &self,
    out: &mut DO,
    start: usize,
    end: usize,
    packed_values: &mut T,
  ) -> Result<()>
  where
    DO: DataOutput,
    T: PackedValues,
  {
    for i in start..end {
      let value = packed_values.get_value(i)?;
      let bytes_ref = value.bytes;
      let offset = value.offset;
      let length = value.length;
      debug_assert!(length == self.config.packed_bytes_length());

      for dim in 0..self.config.num_dims {
        let prefix = self.common_prefix_lengths[dim];
        out.write_bytes_range(
          bytes_ref,
          offset + (dim * self.config.bytes_per_dim) + prefix,
          self.config.bytes_per_dim - prefix,
        )?;
      }
    }
    Ok(())
  }

  fn run_len<T>(
    packed_values: &mut T,
    start: usize,
    end: usize,
    byte_offset: usize,
  ) -> Result<usize>
  where
    T: PackedValues,
  {
    let value = packed_values.get_value(start)?;
    let bytes_ref = value.bytes;
    let offset = value.offset;
    let b = bytes_ref[offset + byte_offset];
    for i in (start + 1)..end {
      let value = packed_values.get_value(i)?;
      let bytes_ref = value.bytes;
      let offset = value.offset;
      let b2 = bytes_ref[offset + byte_offset];
      debug_assert!(b2 >= b);
      if b != b2 {
        return Ok(i - start);
      }
    }
    Ok(end - start)
  }
  fn write_common_prefixes<DO>(
    &self,
    out: &mut DO,
    common_prefixes: &[usize],
    packed_value: &[u8],
  ) -> Result<()>
  where
    DO: DataOutput,
  {
    for (dim, &prefix) in common_prefixes
      .iter()
      .enumerate()
      .take(self.config.num_dims)
    {
      out.write_vint(prefix as i32)?;
      out.write_bytes_range(packed_value, dim * self.config.bytes_per_dim, prefix)?;
    }
    Ok(())
  }

  /// Called on error, to check whether the checksum is also corrupt in
  /// this source, and add that information (checksum matched or didn't)
  /// as a suppressed error.
  fn verify_checksum<T, R>(
    &self,
    prior_result: CaughtResult<T>,
    writer: &PointWriterEnum<<TrackingDirectoryWrapper<D> as Directory>::IndexOutput, N>,
  ) -> Result<R> {
    if let PointWriterEnum::Offline(writer) = writer {
      // We are reading from a temp file; go verify the checksum:
      if self.temp_dir.has_created_file(&writer.name) {
        let mut input = self.temp_dir.open_checksum_input(&writer.name)?;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
          CodecUtil::check_footer_with_error(&mut input, Some(prior_result))
        }));
        let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| input.close()));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
          IOUtils::use_or_suppress_caught_result(result, close_result)
        }));
        return IOUtils::rethrow_always(result);
      }
    }
    IOUtils::rethrow_always(prior_result)
  }
  /// Pick the next dimension to split.
  ///
  /// # Arguments
  /// * `min_packed_value` - The min values for all dimensions.
  /// * `max_packed_value` - The max values for all dimensions.
  /// * `parent_splits` - How many times each dimension has been split on the
  ///   parent levels.
  ///
  /// # Returns
  /// The dimension to split.
  fn split(
    &mut self,
    min_packed_value: &[u8],
    max_packed_value: &[u8],
    parent_splits: &[i32],
  ) -> Result<usize> {
    // First look at whether there is a dimension that has split less than
    // 2x less than the dim that has most splits, and return it if
    // there is such a dimension and it does not only have equal
    // values. This helps ensure all dimensions are indexed.
    let mut max_num_splits = 0;
    for num_splits in parent_splits {
      max_num_splits = std::cmp::max(max_num_splits, *num_splits);
    }

    for (dim, &split_count) in parent_splits
      .iter()
      .enumerate()
      .take(self.config.num_index_dims)
    {
      let offset = dim * self.config.bytes_per_dim;
      if split_count < max_num_splits / 2
        && self
          .comparator
          .compare(min_packed_value, offset, max_packed_value, offset)
          != 0
      {
        return Ok(dim);
      }
    }

    // Find which dim has the largest span so we can split on it:
    let mut split_dim: Option<usize> = None;

    for dim in 0..self.config.num_index_dims {
      NumericUtils::subtract(
        self.config.bytes_per_dim,
        dim,
        max_packed_value,
        min_packed_value,
        &mut self.scratch_diff,
      )?;
      if split_dim.is_none()
        || self
          .comparator
          .compare(&self.scratch_diff, 0, &self.scratch, 0)
          > 0
      {
        self
          .scratch
          .copy_from(&self.scratch_diff[0..self.config.bytes_per_dim], 0);
        split_dim = Some(dim);
      }
    }
    split_dim.ok_or_else(|| LuceneError::illegal_state("no split dimension was selected"))
  }
  fn copy_points_to_heap<R: PointReader>(
    reader: &mut R,
    writer: &mut HeapPointWriter<N>,
    count: usize,
  ) -> Result<()> {
    for _ in 0..count {
      let has_next = reader.next()?;
      debug_assert!(has_next);
      let value = reader.point_value()?;
      writer.append_point_value(&value)?;
    }
    Ok(())
  }

  /// Pull a partition back into heap once the point count is low enough while recursing.
  fn switch_to_heap(
    &mut self,
    source: &mut PointWriterEnum<<TrackingDirectoryWrapper<D> as Directory>::IndexOutput, N>,
  ) -> Result<HeapPointWriter<N>> {
    let source_count = source.count();
    if source_count > i32::MAX as usize {
      return Err(LuceneError::unsupported_operation(
        "Too many points to switch to heap",
      ));
    }
    let count = source_count;
    let mut reader = None;
    let mut writer = None;
    let body_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      reader = Some(source.get_reader(0, source_count, &self.temp_dir)?);
      writer = Some(HeapPointWriter::<N>::new_with_bytes(
        self.config.clone(),
        count,
      ));
      let reader = expect_invariant!(
        reader.as_mut(),
        "reader initialization succeeded before copying points"
      );
      let writer = expect_invariant!(
        writer.as_mut(),
        "writer initialization succeeded before copying points"
      );
      match reader {
        PointReaderEnum::Heap(reader) => Self::copy_points_to_heap(reader, writer, count)?,
        PointReaderEnum::Offline(reader) => match reader {
          OfflinePointReader::Plain(reader) => Self::copy_points_to_heap(reader, writer, count)?,
          OfflinePointReader::Checksum(reader) => Self::copy_points_to_heap(reader, writer, count)?,
        },
      }
      source.destroy(&self.temp_dir)?;
      Ok(())
    }));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      IOUtils::close((writer.as_mut(), reader.as_mut()))
    }));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      IOUtils::use_or_suppress_caught_result(body_result, close_result)
    }));
    if let Some(reader) = reader.as_mut() {
      source.take_data(reader.remove_points());
    }
    if !matches!(&result, Ok(Ok(()))) {
      return self.verify_checksum(result, source);
    }

    Ok(expect_invariant!(
      writer,
      "successful point copy and close require an initialized heap writer"
    ))
  }

  /// Recursively reorders the provided reader and writes the bkd-tree on the
  /// fly; this method is used when we are writing a new segment directly
  /// from IndexWriter's indexing buffer (MutablePointsReader).
  #[allow(clippy::too_many_arguments)]
  fn build_with_reader<M, IO>(
    &mut self,
    leaves_offset: usize,
    num_leaves: usize,
    reader: &mut M,
    from: usize,
    to: usize,
    out: &mut IO,
    mut min_packed_value: smallvec::SmallVec<[u8; 32]>,
    mut max_packed_value: smallvec::SmallVec<[u8; 32]>,
    parent_splits: &mut [i32],
    split_packed_values: &mut [u8],
    split_dimension_values: &mut [u8],
    leaf_block_fps: &mut [usize],
    spare_doc_ids: &mut [i32],
  ) -> Result<()>
  where
    M: MutablePointTree,
    IO: IndexOutput,
  {
    if num_leaves == 1 {
      // leaf node
      let count = to - from;
      debug_assert!(count <= self.config.max_points_in_leaf_node);

      // Compute common prefixes
      self.common_prefix_lengths.fill(self.config.bytes_per_dim);
      let mut sorted_dim = 0;
      let mut leaf_cardinality = 1;
      {
        let first = reader.get_value(from, &mut self.scratch_bytes_ref1)?;
        for i in from + 1..to {
          let value = reader.get_value(i, &mut self.scratch_bytes_ref2)?;
          for dim in 0..self.config.num_dims {
            let offset = dim * self.config.bytes_per_dim;
            let dimension_prefix_length = self.common_prefix_lengths[dim];
            let v: usize = self
              .common_prefix_comparator
              .compare(
                first.bytes,
                first.offset + offset,
                value.bytes,
                value.offset + offset,
              )
              .try_convert()?;
            self.common_prefix_lengths[dim] = v.min(dimension_prefix_length);
          }
        }

        // Find the dimension that has the least number of unique bytes
        // at commonPrefixLengths[dim]
        if self.used_bytes.is_empty() {
          self.used_bytes = smallvec::smallvec![None; self.config.num_dims];
        }
        let used_bytes = self.used_bytes.as_mut_slice();
        for (dim, used) in used_bytes.iter_mut().take(self.config.num_dims).enumerate() {
          if self.common_prefix_lengths[dim] < self.config.bytes_per_dim {
            match used {
              Some(set) => set.clear()?,
              None => *used = Some(FixedBitSet::new(256)),
            }
          } else {
            *used = None;
          }
        }

        #[allow(clippy::needless_range_loop)]
        for i in from + 1..to {
          for (dim, used) in used_bytes.iter_mut().take(self.config.num_dims).enumerate() {
            if let Some(set) = used {
              let b = reader.get_byte_at(
                i,
                dim * self.config.bytes_per_dim + self.common_prefix_lengths[dim],
              );
              set.set(b as usize)?;
            }
          }
        }
        let mut sorted_dim_cardinality = i32::MAX as usize;
        for (dim, used) in used_bytes.iter().take(self.config.num_dims).enumerate() {
          if let Some(set) = used {
            let cardinality = set.cardinality();
            if cardinality < sorted_dim_cardinality {
              sorted_dim = dim;
              sorted_dim_cardinality = cardinality;
            }
          }
        }

        // sort by sortedDim
        MutablePointTreeReaderUtils::sort_by_dim_with_bytes::<_, N>(
          &self.config,
          sorted_dim,
          &self.common_prefix_lengths,
          reader,
          from,
          to,
          &mut self.scratch_bytes_ref1,
          &mut self.scratch_bytes_ref2,
        )?;
        // Keep the previous distinct value across reuse of the cross-block spare.
        let comparator = &mut self.scratch_bytes_ref1;
        let collector_spare = &mut self.scratch_bytes_ref2;
        let first = reader.get_value(from, collector_spare)?;
        comparator.copy_from_slice(&first.bytes[first.offset..first.offset + first.length]);
        for i in from + 1..to {
          let collector = reader.get_value(i, collector_spare)?;
          for dim in 0..self.config.num_dims {
            let start = dim * self.config.bytes_per_dim;
            if !self.equals_predicate.test(
              collector.bytes,
              collector.offset + start,
              &comparator.bytes,
              comparator.offset + start,
            ) {
              leaf_cardinality += 1;
              comparator.copy_from_slice(
                &collector.bytes[collector.offset..collector.offset + collector.length],
              );
              break;
            }
          }
        }

        // Save the block file pointer:
        leaf_block_fps[leaves_offset] = out.get_file_pointer()?;

        // Write doc IDs
        for i in from..to {
          spare_doc_ids[i - from] = reader.get_doc_id(i)?;
        }
        self.write_leaf_block_docs(out, spare_doc_ids, 0, count)?;

        // Write the common prefixes, borrowing the point directly while writing.
        let mut spare = std::mem::take(&mut self.scratch_bytes_ref1);
        let result = reader.get_value(from, &mut spare).and_then(|value| {
          self.write_common_prefixes(
            out,
            &self.common_prefix_lengths,
            &value.bytes[value.offset..value.offset + self.config.packed_bytes_length()],
          )
        });
        self.scratch_bytes_ref1 = spare;
        result?;
      }
      // Write the full values:
      let mut packed_values = PackedValuesImpl2 {
        scratch: std::mem::take(&mut self.scratch_bytes_ref1),
        reader,
        from,
      };
      let result = (|| -> Result<()> {
        debug_assert!(values_in_order_and_bounds(
          self.config.clone(),
          count,
          sorted_dim,
          min_packed_value.as_mut(),
          max_packed_value.as_mut(),
          &mut packed_values,
          spare_doc_ids,
          0,
        )?);

        self.write_leaf_block_packed_values(
          out,
          count,
          sorted_dim,
          &mut packed_values,
          leaf_cardinality,
        )
      })();
      self.scratch_bytes_ref1 = std::mem::take(&mut packed_values.scratch);
      result?;
    } else {
      // inner node

      let split_dim = if self.config.num_index_dims == 1 {
        0
      } else {
        // for dimensions > 2 we recompute the bounds for the current
        // inner node to help the algorithm choose best
        // split dimensions. Because it is an expensive operation, the
        // frequency we recompute the bounds is given
        // by SPLITS_BEFORE_EXACT_BOUNDS.
        if num_leaves != leaf_block_fps.len()
          && self.config.num_index_dims > 2
          && parent_splits.iter().sum::<i32>() % SPLITS_BEFORE_EXACT_BOUNDS == 0
        {
          match self.config.bytes_per_dim {
            4 => self.compute_packed_value_bounds_with_tree::<_, 4>(
              reader,
              from,
              to,
              min_packed_value.as_mut(),
              max_packed_value.as_mut(),
            )?,
            8 => self.compute_packed_value_bounds_with_tree::<_, 8>(
              reader,
              from,
              to,
              min_packed_value.as_mut(),
              max_packed_value.as_mut(),
            )?,
            _ => self.compute_packed_value_bounds_with_tree::<_, 0>(
              reader,
              from,
              to,
              min_packed_value.as_mut(),
              max_packed_value.as_mut(),
            )?,
          }
        }
        self.split(
          min_packed_value.as_ref(),
          max_packed_value.as_ref(),
          parent_splits,
        )?
      };
      // How many leaves will be in the left tree:
      let num_left_leaf_nodes = self.get_num_left_leaf_nodes(num_leaves);
      // How many points will be in the left tree:
      let mid = from + num_left_leaf_nodes * self.config.max_points_in_leaf_node;

      let common_prefix_len = self.common_prefix_comparator.compare(
        min_packed_value.as_ref(),
        split_dim * self.config.bytes_per_dim,
        max_packed_value.as_ref(),
        split_dim * self.config.bytes_per_dim,
      );

      MutablePointTreeReaderUtils::partition_with_bytes::<_, N>(
        &self.config,
        self.max_doc,
        split_dim,
        common_prefix_len.try_convert()?,
        reader,
        from,
        to,
        mid,
        &mut self.scratch_bytes_ref1,
        &mut self.scratch_bytes_ref2,
      )?;

      let right_offset = leaves_offset + num_left_leaf_nodes;
      let split_offset = right_offset - 1;
      let address = split_offset * self.config.bytes_per_dim;
      split_dimension_values[split_offset] = split_dim as u8;
      let value = reader.get_value(mid, &mut self.scratch_bytes_ref1)?;
      let start = value.offset + (split_dim * self.config.bytes_per_dim);
      split_packed_values.copy_from(
        &value.bytes[start..start + self.config.bytes_per_dim],
        address,
      );

      let mut min_split_packed_value = smallvec::SmallVec::from_slice(
        &self.min_packed_value[..self.config.packed_index_bytes_length()],
      );
      let mut max_split_packed_value = smallvec::SmallVec::from_slice(
        &self.max_packed_value[..self.config.packed_index_bytes_length()],
      );

      let start = value.offset + (split_dim * self.config.bytes_per_dim);
      let end = start + self.config.bytes_per_dim;
      let offset = split_dim * self.config.bytes_per_dim;
      min_split_packed_value.copy_from(&value.bytes[start..end], offset);
      max_split_packed_value.copy_from(&value.bytes[start..end], offset);
      // recurse
      parent_splits[split_dim] += 1;
      self.build_with_reader(
        leaves_offset,
        num_left_leaf_nodes,
        reader,
        from,
        mid,
        out,
        min_packed_value,
        max_split_packed_value,
        parent_splits,
        split_packed_values,
        split_dimension_values,
        leaf_block_fps,
        spare_doc_ids,
      )?;
      self.build_with_reader(
        right_offset,
        num_leaves - num_left_leaf_nodes,
        reader,
        mid,
        to,
        out,
        min_split_packed_value,
        max_packed_value,
        parent_splits,
        split_packed_values,
        split_dimension_values,
        leaf_block_fps,
        spare_doc_ids,
      )?;
      parent_splits[split_dim] -= 1;
    }

    Ok(())
  }

  fn compute_packed_value_bounds_from_reader<R: PointReader>(
    &self,
    reader: &mut R,
    min_packed_value: &mut [u8],
    max_packed_value: &mut [u8],
  ) -> Result<()> {
    if !reader.next()? {
      return Ok(());
    }
    {
      let point_value = reader.point_value()?;
      let value = point_value.packed_value();
      let offset = value.offset;
      let value = value.bytes;
      min_packed_value.copy_from(
        &value[offset..(offset + self.config.packed_index_bytes_length())],
        0,
      );
      max_packed_value.copy_from(
        &value[offset..(offset + self.config.packed_index_bytes_length())],
        0,
      );
    }

    while reader.next()? {
      let point_value = reader.point_value()?;
      let value = point_value.packed_value();
      let offset = value.offset;
      let value = value.bytes;
      for dim in 0..self.config.num_index_dims {
        let start_offset = dim * self.config.bytes_per_dim;
        if self
          .comparator
          .compare(value, offset + start_offset, min_packed_value, start_offset)
          < 0
        {
          min_packed_value.copy_from(
            &value[offset + start_offset..offset + start_offset + self.config.bytes_per_dim],
            start_offset,
          );
        } else if self.comparator.compare(
          value,
          offset + start_offset,
          max_packed_value,
          start_offset,
        ) > 0
        {
          max_packed_value.copy_from(
            &value[offset + start_offset..offset + start_offset + self.config.bytes_per_dim],
            start_offset,
          );
        }
      }
    }
    Ok(())
  }

  fn compute_packed_value_bounds(
    &mut self,
    slice: &mut PathSlice<<TrackingDirectoryWrapper<D> as Directory>::IndexOutput, N>,
    min_packed_value: &mut [u8],
    max_packed_value: &mut [u8],
  ) -> Result<()> {
    let mut reader = slice
      .writer
      .get_reader(slice.start, slice.count, &self.temp_dir)?;

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      match &mut reader {
        PointReaderEnum::Heap(reader) => {
          self.compute_packed_value_bounds_from_reader(reader, min_packed_value, max_packed_value)
        },
        PointReaderEnum::Offline(reader) => match reader {
          OfflinePointReader::Plain(reader) => {
            self.compute_packed_value_bounds_from_reader(reader, min_packed_value, max_packed_value)
          },
          OfflinePointReader::Checksum(reader) => {
            self.compute_packed_value_bounds_from_reader(reader, min_packed_value, max_packed_value)
          },
        },
      }
    }));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reader.close()));
    slice.writer.take_data(reader.remove_points());

    IOUtils::use_or_suppress_caught_result(result, close_result)
  }
  /// The point writer contains the data that is going to be splitted using
  /// radix selection. /* This method is used when we are merging
  /// previously written segments, in the numDims > 1 case.
  #[allow(clippy::too_many_arguments)]
  fn build<IO>(
    &mut self,
    leaves_offset: usize,
    num_leaves: usize,
    points: &mut PathSlice<<TrackingDirectoryWrapper<D> as Directory>::IndexOutput, N>,
    out: &mut IO,
    radix_selector: &mut BKDRadixSelector<N>,
    min_packed_value: &mut [u8],
    max_packed_value: &mut [u8],
    parent_splits: &mut [i32],
    split_packed_values: &mut [u8],
    split_dimension_values: &mut [u8],
    leaf_block_fps: &mut [usize],
    spare_doc_ids: &mut [i32],
  ) -> Result<()>
  where
    IO: IndexOutput,
  {
    if num_leaves == 1 {
      let mut heap_writer;
      let heap_source = match &mut *points.writer {
        PointWriterEnum::Heap(heap) => heap,
        source => {
          heap_writer = self.switch_to_heap(source)?;
          &mut heap_writer
        },
      };

      let from = points.start;
      let to = points.start + points.count;

      let mut sorted_dim = 0;
      {
        self.compute_common_prefix_length(heap_source, from, to)?;
        let mut sorted_dim_cardinality = i32::MAX as usize;
        if self.used_bytes.is_empty() {
          self.used_bytes = smallvec::smallvec![None; self.config.num_dims];
        }
        let used_bytes = self.used_bytes.as_mut_slice();
        for (dim, used) in used_bytes.iter_mut().take(self.config.num_dims).enumerate() {
          if self.common_prefix_lengths[dim] < self.config.bytes_per_dim {
            match used {
              Some(set) => set.clear()?,
              None => *used = Some(FixedBitSet::new(256)),
            }
          } else {
            *used = None;
          }
        }

        #[allow(clippy::needless_range_loop)]
        for dim in 0..self.config.num_dims {
          let prefix = self.common_prefix_lengths[dim];
          if prefix < self.config.bytes_per_dim {
            let offset = dim * self.config.bytes_per_dim;
            for i in from..to {
              let value = heap_source.get_packed_value_slice(i)?.packed_value();
              let bucket = value.bytes[value.offset + offset + prefix] as usize;
              match used_bytes[dim] {
                Some(ref mut set) => set.set(bucket)?,
                None => {
                  return Err(LuceneError::illegal_state(
                    "used_bytes[dim] should not be None",
                  ));
                },
              }
            }
            let cardinality = match used_bytes[dim] {
              Some(ref set) => set.cardinality(),
              None => {
                return Err(LuceneError::illegal_state(
                  "used_bytes[dim] should not be None",
                ));
              },
            };
            if cardinality < sorted_dim_cardinality {
              sorted_dim = dim;
              sorted_dim_cardinality = cardinality;
            }
          }
        }
      }
      radix_selector.heap_radix_sort(
        heap_source,
        from,
        to,
        sorted_dim,
        self.common_prefix_lengths[sorted_dim],
      )?;
      let count = to - from;
      let leaf_cardinality =
        heap_source.compute_cardinality(from, to, &self.common_prefix_lengths)?;
      {
        leaf_block_fps[leaves_offset] = out.get_file_pointer()?;

        debug_assert!(count > 0);
        debug_assert!(count <= spare_doc_ids.len());
        for (i, out) in spare_doc_ids.iter_mut().take(count).enumerate() {
          *out = heap_source.get_packed_value_slice(from + i)?.doc_id();
        }
      };

      self.write_leaf_block_docs(out, spare_doc_ids, 0, count)?;

      self.write_common_prefixes(out, &self.common_prefix_lengths, &self.scratch)?;

      let mut packed_values = PackedValuesImpl3 { heap_source, from };
      debug_assert!(values_in_order_and_bounds(
        self.config.clone(),
        count,
        sorted_dim,
        min_packed_value,
        max_packed_value,
        &mut packed_values,
        spare_doc_ids,
        0,
      )?);

      self.write_leaf_block_packed_values(
        out,
        count,
        sorted_dim,
        &mut packed_values,
        leaf_cardinality,
      )?;
    } else {
      let split_dim = if self.config.num_index_dims == 1 {
        0
      } else {
        if num_leaves != leaf_block_fps.len()
          && self.config.num_index_dims > 2
          && parent_splits.iter().sum::<i32>() % SPLITS_BEFORE_EXACT_BOUNDS == 0
        {
          self.compute_packed_value_bounds(points, min_packed_value, max_packed_value)?;
        }
        self.split(min_packed_value, max_packed_value, parent_splits)?
      };

      debug_assert!(num_leaves <= leaf_block_fps.len());

      let num_left_leaf_nodes = self.get_num_left_leaf_nodes(num_leaves);
      let left_count = num_left_leaf_nodes * self.config.max_points_in_leaf_node;

      let common_prefix_len = self.common_prefix_comparator.compare(
        min_packed_value,
        split_dim * self.config.bytes_per_dim,
        max_packed_value,
        split_dim * self.config.bytes_per_dim,
      );

      let mut select_slice = radix_selector.select(
        points,
        points.start,
        points.start + points.count,
        points.start + left_count,
        split_dim,
        common_prefix_len.try_convert()?,
        &self.temp_dir,
      )?;
      let right_offset = leaves_offset + num_left_leaf_nodes;
      let split_value_offset = right_offset - 1;

      split_dimension_values[split_value_offset] = split_dim as u8;
      let address = split_value_offset * self.config.bytes_per_dim;
      split_packed_values.copy_from(
        &select_slice.partition[0..self.config.bytes_per_dim],
        address,
      );

      let bounds_len = self.config.packed_index_bytes_length();
      let mut min_split_packed_value =
        smallvec::SmallVec::<[u8; 32]>::from_slice(&min_packed_value[..bounds_len]);
      let mut max_split_packed_value =
        smallvec::SmallVec::<[u8; 32]>::from_slice(&max_packed_value[..bounds_len]);

      let start = split_dim * self.config.bytes_per_dim;
      min_split_packed_value
        .copy_from(&select_slice.partition[0..self.config.bytes_per_dim], start);
      max_split_packed_value
        .copy_from(&select_slice.partition[0..self.config.bytes_per_dim], start);

      parent_splits[split_dim] += 1;
      let mut left_slice = match select_slice.left_writer {
        Some(ref mut left_writer) => {
          PathSlice::new(left_writer, select_slice.left_from, select_slice.left_to)
        },
        None => PathSlice::new(points.writer, select_slice.left_from, select_slice.left_to),
      };
      self.build(
        leaves_offset,
        num_left_leaf_nodes,
        &mut left_slice,
        out,
        radix_selector,
        min_packed_value,
        &mut max_split_packed_value,
        parent_splits,
        split_packed_values,
        split_dimension_values,
        leaf_block_fps,
        spare_doc_ids,
      )?;
      let mut right_slice = match select_slice.right_writer {
        Some(ref mut right_writer) => {
          PathSlice::new(right_writer, select_slice.right_from, select_slice.right_to)
        },
        None => PathSlice::new(
          points.writer,
          select_slice.right_from,
          select_slice.right_to,
        ),
      };
      self.build(
        right_offset,
        num_leaves - num_left_leaf_nodes,
        &mut right_slice,
        out,
        radix_selector,
        &mut min_split_packed_value,
        max_packed_value,
        parent_splits,
        split_packed_values,
        split_dimension_values,
        leaf_block_fps,
        spare_doc_ids,
      )?;

      parent_splits[split_dim] -= 1;
    }

    Ok(())
  }

  fn compute_common_prefix_length(
    &mut self,
    heap_point_writer: &mut HeapPointWriter<N>,
    from: usize,
    to: usize,
  ) -> Result<()> {
    self.common_prefix_lengths.fill(self.config.bytes_per_dim);

    {
      let point_value = heap_point_writer.get_packed_value_slice(from)?;
      let value = point_value.packed_value();
      let bytes = value.bytes;
      let offset = value.offset;

      for dim in 0..self.config.num_dims {
        let src_offset = offset + dim * self.config.bytes_per_dim;
        let dst_offset = dim * self.config.bytes_per_dim;
        self.scratch.copy_from(
          &bytes[src_offset..src_offset + self.config.bytes_per_dim],
          dst_offset,
        );
      }
    }

    for i in from + 1..to {
      let point_value = heap_point_writer.get_packed_value_slice(i)?;
      let value = point_value.packed_value();
      let bytes = value.bytes;
      let offset = value.offset;

      for dim in 0..self.config.num_dims {
        if self.common_prefix_lengths[dim] != 0 {
          let v: usize = self
            .common_prefix_comparator
            .compare(
              &self.scratch,
              dim * self.config.bytes_per_dim,
              bytes,
              offset + dim * self.config.bytes_per_dim,
            )
            .try_convert()?;
          self.common_prefix_lengths[dim] = self.common_prefix_lengths[dim].min(v);
        }
      }
    }
    Ok(())
  }
  pub fn close(&mut self) -> Result<()> {
    self.finished = true;
    if let Some(PointWriterEnum::Offline(offline_point_writer)) = self.point_writer.as_mut() {
      let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // Exception cleanup closes the output directly, without writing a footer.
        // Record the attempt so Rust Drop does not repeat the writer-level close.
        offline_point_writer.close_attempted = true;
        match offline_point_writer.out.as_mut() {
          Some(out) => out.close(),
          None => Ok(()),
        }
      }));
      let delete_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
          if let Some(PointWriterEnum::Offline(offline_point_writer)) = self.point_writer.as_mut() {
            self.temp_dir.delete_file(&offline_point_writer.name)?;
            offline_point_writer.closed = true;
          }
          self.point_writer = None;
          Ok(())
        }));
      IOUtils::finally_caught_result(close_result, delete_result)?;
    }
    Ok(())
  }
}

impl<D, const N: usize> Closeable for BKDWriter<D, N>
where
  D: Directory,
{
  fn close(&mut self) -> Result<()> {
    BKDWriter::close(self)
  }
}

pub struct OneDimensionBKDWriter<'a, D, O, const N: usize>
where
  D: Directory,
  O: IndexOutput,
{
  data_out: &'a mut O,
  data_start_fp: usize,
  leaf_block_fps: Vec<usize>,
  leaf_block_start_values: Vec<u8>,
  leaf_values: Vec<u8>,
  leaf_docs: Vec<i32>,
  value_count: i64,
  leaf_count: usize,
  leaf_cardinality: usize,
  // for asserts
  #[cfg(debug_assertions)]
  last_packed_value: Vec<u8>,
  #[cfg(debug_assertions)]
  last_doc_id: i32,
  bkd_writer: &'a mut BKDWriter<D, N>,
}

impl<'a, D, O, const N: usize> OneDimensionBKDWriter<'a, D, O, N>
where
  D: Directory,
  O: IndexOutput,
{
  pub fn new(data_out: &'a mut O, bkd_writer: &'a mut BKDWriter<D, N>) -> Result<Self> {
    if bkd_writer.config.num_index_dims != 1 {
      return Err(LuceneError::unsupported_operation(format!(
        "config.num_index_dims() must be 1 but got {}",
        bkd_writer.config.num_index_dims
      )));
    }
    if bkd_writer.point_count != 0 {
      return Err(LuceneError::illegal_state("cannot mix add and merge"));
    }

    // Catch user silliness:
    if bkd_writer.finished {
      return Err(LuceneError::illegal_state("already finished"));
    }

    // Mark that we already finished:
    bkd_writer.finished = true;

    let data_start_fp = data_out.get_file_pointer()?;
    let leaf_values = vec![
      0u8;
      bkd_writer.config.max_points_in_leaf_node
        * bkd_writer.config.packed_bytes_length()
    ];
    let leaf_docs = vec![0i32; bkd_writer.config.max_points_in_leaf_node];
    #[cfg(debug_assertions)]
    let last_packed_value = vec![0u8; bkd_writer.config.packed_bytes_length()];

    Ok(OneDimensionBKDWriter {
      data_out,
      data_start_fp,
      leaf_block_fps: Vec::new(),
      leaf_block_start_values: Vec::new(),
      leaf_values,
      leaf_docs,
      value_count: 0,
      leaf_count: 0,
      leaf_cardinality: 0,
      #[cfg(debug_assertions)]
      last_packed_value,
      #[cfg(debug_assertions)]
      last_doc_id: 0,
      bkd_writer,
    })
  }
  pub fn add(&mut self, packed_value: &[u8], doc_id: i32) -> Result<()> {
    #[cfg(debug_assertions)]
    debug_assert!(value_in_order(
      self.bkd_writer.config.clone(),
      self.value_count + self.leaf_count as i64,
      0,
      self.last_packed_value.as_mut_slice(),
      packed_value,
      0,
      doc_id,
      self.last_doc_id
    ));

    if self.leaf_count == 0
      || !self.bkd_writer.equals_predicate.test(
        &self.leaf_values,
        (self.leaf_count - 1) * self.bkd_writer.config.bytes_per_dim,
        packed_value,
        0,
      )
    {
      self.leaf_cardinality += 1;
    }

    let offset = self.leaf_count * self.bkd_writer.config.packed_bytes_length();
    let length = self.bkd_writer.config.packed_bytes_length();
    self.leaf_values.copy_from(&packed_value[0..length], offset);

    self.leaf_docs[self.leaf_count] = doc_id;
    self.bkd_writer.docs_seen.set(doc_id as usize)?;
    self.leaf_count += 1;

    if self.value_count + self.leaf_count as i64 > self.bkd_writer.total_point_count {
      return Err(LuceneError::illegal_state(format!(
        "totalPointCount={} was passed when we were created, but we just hit {} values",
        self.bkd_writer.total_point_count,
        self.value_count + self.leaf_count as i64
      )));
    }

    if self.leaf_count == self.bkd_writer.config.max_points_in_leaf_node {
      self.write_leaf_block(self.leaf_cardinality)?;
      self.leaf_cardinality = 0;
      self.leaf_count = 0;
    }

    debug_assert!(doc_id >= 0);
    #[cfg(debug_assertions)]
    {
      self.last_doc_id = doc_id;
    }

    Ok(())
  }
  pub fn finish(&mut self) -> Result<Option<IORunnable>> {
    if self.leaf_count > 0 {
      self.write_leaf_block(self.leaf_cardinality)?;
      self.leaf_cardinality = 0;
      self.leaf_count = 0;
    }

    if self.value_count == 0 {
      return Ok(None);
    }
    self.bkd_writer.point_count = self.value_count;

    debug_assert!(
      self.leaf_block_start_values.len() / self.bkd_writer.config.packed_index_bytes_length() + 1
        == self.leaf_block_fps.len()
    );

    let num_leaves = self.leaf_block_fps.len();
    let leaf_nodes = BKDTreeLeafNodesEnum::OneDimension(BKDTreeLeafNodesOneDimension {
      leaf_block_fps: std::mem::take(&mut self.leaf_block_fps),
      offset: 0,
      length: self.bkd_writer.config.bytes_per_dim,
      leaf_block_start_values: std::mem::take(&mut self.leaf_block_start_values),
    });
    Ok(Option::from(IORunnable {
      leaf_nodes,
      num_leaves,
      count_per_leaf: self.bkd_writer.config.max_points_in_leaf_node,
      data_start_fp: self.data_start_fp,
    }))
  }
  fn write_leaf_block(&mut self, leaf_cardinality: usize) -> Result<()> {
    debug_assert!(self.leaf_count != 0);
    let packed_index_bytes_length = self.bkd_writer.config.packed_index_bytes_length();
    if self.value_count == 0 {
      self
        .bkd_writer
        .min_packed_value
        .copy_from(&self.leaf_values[0..packed_index_bytes_length], 0);
    }
    let start = (self.leaf_count - 1) * packed_index_bytes_length;
    self.bkd_writer.max_packed_value.copy_from(
      &self.leaf_values[start..start + packed_index_bytes_length],
      0,
    );
    self.value_count += self.leaf_count as i64;

    if !self.leaf_block_fps.is_empty() {
      // Save the first (minimum) value in each leaf block except the
      // first, to build the split value index in the end:
      self
        .leaf_block_start_values
        .extend_from_slice(&self.leaf_values[0..packed_index_bytes_length]);
    }
    self.leaf_block_fps.push(self.data_out.get_file_pointer()?);
    self
      .bkd_writer
      .check_max_leaf_node_count(self.leaf_block_fps.len())?;

    // Find per-dim common prefix:
    self.bkd_writer.common_prefix_lengths[0] = self
      .bkd_writer
      .common_prefix_comparator
      .compare(
        &self.leaf_values,
        0,
        &self.leaf_values,
        (self.leaf_count - 1) * packed_index_bytes_length,
      )
      .try_convert()?;

    self
      .bkd_writer
      .write_leaf_block_docs(self.data_out, &self.leaf_docs, 0, self.leaf_count)?;
    self.bkd_writer.write_common_prefixes(
      self.data_out,
      &self.bkd_writer.common_prefix_lengths,
      &self.leaf_values,
    )?;

    self.bkd_writer.scratch_bytes_ref1.length = self.bkd_writer.config.packed_index_bytes_length();
    self.bkd_writer.scratch_bytes_ref1.bytes = Vec::new();

    let length = self.bkd_writer.scratch_bytes_ref1.length;
    let packed_bytes_length = self.bkd_writer.config.packed_bytes_length();

    let mut packed_values = PackedValuesImpl1 {
      scratch_bytes_ref_byte: &self.leaf_values,
      packed_bytes_length,
      length,
    };
    debug_assert!(values_in_order_and_bounds(
      self.bkd_writer.config.clone(),
      self.leaf_count,
      0,
      &self.leaf_values[0..packed_index_bytes_length],
      &self.leaf_values[((self.leaf_count - 1) * self.bkd_writer.config.packed_index_bytes_length())
        ..((self.leaf_count) * self.bkd_writer.config.packed_index_bytes_length())],
      &mut packed_values,
      &self.leaf_docs,
      0
    )?);

    self.bkd_writer.write_leaf_block_packed_values(
      self.data_out,
      self.leaf_count,
      0,
      &mut packed_values,
      leaf_cardinality,
    )?;
    Ok(())
  }
}
// only called from assert
#[allow(clippy::too_many_arguments)]
fn values_in_order_and_bounds<T>(
  config: BKDConfig,
  count: usize,
  sorted_dim: usize,
  min_packed_value: &[u8],
  max_packed_value: &[u8],
  values: &mut T,
  docs: &[i32],
  docs_offset: usize,
) -> Result<bool>
where
  T: PackedValues,
{
  let mut last_packed_value = vec![0u8; config.packed_bytes_length()];
  let mut last_doc = -1;
  for i in 0..count {
    let value = values.get_value(i)?;
    let bytes_ref = value.bytes;
    let offset = value.offset;
    let length = value.length;
    let bytes = bytes_ref;
    debug_assert_eq!(length, config.packed_bytes_length());
    debug_assert!(value_in_order(
      config.clone(),
      i as i64,
      sorted_dim,
      &mut last_packed_value,
      bytes,
      offset,
      docs[docs_offset + i],
      last_doc
    ));
    last_doc = docs[docs_offset + i];
    // Make sure this value does in fact fall within this leaf cell:
    debug_assert!(value_in_bounds(
      config.clone(),
      bytes,
      offset,
      min_packed_value,
      max_packed_value
    ));
  }
  Ok(true)
}

// only called from assert
#[allow(clippy::too_many_arguments)]
fn value_in_order(
  config: BKDConfig,
  ord: i64,
  sorted_dim: usize,
  last_packed_value: &mut [u8],
  packed_value: &[u8],
  packed_value_offset: usize,
  doc: i32,
  last_doc: i32,
) -> bool {
  let dim_offset = sorted_dim * config.bytes_per_dim;
  if ord > 0 {
    let mut cmp = last_packed_value[dim_offset..dim_offset + config.bytes_per_dim]
      .cmp(
        &packed_value[packed_value_offset + dim_offset
          ..(packed_value_offset + config.bytes_per_dim) + dim_offset],
      )
      .to_int();
    if cmp > 0 {
      debug_assert!(
        false,
        "values out of order: last value={:?} current value={:?} ord={}",
        BytesRef {
          bytes: &*last_packed_value,
          offset: 0,
          length: last_packed_value.len()
        },
        BytesRef {
          bytes: packed_value,
          offset: packed_value_offset,
          length: config.packed_index_bytes_length()
        },
        ord
      );
    }
    if cmp == 0 && config.num_dims > config.num_index_dims {
      cmp = last_packed_value[config.packed_index_bytes_length()..config.packed_bytes_length()]
        .cmp(
          &packed_value[(packed_value_offset + config.packed_index_bytes_length())
            ..(packed_value_offset + config.packed_bytes_length())],
        )
        .to_int();

      if cmp > 0 {
        debug_assert!(
          false,
          "data values out of order: last value={:?} current value={:?} ord={}",
          BytesRef {
            bytes: &*last_packed_value,
            offset: 0,
            length: last_packed_value.len()
          },
          BytesRef {
            bytes: packed_value,
            offset: packed_value_offset,
            length: config.packed_index_bytes_length()
          },
          ord
        );
      }
    }
    if cmp == 0 && doc < last_doc {
      debug_assert!(
        false,
        "docs out of order: last doc={last_doc} current doc={doc} ord={ord}"
      );
    }
  }
  last_packed_value.copy_from(
    &packed_value[packed_value_offset..(packed_value_offset + config.packed_bytes_length())],
    0,
  );
  true
}

// only called from assert
fn value_in_bounds(
  config: BKDConfig,
  bytes: &[u8],
  bytes_offset: usize,
  min_packed_value: &[u8],
  max_packed_value: &[u8],
) -> bool {
  for dim in 0..config.num_index_dims {
    let offset = config.bytes_per_dim * dim;
    let start = bytes_offset + offset;
    let end = start + config.bytes_per_dim;
    if bytes[start..end]
      .cmp(&min_packed_value[offset..offset + config.bytes_per_dim])
      .to_int()
      < 0
    {
      return false;
    }
    if bytes[start..end]
      .cmp(&max_packed_value[offset..offset + config.bytes_per_dim])
      .to_int()
      > 0
    {
      return false;
    }
  }
  true
}
struct MergeReader<PT, DM>
where
  PT: PointTree,
  DM: Fn(i32) -> Result<i32>,
{
  point_tree: Option<PT>,
  packed_bytes_length: usize,
  doc_map: DM,
  merge_intersects_visitor: MergeIntersectsVisitor,
  doc_block_upto: usize,
  doc_id: i32,
  packed_value_offset: usize,
}

impl<PT, DM> MergeReader<PT, DM>
where
  PT: PointTree,
  DM: Fn(i32) -> Result<i32>,
{
  fn new<S>(point_values: &mut S, doc_map: DM) -> Result<Self>
  where
    S: PointValues<PointTree = PT>,
  {
    let packed_bytes_length =
      point_values.get_bytes_per_dimension()? * point_values.get_num_dimensions()?;
    let mut point_tree = point_values.get_point_tree()?;
    let mut merge_intersects_visitor = MergeIntersectsVisitor::new(packed_bytes_length);
    // Move to first child of the tree and collect docs
    while point_tree.move_to_child()? {}

    point_tree.visit_doc_values(&mut merge_intersects_visitor)?;

    Ok(Self {
      point_tree: Some(point_tree),
      packed_bytes_length,
      doc_map,
      merge_intersects_visitor,
      doc_block_upto: 0,
      doc_id: -1,
      packed_value_offset: 0,
    })
  }
  pub fn next(&mut self) -> Result<bool> {
    loop {
      if self.doc_block_upto == self.merge_intersects_visitor.docs_in_block {
        if !self.collect_next_leaf()? {
          debug_assert!(self.merge_intersects_visitor.docs_in_block == 0);
          return Ok(false);
        }
        debug_assert!(self.merge_intersects_visitor.docs_in_block > 0);
        self.doc_block_upto = 0;
      }

      let index = self.doc_block_upto;
      self.doc_block_upto += 1;
      let old_doc_id = self.merge_intersects_visitor.doc_ids[index];

      let mapped_doc_id = (self.doc_map)(old_doc_id)?;

      if mapped_doc_id != -1 {
        // Not deleted!
        self.doc_id = mapped_doc_id;
        let start = index * self.packed_bytes_length;
        let end = start + self.packed_bytes_length;
        // Validate the current value before making its position available to the merge queue.
        let _ = &self.merge_intersects_visitor.packed_values[start..end];
        self.packed_value_offset = start;
        return Ok(true);
      }
    }
  }

  fn collect_next_leaf(&mut self) -> Result<bool> {
    match self.point_tree {
      Some(ref mut point_tree) => {
        debug_assert!(!point_tree.move_to_child()?);
        self.merge_intersects_visitor.reset();
        loop {
          if point_tree.move_to_sibling()? {
            // Move to first child of this node and collect docs
            while point_tree.move_to_child()? {}
            point_tree.visit_doc_values(&mut self.merge_intersects_visitor)?;
            return Ok(true);
          }
          if !point_tree.move_to_parent()? {
            break;
          }
        }
      },
      None => {
        return Ok(false);
      },
    }

    Ok(false)
  }
}

struct MergeIntersectsVisitor {
  docs_in_block: usize,
  packed_values: Vec<u8>,
  doc_ids: Vec<i32>,
  packed_bytes_length: usize,
}

impl MergeIntersectsVisitor {
  fn new(packed_bytes_length: usize) -> Self {
    Self {
      docs_in_block: 0,
      packed_values: vec![0u8; packed_bytes_length],
      doc_ids: Vec::new(),
      packed_bytes_length,
    }
  }
  fn reset(&mut self) {
    self.docs_in_block = 0;
  }
}
impl IntersectVisitor for MergeIntersectsVisitor {
  fn visit(&mut self, _doc_id: i32) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
    self.packed_values.copy_from(
      &packed_value[..self.packed_bytes_length],
      self.docs_in_block * self.packed_bytes_length,
    );
    self.doc_ids[self.docs_in_block] = doc_id;
    self.docs_in_block += 1;
    Ok(())
  }

  fn compare(&self, _min_packed_value: &[u8], _max_packed_value: &[u8]) -> Result<Relation> {
    Ok(Relation::CellCrossesQuery)
  }

  fn grow(&mut self, count: usize) -> Result<()> {
    debug_assert_eq!(self.docs_in_block, 0);
    if self.doc_ids.len() < count {
      ArrayUtil::grow_no_copy(&mut self.doc_ids, count, || 0)?;
      let packed_values_size: i32 =
        (self.doc_ids.len() * self.packed_bytes_length).try_convert()?;
      // TODO:
      // if packed_values_size > ArrayUtil::MAX_ARRAY_LENGTH {
      //     return Err(LuceneError::illegal_state(format!(
      //         "array length must be <= {} but was: {}",
      //         ArrayUtil::MAX_ARRAY_LENGTH,
      //         packed_values_size
      //     )));
      // }
      ArrayUtil::grow_no_copy(&mut self.packed_values, packed_values_size as usize, || 0)?;
    }
    Ok(())
  }
}

struct MergeReaderCmp<const N: usize> {
  comparator: BKDBytes<N>,
}
impl<const N: usize> MergeReaderCmp<N> {
  fn new(bytes_per_dim: usize) -> Self {
    Self {
      comparator: BKDBytes::new(bytes_per_dim),
    }
  }
}
impl<PT, DM, const N: usize> Compare<Box<MergeReader<PT, DM>>> for MergeReaderCmp<N>
where
  PT: PointTree,
  DM: Fn(i32) -> Result<i32>,
{
  fn less_than(&self, a: &Box<MergeReader<PT, DM>>, b: &Box<MergeReader<PT, DM>>) -> Result<bool> {
    debug_assert!(!std::ptr::eq(a, b));
    let cmp = self.comparator.compare(
      &a.merge_intersects_visitor.packed_values,
      a.packed_value_offset,
      &b.merge_intersects_visitor.packed_values,
      b.packed_value_offset,
    );

    if cmp < 0 {
      Ok(true)
    } else if cmp > 0 {
      Ok(false)
    } else {
      // Tie break by sorting smaller docIDs earlier:
      Ok(a.doc_id < b.doc_id)
    }
  }
}

/// flat representation of a kd-tree
trait BKDTreeLeafNodes {
  /// number of leaf nodes
  fn num_leaves(&self) -> usize;

  /// pointer to the leaf node previously written. Leaves are order from left
  /// to right, so leaf at `index` 0 is the leftmost leaf and the leaf at
  /// `num_leaves()` - 1 is the rightmost
  fn get_leaf_lp(&self, index: usize) -> usize;

  /// split value between two leaves. The split value at position n
  /// corresponds to the leaves at (n -1) and n.
  fn get_split_value(&self, index: usize) -> BytesRef<&[u8]>;

  /// split dimension between two leaves. The split dimension at position n
  /// corresponds to the leaves at (n -1) and n.
  fn get_split_dimension(&self, index: usize) -> usize;
}
struct BKDTreeLeafNodesOneDimension {
  leaf_block_start_values: Vec<u8>,
  offset: usize,
  length: usize,
  leaf_block_fps: Vec<usize>,
}
impl BKDTreeLeafNodes for BKDTreeLeafNodesOneDimension {
  fn num_leaves(&self) -> usize {
    self.leaf_block_fps.len()
  }

  fn get_leaf_lp(&self, index: usize) -> usize {
    self.leaf_block_fps[index]
  }

  fn get_split_value(&self, index: usize) -> BytesRef<&[u8]> {
    BytesRef {
      bytes: &self.leaf_block_start_values[index * self.length..(index + 1) * self.length],
      offset: self.offset,
      length: self.length,
    }
  }

  fn get_split_dimension(&self, _index: usize) -> usize {
    0
  }
}
struct BKDTreeLeafNodesImpl {
  scratch_bytes_ref1: BytesRef<Vec<u8>>,
  leaf_block_fps: Vec<usize>,
  split_dimension_values: smallvec::SmallVec<[u8; 16]>,
  bytes_per_dim: usize,
}
impl BKDTreeLeafNodes for BKDTreeLeafNodesImpl {
  fn num_leaves(&self) -> usize {
    self.leaf_block_fps.len()
  }

  fn get_leaf_lp(&self, index: usize) -> usize {
    self.leaf_block_fps[index]
  }

  fn get_split_value(&self, index: usize) -> BytesRef<&[u8]> {
    BytesRef {
      bytes: self.scratch_bytes_ref1.bytes.as_slice(),
      offset: index * self.bytes_per_dim,
      length: self.bytes_per_dim,
    }
  }

  fn get_split_dimension(&self, index: usize) -> usize {
    self.split_dimension_values[index] as usize
  }
}

enum BKDTreeLeafNodesEnum {
  OneDimension(BKDTreeLeafNodesOneDimension),
  MultiDimensions(BKDTreeLeafNodesImpl),
}
pub struct IORunnable {
  leaf_nodes: BKDTreeLeafNodesEnum,
  num_leaves: usize,
  count_per_leaf: usize,
  data_start_fp: usize,
}

struct IntersectVisitorImpl<'a, D, O, const N: usize>
where
  D: Directory,
  O: IndexOutput,
{
  pub one_dim_writer: OneDimensionBKDWriter<'a, D, O, N>,
}
impl<D, O, const N: usize> IntersectVisitor for IntersectVisitorImpl<'_, D, O, N>
where
  D: Directory,
  O: IndexOutput,
{
  fn visit(&mut self, _doc_id: i32) -> Result<()> {
    Err(LuceneError::illegal_argument(""))
  }

  fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
    self.one_dim_writer.add(packed_value, doc_id)
  }

  fn compare(&self, _min_packed_value: &[u8], _max_packed_value: &[u8]) -> Result<Relation> {
    Ok(Relation::CellCrossesQuery)
  }
}

trait PackedValues {
  fn get_value(&mut self, i: usize) -> Result<BytesRef<&[u8]>>;
}
struct PackedValuesImpl1<'a> {
  scratch_bytes_ref_byte: &'a [u8],
  packed_bytes_length: usize,
  length: usize,
}
impl PackedValues for PackedValuesImpl1<'_> {
  fn get_value(&mut self, i: usize) -> Result<BytesRef<&[u8]>> {
    Ok(BytesRef {
      bytes: self.scratch_bytes_ref_byte,
      offset: self.packed_bytes_length * i,
      length: self.length,
    })
  }
}

struct PackedValuesImpl2<'a, M>
where
  M: MutablePointTree,
{
  scratch: BytesRef<Vec<u8>>,
  reader: &'a M,
  from: usize,
}
impl<M> PackedValues for PackedValuesImpl2<'_, M>
where
  M: MutablePointTree,
{
  fn get_value(&mut self, i: usize) -> Result<BytesRef<&[u8]>> {
    self.reader.get_value(i + self.from, &mut self.scratch)
  }
}
struct PackedValuesImpl3<'a, const N: usize> {
  heap_source: &'a mut HeapPointWriter<N>,
  from: usize,
}
impl<const N: usize> PackedValues for PackedValuesImpl3<'_, N> {
  fn get_value(&mut self, i: usize) -> Result<BytesRef<&[u8]>> {
    Ok(
      self
        .heap_source
        .get_packed_value_slice(self.from + i)?
        .packed_value(),
    )
  }
}

pub const CODEC_NAME: &str = "BKD";
pub const VERSION_START: i32 = 4; // version used by Lucene 7.0
// pub const VERSION_CURRENT: i32 = VERSION_START;
pub const VERSION_LEAF_STORES_BOUNDS: i32 = 5;
pub const VERSION_SELECTIVE_INDEXING: i32 = 6;
pub const VERSION_LOW_CARDINALITY_LEAVES: i32 = 7;
pub const VERSION_META_FILE: i32 = 9;
pub const VERSION_CURRENT: i32 = VERSION_META_FILE;
/// Number of splits before we compute the exact bounding box of an inner
/// node.
const SPLITS_BEFORE_EXACT_BOUNDS: i32 = 4;
/// Default maximum heap to use, before spilling to (slower) disk.
pub const DEFAULT_MAX_MB_SORT_IN_HEAP: f32 = 16.0;
