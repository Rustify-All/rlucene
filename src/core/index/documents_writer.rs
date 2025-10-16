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
use crate::core::document::fields::Fields;
use crate::core::index::doc_values_update::DocValuesUpdate;
use crate::core::index::documents_writer_delete_queue::{DocumentsWriterDeleteQueue, Node};
use crate::core::index::documents_writer_flush_control::DocumentsWriterFlushControl;
use crate::core::index::documents_writer_flush_queue::{DocumentsWriterFlushQueue, FlushTicket};
use crate::core::index::documents_writer_per_thread::DocumentsWriterPerThread;
use crate::core::index::documents_writer_per_thread_pool::DwptWrapper;
use crate::core::index::field_infos::FieldNumbers;
use crate::core::index::field_infos::build::Builder;
use crate::core::index::index_writer::{IndexWriter, IndexWriterBase};
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::lockable_concurrent_approximate_priority_queue::Lock;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::term::Term;
use crate::core::search::query::Query;
use crate::core::store::directory::Directory;
use crate::core::store::lock_validating_directory_wrapper::LockValidatingDirectoryWrapper;
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::info_stream::{InfoStream, InfoStreamMT};
use crate::core::util::supplier::Supplier;
use parking_lot::Mutex;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, Ordering};
use std::thread;

/// This struct accepts multiple added documents and directly writes segment files.
///
/// Each added document is passed to the indexing chain, which processes the document into
/// the different codec formats. Some formats write bytes to files immediately (e.g. stored fields
/// and term vectors), while others are buffered by the indexing chain and written only on flush.
///
/// Once we have used our allowed RAM buffer, or the number of added docs is large enough (in the
/// case we are flushing by doc count instead of RAM usage), we create a real segment and flush it to
/// the `Directory`.
///
/// Threads:
///
/// Multiple threads are allowed into `add_document` at once. There is an initial synchronized call
/// to [`DocumentsWriterFlushControl::obtain_and_lock()`] which allocates a DWPT for this indexing
/// thread. The same thread will not necessarily get the same DWPT over time. Then `update_documents` is
/// called on that DWPT without synchronization (most of the “heavy lifting” is in this call). Once a
/// DWPT fills up enough RAM or holds enough documents in memory, the DWPT is checked out for flush and
/// all changes are written to the directory. Each DWPT corresponds to one segment being written.
///
/// When `flush` is called by `IndexWriter`, we check out all DWPTs associated with the
/// current [`DocumentsWriterDeleteQueue`] out of the [`DocumentsWriterPerThreadPool`] and
/// write them to disk. The flush process can piggyback on incoming indexing threads or even block
/// them from adding documents if flushing can’t keep up with new documents being added. Unless the
/// stall control kicks in to block indexing threads, flushes happen concurrently with indexing requests.
///
/// Exceptions:
///
/// Because this struct directly updates in-memory posting lists, and flushes stored fields and
/// term vectors directly to files in the directory, there are limited times when an
/// exception can corrupt this state. For example, a disk full while flushing stored fields can leave
/// the file in a corrupt state. Or an OOM exception while appending to the in-memory posting lists
/// can corrupt that posting list. We call such errors “aborting exceptions.” In these cases we
/// must call `abort()` to discard all docs added since the last flush.
///
/// All other exceptions (“non-aborting exceptions”) can still partially update the index
/// structures. These updates are consistent but represent only a part of the document seen up
/// until the exception was hit. When this happens, we immediately mark the document as deleted so
/// that the document is always atomically (“all or none”) added to the index.
pub(crate) struct DocumentsWriter<D, L, FN>
where
    D: Directory,
    L: LiveIndexWriterConfig,
    FN: FlushNotifications,
{
    closed: AtomicBool,
    info_stream: InfoStreamMT,
    config: Arc<L>,
    num_docs_in_ram: AtomicI32,
    ticket_queue: DocumentsWriterFlushQueue<D>,
    // we preserve changes during a full flush since IW might not check out before
    // we release all changes. NRT Readers otherwise suddenly return true from
    // isCurrent while there are actually changes currently committed. See also
    // #anyChanges() & #flushAllThreads
    pending_changes_in_current_full_flush: AtomicBool,
    pub(crate) inner: Mutex<Inner>,
    flush_control: DocumentsWriterFlushControl<D, L>,
    index_created_version_major: i32,
    directory: Arc<LockValidatingDirectoryWrapper<D>>,
    directory_orig: Arc<D>,
    enable_test_points: bool,
    global_field_number_map: Arc<Mutex<FieldNumbers>>,
    flush_notifications: FN,
}
pub(crate) struct Inner {
    pub(crate) delete_queue: Arc<DocumentsWriterDeleteQueue>,
    current_full_flush_del_queue: Option<Arc<DocumentsWriterDeleteQueue>>,
}
impl<D, L, FN> DocumentsWriter<D, L, FN>
where
    D: Directory,
    L: LiveIndexWriterConfig,
    FN: FlushNotifications,
{
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        flush_notifications: FN,
        index_created_version_major: i32,
        enable_test_points: bool,
        config: Arc<L>,
        directory_orig: Arc<D>,
        directory: Arc<LockValidatingDirectoryWrapper<D>>,
        global_field_number_map: Arc<Mutex<FieldNumbers>>,
    ) -> Result<Self> {
        let info_stream = config.get_info_stream();
        let delete_queue = Arc::new(DocumentsWriterDeleteQueue::new(info_stream.clone()));
        Ok(DocumentsWriter {
            closed: AtomicBool::new(false),
            info_stream,
            config: config.clone(),
            num_docs_in_ram: AtomicI32::new(0),
            ticket_queue: DocumentsWriterFlushQueue::new(),
            pending_changes_in_current_full_flush: AtomicBool::new(false),
            inner: Mutex::new(Inner {
                delete_queue: delete_queue.clone(),
                current_full_flush_del_queue: None,
            }),
            flush_control: DocumentsWriterFlushControl::new(config)?,
            index_created_version_major,
            directory,
            directory_orig,
            enable_test_points,
            global_field_number_map,
            flush_notifications,
        })
    }
    pub(crate) fn delete_queries(&self, queries: Vec<Query>) -> Result<i64> {
        self.apply_delete_or_update(|upd| {
            upd.add_delete_query(queries.into_iter().map(Arc::new).collect())
        })
    }

    pub(crate) fn delete_terms(&self, terms: Vec<Term>) -> Result<i64> {
        self.apply_delete_or_update(|upd| upd.add_delete_term(terms))
    }

    pub(crate) fn update_doc_values(&self, updates: Vec<DocValuesUpdate>) -> Result<i64> {
        self.apply_delete_or_update(|upd| upd.add_doc_values_updates(updates))
    }
    fn apply_delete_or_update<F>(&self, func: F) -> Result<i64>
    where
        F: FnOnce(&DocumentsWriterDeleteQueue) -> Result<i64>,
    {
        // Check the applyAllDeletes flag first. This helps exit early most of the time without checking
        // isFullFlush(), which takes a lock and introduces contention on small documents that are quick
        // to index.
        let inner = self.inner.lock();
        let mut seq_no = func(&inner.delete_queue)?;
        self.flush_control
            .do_on_delete(self.config.get_flush_policy(), inner.delete_queue.as_ref())?;
        if self.apply_all_deletes(Some(&inner))? {
            seq_no = -seq_no;
        }
        Ok(seq_no)
    }
    /// If buffered deletes are using too much heap, resolve them and write disk and return true.
    fn apply_all_deletes(&self, inner: Option<&Inner>) -> Result<bool> {
        // Check the applyAllDeletes flag first. This helps exit early most of the time without checking
        // isFullFlush(), which takes a lock and introduces contention on small documents that are quick
        // to index.
        let delete_queue = {
            match inner {
                Some(inner) => inner.delete_queue.clone(),
                None => self.inner.lock().delete_queue.clone(),
            }
        };
        if self.flush_control.get_apply_all_deletes()
            && !self.flush_control.is_full_flush()
            // never apply deletes during full flush this breaks happens before relationship.
            && delete_queue.is_open()
            // if it's closed then it's already fully applied and we have a new delete queue
            && self.flush_control.get_and_reset_apply_all_deletes()
        {
            let supplier = SupplierImpl::new(delete_queue);
            if self.ticket_queue.add_ticket(supplier)?.is_some() {
                self.flush_notifications.on_deletes_applied()?; // apply deletes event forces a purge
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub(crate) fn purge_flush_tickets<F, B>(
        &self,
        forced: bool,
        writer: &IndexWriter<D, L, B>,
        consumer: F,
    ) -> Result<()>
    where
        F: Fn(FlushTicket<D>, &IndexWriter<D, L, B>) -> Result<()>,
        B: IndexWriterBase,
    {
        if forced {
            self.ticket_queue.force_purge(writer, consumer)
        } else {
            self.ticket_queue.try_purge(writer, consumer)
        }
    }
    /// Returns how many docs are currently buffered in RAM.
    pub(crate) fn get_num_docs(&self) -> i32 {
        self.num_docs_in_ram.load(Ordering::SeqCst)
    }

    fn ensure_open(&self) -> Result<()> {
        if self.closed.load(Ordering::SeqCst) {
            Err(LuceneError::already_closed(
                "this DocumentsWriter is closed",
            ))
        } else {
            Ok(())
        }
    }
    pub(crate) fn flush_one_dwpt<B>(&self, writer: &IndexWriter<D, L, B>) -> Result<bool>
    where
        B: IndexWriterBase,
    {
        {
            if self.info_stream.enabled("DW") {
                self.info_stream.message("DW", "startFlushOneDWPT");
            }
        }

        if !self.maybe_flush(writer)? {
            // if let Some(dwpt) = self.flush_control.checkout_largest_non_pending_writer() {
            //     self.do_flush(dwpt)?;
            //     return Ok(true);
            // }
            // return Ok(false);
            unimplemented!()
        }
        Ok(true)
    }

    /// Locks all currently active DWPT and aborts them.
    /// The returned Closeable should be closed once the locks for the aborted DWPTs can be released.
    fn lock_and_abort_all(&self) {
        // TODO
    }
    /// Returns how many documents were aborted.
    fn abort_documents_writer_per_thread(&self, per_thread: Arc<DwptWrapper<D>>) -> Result<()> {
        debug_assert!(self.inner.is_locked());
        {
            let num = per_thread.state.get_num_docs_in_ram();
            self.subtract_flushed_num_docs(num);
            per_thread.dwpt.lock().abort()?;
        }
        self.flush_control.do_on_abort(&per_thread);

        Ok(())
    }
    /// returns the maximum sequence number for all previously completed operations
    pub(crate) fn get_max_completed_sequence_number(&self) -> i64 {
        let inner = self.inner.lock();
        inner.delete_queue.get_max_completed_seq_no()
    }
    fn any_changes(&self, inner: Option<&Inner>) -> bool {
        // changes are either in a DWPT or in the deleteQueue.
        // yet if we currently flush deletes and / or dwpt there
        // could be a window where all changes are in the ticket queue
        // before they are published to the IW. ie we need to check if the
        // ticket queue has any tickets.
        let num_docs = self.num_docs_in_ram.load(Ordering::SeqCst) != 0;
        let deletions = self.any_deletions(inner);
        let tickets = self.ticket_queue.has_tickets();
        let pending_full = self
            .pending_changes_in_current_full_flush
            .load(Ordering::SeqCst);

        let any = num_docs || deletions || tickets || pending_full;

        if self.info_stream.enabled("DW") && any {
            self.info_stream.message(
                "DW",
                &format!(
                    "anyChanges? numDocsInRam={num_docs} deletes={deletions} hasTickets={tickets} pendingChangesInFullFlush={pending_full}"
                ),
            );
        }

        any
    }
    pub(crate) fn get_buffered_delete_terms_size(&self, inner: Option<&Inner>) -> Result<i32> {
        let inner = match inner {
            Some(inner) => inner,
            None => &self.inner.lock(),
        };
        let delete_queue = inner.delete_queue.clone();
        delete_queue.get_buffered_updates_terms_size()
    }
    pub(crate) fn any_deletions(&self, inner: Option<&Inner>) -> bool {
        let inner = match inner {
            Some(inner) => inner,
            None => &self.inner.lock(),
        };
        let delete_queue = inner.delete_queue.clone();
        delete_queue.any_changes(None)
    }
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        // TODO
    }
    fn pre_update<B>(&self, writer: &IndexWriter<D, L, B>) -> Result<bool>
    where
        B: IndexWriterBase,
    {
        self.ensure_open()?;
        let mut has_events = false;

        while self.flush_control.any_stalled_threads()
            || (self.config.get_check_pending_flush_on_update()
                && self.flush_control.num_queued_flushes() > 0)
        {
            // Help out flushing any queued DWPTs so we can un-stall:
            // Try pickup pending threads here if possible
            // no need to loop over the next pending flushes... doFlush will take care of this
            has_events |= self.maybe_flush(writer)?;
            self.flush_control.wait_if_stalled();
        }

        Ok(has_events)
    }

    fn post_update<B>(
        &self,
        flushing_dwpt: Option<Arc<DwptWrapper<D>>>,
        mut has_events: bool,
        writer: &IndexWriter<D, L, B>,
    ) -> Result<bool>
    where
        B: IndexWriterBase,
    {
        has_events |= self.apply_all_deletes(None)?;
        if let Some(dwpt) = flushing_dwpt {
            self.do_flush(dwpt, writer)?;
            has_events = true;
        } else if self.config.get_check_pending_flush_on_update() {
            has_events |= self.maybe_flush(writer)?;
        }
        Ok(has_events)
    }
    pub(crate) fn update_documents<DI, DF, B>(
        &self,
        docs: DI,
        del_node: Option<Arc<Node>>,
        writer: &IndexWriter<D, L, B>,
    ) -> Result<i64>
    where
        DI: IntoIterator<Item = DF>,
        DF: IntoIterator<Item = Fields>,
        B: IndexWriterBase,
    {
        let has_events = self.pre_update(writer)?;

        let dwpt_wrapper = self
            .flush_control
            .obtain_and_lock(&self.inner.lock().delete_queue, writer)?;
        let mut flushing_dwpt_opt = None;
        let mut seq_no = 0;
        let result = (|| {
            // This must happen after we've pulled the DWPT because IW.close
            // waits for all DWPT to be released:
            self.ensure_open()?;
            let result: Result<()> = {
                seq_no = dwpt_wrapper.dwpt.lock().update_documents(
                    docs,
                    del_node,
                    &self.flush_notifications,
                    self.config.as_ref(),
                    &self.num_docs_in_ram,
                    writer,
                )?;
                Ok(())
            };
            if dwpt_wrapper.state.is_aborted() {
                self.flush_control.do_on_abort(&dwpt_wrapper);
            }
            if result.is_ok() {
                flushing_dwpt_opt = self.flush_control.do_after_document(
                    &dwpt_wrapper.dwpt.lock(),
                    self.config.get_flush_policy(),
                    self.inner.lock().delete_queue.as_ref(),
                )?
            }
            {
                // If a flush is occurring, we don't want to allow this dwpt to be reused
                // If it is aborted, we shouldn't allow it to be reused
                // If the deleteQueue is advanced, this means the maximum seqNo has been set and it cannot be
                // reused
                let inner = self.flush_control.inner.lock();
                if dwpt_wrapper.state.is_flush_pending()
                    || dwpt_wrapper.state.is_aborted()
                    || dwpt_wrapper.state.delete_queue.is_advanced()
                {
                    dwpt_wrapper.state.unlock();
                } else {
                    self.flush_control
                        .per_thread_pool
                        .mark_as_free_and_unlock(dwpt_wrapper)?;
                }
                drop(inner)
            }
            result
        })();
        result?;
        if self.post_update(flushing_dwpt_opt, has_events, writer)? {
            seq_no = -seq_no;
        }
        Ok(seq_no)
    }

    fn maybe_flush<B>(&self, writer: &IndexWriter<D, L, B>) -> Result<bool>
    where
        B: IndexWriterBase,
    {
        let flushing_dwpt = self.flush_control.next_pending_flush(None)?;

        if let Some(flushing_dwpt) = flushing_dwpt {
            self.do_flush(flushing_dwpt, writer)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
    fn do_flush<B>(
        &self,
        mut flushing_dwpt: Arc<DwptWrapper<D>>,
        writer: &IndexWriter<D, L, B>,
    ) -> Result<()>
    where
        B: IndexWriterBase,
    {
        loop {
            assert!(!flushing_dwpt.state.has_flushed());

            let res: Result<_> = (|| {
                debug_assert!({
                    let current_full_flush_del_queue =
                        self.inner.lock().current_full_flush_del_queue.clone();
                    current_full_flush_del_queue.is_none()
                        || Arc::ptr_eq(
                            &flushing_dwpt.state.delete_queue,
                            current_full_flush_del_queue.as_ref().unwrap(),
                        )
                });

                // Since with DWPT the flush process is concurrent and several DWPT
                // could flush at the same time we must maintain the order of the
                // flushes before we can apply the flushed segment and the frozen global
                // deletes it is buffering. The reason for this is that the global
                // deletes mark a certain point in time where we took a DWPT out of
                // rotation and freeze the global deletes.
                //
                // Example: A flush 'A' starts and freezes the global deletes, then
                // flush 'B' starts and freezes all deletes occurred since 'A' has
                // started. if 'B' finishes before 'A' we need to wait until 'A' is done
                // otherwise the deletes frozen by 'B' are not applied to 'A' and we
                // might miss to deletes documents in 'A'.
                let mut has_ticket = None;
                let result = (|| {
                    debug_assert!(
                        self.assert_ticket_queue_modification(&flushing_dwpt.state.delete_queue)
                    );
                    let ticket = {
                        let mut dwpt = flushing_dwpt.dwpt.lock();
                        let supplier = SupplierImpl1::new(&mut *dwpt);
                        self.ticket_queue.add_ticket(supplier)?
                    };
                    match ticket {
                        Some(ticket) => {
                            has_ticket = Some(ticket);
                            let flushing_docs_in_ram = flushing_dwpt.state.get_num_docs_in_ram();
                            {
                                let mut dwpt = flushing_dwpt.dwpt.lock();
                                let result = (|| {
                                    let v = dwpt.flush(
                                        &self.flush_notifications,
                                        self.config.as_ref(),
                                        writer,
                                    )?;
                                    match v {
                                        Some(new_segment) => {
                                            self.ticket_queue.add_segment(ticket, new_segment);
                                            Ok(())
                                        },
                                        None => Err(LuceneError::illegal_state(
                                            "flush_segment returned None",
                                        )),
                                    }
                                })();
                                self.subtract_flushed_num_docs(flushing_docs_in_ram);
                                if !dwpt.pending_files_to_delete().is_empty() {
                                    let files = dwpt.pending_files_to_delete().clone();
                                    self.flush_notifications.delete_unused_files(files)?;
                                }
                                if result.is_err() {
                                    let dir = dwpt.segment_info.dir.clone();
                                    self.flush_notifications.flush_failed(std::mem::replace(
                                        &mut dwpt.segment_info,
                                        SegmentInfo::dummy(dir),
                                    ))?
                                }
                                result
                            }
                        },
                        None => Err(LuceneError::illegal_state("ticket returned None")),
                    }
                })();
                if result.is_err()
                    && let Some(ticket_idx) = has_ticket
                {
                    // In the case of a failure make sure we are making progress and
                    // apply all the deletes since the segment flush failed since the flush
                    // ticket could hold global deletes see FlushTicket#canPublish()
                    self.ticket_queue.mark_ticket_failed(ticket_idx);
                }
                result?;
                //Now we are done and try to flush the ticket queue if the head of the
                // queue has already finished the flush.
                if self.ticket_queue.get_ticket_count() as usize
                    >= self.flush_control.per_thread_pool.size()
                {
                    // This means there is a backlog: the one
                    // thread in innerPurge can't keep up with all
                    // other threads flushing segments.  In this case
                    // we forcefully stall the producers.
                    self.flush_notifications.on_ticket_backlog()?;
                }
                Ok(())
            })();
            self.flush_control.do_after_flush(flushing_dwpt);
            res?;
            let v = self.flush_control.next_pending_flush(None)?;

            match v {
                Some(next_dwpt) => {
                    flushing_dwpt = next_dwpt;
                    continue;
                },
                None => break,
            }
        }
        self.flush_notifications.after_segments_flushed(writer)?;
        Ok(())
    }
    pub(crate) fn get_next_sequence_number(&self) -> i64 {
        let delete_queue = self.inner.lock().delete_queue.clone();
        delete_queue.get_next_sequence_number()
    }

    pub(crate) fn reset_delete_queue(
        &self,
        inner: &mut Inner,
        max_num_pending_ops: i64,
    ) -> Result<i64> {
        let new_queue = inner.delete_queue.advance_queue(max_num_pending_ops)?;
        debug_assert!(inner.delete_queue.is_advanced());
        debug_assert!(!new_queue.is_advanced());
        debug_assert!(
            inner.delete_queue.get_last_sequence_number() <= new_queue.get_last_sequence_number()
        );
        debug_assert!(
            inner.delete_queue.get_max_seq_no() <= new_queue.get_last_sequence_number(),
            "max_seq_no: {} vs. {}",
            inner.delete_queue.get_max_seq_no(),
            new_queue.get_last_sequence_number()
        );
        let old_max_seq_no = inner.delete_queue.get_max_seq_no();
        inner.delete_queue = Arc::new(new_queue);
        Ok(old_max_seq_no)
    }

    pub(crate) fn subtract_flushed_num_docs(&self, num_flushed: i32) {
        let mut old_value = self.num_docs_in_ram.load(Ordering::SeqCst);
        loop {
            let new_value = old_value - num_flushed;
            if self
                .num_docs_in_ram
                .compare_exchange(old_value, new_value, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                break;
            }
            old_value = self.num_docs_in_ram.load(Ordering::SeqCst);
        }
        debug_assert!(self.num_docs_in_ram.load(Ordering::SeqCst) >= 0);
    }

    fn set_flushing_delete_queue(
        &self,
        session: Option<Arc<DocumentsWriterDeleteQueue>>,
        inner: Option<&mut Inner>,
    ) -> bool {
        let inner = match inner {
            Some(inner) => inner,
            None => &mut *self.inner.lock(),
        };
        debug_assert!(
            inner
                .current_full_flush_del_queue
                .as_ref()
                .is_none_or(|q| !q.is_open()),
            "Can not replace a full flush queue if the queue is not closed"
        );
        inner.current_full_flush_del_queue = session;
        true
    }
    fn assert_ticket_queue_modification(
        &self,
        delete_queue: &Arc<DocumentsWriterDeleteQueue>,
    ) -> bool {
        let inner = self.inner.lock();
        debug_assert!(
            inner
                .current_full_flush_del_queue
                .as_ref()
                .is_none_or(|q| Arc::ptr_eq(q, delete_queue)),
            "only modifications from the current flushing queue are permitted while doing a full flush"
        );
        true
    }

    // FlushAllThreads is synced by IW fullFlushLock. Flushing all threads is a
    // two stage operation; the caller must ensure (in try/finally) that finishFlush
    // is called after this method, to release the flush lock in DWFlushControl
    pub(crate) fn flush_all_threads<B>(&self, writer: &IndexWriter<D, L, B>) -> Result<i64>
    where
        B: IndexWriterBase,
    {
        if self.info_stream.enabled("DW") {
            self.info_stream.message("DW", "startFullFlush");
        }
        let (flushing_delete_queue, seq_no) = {
            let mut inner = self.inner.lock();
            let pending = self.any_changes(Some(&inner));
            self.pending_changes_in_current_full_flush
                .store(pending, Ordering::SeqCst);
            let fq = inner.delete_queue.clone();
            // Cutover to a new delete queue.  This must be synced on the flush control
            // otherwise a new DWPT could sneak into the loop with an already flushing
            // delete queue
            let sn = self.flush_control.mark_for_full_flush(self, &mut inner)?;
            debug_assert!(self.set_flushing_delete_queue(Some(Arc::clone(&fq)), Some(&mut inner)));
            (fq, sn)
        };
        debug_assert!({
            let inner = self.inner.lock();
            let current_full_flush_del_queue = inner.current_full_flush_del_queue.clone();
            current_full_flush_del_queue.is_some()
                && !Arc::ptr_eq(&inner.delete_queue, &current_full_flush_del_queue.unwrap())
        });

        let mut anything_flushed = false;
        let result: Result<()> = (|| {
            anything_flushed |= self.maybe_flush(writer)?;
            // If a concurrent flush is still in flight wait for it
            self.flush_control.wait_for_flush();
            if !anything_flushed && flushing_delete_queue.any_changes(None) {
                {
                    if self.info_stream.enabled("DW") {
                        let v = thread::current();
                        let name = v.name().unwrap_or("<unnamed>");
                        self.info_stream
                            .message("DW", &format!("{name}: flush naked frozen global deletes"));
                    }
                }

                debug_assert!(self.assert_ticket_queue_modification(&flushing_delete_queue));
                let supplier = SupplierImpl::new(flushing_delete_queue.clone());
                self.ticket_queue.add_ticket(supplier)?;
            }
            // we can't assert that we don't have any tickets in the queue since we might add a
            // DocumentsWriterDeleteQueue
            // concurrently if we have very small ram buffers this happens quite frequently
            debug_assert!(!flushing_delete_queue.any_changes(None));
            Ok(())
        })();
        let inner = self.inner.lock();
        debug_assert!(Arc::ptr_eq(
            &flushing_delete_queue,
            inner.current_full_flush_del_queue.as_ref().unwrap()
        ));
        // all DWPT have been processed and this queue has been fully flushed to the
        // ticket-queue
        flushing_delete_queue.close()?;
        result?;
        Ok(if anything_flushed { -seq_no } else { seq_no })
    }
    pub(crate) fn finish_full_flush(&self, success: bool) -> Result<()> {
        if self.info_stream.enabled("DW") {
            let thread_name = thread::current().name().unwrap_or("<unnamed>").to_string();
            self.info_stream.message(
                "DW",
                &format!("{thread_name} finishFullFlush success={success}"),
            );
        }
        debug_assert!(self.set_flushing_delete_queue(None, None));

        {
            let delete_queue = &self.inner.lock().delete_queue;
            if success {
                self.flush_control.finish_full_flush(delete_queue);
            } else {
                // TODO
                // self.flush_control.abort_full_flushes(delete_queue)?;
            }
        }
        self.pending_changes_in_current_full_flush
            .store(false, Ordering::SeqCst);
        // make sure we do execute this since we block applying deletes during full
        // flush
        self.apply_all_deletes(None)?;

        Ok(())
    }

    /// Returns the number of bytes currently being flushed
    /// This is a subset of the value returned by ramBytesUsed()
    pub(crate) fn get_flushing_bytes(&self) -> i64 {
        self.flush_control.get_flushing_bytes(None)
    }
}
impl<D, L, FN> Accountable for DocumentsWriter<D, L, FN>
where
    D: Directory,
    L: LiveIndexWriterConfig,
    FN: FlushNotifications,
{
    fn ram_bytes_used(&self) -> Result<i64> {
        let inner = self.inner.lock();
        Ok(self
            .flush_control
            .get_delete_bytes_used(&inner.delete_queue)?
            + self.flush_control.net_bytes(None))
    }
}

pub(crate) trait FlushNotifications {
    /// Called when files were written to disk that are not used anymore.
    /// It's the implementation's responsibility to clean these files up.
    fn delete_unused_files(&self, files: HashSet<String>) -> Result<()>;

    /// Called when a segment failed to flush.
    fn flush_failed<D>(&self, info: SegmentInfo<D>) -> Result<()>
    where
        D: Directory;

    /// Called after one or more segments were flushed to disk.
    fn after_segments_flushed<D, L, B>(&self, writer: &IndexWriter<D, L, B>) -> Result<()>
    where
        D: Directory,
        L: LiveIndexWriterConfig,
        B: IndexWriterBase;

    /// Should be called if a flush or an indexing operation caused
    /// a tragic / unrecoverable event.
    fn on_tragic_event<D, L, B>(
        &self,
        event: LuceneError,
        message: &str,
        writer: &IndexWriter<D, L, B>,
    ) -> Result<()>
    where
        D: Directory,
        L: LiveIndexWriterConfig,
        B: IndexWriterBase;

    /// Called once deletes have been applied either after a flush or on a deletes call.
    fn on_deletes_applied(&self) -> Result<()>;

    /// Called once the DocumentsWriter ticket queue has a backlog. This means there is an inner
    /// thread that tries to publish flushed segments but can't keep up with the other threads
    /// flushing new segments. This likely requires other thread to forcefully purge the buffer to
    /// help publishing. This can't be done in-place since we might hold index writer locks when this
    /// is called. The caller must ensure that the purge happens without an index writer lock being
    /// held.
    fn on_ticket_backlog(&self) -> Result<()>;
}

struct SupplierImpl {
    delete_queue: Arc<DocumentsWriterDeleteQueue>,
}
impl SupplierImpl {
    pub(crate) fn new(delete_queue: Arc<DocumentsWriterDeleteQueue>) -> Self {
        SupplierImpl { delete_queue }
    }
}
impl<D> Supplier<Option<FlushTicket<D>>> for SupplierImpl
where
    D: Directory,
{
    fn get(&self) -> Result<Option<FlushTicket<D>>> {
        // it's maybeFreezeGlobalBuffer(DocumentsWriterDeleteQueue deleteQueue)'s logic in Java Lucene
        match self.delete_queue.maybe_freeze_global_buffer()? {
            Some(frozen_updates) => Ok(Some(FlushTicket::new(Some(frozen_updates), false))),
            _ => Ok(None),
        }
    }
}

struct SupplierImpl1<'a, D>
where
    D: Directory,
{
    dwpt: &'a mut DocumentsWriterPerThread<D>,
}
impl<'a, D> SupplierImpl1<'a, D>
where
    D: Directory,
{
    pub(crate) fn new(dwpt: &'a mut DocumentsWriterPerThread<D>) -> Self {
        SupplierImpl1 { dwpt }
    }
}
impl<'a, D> Supplier<Option<FlushTicket<D>>> for SupplierImpl1<'a, D>
where
    D: Directory,
{
    fn get_mut(&mut self) -> Result<Option<FlushTicket<D>>> {
        let frozen_buffered_updates = self.dwpt.prepare_flush()?;
        Ok(Some(FlushTicket::new(frozen_buffered_updates, true)))
    }
}

struct SupplierImpl2<D, L>
where
    D: Directory,
    L: LiveIndexWriterConfig,
{
    index_major_version_created: i32,
    directory_orig: Arc<D>,
    directory: Arc<LockValidatingDirectoryWrapper<D>>,
    config: Arc<L>,
    delete_queue: Arc<DocumentsWriterDeleteQueue>,
    pending_num_docs: Arc<AtomicI64>,
    enable_test_points: bool,
    field_numbers: Arc<Mutex<FieldNumbers>>,
}
impl<D, L> SupplierImpl2<D, L>
where
    D: Directory,
    L: LiveIndexWriterConfig,
{
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        index_major_version_created: i32,
        directory_orig: Arc<D>,
        directory: Arc<LockValidatingDirectoryWrapper<D>>,
        config: Arc<L>,
        delete_queue: Arc<DocumentsWriterDeleteQueue>,
        field_numbers: Arc<Mutex<FieldNumbers>>,
        pending_num_docs: Arc<AtomicI64>,
        enable_test_points: bool,
    ) -> Self {
        SupplierImpl2 {
            index_major_version_created,
            directory_orig,
            directory,
            config,
            delete_queue,
            pending_num_docs,
            enable_test_points,
            field_numbers,
        }
    }
}
impl<D, L> Supplier<DocumentsWriterPerThread<D>> for SupplierImpl2<D, L>
where
    D: Directory,
    L: LiveIndexWriterConfig,
{
    fn get(&self) -> Result<DocumentsWriterPerThread<D>> {
        let infos = Builder::new(self.field_numbers.clone());
        let dwpt = DocumentsWriterPerThread::new(
            self.index_major_version_created,
            "",
            self.directory_orig.clone(),
            self.directory.clone(),
            self.config.as_ref(),
            self.delete_queue.clone(),
            infos,
            self.pending_num_docs.clone(),
            self.enable_test_points,
        )?;
        Ok(dwpt)
    }
}
