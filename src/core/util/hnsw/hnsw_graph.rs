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
use std::borrow::Borrow;
use std::sync::Arc;

use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::hnsw::neighbor_array::NeighborArray;
/// Hierarchical Navigable Small World (HNSW) graph.
///
/// Provides efficient approximate nearest neighbor search for high-dimensional
/// vectors. See the paper:
/// ["Efficient and robust approximate nearest neighbor search using Hierarchical Navigable Small World graphs (2018)"](https://arxiv.org/abs/1603.09320)
/// for algorithmic details.
///
/// The nomenclature here differs slightly from the paper:
///
/// ## Hyperparameters
///
/// - `beam_width` in
///   [`HnswGraphBuilder`](crate::core::util::hnsw::hnsw_graph_builder::HnswGraphBuilder)
///   corresponds to `efConst` in the
///   paper: it is the number of nearest neighbor candidates tracked while
///   searching the graph for each newly inserted node.
///
/// - `max_conn` corresponds to `M` in the paper: it controls how many of the
///   `efConst` neighbors are connected to the new node.
///
/// Note: The graph may be searched by multiple threads concurrently, but
/// **updates are not thread-safe**. The search method optionally takes a set of
/// *accepted nodes*, which can be used to exclude deleted documents.
pub trait HnswGraph {
  /// Whether traversal can exclusively borrow its current neighbor array.
  const HAS_BORROWED_NEIGHBORS: bool = false;

  /// Move the pointer to exactly the given `level`'s `target`.
  ///
  /// After this method returns, call [`next_neighbor()`](Self::next_neighbor)
  /// to return successive (ordered) connected node ordinals.
  ///
  /// - `level`: the level of the graph.
  /// - `target`: the ordinal of a node in the graph; must be `>= 0` and `<
  ///   [FloatVectorValues::size()]`.
  fn seek(&mut self, level: usize, target: usize) -> Result<()>;
  /// Returns the number of nodes in the graph
  fn size(&self) -> usize;
  /// Returns max node id, inclusive. Normally this value will be size - 1.
  fn max_node_id(&self) -> Option<usize> {
    Some(self.size() - 1)
  }
  /// Iterates over the neighbor list.
  ///
  /// It is illegal to call this method after it returns [`NO_MORE_DOCS`]
  /// without calling [`seek(level, target)`](Self::seek), which resets the
  /// iterator.
  ///
  /// # Returns
  ///
  /// A node ordinal in the graph, or [`NO_MORE_DOCS`] if the iteration is
  /// complete.
  fn next_neighbor(&mut self) -> Result<usize>;
  /// Borrows the current array and the graph's original next-neighbor index.
  ///
  /// Only implementations with [`Self::HAS_BORROWED_NEIGHBORS`] may use this
  /// entry. Success leaves the index unchanged. Failure, including a panic,
  /// must leave it as a failed first [`Self::next_neighbor`] call would.
  /// The array cannot change while borrowed; shared traversal must keep its
  /// original per-neighbor access and lock boundaries instead.
  fn borrow_neighbors(&mut self) -> Result<(&NeighborArray<true>, &mut usize)> {
    Err(LuceneError::unsupported_operation(
      "exclusive neighbor borrowing is unavailable",
    ))
  }
  /// Returns the number of levels of the graph
  fn num_levels(&self) -> Result<usize>;
  /// Returns graph's entry point on the top level *
  fn entry_node(&self) -> Result<Option<usize>>;
  type NodeIterator: NodesIterator;
  /// Get all nodes on a given level as node 0th ordinals.
  ///
  /// The nodes are **NOT** guaranteed to be presented in any particular
  /// order.
  ///
  /// # Arguments
  ///
  /// * `level` - Level for which to get all nodes.
  ///
  /// # Returns
  ///
  /// An iterator over nodes where `next_int()` returns the next node on the
  /// level.
  fn get_nodes_on_level(&mut self, level: usize) -> Result<Self::NodeIterator>;
  /// Executes an action with the [`NeighborArray`] connected to the given
  /// node.
  ///
  /// # Arguments
  ///
  /// * `level` - The level of the graph.
  /// * `node` - The node whose neighbors are returned, represented as an
  ///   ordinal on level 0.
  fn with_neighbors<T, F>(&self, _level: usize, _node: usize, _action: F) -> Result<T>
  where
    F: FnOnce(&NeighborArray<true>) -> Result<T>,
  {
    Err(LuceneError::unsupported_operation(""))
  }
}
impl<T> HnswGraph for Box<T>
where
  T: HnswGraph + ?Sized,
{
  const HAS_BORROWED_NEIGHBORS: bool = T::HAS_BORROWED_NEIGHBORS;

  fn seek(&mut self, level: usize, target: usize) -> Result<()> {
    (**self).seek(level, target)
  }

  fn size(&self) -> usize {
    (**self).size()
  }

  fn max_node_id(&self) -> Option<usize> {
    (**self).max_node_id()
  }

  fn next_neighbor(&mut self) -> Result<usize> {
    (**self).next_neighbor()
  }

  #[inline]
  fn borrow_neighbors(&mut self) -> Result<(&NeighborArray<true>, &mut usize)> {
    (**self).borrow_neighbors()
  }

  fn num_levels(&self) -> Result<usize> {
    (**self).num_levels()
  }

  fn entry_node(&self) -> Result<Option<usize>> {
    (**self).entry_node()
  }

  type NodeIterator = T::NodeIterator;

  fn get_nodes_on_level(&mut self, level: usize) -> Result<Self::NodeIterator> {
    (**self).get_nodes_on_level(level)
  }

  fn with_neighbors<R, F>(&self, level: usize, node: usize, action: F) -> Result<R>
  where
    F: FnOnce(&NeighborArray<true>) -> Result<R>,
  {
    (**self).with_neighbors(level, node, action)
  }
}
// Preserve the graph's mutable cursor when a search owns only its access handle.
// Shared on-heap graphs provide their separate implementation for `&OnHeapHnswGraph`.
impl<T> HnswGraph for &mut T
where
  T: HnswGraph + ?Sized,
{
  const HAS_BORROWED_NEIGHBORS: bool = T::HAS_BORROWED_NEIGHBORS;

  fn seek(&mut self, level: usize, target: usize) -> Result<()> {
    (**self).seek(level, target)
  }

  fn size(&self) -> usize {
    (**self).size()
  }

  fn max_node_id(&self) -> Option<usize> {
    (**self).max_node_id()
  }

  #[inline]
  fn next_neighbor(&mut self) -> Result<usize> {
    (**self).next_neighbor()
  }

  #[inline]
  fn borrow_neighbors(&mut self) -> Result<(&NeighborArray<true>, &mut usize)> {
    (**self).borrow_neighbors()
  }

  fn num_levels(&self) -> Result<usize> {
    (**self).num_levels()
  }

  fn entry_node(&self) -> Result<Option<usize>> {
    (**self).entry_node()
  }

  type NodeIterator = T::NodeIterator;

  fn get_nodes_on_level(&mut self, level: usize) -> Result<Self::NodeIterator> {
    (**self).get_nodes_on_level(level)
  }

  fn with_neighbors<R, F>(&self, level: usize, node: usize, action: F) -> Result<R>
  where
    F: FnOnce(&NeighborArray<true>) -> Result<R>,
  {
    (**self).with_neighbors(level, node, action)
  }
}

pub struct EmptyHnswGraph;
impl HnswGraph for EmptyHnswGraph {
  fn seek(&mut self, _level: usize, _target: usize) -> Result<()> {
    Ok(())
  }

  fn size(&self) -> usize {
    0
  }

  fn next_neighbor(&mut self) -> Result<usize> {
    Ok(NO_MORE_DOCS as usize)
  }

  fn num_levels(&self) -> Result<usize> {
    Ok(0)
  }

  fn entry_node(&self) -> Result<Option<usize>> {
    Ok(Some(0))
  }

  type NodeIterator = ArrayNodesIterator;

  fn get_nodes_on_level(&mut self, _level: usize) -> Result<Self::NodeIterator> {
    Ok(ArrayNodesIterator::empty())
  }
}
/// Iterator over the graph nodes on a certain level. Iterator also provides the
/// size – the total number of nodes to be iterated over. The nodes are NOT
/// guaranteed to be presented in any  particular order.
pub trait NodesIterator: Iterator<Item = usize> {
  ///  The number of elements in this iterator
  fn size(&self) -> usize;
  /// Consume integers from the iterator and place them into the `dest` array.
  ///
  /// # Arguments
  ///
  /// * `dest` - Where to put the integers.
  ///
  /// # Returns
  ///
  /// The number of integers written to `dest`.
  fn consume(&mut self, dest: &mut [usize]) -> Result<usize>;

  fn has_next(&self) -> bool;
}
macro_rules! define_nodes_iterator_enum {
    (
        $enum_name:ident,
        [$($V:ident),+ $(,)?]
    ) => {
        pub enum $enum_name<$($V),+> {
            $($V($V)),+
        }

        impl<$($V),+> Iterator for $enum_name<$($V),+>
        where
            $($V: Iterator<Item = usize>,)+
        {
            type Item = usize;

            fn next(&mut self) -> Option<Self::Item> {
                match self {
                    $(Self::$V(iter) => iter.next(),)+
                }
            }
        }

        impl<$($V),+> NodesIterator for $enum_name<$($V),+>
        where
            $($V: NodesIterator,)+
        {
            fn size(&self) -> usize {
                match self {
                    $(Self::$V(iter) => iter.size(),)+
                }
            }

            fn consume(&mut self, dest: &mut [usize]) -> Result<usize> {
                match self {
                    $(Self::$V(iter) => iter.consume(dest),)+
                }
            }

            fn has_next(&self) -> bool {
                match self {
                    $(Self::$V(iter) => iter.has_next(),)+
                }
            }
        }
    };
}
define_nodes_iterator_enum!(NodesIteratorEnum2, [A, B]);
pub fn get_sorted_nodes<I>(nodes: &mut I) -> Vec<usize>
where
  I: NodesIterator,
{
  let mut sorted = Vec::with_capacity(nodes.size());
  for v in nodes.by_ref() {
    sorted.push(v);
  }
  sorted.sort_unstable();
  sorted
}
/// NodesIterator that accepts nodes as an integer array.
pub struct ArrayNodesIterator<N = Arc<Vec<usize>>> {
  nodes: Option<N>,
  cur: usize,
  size: usize,
}

impl<N: Borrow<Vec<usize>>> ArrayNodesIterator<N> {
  /// Creates explicit node list (with partial length).
  pub fn from_nodes(nodes: Option<N>, size: usize) -> Self {
    debug_assert!(
      nodes
        .as_ref()
        .is_some_and(|nodes| size <= <N as Borrow<Vec<usize>>>::borrow(nodes).len())
    );
    Self {
      nodes,
      cur: 0,
      size,
    }
  }

  /// Creates implicit index-based iteration (0..size).
  pub fn from_size(size: usize) -> Self {
    Self {
      nodes: None,
      cur: 0,
      size,
    }
  }

  /// Shared empty singleton.
  pub fn empty() -> Self {
    Self::from_size(0)
  }
}

impl<N: Borrow<Vec<usize>>> Iterator for ArrayNodesIterator<N> {
  type Item = usize;

  fn next(&mut self) -> Option<Self::Item> {
    if !self.has_next() {
      None
    } else {
      let value = match &self.nodes {
        Some(vec) => <N as Borrow<Vec<usize>>>::borrow(vec)[self.cur],
        None => self.cur,
      };
      self.cur += 1;
      Some(value)
    }
  }
}

impl<N: Borrow<Vec<usize>>> NodesIterator for ArrayNodesIterator<N> {
  fn size(&self) -> usize {
    self.size
  }

  fn consume(&mut self, dest: &mut [usize]) -> Result<usize> {
    if !self.has_next() {
      return Err(LuceneError::no_such_element(""));
    }
    let num_to_copy = std::cmp::min(dest.len(), self.size - self.cur);
    match &self.nodes {
      Some(vec) => {
        let vec: &Vec<usize> = vec.borrow();
        let copy_source = &vec[self.cur..self.cur + num_to_copy];
        dest[..copy_source.len()].copy_from_slice(copy_source);
        self.cur += num_to_copy;
        Ok(num_to_copy)
      },
      None => {
        for (i, slot) in dest.iter_mut().enumerate().take(num_to_copy) {
          *slot = self.cur + i;
        }
        Ok(num_to_copy)
      },
    }
  }

  fn has_next(&self) -> bool {
    self.cur < self.size
  }
}

/// Nodes iterator based on set representation of nodes.
pub struct CollectionNodesIterator {
  nodes: Arc<Option<Vec<usize>>>,
  index: usize,
  size: usize,
}

impl CollectionNodesIterator {
  /// Constructs a new iterator from a collection of nodes.
  pub fn new(nodes: Arc<Option<Vec<usize>>>) -> Self {
    let size = nodes.as_ref().as_ref().map_or(0, |v| v.len());
    Self {
      nodes,
      index: 0,
      size,
    }
  }
}

impl Iterator for CollectionNodesIterator {
  type Item = usize;

  fn next(&mut self) -> Option<Self::Item> {
    if self.size == 0 {
      return None;
    }
    let vec = self.nodes.as_ref().as_ref()?;
    if self.index < vec.len() {
      let val = vec[self.index];
      self.index += 1;
      Some(val)
    } else {
      None
    }
  }
}

impl NodesIterator for CollectionNodesIterator {
  fn size(&self) -> usize {
    self.size
  }

  fn consume(&mut self, dest: &mut [usize]) -> Result<usize> {
    if !self.has_next() {
      return Err(LuceneError::no_such_element(""));
    }
    let mut dest_index = 0;
    while self.has_next() && dest_index < dest.len() {
      dest[dest_index] = self
        .next()
        .ok_or_else(|| LuceneError::no_such_element(""))?;
      dest_index += 1;
    }
    Ok(dest_index)
  }

  fn has_next(&self) -> bool {
    self.index < self.size
  }
}
