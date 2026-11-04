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

/// A priority queue maintains a partial ordering of its elements such that the
/// least element can always be found in constant time. `put()` and `pop()`
/// operations require `O(log(size))` time, but the `remove()` operation is
/// implemented with a linear cost.
///
/// # Note
/// This struct pre-allocates an array of length `max_size + 1` and pre-fills it
/// with elements if instantiated via the
/// [`PriorityQueue::with_sentinel_object`]
/// initialization method.
///
/// # Note
/// Iteration order is not specified.
///
/// # Note
/// This is an internal API.
pub struct PriorityQueue<T, C> {
  size: usize,
  max_size: usize,
  heap: Vec<Option<T>>,
  pub(crate) compare: C,
}
impl<T, C> PriorityQueue<T, C>
where
  C: Compare<T>,
  T: PartialEq,
{
  /// Removes an existing element currently stored in the priority queue. The
  /// cost is linear with the size of the queue. (A specialization of the
  /// priority queue that tracks element positions would provide a
  /// constant remove time, but the trade-off would be extra cost to all
  /// additions/insertions.)
  pub fn remove(&mut self, element: &T) -> Result<bool> {
    if let Some(i) = (1..=self.size).find(|&idx| {
      self.heap[idx]
        .as_ref()
        .map(|value| value == element)
        .unwrap_or(false)
    }) {
      let last_index = self.size;
      self.heap.swap(i, last_index);
      self.heap[last_index] = None;
      self.size -= 1;
      if i <= self.size && !self.up_heap(i)? {
        self.down_heap(i)?;
      }
      return Ok(true);
    }
    Ok(false)
  }
}

impl<T, C> PriorityQueue<T, C> {
  pub fn heap(&self) -> &Vec<Option<T>> {
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
  /// value (i.e., [`Compare::less_than`] should always favor
  /// the non-sentinel values).
  ///
  /// By default, the supplier returns `None`, which means the queue will not
  /// be filled with sentinel values. Otherwise, the value returned will
  /// be used to pre-populate the queue.
  ///
  /// # Usage
  /// If this method is extended to return a `Some` value, the following
  /// usage pattern is recommended:
  ///
  /// ```text
  /// let mut pq: MyQueue<MyObject> = MyQueue::new(num_hits);
  /// // Save the 'top' element, which is guaranteed to not be None.
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
  /// previously returned a present value, and all returned instances
  /// must be comparable using [`Compare::less_than`].
  pub fn with_sentinel_object<F>(
    max_size: usize,
    sentinel_object_supplier: F,
    compare: C,
  ) -> Result<PriorityQueue<T, C>>
  where
    F: Fn() -> Option<T>,
  {
    let heap_size = if 0 == max_size {
      // We allocate 1 extra to avoid if statement in top()
      2
    } else {
      if !(0..i32::MAX as usize).contains(&max_size) {
        return Err(LuceneError::illegal_argument(format!(
          "maxSize must be >= 0 and < {}; got: {}",
          i32::MAX,
          max_size
        )));
      }
      // NOTE: we add +1 because all access to heap is
      // 1-based not 0-based.  heap[0] is unused.
      max_size + 1
    };
    let mut heap: Vec<Option<T>> = (0..heap_size).map(|_| None).collect();
    if let Some(sentinel) = sentinel_object_supplier() {
      heap[1] = Some(sentinel);
      #[allow(clippy::needless_range_loop)]
      for i in 2..heap.len() {
        heap[i] = Some(sentinel_object_supplier().ok_or_else(|| {
          LuceneError::illegal_state("sentinel_object_supplier must not return None")
        })?);
      }
      return Ok(PriorityQueue {
        max_size,
        size: max_size,
        heap,
        compare,
      });
    }
    Ok(PriorityQueue {
      max_size,
      size: 0,
      heap,
      compare,
    })
  }

  // construct
  pub fn new(max_size: usize, compare: C) -> Result<PriorityQueue<T, C>>
  where
    C: Compare<T>,
  {
    Self::with_sentinel_object(max_size, || None, compare)
  }

  /// Returns the least element of the PriorityQueue in constant time.
  pub fn top_mut(&mut self) -> Option<&mut T> {
    // We don't need to check size here: if maxSize is 0,
    // then heap is length 2 array with both entries None.
    // If size is 0 then heap[1] is already None.
    self.heap[1].as_mut()
  }
  pub fn top(&self) -> Option<&T> {
    self.heap[1].as_ref()
  }
  pub fn take_top(&mut self) -> Option<T> {
    self.heap[1].take()
  }

  /// Returns the number of elements currently stored in the PriorityQueue.
  pub fn size(&self) -> usize {
    self.size
  }

  /// Removes all entries from the PriorityQueue.
  pub fn clear(&mut self) {
    let heap = &mut self.heap[..=self.size];
    for entry in &mut heap[1..] {
      *entry = None;
    }
    self.size = 0;
  }

  pub fn iter_ref(&'_ self) -> PriorityQueueIterator<'_, T, C> {
    PriorityQueueIterator::new(self)
  }
  pub fn iter(self) -> PriorityQueueIntoIterator<T, C> {
    PriorityQueueIntoIterator::new(self)
  }
}

impl<T, C> PriorityQueue<T, C>
where
  C: Compare<T>,
{
  /// Adds all elements of the collection into the queue. This method should
  /// be preferred over calling [`add`](Self::add) in a loop if all
  /// elements are known in advance, as it builds the queue faster.
  ///
  /// # Errors
  /// If one tries to add more objects than the `max_size` passed in the
  /// initialization method, an
  /// [`ArrayIndexOutOfBoundsError`](crate::core::util::error::ArrayIndexOutOfBoundsError) is returned.
  pub fn add_all<I>(&mut self, elements: I) -> Result<()>
  where
    I: IntoIterator<Item = T>,
  {
    let elements = elements.into_iter().collect::<Vec<_>>();
    self.add_all_exact(elements.into_iter())
  }

  pub(crate) fn add_all_exact<I>(&mut self, elements: I) -> Result<()>
  where
    I: ExactSizeIterator<Item = T>,
  {
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
    for element in elements {
      self.heap[self.size + 1] = Some(element);
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
  /// is returned.
  ///
  /// # Returns
  /// The new 'top' element in the queue.
  pub fn add(&mut self, element: T) -> Result<&T> {
    let index = self.size + 1;
    if index >= self.heap.len() {
      return Err(LuceneError::array_index_out_of_bounds(format!(
        "Cannot add an element to a queue with remaining capacity: {}",
        self.max_size.saturating_sub(self.size)
      )));
    }
    self.heap[index] = Some(element);
    self.size = index;
    self.up_heap(index)?;
    self.heap_value(1)
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
    } else if self.size > 0 {
      if let Some(top) = self.heap[1].as_ref()
        && self.compare.less_than(top, &element)?
      {
        let ret = self.heap[1]
          .replace(element)
          .ok_or_else(|| LuceneError::illegal_state("priority queue top element should exist"))?;
        self.update_top()?;
        Ok(Some(ret))
      } else {
        Ok(Some(element))
      }
    } else {
      Ok(Some(element))
    }
  }

  /// Removes and returns the least element of the PriorityQueue in log(size)
  /// time.
  pub fn pop(&mut self) -> Result<Option<T>> {
    if self.size > 0 {
      let result = self.pop_unchecked()?;
      Ok(Some(result))
    } else {
      Ok(None)
    }
  }
  pub(crate) fn pop_unchecked(&mut self) -> Result<T> {
    debug_assert!(self.size > 0, "pop_unchecked called on empty queue");
    self.heap.swap(1, self.size);
    let result = self.heap[self.size]
      .take()
      .ok_or_else(|| LuceneError::illegal_state("priority queue element should exist"))?;
    self.size -= 1;
    self.down_heap(1)?;
    Ok(result)
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
  pub fn update_top(&mut self) -> Result<&mut T> {
    self.down_heap(1)?;
    self.heap_value_mut(1)
  }

  /// Replace the top of the pq with `newTop` and run `updateTop()`.
  pub fn update_top_with_new_top(&mut self, new_top: T) -> Result<&mut T> {
    self.heap[1] = Some(new_top);
    self.update_top()
  }

  pub fn up_heap(&mut self, orig_pos: usize) -> Result<bool> {
    let mut i = orig_pos;
    let mut j = i >> 1;
    while j > 0
      && self
        .compare
        .less_than(self.heap_value(i)?, self.heap_value(j)?)?
    {
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
      let (parents, children) = self.heap.split_at_mut(j);

      let child = if k <= size {
        let (left, right) = children.split_at_mut(1);
        let right = right[0]
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("priority queue element should exist"))?;
        let left = left[0]
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("priority queue element should exist"))?;
        if self.compare.less_than(right, left)? {
          j = k;
          right
        } else {
          left
        }
      } else {
        children[0]
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("priority queue element should exist"))?
      };
      let node = parents[i]
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("priority queue element should exist"))?;

      if !self.compare.less_than(child, node)? {
        break;
      }

      std::mem::swap(node, child);
      i = j;
    }
    Ok(())
  }

  /// This method returns the internal heap array as `Vec<Object>`.
  ///
  /// # Note
  /// This is an internal API.
  pub(crate) fn get_heap_array(&self) -> &[Option<T>] {
    &self.heap
  }
  pub(crate) fn take_heap_array(&mut self) -> Vec<T> {
    if self.size == 0 {
      return Vec::new();
    }
    let taken = self.heap[1..=self.size]
      .iter_mut()
      .filter_map(Option::take)
      .collect();
    self.size = 0;
    taken
  }

  fn heap_value(&self, index: usize) -> Result<&T> {
    self.heap[index]
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("priority queue element should exist"))
  }
  fn heap_value_mut(&mut self, index: usize) -> Result<&mut T> {
    self.heap[index]
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("priority queue element should exist"))
  }
}
pub struct PriorityQueueIntoIterator<T, C> {
  pq: PriorityQueue<T, C>,
  index: usize,
}

impl<T, C> PriorityQueueIntoIterator<T, C> {
  fn new(pq: PriorityQueue<T, C>) -> Self {
    Self { pq, index: 0 }
  }
}

impl<T, C> Iterator for PriorityQueueIntoIterator<T, C> {
  type Item = T;

  fn next(&mut self) -> Option<Self::Item> {
    while self.index < self.pq.size {
      self.index += 1;
      if let Some(result) = self.pq.heap[self.index].take() {
        return Some(result);
      }
    }
    None
  }
}

/// Each call can start iterating over the elements in the priority queue from
/// the beginning. The access order is not sorted; if a sorted order is
/// required, you can directly use [`pop`](PriorityQueue::pop).
pub struct PriorityQueueIterator<'a, T, C> {
  pq: &'a PriorityQueue<T, C>,
  index: usize,
}
impl<'a, T, C> PriorityQueueIterator<'a, T, C> {
  fn new(pq: &'a PriorityQueue<T, C>) -> Self {
    Self { pq, index: 0 }
  }
}
impl<'a, T, C> Iterator for PriorityQueueIterator<'a, T, C> {
  type Item = &'a T;

  fn next(&mut self) -> Option<Self::Item> {
    while self.index < self.pq.size {
      self.index += 1;
      if let Some(result) = self.pq.heap[self.index].as_ref() {
        return Some(result);
      }
    }
    None
  }
}

pub trait Compare<T> {
  /// Determines the ordering of values in this priority queue. Implementations
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
impl<T, C> Compare<T> for &C
where
  C: Compare<T>,
{
  fn less_than(&self, a: &T, b: &T) -> Result<bool> {
    (**self).less_than(a, b)
  }
}
macro_rules! either_compare {
    ($vis:vis $name:ident { $( $Variant:ident : $T:ident ),+ $(,)? }) => {
        $vis enum $name<$( $T ),+> {
            $( $Variant($T), )+
        }

        impl<T, $( $T ),+> Compare<T> for $name<$( $T ),+>
        where
            $( $T: Compare<T> ),+
        {
            #[inline]
            fn less_than(&self, a: &T, b: &T) -> Result<bool> {
                match self {
                    $( Self::$Variant(inner) => inner.less_than(a, b), )+
                }
            }
        }
    };
}
either_compare!(pub CompareEnum2 { A: A, B: B });
