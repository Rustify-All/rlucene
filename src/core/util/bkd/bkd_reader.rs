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
use std::cell::RefCell;
use std::rc::Rc;

use crate::core::codecs::CodecUtil;
use crate::core::index::BytesRef;
use crate::core::index::point_values::{IntersectVisitor, PointTree, PointValues, Relation};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::store::{DataInput, IndexInput};
use crate::core::util::SliceCopyOps;
use crate::core::util::array_util::{ArrayUtil, ByteArrayComparator};
use crate::core::util::bkd::bkd_config::BKDConfig;
use crate::core::util::bkd::bkd_writer::{
    CODEC_NAME, VERSION_CURRENT, VERSION_LEAF_STORES_BOUNDS, VERSION_LOW_CARDINALITY_LEAVES,
    VERSION_META_FILE, VERSION_SELECTIVE_INDEXING, VERSION_START,
};
use crate::core::util::bkd::doc_ids_writer::DocIdsWriter;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::math_util::MathUtil;

/// Handles reading a block KD-tree in byte[] space previously written with
/// `BKDWriter`
pub struct BKDReader<I>
where
    I: IndexInput,
{
    config: Rc<BKDConfig>,
    num_leaves: i32,
    index_in: Rc<RefCell<I>>,
    data_in: Rc<RefCell<I>>,
    min_packed_value: Vec<u8>,
    max_packed_value: Vec<u8>,
    point_count: i64,
    doc_count: i32,
    version: i32,
    #[allow(dead_code)]
    min_leaf_block_fp: i64,
    index_start_pointer: i64,
    num_index_bytes: i32,
    is_tree_balanced: bool,
}

impl<I: IndexInput> BKDReader<I>
where
    I: IndexInput,
{
    /// Caller must pre-seek the provided `IndexInput` to the index location
    /// that `BKDWriter::finish()` returned. BKD tree is always stored off-heap.
    pub fn new(
        meta_in: Rc<RefCell<I>>,
        index_in: Rc<RefCell<I>>,
        data_in: Rc<RefCell<I>>,
    ) -> Result<Self> {
        let meta_in = &mut *meta_in.borrow_mut();
        let version = CodecUtil::check_header(meta_in, CODEC_NAME, VERSION_START, VERSION_CURRENT)?;

        let num_dims = meta_in.read_vint()?;
        let num_index_dims = if version >= VERSION_SELECTIVE_INDEXING {
            meta_in.read_vint()?
        } else {
            num_dims
        };

        let max_points_in_leaf_node = meta_in.read_vint()?;
        let bytes_per_dim = meta_in.read_vint()?;
        let config = Rc::new(BKDConfig::new(
            num_dims,
            num_index_dims,
            bytes_per_dim,
            max_points_in_leaf_node,
        )?);

        // Read index:
        let num_leaves = meta_in.read_vint()?;
        debug_assert!(num_leaves > 0);
        let packed_index_bytes_length = config.packed_index_bytes_length();
        let mut min_packed_value = vec![0; packed_index_bytes_length as usize];
        let mut max_packed_value = vec![0; packed_index_bytes_length as usize];

        DataInput::read_bytes(meta_in, &mut min_packed_value, 0, packed_index_bytes_length)?;
        DataInput::read_bytes(meta_in, &mut max_packed_value, 0, packed_index_bytes_length)?;

        let bytes_per_dim = config.bytes_per_dim as usize;
        let comparator = ArrayUtil::get_unsigned_comparator(bytes_per_dim);
        for dim in 0..config.num_index_dims as usize {
            let offset = dim * bytes_per_dim;
            if comparator.compare(&min_packed_value, offset, &max_packed_value, offset) > 0 {
                return Err(LuceneError::corrupt_index(format!(
                    "minPackedValue {} is > maxPackedValue {} for dim={}, (resource={})",
                    BytesRef::from_bytes(min_packed_value),
                    BytesRef::from_bytes(max_packed_value),
                    dim,
                    meta_in
                )));
            }
        }

        let point_count = meta_in.read_vlong()?;
        let doc_count = meta_in.read_vint()?;
        let num_index_bytes = meta_in.read_vint()?;

        let (min_leaf_block_fp, index_start_pointer) = if version >= VERSION_META_FILE {
            (
                DataInput::read_long(meta_in)?,
                DataInput::read_long(meta_in)?,
            )
        } else {
            let mut index_in = index_in.borrow_mut();
            let index_start_pointer = index_in.get_file_pointer();
            let min_leaf_block_fp = index_in.read_vlong()?;
            index_in.seek(index_start_pointer)?;
            (min_leaf_block_fp, index_start_pointer)
        };
        let mut reader = Self {
            config,
            num_leaves,
            index_in,
            data_in,
            min_packed_value,
            max_packed_value,
            point_count,
            doc_count,
            version,
            min_leaf_block_fp,
            index_start_pointer,
            num_index_bytes,
            is_tree_balanced: false,
        };
        reader.is_tree_balanced = num_leaves != 1 && reader.is_tree_balanced()?;
        Ok(reader)
    }
    /// Checks if the tree is balanced.
    fn is_tree_balanced(&self) -> Result<bool> {
        if self.version >= VERSION_META_FILE {
            // Since Lucene 8.6 all trees are unbalanced.
            return Ok(false);
        }
        if self.config.num_dims > 1 {
            // High dimensional tree in pre-8.6 indices are balanced.
            debug_assert!((1 << MathUtil::log(self.num_leaves as i64, 2)?) == self.num_leaves);
            return Ok(true);
        }
        if (1 << MathUtil::log(self.num_leaves as i64, 2)?) != self.num_leaves {
            // If we don't have enough leaves to fill the last level then it is
            // unbalanced.
            return Ok(false);
        }

        // Count of the last node for unbalanced trees.
        let last_leaf_node_point_count =
            (self.point_count % self.config.max_points_in_leaf_node as i64) as i32;

        // Navigate to last node.
        let mut point_tree = self.get_point_tree()?;
        while point_tree.move_to_sibling()? {}
        while point_tree.move_to_child()? {}

        // Count number of docs in the node.
        let mut count = vec![0; 1];
        let mut visitor = IntersectVisitorImpl { count: &mut count };
        point_tree.visit_doc_ids(&mut visitor)?;

        Ok(count[0] != last_leaf_node_point_count)
    }
}
impl<I> PointValues for BKDReader<I>
where
    I: IndexInput,
{
    fn get_min_packed_value(&self) -> Result<Option<Vec<u8>>> {
        Ok(Option::from(self.min_packed_value.clone()))
    }

    fn get_max_packed_value(&self) -> Result<Option<Vec<u8>>> {
        Ok(Option::from(self.max_packed_value.clone()))
    }

    fn get_num_dimensions(&self) -> Result<i32> {
        Ok(self.config.num_dims)
    }

    fn get_num_index_dimensions(&self) -> Result<i32> {
        Ok(self.config.num_index_dims)
    }

    fn get_bytes_per_dimension(&self) -> Result<i32> {
        Ok(self.config.bytes_per_dim)
    }

    fn size(&self) -> Result<i64> {
        Ok(self.point_count)
    }

    fn get_doc_count(&self) -> Result<i32> {
        Ok(self.doc_count)
    }

    type PointTree = BKDPointTree<I>;

    fn get_point_tree(&self) -> Result<Self::PointTree> {
        let slice = self.index_in.borrow_mut().slice(
            "packedIndex",
            self.index_start_pointer,
            self.num_index_bytes as i64,
        )?;
        BKDPointTree::new(
            slice,
            self.data_in.clone(),
            self.config.clone(),
            self.num_leaves,
            self.version,
            self.point_count,
            &self.min_packed_value,
            &self.max_packed_value,
            self.is_tree_balanced,
        )
    }
}

pub struct BKDPointTree<I: IndexInput> {
    /// Current node ID in the tree.
    node_id: i32,
    /// During clone, the node root can be different from 1.
    node_root: i32,
    /// Level is 1-based so that we can do `level - 1` without checking each
    /// time.
    level: i32,
    /// Used to read the packed tree off-heap.
    inner_nodes: I::Slice,
    /// Used to read the packed leaves off-heap.
    leaf_nodes: Rc<RefCell<I>>,
    /// Holds the minimum (left-most) leaf block file pointer for each level
    /// we've recursed to.
    leaf_block_fp_stack: Vec<i64>,
    /// Holds the address, in the off-heap index, after reading the node data
    /// of each level.
    read_node_data_positions: Vec<i32>,
    /// Holds the address, in the off-heap index, of the right-node of each
    /// level.
    right_node_positions: Vec<i32>,
    /// Holds the splitDim position for each level.
    split_dims_pos: Vec<i32>,
    /// True if the per-dimension delta we read for the node at this level is a
    /// negative offset versus the last split on this dimension.
    /// This is a packed 2D array, i.e., to access `array[level][dim]`,
    /// you read from `negative_deltas[level * num_dims + dim]`.
    /// This will be true if the last time we split on this dimension,
    /// we next pushed to the left sub-tree.
    negative_deltas: Vec<bool>,
    /// Holds the packed per-level split values.
    split_values_stack: Vec<Vec<u8>>,
    /// Holds the min / max value of the current node.
    min_packed_value: Vec<u8>,
    max_packed_value: Vec<u8>,
    /// Holds the previous value of the split dimension.
    split_dim_value_stack: Vec<Vec<u8>>,
    /// Tree parameters.
    config: Rc<BKDConfig>,
    /// Number of leaves.
    leaf_node_offset: i32,
    /// Version of the index.
    version: i32,
    /// Total number of points.
    point_count: i64,
    /// Last node might not be fully populated.
    last_leaf_node_point_count: i32,
    /// Right-most leaf node ID.
    right_most_leaf_node: i32,
    /// Helper objects for reading doc values.
    scratch_data_packed_value: Vec<u8>,
    scratch_min_index_packed_value: Vec<u8>,
    scratch_max_index_packed_value: Vec<u8>,
    common_prefix_lengths: Vec<i32>,
    scratch_iterator: BKDReaderDocIDSetIterator,
    /// If true, the tree is balanced; otherwise, it is unbalanced.
    is_tree_balanced: bool,
}

impl<I> BKDPointTree<I>
where
    I: IndexInput,
{
    #[allow(clippy::too_many_arguments)]
    fn new(
        inner_nodes: I::Slice,
        leaf_nodes: Rc<RefCell<I>>,
        config: Rc<BKDConfig>,
        num_leaves: i32,
        version: i32,
        point_count: i64,
        min_packed_value: &[u8],
        max_packed_value: &[u8],
        is_tree_balanced: bool,
    ) -> Result<Self> {
        let packed_bytes_len = config.packed_bytes_length() as usize;
        let packed_index_bytes_len = config.packed_index_bytes_length() as usize;
        let num_dims = config.num_dims as usize;
        let disi_len = config.max_points_in_leaf_node;

        let mut tree = Self::with_scratch_iterator(
            inner_nodes,
            leaf_nodes,
            config,
            num_leaves,
            version,
            point_count,
            1,
            1,
            min_packed_value,
            max_packed_value,
            BKDReaderDocIDSetIterator::new(disi_len),
            vec![0; packed_bytes_len],
            vec![0; packed_index_bytes_len],
            vec![0; packed_index_bytes_len],
            vec![0; num_dims],
            is_tree_balanced,
        )?;
        tree.read_node_data(false)?;
        Ok(tree)
    }
    #[allow(clippy::too_many_arguments)]
    fn with_scratch_iterator(
        inner_nodes: I::Slice,
        leaf_nodes: Rc<RefCell<I>>,
        config: Rc<BKDConfig>,
        num_leaves: i32,
        version: i32,
        point_count: i64,
        node_id: i32,
        level: i32,
        min_packed_value: &[u8],
        max_packed_value: &[u8],
        scratch_iterator: BKDReaderDocIDSetIterator,
        scratch_data_packed_value: Vec<u8>,
        scratch_min_index_packed_value: Vec<u8>,
        scratch_max_index_packed_value: Vec<u8>,
        common_prefix_lengths: Vec<i32>,
        is_tree_balanced: bool,
    ) -> Result<Self> {
        // stack arrays that keep information at different levels
        let tree_depth = Self::get_tree_depth(num_leaves)? as usize;
        let split_values_stack =
            vec![vec![0; config.packed_index_bytes_length() as usize]; tree_depth];
        let right_most_leaf_node = (1 << (tree_depth - 1)) - 1;
        let last_leaf_node_point_count =
            (point_count % config.max_points_in_leaf_node as i64).try_into()?;
        let last_leaf_node_point_count = if last_leaf_node_point_count == 0 {
            config.max_points_in_leaf_node
        } else {
            last_leaf_node_point_count
        };
        let negative_deltas_len = config.num_index_dims as usize * tree_depth;

        Ok(BKDPointTree {
            config,
            version,
            node_id,
            node_root: node_id,
            level,
            is_tree_balanced,
            leaf_node_offset: num_leaves,
            inner_nodes,
            leaf_nodes,
            min_packed_value: min_packed_value.to_vec(),
            max_packed_value: max_packed_value.to_vec(),
            split_dim_value_stack: vec![vec![]; tree_depth],
            split_values_stack,
            leaf_block_fp_stack: vec![0; tree_depth + 1],
            read_node_data_positions: vec![0; tree_depth + 1],
            right_node_positions: vec![0; tree_depth],
            split_dims_pos: vec![0; tree_depth],
            negative_deltas: vec![false; negative_deltas_len],
            point_count,
            right_most_leaf_node,
            last_leaf_node_point_count,
            // scratch objects, reused between clones so NN search are not
            // creating those objects in every clone.
            scratch_iterator,
            common_prefix_lengths,
            scratch_data_packed_value,
            scratch_min_index_packed_value,
            scratch_max_index_packed_value,
        })
    }
    fn reset_node_data_position(&mut self) -> Result<()> {
        // move position of the inner nodes index to visit the first child
        let position = self.read_node_data_positions[self.level as usize] as i64;
        debug_assert!(position <= self.inner_nodes.get_file_pointer());
        self.inner_nodes.seek(position)?;
        Ok(())
    }
    fn push_bounds_left(&mut self) {
        let level = self.level as usize;
        let bytes_per_dim = self.config.bytes_per_dim as usize;
        let split_dim_pos = self.split_dims_pos[level] as usize;

        if self.split_dim_value_stack[level].is_empty() {
            self.split_dim_value_stack[level] = vec![0; bytes_per_dim];
        }
        // save the dimension we are going to change
        self.split_dim_value_stack[level].copy_from(
            &self.max_packed_value[split_dim_pos..split_dim_pos + bytes_per_dim],
            0,
        );

        debug_assert!(
            ArrayUtil::get_unsigned_comparator(bytes_per_dim).compare(
                &self.max_packed_value,
                split_dim_pos,
                &self.split_values_stack[level],
                split_dim_pos
            ) >= 0,
            "config.bytesPerDim={} splitDimPos={} config.numIndexDims={} config.numDims={}",
            self.config.bytes_per_dim,
            self.split_dims_pos[level],
            self.config.num_index_dims,
            self.config.num_dims
        );

        // add the split dim value:
        self.max_packed_value.copy_from(
            &self.split_values_stack[level][split_dim_pos..split_dim_pos + bytes_per_dim],
            split_dim_pos,
        );
    }
    fn push_left(&mut self) -> Result<()> {
        self.node_id *= 2;
        self.level += 1;
        self.read_node_data(true)
    }
    fn push_bounds_right(&mut self) {
        let level = self.level as usize;
        let bytes_per_dim = self.config.bytes_per_dim as usize;
        let split_dim_pos = self.split_dims_pos[level] as usize;
        // we should have already visited the left node
        debug_assert!(!self.split_dim_value_stack[level].is_empty());
        // save the dimension we are going to change
        self.split_dim_value_stack[level].copy_from(
            &self.min_packed_value[split_dim_pos..split_dim_pos + bytes_per_dim],
            0,
        );

        debug_assert!(
            ArrayUtil::get_unsigned_comparator(bytes_per_dim).compare(
                &self.min_packed_value,
                split_dim_pos,
                &self.split_values_stack[level],
                split_dim_pos
            ) <= 0,
            "config.bytesPerDim={} splitDimPos={} config.numIndexDims={} config.numDims={}",
            self.config.bytes_per_dim,
            self.split_dims_pos[level],
            self.config.num_index_dims,
            self.config.num_dims
        );
        // add the split dim value:
        self.min_packed_value.copy_from(
            &self.split_values_stack[level][split_dim_pos..split_dim_pos + bytes_per_dim],
            split_dim_pos,
        );
    }
    fn push_right(&mut self) -> Result<()> {
        let node_position = self.right_node_positions[self.level as usize] as i64;

        debug_assert!(
            node_position >= self.inner_nodes.get_file_pointer(),
            "nodePosition = {} < currentPosition={}",
            node_position,
            self.inner_nodes.get_file_pointer()
        );

        self.inner_nodes.seek(node_position)?;
        self.node_id = 2 * self.node_id + 1;
        self.level += 1;
        self.read_node_data(false)
    }
    fn pop(&mut self) {
        self.node_id /= 2;
        self.level -= 1;
    }

    fn pop_bounds(&mut self, is_left: bool) {
        let level = self.level as usize;
        let split_dim_pos = self.split_dims_pos[level] as usize;
        let bytes_per_dim = self.config.bytes_per_dim as usize;

        if is_left {
            self.max_packed_value.copy_from(
                &self.split_dim_value_stack[level][..bytes_per_dim],
                split_dim_pos,
            );
        } else {
            self.min_packed_value.copy_from(
                &self.split_dim_value_stack[level][..bytes_per_dim],
                split_dim_pos,
            );
        }
    }
    fn is_root_node(&self) -> bool {
        self.node_id == self.node_root
    }

    fn is_left_node(&self) -> bool {
        (self.node_id & 1) == 0
    }

    fn is_leaf_node(&self) -> bool {
        self.node_id >= self.leaf_node_offset
    }

    fn node_exists(&self) -> bool {
        self.node_id - self.leaf_node_offset < self.leaf_node_offset
    }
    /// Only valid after pushLeft or pushRight, not pop!.
    fn get_leaf_block_fp(&self) -> Result<i64> {
        debug_assert!(self.is_leaf_node(), "nodeID={} is not a leaf", self.node_id);
        Ok(self.leaf_block_fp_stack[self.level as usize])
    }
    fn size_from_balanced_tree(
        &self,
        left_most_leaf_node: i32,
        right_most_leaf_node: i32,
    ) -> Result<i64> {
        // number of points that need to be distributed between leaves, one per
        // leaf
        let extra_points: i32 = (self.config.max_points_in_leaf_node as i64
            * self.leaf_node_offset as i64
            - self.point_count)
            .try_into()?;

        debug_assert!(
            extra_points < self.leaf_node_offset,
            "point excess should be lower than leafNodeOffset"
        );

        // offset where we stop adding one point to the leaves
        let node_offset = self.leaf_node_offset - extra_points;
        let mut count: i64 = 0;

        for node in left_most_leaf_node..=right_most_leaf_node {
            // offsetPosition provides which extra point will be added to this
            // node
            if Self::balance_tree_node_position(
                0,
                self.leaf_node_offset,
                node - self.leaf_node_offset,
                0,
                0,
            ) < node_offset
            {
                count += self.config.max_points_in_leaf_node as i64;
            } else {
                count += (self.config.max_points_in_leaf_node - 1) as i64;
            }
        }
        Ok(count)
    }
    fn balance_tree_node_position(
        min_node: i32,
        max_node: i32,
        node: i32,
        position: i32,
        level: i32,
    ) -> i32 {
        if max_node - min_node == 1 {
            return position;
        }
        let mid = (min_node + max_node + 1) / 2;
        if mid > node {
            Self::balance_tree_node_position(min_node, mid, node, position, level + 1)
        } else {
            Self::balance_tree_node_position(
                mid,
                max_node,
                node,
                position + (1 << level),
                level + 1,
            )
        }
    }
    fn add_all(&mut self, visitor: &mut impl IntersectVisitor, mut grown: bool) -> Result<()> {
        if !grown {
            let size = self.size()?;
            if size <= i32::MAX as i64 {
                visitor.grow(size as i32)?;
                grown = true;
            }
        }

        if self.is_leaf_node() {
            let mut leaf_nodes = self.leaf_nodes.borrow_mut();
            // Leaf node
            let leaf_fp = self.get_leaf_block_fp()?;
            leaf_nodes.seek(leaf_fp)?;
            // How many points are stored in this leaf cell:
            let count = leaf_nodes.read_vint()?;
            // No need to call grow(), it has been called up-front
            self.scratch_iterator
                .doc_ids_writer
                .read_ints_with_visitor(&mut *leaf_nodes, count, visitor)?;
        } else {
            self.push_left()?;
            self.add_all(visitor, grown)?;
            self.pop();
            self.push_right()?;
            self.add_all(visitor, grown)?;
            self.pop();
        }

        Ok(())
    }
    fn visit_leaves_one_by_one(&mut self, visitor: &mut impl IntersectVisitor) -> Result<()> {
        if self.is_leaf_node() {
            let leaf_fp = self.get_leaf_block_fp()?;
            self.visit_doc_values(visitor, leaf_fp)?;
        } else {
            self.push_left()?;
            self.visit_leaves_one_by_one(visitor)?;
            self.pop();

            self.push_right()?;
            self.visit_leaves_one_by_one(visitor)?;
            self.pop();
        }
        Ok(())
    }

    fn visit_doc_values(&mut self, visitor: &mut impl IntersectVisitor, fp: i64) -> Result<()> {
        let count = self.read_doc_ids(fp)?;

        if self.version >= VERSION_LOW_CARDINALITY_LEAVES {
            self.visit_doc_values_with_cardinality(count, visitor)?;
        } else {
            self.visit_doc_values_no_cardinality(count, visitor)?;
        }

        Ok(())
    }

    fn read_doc_ids(&mut self, block_fp: i64) -> Result<i32> {
        let mut index_input = self.leaf_nodes.borrow_mut();
        index_input.seek(block_fp)?;
        let count = index_input.read_vint()?;
        self.scratch_iterator.doc_ids_writer.read_ints(
            &mut *index_input,
            count,
            &mut self.scratch_iterator.doc_ids,
        )?;
        Ok(count)
    }

    fn get_num_leaves_slow(&self, node: i32) -> i32 {
        if node >= 2 * self.leaf_node_offset {
            0
        } else if node >= self.leaf_node_offset {
            1
        } else {
            let left_count = self.get_num_leaves_slow(node * 2);
            let right_count = self.get_num_leaves_slow(node * 2 + 1);
            left_count + right_count
        }
    }

    fn read_node_data(&mut self, is_left: bool) -> Result<()> {
        self.leaf_block_fp_stack[self.level as usize] =
            self.leaf_block_fp_stack[(self.level - 1) as usize];
        if !is_left {
            // Read leaf block FP delta
            self.leaf_block_fp_stack[self.level as usize] += self.inner_nodes.read_vlong()?;
        }

        if !self.is_leaf_node() {
            let num_index_dims = self.config.num_index_dims as usize;
            let level = self.level as usize;

            // Copy the negative deltas from the previous level
            let prev_offset = (level - 1) * num_index_dims;
            let curr_offset = level * num_index_dims;
            self.negative_deltas
                .copy_within(prev_offset..prev_offset + num_index_dims, curr_offset);
            self.negative_deltas[curr_offset
                + (self.split_dims_pos[level - 1] / self.config.bytes_per_dim) as usize] = is_left;

            // Clone or copy the previous level's split values
            if self.split_values_stack[level].is_empty() {
                self.split_values_stack[level] = self.split_values_stack[level - 1].clone();
            } else {
                let (before, after) = self.split_values_stack.split_at_mut(level);
                let source = &before[level - 1][..self.config.packed_index_bytes_length() as usize];
                after[0].copy_from(source, 0);
            }

            // Read split dim, prefix, and firstDiffByteDelta encoded as an int
            let mut code = self.inner_nodes.read_vint()?;
            let split_dim = code % self.config.num_index_dims;
            self.split_dims_pos[level] = split_dim * self.config.bytes_per_dim;
            code /= self.config.num_index_dims;
            let prefix = code % (1 + self.config.bytes_per_dim);
            let suffix = self.config.bytes_per_dim - prefix;

            if suffix > 0 {
                let mut first_diff_byte_delta = code / (1 + self.config.bytes_per_dim);
                if self.negative_deltas[curr_offset + split_dim as usize] {
                    first_diff_byte_delta = -first_diff_byte_delta;
                }
                let start_pos = self.split_dims_pos[level] + prefix;
                let old_byte = self.split_values_stack[level][start_pos as usize] as i32;
                self.split_values_stack[level][start_pos as usize] =
                    (old_byte + first_diff_byte_delta) as u8;
                DataInput::read_bytes(
                    &mut self.inner_nodes,
                    &mut self.split_values_stack[level],
                    start_pos + 1,
                    suffix - 1,
                )?;
            } else {
                // Our split value is == last split value in this dim, which can
                // happen when there are many duplicate values.
            }

            let left_num_bytes = if self.node_id * 2 < self.leaf_node_offset {
                self.inner_nodes.read_vint()?
            } else {
                0
            };
            self.right_node_positions[level] =
                (self.inner_nodes.get_file_pointer() + left_num_bytes as i64).try_into()?;
            self.read_node_data_positions[level] =
                self.inner_nodes.get_file_pointer().try_into()?;
        }
        Ok(())
    }
    /// Computes the depth of the tree based on the number of leaves.
    ///
    /// - The first `+1` accounts for the fact that all non-leaf nodes form
    ///   another power of 2. For example, to have a fully balanced tree with 4
    ///   leaves, you need a tree of depth 3.
    /// - The second `+1` ensures that the depth is correctly calculated, as
    ///   `log2(num_leaves)` computes the floor of the logarithm. For example,
    ///   with 5 leaves, you need a tree of depth 4.
    fn get_tree_depth(num_leaves: i32) -> Result<i32> {
        Ok(MathUtil::log(num_leaves as i64, 2)? + 2)
    }
    fn visit_doc_values_no_cardinality(
        &mut self,
        count: i32,
        visitor: &mut impl IntersectVisitor,
    ) -> Result<()> {
        let packed_index_bytes_length = self.config.packed_index_bytes_length() as usize;

        self.read_common_prefixes()?;

        if self.config.num_index_dims > 1 && self.version >= VERSION_LEAF_STORES_BOUNDS {
            self.scratch_max_index_packed_value.copy_from(
                &self.scratch_data_packed_value[..packed_index_bytes_length],
                0,
            );
            self.scratch_max_index_packed_value.copy_from(
                &self.scratch_min_index_packed_value[..packed_index_bytes_length],
                0,
            );
            self.read_min_max()?;

            // The index gives us range of values for each dimension, but the
            // actual range of values might be much more narrow than
            // what the index told us, so we double check the relation
            // here, which is cheap yet might help figure out that the block
            // either entirely matches or does not match at all.
            // This is especially more likely in the case that there are
            // multiple dimensions that have correlation, ie. splitting on one
            // dimension also significantly changes the range of
            // values in another dimension.
            let relation = visitor.compare(
                &self.scratch_min_index_packed_value,
                &self.scratch_max_index_packed_value,
            )?;
            if relation == Relation::CellOutsideQuery {
                return Ok(());
            }
            visitor.grow(count)?;

            if relation == Relation::CellInsideQuery {
                for i in 0..count as usize {
                    visitor.visit(self.scratch_iterator.doc_ids[i])?;
                }
                return Ok(());
            }
        } else {
            visitor.grow(count)?;
        }

        let compressed_dim = self.read_compressed_dim()?;

        if compressed_dim == -1 {
            self.visit_unique_raw_doc_values(count, visitor)?;
        } else {
            self.visit_compressed_doc_values(count, visitor, compressed_dim)?;
        }

        Ok(())
    }
    fn visit_doc_values_with_cardinality(
        &mut self,
        count: i32,
        visitor: &mut impl IntersectVisitor,
    ) -> Result<()> {
        let packed_index_bytes_length = self.config.packed_index_bytes_length() as usize;
        self.read_common_prefixes()?;
        let compressed_dim = self.read_compressed_dim()?;
        if compressed_dim == -1 {
            // all values are the same
            visitor.grow(count)?;
            self.visit_unique_raw_doc_values(count, visitor)?;
        } else {
            if self.config.num_index_dims != 1 {
                self.scratch_min_index_packed_value.copy_from(
                    &self.scratch_data_packed_value[..packed_index_bytes_length],
                    0,
                );
                self.scratch_max_index_packed_value.copy_from(
                    &self.scratch_min_index_packed_value[..packed_index_bytes_length],
                    0,
                );
                self.read_min_max()?;

                // The index gives us range of values for each dimension, but
                // the actual range of values might be much more
                // narrow than what the index told us, so we double check the
                // relation here, which is cheap yet might help
                // figure out that the block either entirely matches
                // or does not match at all. This is especially more likely in
                // the case that there are multiple dimensions
                // that have correlation, ie. splitting on one dimension also
                // significantly changes the range of values in another
                // dimension.
                let relation = visitor.compare(
                    &self.scratch_min_index_packed_value,
                    &self.scratch_max_index_packed_value,
                )?;
                if relation == Relation::CellOutsideQuery {
                    return Ok(());
                }
                visitor.grow(count)?;

                if relation == Relation::CellInsideQuery {
                    for i in 0..count as usize {
                        visitor.visit(self.scratch_iterator.doc_ids[i])?;
                    }
                    return Ok(());
                }
            } else {
                visitor.grow(count)?;
            }

            if compressed_dim == -2 {
                // low cardinality values
                self.visit_sparse_raw_doc_values(count, visitor)?;
            } else {
                // high cardinality
                self.visit_compressed_doc_values(count, visitor, compressed_dim)?;
            }
        }

        Ok(())
    }
    fn read_min_max(&mut self) -> Result<()> {
        let index_input = &mut *self.leaf_nodes.borrow_mut();
        for dim in 0..self.config.num_index_dims {
            let prefix = self.common_prefix_lengths[dim as usize];
            DataInput::read_bytes(
                index_input,
                &mut self.scratch_min_index_packed_value,
                dim * self.config.bytes_per_dim + prefix,
                self.config.bytes_per_dim - prefix,
            )?;
            DataInput::read_bytes(
                index_input,
                &mut self.scratch_max_index_packed_value,
                dim * self.config.bytes_per_dim + prefix,
                self.config.bytes_per_dim - prefix,
            )?;
        }

        Ok(())
    }

    // read cardinality and point
    fn visit_sparse_raw_doc_values(
        &mut self,
        count: i32,
        visitor: &mut impl IntersectVisitor,
    ) -> Result<()> {
        let mut i = 0;
        {
            let index_input = &mut *self.leaf_nodes.borrow_mut();
            while i < count {
                let length = DataInput::read_vint(index_input)?;
                for dim in 0..self.config.num_dims {
                    let prefix = self.common_prefix_lengths[dim as usize];
                    DataInput::read_bytes(
                        index_input,
                        &mut self.scratch_data_packed_value,
                        dim * self.config.bytes_per_dim + prefix,
                        self.config.bytes_per_dim - prefix,
                    )?;
                }
                self.scratch_iterator.reset(i, length);
                visitor.visit_iterator_with_packed_value(
                    &mut self.scratch_iterator,
                    &self.scratch_data_packed_value,
                )?;
                i += length;
            }
        }

        if i != count {
            return Err(LuceneError::corrupt_index(format!(
                "Sub blocks do not add up to the expected count: {} != {}, (resource={})",
                count,
                i,
                self.leaf_nodes.borrow()
            )));
        }

        Ok(())
    }

    // point is under commonPrefix
    pub fn visit_unique_raw_doc_values(
        &mut self,
        count: i32,
        visitor: &mut impl IntersectVisitor,
    ) -> Result<()> {
        self.scratch_iterator.reset(0, count);
        visitor.visit_iterator_with_packed_value(
            &mut self.scratch_iterator,
            &self.scratch_data_packed_value,
        )?;
        Ok(())
    }
    fn visit_compressed_doc_values(
        &mut self,
        count: i32,
        visitor: &mut impl IntersectVisitor,
        compressed_dim: i32,
    ) -> Result<()> {
        let bytes_per_dim = self.config.bytes_per_dim as usize;
        let compressed_dim = compressed_dim as usize;

        // the byte at `compressedByteOffset` is compressed using run-length
        // compression, other suffix bytes are stored verbatim
        let compressed_byte_offset =
            compressed_dim * bytes_per_dim + self.common_prefix_lengths[compressed_dim] as usize;
        self.common_prefix_lengths[compressed_dim] += 1;

        let mut i = 0;
        {
            let index_input = &mut *self.leaf_nodes.borrow_mut();
            while i < count {
                self.scratch_data_packed_value[compressed_byte_offset] =
                    DataInput::read_byte(index_input)?;
                let run_len = DataInput::read_byte(index_input)? as usize;
                for j in 0..run_len {
                    for dim in 0..self.config.num_dims {
                        let prefix = self.common_prefix_lengths[dim as usize];
                        DataInput::read_bytes(
                            index_input,
                            &mut self.scratch_data_packed_value,
                            dim * self.config.bytes_per_dim + prefix,
                            self.config.bytes_per_dim - prefix,
                        )?;
                    }
                    visitor.visit_with_packed_value(
                        self.scratch_iterator.doc_ids[i as usize + j],
                        &self.scratch_data_packed_value,
                    )?;
                }
                i += run_len as i32;
            }
        }

        if i != count {
            return Err(LuceneError::corrupt_index(format!(
                "Sub blocks do not add up to the expected count: {} != {}, (resource={})",
                count,
                i,
                self.leaf_nodes.borrow()
            )));
        }

        Ok(())
    }
    fn read_compressed_dim(&mut self) -> Result<i32> {
        let compressed_dim = DataInput::read_byte(&mut *self.leaf_nodes.borrow_mut())? as i8 as i32;

        if compressed_dim < -2
            || compressed_dim >= self.config.num_dims
            || (self.version < VERSION_LOW_CARDINALITY_LEAVES && compressed_dim == -2)
        {
            return Err(LuceneError::corrupt_index(format!(
                "Got compressedDim={} from input, (resource={})",
                compressed_dim,
                self.leaf_nodes.borrow()
            )));
        }

        Ok(compressed_dim)
    }

    pub fn read_common_prefixes(&mut self) -> Result<()> {
        let num_dims = self.config.num_dims;
        let index_input = &mut *self.leaf_nodes.borrow_mut();
        for dim in 0..num_dims {
            let prefix = index_input.read_vint()?;
            self.common_prefix_lengths[dim as usize] = prefix;
            if prefix > 0 {
                DataInput::read_bytes(
                    index_input,
                    &mut self.scratch_data_packed_value,
                    dim * self.config.bytes_per_dim,
                    prefix,
                )?;
            }
        }

        Ok(())
    }
}

impl<I> Clone for BKDPointTree<I>
where
    I: IndexInput,
{
    fn clone(&self) -> Self {
        // TODO: do we need this?
        unimplemented!()
    }
}

impl<I> PointTree for BKDPointTree<I>
where
    I: IndexInput,
{
    fn move_to_child(&mut self) -> Result<bool> {
        if self.is_leaf_node() {
            return Ok(false);
        }
        self.reset_node_data_position()?;
        self.push_bounds_left();
        self.push_left()?;
        Ok(true)
    }

    fn move_to_sibling(&mut self) -> Result<bool> {
        if !self.is_left_node() || self.is_root_node() {
            return Ok(false);
        }

        self.pop();
        self.pop_bounds(true);
        self.push_bounds_right();
        self.push_right()?;

        debug_assert!(self.node_exists(), "Sibling node must exist");
        Ok(true)
    }

    fn move_to_parent(&mut self) -> Result<bool> {
        if self.is_root_node() {
            return Ok(false);
        }
        let is_left = self.is_left_node();
        self.pop();
        self.pop_bounds(is_left);
        Ok(true)
    }

    fn get_min_packed_value(&self) -> Result<&[u8]> {
        Ok(&self.min_packed_value)
    }

    fn get_max_packed_value(&self) -> Result<&[u8]> {
        Ok(&self.max_packed_value)
    }

    fn size(&self) -> Result<i64> {
        let mut left_most_leaf_node = self.node_id;
        while left_most_leaf_node < self.leaf_node_offset {
            left_most_leaf_node *= 2;
        }

        let mut right_most_leaf_node = self.node_id;
        while right_most_leaf_node < self.leaf_node_offset {
            right_most_leaf_node = right_most_leaf_node * 2 + 1;
        }

        let num_leaves = if right_most_leaf_node >= left_most_leaf_node {
            // both are on the same level
            right_most_leaf_node - left_most_leaf_node + 1
        } else {
            // left is one level deeper than right
            right_most_leaf_node - left_most_leaf_node + 1 + self.leaf_node_offset
        };

        debug_assert!(
            num_leaves == self.get_num_leaves_slow(self.node_id),
            "numLeaves mismatch: {} vs {}",
            num_leaves,
            self.get_num_leaves_slow(self.node_id)
        );

        if self.is_tree_balanced {
            // before lucene 8.6, trees might have been constructed as fully
            // balanced trees.
            return self.size_from_balanced_tree(left_most_leaf_node, right_most_leaf_node);
        }

        // size for an unbalanced tree.
        let size = if right_most_leaf_node == self.right_most_leaf_node {
            (num_leaves as i64 - 1) * self.config.max_points_in_leaf_node as i64
                + self.last_leaf_node_point_count as i64
        } else {
            num_leaves as i64 * self.config.max_points_in_leaf_node as i64
        };

        Ok(size)
    }

    fn visit_doc_ids<IV>(&mut self, visitor: &mut IV) -> Result<()>
    where
        IV: IntersectVisitor,
    {
        self.reset_node_data_position()?;
        self.add_all(visitor, false)
    }

    fn visit_doc_values<IV>(&mut self, visitor: &mut IV) -> Result<()>
    where
        IV: IntersectVisitor,
    {
        self.reset_node_data_position()?;
        self.visit_leaves_one_by_one(visitor)
    }
}
/// Reusable [`DocIdSetIterator`] to handle low cardinality leaves.
struct BKDReaderDocIDSetIterator {
    idx: i32,
    length: i32,
    offset: i32,
    doc_id: i32,
    doc_ids: Vec<i32>,
    doc_ids_writer: DocIdsWriter,
}

impl BKDReaderDocIDSetIterator {
    pub fn new(max_points_in_leaf_node: i32) -> Self {
        Self {
            idx: 0,
            length: 0,
            offset: 0,
            doc_id: -1,
            doc_ids: vec![0; max_points_in_leaf_node as usize],
            doc_ids_writer: DocIdsWriter::new(max_points_in_leaf_node),
        }
    }
    fn reset(&mut self, offset: i32, length: i32) {
        self.offset = offset;
        self.length = length;
        debug_assert!((offset + length) as usize <= self.doc_ids.len());
        self.doc_id = -1;
        self.idx = 0;
    }
}
impl DocIdSetIterator for BKDReaderDocIDSetIterator {
    fn doc_id(&self) -> i32 {
        self.doc_id
    }

    fn next_doc(&mut self) -> Result<i32> {
        if self.idx == self.length {
            self.doc_id = NO_MORE_DOCS;
        } else {
            self.doc_id = self.doc_ids[(self.offset + self.idx) as usize];
            self.idx += 1;
        }
        Ok(self.doc_id)
    }

    fn advance(&mut self, target: i32) -> Result<i32> {
        DocIdSetIterator::slow_advance(self, target)
    }

    fn cost(&self) -> Result<i64> {
        Ok(self.length as i64)
    }
}

struct IntersectVisitorImpl<'a> {
    count: &'a mut [i32],
}
impl IntersectVisitor for IntersectVisitorImpl<'_> {
    fn visit(&mut self, _doc_id: i32) -> Result<()> {
        self.count[0] += 1;
        Ok(())
    }

    fn visit_with_packed_value(&mut self, _doc_id: i32, _packed_value: &[u8]) -> Result<()> {
        Err(LuceneError::not_implemented(""))
    }

    fn compare(&mut self, _min_packed_value: &[u8], _max_packed_value: &[u8]) -> Result<Relation> {
        Err(LuceneError::not_implemented(""))
    }
}
