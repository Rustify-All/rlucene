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
use crate::core::index::documents_writer_per_thread::DocumentsWriterPerThread;
use crate::core::index::documents_writer_per_thread_pool::{
    DocumentsWriterPerThreadPool, DwptWrapper,
};
use crate::core::index::documents_writer_stall_control::DocumentsWriterStallControl;
use crate::core::index::flush_policy::FlushPolicy;
use crate::core::index::index_writer::{IndexWriter, IndexWriterBase};
use crate::core::index::index_writer_config::DISABLE_AUTO_FLUSH;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::lockable_concurrent_approximate_priority_queue::Lock;
use crate::core::store::directory::Directory;
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::info_stream::{InfoStream, InfoStreamMT};
use parking_lot::{Condvar, Mutex, MutexGuard};
use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

pub(crate) struct DocumentsWriterFlushControl<D, L>
where
    D: Directory,
    L: LiveIndexWriterConfig,
{
    flush_deletes: AtomicBool,
    pub(crate) info_stream: InfoStreamMT,
    pub(crate) inner: Mutex<Inner<D>>,
    pub(crate) config: Arc<L>,
    stall_control: DocumentsWriterStallControl,
    pausing: Condvar,
    pub(crate) per_thread_pool: DocumentsWriterPerThreadPool<D>,
}
pub(crate) struct Inner<D>
where
    D: Directory,
{
    // only with assert
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

impl<D, L> DocumentsWriterFlushControl<D, L>
where
    D: Directory,
    L: LiveIndexWriterConfig,
{
    pub(crate) fn new(config: Arc<L>) -> Result<Self> {
        // Initialize the Inner state with defaults
        let inner = Inner {
            flush_by_ram_was_disabled: false,
            max_configured_ram_buffer: 0f64,
            hard_max_bytes_per_dwpt: (config.get_ram_per_thread_hard_limit_mb() * 1024 * 1024)
                as i64,
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
            config,
            stall_control: DocumentsWriterStallControl::new(),
            pausing: Condvar::new(),
            per_thread_pool: DocumentsWriterPerThreadPool::new()?,
        })
    }

    pub fn active_bytes(&self, inner: Option<&Inner<D>>) -> i64 {
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

    fn stall_limit_bytes(&self) -> i64 {
        let max_ram_mb = self.config.get_ram_buffer_size_mb();
        if max_ram_mb != DISABLE_AUTO_FLUSH as f64 {
            (2.0 * (max_ram_mb * 1024.0 * 1024.0)) as i64
        } else {
            i64::MAX
        }
    }
    fn assert_memory(&self, inner: &mut Inner<D>) -> bool {
        let max_ram_mb = self.config.get_ram_buffer_size_mb();
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
                    + self.num_flushing_dwpt() as i64
                    + self.num_blocked_flushes() as i64)
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
                    self.num_flushing_dwpt(),
                    self.num_blocked_flushes(),
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
    fn ram_buffer_granularity(&self) -> i64 {
        let mut ram_buffer_mb = self.config.get_ram_buffer_size_mb();
        if ram_buffer_mb == DISABLE_AUTO_FLUSH as f64 {
            ram_buffer_mb = self.config.get_ram_per_thread_hard_limit_mb() as f64;
        }
        // No more than ~0.1% of the RAM buffer size.
        let mut granularity = (ram_buffer_mb * 1024.0) as i64;
        // Or 16kB, so that with e.g. 64 active DWPTs, we'd never be missing more than 64*16kB = 1MB in
        // the global RAM buffer accounting.
        granularity = granularity.min(16 * 1024);
        granularity
    }
    pub(crate) fn do_after_document<FP>(
        &self,
        per_thread: &MutexGuard<'_, DocumentsWriterPerThread<D>>,
        flush_policy: &FP,
        delete_queue: &DocumentsWriterDeleteQueue,
    ) -> Result<Option<Arc<DwptWrapper<D>>>>
    where
        FP: FlushPolicy,
    {
        let delta = per_thread.get_commit_last_bytes_used_delta()?;
        // in order to prevent contention in the case of many threads indexing small documents
        // we skip ram accounting unless the DWPT accumulated enough ram to be worthwhile
        if self.config.get_max_buffered_docs() == DISABLE_AUTO_FLUSH
            && delta < self.ram_buffer_granularity()
        {
            // Skip accounting for now, we'll come back to it later when the delta is bigger
            return Ok(None);
        }
        let mut inner = self.inner.lock();
        let result = (|| {
            // we need to commit this under lock but calculate it outside of the lock to minimize the time
            // this lock is held
            // per document. The reason we update this under lock is that we mark DWPTs as pending without
            // acquiring it's
            // lock in #setFlushPending and this also reads the committed bytes and modifies the
            // flush/activeBytes.
            // In the future we can clean this up to be more intuitive.
            per_thread.commit_last_bytes_used(delta)?;
            // We need to differentiate here if we are pending since setFlushPending
            // moves the perThread memory to the flushBytes and we could be set to
            // pending during a delete
            if per_thread.is_flush_pending() {
                inner.flush_bytes += delta;
                self.update_peaks(delta, &mut inner);
            } else {
                inner.active_bytes += delta;
                self.update_peaks(delta, &mut inner);
                flush_policy.on_change(self, &mut inner, Some(per_thread), delete_queue)?;
                if !per_thread.is_flush_pending()
                    && per_thread.ram_bytes_used()? > inner.hard_max_bytes_per_dwpt
                {
                    // Safety check to prevent a single DWPT exceeding its RAM limit. This
                    // is super important since we can not address more than 2048 MB per DWPT
                    self.set_flush_pending(per_thread, Some(&mut inner))?;
                }
            }
            self.checkout(&mut inner, per_thread, false)
        })();

        let stall = self.update_stall_state(&mut inner);
        debug_assert!(
            self.assert_num_docs_since_stalled(stall, &mut inner) && self.assert_memory(&mut inner)
        );

        result
    }
    fn checkout(
        &self,
        inner: &mut Inner<D>, // Same to Java's Thread.holdsLock(this)
        per_thread: &MutexGuard<'_, DocumentsWriterPerThread<D>>,
        mark_pending: bool,
    ) -> Result<Option<Arc<DwptWrapper<D>>>> {
        if inner.full_flush {
            if per_thread.is_flush_pending() {
                self.checkout_and_block(per_thread, inner);
                return self.next_pending_flush(Some(inner));
            }
        } else {
            if mark_pending {
                debug_assert!(!per_thread.is_flush_pending());
                self.set_flush_pending(per_thread, Some(inner))?;
            }
            if per_thread.is_flush_pending() {
                return Ok(Some(self.check_out_for_flush(per_thread, inner)?));
            }
        }
        Ok(None)
    }
    fn assert_num_docs_since_stalled(&self, stalled: bool, inner: &mut Inner<D>) -> bool {
        //  updates the number of documents "finished" while we are in a stalled state.
        //  this is important for asserting memory upper bounds since it corresponds
        //  to the number of threads that are in-flight and crossed the stall control
        //  check before we actually stalled.
        //  see #assertMemory()
        if stalled {
            inner.num_docs_since_stalled += 1;
        } else {
            inner.num_docs_since_stalled = 0;
        }
        true
    }
    pub(crate) fn do_after_flush(&self, dwpt: Arc<DwptWrapper<D>>) {
        let mut inner = self.inner.lock();
        debug_assert!(inner.flushing_writers.contains(&dwpt));
        {
            if let Some(pos) = inner
                .flushing_writers
                .iter()
                .position(|w| w.state.id == dwpt.state.id)
            {
                inner.flushing_writers.remove(pos);
            }
            inner.flush_bytes += dwpt.state.get_last_committed_bytes_used();
            debug_assert!(self.assert_memory(&mut inner));
        };
        let _ = self.update_stall_state(&mut inner);
        self.pausing.notify_all();
    }
    fn update_stall_state(&self, inner: &mut Inner<D>) -> bool {
        let limit = self.stall_limit_bytes();
        let active = inner.active_bytes;
        let flush = inner.flush_bytes;
        let stall = (active + flush) > limit && active < limit && !inner.closed;

        if self.info_stream.enabled("DWFC") && stall != self.stall_control.any_stalled_threads() {
            if stall {
                self.info_stream.message(
                        "DW",
                        &format!(
                            "now stalling flushes: netBytes: {:.1} MB flushBytes: {:.1} MB fullFlush: {}",
                            (self.net_bytes(Some(inner)) as f64) / 1024.0 / 1024.0,
                            (self.get_flushing_bytes(Some(inner)) as f64) / 1024.0 / 1024.0,
                            inner.full_flush
                        ),
                    );
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
                    );
            }
        }

        self.stall_control.update_stalled(stall);
        stall
    }

    pub fn wait_for_flush(&self) {
        let mut inner = self.inner.lock();
        while !inner.flushing_writers.is_empty() {
            self.pausing.wait(&mut inner);
        }
    }
    /// Sets flush pending state on the given [`DocumentsWriterPerThread`].
    /// The [`DocumentsWriterPerThread`] must have indexed at least on Document and must not be already pending.
    pub fn set_flush_pending(
        &self,
        per_thread: &DocumentsWriterPerThread<D>,
        inner: Option<&mut Inner<D>>,
    ) -> Result<()> {
        let inner = match inner {
            Some(inner) => inner,
            None => &mut *self.inner.lock(),
        };
        debug_assert!(!per_thread.is_flush_pending());
        if per_thread.get_num_docs_in_ram() > 0 {
            per_thread.set_flush_pending()?;
            let bytes = per_thread.get_last_committed_bytes_used();
            inner.flush_bytes += bytes;
            inner.active_bytes -= bytes;
            inner.num_pending += 1;
            assert!(self.assert_memory(inner));
        }
        Ok(())
    }
    pub fn do_on_abort(&self, per_thread: &Arc<DwptWrapper<D>>) {
        let mut inner = self.inner.lock();
        let dwpt = per_thread.dwpt.lock();
        debug_assert!(self.per_thread_pool.is_registered(per_thread.id()));
        let bytes = dwpt.get_last_committed_bytes_used();
        if dwpt.is_flush_pending() {
            inner.flush_bytes -= bytes;
        } else {
            inner.active_bytes -= bytes;
        };
        debug_assert!(self.assert_memory(&mut inner));
        // Take it out of the loop this DWPT is stale

        let _ = self.update_stall_state(&mut inner);
        let checked_out = self.per_thread_pool.checkout(&dwpt);
        debug_assert!(checked_out.is_some());
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
    fn check_out_for_flush(
        &self,
        per_thread: &MutexGuard<'_, DocumentsWriterPerThread<D>>,
        inner: &mut Inner<D>, // Same to Java's Thread.holdsLock(this)
    ) -> Result<Arc<DwptWrapper<D>>> {
        debug_assert!(per_thread.is_flush_pending());
        debug_assert!(per_thread.state.is_locked());
        debug_assert!(self.per_thread_pool.is_registered(&per_thread.state.id));
        let result = {
            inner.num_pending -= 1;
            match self.per_thread_pool.checkout(per_thread) {
                Some(v) => {
                    self.add_flushing_dwpt(v.clone(), inner);
                    v
                },
                None => return Err(LuceneError::illegal_state("DWPT not registered in pool")),
            }
        };
        self.update_stall_state(inner);
        Ok(result)
    }
    fn add_flushing_dwpt(&self, per_thread: Arc<DwptWrapper<D>>, inner: &mut Inner<D>) {
        debug_assert!(
            !inner.flushing_writers.contains(&per_thread),
            "DWPT is already flushing"
        );
        // Record the flushing DWPT to reduce flushBytes in doAfterFlush
        inner.flushing_writers.push(per_thread);
    }
    pub(crate) fn next_pending_flush(
        &self,
        inner: Option<&mut Inner<D>>,
    ) -> Result<Option<Arc<DwptWrapper<D>>>> {
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
                self.update_stall_state(inner);
                return Ok(Some(dwpt));
            }
            full_flush = inner.full_flush;
            num_pending = inner.num_pending;
        }
        if num_pending > 0 && !full_flush {
            let dwpts = self.per_thread_pool.iterator(None);
            for (id, next) in &dwpts {
                if next.state.is_flush_pending() && next.try_lock() {
                    let result = (|| {
                        if self.per_thread_pool.is_registered(id) {
                            let mut inner = self.inner.lock();
                            return Ok(Some(
                                self.check_out_for_flush(&next.dwpt.lock(), &mut inner)?,
                            ));
                        } else {
                            Ok(None)
                        }
                    })();
                    next.unlock();
                    return result;
                }
            }
        }
        Ok(None)
    }

    pub fn close(&self) {
        let mut inner = self.inner.lock();
        inner.closed = true;
    }

    pub(crate) fn do_on_delete<FP>(
        &self,
        flush_policy: &FP,
        delete_queue: &DocumentsWriterDeleteQueue,
    ) -> Result<()>
    where
        FP: FlushPolicy,
    {
        let mut inner = self.inner.lock();
        flush_policy.on_change(self, &mut inner, None, delete_queue)?;
        Ok(())
    }

    /// Returns heap bytes currently consumed by buffered deletes/updates that would be freed if we pushed all deletes.
    /// This does not include bytes consumed by already pushed delete/update packets.
    pub(crate) fn get_delete_bytes_used(
        &self,
        delete_queue: &DocumentsWriterDeleteQueue,
    ) -> Result<i64> {
        delete_queue.ram_bytes_used()
    }

    pub(crate) fn num_flushing_dwpt(&self) -> usize {
        let inner = self.inner.lock();
        inner.flushing_writers.len()
    }
    pub fn get_and_reset_apply_all_deletes(&self) -> bool {
        self.flush_deletes.swap(false, Ordering::SeqCst)
    }
    /// Check whether deletes need to be applied. This can be used as a pre-flight check before calling
    /// [`getAndResetApplyAllDeletes()`](Self::get_and_reset_apply_all_deletes) to make sure that a single thread applies deletes.
    pub fn get_apply_all_deletes(&self) -> bool {
        self.flush_deletes.load(Ordering::SeqCst)
    }

    pub fn set_apply_all_deletes(&self) {
        self.flush_deletes.store(true, Ordering::SeqCst);
    }

    pub fn obtain_and_lock<B>(
        &self,
        delete_queue: &Arc<DocumentsWriterDeleteQueue>,
        writer: &IndexWriter<D, L, B>,
    ) -> Result<Arc<DwptWrapper<D>>>
    where
        B: IndexWriterBase,
    {
        loop {
            {
                let inner = self.inner.lock();
                if inner.closed {
                    return Err(LuceneError::already_closed("flush control is closed"));
                }
            }

            let per_thread = self
                .per_thread_pool
                .get_and_lock(writer, delete_queue.clone())?;
            if Arc::ptr_eq(&per_thread.state.delete_queue, delete_queue) {
                // simply return the DWPT even in a flush all case since we already hold the lock and the
                // DWPT is not stale
                // since it has the current delete queue associated with it. This means we have established
                // a happens-before
                // relationship and all docs indexed into this DWPT are guaranteed to not be flushed with
                // the currently
                // progress full flush.
                return Ok(per_thread);
            } else {
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
                per_thread.unlock()
            }
        }
    }
    pub(crate) fn mark_for_full_flush<FN>(
        &self,
        documents_writer: &DocumentsWriter<D, L, FN>,
    ) -> Result<i64>
    where
        FN: FlushNotifications,
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
            let mut documents_writer_inner = documents_writer.inner.lock();
            flushing_queue = documents_writer_inner.delete_queue.clone();
            // Set a new delete queue - all subsequent DWPT will use this queue until
            // we do another full flush
            self.per_thread_pool.lock_new_writers();
            // no new thread-states while we do a flush otherwise the seqNo
            // accounting might be off

            let size = self.per_thread_pool.size();
            debug_assert!(size <= i64::MAX as usize);
            // Insert a gap in seqNo of current active thread count, in the worst case each of those
            // threads now have one operation in flight.  It's fine
            // if we have some sequence numbers that were never assigned:
            let result =
                documents_writer.reset_delete_queue(&mut documents_writer_inner, size as i64);
            self.per_thread_pool.unlock_new_writers();
            result
        }?;

        let mut full_flush_buffer = Vec::new();
        let dwpts = {
            self.per_thread_pool
                .filter_and_lock(|v| Arc::ptr_eq(&v.state.delete_queue, &flushing_queue))?
        };

        for dwpt in dwpts {
            let result: Result<()> = (|| {
                let next = dwpt.dwpt.lock();
                if next.get_num_docs_in_ram() > 0 {
                    let flushing_dwpt = {
                        let mut inner = self.inner.lock();
                        if !next.is_flush_pending() {
                            self.set_flush_pending(&next, Some(&mut inner))?;
                        }
                        self.check_out_for_flush(&next, &mut inner)?
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
            })();
            dwpt.unlock();
            result?;
        }

        {
            // make sure we move all DWPT that are where concurrently marked as
            // pending and moved to blocked are moved over to the flushQueue. There is
            // a chance that this happens since we marking DWPT for full flush without
            // blocking indexing
            let mut inner = self.inner.lock();
            self.prune_blocked_queue(&flushing_queue, &mut inner);
            debug_assert!(self.assert_blocked_flushes(&documents_writer.inner.lock().delete_queue));
            inner.flush_queue.extend(full_flush_buffer);
            self.update_stall_state(&mut inner);
            inner.full_flush_mark_done = true;
        }

        debug_assert!(self.assert_active_delete_queue(&documents_writer.inner.lock().delete_queue));
        debug_assert!(flushing_queue.get_last_sequence_number() <= flushing_queue.get_max_seq_no());

        Ok(seq_no)
    }
    pub fn assert_active_delete_queue(&self, queue: &Arc<DocumentsWriterDeleteQueue>) -> bool {
        let dwpts = self.per_thread_pool.iterator(None);
        for (_, next) in dwpts.iter() {
            debug_assert!(Arc::ptr_eq(&next.state.delete_queue, queue));
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
            let dwpt = inner
                .blocked_flushes
                .remove(i)
                .expect("should never fail to remove blocked DWPT under lock");
            self.add_flushing_dwpt(dwpt.clone(), inner);
            inner.flush_queue.push_back(dwpt);
        }
    }
    pub(crate) fn finish_full_flush(&self, delete_queue: &Arc<DocumentsWriterDeleteQueue>) {
        let mut inner = self.inner.lock();
        debug_assert!(inner.full_flush);
        debug_assert!(inner.flush_queue.is_empty());
        debug_assert!(
            inner.flushing_writers.is_empty(),
            "flushing_writers must be empty"
        );

        if !inner.blocked_flushes.is_empty() {
            debug_assert!(self.assert_blocked_flushes(delete_queue),);
            self.prune_blocked_queue(delete_queue, &mut inner);
            debug_assert!(
                inner.blocked_flushes.is_empty(),
                "blocked_flushes must be empty after pruning"
            );
        }

        inner.full_flush_mark_done = false;
        inner.full_flush = false;
        let _ = self.update_stall_state(&mut inner);
    }
    pub(crate) fn assert_blocked_flushes(
        &self,
        flushing_queue: &Arc<DocumentsWriterDeleteQueue>,
    ) -> bool {
        let inner = self.inner.lock();
        for blocked in inner.blocked_flushes.iter() {
            debug_assert!(Arc::ptr_eq(&blocked.state.delete_queue, flushing_queue));
        }
        true
    }

    /// Returns `true` if a full flush is currently running
    pub fn is_full_flush(&self) -> bool {
        let inner = self.inner.lock();
        inner.full_flush
    }

    /// Returns the number of flushes that are already checked out but not yet actively flushing
    pub fn num_queued_flushes(&self) -> usize {
        let inner = self.inner.lock();
        inner.flush_queue.len()
    }

    /// Returns the number of flushes that are checked out but not yet available for flushing.
    /// This only applies during a full flush if a DWPT needs flushing but must not be flushed
    /// until the full flush has finished.
    pub fn num_blocked_flushes(&self) -> i32 {
        let inner = self.inner.lock();
        inner.blocked_flushes.len() as i32
    }

    /// This method will block if too many DWPT are currently flushing and no checked out DWPT are available
    pub fn wait_if_stalled(&self) {
        self.stall_control.wait_if_stalled();
    }

    /// Returns `true` iff stalled.
    pub fn any_stalled_threads(&self) -> bool {
        self.stall_control.any_stalled_threads()
    }
    pub(crate) fn find_largest_non_pending_writer(&self) -> Option<Arc<DwptWrapper<D>>> {
        let mut max_ram_using_writer: Option<Arc<DwptWrapper<D>>> = None;
        // Note: should be initialized to -1 since some DWPTs might return 0 if their RAM usage has not
        // been committed yet.
        let mut max_ram_so_far: i64 = -1;
        let mut count = 0;

        for (_id, next) in self.per_thread_pool.iterator(None) {
            if !next.state.is_flush_pending() && next.state.get_num_docs_in_ram() > 0 {
                let next_ram = next.state.get_last_committed_bytes_used();

                if self.info_stream.enabled("FP") {
                    self.info_stream.message(
                        "FP",
                        &format!(
                            "thread state has {} bytes; docInRAM={}",
                            next_ram,
                            next.state.get_num_docs_in_ram()
                        ),
                    );
                }

                count += 1;
                if next_ram > max_ram_so_far {
                    max_ram_so_far = next_ram;
                    max_ram_using_writer = Some(Arc::clone(&next));
                }
            }
        }

        if self.info_stream.enabled("FP") {
            self.info_stream.message(
                "FP",
                &format!("{} in-use non-flushing threads states", count),
            );
        }

        max_ram_using_writer
    }

    pub(crate) fn get_peak_active_bytes(&self) -> i64 {
        let inner = self.inner.lock();
        inner.peak_active_bytes
    }

    pub(crate) fn get_peak_net_bytes(&self) -> i64 {
        let inner = self.inner.lock();
        inner.peak_net_bytes
    }
}
pub(crate) type EitherDWPT<D> = (
    Option<DocumentsWriterPerThread<D>>,
    Option<DocumentsWriterPerThread<D>>,
);
impl<D, L> Drop for DocumentsWriterFlushControl<D, L>
where
    D: Directory,
    L: LiveIndexWriterConfig,
{
    fn drop(&mut self) {
        self.close()
    }
}
impl<D, L> fmt::Display for DocumentsWriterFlushControl<D, L>
where
    D: Directory,
    L: LiveIndexWriterConfig,
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
