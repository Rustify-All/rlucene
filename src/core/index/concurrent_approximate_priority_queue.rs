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
use std::cmp::{max, min};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use parking_lot::Mutex;

use crate::core::index::approximate_priority_queue::{ApproximatePriorityQueue, IdentityId};
use crate::core::util::error::lucene_error::{LuceneError, Result};

pub(crate) const MIN_CONCURRENCY: usize = 1;
pub(crate) const MAX_CONCURRENCY: usize = 256;
fn compute_thread_hash() -> usize {
  let mut hasher = DefaultHasher::new();
  std::thread::current().id().hash(&mut hasher);
  (hasher.finish() as usize) & 0xFFFF
}
thread_local! {
  static THREAD_HASH: usize = compute_thread_hash();
}
/// Concurrent version of [`ApproximatePriorityQueue`], which trades a bit more
/// of ordering for better concurrency by maintaining multiple sub
/// [`ApproximatePriorityQueue`]s that are locked independently. The number of
/// subs is computed dynamically based on hardware concurrency.
pub struct ConcurrentApproximatePriorityQueue<T> {
  pub(crate) queues: Vec<Mutex<ApproximatePriorityQueue<T>>>,
}

impl<T> ConcurrentApproximatePriorityQueue<T> {
  fn get_concurrency() -> usize {
    let core_count = std::thread::available_parallelism()
      .map(|n| n.get())
      .unwrap_or(1);
    // Aim for ~4 entries per slot when indexing with one thread per CPU
    // core. The trade-off is that if we set the concurrency too
    // high then we'll completely lose the bias towards larger
    // DWPTs. And if we set it too low then we risk seeing contention.
    let mut concurrency = core_count / 4;
    concurrency = max(MIN_CONCURRENCY, concurrency);
    concurrency = min(MAX_CONCURRENCY, concurrency);
    concurrency
  }

  pub(crate) fn new() -> Result<Self> {
    Self::with_concurrency(Self::get_concurrency())
  }

  pub(crate) fn with_concurrency(concurrency: usize) -> Result<Self> {
    if !(MIN_CONCURRENCY..=MAX_CONCURRENCY).contains(&concurrency) {
      return Err(LuceneError::illegal_argument(format!(
        "concurrency must be in [{MIN_CONCURRENCY}, {MAX_CONCURRENCY}], got {concurrency}"
      )));
    }
    let mut queues = Vec::with_capacity(concurrency);
    for _ in 0..concurrency {
      queues.push(Mutex::new(ApproximatePriorityQueue::new()));
    }
    Ok(Self { queues })
  }

  fn thread_hash() -> usize {
    THREAD_HASH
      .try_with(|hash| *hash)
      .unwrap_or_else(|_| compute_thread_hash())
  }
}

impl<T> ConcurrentApproximatePriorityQueue<T>
where
  T: IdentityId,
{
  pub(crate) fn add(&self, entry: T, weight: i64) {
    let start = Self::thread_hash() % self.queues.len();
    let (before, after) = self.queues.split_at(start);
    for queue in after.iter().chain(before) {
      if let Some(mut queue) = queue.try_lock() {
        queue.add(entry, weight);
        return;
      }
    }
    let mut queue = self.queues[start].lock();
    queue.add(entry, weight)
  }

  pub(crate) fn poll<F>(&self, predicate: F) -> Option<T>
  where
    F: Fn(&T) -> bool,
  {
    let start = Self::thread_hash() % self.queues.len();
    let (before, after) = self.queues.split_at(start);
    for queue in after.iter().chain(before) {
      if let Some(mut queue) = queue.try_lock()
        && let Some(entry) = queue.poll(&predicate)
      {
        return Some(entry);
      }
    }
    for queue in after.iter().chain(before) {
      let mut queue = queue.lock();
      if let Some(entry) = queue.poll(&predicate) {
        return Some(entry);
      }
    }
    None
  }

  pub(crate) fn contains(&self, o: &str) -> bool {
    for mutex in &self.queues {
      let queue = mutex.lock();
      if queue.contains(o) {
        return true;
      }
    }
    false
  }

  pub(crate) fn remove(&self, o: &str) -> Option<T> {
    for mutex in &self.queues {
      let mut queue = mutex.lock();
      match queue.remove(o) {
        Some(entry) => return Some(entry),
        None => continue,
      }
    }
    None
  }
}
