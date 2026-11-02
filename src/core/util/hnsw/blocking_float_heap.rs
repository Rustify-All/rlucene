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
use parking_lot::{Mutex, MutexGuard};

use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;
/// A blocking bounded min heap that stores `f32` values. The top element is the
/// smallest value in the heap.
///
/// This is a primitive priority queue that maintains a partial ordering of its
/// elements, ensuring the minimum element can always be accessed in constant
/// time.
///
/// The implementation is based on [LongHeap`](crate::core::util::long_heap::LongHeap)
/// from Lucene.
pub struct BlockingFloatHeap {
  max_size: usize,
  lock: Mutex<Inner>,
}
pub struct Inner {
  heap: Box<[f32]>,
  size: usize,
}
impl BlockingFloatHeap {
  pub fn new(max_size: usize) -> Self {
    let heap = vec![0f32; max_size + 1].into_boxed_slice();
    let inner = Inner { heap, size: 0 };
    Self {
      max_size,
      lock: Mutex::new(inner),
    }
  }
  /// Inserts a value into this heap.
  ///
  /// If the number of values would exceed the heap's `max_size`, the least
  /// value is discarded.
  ///
  /// # Arguments
  ///
  /// * `value` - The value to add.
  ///
  /// # Returns
  ///
  /// The new 'top' element in the queue.
  pub fn offer(&self, value: f32) -> f32 {
    let mut inner = self.lock.lock();

    if inner.size < self.max_size {
      Self::push(&mut inner, value);
    } else if value >= inner.heap[1] {
      Self::update_top(&mut inner, value);
    }

    inner.heap[1]
  }
  /// Inserts an array of values into this heap.
  ///
  /// Values must be sorted in ascending order.
  ///
  /// # Arguments
  ///
  /// * `values` - A slice of values to insert, must be sorted in ascending
  ///   order.
  /// * `len` - Number of values from the `values` slice to insert.
  ///
  /// # Returns
  ///
  /// The new 'top' element in the queue.
  pub fn offer_array(&self, values: &[f32], len: usize) -> f32 {
    let mut inner = self.lock.lock();

    for i in (0..len).rev() {
      if inner.size < self.max_size {
        Self::push(&mut inner, values[i]);
      } else if values[i] >= inner.heap[1] {
        Self::update_top(&mut inner, values[i]);
      } else {
        break;
      }
    }

    inner.heap[1]
  }
  /// Removes and returns the head of the heap.
  ///
  /// # Returns
  ///
  /// The head of the heap, the smallest value.
  ///
  /// # Error
  ///
  /// Error if the heap is empty.
  pub fn poll(&self) -> Result<f32> {
    let mut inner = self.lock.lock();
    if inner.size == 0 {
      return Err(LuceneError::illegal_state("The heap is empty"));
    }

    let result = inner.heap[1];
    inner.heap[1] = inner.heap[inner.size];
    inner.size -= 1;
    Self::down_heap(&mut inner, 1);
    Ok(result)
  }

  /// Retrieves, but does not remove, the head of this heap.
  ///
  /// # Returns
  ///
  /// The head of the heap, the smallest value.
  pub fn peek(&self) -> f32 {
    let inner = self.lock.lock();
    inner.heap[1]
  }
  /// Returns the number of elements in this heap.
  ///
  /// # Returns
  ///
  /// The number of elements in this heap.
  pub fn size(&self) -> usize {
    let inner = self.lock.lock();
    inner.size
  }
  fn push(inner: &mut MutexGuard<'_, Inner>, element: f32) {
    inner.size += 1;
    let size = inner.size;
    inner.heap[size] = element;
    Self::up_heap(inner, size)
  }

  fn update_top(inner: &mut MutexGuard<'_, Inner>, value: f32) -> f32 {
    inner.heap[1] = value;
    Self::down_heap(inner, 1);
    inner.heap[1]
  }
  fn down_heap(inner: &mut MutexGuard<'_, Inner>, mut i: usize) {
    let value = inner.heap[i]; // save top value
    let mut j = i << 1; // find smaller child
    let mut k = j + 1;

    if k <= inner.size && inner.heap[k] < inner.heap[j] {
      j = k;
    }

    while j <= inner.size && inner.heap[j] < value {
      inner.heap[i] = inner.heap[j]; // shift up child
      i = j;
      j = i << 1;
      k = j + 1;
      if k <= inner.size && inner.heap[k] < inner.heap[j] {
        j = k;
      }
    }

    inner.heap[i] = value; // install saved value
  }
  fn up_heap(inner: &mut MutexGuard<'_, Inner>, orig_pos: usize) {
    let mut i = orig_pos;
    let value = inner.heap[i];
    let mut j = i >> 1;
    while j > 0 && value < inner.heap[j] {
      inner.heap[i] = inner.heap[j];
      i = j;
      j >>= 1;
    }
    inner.heap[i] = value;
  }
}
