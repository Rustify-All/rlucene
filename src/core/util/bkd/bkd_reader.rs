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
use crate::core::codecs::dummy::dummy_mutable_point_tree::DummyMutablePointTree;
use crate::core::index::BytesRef;
use crate::core::index::point_values::{IntersectVisitor, PointTree, PointValues, Relation};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::store::{DataInput, IndexInput};
use crate::core::util::array_util::ByteArrayComparator;
use crate::core::util::bkd::bkd_bytes::BKDBytes;
use crate::core::util::bkd::bkd_config::BKDConfig;
use crate::core::util::bkd::bkd_writer::{
  CODEC_NAME, VERSION_CURRENT, VERSION_LEAF_STORES_BOUNDS, VERSION_LOW_CARDINALITY_LEAVES,
  VERSION_META_FILE, VERSION_SELECTIVE_INDEXING, VERSION_START,
};
use crate::core::util::bkd::doc_ids_writer::DocIdsWriter;
use crate::core::util::clone::TryClone;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::math_util::MathUtil;
use crate::core::util::{SliceCopyOps, TryIntoInt};
use parking_lot::Mutex;
use std::borrow::Cow;
use std::sync::Arc;

/// Handles reading a block KD-tree in byte-slice space previously written with
/// `BKDWriter`
pub struct BKDReader<I>
where
  I: IndexInput,
{
  config: BKDConfig,
  pub(crate) num_leaves: i32,
  index_in: Option<Arc<I>>,
  data_in: Arc<Mutex<I>>,
  min_packed_value: Box<[u8]>,
  max_packed_value: Box<[u8]>,
  point_count: usize,
  doc_count: i32,
  version: i32,
  #[allow(dead_code)]
  pub(crate) min_leaf_block_fp: i64,
  pub(crate) index_start_pointer: usize,
  num_index_bytes: usize,
  pub(crate) is_tree_balanced: bool,
  /// index_in,data_in are the same instance
  #[cfg(test)]
  pub same_in: bool,
}
impl<I: IndexInput> BKDReader<I>
where
  I: IndexInput,
{
  /// Caller must pre-seek the provided [`IndexInput`] to the index location
  /// that `BKDWriter::finish()` returned. BKD tree is always stored off-heap.
  ///
  /// Retains a cloned index view so the returned reader can create point trees.
  pub fn new<I1>(meta_in: &mut I1, index_in: &mut I, data_in: Arc<Mutex<I>>) -> Result<Self>
  where
    I1: IndexInput,
  {
    let mut reader = Self::read_metadata(meta_in, index_in, data_in)?;
    reader.init_index_in(Arc::new(index_in.try_clone()?))?;
    Ok(reader)
  }

  /// Reads metadata before the caller initializes the shared index input.
  pub(crate) fn read_metadata<I1>(
    meta_in: &mut I1,
    index_in: &mut I,
    data_in: Arc<Mutex<I>>,
  ) -> Result<Self>
  where
    I1: IndexInput,
  {
    let (mut reader, version) = Self::init_with_meta(meta_in, data_in)?;

    let (min_leaf_block_fp, index_start_pointer) = if version >= VERSION_META_FILE {
      (
        DataInput::read_long(meta_in)?,
        DataInput::read_long(meta_in)?.try_convert()?,
      )
    } else {
      let index_start_pointer = index_in.get_file_pointer()?;
      let min_leaf_block_fp = index_in.read_vlong()?;
      index_in.seek(index_start_pointer)?;
      (min_leaf_block_fp, index_start_pointer)
    };
    reader.min_leaf_block_fp = min_leaf_block_fp;
    reader.index_start_pointer = index_start_pointer;
    Ok(reader)
  }
  pub(crate) fn init_with_meta<I1>(meta_in: &mut I1, data_in: Arc<Mutex<I>>) -> Result<(Self, i32)>
  where
    I1: IndexInput,
  {
    let version = CodecUtil::check_header(meta_in, CODEC_NAME, VERSION_START, VERSION_CURRENT)?;

    let num_dims: usize = meta_in.read_vint()?.try_convert()?;
    let num_index_dims = if version >= VERSION_SELECTIVE_INDEXING {
      meta_in.read_vint()?.try_convert()?
    } else {
      num_dims
    };

    let max_points_in_leaf_node = meta_in.read_vint()?.try_convert()?;
    let bytes_per_dim = meta_in.read_vint()?.try_convert()?;
    let config = BKDConfig::new(
      num_dims,
      num_index_dims,
      bytes_per_dim,
      max_points_in_leaf_node,
    )?;

    // Read index:
    let num_leaves = meta_in.read_vint()?;
    debug_assert!(num_leaves > 0);
    let packed_index_bytes_length = config.packed_index_bytes_length();
    let min_packed_value = meta_in.get_bytes(packed_index_bytes_length)?.into_owned();
    let max_packed_value = meta_in.get_bytes(packed_index_bytes_length)?.into_owned();

    let bytes_per_dim = config.bytes_per_dim;
    let comparator = BKDBytes::<0>::new(bytes_per_dim);
    for dim in 0..config.num_index_dims {
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

    let point_count = meta_in.read_vlong()?.try_convert()?;
    let doc_count = meta_in.read_vint()?;
    let num_index_bytes = meta_in.read_vint()?.try_convert()?;
    Ok((
      Self {
        config,
        num_leaves,
        index_in: None,
        data_in,
        min_packed_value: min_packed_value.into_boxed_slice(),
        max_packed_value: max_packed_value.into_boxed_slice(),
        point_count,
        doc_count,
        version,
        min_leaf_block_fp: 0,
        index_start_pointer: 0,
        num_index_bytes,
        is_tree_balanced: false,
        #[cfg(test)]
        same_in: false,
      },
      version,
    ))
  }
  pub(crate) fn init_index_in(&mut self, index_in: Arc<I>) -> Result<()> {
    self.index_in = Some(index_in);
    self.is_tree_balanced = self.num_leaves != 1 && self.is_tree_balanced()?;
    Ok(())
  }
  /// Checks if the tree is balanced.
  pub(crate) fn is_tree_balanced(&self) -> Result<bool> {
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
    let last_leaf_node_point_count = self.point_count % self.config.max_points_in_leaf_node;

    // Navigate to last node.
    let mut point_tree = self.get_point_tree()?;
    while point_tree.move_to_sibling()? {}
    while point_tree.move_to_child()? {}

    // Count number of docs in the node.
    let mut visitor = IntersectVisitorImpl::new();
    point_tree.visit_doc_ids(&mut visitor)?;

    Ok(visitor.count != last_leaf_node_point_count)
  }
}
impl<I> PointValues for BKDReader<I>
where
  I: IndexInput,
{
  fn get_min_packed_value(&self) -> Result<Option<Cow<'_, [u8]>>> {
    Ok(Some(Cow::Borrowed(&self.min_packed_value)))
  }

  fn get_max_packed_value(&self) -> Result<Option<Cow<'_, [u8]>>> {
    Ok(Some(Cow::Borrowed(&self.max_packed_value)))
  }

  fn get_num_dimensions(&self) -> Result<usize> {
    Ok(self.config.num_dims)
  }

  fn get_num_index_dimensions(&self) -> Result<usize> {
    Ok(self.config.num_index_dims)
  }

  fn get_bytes_per_dimension(&self) -> Result<usize> {
    Ok(self.config.bytes_per_dim)
  }

  fn size(&self) -> Result<usize> {
    Ok(self.point_count)
  }

  fn get_doc_count(&self) -> Result<i32> {
    Ok(self.doc_count)
  }

  type PointTree = BKDPointTree<I>;

  fn get_point_tree(&self) -> Result<Self::PointTree> {
    let slice = match self.index_in {
      Some(ref index_in) => index_in.slice(
        "packedIndex",
        self.index_start_pointer,
        self.num_index_bytes,
      )?,
      None => {
        #[cfg(test)]
        {
          if !self.same_in {
            return Err(LuceneError::illegal_state(
              "index_in data_int not the same instance",
            ));
          }
          let data_in = &*self.data_in.lock();
          data_in.slice(
            "packedIndex",
            self.index_start_pointer,
            self.num_index_bytes,
          )?
        }
        #[cfg(not(test))]
        {
          return Err(LuceneError::illegal_state("index_in is None"));
        }
      },
    };
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

#[derive(Clone, Copy)]
struct LeafLevelPositions {
  leaf_block_fp: usize,
  read_node_data: usize,
}

#[derive(Clone, Copy)]
struct SplitLevelPositions {
  right_node: usize,
  dimension: usize,
}

pub struct BKDPointTree<I: IndexInput> {
  /// Current node ID in the tree.
  node_id: i32,
  /// During clone, the node root can be different from 1.
  node_root: i32,
  /// Level is 1-based so that we can do `level - 1` without checking each
  /// time.
  level: usize,
  /// Used to read the packed tree off-heap.
  inner_nodes: I::IndexInput,
  /// Used to read the packed leaves off-heap.
  leaf_nodes: Arc<Mutex<I>>,
  /// Leaf file pointer and position after node data, including the leaf sentinel level.
  leaf_positions: Box<[LeafLevelPositions]>,
  /// Right-child address and split dimension for internal levels.
  split_positions: Box<[SplitLevelPositions]>,
  /// True if the per-dimension delta we read for the node at this level is a
  /// negative offset versus the last split on this dimension.
  /// This is a packed 2D array, i.e., to access `array[level][dim]`,
  /// you read from `negative_deltas[level * num_dims + dim]`.
  /// This will be true if the last time we split on this dimension,
  /// we next pushed to the left sub-tree.
  negative_deltas: Box<[bool]>,
  /// Holds the packed per-level split values.
  split_values_stack: Box<[Box<[u8]>]>,
  /// Minimum then maximum value of the current node, each packed_index_bytes_length bytes.
  packed_bounds: Box<[u8]>,
  /// Holds the previous value of the split dimension.
  split_dim_value_stack: Box<[Box<[u8]>]>,
  /// Tree parameters.
  config: BKDConfig,
  /// Number of leaves.
  leaf_node_offset: i32,
  /// Version of the index.
  version: i32,
  /// Total number of points.
  point_count: usize,
  /// Last node might not be fully populated.
  last_leaf_node_point_count: usize,
  /// Right-most leaf node ID.
  right_most_leaf_node: i32,
  /// Data value, then indexed minimum and maximum, using the config's fixed widths.
  scratch_packed_values: Box<[u8]>,
  common_prefix_lengths: Box<[usize]>,
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
    inner_nodes: I::IndexInput,
    leaf_nodes: Arc<Mutex<I>>,
    config: BKDConfig,
    num_leaves: i32,
    version: i32,
    point_count: usize,
    min_packed_value: &[u8],
    max_packed_value: &[u8],
    is_tree_balanced: bool,
  ) -> Result<Self> {
    let packed_bytes_len = config.packed_bytes_length();
    let packed_index_bytes_len = config.packed_index_bytes_length();
    let num_dims = config.num_dims;
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
      vec![0; packed_bytes_len + 2 * packed_index_bytes_len],
      vec![0; num_dims],
      is_tree_balanced,
    )?;
    tree.split_values_stack[0] = vec![0; packed_index_bytes_len].into_boxed_slice();
    tree.read_node_data(false)?;
    Ok(tree)
  }
  #[allow(clippy::too_many_arguments)]
  fn with_scratch_iterator(
    inner_nodes: I::IndexInput,
    leaf_nodes: Arc<Mutex<I>>,
    config: BKDConfig,
    num_leaves: i32,
    version: i32,
    point_count: usize,
    node_id: i32,
    level: usize,
    min_packed_value: &[u8],
    max_packed_value: &[u8],
    scratch_iterator: BKDReaderDocIDSetIterator,
    scratch_packed_values: Vec<u8>,
    common_prefix_lengths: Vec<usize>,
    is_tree_balanced: bool,
  ) -> Result<Self> {
    // stack arrays that keep information at different levels
    let tree_depth = Self::get_tree_depth(num_leaves)? as usize;
    let split_values_stack = vec![Box::<[u8]>::default(); tree_depth];
    let right_most_leaf_node = (1 << (tree_depth - 1)) - 1;
    let last_leaf_node_point_count = point_count % config.max_points_in_leaf_node;
    let last_leaf_node_point_count = if last_leaf_node_point_count == 0 {
      config.max_points_in_leaf_node
    } else {
      last_leaf_node_point_count
    };
    let negative_deltas_len = config.num_index_dims * tree_depth;

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
      packed_bounds: [min_packed_value, max_packed_value]
        .concat()
        .into_boxed_slice(),
      split_dim_value_stack: vec![Box::<[u8]>::default(); tree_depth].into_boxed_slice(),
      split_values_stack: split_values_stack.into_boxed_slice(),
      leaf_positions: vec![
        LeafLevelPositions {
          leaf_block_fp: 0,
          read_node_data: 0
        };
        tree_depth + 1
      ]
      .into_boxed_slice(),
      split_positions: vec![
        SplitLevelPositions {
          right_node: 0,
          dimension: 0
        };
        tree_depth
      ]
      .into_boxed_slice(),
      negative_deltas: vec![false; negative_deltas_len].into_boxed_slice(),
      point_count,
      right_most_leaf_node,
      last_leaf_node_point_count,
      // scratch objects, reused between clones so NN search are not
      // creating those objects in every clone.
      scratch_iterator,
      common_prefix_lengths: common_prefix_lengths.into_boxed_slice(),
      scratch_packed_values: scratch_packed_values.into_boxed_slice(),
    })
  }
  fn reset_node_data_position(&mut self) -> Result<()> {
    // move position of the inner nodes index to visit the first child
    let position = self.leaf_positions[self.level].read_node_data;
    debug_assert!(position <= self.inner_nodes.get_file_pointer()?);
    self.inner_nodes.seek(position)?;
    Ok(())
  }
  fn push_bounds_left(&mut self) {
    let level = self.level;
    let bytes_per_dim = self.config.bytes_per_dim;
    let split_dim_pos = self.split_positions[level].dimension;

    // Save the dimension we are going to change.
    let start = self.config.packed_index_bytes_length() + split_dim_pos;
    let bound = &self.packed_bounds[start..start + bytes_per_dim];
    let saved = &mut self.split_dim_value_stack[level];
    if saved.is_empty() {
      *saved = Box::from(bound);
    } else {
      saved.copy_from_slice(bound);
    }

    debug_assert!(
      BKDBytes::<0>::new(bytes_per_dim).compare(
        &self.packed_bounds[self.config.packed_index_bytes_length()..],
        split_dim_pos,
        &self.split_values_stack[level],
        split_dim_pos
      ) >= 0,
      "config.bytesPerDim={} splitDimPos={} config.numIndexDims={} config.numDims={}",
      self.config.bytes_per_dim,
      self.split_positions[level].dimension,
      self.config.num_index_dims,
      self.config.num_dims
    );

    // add the split dim value:
    self.packed_bounds[self.config.packed_index_bytes_length()..].copy_from(
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
    let level = self.level;
    let bytes_per_dim = self.config.bytes_per_dim;
    let split_dim_pos = self.split_positions[level].dimension;
    // we should have already visited the left node
    debug_assert!(!self.split_dim_value_stack[level].is_empty());
    // save the dimension we are going to change
    self.split_dim_value_stack[level].copy_from(
      &self.packed_bounds[..self.config.packed_index_bytes_length()]
        [split_dim_pos..split_dim_pos + bytes_per_dim],
      0,
    );

    debug_assert!(
      BKDBytes::<0>::new(bytes_per_dim).compare(
        &self.packed_bounds[..self.config.packed_index_bytes_length()],
        split_dim_pos,
        &self.split_values_stack[level],
        split_dim_pos
      ) <= 0,
      "config.bytesPerDim={} splitDimPos={} config.numIndexDims={} config.numDims={}",
      self.config.bytes_per_dim,
      self.split_positions[level].dimension,
      self.config.num_index_dims,
      self.config.num_dims
    );
    // add the split dim value:
    self.packed_bounds[..self.config.packed_index_bytes_length()].copy_from(
      &self.split_values_stack[level][split_dim_pos..split_dim_pos + bytes_per_dim],
      split_dim_pos,
    );
  }
  fn push_right(&mut self) -> Result<()> {
    let node_position = self.split_positions[self.level].right_node;

    debug_assert!(
      node_position >= self.inner_nodes.get_file_pointer()?,
      "nodePosition = {} < currentPosition={}",
      node_position,
      self.inner_nodes.get_file_pointer()?
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
    let level = self.level;
    let split_dim_pos = self.split_positions[level].dimension;
    let bytes_per_dim = self.config.bytes_per_dim;

    if is_left {
      self.packed_bounds[self.config.packed_index_bytes_length()..].copy_from(
        &self.split_dim_value_stack[level][..bytes_per_dim],
        split_dim_pos,
      );
    } else {
      self.packed_bounds[..self.config.packed_index_bytes_length()].copy_from(
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
  fn get_leaf_block_fp(&self) -> Result<usize> {
    debug_assert!(self.is_leaf_node(), "nodeID={} is not a leaf", self.node_id);
    Ok(self.leaf_positions[self.level].leaf_block_fp)
  }
  fn size_from_balanced_tree(
    &self,
    left_most_leaf_node: i32,
    right_most_leaf_node: i32,
  ) -> Result<usize> {
    // number of points that need to be distributed between leaves, one per
    // leaf
    let extra_points: i32 = (self.config.max_points_in_leaf_node * self.leaf_node_offset as usize
      - self.point_count)
      .try_convert()?;

    debug_assert!(
      extra_points < self.leaf_node_offset,
      "point excess should be lower than leafNodeOffset"
    );

    // offset where we stop adding one point to the leaves
    let node_offset = self.leaf_node_offset - extra_points;
    let mut count = 0;

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
        count += self.config.max_points_in_leaf_node;
      } else {
        count += self.config.max_points_in_leaf_node - 1;
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
      Self::balance_tree_node_position(mid, max_node, node, position + (1 << level), level + 1)
    }
  }
  fn add_all<V>(&mut self, visitor: &mut V, mut grown: bool, leaf_nodes: &mut I) -> Result<()>
  where
    V: IntersectVisitor,
  {
    if !grown {
      let size = self.size()?;
      if size <= i32::MAX as usize {
        visitor.grow(size)?;
        grown = true;
      }
    }

    if self.is_leaf_node() {
      // Leaf node
      let leaf_fp = self.get_leaf_block_fp()?;
      leaf_nodes.seek(leaf_fp)?;
      // How many points are stored in this leaf cell:
      let count: usize = leaf_nodes.read_vint()?.try_convert()?;
      // No need to call grow(), it has been called up-front
      self
        .scratch_iterator
        .doc_ids_writer
        .read_ints_with_visitor(leaf_nodes, count, visitor)?;
    } else {
      self.push_left()?;
      self.add_all(visitor, grown, leaf_nodes)?;
      self.pop();
      self.push_right()?;
      self.add_all(visitor, grown, leaf_nodes)?;
      self.pop();
    }

    Ok(())
  }
  fn visit_leaves_one_by_one<V>(&mut self, visitor: &mut V, leaf_node: &mut I) -> Result<()>
  where
    V: IntersectVisitor,
  {
    if self.is_leaf_node() {
      let leaf_fp = self.get_leaf_block_fp()?;
      self.visit_doc_values(visitor, leaf_fp, leaf_node)?;
    } else {
      self.push_left()?;
      self.visit_leaves_one_by_one(visitor, leaf_node)?;
      self.pop();

      self.push_right()?;
      self.visit_leaves_one_by_one(visitor, leaf_node)?;
      self.pop();
    }
    Ok(())
  }

  fn visit_doc_values<V>(&mut self, visitor: &mut V, fp: usize, leaf_node: &mut I) -> Result<()>
  where
    V: IntersectVisitor,
  {
    let count = self.read_doc_ids(fp, leaf_node)?;

    if self.version >= VERSION_LOW_CARDINALITY_LEAVES {
      self.visit_doc_values_with_cardinality(count, visitor, leaf_node)?;
    } else {
      self.visit_doc_values_no_cardinality(count, visitor, leaf_node)?;
    }

    Ok(())
  }

  fn read_doc_ids(&mut self, block_fp: usize, index_input: &mut I) -> Result<usize> {
    index_input.seek(block_fp)?;
    let count = index_input.read_vint()?.try_convert()?;
    self.scratch_iterator.doc_ids_writer.read_ints(
      index_input,
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
    self.leaf_positions[self.level].leaf_block_fp =
      self.leaf_positions[self.level - 1].leaf_block_fp;
    if !is_left {
      // Read leaf block FP delta
      let v: usize = self.inner_nodes.read_vlong()?.try_convert()?;
      self.leaf_positions[self.level].leaf_block_fp += v;
    }

    if !self.is_leaf_node() {
      let num_index_dims = self.config.num_index_dims;

      // Copy the negative deltas from the previous level
      let prev_offset = (self.level - 1) * num_index_dims;
      let curr_offset = self.level * num_index_dims;
      self
        .negative_deltas
        .copy_within(prev_offset..prev_offset + num_index_dims, curr_offset);
      self.negative_deltas[curr_offset
        + self.split_positions[self.level - 1].dimension / self.config.bytes_per_dim] = is_left;

      // Clone or copy the previous level's split values
      if self.split_values_stack[self.level].is_empty() {
        self.split_values_stack[self.level] = self.split_values_stack[self.level - 1].clone();
      } else {
        let (before, after) = self.split_values_stack.split_at_mut(self.level);
        let source = &before[self.level - 1][..self.config.packed_index_bytes_length()];
        after[0].copy_from(source, 0);
      }

      // Read split dim, prefix, and firstDiffByteDelta encoded as an int
      let mut code: usize = self.inner_nodes.read_vint()?.try_convert()?;
      let split_dim = code % self.config.num_index_dims;
      self.split_positions[self.level].dimension = split_dim * self.config.bytes_per_dim;
      code /= self.config.num_index_dims;
      let prefix = code % (1 + self.config.bytes_per_dim);
      let suffix = self.config.bytes_per_dim - prefix;

      if suffix > 0 {
        let mut first_diff_byte_delta = (code / (1 + self.config.bytes_per_dim)) as i32;
        if self.negative_deltas[curr_offset + split_dim] {
          first_diff_byte_delta = -first_diff_byte_delta;
        }
        let start_pos = self.split_positions[self.level].dimension + prefix;
        let old_byte = self.split_values_stack[self.level][start_pos] as i32;
        self.split_values_stack[self.level][start_pos] = (old_byte + first_diff_byte_delta) as u8;
        DataInput::read_bytes(
          &mut self.inner_nodes,
          &mut self.split_values_stack[self.level],
          start_pos + 1,
          suffix - 1,
        )?;
      } else {
        // Our split value is == last split value in this dim, which can
        // happen when there are many duplicate values.
      }

      let left_num_bytes = if self.node_id * 2 < self.leaf_node_offset {
        self.inner_nodes.read_vint()?.try_convert()?
      } else {
        0
      };
      self.split_positions[self.level].right_node =
        self.inner_nodes.get_file_pointer()? + left_num_bytes;
      self.leaf_positions[self.level].read_node_data = self.inner_nodes.get_file_pointer()?;
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
  fn visit_doc_values_no_cardinality<V>(
    &mut self,
    count: usize,
    visitor: &mut V,
    leaf_node: &mut I,
  ) -> Result<()>
  where
    V: IntersectVisitor,
  {
    let packed_bytes_length = self.config.packed_bytes_length();
    let packed_index_bytes_length = self.config.packed_index_bytes_length();

    self.read_common_prefixes(leaf_node)?;

    if self.config.num_index_dims > 1 && self.version >= VERSION_LEAF_STORES_BOUNDS {
      self.scratch_packed_values.copy_within(
        ..packed_index_bytes_length,
        packed_bytes_length + packed_index_bytes_length,
      );
      self.scratch_packed_values.copy_within(
        packed_bytes_length..packed_bytes_length + packed_index_bytes_length,
        packed_bytes_length + packed_index_bytes_length,
      );
      self.read_min_max(leaf_node)?;

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
        &self.scratch_packed_values
          [packed_bytes_length..packed_bytes_length + packed_index_bytes_length],
        &self.scratch_packed_values[packed_bytes_length + packed_index_bytes_length..],
      )?;
      if relation == Relation::CellOutsideQuery {
        return Ok(());
      }
      visitor.grow(count)?;

      if relation == Relation::CellInsideQuery {
        for i in 0..count {
          visitor.visit(self.scratch_iterator.doc_ids[i])?;
        }
        return Ok(());
      }
    } else {
      visitor.grow(count)?;
    }

    let compressed_dim = self.read_compressed_dim(leaf_node)?;

    if compressed_dim == -1 {
      self.visit_unique_raw_doc_values(count, visitor)?;
    } else {
      self.visit_compressed_doc_values(count, visitor, compressed_dim, leaf_node)?;
    }

    Ok(())
  }
  fn visit_doc_values_with_cardinality<V>(
    &mut self,
    count: usize,
    visitor: &mut V,
    leaf_node: &mut I,
  ) -> Result<()>
  where
    V: IntersectVisitor,
  {
    let packed_bytes_length = self.config.packed_bytes_length();
    let packed_index_bytes_length = self.config.packed_index_bytes_length();
    self.read_common_prefixes(leaf_node)?;
    let compressed_dim = self.read_compressed_dim(leaf_node)?;
    if compressed_dim == -1 {
      // all values are the same
      visitor.grow(count)?;
      self.visit_unique_raw_doc_values(count, visitor)?;
    } else {
      if self.config.num_index_dims != 1 {
        self
          .scratch_packed_values
          .copy_within(..packed_index_bytes_length, packed_bytes_length);
        self.scratch_packed_values.copy_within(
          packed_bytes_length..packed_bytes_length + packed_index_bytes_length,
          packed_bytes_length + packed_index_bytes_length,
        );
        self.read_min_max(leaf_node)?;

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
          &self.scratch_packed_values
            [packed_bytes_length..packed_bytes_length + packed_index_bytes_length],
          &self.scratch_packed_values[packed_bytes_length + packed_index_bytes_length..],
        )?;
        if relation == Relation::CellOutsideQuery {
          return Ok(());
        }
        visitor.grow(count)?;

        if relation == Relation::CellInsideQuery {
          for i in 0..count {
            visitor.visit(self.scratch_iterator.doc_ids[i])?;
          }
          return Ok(());
        }
      } else {
        visitor.grow(count)?;
      }

      if compressed_dim == -2 {
        // low cardinality values
        self.visit_sparse_raw_doc_values(count, visitor, leaf_node)?;
      } else {
        // high cardinality
        self.visit_compressed_doc_values(count, visitor, compressed_dim, leaf_node)?;
      }
    }

    Ok(())
  }
  fn read_min_max(&mut self, index_input: &mut I) -> Result<()> {
    for dim in 0..self.config.num_index_dims {
      let prefix = self.common_prefix_lengths[dim];
      let offset = dim * self.config.bytes_per_dim + prefix;
      let len = self.config.bytes_per_dim - prefix;
      DataInput::read_bytes(
        index_input,
        &mut self.scratch_packed_values[self.config.packed_bytes_length()
          ..self.config.packed_bytes_length() + self.config.packed_index_bytes_length()],
        offset,
        len,
      )?;
      DataInput::read_bytes(
        index_input,
        &mut self.scratch_packed_values
          [self.config.packed_bytes_length() + self.config.packed_index_bytes_length()..],
        offset,
        len,
      )?;
    }

    Ok(())
  }

  // read cardinality and point
  fn visit_sparse_raw_doc_values<V>(
    &mut self,
    count: usize,
    visitor: &mut V,
    index_input: &mut I,
  ) -> Result<()>
  where
    V: IntersectVisitor,
  {
    let mut i: usize = 0;
    {
      while i < count {
        let length = DataInput::read_vint(index_input)?.try_convert()?;
        for dim in 0..self.config.num_dims {
          let prefix = self.common_prefix_lengths[dim];
          DataInput::read_bytes(
            index_input,
            &mut self.scratch_packed_values[..self.config.packed_bytes_length()],
            dim * self.config.bytes_per_dim + prefix,
            self.config.bytes_per_dim - prefix,
          )?;
        }
        self.scratch_iterator.reset(i, length);
        visitor.visit_iterator_with_packed_value(
          &mut self.scratch_iterator,
          &self.scratch_packed_values[..self.config.packed_bytes_length()],
        )?;
        i += length;
      }
    }

    if i != count {
      return Err(LuceneError::corrupt_index(format!(
        "Sub blocks do not add up to the expected count: {} != {}, (resource={})",
        count, i, index_input
      )));
    }

    Ok(())
  }

  // point is under commonPrefix
  pub fn visit_unique_raw_doc_values<V>(&mut self, count: usize, visitor: &mut V) -> Result<()>
  where
    V: IntersectVisitor,
  {
    self.scratch_iterator.reset(0, count);
    visitor.visit_iterator_with_packed_value(
      &mut self.scratch_iterator,
      &self.scratch_packed_values[..self.config.packed_bytes_length()],
    )?;
    Ok(())
  }
  fn visit_compressed_doc_values<V>(
    &mut self,
    count: usize,
    visitor: &mut V,
    compressed_dim: i32,
    index_input: &mut I,
  ) -> Result<()>
  where
    V: IntersectVisitor,
  {
    let bytes_per_dim = self.config.bytes_per_dim;
    let compressed_dim = compressed_dim as usize;

    // the byte at `compressedByteOffset` is compressed using run-length
    // compression, other suffix bytes are stored verbatim
    let compressed_byte_offset =
      compressed_dim * bytes_per_dim + self.common_prefix_lengths[compressed_dim];
    self.common_prefix_lengths[compressed_dim] += 1;

    let num_dims = self.config.num_dims;
    let common_prefix_lengths = &self.common_prefix_lengths[..num_dims];
    let scratch_data_packed_value =
      &mut self.scratch_packed_values[..self.config.packed_bytes_length()];
    let doc_ids = self.scratch_iterator.doc_ids.as_ref();
    let mut i = 0;
    {
      while i < count {
        scratch_data_packed_value[compressed_byte_offset] = DataInput::read_byte(index_input)?;
        let run_len = DataInput::read_byte(index_input)? as usize;
        for j in 0..run_len {
          // Preserve the measured indexed loop over the bounded dimension slice.
          #[allow(clippy::needless_range_loop)]
          for dim in 0..num_dims {
            let prefix = common_prefix_lengths[dim];
            DataInput::read_bytes(
              index_input,
              scratch_data_packed_value,
              dim * bytes_per_dim + prefix,
              bytes_per_dim - prefix,
            )?;
          }
          visitor.visit_with_packed_value(doc_ids[i + j], scratch_data_packed_value)?;
        }
        i += run_len;
      }
    }

    if i != count {
      return Err(LuceneError::corrupt_index(format!(
        "Sub blocks do not add up to the expected count: {} != {}, (resource={})",
        count, i, index_input,
      )));
    }

    Ok(())
  }
  fn read_compressed_dim(&mut self, leaf_node: &mut I) -> Result<i32> {
    let compressed_dim = DataInput::read_byte(leaf_node)? as i8 as i32;

    if compressed_dim < -2
      || compressed_dim >= self.config.num_dims as i32
      || (self.version < VERSION_LOW_CARDINALITY_LEAVES && compressed_dim == -2)
    {
      return Err(LuceneError::corrupt_index(format!(
        "Got compressedDim={} from input, (resource={})",
        compressed_dim, leaf_node,
      )));
    }

    Ok(compressed_dim)
  }

  pub fn read_common_prefixes(&mut self, index_input: &mut I) -> Result<()> {
    let num_dims = self.config.num_dims;
    for dim in 0..num_dims {
      let prefix = index_input.read_vint()?.try_convert()?;
      self.common_prefix_lengths[dim] = prefix;
      if prefix > 0 {
        DataInput::read_bytes(
          index_input,
          &mut self.scratch_packed_values[..self.config.packed_bytes_length()],
          dim * self.config.bytes_per_dim,
          prefix,
        )?;
      }
    }

    Ok(())
  }
}
impl<I> TryClone for BKDPointTree<I>
where
  I: IndexInput,
{
  fn try_clone(&self) -> Result<Self>
  where
    Self: Sized,
  {
    let mut index = BKDPointTree::with_scratch_iterator(
      self.inner_nodes.try_clone()?,
      self.leaf_nodes.clone(),
      self.config.clone(),
      self.leaf_node_offset,
      self.version,
      self.point_count,
      self.node_id,
      self.level,
      &self.packed_bounds[..self.config.packed_index_bytes_length()],
      &self.packed_bounds[self.config.packed_index_bytes_length()..],
      self.scratch_iterator.clone(),
      self.scratch_packed_values.to_vec(),
      self.common_prefix_lengths.to_vec(),
      self.is_tree_balanced,
    )?;

    let level = self.level;
    let dims = self.config.num_index_dims;

    index.leaf_positions[level].leaf_block_fp = self.leaf_positions[level].leaf_block_fp;

    if !self.is_leaf_node() {
      index.split_positions[level].right_node = self.split_positions[level].right_node;
      index.leaf_positions[level].read_node_data = self.leaf_positions[level].read_node_data;
      index.split_values_stack[level] = self.split_values_stack[level].clone();

      let src = level * dims;
      index
        .negative_deltas
        .copy_from(&self.negative_deltas[src..src + dims], src);

      index.split_positions[level].dimension = self.split_positions[level].dimension;
    }

    Ok(index)
  }
}

impl<I> PointTree for BKDPointTree<I>
where
  I: IndexInput,
{
  type MutablePointTree = DummyMutablePointTree;

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

  fn get_min_packed_value(&self) -> Result<Cow<'_, [u8]>> {
    Ok(Cow::Borrowed(
      &self.packed_bounds[..self.config.packed_index_bytes_length()],
    ))
  }

  fn get_max_packed_value(&self) -> Result<Cow<'_, [u8]>> {
    Ok(Cow::Borrowed(
      &self.packed_bounds[self.config.packed_index_bytes_length()..],
    ))
  }

  fn size(&self) -> Result<usize> {
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
      (num_leaves as usize - 1) * self.config.max_points_in_leaf_node
        + self.last_leaf_node_point_count
    } else {
      num_leaves as usize * self.config.max_points_in_leaf_node
    };

    Ok(size)
  }

  fn visit_doc_ids<IV>(&mut self, visitor: &mut IV) -> Result<()>
  where
    IV: IntersectVisitor,
  {
    let leaf_node = self.leaf_nodes.clone();
    self.reset_node_data_position()?;
    self.add_all(visitor, false, &mut *leaf_node.lock())
  }

  fn visit_doc_values<IV>(&mut self, visitor: &mut IV) -> Result<()>
  where
    IV: IntersectVisitor,
  {
    let leaf_node = self.leaf_nodes.clone();
    self.reset_node_data_position()?;
    self.visit_leaves_one_by_one(visitor, &mut *leaf_node.lock())
  }
}
/// Reusable [`DocIdSetIterator`] to handle low cardinality leaves.
#[derive(Clone)]
struct BKDReaderDocIDSetIterator {
  idx: usize,
  length: usize,
  offset: usize,
  doc_id: i32,
  doc_ids: Box<[i32]>,
  doc_ids_writer: DocIdsWriter,
}

impl BKDReaderDocIDSetIterator {
  pub fn new(max_points_in_leaf_node: usize) -> Self {
    Self {
      idx: 0,
      length: 0,
      offset: 0,
      doc_id: -1,
      doc_ids: vec![0; max_points_in_leaf_node].into_boxed_slice(),
      doc_ids_writer: DocIdsWriter::new(max_points_in_leaf_node),
    }
  }
  fn reset(&mut self, offset: usize, length: usize) {
    self.offset = offset;
    self.length = length;
    debug_assert!((offset + length) <= self.doc_ids.len());
    self.doc_id = -1;
    self.idx = 0;
  }
}
impl crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for BKDReaderDocIDSetIterator
{
}
impl crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for BKDReaderDocIDSetIterator {}

impl DocIdSetIterator for BKDReaderDocIDSetIterator {
  fn doc_id(&self) -> i32 {
    self.doc_id
  }

  fn next_doc(&mut self) -> Result<i32> {
    if self.idx == self.length {
      self.doc_id = NO_MORE_DOCS;
    } else {
      self.doc_id = self.doc_ids[self.offset + self.idx];
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

struct IntersectVisitorImpl {
  count: usize,
}
impl IntersectVisitorImpl {
  fn new() -> Self {
    Self { count: 0 }
  }
}
impl IntersectVisitor for IntersectVisitorImpl {
  fn visit(&mut self, _doc_id: i32) -> Result<()> {
    self.count += 1;
    Ok(())
  }

  fn visit_with_packed_value(&mut self, _doc_id: i32, _packed_value: &[u8]) -> Result<()> {
    Err(LuceneError::not_implemented(""))
  }

  fn compare(&self, _min_packed_value: &[u8], _max_packed_value: &[u8]) -> Result<Relation> {
    Err(LuceneError::not_implemented(""))
  }
}
