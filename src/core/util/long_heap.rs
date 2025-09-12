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
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
/// A min heap that stores `i64` values.
/// This is a primitive priority queue that maintains a partial ordering of
/// elements such that the smallest element can always be found in constant
/// time.
///
/// `push()` and `pop()` require O(log(n)) time complexity.
/// This heap supports both unbounded growth (via `push()`) and bounded-size
/// insertion (via `insert_with_overflow()`).
///
/// The heap is 1-based internally: index 0 is unused.
pub struct LongHeap {
    max_size: usize,
    heap: Vec<i64>,
    size: usize,
}

impl LongHeap {
    /// Create an empty priority queue of the configured initial size.
    ///
    /// # Arguments
    ///
    /// * `max_size` - The maximum size of the heap. Must be > 0 and <
    ///   ArrayUtil::MAX_ARRAY_LENGTH.
    ///
    /// # Errors
    ///
    /// Returns `Err` if `max_size` is invalid to prevent confusing
    /// out-of-memory errors.
    pub fn new(max_size: i32) -> Result<Self> {
        // TODO
        // if max_size < 1 || max_size >= ArrayUtil::MAX_ARRAY_LENGTH {
        if max_size < 1 {
            return Err(LuceneError::illegal_argument(format!(
                "max_size must be > 0 and < {}; got: {}",
                ArrayUtil::MAX_ARRAY_LENGTH - 1,
                max_size
            )));
        }
        // We add +1 because index 0 is unused.
        let heap_size = max_size + 1;
        let heap = vec![0i64; heap_size as usize];

        Ok(Self {
            max_size: max_size as usize,
            heap,
            size: 0,
        })
    }
    /// Adds a value in O(log(n)) time. Grows unbounded as needed to accommodate
    /// new values. Returns the new top element.
    pub fn push(&mut self, element: i64) -> i64 {
        self.size += 1;
        if self.size == self.heap.len() {
            let new_capacity = (self.size * 3).div_ceil(2);
            debug_assert!(new_capacity <= i32::MAX as usize);
            ArrayUtil::grow_with_len(&mut self.heap, new_capacity);
        }
        self.heap[self.size] = element;
        self.up_heap(self.size);
        self.heap[1]
    }
    /// Adds a value in O(log(n)) time. If the number of values would exceed
    /// `max_size`, the least value is discarded.
    ///
    /// Returns whether the value was added.
    pub fn insert_with_overflow(&mut self, value: i64) -> bool {
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
    /// Returns the least element of the heap in constant time.
    /// The caller must ensure the heap is not empty.
    pub fn top(&self) -> i64 {
        self.heap[1]
    }

    /// Removes and returns the least element of the heap in O(log(n)) time.
    ///
    /// # Errors
    ///
    /// Returns error if the heap is empty.
    pub fn pop(&mut self) -> Result<i64> {
        if self.size > 0 {
            let result = self.heap[1];
            self.heap[1] = self.heap[self.size];
            self.size -= 1;
            self.down_heap(1);
            Ok(result)
        } else {
            Err(LuceneError::illegal_state("The heap is empty"))
        }
    }
    /// Replaces the top of the heap with `new_top`.
    /// This is faster than calling `pop()` followed by `push()`.
    /// No-op if the heap is empty.
    pub fn update_top(&mut self, value: i64) -> i64 {
        if self.size > 0 {
            self.heap[1] = value;
            self.down_heap(1);
        }
        self.heap[1]
    }

    /// Returns the number of elements currently stored in the heap.
    pub fn size(&self) -> i32 {
        debug_assert!(self.size <= i32::MAX as usize);
        self.size as i32
    }

    /// Removes all entries from the heap.
    pub fn clear(&mut self) {
        self.size = 0;
    }
    fn up_heap(&mut self, mut i: usize) {
        let value = self.heap[i]; // save bottom value
        let mut j = i >> 1;
        while j > 0 && value < self.heap[j] {
            self.heap[i] = self.heap[j]; // shift parents down
            i = j;
            j >>= 1;
        }
        self.heap[i] = value;
    }

    fn down_heap(&mut self, mut i: usize) {
        let value = self.heap[i];
        let mut j = i << 1;
        let mut k = j + 1;

        if k <= self.size && self.heap[k] < self.heap[j] {
            j = k;
        }

        while j <= self.size && self.heap[j] < value {
            self.heap[i] = self.heap[j];
            i = j;
            j = i << 1;
            k = j + 1;
            if k <= self.size && self.heap[k] < self.heap[j] {
                j = k;
            }
        }

        self.heap[i] = value;
    }
    /// Pushes all elements from another heap into this heap.
    pub fn push_all(&mut self, other: &LongHeap) {
        for i in 1..=other.size {
            self.push(other.heap[i]);
        }
    }

    /// Returns the element at the ith location in the heap array.
    /// Valid indices are in [1, size].
    pub fn get(&self, i: usize) -> i64 {
        self.heap[i]
    }

    /// Returns the internal heap array.
    #[cfg(test)]
    pub fn get_heap_array(&self) -> &[i64] {
        &self.heap
    }
}

#[cfg(test)]
mod tests {

    use rand::Rng;

    use crate::core::util::error::lucene_error::{LuceneError, Result};
    use crate::core::util::long_heap::LongHeap;
    use crate::test::util::lucene_test_case::lucene_test_case_util::random;
    #[allow(dead_code)] // for quick search
    struct TestLongHeap;
    /// Checks that the heap property is maintained.
    fn check_validity(heap: &LongHeap) {
        let heap_array = heap.get_heap_array();
        let size = heap.size() as usize;
        for i in 2..=size {
            let parent = i >> 1;
            assert!(
                heap_array[parent] <= heap_array[i],
                "Heap property violated at index {}: parent={} > child={}",
                i,
                heap_array[parent],
                heap_array[i]
            );
        }
    }

    #[test]
    fn test_pq_basic() -> Result<()> {
        let mut random = random();
        test_pq_with_random(10_000, &mut random)
    }

    fn test_pq_with_random<R: Rng + ?Sized>(count: usize, random: &mut R) -> Result<()> {
        let mut pq = LongHeap::new(count as i32)?;
        let mut sum: i64 = 0;
        let mut sum2: i64 = 0;

        for _ in 0..count {
            let next = random.random();
            sum += next;
            pq.push(next);
            check_validity(&pq);
        }

        let mut last = i64::MIN;
        for _ in 0..count {
            let next = pq.pop()?;
            assert!(
                next >= last,
                "Heap out of order: current {} < last {}",
                next,
                last
            );
            last = next;
            sum2 += last;
        }

        assert_eq!(sum, sum2, "Sum mismatch after all pops");
        Ok(())
    }
    #[test]
    fn test_clear() -> Result<()> {
        let mut pq = LongHeap::new(3)?;
        pq.push(2);
        pq.push(3);
        pq.push(1);
        assert_eq!(3, pq.size());
        pq.clear();
        assert_eq!(0, pq.size());
        Ok(())
    }
    #[test]
    fn test_exceed_bounds() -> Result<()> {
        let mut pq = LongHeap::new(1)?;
        pq.push(2);
        pq.push(0);
        assert_eq!(2, pq.size());
        assert_eq!(0, pq.top());
        Ok(())
    }
    #[test]
    fn test_fixed_size() -> Result<()> {
        let mut pq = LongHeap::new(3)?;
        pq.insert_with_overflow(2);
        pq.insert_with_overflow(3);
        pq.insert_with_overflow(1);
        pq.insert_with_overflow(5);
        pq.insert_with_overflow(7);
        pq.insert_with_overflow(1);
        assert_eq!(3, pq.size());
        assert_eq!(3, pq.top());
        Ok(())
    }

    #[test]
    fn test_duplicate_values() -> Result<()> {
        let mut pq = LongHeap::new(3)?;
        pq.push(2);
        pq.push(3);
        pq.push(1);
        assert_eq!(1, pq.top());
        pq.update_top(3);
        assert_eq!(3, pq.size());
        assert_eq!(&[0, 2, 3, 3], pq.get_heap_array());
        Ok(())
    }

    #[test]
    fn test_insertions() -> Result<()> {
        let mut random = random();
        let num_docs_in_pq = random.random_range(1..=100);
        let mut pq = LongHeap::new(num_docs_in_pq)?;
        let mut last_least: Option<i64> = None;

        for _ in 0..(num_docs_in_pq * 10) {
            let new_entry = random.random();
            pq.insert_with_overflow(new_entry);
            check_validity(&pq);
            let new_least = pq.top();
            if let Some(last) =
                last_least.filter(|&last| new_least != new_entry && new_least != last)
            {
                assert!(new_least <= new_entry);
                assert!(new_least >= last);
            }
            last_least = Some(new_least);
        }
        Ok(())
    }

    #[test]
    fn test_invalid() -> Result<()> {
        assert!(matches!(
            LongHeap::new(-1),
            Err(LuceneError::IllegalArgument(_))
        ));
        assert!(matches!(
            LongHeap::new(0),
            Err(LuceneError::IllegalArgument(_))
        ));
        // TODO: see ArrayUtil::MAX_ARRAY_LENGTH
        // assert!(matches!(
        //     LongHeap::new(ArrayUtil::MAX_ARRAY_LENGTH as i32),
        //     Err(LuceneError::IllegalArgument(_))
        // ));
        Ok(())
    }

    #[test]
    fn test_unbounded() -> Result<()> {
        let mut random = random();
        let initial_size = random.random_range(1..=10);
        let mut pq = LongHeap::new(initial_size)?;
        let num = random.random_range(1..=100);
        let mut max_value = i64::MIN;
        let mut count = 0;

        for _ in 0..num {
            let value: i64 = random.random();
            if random.random_bool(0.5) {
                pq.push(value);
                count += 1;
            } else {
                let full = pq.size() >= initial_size;
                if pq.insert_with_overflow(value) && !full {
                    count += 1;
                }
            }
            max_value = std::cmp::max(max_value, value);
        }

        assert_eq!(count, pq.size());
        let mut last = i64::MIN;
        while pq.size() > 0 {
            let top = pq.top();
            let next = pq.pop()?;
            assert_eq!(top, next);
            count -= 1;
            assert!(next >= last);
            last = next;
        }
        assert_eq!(0, count);
        assert_eq!(max_value, last);
        Ok(())
    }
}
