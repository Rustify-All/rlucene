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
use crate::core::util::priority_queue::{Compare, PriorityQueue};

use crate::core::util::ToInt;
/// Provides a merged, sorted view over several sorted iterators.
///
/// If built with `remove_duplicates` set to `true` and an element appears in multiple iterators,
/// then it is deduplicated; in other words, this iterator returns the sorted union of elements.
///
/// If built with `remove_duplicates` set to `false`, then all elements from all iterators are
/// returned.
///
/// # Caveats
///
/// - The behavior is undefined if the iterators are not actually sorted.
/// - `None` elements are unsupported.
/// - If `remove_duplicates` is set to `true` and a single iterator itself contains duplicates,
///   those duplicates will **not** be deduplicated.
/// - When elements are deduplicated, it is not defined which instance is returned.
/// - If `remove_duplicates` is set to `false`, the order in which duplicates are returned is
///   undefined.
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::iterator::IteratorExt;

pub struct MergedIterator<E>
where
  E: IteratorExt,
  E::Item: Ord + Clone,
{
  current: Option<E::Item>,
  queue: PriorityQueue<usize, TermMergeQueueCmp<E>>,
  top: smallvec::SmallVec<[usize; 4]>,
  remove_duplicates: bool,
  num_top: usize,
}
impl<E> MergedIterator<E>
where
  E: IteratorExt,
  E::Item: Ord + Clone,
{
  pub fn new<I>(iterators: I) -> Result<Self>
  where
    I: IntoIterator<Item = E>,
  {
    Self::with_remove_duplicates(true, iterators)
  }
  pub fn with_remove_duplicates<I>(remove_duplicates: bool, iterators: I) -> Result<Self>
  where
    I: IntoIterator<Item = E>,
  {
    let iterators = iterators.into_iter().collect::<Vec<_>>();
    let len = iterators.len();
    let mut sub_iterators = Vec::with_capacity(len);
    for (index, mut it) in iterators.into_iter().enumerate() {
      if it.has_next()? {
        let current = it
          .next()?
          .ok_or_else(|| LuceneError::illegal_state("has no next"))?;
        sub_iterators.push(SubIterator {
          iterator: it,
          current: Some(current),
          index,
        });
      }
    }
    let sub_iterator_len = sub_iterators.len();
    let cmp = TermMergeQueueCmp {
      sub_iterator: sub_iterators,
    };

    let queue_size = if len < i32::MAX as usize {
      sub_iterator_len
    } else {
      len
    };
    let mut queue = PriorityQueue::new(queue_size, cmp)?;

    for i in 0..sub_iterator_len {
      queue.add(i)?;
    }
    let top_len = if remove_duplicates {
      sub_iterator_len
    } else {
      sub_iterator_len.min(1)
    };
    Ok(Self {
      current: None,
      queue,
      top: smallvec::smallvec![0; top_len],
      remove_duplicates,
      num_top: 0,
    })
  }
  fn pull_top(&mut self) -> Result<()> {
    debug_assert!(self.num_top == 0);
    let first = self
      .queue
      .pop()?
      .ok_or_else(|| LuceneError::illegal_state("queue is empty"))?;
    self.top[self.num_top] = first;
    self.num_top += 1;

    if self.remove_duplicates {
      // extract all subs from the queue that have the same top element
      while self.queue.size() > 0 {
        let top_idx = *self
          .queue
          .top()
          .ok_or_else(|| LuceneError::number_format("queue is empty"))?;
        let first_idx = self.top[0];

        if self.queue.compare.sub_iterator[top_idx].current
          == self.queue.compare.sub_iterator[first_idx].current
        {
          let idx = self
            .queue
            .pop()?
            .ok_or_else(|| LuceneError::number_format("queue is empty"))?;
          self.top[self.num_top] = idx;
          self.num_top += 1;
        } else {
          break;
        }
      }
    }
    let first_idx = self.top[0];
    self.current = self.queue.compare.sub_iterator[first_idx].current.take();

    Ok(())
  }
  fn push_top(&mut self) -> Result<()> {
    // call next() on each top, and put back into queue
    for i in 0..self.num_top {
      let idx = self.top[i];
      let top = &mut self.queue.compare.sub_iterator[idx];

      if top.iterator.has_next()? {
        top.current = top.iterator.next()?;
        self.queue.add(idx)?;
      } else {
        top.current = None;
      }
    }

    self.num_top = 0;
    Ok(())
  }
}

impl<E> IteratorExt for MergedIterator<E>
where
  E: IteratorExt,
  E::Item: Ord + Clone,
{
  type Item = E::Item;

  fn next(&mut self) -> Result<Option<Self::Item>> {
    // restore queue
    self.push_top()?;

    // gather equal top elements
    if self.queue.size() > 0 {
      self.pull_top()?;
    } else {
      self.current = None;
    }
    match self.current.take() {
      None => Err(LuceneError::no_such_element("no such element")),
      Some(current) => Ok(Some(current)),
    }
  }
  fn has_next(&self) -> Result<bool> {
    if self.queue.size() > 0 {
      return Ok(true);
    }

    for i in 0..self.num_top {
      let idx = self.top[i];
      if self.queue.compare.sub_iterator[idx].iterator.has_next()? {
        return Ok(true);
      }
    }
    Ok(false)
  }
}

pub struct TermMergeQueueCmp<E>
where
  E: IteratorExt,
  E::Item: Ord + Clone,
{
  sub_iterator: Vec<SubIterator<E>>,
}
impl<E> Compare<usize> for TermMergeQueueCmp<E>
where
  E: IteratorExt,
  E::Item: Ord + Clone,
{
  fn less_than(&self, a: &usize, b: &usize) -> Result<bool> {
    let cmp = self.sub_iterator[*a]
      .current
      .cmp(&self.sub_iterator[*b].current)
      .to_int();
    if cmp != 0 {
      Ok(cmp < 0)
    } else {
      Ok(self.sub_iterator[*a].index < self.sub_iterator[*b].index)
    }
  }
}

pub struct SubIterator<E>
where
  E: IteratorExt,
  E::Item: Ord + Clone,
{
  iterator: E,
  current: Option<E::Item>,
  index: usize,
}
