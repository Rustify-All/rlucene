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
use crate::core::index::merge_state::{DocMap, DocMapEnum};
use crate::core::index::point_values::{
    IntersectVisitor, PointTree, PointValues, PointValuesBase, Relation,
};
use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::store::directory::Directory;
use crate::core::store::tracking_directory_wrapper::TrackingDirectoryWrapper;
use crate::core::store::{ByteBuffersDataOutput, DataOutput, IndexOutput};
use crate::core::util::array_util::{ArrayUtil, ByteArrayComparator, ByteArrayComparatorEnum};
use crate::core::util::bit_set::BitSet;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bkd::bkd_config::BKDConfig;
use crate::core::util::bkd::bkd_radix_selector::{BKDRadixSelector, PathSlice};
use crate::core::util::bkd::bkd_util::{BKDUtil, ByteArrayPredicate, ByteArrayPredicateEnum};
use crate::core::util::bkd::doc_ids_writer::DocIdsWriter;
use crate::core::util::bkd::heap_point_write::HeapPointWriter;
use crate::core::util::bkd::mutable_point_tree_reader_utils::MutablePointTreeReaderUtils;
use crate::core::util::bkd::offline_point_write::OfflinePointWriter;
use crate::core::util::bkd::point_reader::PointReader;
use crate::core::util::bkd::point_value::PointValue;
use crate::core::util::bkd::point_writer::{PointWriter, PointWriterEnum};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::numeric_utils::NumericUtils;
use crate::core::util::priority_queue::{Compare, PriorityQueue};
use crate::core::util::{IOUtils, SliceCopyOps, ToInt};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
// TODO
//   - allow variable length `byte[]` (across docs and dims), but this is quite
//     a bit more complex
//   - we could also index "auto-prefix terms" here, and use better compression,
//     and maybe only use for the "fully contained" case so we'd only index
//     docIDs
//   - the index could be efficiently encoded as an FST, so we don't have
//     wasteful (monotonic) `long[]` leafBlockFPs; or we could use
//     `MonotonicLongValues` ... but then the index is already quite small: 60M
//     OSM points --> 1.1 MB with 128 points per leaf, and you can reduce that
//     by putting more points per leaf
//   - we could use threads while building; the higher nodes are very
//     parallelizable

/// Recursively builds a block KD-tree to assign all incoming points in N-dim
/// space to smaller and smaller N-dim rectangles (cells) until the number of
/// points in a given rectangle is <= `config.max_points_in_leaf_node()`. The
/// tree is partially balanced, which means the leaf nodes will have the
/// requested `config.max_points_in_leaf_node()` except one that might have
/// less. Leaf nodes may straddle the two bottom levels of the binary tree.
/// Values that fall exactly on a cell boundary may be in either cell.
///
/// The number of dimensions can be 1 to 8, but every `byte[]` value is fixed
/// length.
///
/// This consumes heap during writing: it allocates a `Vec<i64>` (`num_leaves`
/// size), a `Vec<u8>` (`num_leaves * (1 + config.bytes_per_dim)`) and then uses
/// up to the specified `max_mb_sort_in_heap` heap space for writing.
///
/// **NOTE**: This can write at most `i32::MAX * config.max_points_in_leaf_node
/// / config.bytes_per_dim` total points.
pub struct BKDWriter<D>
where
    D: Directory,
{
    config: Rc<BKDConfig>,
    comparator: ByteArrayComparatorEnum,
    common_prefix_comparator: ByteArrayComparatorEnum,
    temp_dir: TrackingDirectoryWrapper<D>,
    temp_file_name_prefix: String,

    max_mb_sort_in_heap: f64,
    scratch_diff: Vec<u8>,
    scratch: Vec<u8>,
    scratch_bytes_ref1: BytesRef<Vec<u8>>,
    scratch_bytes_ref2: BytesRef<Vec<u8>>,
    common_prefix_lengths: Vec<i32>,
    docs_seen: FixedBitSet,
    point_writer: Option<PointWriterEnum<<TrackingDirectoryWrapper<D> as Directory>::IndexOutput>>,
    finished: bool,
    max_points_sort_in_heap: i32,
    /// Minimum per-dim values, packed.
    min_packed_value: Vec<u8>,
    /// Maximum per-dim values, packed.
    max_packed_value: Vec<u8>,
    point_count: i64,
    /// An upper bound on how many points the caller will add (includes
    /// deletions).
    total_point_count: i64,
    equals_predicate: ByteArrayPredicateEnum,
    max_doc: i32,
    doc_ids_writer: DocIdsWriter,
}

impl<D> BKDWriter<D>
where
    D: Directory,
{
    pub fn new(
        max_doc: i32,
        temp_dir: Arc<D>,
        temp_file_name_prefix: &str,
        config: Rc<BKDConfig>,
        max_mb_sort_in_heap: f64,
        total_point_count: i64,
    ) -> Result<Self> {
        Self::verify_params(max_mb_sort_in_heap, total_point_count)?;

        let bytes_per_dim = config.bytes_per_dim as usize;
        let packed_bytes_length = config.packed_bytes_length() as usize;
        let packed_index_bytes_length = config.packed_index_bytes_length() as usize;
        let temp_dir = TrackingDirectoryWrapper::new(temp_dir);
        let comparator = ArrayUtil::get_unsigned_comparator(bytes_per_dim);
        let equals_predicate = BKDUtil::get_equals_predicate(bytes_per_dim);
        let common_prefix_comparator = BKDUtil::get_prefix_length_comparator(bytes_per_dim);
        let docs_seen = FixedBitSet::new(max_doc);

        let scratch_diff = vec![0u8; config.bytes_per_dim as usize];
        let scratch = vec![0u8; packed_bytes_length];
        let common_prefix_lengths = vec![0; config.num_dims as usize];

        let min_packed_value = vec![0u8; packed_index_bytes_length];
        let max_packed_value = vec![0u8; packed_index_bytes_length];

        // Maximum number of points we hold in memory at any time
        let max_points_sort_in_heap =
            ((max_mb_sort_in_heap * 1024.0 * 1024.0) / config.bytes_per_doc() as f64) as i32;
        let doc_ids_writer = DocIdsWriter::new(config.max_points_in_leaf_node);
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
            temp_file_name_prefix: temp_file_name_prefix.to_string(),
            max_mb_sort_in_heap,
            scratch_diff,
            scratch,
            scratch_bytes_ref1: BytesRef::default(),
            scratch_bytes_ref2: BytesRef::default(),
            common_prefix_lengths,
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
            self.point_writer = Some(PointWriterEnum::Heap(HeapPointWriter::new(
                self.config.clone(),
                self.total_point_count.try_into()?,
            )));
        }

        Ok(())
    }
    pub fn add(&mut self, packed_value: &[u8], doc_id: i32) -> Result<()> {
        if packed_value.len() != self.config.packed_bytes_length() as usize {
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
            let length = self.config.packed_index_bytes_length() as usize;
            self.min_packed_value.copy_from(&packed_value[..length], 0);
            self.max_packed_value.copy_from(&packed_value[..length], 0);
        } else {
            let bytes_per_dim = self.config.bytes_per_dim as usize;
            for dim in 0..self.config.num_index_dims as usize {
                let offset = dim * bytes_per_dim;
                if self
                    .comparator
                    .compare(packed_value, offset, &self.min_packed_value, offset)
                    < 0
                {
                    self.min_packed_value
                        .copy_from(&packed_value[offset..offset + bytes_per_dim], offset);
                } else if self.comparator.compare(
                    packed_value,
                    offset,
                    &self.max_packed_value,
                    offset,
                ) > 0
                {
                    self.max_packed_value
                        .copy_from(&packed_value[offset..offset + bytes_per_dim], offset);
                }
            }
        }
        self.point_writer
            .as_mut()
            .unwrap()
            .append_bytes(packed_value, doc_id)?;
        self.point_count += 1;
        self.docs_seen.set(doc_id);
        Ok(())
    }
    /// Write a field from a `MutablePointTree`. This way of writing points is
    /// faster than regular writes with `BKDWriter::add` since there is
    /// opportunity for reordering points before writing them to disk. This
    /// method does not use transient disk in order to reorder points.
    pub fn write_field<M>(
        &mut self,
        data_out: Rc<RefCell<D::IndexOutput>>,
        reader: Rc<RefCell<M>>,
        filename: &str,
    ) -> Result<Option<IORunnable>>
    where
        M: MutablePointTree,
    {
        if self.config.num_dims == 1 {
            self.write_field_1dim(data_out, filename, reader)
        } else {
            self.write_field_n_dims(data_out, reader)
        }
    }

    fn compute_packed_value_bounds_with_tree<M>(
        &mut self,
        values: &M,
        from: i32,
        to: i32,
        min_packed_value: &mut [u8],
        max_packed_value: &mut [u8],
    ) -> Result<()>
    where
        M: MutablePointTree,
    {
        if from == to {
            return Ok(());
        }
        values.get_value(from as usize, &mut self.scratch_bytes_ref1);
        min_packed_value.copy_from(
            &self.scratch_bytes_ref1.bytes[self.scratch_bytes_ref1.offset
                ..self.scratch_bytes_ref1.offset
                    + self.config.packed_index_bytes_length() as usize],
            0,
        );
        max_packed_value.copy_from(
            &self.scratch_bytes_ref1.bytes[self.scratch_bytes_ref1.offset
                ..self.scratch_bytes_ref1.offset
                    + self.config.packed_index_bytes_length() as usize],
            0,
        );

        for i in from + 1..to {
            values.get_value(i as usize, &mut self.scratch_bytes_ref1);
            let offset = self.scratch_bytes_ref1.offset;
            for dim in 0..self.config.num_index_dims {
                let start_offset = (dim * self.config.bytes_per_dim) as usize;
                let end_offset = start_offset + self.config.bytes_per_dim as usize;

                if self.scratch_bytes_ref1.bytes[offset + start_offset..offset + end_offset]
                    .cmp(&min_packed_value[start_offset..end_offset])
                    .to_int()
                    < 0
                {
                    min_packed_value.copy_from(
                        &self.scratch_bytes_ref1.bytes[offset + start_offset..offset + end_offset],
                        start_offset,
                    );
                } else if self.scratch_bytes_ref1.bytes[offset + start_offset..offset + end_offset]
                    .cmp(&max_packed_value[start_offset..end_offset])
                    .to_int()
                    > 0
                {
                    max_packed_value.copy_from(
                        &self.scratch_bytes_ref1.bytes[offset + start_offset..offset + end_offset],
                        start_offset,
                    );
                }
            }
        }

        Ok(())
    }
    /// In the 2+D case, we recursively pick the split dimension, compute the
    /// median value and partition other values around it.
    pub fn write_field_n_dims<M>(
        &mut self,
        data_out: Rc<RefCell<D::IndexOutput>>,
        values: Rc<RefCell<M>>,
    ) -> Result<Option<IORunnable>>
    where
        M: MutablePointTree,
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

        self.point_count = values.borrow().size()?;

        if self.point_count == 0 {
            return Ok(None);
        }

        let num_leaves = ((self.point_count + self.config.max_points_in_leaf_node as i64 - 1)
            / self.config.max_points_in_leaf_node as i64)
            .try_into()?;
        let num_splits = num_leaves - 1;

        self.check_max_leaf_node_count(num_leaves as usize)?;

        let mut split_packed_values = vec![0u8; (num_splits * self.config.bytes_per_dim) as usize];
        let mut split_dimension_values = vec![0u8; num_splits as usize];
        let mut leaf_block_fps = vec![0i64; num_leaves as usize];

        let point_count = self.point_count.try_into()?;
        let mut min_packed_value = vec![0u8; self.min_packed_value.len()];
        let mut max_packed_value = vec![0u8; self.max_packed_value.len()];
        // Compute the min/max for this slice
        self.compute_packed_value_bounds_with_tree(
            &*values.borrow(),
            0,
            point_count,
            &mut min_packed_value,
            &mut max_packed_value,
        )?;

        {
            let values_b = values.borrow();
            for i in 0..self.point_count as i32 {
                self.docs_seen.set(values_b.get_doc_id(i as usize));
            }
        }

        let data_start_fp = data_out.borrow().get_file_pointer();
        let mut parent_splits = vec![0i32; self.config.num_index_dims as usize];

        self.build_with_reader(
            0,
            num_leaves,
            values,
            0,
            self.point_count as i32,
            &mut *data_out.borrow_mut(),
            &mut min_packed_value,
            &mut max_packed_value,
            &mut parent_splits,
            &mut split_packed_values,
            &mut split_dimension_values,
            &mut leaf_block_fps,
            &mut vec![0; self.config.max_points_in_leaf_node as usize],
        )?;

        debug_assert!(
            parent_splits.iter().all(|&x| x == 0),
            "parent_splits should be all zeros at the end"
        );

        let split_packed_values =
            BytesRef::from_slice(split_packed_values, self.config.bytes_per_dim as usize, 0);

        self.make_writer(
            split_packed_values,
            split_dimension_values,
            leaf_block_fps,
            data_start_fp,
        )
    }

    /// In the 1D case, we can simply sort points in ascending order and use the
    /// same writing logic as we use at merge time.
    fn write_field_1dim<M>(
        &mut self,
        data_out: Rc<RefCell<D::IndexOutput>>,
        _field_name: &str,
        reader: Rc<RefCell<M>>,
    ) -> Result<Option<IORunnable>>
    where
        M: MutablePointTree,
    {
        let mut reader = reader.borrow_mut();
        let size = reader.size()?.try_into()?;
        MutablePointTreeReaderUtils::sort(&self.config, self.max_doc, &mut *reader, 0, size)?;

        let one_dim_writer = OneDimensionBKDWriter::new(data_out, self)?;
        let mut intersect_visitor = IntersectVisitorImpl { one_dim_writer };
        reader.visit_doc_values(&mut intersect_visitor)?;
        intersect_visitor.one_dim_writer.finish()
    }
    /// More efficient bulk-add for incoming `PointValuesBase`s.
    /// This does a merge sort of the already sorted values and currently only
    /// works when num_dims==1. This returns `None` if all documents
    /// containing dimensional values were deleted.
    pub fn merge<S>(
        &mut self,
        _meta_out: Rc<RefCell<D::IndexOutput>>,
        _index_out: Rc<RefCell<D::IndexOutput>>,
        data_out: Rc<RefCell<D::IndexOutput>>,
        doc_maps: Option<Vec<Rc<DocMapEnum>>>,
        readers: Vec<PointValues<S>>,
    ) -> Result<Option<IORunnable>>
    where
        S: PointValuesBase,
    {
        let readers_len = readers.len();
        debug_assert!(doc_maps.is_none() || readers_len == doc_maps.as_ref().unwrap().len());
        debug_assert!(readers_len <= i32::MAX as usize);
        let mut queue = PriorityQueue::new(
            readers_len as i32,
            MergeReaderCmp::new(self.config.bytes_per_dim as usize),
        )?;

        for (i, mut point_values) in readers.into_iter().enumerate() {
            debug_assert_eq!(point_values.get_num_dimensions()?, self.config.num_dims);
            assert_eq!(
                point_values.get_bytes_per_dimension()?,
                self.config.bytes_per_dim
            );
            debug_assert_eq!(
                point_values.get_num_index_dimensions()?,
                self.config.num_index_dims
            );
            let doc_map = doc_maps.as_ref().map(|doc_maps| doc_maps[i].clone());
            let mut reader = MergeReader::new(&mut point_values, doc_map)?;
            if reader.next()? {
                queue.add(reader);
            }
        }

        let mut one_dim_writer = OneDimensionBKDWriter::new(data_out.clone(), self)?;

        while queue.size() != 0 {
            let reader = queue.top_mut();
            one_dim_writer.add(&reader.packed_value, reader.doc_id)?;

            if reader.next()? {
                queue.update_top();
            } else {
                // This segment was exhausted
                queue.pop();
            }
        }

        one_dim_writer.finish()
    }
    fn get_num_left_leaf_nodes(&self, num_leaves: i32) -> i32 {
        debug_assert!(
            num_leaves > 1,
            "get_num_left_leaf_nodes() called with {num_leaves}"
        );
        // return the level that can be filled with this number of leaves
        let last_full_level = 31 - num_leaves.leading_zeros() as i32;
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
        if (self.config.bytes_per_dim as u64) * (num_leaves as u64)
            > ArrayUtil::MAX_ARRAY_LENGTH as u64
        {
            return Err(LuceneError::illegal_state(format!(
                "too many nodes; increase config.maxPointsInLeafNode() (currently {}) and reindex",
                self.config.max_points_in_leaf_node
            )));
        }
        Ok(())
    }
    /// Writes the BKD tree to the provided `IndexOutput`s and returns an
    /// `IORunnable` that writes the index of the tree if at least one point
    /// has been added, or `None` otherwise.
    pub fn finish(
        &mut self,
        data_out: Rc<RefCell<<TrackingDirectoryWrapper<D> as Directory>::IndexOutput>>,
    ) -> Result<Option<IORunnable>> {
        if self.finished {
            return Err(LuceneError::illegal_state("already finished"));
        }

        if self.point_count == 0 {
            return Ok(None);
        }

        // Mark as finished
        self.finished = true;

        self.point_writer.as_mut().unwrap().close();
        let mut points = PathSlice::new(
            Rc::new(RefCell::new(self.point_writer.take().unwrap())),
            0,
            self.point_count,
        );

        let max_points_in_leaf_node = self.config.max_points_in_leaf_node as i64;
        let num_leaves = ((self.point_count + max_points_in_leaf_node - 1)
            / max_points_in_leaf_node)
            .try_into()?;
        let num_splits = num_leaves - 1;

        debug_assert!(num_leaves >= 0);
        self.check_max_leaf_node_count(num_leaves as usize)?;

        // NOTE: we could save the `1+` here, to use a bit less heap at search
        // time, but then we'd need a somewhat costly check at each step
        // of the recursion to recompute the split dim:

        // Indexed by nodeID, but first (root) nodeID is 1.
        // We do `1+` because the lead byte at each recursion says which dim we
        // split on.
        let mut split_packed_values = vec![0u8; (num_splits * self.config.bytes_per_dim) as usize];
        let mut split_dimension_values = vec![0u8; num_splits as usize];

        // +1 because leaf count is power of 2 (e.g. 8), and innerNodeCount is
        // power of 2 minus 1 (e.g. 7)
        let mut leaf_block_fps = vec![0i64; num_leaves as usize];

        // Make sure the math above "worked":
        debug_assert!(
            self.point_count / num_leaves as i64 <= self.config.max_points_in_leaf_node as i64,
            "point_count={} numLeaves={} config.maxPointsInLeafNode()={}",
            self.point_count,
            num_leaves,
            self.config.max_points_in_leaf_node
        );

        // We re-use the selector so we do not need to create an object every
        // time.
        let mut radix_selector = BKDRadixSelector::new(
            self.config.clone(),
            self.max_points_sort_in_heap,
            &self.temp_file_name_prefix,
        );

        let data_start_fp = data_out.borrow().get_file_pointer();

        let result = (|| -> Result<()> {
            let mut parent_splits = vec![0i32; self.config.num_index_dims as usize];
            self.build(
                0,
                num_leaves,
                &mut points,
                &mut *data_out.borrow_mut(),
                &mut radix_selector,
                &mut self.min_packed_value.clone(),
                &mut self.max_packed_value.clone(),
                &mut parent_splits,
                &mut split_packed_values,
                &mut split_dimension_values,
                &mut leaf_block_fps,
                &mut vec![0; self.config.max_points_in_leaf_node as usize],
            )?;

            debug_assert!(
                parent_splits.iter().all(|&x| x == 0),
                "parentSplits should be all zeros at the end"
            );
            // If no exception, we should have cleaned everything up:
            debug_assert!(
                self.temp_dir
                    .get_created_files()
                    .lock()
                    .created_filenames
                    .is_empty(),
                "Temp directory should be empty"
            );
            Ok(())
        })();
        match result {
            Ok(_) => {},
            Err(e) => {
                IOUtils::delete_files_ignoring_exceptions(
                    &self.temp_dir,
                    &self.temp_dir.get_created_files().lock().created_filenames,
                );
                return Err(e);
            },
        }

        self.scratch_bytes_ref1.bytes = split_packed_values.clone();
        self.scratch_bytes_ref1.length = self.config.bytes_per_dim as usize;
        let split_packed_values =
            BytesRef::from_slice(split_packed_values, self.config.bytes_per_dim as usize, 0);

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
        split_dimension_values: Vec<u8>,
        leaf_block_fps: Vec<i64>,
        data_start_fp: i64,
    ) -> Result<Option<IORunnable>> {
        let leaf_nodes = BKDTreeLeafNodesEnum::MultiDimensions(BKDTreeLeafNodesImpl {
            scratch_bytes_ref1: split_packed_values,
            leaf_block_fps,
            split_dimension_values,
            bytes_per_dim: self.config.bytes_per_dim,
        });
        Ok(Option::from(IORunnable {
            leaf_nodes,
            count_per_leaf: self.config.max_points_in_leaf_node,
            data_start_fp,
        }))
    }
    /// Packs the two arrays, representing a semi-balanced binary tree, into a
    /// compact byte[] structure.
    fn pack_index(&self, leaf_nodes: &BKDTreeLeafNodesEnum) -> Result<Vec<u8>> {
        // Reused while packing the index
        let mut write_buffer = ByteBuffersDataOutput::new_resettable_instance();

        // This is the "file" we append the byte[] to:
        let mut blocks: Vec<Option<Vec<u8>>> = Vec::new();
        let mut last_split_values =
            vec![0u8; (self.config.bytes_per_dim * self.config.num_index_dims) as usize];

        let mut negative_deltas = vec![false; self.config.num_index_dims as usize];
        let total_size = self.recurse_pack_index(
            &mut write_buffer,
            leaf_nodes,
            0,
            &mut blocks,
            &mut last_split_values,
            &mut negative_deltas,
            false,
            0,
            leaf_nodes.num_leaves(),
        )?;

        // Compact the byte[] blocks into single byte index:
        let mut index = vec![0u8; total_size as usize];
        let mut upto = 0;
        for block in &blocks {
            debug_assert!(block.is_some());
            let block = block.as_ref().unwrap();
            let block_len = block.len();
            index.copy_from(block, upto);
            upto += block_len;
        }

        debug_assert!(upto == total_size as usize);

        Ok(index)
    }

    /// Appends the current contents of writeBuffer as another block on the
    /// growing in-memory file.
    fn append_block(
        &self,
        write_buffer: &mut ByteBuffersDataOutput,
        blocks: &mut Vec<Option<Vec<u8>>>,
    ) -> i32 {
        let block = write_buffer.try_get_array_ownership();
        debug_assert!(blocks.len() <= i32::MAX as usize);
        let block_len = block.len() as i32;
        blocks.push(Option::from(block));
        write_buffer.reset();
        block_len
    }
    /// lastSplitValues is per-dimension split value previously seen; we use
    /// this to prefix-code the split byte[] on each inner node
    #[allow(clippy::too_many_arguments)]
    fn recurse_pack_index(
        &self,
        write_buffer: &mut ByteBuffersDataOutput,
        leaf_nodes: &BKDTreeLeafNodesEnum,
        min_block_fp: i64,
        blocks: &mut Vec<Option<Vec<u8>>>,
        last_split_values: &mut [u8],
        negative_deltas: &mut [bool],
        is_left: bool,
        leaves_offset: i32,
        num_leaves: i32,
    ) -> Result<i32> {
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
                write_buffer.write_vlong(delta)?;
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
                write_buffer.write_vlong(delta)?;
            }

            let num_left_leaf_nodes = self.get_num_left_leaf_nodes(num_leaves);
            let right_offset = leaves_offset + num_left_leaf_nodes;
            let split_offset = right_offset - 1;

            let split_dim = leaf_nodes.get_split_dimension(split_offset);
            let (split_bytes, split_offset, _) = leaf_nodes.get_split_value(split_offset);
            let address = split_offset;

            let prefix = self.common_prefix_comparator.compare(
                split_bytes,
                address as usize,
                last_split_values,
                (split_dim * self.config.bytes_per_dim) as usize,
            );

            let first_diff_byte_delta = if prefix < self.config.bytes_per_dim {
                let diff = (split_bytes[(address + prefix) as usize] as i32)
                    - (last_split_values[(split_dim * self.config.bytes_per_dim + prefix) as usize]
                        as i32);
                if negative_deltas[split_dim as usize] {
                    debug_assert!(diff < 0);
                    -diff
                } else {
                    debug_assert!(diff > 0);
                    diff
                }
            } else {
                0
            };
            // pack the prefix, splitDim and delta first diff byte into a single
            // vInt:
            let code = (first_diff_byte_delta * (1 + self.config.bytes_per_dim) + prefix)
                * self.config.num_index_dims
                + split_dim;

            write_buffer.write_vint(code)?;

            let suffix = self.config.bytes_per_dim - prefix;
            let mut sav_split_value = vec![0u8; suffix as usize];

            if suffix > 1 {
                write_buffer.write_bytes_range(split_bytes, address + prefix + 1, suffix - 1)?;
            }

            let cmp = last_split_values.to_vec();
            let last_split_values_start = (split_dim * self.config.bytes_per_dim + prefix) as usize;
            sav_split_value.copy_from(
                &last_split_values
                    [last_split_values_start..last_split_values_start + suffix as usize],
                0,
            );
            // copy our split value into lastSplitValues for our children to
            // prefix-code against
            let split_bytes_start = (address + prefix) as usize;
            last_split_values.copy_from(
                &split_bytes[split_bytes_start..split_bytes_start + suffix as usize],
                last_split_values_start,
            );

            let num_bytes = self.append_block(write_buffer, blocks);

            let idx_sav = blocks.len();
            blocks.push(None);

            let sav_negative_delta = negative_deltas[split_dim as usize];
            negative_deltas[split_dim as usize] = true;

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

            if num_left_leaf_nodes != 1 {
                write_buffer.write_vint(left_num_bytes)?;
            } else {
                debug_assert!(left_num_bytes == 0, "leftNumBytes={left_num_bytes}");
            }

            let bytes2 = write_buffer.try_get_array_ownership();
            let bytes2_len = bytes2.len();
            debug_assert!(bytes2_len <= i32::MAX as usize);
            write_buffer.reset();
            // replace our placeholder:
            blocks[idx_sav] = Some(bytes2);

            negative_deltas[split_dim as usize] = false;
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

            negative_deltas[split_dim as usize] = sav_negative_delta;

            let start = (split_dim * self.config.bytes_per_dim + prefix) as usize;
            last_split_values.copy_from(&sav_split_value[..suffix as usize], start);

            debug_assert!(last_split_values == &cmp[..]);

            Ok(num_bytes + bytes2_len as i32 + left_num_bytes + right_num_bytes)
        }
    }
    pub fn write_index(
        &self,
        meta_out: Rc<RefCell<D::IndexOutput>>,
        index_out: Rc<RefCell<D::IndexOutput>>,
        data: &IORunnable,
    ) -> Result<()> {
        let packed_index = self.pack_index(&data.leaf_nodes)?;
        self.write_index_with_packed_index(meta_out, index_out, &packed_index, data)
    }
    pub fn write_index_with_packed_index(
        &self,
        meta_out: Rc<RefCell<D::IndexOutput>>,
        index_out: Rc<RefCell<D::IndexOutput>>,
        packed_index: &[u8],
        data: &IORunnable,
    ) -> Result<()> {
        // If metaOut and indexOut are the same file, we account for the fact
        // that writing a long makes the index start 8 bytes later.
        let index_start_offset = if Rc::ptr_eq(&meta_out, &index_out) {
            BitUtil::LONG_BYTES as i64
        } else {
            0
        };
        let packed_index_len = packed_index.len() as i32;
        {
            let mut meta_out = meta_out.borrow_mut();
            CodecUtil::write_header(&mut *meta_out, CODEC_NAME, VERSION_CURRENT)?;
            meta_out.write_vint(self.config.num_dims)?;
            meta_out.write_vint(self.config.num_index_dims)?;
            meta_out.write_vint(data.count_per_leaf)?;
            meta_out.write_vint(self.config.bytes_per_dim)?;

            let num_leaves = data.leaf_nodes.num_leaves();
            debug_assert!(num_leaves > 0);
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
            meta_out.write_vint(self.docs_seen.cardinality())?;
            meta_out.write_vint(packed_index_len)?;
            meta_out.write_long(data.data_start_fp)?;
        }
        let file_pointer;
        {
            let index_out = index_out.borrow_mut();
            file_pointer = index_out.get_file_pointer();
        }
        meta_out
            .borrow_mut()
            .write_long(file_pointer + index_start_offset)?;
        index_out
            .borrow_mut()
            .write_bytes_range(packed_index, 0, packed_index_len)?;

        Ok(())
    }
    fn write_leaf_block_docs(
        &mut self,
        out: &mut impl IndexOutput,
        doc_ids: &[i32],
        start: i32,
        count: i32,
    ) -> Result<()> {
        debug_assert!(
            count > 0,
            "config.max_points_in_leaf_node()={}",
            self.config.max_points_in_leaf_node
        );
        out.write_vint(count)?;
        self.doc_ids_writer
            .write_doc_ids(doc_ids, start, count, out)?;
        Ok(())
    }
    fn write_leaf_block_packed_values(
        &mut self,
        out: &mut impl DataOutput,
        count: i32,
        sorted_dim: i32,
        packed_values: &mut impl PackedValues,
        leaf_cardinality: i32,
    ) -> Result<()> {
        let prefix_len_sum: i32 = self.common_prefix_lengths.iter().sum();
        if prefix_len_sum == self.config.packed_bytes_length() {
            // all values in this block are equal
            out.write_byte(-1i8 as u8)?;
        } else {
            debug_assert!(
                self.common_prefix_lengths[sorted_dim as usize] < self.config.bytes_per_dim
            );

            // estimate if storing the values with cardinality is cheaper than
            // storing all values.
            let compressed_byte_offset = (sorted_dim * self.config.bytes_per_dim
                + self.common_prefix_lengths[sorted_dim as usize])
                as usize;

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
                let high_cardinality_cost = count
                    * (self.config.packed_bytes_length() - prefix_len_sum - 1)
                    + 2 * num_run_lens;

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
    fn write_low_cardinality_leaf_block_packed_values(
        &mut self,
        out: &mut impl DataOutput,
        count: i32,
        packed_values: &mut impl PackedValues,
    ) -> Result<()> {
        if self.config.num_index_dims != 1 {
            self.write_actual_bounds(out, count, packed_values)?;
        }

        let (bytes_ref, offset, _) = packed_values.get_value(0)?;
        self.scratch.copy_from(
            &bytes_ref[offset as usize..(offset + self.config.packed_bytes_length()) as usize],
            0,
        );

        let mut cardinality = 1;
        for i in 1..count {
            let (bytes_ref, offset, _) = packed_values.get_value(i)?;
            for dim in 0..self.config.num_dims {
                let start = (dim * self.config.bytes_per_dim) as usize;
                if !self.equals_predicate.test(
                    bytes_ref,
                    offset as usize + start,
                    &self.scratch,
                    start,
                ) {
                    out.write_vint(cardinality)?;
                    for j in 0..self.config.num_dims {
                        out.write_bytes_range(
                            &self.scratch,
                            j * self.config.bytes_per_dim + self.common_prefix_lengths[j as usize],
                            self.config.bytes_per_dim - self.common_prefix_lengths[j as usize],
                        )?;
                    }
                    self.scratch.copy_from(
                        &bytes_ref[offset as usize
                            ..(offset + self.config.packed_bytes_length()) as usize],
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
                i * self.config.bytes_per_dim + self.common_prefix_lengths[i as usize],
                self.config.bytes_per_dim - self.common_prefix_lengths[i as usize],
            )?;
        }

        Ok(())
    }
    fn write_high_cardinality_leaf_block_packed_values(
        &mut self,
        out: &mut impl DataOutput,
        count: i32,
        sorted_dim: i32,
        packed_values: &mut impl PackedValues,
        compressed_byte_offset: usize,
    ) -> Result<()> {
        if self.config.num_index_dims != 1 {
            self.write_actual_bounds(out, count, packed_values)?;
        }

        self.common_prefix_lengths[sorted_dim as usize] += 1;

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

            let (bytes_ref, offset, _) = packed_values.get_value(i)?;
            let prefix_byte = bytes_ref[offset as usize + compressed_byte_offset];

            out.write_byte(prefix_byte)?;
            out.write_byte(run_len as u8)?;

            self.write_leaf_block_packed_values_range(out, i, i + run_len, packed_values)?;
            i += run_len;
            debug_assert!(i <= count);
        }

        Ok(())
    }
    fn write_actual_bounds(
        &self,
        out: &mut impl DataOutput,
        count: i32,
        packed_values: &mut impl PackedValues,
    ) -> Result<()> {
        for dim in 0..self.config.num_index_dims {
            let common_prefix_length = self.common_prefix_lengths[dim as usize];
            let suffix_length = self.config.bytes_per_dim - common_prefix_length;

            if suffix_length > 0 {
                let (min, max) = self.compute_min_max(
                    count,
                    packed_values,
                    dim * self.config.bytes_per_dim + common_prefix_length,
                    suffix_length,
                )?;

                out.write_bytes_range(&min.bytes, min.offset as i32, min.length as i32)?;
                out.write_bytes_range(&max.bytes, max.offset as i32, max.length as i32)?;
            }
        }
        Ok(())
    }
    /// Return an array that contains the min and max values for the [offset,
    ///
    /// offset+length] interval of the given {@link BytesRef}s.
    #[allow(clippy::type_complexity)]
    fn compute_min_max(
        &self,
        count: i32,
        packed_values: &mut impl PackedValues,
        offset: i32,
        length: i32,
    ) -> Result<(BytesRef<Vec<u8>>, BytesRef<Vec<u8>>)> {
        debug_assert!(length > 0);
        let (bytes_ref, first_offset, _first_length) = packed_values.get_value(0)?;
        let mut min: BytesRefBuilder<Vec<u8>> = BytesRefBuilder::new();
        let mut max: BytesRefBuilder<Vec<u8>> = BytesRefBuilder::new();
        let bytes = bytes_ref;
        min.copy_bytes_with_vec(bytes, (first_offset + offset) as usize, length as usize);
        max.copy_bytes_with_vec(bytes, (first_offset + offset) as usize, length as usize);

        let length_usize = length as usize;
        let offset_usize = offset as usize;
        for i in 1..count {
            let (bytes_ref, candidate_offset, _candidate_length) = packed_values.get_value(i)?;
            let candidate_offset_usize = candidate_offset as usize;
            let candidate_bytes = bytes_ref;
            if min.bytes_ref().bytes[0..length_usize]
                .cmp(
                    &candidate_bytes[candidate_offset_usize + offset_usize
                        ..candidate_offset_usize + offset_usize + length_usize],
                )
                .to_int()
                > 0
            {
                min.copy_bytes_with_vec(
                    candidate_bytes,
                    (candidate_offset + offset) as usize,
                    length as usize,
                );
            } else if max.bytes_ref().bytes[0..length_usize]
                .cmp(
                    &candidate_bytes[candidate_offset_usize + offset_usize
                        ..candidate_offset_usize + offset_usize + length_usize],
                )
                .to_int()
                < 0
            {
                max.copy_bytes_with_vec(
                    candidate_bytes,
                    (candidate_offset + offset) as usize,
                    length as usize,
                )
            }
        }
        Ok((min.get_bytes_owner(), max.get_bytes_owner()))
    }
    fn write_leaf_block_packed_values_range(
        &self,
        out: &mut impl DataOutput,
        start: i32,
        end: i32,
        packed_values: &mut impl PackedValues,
    ) -> Result<()> {
        for i in start..end {
            let (bytes_ref, offset, length) = packed_values.get_value(i)?;
            debug_assert!(length == self.config.packed_bytes_length());

            for dim in 0..self.config.num_dims {
                let prefix = self.common_prefix_lengths[dim as usize];
                out.write_bytes_range(
                    bytes_ref,
                    offset + (dim * self.config.bytes_per_dim) + prefix,
                    self.config.bytes_per_dim - prefix,
                )?;
            }
        }
        Ok(())
    }

    fn run_len(
        packed_values: &mut impl PackedValues,
        start: i32,
        end: i32,
        byte_offset: usize,
    ) -> Result<i32> {
        let (bytes_ref, offset, _) = packed_values.get_value(start)?;
        let b = bytes_ref[offset as usize + byte_offset];
        for i in (start + 1)..end {
            let (bytes_ref, offset, _) = packed_values.get_value(i)?;
            let b2 = bytes_ref[offset as usize + byte_offset];
            debug_assert!(b2 >= b);
            if b != b2 {
                return Ok(i - start);
            }
        }
        Ok(end - start)
    }
    fn write_common_prefixes(
        &self,
        out: &mut impl DataOutput,
        common_prefixes: &[i32],
        packed_value: &[u8],
    ) -> Result<()> {
        let num_dims = self.config.num_dims as usize;
        for (dim, &prefix) in common_prefixes.iter().enumerate().take(num_dims) {
            out.write_vint(prefix)?;
            out.write_bytes_range(packed_value, dim as i32 * self.config.bytes_per_dim, prefix)?;
        }
        Ok(())
    }

    /// Called on exception, to check whether the checksum is also corrupt in
    /// this source, and add that information (checksum matched or didn't)
    /// as a suppressed exception.
    fn verify_checksum(
        &self,
        prior_exception: LuceneError,
        writer: &PointWriterEnum<<TrackingDirectoryWrapper<D> as Directory>::IndexOutput>,
    ) -> Result<()> {
        // TODO: we could improve this, to always validate checksum as we recurse, if we shared left and
        // right reader after recursing to children, and possibly within recursed children,
        // since all together they make a single pass through the file.  But this is a sizable re-org,
        // and would mean leaving readers (IndexInputs) open for longer:
        if let PointWriterEnum::Offline(writer) = writer {
            // We are reading from a temp file; go verify the checksum:
            if self
                .temp_dir
                .get_created_files()
                .lock()
                .created_filenames
                .contains(&writer.name)
            {
                let mut input = self.temp_dir.open_checksum_input(&writer.name)?;
                return Err(CodecUtil::check_footer_with_error(
                    &mut input,
                    prior_exception,
                ));
            }
        }
        Err(prior_exception)
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
    ) -> Result<i32> {
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
            .take(self.config.num_index_dims as usize)
        {
            let offset = dim * self.config.bytes_per_dim as usize;
            if split_count < max_num_splits / 2
                && self
                    .comparator
                    .compare(min_packed_value, offset, max_packed_value, offset)
                    != 0
            {
                return Ok(dim as i32);
            }
        }

        // Find which dim has the largest span so we can split on it:
        let mut split_dim = -1;

        for dim in 0..self.config.num_index_dims {
            NumericUtils::subtract(
                self.config.bytes_per_dim,
                dim,
                max_packed_value,
                min_packed_value,
                &mut self.scratch_diff,
            )?;
            if split_dim == -1
                || self
                    .comparator
                    .compare(&self.scratch_diff, 0, &self.scratch, 0)
                    > 0
            {
                self.scratch
                    .copy_from(&self.scratch_diff[0..self.config.bytes_per_dim as usize], 0);
                split_dim = dim;
            }
        }
        Ok(split_dim)
    }
    /// Pull a partition back into heap once the point count is low enough while
    /// recursing.
    fn switch_to_heap(
        &mut self,
        source: &mut PointWriterEnum<<TrackingDirectoryWrapper<D> as Directory>::IndexOutput>,
    ) -> Result<PointWriterEnum<<TrackingDirectoryWrapper<D> as Directory>::IndexOutput>> {
        let source_count = source.count();
        let count = source_count.try_into()?;
        let mut reader = source.get_reader(0, source_count, &self.temp_dir)?;
        let mut writer = HeapPointWriter::new(self.config.clone(), count);

        let result: Result<_> = (|| {
            for _ in 0..count {
                let has_next = reader.next()?;
                debug_assert!(has_next);
                writer.append_point_value(reader.point_value())?;
            }
            writer.close();
            Ok(())
        })();
        source.take_data(reader.remove_points());
        if let Err(err) = result {
            return Err(self.verify_checksum(err, source).unwrap_err());
        }

        source.destroy(&self.temp_dir)?;
        Ok(PointWriterEnum::Heap(writer))
    }

    /// Recursively reorders the provided reader and writes the bkd-tree on the
    /// fly; this method is used when we are writing a new segment directly
    /// from IndexWriter's indexing buffer (MutablePointsReader).
    #[allow(clippy::too_many_arguments)]
    fn build_with_reader<M>(
        &mut self,
        leaves_offset: i32,
        num_leaves: i32,
        reader: Rc<RefCell<M>>,
        from: i32,
        to: i32,
        out: &mut impl IndexOutput,
        min_packed_value: &mut [u8],
        max_packed_value: &mut [u8],
        parent_splits: &mut [i32],
        split_packed_values: &mut [u8],
        split_dimension_values: &mut [u8],
        leaf_block_fps: &mut [i64],
        spare_doc_ids: &mut [i32],
    ) -> Result<()>
    where
        M: MutablePointTree,
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
                let mut reader_ref = reader.borrow_mut();
                reader_ref.get_value(from as usize, &mut self.scratch_bytes_ref1);
                for i in from + 1..to {
                    reader_ref.get_value(i as usize, &mut self.scratch_bytes_ref2);
                    for dim in 0..self.config.num_dims {
                        let offset = (dim * self.config.bytes_per_dim) as usize;
                        let dimension_prefix_length = self.common_prefix_lengths[dim as usize];
                        self.common_prefix_lengths[dim as usize] = self
                            .common_prefix_comparator
                            .compare(
                                &self.scratch_bytes_ref1.bytes,
                                self.scratch_bytes_ref1.offset + offset,
                                &self.scratch_bytes_ref2.bytes,
                                self.scratch_bytes_ref2.offset + offset,
                            )
                            .min(dimension_prefix_length);
                    }
                }

                // Find the dimension that has the least number of unique bytes
                // at commonPrefixLengths[dim]
                let mut used_bytes = vec![None; self.config.num_dims as usize];
                for dim in 0..self.config.num_dims {
                    if self.common_prefix_lengths[dim as usize] < self.config.bytes_per_dim {
                        used_bytes[dim as usize] = Some(FixedBitSet::new(256));
                    }
                }
                for i in from + 1..to {
                    for dim in 0..self.config.num_dims {
                        if let Some(ref mut set) = used_bytes[dim as usize] {
                            let b = reader_ref.get_byte_at(
                                i as usize,
                                (dim * self.config.bytes_per_dim
                                    + self.common_prefix_lengths[dim as usize])
                                    as usize,
                            );
                            set.set(b as i32);
                        }
                    }
                }
                let mut sorted_dim_cardinality = i32::MAX;
                for dim in 0..self.config.num_dims {
                    if let Some(ref set) = used_bytes[dim as usize] {
                        let cardinality = set.cardinality();
                        if cardinality < sorted_dim_cardinality {
                            sorted_dim = dim;
                            sorted_dim_cardinality = cardinality;
                        }
                    }
                }

                // sort by sortedDim
                MutablePointTreeReaderUtils::sort_by_dim(
                    &self.config,
                    sorted_dim,
                    &self.common_prefix_lengths,
                    &mut *reader_ref,
                    from,
                    to,
                    &mut self.scratch_bytes_ref1,
                    &mut self.scratch_bytes_ref2,
                )?;

                let mut comparator = self.scratch_bytes_ref1.clone();
                let mut collector = self.scratch_bytes_ref2.clone();
                reader_ref.get_value(from as usize, &mut comparator);
                for i in from + 1..to {
                    reader_ref.get_value(i as usize, &mut collector);
                    for dim in 0..self.config.num_dims {
                        let start = (dim * self.config.bytes_per_dim) as usize;
                        if !self.equals_predicate.test(
                            &collector.bytes,
                            collector.offset + start,
                            &comparator.bytes,
                            comparator.offset + start,
                        ) {
                            leaf_cardinality += 1;
                            std::mem::swap(&mut collector, &mut comparator);
                            break;
                        }
                    }
                }

                // Save the block file pointer:
                leaf_block_fps[leaves_offset as usize] = out.get_file_pointer();

                // Write doc IDs
                for i in from..to {
                    spare_doc_ids[(i - from) as usize] = reader_ref.get_doc_id(i as usize);
                }
                self.write_leaf_block_docs(out, spare_doc_ids, 0, count)?;

                // Write the common prefixes:
                reader_ref.get_value(from as usize, &mut self.scratch_bytes_ref1);
                self.scratch.copy_from(
                    &self.scratch_bytes_ref1.bytes[self.scratch_bytes_ref1.offset
                        ..self.scratch_bytes_ref1.offset
                            + self.config.packed_bytes_length() as usize],
                    0,
                );
                self.write_common_prefixes(out, &self.common_prefix_lengths, &self.scratch)?;
            }
            // Write the full values:
            let mut packed_values = PackedValuesImpl2 {
                scratch: BytesRef::new(),
                reader: reader.clone(),
                from,
            };
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
            // inner node

            let split_dim = if self.config.num_index_dims == 1 {
                0
            } else {
                // for dimensions > 2 we recompute the bounds for the current
                // inner node to help the algorithm choose best
                // split dimensions. Because it is an expensive operation, the
                // frequency we recompute the bounds is given
                // by SPLITS_BEFORE_EXACT_BOUNDS.
                if num_leaves != leaf_block_fps.len() as i32
                    && self.config.num_index_dims > 2
                    && parent_splits.iter().sum::<i32>() % SPLITS_BEFORE_EXACT_BOUNDS == 0
                {
                    let reader_ref = reader.borrow();
                    self.compute_packed_value_bounds_with_tree(
                        &*reader_ref,
                        from,
                        to,
                        min_packed_value,
                        max_packed_value,
                    )?;
                }
                self.split(min_packed_value, max_packed_value, parent_splits)?
            };
            // How many leaves will be in the left tree:
            let num_left_leaf_nodes = self.get_num_left_leaf_nodes(num_leaves);
            // How many points will be in the left tree:
            let mid = from + num_left_leaf_nodes * self.config.max_points_in_leaf_node;

            let common_prefix_len = self.common_prefix_comparator.compare(
                min_packed_value,
                (split_dim * self.config.bytes_per_dim) as usize,
                max_packed_value,
                (split_dim * self.config.bytes_per_dim) as usize,
            );

            MutablePointTreeReaderUtils::partition(
                &self.config,
                self.max_doc,
                split_dim,
                common_prefix_len,
                reader.clone(),
                from,
                to,
                mid,
                &mut self.scratch_bytes_ref1,
                &mut self.scratch_bytes_ref2,
            )?;

            let right_offset = leaves_offset + num_left_leaf_nodes;
            let split_offset = right_offset - 1;
            let address = (split_offset * self.config.bytes_per_dim) as usize;
            split_dimension_values[split_offset as usize] = split_dim as u8;
            reader
                .borrow()
                .get_value(mid as usize, &mut self.scratch_bytes_ref1);
            let start =
                self.scratch_bytes_ref1.offset + (split_dim * self.config.bytes_per_dim) as usize;
            split_packed_values.copy_from(
                &self.scratch_bytes_ref1.bytes[start..start + self.config.bytes_per_dim as usize],
                address,
            );

            let mut min_split_packed_value = ArrayUtil::copy_of_sub_array(
                &self.min_packed_value,
                0,
                self.config.packed_index_bytes_length() as usize,
            );
            let mut max_split_packed_value = ArrayUtil::copy_of_sub_array(
                &self.max_packed_value,
                0,
                self.config.packed_index_bytes_length() as usize,
            );

            let start =
                self.scratch_bytes_ref1.offset + (split_dim * self.config.bytes_per_dim) as usize;
            let end = start + self.config.bytes_per_dim as usize;
            let offset = (split_dim * self.config.bytes_per_dim) as usize;
            min_split_packed_value.copy_from(&self.scratch_bytes_ref1.bytes[start..end], offset);
            max_split_packed_value.copy_from(&self.scratch_bytes_ref1.bytes[start..end], offset);
            // recurse
            parent_splits[split_dim as usize] += 1;
            self.build_with_reader(
                leaves_offset,
                num_left_leaf_nodes,
                reader.clone(),
                from,
                mid,
                out,
                min_packed_value,
                &mut max_split_packed_value,
                parent_splits,
                split_packed_values,
                split_dimension_values,
                leaf_block_fps,
                spare_doc_ids,
            )?;
            self.build_with_reader(
                right_offset,
                num_leaves - num_left_leaf_nodes,
                reader.clone(),
                mid,
                to,
                out,
                &mut min_split_packed_value,
                max_packed_value,
                parent_splits,
                split_packed_values,
                split_dimension_values,
                leaf_block_fps,
                spare_doc_ids,
            )?;
            parent_splits[split_dim as usize] -= 1;
        }

        Ok(())
    }

    fn compute_packed_value_bounds(
        &mut self,
        slice: &PathSlice<<TrackingDirectoryWrapper<D> as Directory>::IndexOutput>,
        min_packed_value: &mut [u8],
        max_packed_value: &mut [u8],
    ) -> Result<()> {
        let mut reader =
            slice
                .writer
                .borrow_mut()
                .get_reader(slice.start, slice.count, &self.temp_dir)?;

        if !reader.next()? {
            slice.writer.borrow_mut().take_data(reader.remove_points());
            return Ok(());
        }
        {
            let point_value = reader.point_value();
            let (value, offset, _length) = point_value.packed_value();
            min_packed_value.copy_from(
                &value
                    [offset as usize..(offset + self.config.packed_index_bytes_length()) as usize],
                0,
            );
            max_packed_value.copy_from(
                &value
                    [offset as usize..(offset + self.config.packed_index_bytes_length()) as usize],
                0,
            );
        }

        while reader.next()? {
            let point_value = reader.point_value();
            let (value, offset, _length) = point_value.packed_value();
            for dim in 0..self.config.num_index_dims {
                let start_offset = (dim * self.config.bytes_per_dim) as usize;
                if self.comparator.compare(
                    value,
                    offset as usize + start_offset,
                    min_packed_value,
                    start_offset,
                ) < 0
                {
                    min_packed_value.copy_from(
                        &value[offset as usize + start_offset
                            ..offset as usize + start_offset + self.config.bytes_per_dim as usize],
                        start_offset,
                    );
                } else if self.comparator.compare(
                    value,
                    offset as usize + start_offset,
                    max_packed_value,
                    start_offset,
                ) > 0
                {
                    max_packed_value.copy_from(
                        &value[offset as usize + start_offset
                            ..offset as usize + start_offset + self.config.bytes_per_dim as usize],
                        start_offset,
                    );
                }
            }
        }
        slice.writer.borrow_mut().take_data(reader.remove_points());

        Ok(())
    }
    /// The point writer contains the data that is going to be splitted using
    /// radix selection. /* This method is used when we are merging
    /// previously written segments, in the numDims > 1 case.
    #[allow(clippy::too_many_arguments)]
    fn build(
        &mut self,
        leaves_offset: i32,
        num_leaves: i32,
        points: &mut PathSlice<<TrackingDirectoryWrapper<D> as Directory>::IndexOutput>,
        out: &mut impl IndexOutput,
        radix_selector: &mut BKDRadixSelector,
        min_packed_value: &mut [u8],
        max_packed_value: &mut [u8],
        parent_splits: &mut [i32],
        split_packed_values: &mut [u8],
        split_dimension_values: &mut [u8],
        leaf_block_fps: &mut [i64],
        spare_doc_ids: &mut [i32],
    ) -> Result<()> {
        if num_leaves == 1 {
            let is_heap = {
                let writer = points.writer.borrow();
                matches!(*writer, PointWriterEnum::Heap(_))
            };
            let heap_source = if is_heap {
                points.writer.clone()
            } else {
                Rc::new(RefCell::new(
                    self.switch_to_heap(&mut *points.writer.borrow_mut())?,
                ))
            };

            let from = points.start as i32;
            let to = (points.start + points.count) as i32;

            let mut sorted_dim = 0;
            match &mut *heap_source.borrow_mut() {
                PointWriterEnum::Heap(heap_source) => {
                    self.compute_common_prefix_length(heap_source, from, to);
                    let mut sorted_dim_cardinality = i32::MAX;
                    let mut used_bytes = vec![None; self.config.num_dims as usize];

                    for dim in 0..self.config.num_dims {
                        if self.common_prefix_lengths[dim as usize] < self.config.bytes_per_dim {
                            used_bytes[dim as usize] = Some(FixedBitSet::new(256));
                        }
                    }

                    for dim in 0..self.config.num_dims {
                        let prefix = self.common_prefix_lengths[dim as usize];
                        if prefix < self.config.bytes_per_dim {
                            let offset = (dim * self.config.bytes_per_dim) as usize;
                            for i in from..to {
                                let (bytes, bytes_offset, _) =
                                    heap_source.get_packed_value_slice(i).packed_value();
                                let bucket = bytes[bytes_offset as usize + offset + prefix as usize]
                                    as usize;
                                used_bytes[dim as usize]
                                    .as_mut()
                                    .unwrap()
                                    .set(bucket as i32);
                            }
                            let cardinality =
                                used_bytes[dim as usize].as_ref().unwrap().cardinality();
                            if cardinality < sorted_dim_cardinality {
                                sorted_dim = dim;
                                sorted_dim_cardinality = cardinality;
                            }
                        }
                    }
                },
                _ => {
                    debug_assert!(false);
                },
            }
            let mut leaf_cardinality = 0;
            radix_selector.heap_radix_sort(
                heap_source.clone(),
                from,
                to,
                sorted_dim,
                self.common_prefix_lengths[sorted_dim as usize],
            )?;
            let count = to - from;
            match &mut *heap_source.borrow_mut() {
                PointWriterEnum::Heap(heap_source) => {
                    leaf_cardinality =
                        heap_source.compute_cardinality(from, to, &self.common_prefix_lengths);
                    leaf_block_fps[leaves_offset as usize] = out.get_file_pointer();

                    debug_assert!(count > 0);
                    debug_assert!(count <= spare_doc_ids.len() as i32);
                    for i in 0..count {
                        spare_doc_ids[i as usize] =
                            heap_source.get_packed_value_slice(from + i).doc_id();
                    }
                },
                _ => debug_assert!(false),
            };

            self.write_leaf_block_docs(out, spare_doc_ids, 0, count)?;

            self.write_common_prefixes(out, &self.common_prefix_lengths, &self.scratch)?;

            let mut packed_values = PackedValuesImpl3 {
                heap_source: heap_source.clone(),
                bytes: vec![],
                from,
            };
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
                if num_leaves != leaf_block_fps.len() as i32
                    && self.config.num_index_dims > 2
                    && parent_splits.iter().sum::<i32>() % SPLITS_BEFORE_EXACT_BOUNDS == 0
                {
                    self.compute_packed_value_bounds(points, min_packed_value, max_packed_value)?;
                }
                self.split(min_packed_value, max_packed_value, parent_splits)?
            };

            debug_assert!(num_leaves as usize <= leaf_block_fps.len());

            let num_left_leaf_nodes = self.get_num_left_leaf_nodes(num_leaves);
            let left_count =
                num_left_leaf_nodes as i64 * self.config.max_points_in_leaf_node as i64;

            let mut slices = Vec::with_capacity(2);

            let common_prefix_len = self.common_prefix_comparator.compare(
                min_packed_value,
                (split_dim * self.config.bytes_per_dim) as usize,
                max_packed_value,
                (split_dim * self.config.bytes_per_dim) as usize,
            );

            let split_value = radix_selector.select(
                points,
                &mut slices,
                points.start,
                points.start + points.count,
                points.start + left_count,
                split_dim,
                common_prefix_len,
                &self.temp_dir,
            )?;

            let right_offset = leaves_offset + num_left_leaf_nodes;
            let split_value_offset = right_offset - 1;

            split_dimension_values[split_value_offset as usize] = split_dim as u8;
            let address = (split_value_offset * self.config.bytes_per_dim) as usize;
            split_packed_values
                .copy_from(&split_value[0..self.config.bytes_per_dim as usize], address);

            let mut min_split_packed_value =
                vec![0u8; self.config.packed_index_bytes_length() as usize];
            min_split_packed_value.copy_from(
                &min_packed_value[0..self.config.packed_index_bytes_length() as usize],
                0,
            );
            let mut max_split_packed_value =
                vec![0u8; self.config.packed_index_bytes_length() as usize];
            max_split_packed_value.copy_from(
                &max_packed_value[0..self.config.packed_index_bytes_length() as usize],
                0,
            );

            let start = (split_dim * self.config.bytes_per_dim) as usize;
            min_split_packed_value
                .copy_from(&split_value[0..self.config.bytes_per_dim as usize], start);
            max_split_packed_value
                .copy_from(&split_value[0..self.config.bytes_per_dim as usize], start);

            parent_splits[split_dim as usize] += 1;

            self.build(
                leaves_offset,
                num_left_leaf_nodes,
                &mut slices[0],
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

            self.build(
                right_offset,
                num_leaves - num_left_leaf_nodes,
                &mut slices[1],
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

            parent_splits[split_dim as usize] -= 1;
        }

        Ok(())
    }

    fn compute_common_prefix_length(
        &mut self,
        heap_point_writer: &mut HeapPointWriter,
        from: i32,
        to: i32,
    ) {
        self.common_prefix_lengths.fill(self.config.bytes_per_dim);

        {
            let point_value = heap_point_writer.get_packed_value_slice(from);
            let (bytes, offset, _) = point_value.packed_value();

            for dim in 0..self.config.num_dims {
                let src_offset = (offset + dim * self.config.bytes_per_dim) as usize;
                let dst_offset = (dim * self.config.bytes_per_dim) as usize;
                self.scratch.copy_from(
                    &bytes[src_offset..src_offset + self.config.bytes_per_dim as usize],
                    dst_offset,
                );
            }
        }

        for i in from + 1..to {
            let point_value = heap_point_writer.get_packed_value_slice(i);
            let (bytes, offset, _) = point_value.packed_value();

            for dim in 0..self.config.num_dims {
                if self.common_prefix_lengths[dim as usize] != 0 {
                    self.common_prefix_lengths[dim as usize] = self.common_prefix_lengths
                        [dim as usize]
                        .min(self.common_prefix_comparator.compare(
                            &self.scratch,
                            (dim * self.config.bytes_per_dim) as usize,
                            bytes,
                            (offset + dim * self.config.bytes_per_dim) as usize,
                        ));
                }
            }
        }
    }
    pub fn close(&mut self) -> Result<()> {
        self.finished = true;
        if let Some(PointWriterEnum::Offline(ref mut offline_point_writer)) = self.point_writer
            && let Some(out) = offline_point_writer.out.take()
        {
            self.temp_dir.delete_file(out.get_name())?;
        }
        Ok(())
    }
}

pub struct OneDimensionBKDWriter<'a, D>
where
    D: Directory,
{
    data_out: Rc<RefCell<D::IndexOutput>>,
    data_start_fp: i64,
    leaf_block_fps: Vec<i64>,
    leaf_block_start_values: Vec<Vec<u8>>,
    leaf_values: Vec<u8>,
    leaf_docs: Vec<i32>,
    value_count: i64,
    leaf_count: i32,
    leaf_cardinality: i32,
    // for asserts
    last_packed_value: Vec<u8>,
    last_doc_id: i32,
    bkd_writer: &'a mut BKDWriter<D>,
}

impl<'a, D> OneDimensionBKDWriter<'a, D>
where
    D: Directory,
{
    pub fn new(
        data_out: Rc<RefCell<D::IndexOutput>>,
        bkd_writer: &'a mut BKDWriter<D>,
    ) -> Result<Self> {
        if bkd_writer.config.num_index_dims != 1 {
            return Err(LuceneError::unsupported_operation(format!(
                "config.numIndexDims() must be 1 but got {}",
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

        let data_start_fp = data_out.borrow().get_file_pointer();
        let leaf_values = vec![
            0u8;
            (bkd_writer.config.max_points_in_leaf_node * bkd_writer.config.packed_bytes_length())
                as usize
        ];
        let leaf_docs = vec![0i32; bkd_writer.config.max_points_in_leaf_node as usize];
        let last_packed_value = vec![0u8; bkd_writer.config.packed_bytes_length() as usize];

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
            last_packed_value,
            last_doc_id: 0,
            bkd_writer,
        })
    }
    pub fn add(&mut self, packed_value: &[u8], doc_id: i32) -> Result<()> {
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
                ((self.leaf_count - 1) * self.bkd_writer.config.bytes_per_dim) as usize,
                packed_value,
                0,
            )
        {
            self.leaf_cardinality += 1;
        }

        let offset = (self.leaf_count * self.bkd_writer.config.packed_bytes_length()) as usize;
        let length = self.bkd_writer.config.packed_bytes_length() as usize;
        self.leaf_values.copy_from(&packed_value[0..length], offset);

        self.leaf_docs[self.leaf_count as usize] = doc_id;
        // docsSeen.set(doc_id);
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
        self.last_doc_id = doc_id;

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
        // self.bkd_writer.scratch_bytes_ref1.length =
        // self.bkd_writer.config.bytes_per_dim; self.bkd_writer.
        // scratch_bytes_ref1.offset = 0;

        debug_assert!(self.leaf_block_start_values.len() + 1 == self.leaf_block_fps.len());

        let leaf_nodes = BKDTreeLeafNodesEnum::OneDimension(BKDTreeLeafNodesOneDimension {
            leaf_block_fps: std::mem::take(&mut self.leaf_block_fps),
            offset: 0,
            length: self.bkd_writer.config.bytes_per_dim,
            leaf_block_start_values: std::mem::take(&mut self.leaf_block_start_values),
        });
        Ok(Option::from(IORunnable {
            leaf_nodes,
            count_per_leaf: self.bkd_writer.config.max_points_in_leaf_node,
            data_start_fp: self.data_start_fp,
        }))
    }
    fn write_leaf_block(&mut self, leaf_cardinality: i32) -> Result<()> {
        debug_assert!(self.leaf_count != 0);
        let packed_index_bytes_length = self.bkd_writer.config.packed_index_bytes_length() as usize;
        if self.value_count == 0 {
            self.bkd_writer
                .min_packed_value
                .copy_from(&self.leaf_values[0..packed_index_bytes_length], 0);
        }
        let start = (self.leaf_count - 1) as usize * packed_index_bytes_length;
        self.bkd_writer.max_packed_value.copy_from(
            &self.leaf_values[start..start + packed_index_bytes_length],
            0,
        );
        self.value_count += self.leaf_count as i64;

        if !self.leaf_block_fps.is_empty() {
            // Save the first (minimum) value in each leaf block except the
            // first, to build the split value index in the end:
            self.leaf_block_start_values
                .push(self.leaf_values[0..packed_index_bytes_length].to_vec());
        }
        self.leaf_block_fps
            .push(self.data_out.borrow().get_file_pointer());
        self.bkd_writer
            .check_max_leaf_node_count(self.leaf_block_fps.len())?;

        // Find per-dim common prefix:
        self.bkd_writer.common_prefix_lengths[0] =
            self.bkd_writer.common_prefix_comparator.compare(
                &self.leaf_values,
                0,
                &self.leaf_values,
                (self.leaf_count - 1) as usize * packed_index_bytes_length,
            );

        self.bkd_writer.write_leaf_block_docs(
            &mut *self.data_out.borrow_mut(),
            &self.leaf_docs,
            0,
            self.leaf_count,
        )?;
        self.bkd_writer.write_common_prefixes(
            &mut *self.data_out.borrow_mut(),
            &self.bkd_writer.common_prefix_lengths,
            &self.leaf_values,
        )?;

        self.bkd_writer.scratch_bytes_ref1.length =
            self.bkd_writer.config.packed_index_bytes_length() as usize;
        self.bkd_writer.scratch_bytes_ref1.bytes = self.leaf_values.clone();

        let length = self.bkd_writer.scratch_bytes_ref1.length;
        let packed_bytes_length = self.bkd_writer.config.packed_bytes_length();

        let mut packed_values = PackedValuesImpl1 {
            scratch_bytes_ref_byte: std::mem::take(&mut self.bkd_writer.scratch_bytes_ref1.bytes),
            packed_bytes_length,
            length: length as i32,
        };
        debug_assert!(values_in_order_and_bounds(
            self.bkd_writer.config.clone(),
            self.leaf_count,
            0,
            &self.leaf_values[0..packed_index_bytes_length],
            &self.leaf_values[((self.leaf_count - 1)
                * self.bkd_writer.config.packed_index_bytes_length())
                as usize
                ..((self.leaf_count) * self.bkd_writer.config.packed_index_bytes_length())
                    as usize],
            &mut packed_values,
            &self.leaf_docs,
            0
        )?);

        self.bkd_writer.write_leaf_block_packed_values(
            &mut *self.data_out.borrow_mut(),
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
fn values_in_order_and_bounds(
    config: Rc<BKDConfig>,
    count: i32,
    sorted_dim: i32,
    min_packed_value: &[u8],
    max_packed_value: &[u8],
    values: &mut impl PackedValues,
    docs: &[i32],
    docs_offset: usize,
) -> Result<bool> {
    let mut last_packed_value = vec![0u8; config.packed_bytes_length() as usize];
    let mut last_doc = -1;
    for i in 0..count {
        let (bytes_ref, offset, length) = values.get_value(i)?;
        let bytes = bytes_ref;
        debug_assert_eq!(length, config.packed_bytes_length());
        debug_assert!(value_in_order(
            config.clone(),
            i as i64,
            sorted_dim,
            &mut last_packed_value,
            bytes,
            offset,
            docs[docs_offset + i as usize],
            last_doc
        ));
        last_doc = docs[docs_offset + i as usize];
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
    config: Rc<BKDConfig>,
    ord: i64,
    sorted_dim: i32,
    last_packed_value: &mut [u8],
    packed_value: &[u8],
    packed_value_offset: i32,
    doc: i32,
    last_doc: i32,
) -> bool {
    let dim_offset = (sorted_dim * config.bytes_per_dim) as usize;
    if ord > 0 {
        let mut cmp = last_packed_value[dim_offset..dim_offset + config.bytes_per_dim as usize]
            .cmp(
                &packed_value[packed_value_offset as usize + dim_offset
                    ..(packed_value_offset + config.bytes_per_dim) as usize + dim_offset],
            )
            .to_int();
        if cmp > 0 {
            debug_assert!(
                false,
                "values out of order: last value={:?} current value={:?} ord={}",
                BytesRef::from_bytes(last_packed_value.to_vec()),
                BytesRef::from_slice(
                    packed_value.to_vec(),
                    packed_value_offset as usize,
                    config.packed_index_bytes_length() as usize
                ),
                ord
            );
        }
        if cmp == 0 && config.num_dims > config.num_index_dims {
            cmp = last_packed_value[config.packed_index_bytes_length() as usize
                ..config.packed_bytes_length() as usize]
                .cmp(
                    &packed_value[(packed_value_offset + config.packed_index_bytes_length())
                        as usize
                        ..(packed_value_offset + config.packed_bytes_length()) as usize],
                )
                .to_int();

            if cmp > 0 {
                debug_assert!(
                    false,
                    "data values out of order: last value={:?} current value={:?} ord={}",
                    BytesRef::from_bytes(last_packed_value.to_vec()),
                    BytesRef::from_slice(
                        packed_value.to_vec(),
                        packed_value_offset as usize,
                        config.packed_index_bytes_length() as usize
                    ),
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
        &packed_value[packed_value_offset as usize
            ..(packed_value_offset + config.packed_bytes_length()) as usize],
        0,
    );
    true
}

// only called from assert
fn value_in_bounds(
    config: Rc<BKDConfig>,
    bytes: &[u8],
    bytes_offset: i32,
    min_packed_value: &[u8],
    max_packed_value: &[u8],
) -> bool {
    for dim in 0..config.num_index_dims {
        let offset = (config.bytes_per_dim * dim) as usize;
        let start = bytes_offset as usize + offset;
        let end = start + config.bytes_per_dim as usize;
        if bytes[start..end]
            .cmp(&min_packed_value[offset..offset + config.bytes_per_dim as usize])
            .to_int()
            < 0
        {
            return false;
        }
        if bytes[start..end]
            .cmp(&max_packed_value[offset..offset + config.bytes_per_dim as usize])
            .to_int()
            > 0
        {
            return false;
        }
    }
    true
}
struct MergeReader<S: PointValuesBase> {
    point_tree: Option<S::PointTree>,
    packed_bytes_length: usize,
    doc_map: Option<Rc<DocMapEnum>>,
    merge_intersects_visitor: MergeIntersectsVisitor,
    doc_block_upto: usize,
    doc_id: i32,
    packed_value: Vec<u8>,
}

impl<S: PointValuesBase> MergeReader<S> {
    fn new(point_values: &mut PointValues<S>, doc_map: Option<Rc<DocMapEnum>>) -> Result<Self> {
        let packed_bytes_length = (point_values.get_bytes_per_dimension()? as usize)
            * (point_values.get_num_dimensions()? as usize);
        let mut point_tree = point_values.get_point_tree()?;
        let mut merge_intersects_visitor = MergeIntersectsVisitor {
            docs_in_block: 0,
            packed_values: vec![0u8; packed_bytes_length],
            doc_ids: Vec::new(),
            packed_bytes_length: packed_bytes_length as i32,
        };

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
            packed_value: vec![0u8; packed_bytes_length],
        })
    }
    pub fn next(&mut self) -> Result<bool> {
        loop {
            if self.doc_block_upto == self.merge_intersects_visitor.docs_in_block as usize {
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

            let mapped_doc_id = if self.doc_map.is_none() {
                old_doc_id
            } else {
                self.doc_map.as_ref().unwrap().get(old_doc_id)
            };

            if mapped_doc_id != -1 {
                // Not deleted!
                self.doc_id = mapped_doc_id;
                let start = index * self.packed_bytes_length;
                let end = start + self.packed_bytes_length;
                self.packed_value
                    .copy_from(&self.merge_intersects_visitor.packed_values[start..end], 0);
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
impl<S: PointValuesBase> Default for MergeReader<S> {
    fn default() -> Self {
        Self {
            point_tree: None,
            packed_bytes_length: 0,
            doc_map: None,
            merge_intersects_visitor: MergeIntersectsVisitor::default(),
            doc_block_upto: 0,
            doc_id: -1,
            packed_value: Vec::new(),
        }
    }
}

#[derive(Default)]
struct MergeIntersectsVisitor {
    docs_in_block: i32,
    packed_values: Vec<u8>,
    doc_ids: Vec<i32>,
    packed_bytes_length: i32,
}

impl MergeIntersectsVisitor {
    fn new(packed_bytes_length: i32) -> Self {
        Self {
            docs_in_block: 0,
            doc_ids: Vec::new(),
            packed_values: Vec::new(),
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
            &packed_value[..self.packed_bytes_length as usize],
            (self.docs_in_block * self.packed_bytes_length) as usize,
        );
        self.doc_ids[self.docs_in_block as usize] = doc_id;
        self.docs_in_block += 1;
        Ok(())
    }

    fn compare(&mut self, _min_packed_value: &[u8], _max_packed_value: &[u8]) -> Result<Relation> {
        Ok(Relation::CellCrossesQuery)
    }

    fn grow(&mut self, count: i32) -> Result<()> {
        debug_assert_eq!(self.docs_in_block, 0);
        if self.doc_ids.len() < count as usize {
            ArrayUtil::grow_i32(&mut self.doc_ids, count as usize)?;
            let packed_values_size: i32 =
                (self.doc_ids.len() * self.packed_bytes_length as usize).try_into()?;
            // TODO:
            // if packed_values_size > ArrayUtil::MAX_ARRAY_LENGTH {
            //     return Err(LuceneError::illegal_state(format!(
            //         "array length must be <= {} but was: {}",
            //         ArrayUtil::MAX_ARRAY_LENGTH,
            //         packed_values_size
            //     )));
            // }
            ArrayUtil::grow_exact(&mut self.packed_values, packed_values_size as usize)?;
        }
        Ok(())
    }
}

struct MergeReaderCmp {
    comparator: ByteArrayComparatorEnum,
}
impl MergeReaderCmp {
    fn new(bytes_per_dim: usize) -> Self {
        Self {
            comparator: ArrayUtil::get_unsigned_comparator(bytes_per_dim),
        }
    }
}
#[allow(clippy::comparison_chain)]
impl<S> Compare<MergeReader<S>> for MergeReaderCmp
where
    S: PointValuesBase,
{
    fn less_than(&self, a: &MergeReader<S>, b: &MergeReader<S>) -> bool {
        debug_assert!(!std::ptr::eq(a, b));
        let cmp = self
            .comparator
            .compare(&a.packed_value, 0, &b.packed_value, 0);

        if cmp < 0 {
            true
        } else if cmp > 0 {
            false
        } else {
            // Tie break by sorting smaller docIDs earlier:
            a.doc_id < b.doc_id
        }
    }
}

/// flat representation of a kd-tree
trait BKDTreeLeafNodes {
    /// number of leaf nodes
    fn num_leaves(&self) -> i32;

    /// pointer to the leaf node previously written. Leaves are order from left
    /// to right, so leaf at `index` 0 is the leftmost leaf and the leaf at
    /// `num_leaves()` - 1 is the rightmost
    fn get_leaf_lp(&self, index: i32) -> i64;

    /// split value between two leaves. The split value at position n
    /// corresponds to the leaves at (n -1) and n.
    fn get_split_value(&self, index: i32) -> (&[u8], i32, i32);

    /// split dimension between two leaves. The split dimension at position n
    /// corresponds to the leaves at (n -1) and n.
    fn get_split_dimension(&self, index: i32) -> i32;
}
struct BKDTreeLeafNodesOneDimension {
    leaf_block_start_values: Vec<Vec<u8>>,
    offset: i32,
    length: i32,
    leaf_block_fps: Vec<i64>,
}
impl BKDTreeLeafNodes for BKDTreeLeafNodesOneDimension {
    fn num_leaves(&self) -> i32 {
        self.leaf_block_fps.len() as i32
    }

    fn get_leaf_lp(&self, index: i32) -> i64 {
        self.leaf_block_fps[index as usize]
    }

    fn get_split_value(&self, index: i32) -> (&[u8], i32, i32) {
        (
            self.leaf_block_start_values[index as usize].as_slice(),
            self.offset,
            self.length,
        )
    }

    fn get_split_dimension(&self, _index: i32) -> i32 {
        0
    }
}
struct BKDTreeLeafNodesImpl {
    scratch_bytes_ref1: BytesRef<Vec<u8>>,
    leaf_block_fps: Vec<i64>,
    split_dimension_values: Vec<u8>,
    bytes_per_dim: i32,
}
impl BKDTreeLeafNodes for BKDTreeLeafNodesImpl {
    fn num_leaves(&self) -> i32 {
        self.leaf_block_fps.len() as i32
    }

    fn get_leaf_lp(&self, index: i32) -> i64 {
        self.leaf_block_fps[index as usize]
    }

    fn get_split_value(&self, index: i32) -> (&[u8], i32, i32) {
        (
            &self.scratch_bytes_ref1.bytes,
            index * self.bytes_per_dim,
            self.scratch_bytes_ref1.length as i32,
        )
    }

    fn get_split_dimension(&self, index: i32) -> i32 {
        self.split_dimension_values[index as usize] as i32
    }
}

enum BKDTreeLeafNodesEnum {
    OneDimension(BKDTreeLeafNodesOneDimension),
    MultiDimensions(BKDTreeLeafNodesImpl),
}
impl BKDTreeLeafNodes for BKDTreeLeafNodesEnum {
    fn num_leaves(&self) -> i32 {
        match self {
            BKDTreeLeafNodesEnum::OneDimension(leaf) => leaf.num_leaves(),
            BKDTreeLeafNodesEnum::MultiDimensions(leaf) => leaf.num_leaves(),
        }
    }

    fn get_leaf_lp(&self, index: i32) -> i64 {
        match self {
            BKDTreeLeafNodesEnum::OneDimension(leaf) => leaf.get_leaf_lp(index),
            BKDTreeLeafNodesEnum::MultiDimensions(leaf) => leaf.get_leaf_lp(index),
        }
    }

    fn get_split_value(&self, index: i32) -> (&[u8], i32, i32) {
        match self {
            BKDTreeLeafNodesEnum::OneDimension(leaf) => leaf.get_split_value(index),
            BKDTreeLeafNodesEnum::MultiDimensions(leaf) => leaf.get_split_value(index),
        }
    }

    fn get_split_dimension(&self, index: i32) -> i32 {
        match self {
            BKDTreeLeafNodesEnum::OneDimension(leaf) => leaf.get_split_dimension(index),
            BKDTreeLeafNodesEnum::MultiDimensions(leaf) => leaf.get_split_dimension(index),
        }
    }
}

pub struct IORunnable {
    leaf_nodes: BKDTreeLeafNodesEnum,
    count_per_leaf: i32,
    data_start_fp: i64,
}

struct IntersectVisitorImpl<'a, D>
where
    D: Directory,
{
    pub one_dim_writer: OneDimensionBKDWriter<'a, D>,
}
impl<D> IntersectVisitor for IntersectVisitorImpl<'_, D>
where
    D: Directory,
{
    fn visit(&mut self, _doc_id: i32) -> Result<()> {
        Err(LuceneError::illegal_argument(""))
    }

    fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
        self.one_dim_writer.add(packed_value, doc_id)
    }

    fn compare(&mut self, _min_packed_value: &[u8], _max_packed_value: &[u8]) -> Result<Relation> {
        Ok(Relation::CellCrossesQuery)
    }
}
type PackedValueResult = Result<(Rc<RefCell<Vec<u8>>>, i32, i32)>;

trait PackedValues {
    fn get_value(&mut self, i: i32) -> Result<(&[u8], i32, i32)>;
}
struct PackedValuesImpl1 {
    scratch_bytes_ref_byte: Vec<u8>,
    packed_bytes_length: i32,
    length: i32,
}
impl PackedValues for PackedValuesImpl1 {
    fn get_value(&mut self, i: i32) -> Result<(&[u8], i32, i32)> {
        Ok((
            &self.scratch_bytes_ref_byte,
            self.packed_bytes_length * i,
            self.length,
        ))
    }
}

struct PackedValuesImpl2<M>
where
    M: MutablePointTree,
{
    scratch: BytesRef<Vec<u8>>,
    reader: Rc<RefCell<M>>,
    from: i32,
}
impl<M> PackedValues for PackedValuesImpl2<M>
where
    M: MutablePointTree,
{
    fn get_value(&mut self, i: i32) -> Result<(&[u8], i32, i32)> {
        self.reader
            .borrow()
            .get_value((i + self.from) as usize, &mut self.scratch);
        Ok((
            self.scratch.bytes.as_slice(),
            self.scratch.offset as i32,
            self.scratch.length as i32,
        ))
    }
}
struct PackedValuesImpl3<O>
where
    O: IndexOutput,
{
    heap_source: Rc<RefCell<PointWriterEnum<O>>>,
    bytes: Vec<u8>,
    from: i32,
}
impl<O> PackedValues for PackedValuesImpl3<O>
where
    O: IndexOutput,
{
    fn get_value(&mut self, i: i32) -> Result<(&[u8], i32, i32)> {
        match &mut *self.heap_source.borrow_mut() {
            PointWriterEnum::Heap(heap_source) => {
                let (v, offset, length) = heap_source
                    .get_packed_value_slice(self.from + i)
                    .packed_value();
                // TODO; could we avoid copy here
                self.bytes = v[offset as usize..(offset + length) as usize].to_vec();
                Ok((self.bytes.as_slice(), 0, length))
            },
            _ => Err(LuceneError::illegal_argument("heap_source should be Heap")),
        }
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
