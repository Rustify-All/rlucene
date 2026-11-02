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
use crate::core::search::disi_wrapper::DisiWrapper;
use crate::core::search::scorer::Scorer;
use crate::core::util::error::lucene_error::{LuceneError, Result};
/// A priority queue of [`DocIdSetIterator`](crate::core::search::doc_id_set_iterator::DocIdSetIterator)s that orders by the current doc ID.
#[derive(Default)] // for std::mem::take
pub struct DisiPriorityQueue {
  size: usize,
  pub(crate) heap: Box<[usize]>,
}
impl DisiPriorityQueue {
  pub fn new(max_size: usize) -> Self {
    Self {
      size: 0,
      heap: vec![0; max_size].into_boxed_slice(),
    }
  }

  pub(crate) fn left_node(node: usize) -> usize {
    ((node + 1) << 1) - 1
  }

  pub(crate) fn right_node(node: usize) -> usize {
    node + 1
  }

  pub(crate) fn parent_node(node: usize) -> Option<usize> {
    if node == 0 {
      None
    } else {
      Some((node - 1) >> 1)
    }
  }

  pub fn size(&self) -> usize {
    self.size
  }

  pub fn top(&self) -> Option<usize> {
    if self.size == 0 {
      None
    } else {
      Some(self.heap[0])
    }
  }
  /// Return the 2nd least value in this heap, or None if the heap contains less than 2 values
  pub fn top2<S>(&self, wrappers: &[DisiWrapper<S>]) -> Option<usize>
  where
    S: Scorer,
  {
    match self.size() {
      0 | 1 => None,
      2 => Some(self.heap[1]),
      _ => {
        let left = self.heap[1];
        let right = self.heap[2];
        if wrappers[left].doc <= wrappers[right].doc {
          Some(left)
        } else {
          Some(right)
        }
      },
    }
  }
  /// Get the list of scorers which are on the current doc.
  pub fn top_list_root<S>(&self, wrappers: &mut [DisiWrapper<S>]) -> usize
  where
    S: Scorer,
  {
    let heap = &self.heap;
    let mut list_index = heap[0];
    wrappers[list_index].next = None;

    if self.size >= 3 {
      list_index = self.top_list(list_index, heap, wrappers, self.size, 1);
      list_index = self.top_list(list_index, heap, wrappers, self.size, 2);
    } else if self.size == 2 {
      let child = heap[1];
      if wrappers[child].doc == wrappers[list_index].doc {
        list_index = self.prepend(child, list_index, wrappers);
      }
    }

    list_index
  }
  fn prepend<S>(&self, w1_index: usize, w2_index: usize, wrappers: &mut [DisiWrapper<S>]) -> usize
  where
    S: Scorer,
  {
    wrappers[w1_index].next = Some(w2_index);
    w1_index
  }
  pub fn top_list<S>(
    &self,
    mut list: usize,
    heap: &[usize],
    wrappers: &mut [DisiWrapper<S>],
    size: usize,
    i: usize,
  ) -> usize
  where
    S: Scorer,
  {
    let w_index = heap[i];

    if wrappers[w_index].doc == wrappers[list].doc {
      list = self.prepend(w_index, list, wrappers);

      let left = Self::left_node(i);
      let right = left + 1;

      if right < size {
        list = self.top_list(list, heap, wrappers, size, left);
        list = self.top_list(list, heap, wrappers, size, right);
      } else if left < size {
        let left_index = heap[left];
        if wrappers[left_index].doc == wrappers[list].doc {
          list = self.prepend(left_index, list, wrappers);
        }
      }
    }

    list
  }

  pub fn add<S>(&mut self, entry: usize, wrappers: &[DisiWrapper<S>]) -> usize
  where
    S: Scorer,
  {
    self.heap[self.size] = entry;
    self.up_heap(self.size, wrappers);
    self.size += 1;
    self.heap[0]
  }
  pub fn add_all<S>(
    &mut self,
    entries: &[usize],
    offset: usize,
    len: usize,
    wrappers: &[DisiWrapper<S>],
  ) -> Result<()>
  where
    S: Scorer,
  {
    // Nothing to do if empty:
    if len == 0 {
      return Ok(());
    }
    // Fail early if we're going to over-fill:
    let new_size = self.size.checked_add(len).ok_or_else(|| {
      LuceneError::array_index_out_of_bounds(format!(
        "Cannot add {} elements to a queue with remaining capacity {}",
        len,
        self.heap.len().saturating_sub(self.size)
      ))
    })?;
    if new_size > self.heap.len() {
      return Err(LuceneError::array_index_out_of_bounds(format!(
        "Cannot add {} elements to a queue with remaining capacity {}",
        len,
        self.heap.len().saturating_sub(self.size)
      )));
    }
    let end = offset.checked_add(len).ok_or_else(|| {
      LuceneError::array_index_out_of_bounds(format!(
        "offset + len overflow: offset={offset}, len={len}"
      ))
    })?;
    if end > entries.len() {
      return Err(LuceneError::array_index_out_of_bounds(format!(
        "index out of bounds: offset={offset}, len={len}, entries.len()={}",
        entries.len()
      )));
    }
    // Copy the entries over to our heap array:
    for (idx, entry) in entries[offset..end].iter().enumerate() {
      self.heap[self.size + idx] = *entry;
    }
    self.size = new_size;
    // Heapify in bulk:
    let first_leaf_index = self.size >> 1;

    for root_index in (0..first_leaf_index).rev() {
      let mut parent_index = root_index;
      let parent = self.heap[parent_index];
      let parent_doc = wrappers[parent].doc;

      while parent_index < first_leaf_index {
        let mut child_index = Self::left_node(parent_index);
        let right_child_index = Self::right_node(child_index);

        let mut child = self.heap[child_index];

        if right_child_index < self.size {
          let right_child = self.heap[right_child_index];
          if wrappers[right_child].doc < wrappers[child].doc {
            child = right_child;
            child_index = right_child_index;
          }
        }

        if wrappers[child].doc >= parent_doc {
          break;
        }

        self.heap[parent_index] = child;
        parent_index = child_index;
      }

      self.heap[parent_index] = parent;
    }
    Ok(())
  }
  pub fn pop<S>(&mut self, wrappers: &[DisiWrapper<S>]) -> usize
  where
    S: Scorer,
  {
    let result = match self.top() {
      Some(top) => top,
      None => return 0,
    };
    self.size -= 1;
    let i = self.size;
    self.heap[0] = self.heap[i];
    self.heap[i] = 0;
    self.down_heap(i, wrappers);
    result
  }
  pub fn update_top<S>(&mut self, wrappers: &[DisiWrapper<S>]) -> usize
  where
    S: Scorer,
  {
    self.down_heap(self.size, wrappers);
    self.heap[0]
  }
  pub(crate) fn update_top_with<S>(
    &mut self,
    top_replacement: usize,
    wrappers: &[DisiWrapper<S>],
  ) -> usize
  where
    S: Scorer,
  {
    self.heap[0] = top_replacement;
    self.update_top(wrappers)
  }
  /// Clear the heap.
  pub fn clear(&mut self) {
    for v in self.heap.iter_mut() {
      *v = 0;
    }
    self.size = 0;
  }
  pub(crate) fn up_heap<S>(&mut self, mut i: usize, wrappers: &[DisiWrapper<S>])
  where
    S: Scorer,
  {
    let node_index = self.heap[i];
    let node_doc = wrappers[node_index].doc;

    while let Some(j) = Self::parent_node(i) {
      if node_doc >= wrappers[self.heap[j]].doc {
        break;
      }
      self.heap[i] = self.heap[j];
      i = j;
    }

    self.heap[i] = node_index;
  }
  pub fn down_heap<S>(&mut self, size: usize, wrappers: &[DisiWrapper<S>])
  where
    S: Scorer,
  {
    if size == 0 {
      return;
    }
    let mut i = 0;
    let node = self.heap[0];
    let mut j = Self::left_node(i);

    if j < size {
      let mut k = Self::right_node(j);

      if k < size && wrappers[self.heap[k]].doc < wrappers[self.heap[j]].doc {
        j = k;
      }

      if wrappers[self.heap[j]].doc < wrappers[node].doc {
        loop {
          self.heap[i] = self.heap[j];
          i = j;
          j = Self::left_node(i);
          k = Self::right_node(j);
          if k < size && wrappers[self.heap[k]].doc < wrappers[self.heap[j]].doc {
            j = k;
          }
          if j >= size || wrappers[self.heap[j]].doc >= wrappers[node].doc {
            break;
          }
        }
        self.heap[i] = node;
      }
    }
  }
  pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
    self.heap[..self.size].iter().cloned()
  }
}
