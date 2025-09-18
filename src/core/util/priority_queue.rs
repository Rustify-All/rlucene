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
use std::iter::repeat_with;
use std::mem;

use crate::core::util::error::lucene_error::{LuceneError, Result};

/// A priority queue maintains a partial ordering of its elements such that the
/// least element can always be found in constant time. `put()` and `pop()`
/// operations require `O(log(size))` time, but the `remove()` operation is
/// implemented with a linear cost.
///
/// # Note
/// This struct pre-allocates an array of length `max_size + 1` and pre-fills it
/// with elements if instantiated via the
/// [`PriorityQueue::with_sentinel_object`](#method.with_sentinel_object)
/// constructor.
///
/// # Note
/// Iteration order is not specified.
///
/// # Note
/// This is an internal API.
// TODO: should we use Option<T>, then we can remove the Default bound on T
pub struct PriorityQueue<T, C>
where
    C: Compare<T>,
{
    size: usize,
    max_size: usize,
    heap: Vec<T>,
    compare: C,
}
impl<T, C> PriorityQueue<T, C>
where
    C: Compare<T>,
    T: Default + PartialEq,
{
    /// Removes an existing element currently stored in the priority queue. The
    /// cost is linear with the size of the queue. (A specialization of the
    /// priority queue that tracks element positions would provide a
    /// constant remove time, but the trade-off would be extra cost to all
    /// additions/insertions.)
    pub fn remove(&mut self, element: &T) -> Result<bool> {
        if let Some(i) = (1..=self.size).next() {
            if self.heap[i] == *element {
                self.heap.swap(i, self.size);
            }
            self.size -= 1;
            if i <= self.size && !self.up_heap(i)? {
                self.down_heap(i)?;
            }
            return Ok(true);
        }
        Ok(false)
    }
}

impl<T, C> PriorityQueue<T, C>
where
    C: Compare<T>,
    T: Default,
{
    pub fn heap(&self) -> &Vec<T> {
        &self.heap
    }
    pub fn get_compare(&self) -> &C {
        &self.compare
    }
    /// Creates a priority queue that is pre-filled with sentinel objects, so
    /// that the code which uses that queue can always assume it's full and
    /// only change the top without attempting to insert any new object.
    ///
    /// # Description
    /// Those sentinel values should always compare worse than any non-sentinel
    /// value (i.e., [`lessThan`](Compare::less_than) should always favor
    /// the non-sentinel values).
    ///
    /// By default, the supplier returns `None`, which means the queue will not
    /// be filled with sentinel values. Otherwise, the value returned will
    /// be used to pre-populate the queue.
    ///
    /// # Usage
    /// If this method is extended to return a non-None value, the following
    /// usage pattern is recommended:
    ///
    /// ```text
    /// let mut pq: MyQueue<MyObject> = MyQueue::new(num_hits);
    /// // Save the 'top' element, which is guaranteed to not be null.
    /// let mut pq_top = pq.top();
    /// // Now, in order to add a new element that is 'better' than the top (after
    /// // you've verified it is better), it is as simple as:
    /// pq_top.change();
    /// pq_top = pq.update_top();
    /// ```
    ///
    /// # Note
    /// The given supplier will be called `max_size` times, relying on a new
    /// object to be returned and will not check if it's `None` again.
    /// Therefore, you should ensure any call to this method creates a new
    /// instance and behaves consistently, e.g., it cannot return `None` if it
    /// previously returned a non-null value, and all returned instances
    /// must be comparable using [`lessThan`](Compare::less_than).
    pub fn with_sentinel_object<F>(
        max_size: i32,
        sentinel_object_supplier: F,
        compare: C,
    ) -> Result<PriorityQueue<T, C>>
    where
        F: Fn() -> Option<T>,
        C: Compare<T>,
    {
        let heap_size = if 0 == max_size {
            // We allocate 1 extra to avoid if statement in top()
            2
        } else {
            if !(0..i32::MAX).contains(&max_size) {
                return Err(LuceneError::illegal_argument(format!(
                    "maxSize must be >= 0 and < {}; got: {}",
                    i32::MAX,
                    max_size
                )));
            }
            // NOTE: we add +1 because all access to heap is
            // 1-based not 0-based.  heap[0] is unused.
            (max_size + 1) as usize
        };
        let mut heap: Vec<T> = Vec::with_capacity(heap_size);
        heap.resize_with(heap_size, Default::default);
        if let Some(sentinel) = sentinel_object_supplier() {
            heap[1] = sentinel;
            for (i, value) in repeat_with(|| sentinel_object_supplier().unwrap())
                .take(heap_size)
                .enumerate()
                .skip(2)
            {
                heap[i] = value;
            }
            return Ok(PriorityQueue {
                max_size: max_size as usize,
                size: heap_size,
                heap,
                compare,
            });
        }
        Ok(PriorityQueue {
            max_size: max_size as usize,
            size: 0,
            heap,
            compare,
        })
    }

    // construct
    pub fn new(max_size: i32, compare: C) -> Result<PriorityQueue<T, C>> {
        Self::with_sentinel_object(max_size, || None, compare)
    }

    /// Adds all elements of the collection into the queue. This method should
    /// be preferred over calling [`add`](#method.add) in a loop if all
    /// elements are known in advance, as it builds the queue faster.
    ///
    /// # Errors
    /// If one tries to add more objects than the `max_size` passed in the
    /// constructor, an
    /// [`ArrayIndexOutOfBoundsError`](crate::core::util::error::ArrayIndexOutOfBoundsError) is thrown.
    pub fn add_all(&mut self, elements: Vec<T>) -> Result<()> {
        if (self.size + elements.len()) > self.max_size {
            return Err(LuceneError::array_index_out_of_bounds(format!(
                "Cannot add {} elements to a queue with remaining capacity: {}",
                elements.len(),
                self.max_size - self.size
            )));
        }
        // Heap with size S always takes first S elements of the array,
        // and thus it's safe to fill array further - no actual non-sentinel
        // value will be overwritten.
        for element in elements.into_iter() {
            self.heap[self.size + 1] = element;
            self.size += 1;
        }

        // The loop goes down to 1 as heap is 1-based not 0-based.
        for i in (1..=(self.size >> 1)).rev() {
            self.down_heap(i)?;
        }
        Ok(())
    }

    /// Adds an object to a priority queue in `O(log(size))` time. If more
    /// objects are added than the `max_size` initialized, an
    /// [`ArrayIndexOutOfBoundsError`](crate::core::util::error::ArrayIndexOutOfBoundsError)
    /// is thrown.
    ///
    /// # Returns
    /// The new 'top' element in the queue.
    pub fn add(&mut self, element: T) -> Result<&T> {
        let index = self.size + 1;
        self.heap[index] = element;
        self.size = index;
        self.up_heap(index)?;
        Ok(&self.heap[1])
    }

    /// Adds an object to a priority queue in `O(log(size))` time. It returns
    /// the object (if any) that was dropped off the heap because it was
    /// full. This can be the given parameter (if it is smaller than the
    /// full heap's minimum and couldn't be added), or another object that was
    /// previously the smallest value in the heap and now has been replaced
    /// by a larger one, or `None` if the queue wasn't yet full with `max_size`
    /// elements.
    pub fn insert_with_overflow(&mut self, element: T) -> Result<Option<T>> {
        if self.size < self.max_size {
            self.add(element)?;
            Ok(None)
        } else if self.size > 0 && self.compare.less_than(&self.heap[1], &element)? {
            let ret = mem::replace(&mut self.heap[1], element);
            self.update_top()?;
            Ok(Some(ret))
        } else {
            Ok(Some(element))
        }
    }

    /// Returns the least element of the PriorityQueue in constant time.
    pub fn top_mut(&mut self) -> &mut T {
        // We don't need to check size here: if maxSize is 0,
        // then heap is length 2 array with both entries null.
        // If size is 0 then heap[1] is already null.
        &mut self.heap[1]
    }
    pub fn top(&self) -> &T {
        &self.heap[1]
    }

    /// Removes and returns the least element of the PriorityQueue in log(size)
    /// time.
    pub fn pop(&mut self) -> Result<Option<T>> {
        if self.size > 0 {
            self.heap.swap(1, self.size);
            let result = self.heap.remove(self.size);
            // With size as a sentinel value, we add an invalid value to prevent
            // the length of the Vec from changing
            self.heap.push(T::default());
            self.size -= 1;
            self.down_heap(1)?;
            Ok(Some(result))
        } else {
            Ok(None)
        }
    }

    /// Should be called when the object at the top changes values. It's still
    /// `O(log(n))` in the worst case, but it's at least twice as fast to:
    ///
    /// ```text
    /// pq.top().change();
    /// pq.update_top();
    /// ```
    ///
    /// instead of:
    ///
    /// ```text
    /// let mut o = pq.pop();
    /// o.change();
    /// pq.push(o);
    /// ```
    ///
    /// # Returns
    /// The new 'top' element.
    pub fn update_top(&mut self) -> Result<&T> {
        self.down_heap(1)?;
        Ok(&self.heap[1])
    }

    /// Replace the top of the pq with `newTop` and run `updateTop()`.
    pub fn update_top_with_new_top(&mut self, new_top: T) -> Result<&T> {
        self.heap[1] = new_top;
        self.update_top()
    }

    /// Returns the number of elements currently stored in the PriorityQueue.
    pub fn size(&self) -> usize {
        self.size
    }

    /// Removes all entries from the PriorityQueue.
    pub fn clear(&mut self) {
        for i in 0..=self.size {
            self.heap[i] = T::default();
        }
        self.size = 0;
    }

    pub fn up_heap(&mut self, orig_pos: usize) -> Result<bool> {
        let mut i = orig_pos;
        let mut j = i >> 1;
        while j > 0 && self.compare.less_than(&self.heap[i], &self.heap[j])? {
            self.heap.swap(i, j);
            i = j;
            j = i >> 1;
        }
        Ok(i != orig_pos)
    }

    pub fn down_heap(&mut self, mut i: usize) -> Result<()> {
        let size = self.size;
        while i * 2 <= size {
            let mut j = i * 2;
            let k = j + 1;

            if k <= size && self.compare.less_than(&self.heap[k], &self.heap[j])? {
                j = k;
            }

            if !self.compare.less_than(&self.heap[j], &self.heap[i])? {
                break;
            }

            self.heap.swap(i, j);
            i = j;
        }
        Ok(())
    }

    /// This method returns the internal heap array as `Vec<Object>`.
    ///
    /// # Note
    /// This is an internal API.
    fn get_heap_array(&self) -> &Vec<T> {
        &self.heap
    }

    pub fn iterator(&'_ self) -> PriorityQueueIterator<'_, T, C> {
        PriorityQueueIterator::new(self)
    }
}

/// Each call can start iterating over the elements in the priority queue from
/// the beginning. The access order is not sorted; if a sorted order is
/// required, you can directly use [`pop`](PriorityQueue::pop).
pub struct PriorityQueueIterator<'a, T, C>
where
    C: Compare<T>,
{
    pq: &'a PriorityQueue<T, C>,
    index: usize,
}
impl<'a, T, C> PriorityQueueIterator<'a, T, C>
where
    C: Compare<T>,
{
    fn new(pq: &'a PriorityQueue<T, C>) -> Self {
        Self { pq, index: 0 }
    }
}
impl<'a, T, C> Iterator for PriorityQueueIterator<'a, T, C>
where
    C: Compare<T>,
{
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index < self.pq.size {
            let result = &self.pq.heap[self.index + 1];
            self.index += 1;
            return Some(result);
        }
        None
    }
}

pub trait Compare<T> {
    /// Determines the ordering of objects in this priority queue. SubStruct
    /// must define this method.
    ///
    /// # Arguments
    /// * `a` - The first object to compare.
    /// * `b` - The second object to compare.
    ///
    /// # Returns
    /// `true` if parameter `a` is less than parameter `b`.
    fn less_than(&self, a: &T, b: &T) -> Result<bool>;
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;

    use rand::Rng;

    use crate::core::util::error::lucene_error::Result;
    use crate::core::util::priority_queue::{Compare, PriorityQueue};
    use crate::test::util::lucene_test_case::lucene_test_case_util::{at_least, random};
    use crate::test::util::test_util::TestUtil;

    #[allow(dead_code)] // for quick search
    struct TestPriorityQueue {}

    struct I32Compare;

    impl Compare<i32> for I32Compare {
        fn less_than(&self, a: &i32, b: &i32) -> Result<bool> {
            Ok(a < b)
        }
    }
    #[test]
    fn test_zero_sized_queue() {
        let mut random = random();
        let mut pq = PriorityQueue::new(0, I32Compare).unwrap();
        assert_eq!(1, pq.insert_with_overflow(1).unwrap().unwrap());
        assert_eq!(0, pq.size());

        pq.add(1).unwrap();
        match random.random_bool(0.5) {
            true => assert_eq!(1, *pq.top_mut()),
            false => assert_eq!(1, *pq.top()),
        }
    }

    #[derive(Default)]
    struct ObjectCompare {
        index: i32,
        value: i32,
    }

    impl PartialEq for ObjectCompare {
        fn eq(&self, other: &Self) -> bool {
            if self.index == other.index && self.value == other.value {
                return true;
            }
            false
        }
    }

    impl ObjectCompare {
        fn new(index: i32, value: i32) -> Self {
            ObjectCompare { index, value }
        }
    }

    impl Compare<ObjectCompare> for ObjectCompare {
        fn less_than(&self, a: &ObjectCompare, b: &ObjectCompare) -> Result<bool> {
            Ok(a.value < b.value)
        }
    }

    #[test]
    fn test_no_extra_work_on_equal_elements() {
        let mut pq = PriorityQueue::new(5, ObjectCompare::default()).unwrap();
        for i in 0..100 {
            pq.insert_with_overflow(ObjectCompare::new(i, 0)).unwrap();
        }
        let mut indexes: Vec<i32> = Vec::new();
        let iter = pq.iterator();
        for e in iter {
            indexes.push(e.index)
        }
        assert_eq!(indexes, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn test_pq() {
        let mut random = random();
        let count: i32 = at_least(&mut random, 10000);
        let pq = PriorityQueue::new(count, I32Compare);
        if let Ok(mut heap) = pq {
            let mut sum: i32 = 0;
            let mut sum2: i32 = 0;
            for _i in 0..count {
                let next: i32 = random.random();
                sum = sum.wrapping_add(next);
                heap.add(next).unwrap();
            }

            let mut last = i32::MIN;
            for _i in 0..count {
                let next = heap.pop().unwrap().unwrap();
                assert!(next >= last);
                last = next;
                sum2 = sum2.wrapping_add(last);
            }

            assert_eq!(sum, sum2);
        } else {
            assert!(count == 0 || count == i32::MAX);
        }
    }

    #[test]
    fn test_clear() {
        let mut pq = PriorityQueue::new(3, I32Compare).unwrap();
        pq.add(2).unwrap();
        pq.add(3).unwrap();
        pq.add(1).unwrap();
        assert_eq!(3, pq.size());
        pq.clear();
        assert_eq!(0, pq.size());
    }

    #[test]
    fn test_fixed_size() {
        let mut pq = PriorityQueue::new(3, I32Compare).unwrap();
        pq.insert_with_overflow(2).unwrap();
        pq.insert_with_overflow(3).unwrap();
        pq.insert_with_overflow(1).unwrap();
        pq.insert_with_overflow(5).unwrap();
        pq.insert_with_overflow(7).unwrap();
        pq.insert_with_overflow(1).unwrap();
        assert_eq!(3, pq.size());
        assert_eq!(3, pq.pop().unwrap().unwrap());
    }

    #[test]
    fn test_insert_with_overflow() {
        let size = 4;
        let mut pq = PriorityQueue::new(size, I32Compare).unwrap();
        let i1 = 2;
        let i2 = 3;
        let i3 = 1;
        let i4 = 5;
        let i5 = 7;
        let i6 = 1;

        assert_eq!(pq.insert_with_overflow(i1).unwrap(), None);
        assert_eq!(pq.insert_with_overflow(i2).unwrap(), None);
        assert_eq!(pq.insert_with_overflow(i3).unwrap(), None);
        assert_eq!(pq.insert_with_overflow(i4).unwrap(), None);
        assert_eq!(pq.insert_with_overflow(i5).unwrap().unwrap(), i3);
        assert_eq!(pq.insert_with_overflow(i6).unwrap().unwrap(), i6);
        assert_eq!(size as usize, pq.size());
        let mut random = random();
        match random.random_bool(0.5) {
            true => assert_eq!(2, *pq.top_mut()),
            false => assert_eq!(2, *pq.top()),
        }
    }

    #[test]
    fn test_add_all_to_empty_queue() {
        let mut random = random();
        let size = 10;
        let mut list: Vec<i32> = Vec::new();
        let mut list2: Vec<i32> = Vec::new();
        let mut value: i32;
        for _i in 0..size {
            value = random.random();
            list.push(value);
            list2.push(value);
        }
        let mut pq = PriorityQueue::new(size, I32Compare).unwrap();
        pq.add_all(list).unwrap();
        check_validity(&pq);
        assert_ordered_when_drained(&mut pq, list2);
    }

    #[test]
    fn test_add_all_to_partially_filled_queue() {
        let mut pq = PriorityQueue::new(20, I32Compare).unwrap();
        let mut one_by_one: Vec<i32> = Vec::new();
        let mut bulk_added: Vec<i32> = Vec::new();
        let mut bulk_added2: Vec<i32> = Vec::new();
        let mut random = random();

        for _i in 0..10 {
            let value: i32 = random.random();
            bulk_added.push(value);
            bulk_added2.push(value);
            let x: i32 = random.random();
            pq.add(x).unwrap();
            one_by_one.push(x);
        }

        pq.add_all(bulk_added).unwrap();
        check_validity(&pq);

        one_by_one.append(&mut bulk_added2);
        assert_ordered_when_drained(&mut pq, one_by_one);
    }

    #[test]
    fn test_add_all_does_not_fit_into_queue() {
        let mut pq = PriorityQueue::new(20, I32Compare).unwrap();
        let mut list: Vec<i32> = Vec::new();
        let mut random = random();
        for _i in 0..11 {
            list.push(random.random());
            pq.add(random.random()).unwrap();
        }
        let result = pq.add_all(list).unwrap_err().to_string();
        assert_eq!(
            result,
            "Cannot add 11 elements to a queue with remaining capacity: 9"
        );
    }

    #[test]
    fn test_removals_and_insertions() {
        let mut random = random();
        let num_docs_in_pq = TestUtil::next_int(&mut random, 1, 100);
        let mut pq = PriorityQueue::new(num_docs_in_pq, I32Compare).unwrap();
        let mut last_least: Option<i32> = None;

        // Basic insertion of new content
        let mut sds: Vec<i32> = Vec::with_capacity(num_docs_in_pq as usize);
        for _i in 0..num_docs_in_pq * 10 {
            let new_entry = random.random::<i32>().abs();
            sds.push(new_entry);
            let evicted = pq.insert_with_overflow(new_entry).unwrap();
            check_validity(&pq);
            if let Some(evicted_value) = evicted {
                let pos = sds.iter().position(|&x| x == evicted_value);
                assert_ne!(pos, None);
                sds.remove(pos.unwrap());
                if evicted_value != new_entry {
                    assert_eq!(evicted_value, last_least.unwrap());
                }
            }
            let new_least = match random.random_bool(0.5) {
                true => pq.top_mut(),
                false => pq.top(),
            };
            if last_least.is_some() && *new_least != new_entry && *new_least != last_least.unwrap()
            {
                // If there has been a change of least entry and it wasn't our
                // new addition we expect the scores to increase
                assert!(*new_least <= new_entry);
                assert!(*new_least >= last_least.unwrap());
            }
            last_least = Some(*new_least);
        }
        // Try many random additions to existing entries - we should always see
        // increasing scores in the lowest entry in the PQ
        for _i in 0..500000 {
            let element = (random.random::<f32>() * ((sds.len() - 1) as f32)) as i32;
            let object_to_remove = sds[element as usize];
            assert_eq!(sds.remove(element as usize), object_to_remove);
            assert!(pq.remove(&object_to_remove).unwrap());
            check_validity(&pq);
            let new_entry = random.random::<i32>().abs();
            sds.push(new_entry);
            assert_eq!(pq.insert_with_overflow(new_entry).unwrap(), None);
            check_validity(&pq);
            let new_least = match random.random_bool(0.5) {
                true => pq.top_mut(),
                false => pq.top(),
            };
            if object_to_remove != last_least.unwrap()
                && last_least.is_some()
                && *new_least != new_entry
            {
                // If there has been a change of least entry and it wasn't our
                // new addition or the loss of our randomly
                // removed entry we expect the
                // scores to increase
                assert!(*new_least <= new_entry);
                assert!(*new_least >= last_least.unwrap());
            }
            last_least = Some(*new_least);
        }
    }

    #[test]
    fn test_iterator_empty() {
        let pq = PriorityQueue::new(3, I32Compare).unwrap();
        let mut it = pq.iterator();
        assert_eq!(it.next(), None);
    }

    #[test]
    fn test_iterator_one() {
        let mut pq = PriorityQueue::new(3, I32Compare).unwrap();
        pq.add(1).unwrap();
        let mut it = pq.iterator();
        assert_eq!(it.next(), Some(&1));
    }

    #[test]
    fn test_iterator_two() {
        let mut pq = PriorityQueue::new(3, I32Compare).unwrap();
        pq.add(1).unwrap();
        pq.add(2).unwrap();
        let mut it = pq.iterator();
        assert_eq!(it.next(), Some(&1));
        assert_eq!(it.next(), Some(&2));
    }

    #[test]
    fn test_iterator_random() {
        let mut random = random();
        let max_size: usize = TestUtil::next_int(&mut random, 1, 20) as usize;
        let mut queue = PriorityQueue::new(max_size as i32, I32Compare).unwrap();
        let iters: usize = at_least(&mut random, 100) as usize;
        let mut expected: Vec<i32> = Vec::new();
        for _i in 0..iters {
            if queue.size() == 0 || (queue.size() < max_size) {
                // if queue.size() == 0 || (queue.size() < max_size &&
                // random.random::<bool>()) {
                let value: i32 = random.random_range(0..=10);
                queue.add(value).unwrap();
                expected.push(value);
            } else {
                let pos = expected
                    .iter()
                    .position(|&x| x == queue.pop().unwrap().unwrap());
                assert_ne!(pos, None);
                expected.remove(pos.unwrap());
            }
            let mut actual: Vec<i32> = Vec::new();
            for value in queue.iterator() {
                actual.push(*value);
            }
            expected.sort();
            actual.sort();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn test_max_int_size() {
        let pq = PriorityQueue::new(i32::MAX, I32Compare);
        assert!(pq.is_err());
    }

    fn assert_ordered_when_drained<T, C>(
        pq: &mut PriorityQueue<T, C>,
        mut reference_data_list: Vec<i32>,
    ) where
        C: Compare<T>,
        T: Into<i32> + Default + Debug + PartialEq,
    {
        reference_data_list.sort();
        let mut i = 0;
        let mut value: i32;
        while pq.size() > 0 {
            value = pq.pop().unwrap().unwrap().into();
            assert_eq!(reference_data_list[i], value);
            i += 1;
        }
    }

    fn check_validity<T, C>(pq: &PriorityQueue<T, C>)
    where
        C: Compare<T>,
        T: Default + PartialEq + Debug,
    {
        let size = pq.size();
        let heap = pq.heap();
        for i in 1..=size {
            let parent = i >> 1;
            if parent > 1 && !pq.get_compare().less_than(&heap[parent], &heap[i]).unwrap() {
                assert_eq!(&heap[parent], &heap[i]);
            }
        }
    }
}
