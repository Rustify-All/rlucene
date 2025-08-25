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
use crate::codecs::live_docs_format::LiveDocsFormat;
use crate::codecs::segment_info_format::SegmentInfoFormat;
use crate::codecs::{Codec, LATEST_CODEC};
use crate::document::fields::Fields;
use crate::document::numeric_doc_values_field::NumericDocValuesField;
use crate::index::approximate_priority_queue::IdentityId;
use crate::index::buffered_updates::{BufferedUpdates, MTBufferedUpdates};
use crate::index::documents_writer::FlushNotifications;
use crate::index::documents_writer_delete_queue::{DeleteSlice, DocumentsWriterDeleteQueue, Node};
use crate::index::field_infos::FieldInfos;
use crate::index::field_infos::build::Builder;
use crate::index::frozen_buffered_updates::FrozenBufferedUpdates;
use crate::index::index_writer::index_writer_util;
use crate::index::indexing_chain::{IndexingChain, ReservedField};
use crate::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::index::lockable_concurrent_approximate_priority_queue::{FlushState, Lock};
use crate::index::pending_soft_deletes::pending_soft_deletes_util;
use crate::index::segment_commit_info::SegmentCommitInfo;
use crate::index::segment_info::SegmentInfo;
use crate::index::segment_write_state::SegmentWriteState;
use crate::index::sorter::{DocMap, DocMapImpl};
use crate::search::query::Query;
use crate::store::IOContext;
use crate::store::directory::Directory;
use crate::store::flush_info::FlushInfo;
use crate::store::lock_validating_directory_wrapper::LockValidatingDirectoryWrapper;
use crate::store::tracking_directory_wrapper::TrackingDirectoryWrapper;
use crate::util::accountable::Accountable;
use crate::util::array_util::ArrayUtil;
use crate::util::bit_set::BitSet;
use crate::util::bits::Bits;
use crate::util::error::lucene_error::{LuceneError, Result};
use crate::util::fixed_bit_set::FixedBitSet;
use crate::util::info_stream::{InfoStream, InfoStreamLock};
use crate::util::io_consumer::IOConsumer;
use crate::util::{LATEST, LUCENE_10_0_0, StringHelper};
use parking_lot::{Condvar, Mutex};
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::iter::{Chain, Once, once};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, Ordering};
use std::sync::{Arc, OnceLock};
use std::{fmt, thread};

pub(crate) struct DocumentsWriterPerThread<D, Q>
where
    D: Directory,
    Q: Query,
{
    pub(crate) directory: Arc<Mutex<TrackingDirectoryWrapper<LockValidatingDirectoryWrapper<D>>>>,
    indexing_chain: IndexingChain<TrackingDirectoryWrapper<LockValidatingDirectoryWrapper<D>>>,
    pending_updates: MTBufferedUpdates<Q>,
    segment_info: SegmentInfo<D>,
    pub(crate) aborted: Arc<AtomicBool>,
    pub(crate) flush_pending: Arc<OnceLock<bool>>,
    last_committed_bytes_used: AtomicI64,
    has_flushed: OnceCell<bool>,
    field_infos: Builder,
    info_stream: InfoStreamLock,
    num_docs_in_ram: i32,
    pub(crate) delete_queue: Arc<DocumentsWriterDeleteQueue<Q>>,
    delete_slice: Option<DeleteSlice<Q>>,
    pending_num_docs: Arc<AtomicI64>,
    enable_test_points: bool,
    delete_doc_ids: Vec<i32>,
    num_deleted_doc_ids: i32,
    index_major_version_created: i32,
    files_to_delete: HashSet<String>,
    aborting_exception: Option<LuceneError>,
    id: String,
    pub(crate) state: Arc<State>,
    parent_field: Option<String>,
}

pub(crate) struct State {
    cvar: Condvar,
    available: Mutex<bool>,
}

impl Lock for State {
    fn lock(&self) {
        let mut guard = self.available.lock();
        while !*guard {
            self.cvar.wait(&mut guard);
        }
        *guard = false;
    }

    fn try_lock(&self) -> bool {
        let mut flag = self.available.lock();
        if *flag {
            *flag = false;
            true
        } else {
            false
        }
    }

    fn unlock(&self) {
        let mut guard = self.available.lock();
        *guard = true;
        self.cvar.notify_one();
    }

    fn is_locked(&self) -> bool {
        let flag = self.available.lock();
        !*flag
    }
}

impl<D, Q> DocumentsWriterPerThread<D, Q>
where
    D: Directory,
    Q: Query,
{
    fn on_aborting_exception(&mut self, throwable: LuceneError) {
        debug_assert!(
            self.aborting_exception.is_none(),
            "aborting exception has already been set"
        );
        self.aborting_exception = Some(throwable);
    }
    pub(crate) fn is_aborted(&self) -> bool {
        self.aborted.load(Ordering::SeqCst)
    }
    pub(crate) fn abort(&mut self) -> Result<()> {
        self.aborted.store(true, Ordering::SeqCst);
        self.pending_num_docs
            .fetch_add(-(self.num_docs_in_ram as i64), Ordering::SeqCst);

        {
            let mut info_stream = self.info_stream.lock();
            if info_stream.enabled("DWPT") {
                info_stream.message("DWPT", "now abort");
            }
        }

        let abort_result = (|| {
            self.indexing_chain.abort()?;
            Ok(())
        })();
        self.pending_updates.clear();

        {
            let mut info_stream = self.info_stream.lock();
            if info_stream.enabled("DWPT") {
                info_stream.message("DWPT", "done abort");
            }
        }
        abort_result
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new<L: LiveIndexWriterConfig>(
        index_major_version_created: i32,
        segment_name: &str,
        directory_orig: Arc<Mutex<D>>,
        directory: Arc<Mutex<LockValidatingDirectoryWrapper<D>>>,
        index_writer_config: &L,
        delete_queue: Arc<DocumentsWriterDeleteQueue<Q>>,
        field_infos: Builder,
        pending_num_docs: Arc<AtomicI64>,
        enable_test_points: bool,
    ) -> Result<Self> {
        let info_stream = index_writer_config.get_info_stream();
        let tracking_dir = TrackingDirectoryWrapper::new(directory.clone());
        let directory_wrapped = Arc::new(Mutex::new(tracking_dir));
        let pending_updates = MTBufferedUpdates::new_sync(segment_name);
        let delete_slice = Some(delete_queue.new_slice());
        let random_id = StringHelper::random_id();
        let id = StringHelper::id_to_string(Some(&random_id));
        let segment_info = SegmentInfo::new(
            directory_orig.clone(),
            Some((*LATEST).clone()),
            Some((*LATEST).clone()),
            segment_name,
            -1,
            false,
            false,
            HashMap::new(),
            random_id,
            HashMap::new(),
            index_writer_config.get_index_sort(),
        )?;

        if info_stream.lock().enabled("DWPT") {
            info_stream.lock().message(
                "DWPT",
                &format!(
                    "{} init seg={} delQueue={}",
                    std::thread::current().name().unwrap_or(""),
                    segment_name,
                    delete_queue
                ),
            );
        }

        let indexing_chain = IndexingChain::new(
            index_major_version_created,
            &segment_info,
            directory_wrapped.clone(),
            index_writer_config,
        );

        // TODO: 应该在updateDocuments期间调用
        // let parent_field = index_writer_config
        //     .get_parent_field()
        //     .map(|pf| indexing_chain.mark_as_reserved(NumericDocValuesField::new(pf, -1)));

        let state = State {
            cvar: Condvar::new(),
            available: Mutex::new(true),
        };
        let parent_field = index_writer_config
            .get_parent_field()
            .map(|parent_field| parent_field.to_string());

        Ok(DocumentsWriterPerThread {
            directory: directory_wrapped,
            indexing_chain,
            pending_updates,
            segment_info,
            aborted: Arc::new(AtomicBool::new(false)),
            flush_pending: Arc::new(OnceLock::new()),
            last_committed_bytes_used: AtomicI64::new(0),
            has_flushed: OnceCell::new(),
            field_infos,
            info_stream,
            num_docs_in_ram: 0,
            delete_queue,
            delete_slice,
            pending_num_docs,
            enable_test_points,
            delete_doc_ids: Vec::new(),
            num_deleted_doc_ids: 0,
            index_major_version_created,
            files_to_delete: HashSet::new(),
            aborting_exception: None,
            id: id.clone(),
            state: Arc::new(state),
            parent_field,
        })
    }

    pub(crate) fn test_point(&self, message: &str) {
        if self.enable_test_points {
            let mut info_stream = self.info_stream.lock();
            debug_assert!(info_stream.enabled("TP"));
            info_stream.message("TP", message);
        }
    }
    /// Anything that will add N docs to the index should reserve first to make sure it's allowed.
    fn reserve_one_doc(&self) -> Result<()> {
        let new_count = self
            .pending_num_docs
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1);

        let max = index_writer_util::ACTUAL_MAX_DOCS as i64;
        if new_count > max {
            self.pending_num_docs.fetch_sub(1, Ordering::SeqCst);
            return Err(LuceneError::illegal_argument(format!(
                "number of documents in the index cannot exceed {max}"
            )));
        }
        Ok(())
    }
    pub(crate) fn update_documents<DI, DF, FN, L>(
        &mut self,
        docs: DI,
        delete_node: Option<Arc<Node<Q>>>,
        flush_notifications: &mut FN,
        index_writer_config: &L,
        num_docs_in_ram: &mut AtomicI32,
    ) -> Result<i64>
    where
        DI: IntoIterator<Item = DF>,
        DF: IntoIterator<Item = Fields>,
        FN: FlushNotifications,
        L: LiveIndexWriterConfig,
    {
        self.test_point("DocumentsWriterPerThread addDocuments start");
        debug_assert!(
            self.aborting_exception.is_none(),
            "DWPT has hit aborting exception but is still indexing"
        );

        {
            let mut info_stream = self.info_stream.lock();
            if info_stream.enabled("DWPT") {
                info_stream.message(
                    "DWPT",
                    &format!(
                        "{} update delTerm={} docID={} seg={} ",
                        thread::current().name().unwrap_or(""),
                        match delete_node {
                            Some(ref node) => node.to_string(),
                            None => "none".to_string(),
                        },
                        self.num_docs_in_ram,
                        self.segment_info.name
                    ),
                );
            }
        }

        let docs_in_ram_before = self.num_docs_in_ram;
        let mut all_docs_indexed = false;
        let result = (|| -> Result<i64> {
            for doc in docs {
                match &self.parent_field {
                    Some(parent) => {
                        let doc_wrapper = DocWrapper::new(doc, parent.clone());
                        self.reserve_one_doc()?;
                        num_docs_in_ram.store(1, Ordering::SeqCst);
                        self.num_docs_in_ram += 1;
                        self.indexing_chain.process_document(
                            self.num_docs_in_ram - 1,
                            doc_wrapper,
                            &mut self.segment_info,
                            &mut self.field_infos,
                            index_writer_config,
                        )?;
                    },
                    None => {
                        if self.segment_info.index_sort.is_some()
                            && self.index_major_version_created >= LUCENE_10_0_0.major
                        {
                            return Err(LuceneError::illegal_argument(
                                "a parent field must be set in order to use document blocks with index sorting; see IndexWriterConfig#set_parent_field",
                            ));
                        } else {
                            self.reserve_one_doc()?;
                            num_docs_in_ram.store(1, Ordering::SeqCst);
                            self.num_docs_in_ram += 1;
                            self.indexing_chain.process_document(
                                self.num_docs_in_ram - 1,
                                doc,
                                &mut self.segment_info,
                                &mut self.field_infos,
                                index_writer_config,
                            )?;
                        }
                    },
                }
            }

            let num_docs = self.num_docs_in_ram - docs_in_ram_before;
            if num_docs > 1 {
                self.segment_info.set_has_blocks();
            }

            all_docs_indexed = true;
            let written = self.finish_documents(delete_node, docs_in_ram_before)?;
            Ok(written)
        })();

        if result.is_err() && !all_docs_indexed && !self.aborted.load(Ordering::SeqCst) {
            // the iterator threw an exception that is not aborting
            // go and mark all docs from this block as deleted
            let to_delete = self.num_docs_in_ram - docs_in_ram_before;
            self.delete_last_docs(to_delete)?;
        }
        self.maybe_abort("updateDocuments", flush_notifications)?;
        result
    }

    fn finish_documents(
        &mut self,
        delete_node: Option<Arc<Node<Q>>>,
        doc_id_up_to: i32,
    ) -> Result<i64> {
        // here we actually finish the document in two steps 1. push the delete into
        // the queue and update our slice. 2. increment the DWPT private document
        // id.
        //
        //the updated slice we get from 1. holds all the deletes that have occurred
        //since we updated the slice the last time.
        //
        // Apply delTerm only after all indexing has
        //succeeded, but apply it only to docs prior to when
        //this batch started:
        let delete_slice = self.delete_slice.as_mut().unwrap();
        let seq_no: i64 = if let Some(node) = delete_node {
            let seq = self
                .delete_queue
                .add_with_slice(node.clone(), delete_slice)?;
            debug_assert!(
                delete_slice.is_tail(&node),
                "expected the delete term as the tail item"
            );
            delete_slice.apply(&mut self.pending_updates, doc_id_up_to)?;
            seq
        } else {
            let mut seq = self.delete_queue.update_slice(delete_slice)?;
            if seq < 0 {
                seq = -seq;
                delete_slice.apply(&mut self.pending_updates, doc_id_up_to)?;
            } else {
                delete_slice.reset();
            }
            seq
        };

        Ok(seq_no)
    }
    // This method marks the last N docs as deleted. This is used
    // in the case of a non-aborting exception. There are several cases
    // where we fail a document ie. due to an exception during analysis
    // that causes the doc to be rejected but won't cause the DWPT to be
    // stale nor the entire IW to abort and shutdown. In such a case
    // we only mark these docs as deleted and turn it into a livedocs
    // during flush
    fn delete_last_docs(&mut self, doc_count: i32) -> Result<()> {
        let from = self.num_docs_in_ram - doc_count;
        let to = self.num_docs_in_ram;
        let new_len = self.num_deleted_doc_ids + (to - from);
        ArrayUtil::grow_i32(&mut self.delete_doc_ids, new_len as usize)?;

        for doc_id in from..to {
            self.delete_doc_ids[self.num_docs_in_ram as usize] = doc_id;
            self.num_deleted_doc_ids += 1;
        }
        debug_assert!(self.delete_doc_ids.len() <= i32::MAX as usize);
        self.num_deleted_doc_ids = self.delete_doc_ids.len() as i32;
        // NOTE: we do not trigger flush here.  This is
        // potentially a RAM leak, if you have an app that tries
        // to add docs but every single doc always hits a
        // non-aborting exception.  Allowing a flush here gets
        // very messy because we are only invoked when handling
        // exceptions so to do this properly, while handling an
        // exception we'd have to go off and flush new deletes
        // which is risky (likely would hit some other
        // confounding exception).
        Ok(())
    }
    /// Returns the number of RAM resident documents in this [`DocumentsWriterPerThread`]
    pub fn get_num_docs_in_ram(&self) -> i32 {
        self.num_docs_in_ram
    }
    /// Prepares this DWPT for flushing. This method will freeze and return the [`DocumentsWriterDeleteQueue`]’s global buffer and apply all pending deletes to this DWPT.
    pub(crate) fn prepare_flush(&mut self) -> Result<FrozenBufferedUpdates<Q>> {
        debug_assert!(self.num_docs_in_ram > 0);

        let global_updates = self
            .delete_queue
            .freeze_global_buffer(&mut self.delete_slice)?;
        // deleteSlice can possibly be null if we have hit non-aborting exceptions during indexing and never succeeded adding a document
        if let Some(delete_slice) = self.delete_slice.as_mut() {
            // apply all deletes before we flush and release the delete slice
            delete_slice.apply(&mut self.pending_updates, self.num_docs_in_ram)?;
            debug_assert!(delete_slice.is_empty());
            delete_slice.reset();
        }
        match global_updates {
            Some(global_updates) => Ok(global_updates),
            None => Err(LuceneError::illegal_state("global_updates is None"))?,
        }
    }
    ///  Flush all pending docs to a new segment
    pub(crate) fn flush<FN, L>(
        &mut self,
        flush_notifications: &FN,
        index_writer_config: &L,
    ) -> Result<Option<FlushedSegment<D, Q>>>
    where
        FN: FlushNotifications,
        L: LiveIndexWriterConfig,
    {
        debug_assert_eq!(self.flush_pending.get(), Some(&true));
        debug_assert!(self.num_docs_in_ram > 0);
        debug_assert!(
            self.delete_slice.as_ref().is_none_or(|ds| ds.is_empty()),
            "all deletes must be applied in prepareFlush"
        );

        self.segment_info.set_max_doc(self.num_docs_in_ram)?;

        let result = (|| -> Result<Option<FlushedSegment<D, Q>>> {
            let (mut fs, sort_map, t0) = {
                let dir = &mut *self.directory.lock();
                let io_context = IOContext::with_flush(FlushInfo::new(
                    self.num_docs_in_ram,
                    self.last_committed_bytes_used.load(Ordering::SeqCst),
                ))?;
                let mut flush_state = SegmentWriteState::new(
                    Some(self.info_stream.clone()),
                    dir,
                    Rc::new(self.field_infos.finish()?),
                    &io_context,
                );

                let start_mb_used =
                    self.last_committed_bytes_used.load(Ordering::SeqCst) as f64 / 1024.0 / 1024.0;

                // Apply delete-by-docID now (delete-byDocID only
                // happens when an exception is hit processing that
                // doc, eg if analyzer has some problem w/ the text):
                if self.num_deleted_doc_ids > 0 {
                    let mut live_docs = FixedBitSet::new(self.num_docs_in_ram);
                    live_docs.set_with_range(0, self.num_docs_in_ram);

                    for &doc_id in &self.delete_doc_ids {
                        live_docs.clear_with_index(doc_id);
                    }

                    flush_state.live_docs = Some(live_docs);
                    flush_state.del_count_on_flush = self.num_deleted_doc_ids;
                    self.delete_doc_ids.clear();
                    self.num_deleted_doc_ids = 0;
                }

                if self.aborted.load(Ordering::SeqCst) {
                    let mut info_stream = self.info_stream.lock();
                    if info_stream.enabled("DWPT") {
                        info_stream.message("DWPT", "flush: skip because aborting is set");
                    }
                    return Ok(None);
                }

                let t0 = std::time::Instant::now();

                {
                    let mut info_stream = self.info_stream.lock();
                    if info_stream.enabled("DWPT") {
                        info_stream.message(
                            "DWPT",
                            &format!(
                                "flush postings as segment {} numDocs={}",
                                self.segment_info.name, self.num_docs_in_ram
                            ),
                        );
                    }
                }
                let mut soft_deleted_docs =
                    if let Some(field) = index_writer_config.get_soft_deletes_field() {
                        self.indexing_chain.get_has_doc_values(field)?
                    } else {
                        None
                    };

                let sort_map = self.indexing_chain.flush(
                    &mut flush_state,
                    &mut self.segment_info,
                    Some(&mut self.pending_updates),
                    index_writer_config,
                )?;

                flush_state.soft_del_count_on_flush = if let Some(ref mut iter) = soft_deleted_docs
                {
                    let cnt = pending_soft_deletes_util::count_soft_deletes(
                        Some(iter),
                        flush_state.live_docs.as_ref(),
                    )?;
                    debug_assert!(
                        self.segment_info.max_doc()? >= (cnt + flush_state.del_count_on_flush)
                    );
                    cnt
                } else {
                    0
                };

                // We clear this here because we already resolved them (private to this segment) when writing
                // postings:
                self.pending_updates.clear_delete_terms();
                let files = self.directory.lock().take_created_files();
                self.segment_info.set_files(files)?;

                let dir = self.segment_info.dir.clone();
                let segment_info_per_commit = SegmentCommitInfo::new(
                    std::mem::replace(&mut self.segment_info, SegmentInfo::dummy(dir)),
                    0,
                    flush_state.soft_del_count_on_flush,
                    -1,
                    -1,
                    -1,
                    Some(StringHelper::random_id()),
                )?;

                {
                    let mut info = self.info_stream.lock();
                    if info.enabled("DWPT") {
                        info.message(
                            "DWPT",
                            &format!(
                                "new segment has {} deleted docs",
                                flush_state.del_count_on_flush
                            ),
                        );
                        info.message(
                            "DWPT",
                            &format!(
                                "new segment has {} soft-deleted docs",
                                flush_state.soft_del_count_on_flush
                            ),
                        );
                        info.message(
                            "DWPT",
                            &format!(
                                "new segment has {}; {}; {}; {}; {}",
                                if flush_state.field_infos.has_term_vectors() {
                                    "vectors"
                                } else {
                                    "no vectors"
                                },
                                if flush_state.field_infos.has_norms() {
                                    "norms"
                                } else {
                                    "no norms"
                                },
                                if flush_state.field_infos.has_doc_values() {
                                    "docValues"
                                } else {
                                    "no docValues"
                                },
                                if flush_state.field_infos.has_prox() {
                                    "prox"
                                } else {
                                    "no prox"
                                },
                                if flush_state.field_infos.has_freq() {
                                    "freqs"
                                } else {
                                    "no freqs"
                                }
                            ),
                        );
                        info.message(
                            "DWPT",
                            &format!("flushedFiles={:?}", segment_info_per_commit.files()),
                        );
                        info.message(
                            "DWPT",
                            &format!("flushed codec={}", index_writer_config.get_codec()),
                        );
                    }
                }

                let segment_deletes = if self.pending_updates.delete_queries.is_empty()
                    && self
                        .pending_updates
                        .num_field_updates
                        .load(Ordering::SeqCst)
                        == 0
                {
                    self.pending_updates.clear();
                    None
                } else {
                    Some(std::mem::replace(
                        &mut self.pending_updates,
                        BufferedUpdates::new_sync("dummy"),
                    ))
                };

                {
                    let mut info = self.info_stream.lock();
                    if info.enabled("DWPT") {
                        let new_size_mb =
                            segment_info_per_commit.size_in_bytes()? as f64 / 1024.0 / 1024.0;
                        info.message(
                            "DWPT",
                            &format!(
                                "flushed: segment={} ramUsed={:.2} MB newFlushedSize={:.2} MB docs/MB={:.2}",
                                self.segment_info.name,
                                start_mb_used,
                                new_size_mb,
                                segment_info_per_commit.info.max_doc()? as f64 / new_size_mb
                            ),
                        );
                    }
                }

                let fs = FlushedSegment::new(
                    self.info_stream.clone(),
                    segment_info_per_commit,
                    flush_state.field_infos.clone(),
                    segment_deletes,
                    flush_state.live_docs.take(),
                    flush_state.del_count_on_flush,
                    sort_map.clone(),
                )?;
                (fs, sort_map, t0)
            };
            self.seal_flushed_segment(&mut fs, sort_map, flush_notifications, index_writer_config)?;

            {
                let mut info = self.info_stream.lock();
                if info.enabled("DWPT") {
                    info.message(
                        "DWPT",
                        &format!("flush time {} ms", t0.elapsed().as_millis()),
                    );
                }
            }

            Ok(Some(fs))
        })();

        self.maybe_abort("flush", flush_notifications)?;
        self
            .has_flushed
            .set(true)
            .map_err(|_| LuceneError::illegal_state("flush already called"))?;
        match &result {
            Ok(_) => {}
            Err(_e) => {
                // TODO Lucene 没有实现clone
                self.on_aborting_exception(LuceneError::illegal_state(""))
            }
        }
        result
    }

    fn maybe_abort<FN>(&mut self, location: &str, flush_notifications: &FN) -> Result<()>
    where
        FN: FlushNotifications,
    {
        match self.aborting_exception {
            Some(_) if !self.aborted.load(Ordering::SeqCst) => {
                // if we are not already aborted, we can abort
                let result = self.abort();
                flush_notifications
                    .on_tragic_event(self.aborting_exception.take().unwrap(), location);
                result
            },
            _ => Ok(()),
        }
    }
    pub(crate) fn pending_files_to_delete(&self) -> &HashSet<String> {
        &self.files_to_delete
    }
    fn sort_live_docs(live_docs: &impl Bits, sort_map: &impl DocMap) -> FixedBitSet {
        let live_docs_len = live_docs.length();
        let mut sorted_live_docs = FixedBitSet::new(live_docs_len);
        sorted_live_docs.set_with_range(0, live_docs_len);

        for i in 0..live_docs_len {
            if !live_docs.get(i) {
                sorted_live_docs.clear_with_index(sort_map.old_to_new(i));
            }
        }
        sorted_live_docs
    }
    /// Seals the `SegmentInfo` for the new flushed segment and persists the deleted documents [`FixedBitSet`].
    pub(crate) fn seal_flushed_segment<FN, DM>(
        &mut self,
        flushed_segment: &mut FlushedSegment<D, Q>,
        sort_map: Option<Rc<DM>>,
        flush_notifications: &FN,
        index_writer_config: &impl LiveIndexWriterConfig,
    ) -> Result<()>
    where
        FN: FlushNotifications,
        DM: DocMap,
    {
        let new_segment = &mut flushed_segment.segment_info;

        index_writer_util::set_diagnostics(&mut new_segment.info, index_writer_util::SOURCE_FLUSH);

        let context = IOContext::with_flush(FlushInfo::new(
            new_segment.info.max_doc()?,
            new_segment.size_in_bytes()?,
        ))?;

        let result: Result<()> = (|| {
            if index_writer_config.get_use_compound_file() {
                let original_files = new_segment.info.files()?.clone();
                let mut dir = TrackingDirectoryWrapper::new(self.directory.clone());
                index_writer_util::create_compound_file(
                    &self.info_stream,
                    &mut dir,
                    &mut new_segment.info,
                    &context,
                    IOConsumerImpl::new(flush_notifications),
                )?;
                self.files_to_delete.extend(original_files);
                new_segment.info.set_use_compound_file(true);
            }

            // Have codec write SegmentInfo.  Must do this after
            // creating CFS so that 1) .si isn't slurped into CFS,
            // and 2) .si reflects useCompoundFile=true change
            // above:
            LATEST_CODEC.segment_info_format().write(
                &mut *self.directory.lock(),
                &mut new_segment.info,
                &context,
            )?;

            // TODO: ideally we would freeze newSegment here!!
            // because any changes after writing the .si will be
            // lost...

            // Must write deleted docs after the CFS so we don't
            // slurp the del file into CFS:
            if let Some(live_docs) = &flushed_segment.live_docs {
                let del_count = flushed_segment.del_count;
                debug_assert!(del_count > 0);

                if self.info_stream.lock().enabled("DWPT") {
                    self.info_stream.lock().message(
                        "DWPT",
                        &format!(
                            "flush: write {} deletes gen={}",
                            del_count,
                            new_segment.get_del_gen()
                        ),
                    );
                }
                // TODO: we should prune the segment if it's 100%
                // deleted... but merge will also catch it.

                // TODO: in the NRT case it'd be better to hand
                // this del vector over to the
                // shortly-to-be-opened SegmentReader and let it
                // carry the changes; there's no reason to use
                // filesystem as intermediary here.
                match sort_map {
                    Some(map) => {
                        LATEST_CODEC.live_docs_format().write_live_docs(
                            &Self::sort_live_docs(live_docs, &*map),
                            &mut *self.directory.lock(),
                            new_segment,
                            del_count,
                            &context,
                        )?;
                    },
                    None => {
                        LATEST_CODEC.live_docs_format().write_live_docs(
                            live_docs,
                            &mut *self.directory.lock(),
                            new_segment,
                            del_count,
                            &context,
                        )?;
                    },
                }

                new_segment.set_del_count(del_count)?;
                new_segment.advance_del_gen();
            }

            Ok(())
        })();

        if result.is_err() && self.info_stream.lock().enabled("DWPT") {
            self.info_stream.lock().message(
                "DWPT",
                &format!(
                    "hit exception creating compound file for newly flushed segment {}",
                    new_segment.info.name
                ),
            );
        }
        result
    }

    pub(crate) fn get_segment_info(&self) -> &SegmentInfo<D> {
        &self.segment_info
    }

    /// Returns true iff this DWPT is marked as flush pending
    pub(crate) fn is_flush_pending(&self) -> &bool {
        self.flush_pending.get().unwrap_or(&false)
    }
    pub(crate) fn is_queue_advanced(&self) -> bool {
        self.delete_queue.is_advanced()
    }
    /// Sets this DWPT as flush pending. This can only be set once.
    pub(crate) fn set_flush_pending(&self) -> Result<()> {
        if self.flush_pending.set(true).is_err() {
            return Err(LuceneError::illegal_state("flush_pending has been set"));
        }
        Ok(())
    }
    /// Returns the last committed bytes for this DWPT. This method can be called without acquiring the DWPT’s lock.
    pub(crate) fn get_last_committed_bytes_used(&self) -> i64 {
        self.last_committed_bytes_used.load(Ordering::SeqCst)
    }
    /// Commits the current [`ram_bytes_used()`](Self::ram_bytes_used) and stores its value for later reuse.
    /// The last committed bytes used can be retrieved via [`get_last_committed_bytes_used()`](Self::get_last_committed_bytes_used).
    pub(crate) fn commit_last_bytes_used(&mut self, delta: i64) -> Result<()> {
        debug_assert_eq!(
            self.get_commit_last_bytes_used_delta()?,
            delta,
            "delta has changed"
        );
        self.last_committed_bytes_used
            .fetch_add(delta, Ordering::SeqCst);
        Ok(())
    }
    /// Calculates the delta between the last committed bytes used and the currently used RAM.
    ///
    /// # Returns
    ///
    /// The difference between [`ram_bytes_used()`](Self::ram_bytes_used) and [`get_last_committed_bytes_used()`](Self::get_last_committed_bytes_used).
    ///
    /// # See
    ///
    /// [`commit_last_bytes_used()`](Self::commit_last_bytes_used)
    pub(crate) fn get_commit_last_bytes_used_delta(&self) -> Result<i64> {
        Ok(self.ram_bytes_used()? - self.last_committed_bytes_used.load(Ordering::SeqCst))
    }

    /// Returns `true` iff this DWPT has been flushed
    pub(crate) fn has_flushed(&self) -> &bool {
        self.has_flushed.get().unwrap_or(&true)
    }
}
impl<D, Q> IdentityId for DocumentsWriterPerThread<D, Q>
where
    D: Directory,
    Q: Query,
{
    fn id(&self) -> &str {
        &self.id
    }
}
impl<D, Q> Accountable for DocumentsWriterPerThread<D, Q>
where
    D: Directory,
    Q: Query,
{
    fn ram_bytes_used(&self) -> Result<i64> {
        // TODO
        Ok(0)
    }

    fn get_child_resources<A>(&self) -> Vec<A>
    where
        A: Accountable,
    {
        todo!()
    }
}
impl<D, Q> Display for DocumentsWriterPerThread<D, Q>
where
    D: Directory,
    Q: Query,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} [pendingDeletes={}, segment={}, aborted={}, numDocsInRAM={}, deleteQueue={}, {} deleted docIds]",
            std::any::type_name::<Self>(),
            self.pending_updates,
            self.segment_info.name,
            self.aborted.load(Ordering::SeqCst),
            self.num_docs_in_ram,
            self.delete_queue,
            self.num_deleted_doc_ids,
        )
    }
}

impl<D, Q> FlushState for DocumentsWriterPerThread<D, Q>
where
    D: Directory,
    Q: Query,
{
    fn is_flush_pending(&self) -> bool {
        *self.is_flush_pending()
    }
}

impl<D, Q> Lock for DocumentsWriterPerThread<D, Q>
where
    D: Directory,
    Q: Query,
{
    fn lock(&self) {
        self.state.lock()
    }

    fn try_lock(&self) -> bool {
        self.state.try_lock()
    }

    fn unlock(&self) {
        self.state.unlock()
    }

    fn is_locked(&self) -> bool {
        self.state.is_locked()
    }
}

pub(crate) struct FlushedSegment<D, Q>
where
    D: Directory,
    Q: Query,
{
    segment_info: SegmentCommitInfo<D>,
    field_infos: Rc<FieldInfos>,
    segment_updates: Option<FrozenBufferedUpdates<Q>>,
    live_docs: Option<FixedBitSet>,
    sort_map: Option<Rc<DocMapImpl>>,
    del_count: i32,
}
impl<D, Q> FlushedSegment<D, Q>
where
    D: Directory,
    Q: Query,
{
    fn new(
        info_stream: InfoStreamLock,
        segment_info: SegmentCommitInfo<D>,
        field_infos: Rc<FieldInfos>,
        mut segment_updates: Option<MTBufferedUpdates<Q>>,
        live_docs: Option<FixedBitSet>,
        del_count: i32,
        sort_map: Option<Rc<DocMapImpl>>,
    ) -> Result<Self> {
        let segment_updates = match segment_updates {
            Some(ref mut upd) if upd.any() => Some(FrozenBufferedUpdates::new(
                info_stream,
                upd,
                Option::from(StringHelper::id_to_string(Some(segment_info.info.get_id()))),
            )?),
            _ => None,
        };

        Ok(FlushedSegment {
            segment_info,
            field_infos,
            segment_updates,
            live_docs,
            del_count,
            sort_map,
        })
    }
}

pub struct IOConsumerImpl<'a, FN>
where
    FN: FlushNotifications,
{
    flush_notifications: &'a FN,
}
impl<'a, FN> IOConsumerImpl<'a, FN>
where
    FN: FlushNotifications,
{
    pub fn new(flush_notifications: &'a FN) -> Self {
        IOConsumerImpl {
            flush_notifications,
        }
    }
}
impl<FN> IOConsumer<&HashSet<String>> for IOConsumerImpl<'_, FN>
where
    FN: FlushNotifications,
{
    fn accept(&mut self, input: &HashSet<String>) -> Result<()> {
        self.flush_notifications.delete_unused_files(input);
        Ok(())
    }
}

pub(crate) struct DocWrapper<B> {
    doc: B,
    parent_field: String,
}
impl<B> DocWrapper<B>
where
    B: IntoIterator<Item = Fields>,
{
    pub fn new(doc: B, parent_field: String) -> Self {
        DocWrapper { doc, parent_field }
    }
}
impl<B> IntoIterator for DocWrapper<B>
where
    B: IntoIterator<Item = Fields>,
{
    type Item = Fields;
    type IntoIter = Chain<Once<Fields>, B::IntoIter>;

    fn into_iter(self) -> Self::IntoIter {
        let parent_field = Fields::Reverse(ReservedField::new(NumericDocValuesField::new(
            &self.parent_field,
            -1,
        )));
        once(parent_field).chain(self.doc)
    }
}
