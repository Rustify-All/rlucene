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
use crate::core::index::approximate_priority_queue::IdentityId;
use crate::core::index::documents_writer::{DocumentsWriter, FlushNotifications};
use crate::core::index::documents_writer_delete_queue::DocumentsWriterDeleteQueue;

use crate::core::index::documents_writer_per_thread::{DocumentsWriterPerThread, State};
use crate::core::index::documents_writer_per_thread_pool::{
  DocumentsWriterPerThreadPool, DwptWrapper,
};
use crate::core::index::documents_writer_stall_control::DocumentsWriterStallControl;
use crate::core::index::flush_policy::FlushPolicy;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::DISABLE_AUTO_FLUSH;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::lockable_concurrent_approximate_priority_queue::Lock;
use crate::core::store::directory::Directory;
use crate::core::util::TryIntoInt;
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::info_stream::{InfoStream, InfoStreamMT};
use parking_lot::{Condvar, Mutex, MutexGuard};
use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

pub struct DocumentsWriterFlushControl<D>
where
  D: Directory,
{
  flush_deletes: AtomicBool,
  pub(crate) info_stream: InfoStreamMT,
  pub(crate) inner: Mutex<Inner<D>>,
  pub(crate) stall_control: DocumentsWriterStallControl,
  pausing: Condvar,
  pub(crate) per_thread_pool: DocumentsWriterPerThreadPool<D>,
  pub(crate) delete_queue: Mutex<Arc<DocumentsWriterDeleteQueue>>,
}
pub struct Inner<D>
where
  D: Directory,
{
  // only with
  flush_by_ram_was_disabled: bool,
  // only with assert
  max_configured_ram_buffer: f64,
  hard_max_bytes_per_dwpt: i64,
  active_bytes: i64,
  flush_bytes: i64,
  num_pending: i32,
  full_flush: bool,
  // only for assertion that we don't get stale DWPTs from the pool
  full_flush_mark_done: bool,
  // The flushQueue is used to concurrently distribute DWPTs that are ready to be flushed ie. when a
  // full flush is in
  // progress. This might be triggered by a commit or NRT refresh. The trigger will only walk all
  // eligible DWPTs and
  // mark them as flushable putting them in the flushQueue ready for other threads (ie. indexing
  // threads) to help flushing
  flush_queue: VecDeque<Arc<DwptWrapper<D>>>,
  // only for safety reasons if a DWPT is close to the RAM limit
  blocked_flushes: VecDeque<Arc<DwptWrapper<D>>>,
  // flushingWriters holds all currently flushing writers. There might be writers in this list that
  // are also in the flushQueue which means that writers in the flushingWriters list are not
  // necessarily
  // already actively flushing. They are only in the state of flushing and might be picked up in the
  // future by
  // polling the flushQueue
  flushing_writers: Vec<Arc<DwptWrapper<D>>>,
  closed: bool,
  stall_start_ns: Instant,
  // only with assert
  peak_active_bytes: i64,
  // only with assert
  peak_flush_bytes: i64,
  // only with assert
  peak_net_bytes: i64,
  // only with assert
  peak_delta: i64,
  num_docs_since_stalled: i32,
}

impl<D> DocumentsWriterFlushControl<D>
where
  D: Directory,
{
  pub(crate) fn new<L>(config: &L, delete_queue: Arc<DocumentsWriterDeleteQueue>) -> Result<Self>
  where
    L: LiveIndexWriterConfig,
  {
    // Initialize the Inner state with defaults
    let inner = Inner {
      flush_by_ram_was_disabled: false,
      max_configured_ram_buffer: 0f64,
      hard_max_bytes_per_dwpt: (config.get_ram_per_thread_hard_limit_mb() * 1024 * 1024) as i64,
      active_bytes: 0,
      flush_bytes: 0,
      num_pending: 0,
      full_flush: false,
      full_flush_mark_done: false,
      flush_queue: VecDeque::new(),
      blocked_flushes: VecDeque::new(),
      flushing_writers: Vec::new(),
      closed: false,
      stall_start_ns: Instant::now(),
      peak_active_bytes: 0,
      peak_flush_bytes: 0,
      peak_net_bytes: 0,
      peak_delta: 0,
      num_docs_since_stalled: 0,
    };

    Ok(DocumentsWriterFlushControl {
      flush_deletes: AtomicBool::new(false),
      info_stream: config.get_info_stream(),
      inner: Mutex::new(inner),
      stall_control: DocumentsWriterStallControl::new(),
      pausing: Condvar::new(),
      per_thread_pool: DocumentsWriterPerThreadPool::new()?,
      delete_queue: Mutex::new(delete_queue),
    })
  }

  pub(crate) fn active_bytes(&self, inner: Option<&Inner<D>>) -> i64 {
    let inner = match inner {
      Some(inner) => inner,
      None => &mut *self.inner.lock(),
    };
    inner.active_bytes
  }

  pub(crate) fn get_flushing_bytes(&self, inner: Option<&Inner<D>>) -> i64 {
    let inner = match inner {
      Some(inner) => inner,
      None => &mut *self.inner.lock(),
    };
    inner.flush_bytes
  }

  pub(crate) fn net_bytes(&self, inner: Option<&Inner<D>>) -> i64 {
    let inner = match inner {
      Some(inner) => inner,
      None => &mut *self.inner.lock(),
    };
    inner.flush_bytes + inner.active_bytes
  }

  fn stall_limit_bytes<L>(&self, config: &L) -> i64
  where
    L: LiveIndexWriterConfig,
  {
    let max_ram_mb = config.get_ram_buffer_size_mb();
    if max_ram_mb != DISABLE_AUTO_FLUSH as f64 {
      (2.0 * (max_ram_mb * 1024.0 * 1024.0)) as i64
    } else {
      i64::MAX
    }
  }
  fn assert_memory<L>(&self, inner: &mut Inner<D>, config: &L) -> bool
  where
    L: LiveIndexWriterConfig,
  {
    let max_ram_mb = config.get_ram_buffer_size_mb();
    // We can only assert if we have always been flushing by RAM usage; otherwise the assert will
    // false trip if e.g. the
    // flush-by-doc-count * doc size was large enough to use far more RAM than the sudden change to
    // IWC's maxRAMBufferSizeMB:
    if max_ram_mb != DISABLE_AUTO_FLUSH as f64 && !inner.flush_by_ram_was_disabled {
      // for this assert we must be tolerant to ram buffer changes!
      inner.max_configured_ram_buffer = inner.max_configured_ram_buffer.max(max_ram_mb);
      let flush_bytes = inner.flush_bytes;
      let active_bytes = inner.active_bytes;
      let num_pending = inner.num_pending;

      let ram = flush_bytes + active_bytes;
      let ram_buffer_bytes = (inner.max_configured_ram_buffer * 1024.0 * 1024.0) as i64;
      // take peakDelta into account - worst case is that all flushing, pending and blocked DWPT had
      // maxMem and the last doc had the peakDelta

      // 2 * ramBufferBytes -> before we stall we need to cross the 2xRAM Buffer border this is
      // still a valid limit
      // (numPending + numFlushingDWPT() + numBlockedFlushes()) * peakDelta) -> those are the total
      // number of DWPT that are not active but not yet fully flushed
      // all of them could theoretically be taken out of the loop once they crossed the RAM buffer
      // and the last document was the peak delta
      // (numDocsSinceStalled * peakDelta) -> at any given time there could be n threads in flight
      // that crossed the stall control before we reached the limit and each of them could hold a
      // peak document
      let expected = (2 * ram_buffer_bytes)
        + ((num_pending as i64
          + self.num_flushing_dwpt(Some(inner)) as i64
          + self.num_blocked_flushes(Some(inner)) as i64)
          * inner.peak_delta)
        + (inner.num_docs_since_stalled as i64 * inner.peak_delta);
      // the expected ram consumption is an upper bound at this point and not really the expected
      // consumption
      if inner.peak_delta < (ram_buffer_bytes >> 1) {
        /*
         * if we are indexing with very low maxRamBuffer like 0.1MB memory can
         * easily overflow if we check out some DWPT based on docCount and have
         * several DWPT in flight indexing large documents (compared to the ram
         * buffer). This means that those DWPT and their threads will not hit
         * the stall control before asserting the memory which would in turn
         * fail. To prevent this we only assert if the largest document seen
         * is smaller than the 1/2 of the maxRamBufferMB
         */
        debug_assert!(
          ram <= expected,
          "actual mem: {} byte, expected mem: {} byte, flush mem: {}, active mem: {}, pending DWPT: {}, flushing DWPT: {}, blocked DWPT: {}, peakDelta mem: {} bytes, ramBufferBytes={}, maxConfiguredRamBuffer={}",
          ram,
          expected,
          flush_bytes,
          active_bytes,
          num_pending,
          self.num_flushing_dwpt(Some(inner)),
          self.num_blocked_flushes(Some(inner)),
          inner.peak_delta,
          ram_buffer_bytes,
          inner.max_configured_ram_buffer
        );
      }
    } else {
      inner.flush_by_ram_was_disabled = true;
    }
    true
  }

  // only for asserts
  fn update_peaks(&self, delta: i64, inner: &mut Inner<D>) -> bool {
    let net = self.net_bytes(Some(inner));
    let active = inner.active_bytes;
    let flush = inner.flush_bytes;

    inner.peak_active_bytes = inner.peak_active_bytes.max(active);
    inner.peak_flush_bytes = inner.peak_flush_bytes.max(flush);
    inner.peak_net_bytes = inner.peak_net_bytes.max(net);
    inner.peak_delta = inner.peak_delta.max(delta);

    true
  }
  /// Return the smallest number of bytes that we would like to make sure to not miss from the global RAM accounting.
  fn ram_buffer_granularity<L>(&self, config: &L) -> i64
  where
    L: LiveIndexWriterConfig,
  {
    let mut ram_buffer_mb = config.get_ram_buffer_size_mb();
    if ram_buffer_mb == DISABLE_AUTO_FLUSH as f64 {
      ram_buffer_mb = config.get_ram_per_thread_hard_limit_mb() as f64;
    }
    // No more than ~0.1% of the RAM buffer size.
    let mut granularity = (ram_buffer_mb * 1024.0) as i64;
    // Or 16kB, so that with e.g. 64 active DWPTs, we'd never be missing more than 64*16kB = 1MB in
    // the global RAM buffer accounting.
    granularity = granularity.min(16 * 1024);
    granularity
  }
  pub(crate) fn do_after_document<L>(
    &self,
    per_thread: &MutexGuard<'_, DocumentsWriterPerThread<D>>,
    config: &L,
  ) -> Result<Option<Arc<DwptWrapper<D>>>>
  where
    L: LiveIndexWriterConfig,
  {
    let delta = per_thread.get_commit_last_bytes_used_delta()?;
    // in order to prevent contention in the case of many threads indexing small documents
    // we skip ram accounting unless the DWPT accumulated enough ram to be worthwhile
    if config.get_max_buffered_docs() == DISABLE_AUTO_FLUSH
      && delta < self.ram_buffer_granularity(config)
    {
      // Skip accounting for now, we'll come back to it later when the delta is bigger
      return Ok(None);
    }
    let mut inner = self.inner.lock();
    // we need to commit this under lock but calculate it outside of the lock to minimize the time
    // this lock is held
    // per document. The reason we update this under lock is that we mark DWPTs as pending without
    // acquiring it's
    // lock in `set_flush_pending`; this also reads committed bytes and modifies the
    // flush/activeBytes.
    // In the future we can clean this up to be more intuitive.
    per_thread.commit_last_bytes_used(delta)?;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      // We need to differentiate here if we are pending since setFlushPending
      // moves the perThread memory to the flushBytes and we could be set to
      // pending during a delete
      if per_thread.is_flush_pending() {
        inner.flush_bytes += delta;
        self.update_peaks(delta, &mut inner);
      } else {
        inner.active_bytes += delta;
        self.update_peaks(delta, &mut inner);
        config
          .get_flush_policy()
          .on_change(self, &mut inner, Some(per_thread), config)?;
        if !per_thread.is_flush_pending()
          && per_thread.ram_bytes_used()? > inner.hard_max_bytes_per_dwpt
        {
          // Safety check to prevent a single DWPT exceeding its RAM limit. This
          // is super important since we can not address more than 2048 MB per DWPT
          self.set_flush_pending(&per_thread.state, Some(&mut inner), config)?;
        }
      }
      self.checkout(&mut inner, per_thread, false, config)
    }));

    let stall = self.update_stall_state(&mut inner, config)?;
    debug_assert!(
      self.assert_num_docs_since_stalled(stall, &mut inner)
        && self.assert_memory(&mut inner, config)
    );

    match result {
      Ok(result) => result,
      Err(payload) => std::panic::resume_unwind(payload),
    }
  }
  fn checkout<L>(
    &self,
    inner: &mut Inner<D>, // Same to Java's Thread.holdsLock(this)
    per_thread: &MutexGuard<'_, DocumentsWriterPerThread<D>>,
    mark_pending: bool,
    config: &L,
  ) -> Result<Option<Arc<DwptWrapper<D>>>>
  where
    L: LiveIndexWriterConfig,
  {
    if inner.full_flush {
      if per_thread.is_flush_pending() {
        self.checkout_and_block(per_thread, inner);
        return self.next_pending_flush(Some(inner), config);
      }
    } else {
      if mark_pending {
        debug_assert!(!per_thread.is_flush_pending());
        self.set_flush_pending(&per_thread.state, Some(inner), config)?;
      }
      if per_thread.is_flush_pending() {
        return Ok(Some(self.check_out_for_flush(per_thread, inner, config)?));
      }
    }
    Ok(None)
  }
  fn assert_num_docs_since_stalled(&self, stalled: bool, inner: &mut Inner<D>) -> bool {
    //  updates the number of documents "finished" while we are in a stalled state.
    //  this is important for asserting memory upper bounds since it corresponds
    //  to the number of threads that are in-flight and crossed the stall control
    //  check before we actually stalled.
    // See `assert_memory`.
    if stalled {
      inner.num_docs_since_stalled += 1;
    } else {
      inner.num_docs_since_stalled = 0;
    }
    true
  }
  pub(crate) fn do_after_flush<L>(
    &self,
    inner: Option<&mut Inner<D>>,
    dwpt: Arc<DwptWrapper<D>>,
    config: &L,
  ) -> Result<()>
  where
    L: LiveIndexWriterConfig,
  {
    let inner = match inner {
      Some(inner) => inner,
      None => &mut *self.inner.lock(),
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      debug_assert!(inner.flushing_writers.contains(&dwpt));
      if let Some(pos) = inner
        .flushing_writers
        .iter()
        .position(|w| w.state.id == dwpt.state.id)
      {
        inner.flushing_writers.remove(pos);
      }
      inner.flush_bytes -= dwpt.state.get_last_committed_bytes_used();
      debug_assert!(self.assert_memory(inner, config));
      Ok(())
    }));

    let stall_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      self.update_stall_state(inner, config)
    }));
    self.pausing.notify_all();

    match stall_result {
      Ok(result) => result?,
      Err(payload) => std::panic::resume_unwind(payload),
    };
    match result {
      Ok(result) => result,
      Err(payload) => std::panic::resume_unwind(payload),
    }
  }
  fn update_stall_state<L>(&self, inner: &mut Inner<D>, config: &L) -> Result<bool>
  where
    L: LiveIndexWriterConfig,
  {
    let limit = self.stall_limit_bytes(config);
    let active = inner.active_bytes;
    let flush = inner.flush_bytes;
    let stall = (active + flush) > limit && active < limit && !inner.closed;

    if self.info_stream.is_enabled("DWFC") && stall != self.stall_control.any_stalled_threads() {
      if stall {
        self.info_stream.message(
          "DW",
          &format!(
            "now stalling flushes: netBytes: {:.1} MB flushBytes: {:.1} MB fullFlush: {}",
            (self.net_bytes(Some(inner)) as f64) / 1024.0 / 1024.0,
            (self.get_flushing_bytes(Some(inner)) as f64) / 1024.0 / 1024.0,
            inner.full_flush
          ),
        )?;
        inner.stall_start_ns = Instant::now()
      } else {
        let elapsed = Instant::now()
          .duration_since(inner.stall_start_ns)
          .as_secs_f64()
          * 1000.0;
        self.info_stream.message(
                        "DW",
                        &format!(
                            "done stalling flushes for {:.1} msec: netBytes: {:.1} MB flushBytes: {:.1} MB fullFlush: {}",
                            elapsed,
                            (self.net_bytes(Some(inner)) as f64) / 1024.0 / 1024.0,
                            (self.get_flushing_bytes(Some(inner)) as f64) / 1024.0 / 1024.0,
                            inner.full_flush
                        ),
                    )?;
      }
    }

    self.stall_control.update_stalled(stall);
    Ok(stall)
  }

  pub(crate) fn wait_for_flush(&self) {
    let mut inner = self.inner.lock();
    while !inner.flushing_writers.is_empty() {
      self.pausing.wait(&mut inner);
    }
  }
  /// Sets flush pending state on the given [`DocumentsWriterPerThread`] state.
  /// The [`DocumentsWriterPerThread`] must have indexed at least one document
  /// and must not already be pending.
  pub(crate) fn set_flush_pending<L>(
    &self,
    state: &State,
    inner: Option<&mut Inner<D>>,
    config: &L,
  ) -> Result<()>
  where
    L: LiveIndexWriterConfig,
  {
    let inner = match inner {
      Some(inner) => inner,
      None => &mut *self.inner.lock(),
    };
    debug_assert!(!state.is_flush_pending());
    if state.get_num_docs_in_ram() > 0 {
      state.set_flush_pending()?;
      let bytes = state.get_last_committed_bytes_used();
      inner.flush_bytes += bytes;
      inner.active_bytes -= bytes;
      inner.num_pending += 1;
      debug_assert!(self.assert_memory(inner, config));
    }
    Ok(())
  }
  pub(crate) fn do_on_abort<L>(&self, per_thread: &Arc<DwptWrapper<D>>, config: &L) -> Result<()>
  where
    L: LiveIndexWriterConfig,
  {
    let mut inner = self.inner.lock();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      let dwpt = per_thread.dwpt.lock();
      debug_assert!(self.per_thread_pool.is_registered(per_thread.id()));
      let bytes = dwpt.get_last_committed_bytes_used();
      if dwpt.is_flush_pending() {
        inner.flush_bytes -= bytes;
      } else {
        inner.active_bytes -= bytes;
      };
      debug_assert!(self.assert_memory(&mut inner, config));
      // Take it out of the loop this DWPT is stale
      Ok(())
    }));

    self.update_stall_state(&mut inner, config)?;
    let checked_out = {
      let dwpt = per_thread.dwpt.lock();
      self.per_thread_pool.checkout(&dwpt)
    };
    debug_assert!(checked_out.is_some());

    match result {
      Ok(result) => result,
      Err(payload) => std::panic::resume_unwind(payload),
    }
  }
  /// To be called only by the owner of this object's monitor lock
  fn checkout_and_block(
    &self,
    per_thread: &MutexGuard<'_, DocumentsWriterPerThread<D>>,
    inner: &mut Inner<D>,
  ) {
    let id = &per_thread.state.id;
    debug_assert!(self.per_thread_pool.is_registered(id));
    debug_assert!(
      per_thread.is_flush_pending(),
      "can not block non-pending threadstate"
    );
    debug_assert!(inner.full_flush, "can not block if fullFlush == false");
    inner.num_pending -= 1;
    match self.per_thread_pool.checkout(per_thread) {
      Some(v) => {
        inner.blocked_flushes.push_back(v);
      },
      None => {
        debug_assert!(false, "should not be here")
      },
    }
  }
  fn check_out_for_flush<L>(
    &self,
    per_thread: &MutexGuard<'_, DocumentsWriterPerThread<D>>,
    inner: &mut Inner<D>, // Same to Java's Thread.holdsLock(this)
    config: &L,
  ) -> Result<Arc<DwptWrapper<D>>>
  where
    L: LiveIndexWriterConfig,
  {
    debug_assert!(per_thread.is_flush_pending());
    debug_assert!(per_thread.state.is_locked());
    debug_assert!(self.per_thread_pool.is_registered(&per_thread.state.id));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
      || -> Result<Arc<DwptWrapper<D>>> {
        inner.num_pending -= 1;
        let v = match self.per_thread_pool.checkout(per_thread) {
          Some(v) => {
            self.add_flushing_dwpt(v.clone(), inner);
            v
          },
          None => return Err(LuceneError::illegal_state("DWPT not registered in pool")),
        };
        Ok(v)
      },
    ));
    self.update_stall_state(inner, config)?;
    match result {
      Ok(result) => result,
      Err(payload) => std::panic::resume_unwind(payload),
    }
  }
  fn add_flushing_dwpt(&self, per_thread: Arc<DwptWrapper<D>>, inner: &mut Inner<D>) {
    debug_assert!(
      !inner.flushing_writers.contains(&per_thread),
      "DWPT is already flushing"
    );
    // Record the flushing DWPT to reduce flushBytes in doAfterFlush
    inner.flushing_writers.push(per_thread);
  }
  pub(crate) fn next_pending_flush<L>(
    &self,
    inner: Option<&mut Inner<D>>,
    config: &L,
  ) -> Result<Option<Arc<DwptWrapper<D>>>>
  where
    L: LiveIndexWriterConfig,
  {
    let num_pending;
    let full_flush;
    {
      let inner = if let Some(inner) = inner {
        inner
      } else {
        &mut *self.inner.lock()
      };
      if let Some(dwpt) = inner.flush_queue.pop_front() {
        // update stall state before returning
        self.update_stall_state(inner, config)?;
        return Ok(Some(dwpt));
      }
      full_flush = inner.full_flush;
      num_pending = inner.num_pending;
    }
    if num_pending > 0 && !full_flush {
      let dwpts = self.per_thread_pool.iterator();
      for (id, next) in &dwpts {
        if next.state.is_flush_pending() && next.try_lock() {
          let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if self.per_thread_pool.is_registered(id) {
              let mut inner = self.inner.lock();
              return Ok(Some(self.check_out_for_flush(
                &next.dwpt.lock(),
                &mut inner,
                config,
              )?));
            } else {
              Ok(None)
            }
          }));
          next.unlock();
          return match result {
            Ok(result) => result,
            Err(payload) => std::panic::resume_unwind(payload),
          };
        }
      }
    }
    Ok(None)
  }

  pub(crate) fn close(&self) {
    let mut inner = self.inner.lock();
    inner.closed = true;
  }

  pub(crate) fn do_on_delete<L>(&self, config: &L) -> Result<()>
  where
    L: LiveIndexWriterConfig,
  {
    let mut inner = self.inner.lock();
    config
      .get_flush_policy()
      .on_change(self, &mut inner, None, config)?;
    Ok(())
  }

  /// Returns heap bytes currently consumed by buffered deletes/updates that would be freed if we pushed all deletes.
  /// This does not include bytes consumed by already pushed delete/update packets.
  pub(crate) fn get_delete_bytes_used(&self) -> Result<i64> {
    self.delete_queue.lock().ram_bytes_used()
  }

  pub(crate) fn num_flushing_dwpt(&self, inner: Option<&Inner<D>>) -> usize {
    let inner = match inner {
      Some(inner) => inner,
      None => &*self.inner.lock(),
    };
    inner.flushing_writers.len()
  }
  pub(crate) fn get_and_reset_apply_all_deletes(&self) -> bool {
    self.flush_deletes.swap(false, Ordering::SeqCst)
  }
  /// Check whether deletes need to be applied. This can be used as a pre-flight check before calling
  /// [`getAndResetApplyAllDeletes()`](Self::get_and_reset_apply_all_deletes) to make sure that a single thread applies deletes.
  pub(crate) fn get_apply_all_deletes(&self) -> bool {
    self.flush_deletes.load(Ordering::SeqCst)
  }

  pub(crate) fn set_apply_all_deletes(&self) {
    self.flush_deletes.store(true, Ordering::SeqCst);
  }

  pub(crate) fn obtain_and_lock(&self, writer: &IndexWriter<D>) -> Result<Arc<DwptWrapper<D>>> {
    loop {
      {
        let inner = self.inner.lock();
        if inner.closed {
          return Err(LuceneError::already_closed("flush control is closed"));
        }
      }
      let per_thread = self
        .per_thread_pool
        .get_and_lock(writer, || self.delete_queue.lock().clone())?;
      if Arc::ptr_eq(&per_thread.state.delete_queue, &self.delete_queue.lock()) {
        // simply return the DWPT even in a flush all case since we already hold the lock and the
        // DWPT is not stale
        // since it has the current delete queue associated with it. This means we have established
        // a happens-before
        // relationship and all docs indexed into this DWPT are guaranteed to not be flushed with
        // the currently
        // progress full flush.
        return Ok(per_thread);
      } else {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
          #[cfg(debug_assertions)]
          {
            let inner = self.inner.lock();
            debug_assert!(
              inner.full_flush && !inner.full_flush_mark_done,
              "found a stale DWPT but full flush mark phase is already done fullFlush: {} markDone: {}",
              inner.full_flush,
              inner.full_flush_mark_done
            );
          }
        }));
        per_thread.unlock();
        if let Err(payload) = result {
          std::panic::resume_unwind(payload);
        }
      }
    }
  }
  pub(crate) fn mark_for_full_flush<FN, L>(
    &self,
    documents_writer: &DocumentsWriter<D, FN>,
    guard: &MutexGuard<'_, ()>,
    config: &L,
  ) -> Result<i64>
  where
    FN: FlushNotifications,
    L: LiveIndexWriterConfig,
  {
    let flushing_queue;
    let seq_no = {
      let mut inner = self.inner.lock();
      debug_assert!(
        !inner.full_flush,
        "called mark_for_full_flush while already in full flush"
      );
      debug_assert!(
        !inner.full_flush_mark_done,
        "fullFlushMarkDone is already true"
      );

      inner.full_flush = true;
      flushing_queue = self.delete_queue.lock().clone();
      // Set a new delete queue - all subsequent DWPT will use this queue until
      // we do another full flush
      self.per_thread_pool.lock_new_writers();
      // no new thread-states while we do a flush otherwise the seqNo
      // accounting might be off

      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<i64> {
        let size = self.per_thread_pool.size();
        // Insert a gap in seqNo of current active thread count, in the worst case each of those
        // threads now have one operation in flight.  It's fine
        // if we have some sequence numbers that were never assigned:
        documents_writer.reset_delete_queue(guard, size.try_convert()?)
      }));
      self.per_thread_pool.unlock_new_writers();
      match result {
        Ok(result) => result,
        Err(payload) => std::panic::resume_unwind(payload),
      }
    }?;

    let mut full_flush_buffer = Vec::new();
    let dwpts = {
      self
        .per_thread_pool
        .filter_and_lock(|v| Arc::ptr_eq(&v.state.delete_queue, &flushing_queue))?
    };

    for dwpt in dwpts {
      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        let next = dwpt.dwpt.lock();
        if next.get_num_docs_in_ram() > 0 {
          let flushing_dwpt = {
            let mut inner = self.inner.lock();
            if !next.is_flush_pending() {
              self.set_flush_pending(&next.state, Some(&mut inner), config)?;
            }
            self.check_out_for_flush(&next, &mut inner, config)?
          };
          debug_assert!(Arc::ptr_eq(&dwpt, &flushing_dwpt));
          full_flush_buffer.push(flushing_dwpt);
        } else {
          // it's possible that we get a DWPT with 0 docs if we flush concurrently to
          // threads getting DWPTs from the pool. In this case we simply remove it from
          // the pool and drop it on the floor.
          let checked_out = self.per_thread_pool.checkout(&next);
          debug_assert!(checked_out.is_some());
        }
        Ok(())
      }));
      dwpt.unlock();
      match result {
        Ok(result) => result?,
        Err(payload) => std::panic::resume_unwind(payload),
      }
    }

    {
      // make sure we move all DWPT that are where concurrently marked as
      // pending and moved to blocked are moved over to the flushQueue. There is
      // a chance that this happens since we marking DWPT for full flush without
      // blocking indexing
      let delete_queue = self.delete_queue.lock().clone();
      let mut inner = self.inner.lock();
      self.prune_blocked_queue(&flushing_queue, &mut inner);
      debug_assert!(self.assert_blocked_flushes(&inner, &delete_queue));
      inner.flush_queue.extend(full_flush_buffer);
      self.update_stall_state(&mut inner, config)?;
      inner.full_flush_mark_done = true;
    }

    debug_assert!(self.assert_active_delete_queue());
    debug_assert!(flushing_queue.get_last_sequence_number() <= flushing_queue.get_max_seq_no());

    Ok(seq_no)
  }
  pub(crate) fn assert_active_delete_queue(&self) -> bool {
    let queue = self.delete_queue.lock().clone();
    let dwpts = self.per_thread_pool.iterator();
    for next in dwpts.values() {
      debug_assert!(
        Arc::ptr_eq(&next.state.delete_queue, &queue),
        "{}",
        format!(
          "num_docs: {}, next_queue_gen: {}, dwpt_queue_gen: {}",
          next.state.num_docs_in_ram.load(Relaxed),
          queue.generation,
          next.state.delete_queue.generation
        )
      );
    }
    true
  }

  /// Prunes the blockedQueue by removing all DWPTs that are associated with the given flush queue.
  fn prune_blocked_queue(
    &self,
    flushing_queue: &Arc<DocumentsWriterDeleteQueue>,
    inner: &mut Inner<D>, // Same to Java's Thread.holdsLock(this)
  ) {
    let mut idxs = Vec::new();
    for (i, dwpt) in inner.blocked_flushes.iter().enumerate() {
      if Arc::ptr_eq(&dwpt.state.delete_queue, flushing_queue) {
        idxs.push(i);
      }
    }

    for &i in idxs.iter().rev() {
      if let Some(dwpt) = inner.blocked_flushes.remove(i) {
        self.add_flushing_dwpt(dwpt.clone(), inner);
        inner.flush_queue.push_back(dwpt);
      }
    }
  }
  pub(crate) fn finish_full_flush<L>(&self, config: &L) -> Result<()>
  where
    L: LiveIndexWriterConfig,
  {
    let delete_queue = self.delete_queue.lock().clone();
    let mut inner = self.inner.lock();
    debug_assert!(inner.full_flush);
    debug_assert!(inner.flush_queue.is_empty());
    debug_assert!(
      inner.flushing_writers.is_empty(),
      "flushing_writers must be empty"
    );

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      if !inner.blocked_flushes.is_empty() {
        debug_assert!(self.assert_blocked_flushes(&inner, &delete_queue));
        self.prune_blocked_queue(&delete_queue, &mut inner);
        debug_assert!(
          inner.blocked_flushes.is_empty(),
          "blocked_flushes must be empty after pruning"
        );
      }
      Ok(())
    }));

    inner.full_flush_mark_done = false;
    inner.full_flush = false;
    self.update_stall_state(&mut inner, config)?;
    match result {
      Ok(result) => result,
      Err(payload) => std::panic::resume_unwind(payload),
    }
  }
  pub(crate) fn assert_blocked_flushes(
    &self,
    inner: &Inner<D>,
    flushing_queue: &Arc<DocumentsWriterDeleteQueue>,
  ) -> bool {
    for blocked in inner.blocked_flushes.iter() {
      debug_assert!(Arc::ptr_eq(&blocked.state.delete_queue, flushing_queue));
    }
    true
  }
  pub(crate) fn abort_full_flushes<FN, L>(
    &self,
    documents_writer: &DocumentsWriter<D, FN>,
    config: &L,
  ) -> Result<()>
  where
    FN: FlushNotifications,
    L: LiveIndexWriterConfig,
  {
    let mut inner = self.inner.lock();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      self.abort_pending_flushes(Some(&mut inner), config, documents_writer)
    }));

    inner.full_flush_mark_done = false;
    inner.full_flush = false;

    match result {
      Ok(result) => result,
      Err(payload) => std::panic::resume_unwind(payload),
    }
  }

  pub(crate) fn abort_pending_flushes<FN, L>(
    &self,
    inner: Option<&mut Inner<D>>,
    config: &L,
    documents_writer: &DocumentsWriter<D, FN>,
  ) -> Result<()>
  where
    FN: FlushNotifications,
    L: LiveIndexWriterConfig,
  {
    let inner = match inner {
      Some(inner) => inner,
      None => &mut *self.inner.lock(),
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      let flush_queue = std::mem::take(&mut inner.flush_queue);

      for dwpt_wrapper in flush_queue {
        let abort_result =
          std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
            let num_docs_in_ram = {
              let dwpt = dwpt_wrapper.dwpt.lock();
              dwpt.get_num_docs_in_ram()
            };
            documents_writer.subtract_flushed_num_docs(num_docs_in_ram);
            dwpt_wrapper.dwpt.lock().abort()?;
            Ok(())
          }));

        self.do_after_flush(Some(inner), dwpt_wrapper.clone(), config)?;
        match abort_result {
          Ok(_) => {},
          Err(payload) => std::panic::resume_unwind(payload),
        }
      }

      let blocked_flushes = std::mem::take(&mut inner.blocked_flushes);

      for dwpt_wrapper in blocked_flushes {
        let abort_result =
          std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
            // add the blockedFlushes for correct accounting in doAfterFlush
            self.add_flushing_dwpt(dwpt_wrapper.clone(), inner);
            let num_docs_in_ram = {
              let dwpt = dwpt_wrapper.dwpt.lock();
              dwpt.get_num_docs_in_ram()
            };
            documents_writer.subtract_flushed_num_docs(num_docs_in_ram);
            dwpt_wrapper.dwpt.lock().abort()?;
            Ok(())
          }));
        self.do_after_flush(Some(inner), dwpt_wrapper.clone(), config)?;
        match abort_result {
          Ok(_) => {},
          Err(payload) => std::panic::resume_unwind(payload),
        }
      }

      Ok(())
    }));

    inner.flush_queue.clear();
    inner.blocked_flushes.clear();

    self.update_stall_state(inner, config)?;

    match result {
      Ok(result) => result,
      Err(payload) => std::panic::resume_unwind(payload),
    }
  }

  /// Returns `true` if a full flush is currently running
  pub(crate) fn is_full_flush(&self) -> bool {
    let inner = self.inner.lock();
    inner.full_flush
  }

  /// Returns the number of flushes that are already checked out but not yet actively flushing
  pub(crate) fn num_queued_flushes(&self) -> usize {
    let inner = self.inner.lock();
    inner.flush_queue.len()
  }

  /// Returns the number of flushes that are checked out but not yet available for flushing.
  /// This only applies during a full flush if a DWPT needs flushing but must not be flushed
  /// until the full flush has finished.
  pub(crate) fn num_blocked_flushes(&self, inner: Option<&Inner<D>>) -> i32 {
    let inner = match inner {
      Some(inner) => inner,
      None => &*self.inner.lock(),
    };
    inner.blocked_flushes.len() as i32
  }

  /// This method will block if too many DWPT are currently flushing and no checked out DWPT are available
  pub(crate) fn wait_if_stalled(&self) {
    self.stall_control.wait_if_stalled();
  }

  /// Returns `true` iff stalled.
  pub(crate) fn any_stalled_threads(&self) -> bool {
    self.stall_control.any_stalled_threads()
  }
  pub(crate) fn find_largest_non_pending_writer(&self) -> Result<Option<Arc<DwptWrapper<D>>>> {
    let mut max_ram_using_writer: Option<Arc<DwptWrapper<D>>> = None;
    // Note: should be initialized to -1 since some DWPTs might return 0 if their RAM usage has not
    // been committed yet.
    let mut max_ram_so_far: i64 = -1;
    let mut count = 0;

    for (_id, next) in self.per_thread_pool.iterator() {
      if !next.state.is_flush_pending() && next.state.get_num_docs_in_ram() > 0 {
        let next_ram = next.state.get_last_committed_bytes_used();

        if self.info_stream.is_enabled("FP") {
          self.info_stream.message(
            "FP",
            &format!(
              "thread state has {} bytes; docInRAM={}",
              next_ram,
              next.state.get_num_docs_in_ram()
            ),
          )?;
        }

        count += 1;
        if next_ram > max_ram_so_far {
          max_ram_so_far = next_ram;
          max_ram_using_writer = Some(Arc::clone(&next));
        }
      }
    }

    if self.info_stream.is_enabled("FP") {
      self.info_stream.message(
        "FP",
        &format!("{} in-use non-flushing threads states", count),
      )?;
    }

    Ok(max_ram_using_writer)
  }

  /// Returns the largest non-pending flushable DWPT or `None` if there is none.
  pub(crate) fn checkout_largest_non_pending_writer<L>(
    &self,
    config: &L,
  ) -> Result<Option<Arc<DwptWrapper<D>>>>
  where
    L: LiveIndexWriterConfig,
  {
    if let Some(largest_non_pending_writer) = self.find_largest_non_pending_writer()? {
      largest_non_pending_writer.lock();
      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let per_thread = largest_non_pending_writer.dwpt.lock();
        if self.per_thread_pool.is_registered(&per_thread.state.id) {
          let mut inner = self.inner.lock();
          let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mark_pending = !per_thread.is_flush_pending();
            self.checkout(&mut inner, &per_thread, mark_pending, config)
          }));
          self.update_stall_state(&mut inner, config)?;
          match result {
            Ok(result) => result,
            Err(payload) => std::panic::resume_unwind(payload),
          }
        } else {
          Ok(None)
        }
      }));
      largest_non_pending_writer.unlock();
      return match result {
        Ok(result) => result,
        Err(payload) => std::panic::resume_unwind(payload),
      };
    }
    Ok(None)
  }

  pub(crate) fn get_peak_active_bytes(&self) -> i64 {
    let inner = self.inner.lock();
    inner.peak_active_bytes
  }

  pub(crate) fn get_peak_net_bytes(&self) -> i64 {
    let inner = self.inner.lock();
    inner.peak_net_bytes
  }

  #[cfg(test)]
  pub(crate) fn was_stalled(&self) -> bool {
    self.stall_control.was_stalled()
  }

  #[cfg(test)]
  pub(crate) fn has_blocked(&self) -> bool {
    self.stall_control.has_blocked()
  }
}
impl<D> Drop for DocumentsWriterFlushControl<D>
where
  D: Directory,
{
  fn drop(&mut self) {
    self.close()
  }
}
impl<D> fmt::Display for DocumentsWriterFlushControl<D>
where
  D: Directory,
{
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let inner = self.inner.lock();
    let active = inner.active_bytes;
    let flush = inner.flush_bytes;
    write!(
      f,
      "{} [activeBytes={active}, flushBytes={flush}]",
      std::any::type_name::<Self>()
    )
  }
}
