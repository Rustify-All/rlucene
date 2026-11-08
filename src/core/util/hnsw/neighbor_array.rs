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
use std::fmt;
use wide::bytemuck::{cast_slice, cast_slice_mut};

use crate::core::util::TryIntoInt;
use crate::core::util::accountable::Accountable;
use crate::core::util::core_helper::CoreHelper;
use crate::core::util::error::lucene_error::{LuceneError, Result};
#[cfg(test)]
use crate::core::util::hnsw::dummy::dummy_random_vector_scorer::DummyRandomVectorScorer;
use crate::core::util::hnsw::random_vector_scorer::RandomVectorScorer;
use crate::core::util::hnsw::random_vector_scorer_supplier::RandomVectorScorerSupplier;
use crate::core::util::ram_usage_estimator::size_of_vec;
/// [`NeighborArray`] encodes the neighbors of a node and their mutual scores in
/// the HNSW graph as a pair of growable arrays. Nodes are arranged in the
/// sorted order of their scores in:
///
/// - descending order if `DESC_ORDER` is `true`
/// - ascending order if `DESC_ORDER` is `false`
///
/// Valid Lucene vector ordinals fit in 32 bits. Adding a Rust-only node ID
/// larger than [`u32::MAX`] returns a conversion error.
#[derive(Clone, Default)]
pub struct NeighborArray<const DESC_ORDER: bool> {
  size: usize,
  // One aligned owner for the two Java-style 32-bit columns. For capacity N,
  // the first N cells are node ordinals and the last N cells are f32 scores.
  // Existing typed views keep the columns independent, including final node sorting.
  data: Vec<u64>,
  sorted_node_size: usize,
}

impl<const DESC_ORDER: bool> NeighborArray<DESC_ORDER> {
  /// Creates an array with the given capacity and the sort order fixed by its type.
  pub fn new(max_size: usize) -> Self {
    Self {
      size: 0,
      sorted_node_size: 0,
      data: vec![0; max_size],
    }
  }
  /// Add a new node to the [`NeighborArray`].
  /// The new node must be worse than all previously stored nodes.
  /// This cannot be called after [`Self::add_out_of_order`].
  pub fn add_in_order(&mut self, new_node: usize, new_score: f32) -> Result<()> {
    debug_assert!(
      self.size == self.sorted_node_size,
      "cannot call add_in_order after add_out_of_order"
    );

    if self.size == self.data.len() {
      return Err(LuceneError::illegal_state("No growth is allowed"));
    }

    if self.size > 0 {
      debug_assert!(
        {
          let previous_score = self.scores()[self.size - 1];
          if DESC_ORDER {
            previous_score >= new_score
          } else {
            previous_score <= new_score
          }
        },
        "Nodes are added in the incorrect order! Comparing {} to {:?}",
        new_score,
        &self.scores()[0..self.size]
      );
    }

    let size = self.size;
    self.nodes_mut()[size] = new_node.try_convert()?;
    let score_start = self.data.len();
    cast_slice_mut::<u64, f32>(&mut self.data)[score_start + size] = new_score;
    self.size += 1;
    self.sorted_node_size += 1;

    Ok(())
  }
  /// Adds `new_node` and `new_score` without inserting them in sorted order.
  pub fn add_out_of_order(&mut self, new_node: usize, new_score: f32) -> Result<()> {
    if self.size == self.data.len() {
      return Err(LuceneError::illegal_state("No growth is allowed"));
    }

    let size = self.size;
    let score_start = self.data.len();
    cast_slice_mut::<u64, f32>(&mut self.data)[score_start + size] = new_score;
    self.nodes_mut()[size] = new_node.try_convert()?;
    self.size += 1;

    Ok(())
  }
  /// In addition to [`Self::add_out_of_order`],
  /// this function will also remove the least-diverse node if the node
  /// array is full after insertion.
  ///
  /// In multi-threading environment, this method needs to be locked as it
  /// will be called by multiple threads, while the other add method is
  /// only supposed to be called by one thread.
  ///
  /// # Arguments
  ///
  /// * `node_id` - Node ID of the owner of this [`NeighborArray`](crate::core::util::hnsw::neighbor_array::NeighborArray).
  pub(crate) fn add_and_ensure_diversity<T>(
    &mut self,
    new_node: usize,
    new_score: f32,
    node_id: usize,
    scorer_supplier: &T,
  ) -> Result<()>
  where
    T: RandomVectorScorerSupplier,
  {
    self.add_out_of_order(new_node, new_score)?;

    if self.size < self.data.len() {
      return Ok(());
    }

    // We're oversize, need to drop the least diverse neighbor
    let worst_idx = self.find_worst_non_diverse(node_id, scorer_supplier)?;
    self.remove_index(worst_idx);

    debug_assert!(self.size == self.data.len() - 1);

    Ok(())
  }
  /// Sorts the array according to scores, and returns the sorted indexes of
  /// previously unsorted nodes (unchecked nodes).
  ///
  /// # Returns
  ///
  /// Indexes of newly sorted (unchecked) nodes, in ascending order, or `None`
  /// if the array is already fully sorted.
  pub(crate) fn sort<S>(&mut self, scorer: &mut S) -> Result<smallvec::SmallVec<[usize; 1]>>
  where
    S: RandomVectorScorer,
  {
    if self.size == self.sorted_node_size {
      // all nodes checked and sorted
      return Ok(smallvec::SmallVec::new());
    }

    debug_assert!(self.sorted_node_size < self.size);

    let mut unchecked_indexes =
      smallvec::SmallVec::with_capacity(self.size - self.sorted_node_size);

    while self.sorted_node_size != self.size {
      let inserted_index = self.insert_sorted_internal(scorer)?;
      for idx in &mut unchecked_indexes {
        if *idx >= inserted_index {
          *idx += 1;
        }
      }
      unchecked_indexes.push(inserted_index);
    }

    unchecked_indexes.sort_unstable();
    Ok(unchecked_indexes)
  }
  /// insert the first unsorted node into its sorted position
  fn insert_sorted_internal<S>(&mut self, scorer: &mut S) -> Result<usize>
  where
    S: RandomVectorScorer,
  {
    debug_assert!(
      self.sorted_node_size < self.size,
      "Call this method only when there's an unsorted node"
    );

    let tmp_node = self.nodes()[self.sorted_node_size];
    let mut tmp_score = self.scores()[self.sorted_node_size];

    if tmp_score.is_nan() {
      tmp_score = scorer.score(tmp_node as usize)?;
    }

    let insertion_point = if DESC_ORDER {
      self.desc_sort_find_rightmost_insertion_point(tmp_score, self.sorted_node_size)
    } else {
      self.asc_sort_find_rightmost_insertion_point(tmp_score, self.sorted_node_size)
    };

    // Move [insertion_point..sorted_node_size) one position to the right
    let sorted_node_size = self.sorted_node_size;
    self
      .nodes_mut()
      .copy_within(insertion_point..sorted_node_size, insertion_point + 1);
    let score_start = self.data.len();
    cast_slice_mut::<u64, f32>(&mut self.data)[score_start..]
      .copy_within(insertion_point..sorted_node_size, insertion_point + 1);

    self.nodes_mut()[insertion_point] = tmp_node;
    cast_slice_mut::<u64, f32>(&mut self.data)[score_start + insertion_point] = tmp_score;

    self.sorted_node_size += 1;
    Ok(insertion_point)
  }
  /// This method is for test only.
  #[cfg(test)]
  pub fn insert_sorted(&mut self, new_node: usize, new_score: f32) -> Result<()> {
    self.add_out_of_order(new_node, new_score)?;
    let mut v = DummyRandomVectorScorer;
    self.insert_sorted_internal(&mut v)?;
    Ok(())
  }
  pub fn size(&self) -> usize {
    self.size
  }
  /// Direct access to the 32-bit node ordinals, matching Java int[] storage.
  /// Provided for efficient graph writing without a conversion buffer.
  pub fn nodes(&self) -> &[u32] {
    &cast_slice::<u64, u32>(&self.data)[..self.data.len()]
  }
  pub fn nodes_mut(&mut self) -> &mut [u32] {
    let capacity = self.data.len();
    &mut cast_slice_mut::<u64, u32>(&mut self.data)[..capacity]
  }

  pub fn scores(&self) -> &[f32] {
    &cast_slice::<u64, f32>(&self.data)[self.data.len()..]
  }

  pub(crate) fn clear_with_capacity(&mut self, min_capacity: usize) {
    self.clear();
    if self.data.len() < min_capacity {
      self.data.resize(min_capacity, 0);
    }
  }

  pub fn clear(&mut self) {
    self.size = 0;
    self.sorted_node_size = 0;
  }

  pub(crate) fn remove_last(&mut self) {
    debug_assert!(self.size > 0);
    self.size -= 1;
    self.sorted_node_size = self.sorted_node_size.min(self.size);
  }

  pub(crate) fn remove_index(&mut self, idx: usize) {
    if idx == self.size - 1 {
      self.remove_last();
      return;
    }
    let size = self.size;
    self.nodes_mut().copy_within(idx + 1..size, idx);
    let score_start = self.data.len();
    cast_slice_mut::<u64, f32>(&mut self.data)[score_start..].copy_within(idx + 1..size, idx);

    if idx < self.sorted_node_size {
      self.sorted_node_size -= 1;
    }

    self.size -= 1;
  }
  fn asc_sort_find_rightmost_insertion_point(&self, new_score: f32, bound: usize) -> usize {
    let mut low = 0;
    let mut high = bound as isize - 1;
    let mut insertion_point = None;
    while low as isize <= high {
      let mid = (low + high as usize) / 2;
      match CoreHelper::compare_f32(self.scores()[mid], new_score) {
        std::cmp::Ordering::Less => low = mid + 1,
        std::cmp::Ordering::Greater => high = mid as isize - 1,
        std::cmp::Ordering::Equal => {
          insertion_point = Some(mid);
          break;
        },
      }
    }

    let Some(mut insertion_point) = insertion_point else {
      return low;
    };

    if insertion_point < bound {
      // Move right over equal values.
      while insertion_point < bound - 1
        && self.scores()[insertion_point + 1] == self.scores()[insertion_point]
      {
        insertion_point += 1;
      }
      insertion_point += 1;
    }

    insertion_point
  }

  /// Finds the rightmost insertion point in descending order (stable insert).
  pub fn desc_sort_find_rightmost_insertion_point(&self, new_score: f32, bound: usize) -> usize {
    let mut start = 0;
    let mut end = bound as isize - 1;
    while start as isize <= end {
      let mid = (start + end as usize) / 2;
      if self.scores()[mid] < new_score {
        end = mid as isize - 1;
      } else {
        start = mid + 1;
      }
    }
    start
  }
  /// Find first non-diverse neighbour among the list of neighbors starting
  /// from the most distant neighbours
  fn find_worst_non_diverse<S>(&mut self, node_ord: usize, scorer_supplier: &S) -> Result<usize>
  where
    S: RandomVectorScorerSupplier,
  {
    let unchecked_indexes = {
      let mut scorer = scorer_supplier.scorer(node_ord)?;
      self.sort(&mut scorer)?
    };

    debug_assert!(
      !unchecked_indexes.is_empty(),
      "We will always have something unchecked"
    );

    let mut unchecked_cursor = unchecked_indexes.len().checked_sub(1);

    for i in (1..self.size).rev() {
      let Some(cursor) = unchecked_cursor else {
        break; // no unchecked node left
      };

      let worst = self.is_worst_non_diverse(i, &unchecked_indexes, cursor, scorer_supplier)?;
      if worst {
        return Ok(i);
      }

      if i == unchecked_indexes[cursor] {
        unchecked_cursor = cursor.checked_sub(1);
      }
    }

    Ok(self.size - 1)
  }
  fn is_worst_non_diverse<S>(
    &self,
    candidate_index: usize,
    unchecked_indexes: &[usize],
    unchecked_cursor: usize,
    scorer_supplier: &S,
  ) -> Result<bool>
  where
    S: RandomVectorScorerSupplier,
  {
    let min_accepted_similarity = self.scores()[candidate_index];
    let candidate_node = self.nodes()[candidate_index];
    let scorer = scorer_supplier.scorer(candidate_node as usize)?;

    if candidate_index == unchecked_indexes[unchecked_cursor] {
      // the candidate itself is unchecked
      for i in (0..candidate_index).rev() {
        let neighbor_node = self.nodes()[i];
        let neighbor_similarity = scorer.score(neighbor_node as usize)?;
        if neighbor_similarity >= min_accepted_similarity {
          return Ok(true);
        }
      }
    } else {
      // else we just need to make sure candidate does not violate diversity with the
      // (newly inserted) unchecked nodes
      debug_assert!(candidate_index > unchecked_indexes[unchecked_cursor],);
      for &unchecked_idx in unchecked_indexes.iter().take(unchecked_cursor + 1).rev() {
        let neighbor_node = self.nodes()[unchecked_idx];
        let neighbor_similarity = scorer.score(neighbor_node as usize)?;
        if neighbor_similarity >= min_accepted_similarity {
          return Ok(true);
        }
      }
    }

    Ok(false)
  }
}

impl<const DESC_ORDER: bool> Accountable for NeighborArray<DESC_ORDER> {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(size_of_vec(&self.data))
  }
}
impl<const DESC_ORDER: bool> fmt::Display for NeighborArray<DESC_ORDER> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(
      f,
      "{}[{}]",
      concat!(module_path!(), "::NeighborArray"),
      self.size
    )
  }
}
