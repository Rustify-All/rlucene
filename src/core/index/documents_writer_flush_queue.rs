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
use crate::core::index::documents_writer_per_thread::FlushedSegment;
use crate::core::index::frozen_buffered_updates::FrozenBufferedUpdates;
use crate::core::index::index_reader::Identity;
use crate::core::store::directory::Directory;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::supplier::Supplier;
use parking_lot::{Mutex, ReentrantMutex};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicI32, Ordering};

pub(crate) struct DocumentsWriterFlushQueue<D>
where
  D: Directory,
{
  pub(crate) inner: Mutex<Inner<D>>,
  // Track tickets separately because the count exists before the ticket is constructed, so the
  // queue length would not reflect it.
  ticket_count: AtomicI32,
  purge_lock: ReentrantMutex<()>,
}
pub(crate) struct Inner<D>
where
  D: Directory,
{
  pub(crate) queue: VecDeque<Identity>,
  pub(crate) value: HashMap<Identity, FlushTicket<D>>,
}
impl<D> DocumentsWriterFlushQueue<D>
where
  D: Directory,
{
  pub(crate) fn new() -> Self {
    DocumentsWriterFlushQueue {
      inner: Mutex::new(Inner {
        queue: VecDeque::new(),
        value: HashMap::new(),
      }),
      ticket_count: AtomicI32::new(0),
      purge_lock: ReentrantMutex::new(()),
    }
  }
  pub(crate) fn add_ticket<S>(&self, mut ticket_supplier: S) -> Result<Option<Identity>>
  where
    S: Supplier<Option<FlushTicket<D>>>,
  {
    // Increment the ticket count first; freezing opens a window in which `any_changes` can fail.
    let mut inner = self.inner.lock();
    self.inc_tickets();
    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      let ticket_opt = ticket_supplier.get_mut()?;
      if let Some(ticket) = ticket_opt {
        let id = ticket.id.clone();
        inner.queue.push_back(ticket.id.clone());
        inner.value.insert(ticket.id.clone(), ticket);
        success = true;
        Ok(Some(id))
      } else {
        Ok(None)
      }
    }));
    if !success {
      self.dec_tickets();
    }
    match result {
      Ok(result) => result,
      Err(payload) => std::panic::resume_unwind(payload),
    }
  }
  fn inc_tickets(&self) {
    // incrementAndGet
    let new_count = self.ticket_count.fetch_add(1, Ordering::SeqCst) + 1;
    debug_assert!(new_count > 0);
  }

  fn dec_tickets(&self) {
    let new_count = self.ticket_count.fetch_sub(1, Ordering::SeqCst) - 1;
    debug_assert!(new_count >= 0);
  }

  pub(crate) fn add_segment(
    &self,
    ticket_index: &Identity,
    segment: FlushedSegment<D>,
  ) -> Result<()> {
    let mut inner = self.inner.lock();
    // the actual flush is done asynchronously and once done the FlushedSegment
    // is passed to the flush ticket
    inner
      .value
      .get_mut(ticket_index)
      .ok_or_else(|| LuceneError::illegal_state("could not get ticket"))?
      .set_segment(segment);
    Ok(())
  }
  pub(crate) fn mark_ticket_failed(&self, ticket_idx: Identity) -> Result<()> {
    let mut inner = self.inner.lock();
    let ticket = inner
      .value
      .get_mut(&ticket_idx)
      .ok_or_else(|| LuceneError::illegal_state("could not get ticket"))?;
    // to free the queue we mark tickets as failed just to clean up the queue.
    ticket.set_failed();
    Ok(())
  }

  pub(crate) fn has_tickets(&self) -> bool {
    let count = self.ticket_count.load(Ordering::SeqCst);
    debug_assert!(count >= 0, "ticket_count should be >= 0 but was: {count}");
    count != 0
  }
  pub(crate) fn inner_purge<F>(&self, consumer: F) -> Result<()>
  where
    F: Fn(FlushTicket<D>) -> Result<()>,
  {
    debug_assert!(self.purge_lock.is_locked());
    loop {
      let can_publish = {
        let inner = self.inner.lock();
        match inner.queue.front() {
          None => false,
          Some(id) => match inner.value.get(id) {
            None => {
              return Err(LuceneError::illegal_state(
                "id in inner.queue but not in inner.value",
              ));
            },
            Some(ticket) => ticket.can_publish(),
          },
        }
      };
      if can_publish {
        /*
         * if we block on publish -> lock IW -> lock BufferedDeletes we don't block
         * concurrent segment flushes just because they want to append to the queue.
         * the downside is that we need to force a purge on fullFlush since there could
         * be a ticket still in the queue.
         */
        let (id, head) = {
          let mut inner = self.inner.lock();
          let id = inner.queue.front().unwrap().clone();
          let head = inner.value.remove(&id).unwrap();
          (id, head)
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| consumer(head)));
        {
          let mut inner = self.inner.lock();
          let polled = inner.queue.pop_front().unwrap();
          self.dec_tickets();
          debug_assert!(polled == id);
        }
        match result {
          Ok(result) => result?,
          Err(payload) => std::panic::resume_unwind(payload),
        }
      } else {
        break;
      }
    }
    Ok(())
  }
  pub(crate) fn force_purge<F>(&self, consumer: F) -> Result<()>
  where
    F: Fn(FlushTicket<D>) -> Result<()>,
  {
    let _purge_guard = self.purge_lock.lock();
    self.inner_purge(consumer)
  }

  pub(crate) fn try_purge<F>(&self, consumer: F) -> Result<()>
  where
    F: Fn(FlushTicket<D>) -> Result<()>,
  {
    if let Some(_purge_guard) = self.purge_lock.try_lock() {
      self.inner_purge(consumer)?;
    }
    Ok(())
  }

  pub(crate) fn get_ticket_count(&self) -> i32 {
    self.ticket_count.load(Ordering::SeqCst)
  }
}

pub(crate) struct FlushTicket<D>
where
  D: Directory,
{
  frozen_updates: Option<FrozenBufferedUpdates>,
  has_segment: bool,
  segment: Option<FlushedSegment<D>>,
  failed: bool,
  published: bool,
  lock: Mutex<()>,
  id: Identity,
}
impl<D> FlushTicket<D>
where
  D: Directory,
{
  pub(crate) fn new(frozen_updates: Option<FrozenBufferedUpdates>, has_segment: bool) -> Self {
    FlushTicket {
      frozen_updates,
      has_segment,
      segment: None,
      failed: false,
      published: false,
      lock: Mutex::new(()),
      id: Identity::new(),
    }
  }
  pub(crate) fn can_publish(&self) -> bool {
    !self.has_segment || self.segment.is_some() || self.failed
  }

  pub(crate) fn mark_published(&mut self) {
    let _guard = self.lock.lock();
    debug_assert!(
      !self.published,
      "ticket was already published - can not publish twice"
    );
    self.published = true;
  }

  fn set_segment(&mut self, segment: FlushedSegment<D>) {
    debug_assert!(!self.failed, "cannot set segment on a failed ticket");
    self.segment = Some(segment);
  }

  fn set_failed(&mut self) {
    debug_assert!(self.segment.is_none());
    self.failed = true;
  }
  /// Returns the flushed segment, or `None` if this flush ticket doesn’t have a segment.
  /// This can occur when the ticket represents a flushed global frozen updates package.
  pub(crate) fn get_flushed_segment(&mut self) -> Option<&mut FlushedSegment<D>> {
    self.segment.as_mut()
  }
  /// Returns a frozen global deletes package.
  pub(crate) fn get_frozen_updates(&self) -> &FrozenBufferedUpdates {
    self.frozen_updates.as_ref().unwrap()
  }
  pub(crate) fn take_frozen_updates(&mut self) -> Option<FrozenBufferedUpdates> {
    self.frozen_updates.take()
  }
}
