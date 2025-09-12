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
use crate::core::util::error::lucene_error::{LuceneError, Result};
/// A bounded min heap that stores `f32` values. The top element is the lowest
/// value of the heap.
///
/// A primitive priority queue that maintains a partial ordering of its elements
/// such that the least element can always be found in constant time.
///
/// Implementation is based on [`LongHeap`](crate::core::util::long_heap::LongHeap).
pub struct FloatHeap {
    max_size: usize,
    heap: Vec<f32>,
    size: usize,
}

impl FloatHeap {
    /// Creates a new FloatHeap with fixed capacity.
    pub fn new(max_size: usize) -> Result<Self> {
        if max_size < 1 {
            return Err(LuceneError::illegal_argument(format!(
                "max_size must be > 0; got: {max_size}"
            )));
        }
        Ok(Self {
            max_size,
            heap: vec![0.0; max_size + 1],
            size: 0,
        })
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
    /// Whether the value was added (unless the heap is full, or the new value
    /// is less than the top value).
    pub fn offer(&mut self, value: f32) -> bool {
        if self.size >= self.max_size {
            if value < self.heap[1] {
                return false;
            }
            self.update_top(value);
            return true;
        }
        self.push(value);
        true
    }

    pub fn get_heap(&self) -> Vec<f32> {
        self.heap[1..=self.size].to_vec()
    }
    /// Removes and returns the head of the heap.
    ///
    /// # Returns
    ///
    /// The head of the heap, the smallest value.
    ///
    /// # Errors
    ///
    /// Returns an error if the heap is empty.
    pub fn poll(&mut self) -> Result<f32> {
        if self.size == 0 {
            return Err(LuceneError::illegal_state("The heap is empty"));
        }
        let result = self.heap[1];
        self.heap[1] = self.heap[self.size];
        self.size -= 1;
        self.down_heap(1);
        Ok(result)
    }

    /// Retrieves, but does not remove, the head of this heap.
    ///
    /// # Returns
    ///
    /// The head of the heap, the smallest value.
    pub fn peek(&self) -> f32 {
        self.heap[1]
    }
    /// Returns the number of elements in this heap.
    ///
    /// # Returns
    ///
    /// The number of elements in this heap.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Clears the heap.
    pub fn clear(&mut self) {
        self.size = 0;
    }

    fn push(&mut self, element: f32) {
        self.size += 1;
        self.heap[self.size] = element;
        self.up_heap(self.size);
    }

    fn update_top(&mut self, value: f32) -> f32 {
        self.heap[1] = value;
        self.down_heap(1);
        self.heap[1]
    }

    fn down_heap(&mut self, mut i: usize) {
        let value = self.heap[i]; // save top value
        let mut j = i << 1; // find smaller child
        let mut k = j + 1;

        if k <= self.size && self.heap[k] < self.heap[j] {
            j = k;
        }

        while j <= self.size && self.heap[j] < value {
            self.heap[i] = self.heap[j]; // shift up child
            i = j;
            j = i << 1;
            k = j + 1;
            if k <= self.size && self.heap[k] < self.heap[j] {
                j = k;
            }
        }

        self.heap[i] = value; // install saved value
    }

    fn up_heap(&mut self, mut i: usize) {
        let value = self.heap[i]; // save bottom value
        let mut j = i >> 1;
        while j > 0 && value < self.heap[j] {
            self.heap[i] = self.heap[j]; // shift parents down
            i = j;
            j >>= 1;
        }
        self.heap[i] = value; // install saved value
    }
}

#[cfg(test)]
mod tests {
    use rand::Rng;

    use crate::core::util::error::lucene_error::Result;
    use crate::core::util::hnsw::float_heap::FloatHeap;

    #[allow(dead_code)] // for quick search
    struct TestFloatHeap;
    #[test]
    fn test_basic_operations() -> Result<()> {
        let mut heap = FloatHeap::new(3)?;

        heap.offer(2.0);
        heap.offer(4.0);
        heap.offer(1.0);
        heap.offer(3.0);
        assert_eq!(heap.size(), 3);
        assert!((heap.peek() - 2.0).abs() < f32::EPSILON);
        assert!((heap.poll()? - 2.0).abs() < f32::EPSILON);
        assert!((heap.poll()? - 3.0).abs() < f32::EPSILON);
        assert!((heap.poll()? - 4.0).abs() < f32::EPSILON);
        assert_eq!(heap.size(), 0);
        Ok(())
    }
    use crate::test::util::lucene_test_case::lucene_test_case_util::{at_least, random};

    #[test]
    fn test_basic_operations2() -> Result<()> {
        let mut random = random();
        let size = at_least(&mut random, 10);
        let mut heap = FloatHeap::new(size as usize)?;

        let mut sum = 0.0;
        let mut sum2 = 0.0;

        for _ in 0..size {
            let next: f32 = random.random_range(0.0..100.0);
            sum += next as f64;
            heap.offer(next);
        }

        let mut last = f32::NEG_INFINITY;
        for _ in 0..size {
            let next = heap.poll()?;
            assert!(next >= last);
            last = next;
            sum2 += last as f64;
        }

        assert!((sum - sum2).abs() < 0.01);
        Ok(())
    }
    #[test]
    fn test_clear() -> Result<()> {
        let mut heap = FloatHeap::new(3)?;

        heap.offer(20.0);
        heap.offer(40.0);
        heap.offer(30.0);

        assert_eq!(heap.size(), 3);
        assert!((heap.peek() - 20.0).abs() < f32::EPSILON);

        heap.clear();
        assert_eq!(heap.size(), 0);
        assert!((heap.peek() - 20.0).abs() < f32::EPSILON);

        heap.offer(15.0);
        heap.offer(35.0);

        assert_eq!(heap.size(), 2);
        assert!((heap.peek() - 15.0).abs() < f32::EPSILON);

        assert!((heap.poll()? - 15.0).abs() < f32::EPSILON);
        assert!((heap.poll()? - 35.0).abs() < f32::EPSILON);
        assert_eq!(heap.size(), 0);

        Ok(())
    }
}
