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
use crate::core::util::error::lucene_error::Result;
use crate::core::util::long_heap::LongHeap;
use crate::core::util::numeric_utils::NumericUtils;
/// [`NeighborQueue`] uses a [`LongHeap`] to store lists of arcs in an HNSW graph,
/// represented as a neighbor node ID with an associated score packed together
/// as a sortable `i64`, which is sorted primarily by score.
///
/// The queue provides both fixed-size and unbounded operations via
/// [`insert_with_overflow(i32, f32)`](NeighborQueue::insert_with_overflow) and
/// [`add(i32, f32)`](NeighborQueue::add).
///
/// The heap direction is fixed by `MAX_HEAP`: `false` for MIN, `true` for MAX.
/// It is part of the type and cannot change after construction.
pub struct NeighborQueue<const MAX_HEAP: bool> {
  pub heap: LongHeap,
  // Used to track the number of neighbors visited during a single graph traversal
  pub visited_count: i32,
  // Whether the search stopped early because it reached the visited nodes limit
  pub incomplete: bool,
}
impl<const MAX_HEAP: bool> NeighborQueue<MAX_HEAP> {
  pub fn new(initial_size: usize) -> Result<Self> {
    Ok(NeighborQueue {
      heap: LongHeap::new(initial_size)?,
      visited_count: 0,
      incomplete: false,
    })
  }
  // Same involution as Java's Order.apply, specialized for this queue's direction.
  // Unlike negation, -1 - v also reverses i64::MIN and i64::MAX correctly.
  fn apply_order(v: i64) -> i64 {
    if MAX_HEAP { -1 - v } else { v }
  }

  /// return the number of elements in the heap
  pub fn size(&self) -> usize {
    self.heap.size()
  }
  /// Adds a new graph arc, extending the storage as needed.
  ///
  /// # Arguments
  ///
  /// * `new_node` - The neighbor node ID.
  /// * `new_score` - The score of the neighbor, relative to some other node.
  pub fn add(&mut self, new_node: usize, new_score: f32) -> Result<()> {
    let encoded = self.encode(new_node, new_score);
    self.heap.push(encoded)?;
    Ok(())
  }
  /// If the heap is not full (size is less than the `initial_size` provided
  /// at creation), adds a new node-and-score element. If the heap
  /// is full, compares the score against the current top score, and
  /// replaces the top element if `new_score` is better than (greater than
  /// unless the heap is reversed) the current top score.
  ///
  /// # Arguments
  ///
  /// * `new_node` - The neighbor node ID.
  /// * `new_score` - The score of the neighbor, relative to some other node.
  pub fn insert_with_overflow(&mut self, new_node: usize, new_score: f32) -> Result<bool> {
    let encoded = self.encode(new_node, new_score);
    self.heap.insert_with_overflow(encoded)
  }
  /// Encodes the node ID and its similarity score as a `u64`, preserving the
  /// Lucene tie-breaking rule: when two scores are equal, the smaller
  /// node ID must win.
  ///
  /// The most significant 32 bits represent the float score, encoded as a
  /// sortable `i32`.
  ///
  /// The less significant 32 bits represent the node ID, but the bits are
  /// complemented to ensure that smaller node IDs are preferred when
  /// scores are equal.
  ///
  /// The bitwise AND with `0xFFFF_FFFFu64` is necessary to extract a `u64`
  /// where:
  ///
  /// - The most significant 32 bits are zero
  /// - The least significant 32 bits represent the complemented node ID
  ///
  /// # Arguments
  ///
  /// * `node` - The node ID
  /// * `score` - The node's similarity score
  ///
  /// # Returns
  ///
  /// A 64-bit encoded representation combining score and node ID.
  fn encode(&self, node: usize, score: f32) -> i64 {
    debug_assert!(node <= i64::MAX as usize);
    let encoded_score = NumericUtils::float_to_sortable_int(score) as i64;
    let encoded_node = !(node as i64) & 0xFFFF_FFFF;
    Self::apply_order((encoded_score << 32) | encoded_node)
  }

  fn decode_score(&self, heap_value: i64) -> f32 {
    let sortable = (Self::apply_order(heap_value) >> 32) as i32;
    NumericUtils::sortable_int_to_float(sortable)
  }

  fn decode_node_id(&self, heap_value: i64) -> usize {
    let v = (!Self::apply_order(heap_value)) as i32;
    v as usize
  }
  /// Removes the top element and returns its node id.
  pub fn pop(&mut self) -> Result<usize> {
    let v = self.heap.pop()?;
    Ok(self.decode_node_id(v))
  }

  pub fn nodes(&self) -> Vec<usize> {
    self.iter_nodes().collect()
  }

  pub(crate) fn iter_nodes(&self) -> impl ExactSizeIterator<Item = usize> + '_ {
    (0..self.size()).map(|i| self.decode_node_id(self.heap.get(i + 1)))
  }

  /// Returns the top element's node id.
  pub fn top_node(&self) -> usize {
    self.decode_node_id(self.heap.top())
  }
  /// Returns the top element's node score.
  ///
  /// - For a min-heap, this is the minimum score.
  /// - For a max-heap, this is the maximum score.
  pub fn top_score(&self) -> f32 {
    self.decode_score(self.heap.top())
  }

  pub fn clear(&mut self) {
    self.heap.clear();
    self.visited_count = 0;
    self.incomplete = false;
  }

  pub fn visited_count(&self) -> i32 {
    self.visited_count
  }

  pub fn set_visited_count(&mut self, visited_count: i32) {
    self.visited_count = visited_count;
  }

  pub fn incomplete(&self) -> bool {
    self.incomplete
  }

  pub fn mark_incomplete(&mut self) {
    self.incomplete = true;
  }
}
impl<const MAX_HEAP: bool> std::fmt::Display for NeighborQueue<MAX_HEAP> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "Neighbors[{}]", self.heap.size())
  }
}
