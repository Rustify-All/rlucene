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
#[cfg(test)]
use crate::core::codecs::hnsw::hnsw_graph_provider::HnswGraphProvider;
#[cfg(test)]
use crate::core::index::codec_reader::CodecReader;
#[cfg(test)]
use crate::core::index::index_reader::{
  IndexReader, IndexReaderContextKind, IndexReaderContextType,
};
#[cfg(test)]
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::hnsw::hnsw_graph::{HnswGraph, NodesIterator};
use smallvec::SmallVec;

/// Utilities for use in tests involving HNSW graphs
pub struct HnswUtil;
impl HnswUtil {
  /*
  For each level, check rooted components from previous level nodes, which are entry
  points with the goal that each node should be reachable from *some* entry point.  For each entry
  point, compute a spanning tree, recording the nodes in a single shared bitset.

  Also record a bitset marking nodes that are not full to be used when reconnecting in order to
  limit the search to include non-full nodes only.
  */
  /// Returns true if every node on every level is reachable from node 0.
  #[cfg(test)]
  pub(crate) fn is_rooted<G>(hnsw: &mut G) -> Result<bool>
  where
    G: HnswGraph,
  {
    for level in 0..hnsw.num_levels()? {
      let comps = Self::components(hnsw, level, None, 0)?;
      if comps.len() > 1 {
        return Ok(false);
      }
    }
    Ok(true)
  }
  /// Returns the sizes of the distinct graph components on level 0. If the
  /// graph is fully-rooted the list will have one entry. If it is empty, the
  /// returned list will be empty.
  #[cfg(test)]
  pub(crate) fn component_sizes<G>(hnsw: &mut G) -> Result<Vec<usize>>
  where
    G: HnswGraph,
  {
    Self::component_sizes_on_level(hnsw, 0)
  }
  /// Returns the sizes of the distinct graph components on the given level.
  /// The forest starting at the entry points (nodes in the next highest
  /// level) is considered as a single component. If the entire graph is
  /// rooted in the entry points--that is, every node is reachable from at
  /// least one entry point--the returned list will have a single entry. If
  /// the graph is empty, the returned list will be empty.
  #[cfg(test)]
  pub(crate) fn component_sizes_on_level<G>(hnsw: &mut G, level: usize) -> Result<Vec<usize>>
  where
    G: HnswGraph,
  {
    let comps = Self::components(hnsw, level, None, 0)?;
    Ok(comps.into_iter().map(|c| c.size).collect())
  }

  fn get_total<N, G>(
    nodes_iter: N,
    hnsw: &mut G,
    level: usize,
    mut not_fully_connected: Option<&mut FixedBitSet>,
    connected_nodes: &mut FixedBitSet,
    max_conn: usize,
  ) -> Result<usize>
  where
    N: Iterator<Item = usize>,
    G: HnswGraph,
  {
    let mut total = 0;
    for entry_point in nodes_iter {
      let component = Self::mark_rooted(
        hnsw,
        level,
        connected_nodes,
        not_fully_connected.as_deref_mut(),
        max_conn,
        entry_point,
      )?;
      total += component.size;
    }
    Ok(total)
  }

  pub(crate) fn components<G>(
    hnsw: &mut G,
    level: usize,
    mut not_fully_connected: Option<&mut FixedBitSet>,
    max_conn: usize,
  ) -> Result<Vec<Component>>
  where
    G: HnswGraph,
  {
    let mut components = Vec::new();
    debug_assert!(hnsw.size() <= i32::MAX as usize);
    let mut connected_nodes = FixedBitSet::new(hnsw.size());

    debug_assert_eq!(hnsw.size(), hnsw.get_nodes_on_level(0)?.size());

    if level >= hnsw.num_levels()? {
      return Err(LuceneError::illegal_argument(format!(
        "Level {} too large for graph with {} levels",
        level,
        hnsw.num_levels()?
      )));
    }

    let mut total = if level == hnsw.num_levels()? - 1 {
      let entry_point = hnsw.entry_node()?;
      debug_assert!(entry_point.is_some());
      let iter = std::iter::once(entry_point.unwrap_or(0));
      Self::get_total(
        iter,
        hnsw,
        level,
        not_fully_connected.as_deref_mut(),
        &mut connected_nodes,
        max_conn,
      )?
    } else {
      let iter = hnsw.get_nodes_on_level(level + 1)?;
      Self::get_total(
        iter,
        hnsw,
        level,
        not_fully_connected.as_deref_mut(),
        &mut connected_nodes,
        max_conn,
      )?
    };

    let entry_point = if let Some(nfc) = not_fully_connected.as_ref() {
      nfc.next_set_bit(0)
    } else {
      connected_nodes.next_set_bit(0)
    };

    components.push(Component {
      start: entry_point,
      size: total,
    });

    if level == 0 {
      let mut next_clear = Self::next_clear_bit(&connected_nodes, 0);
      while next_clear != NO_MORE_DOCS as usize {
        let component = Self::mark_rooted(
          hnsw,
          level,
          &mut connected_nodes,
          not_fully_connected.as_deref_mut(),
          max_conn,
          next_clear,
        )?;
        debug_assert!(component.size > 0);
        components.push(component);
        total += component.size;
        next_clear = Self::next_clear_bit(&connected_nodes, component.start);
      }
    } else {
      let mut nodes = hnsw.get_nodes_on_level(level)?;
      for node in &mut nodes {
        if connected_nodes.get(node)? {
          continue;
        }
        let component = Self::mark_rooted(
          hnsw,
          level,
          &mut connected_nodes,
          not_fully_connected.as_deref_mut(),
          max_conn,
          node,
        )?;
        debug_assert!(component.size > 0);
        components.push(component);
        total += component.size;
      }
    }

    debug_assert_eq!(
      total,
      hnsw.get_nodes_on_level(level)?.size(),
      "Mismatch total={total} vs node size on level {level}"
    );

    Ok(components)
  }
  /// Count the nodes in a rooted component of the graph and mark them in the
  /// `connected_nodes` bitset. "Rooted" means all nodes reachable from a
  /// specific root node.
  ///
  /// # Parameters
  ///
  /// - `hnsw_graph`: the graph to inspect
  /// - `level`: the specific level of the graph to inspect
  /// - `connected_nodes`: a bitset with the size equal to the number of nodes
  ///   in the graph; this method will mark bits of all nodes reachable from
  ///   the entry point
  /// - `not_fully_connected`: optional bitset (same size) to mark visited
  ///   nodes that have fewer than `max_conn` connections
  /// - `max_conn`: the maximum number of neighbors a node can have (i.e., M)
  /// - `entry_point`: the node ID from which traversal begins
  fn mark_rooted<G>(
    hnsw_graph: &mut G,
    level: usize,
    connected_nodes: &mut FixedBitSet,
    mut not_fully_connected: Option<&mut FixedBitSet>,
    max_conn: usize,
    entry_point: usize,
  ) -> Result<Component>
  where
    G: HnswGraph,
  {
    // Start at entry point and search all nodes on this level
    let mut stack = SmallVec::<[usize; 4]>::new();
    stack.push(entry_point);
    let mut count = 0;

    while let Some(node) = stack.pop() {
      if connected_nodes.get(node)? {
        continue;
      }
      count += 1;
      connected_nodes.set(node)?;
      hnsw_graph.seek(level, node)?;

      let mut friend_count = 0;
      let mut friend_ord;
      while {
        friend_ord = hnsw_graph.next_neighbor()?;
        friend_ord != NO_MORE_DOCS as usize
      } {
        friend_count += 1;
        stack.push(friend_ord);
      }

      if friend_count < max_conn
        && let Some(nfc) = not_fully_connected.as_deref_mut()
      {
        nfc.set(node)?;
      }
    }

    Ok(Component {
      start: entry_point,
      size: count,
    })
  }
  fn next_clear_bit(bits: &FixedBitSet, index: usize) -> usize {
    let barray = bits.get_bits();
    debug_assert!(
      index < bits.length(),
      "index={}, num_bits={}",
      index,
      bits.length()
    );

    let mut i = index >> 6;
    let mut word = !barray[i].wrapping_shr(index as u32);
    let mut next: usize = NO_MORE_DOCS as usize;
    if word != 0 {
      next = index + word.trailing_zeros() as usize;
    } else {
      i += 1;
      while i < barray.len() {
        word = !barray[i];
        if word != 0 {
          next = (i << 6) + word.trailing_zeros() as usize;
          break;
        }
        i += 1;
      }
    }

    if next >= bits.length() {
      NO_MORE_DOCS as usize
    } else {
      next
    }
  }
  /// In graph theory, "connected components" are formally defined for
  /// undirected (i.e., bidirectional) graphs. The HNSW graph used here is
  /// directed due to pruning, but it is *mostly* undirected.
  ///
  /// This method evaluates connectivity starting from a single node,
  /// effectively checking whether the graph is a "rooted graph".
  #[cfg(test)]
  pub fn graph_is_rooted<IR>(reader: IR, vector_field: &str) -> Result<bool>
  where
    IR: IndexReader,
    IR::ContextKind: IndexReaderContextKind<IR>,
    IRCLeafReader<IndexReaderContextType<IR>>: CodecReader,
    <IRCLeafReader<IndexReaderContextType<IR>> as CodecReader>::KnnVectorsReader: HnswGraphProvider,
  {
    let context = reader.get_context()?;
    for leaf in context.leaves()? {
      let vector_reader = leaf
        .reader()
        .get_vector_reader()?
        .ok_or_else(|| LuceneError::illegal_state("vector reader is missing"))?;
      if !vector_reader.is_hnsw_graph_provider(vector_field) {
        return Err(LuceneError::illegal_state(format!(
          "vector reader for field {vector_field} does not provide an HNSW graph"
        )));
      }
      let mut graph = vector_reader.get_graph(vector_field)?;
      if !Self::is_rooted(&mut graph)? {
        return Ok(false);
      }
    }
    Ok(true)
  }
}
/// A component (also called "connected component") of an undirected graph is a
/// set of nodes that are connected via neighbor links: every node in the
/// component is reachable from every other node in the same component.  
///
/// See: [Component (graph theory)](https://en.wikipedia.org/wiki/Component_(graph_theory)).
///
/// Such a graph is considered "fully connected" *iff* it has a single
/// component, or it is empty.
///
/// - `start`: the lowest-numbered node in the component
/// - `size`: the number of nodes in the component
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Component {
  pub start: usize,
  pub size: usize,
}
