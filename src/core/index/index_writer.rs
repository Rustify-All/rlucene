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
use crate::core::index::buffered_updates_stream::{
    ApplyDeletesResult, BufferedUpdatesStream, SegmentState,
};
use crate::core::index::documents_writer::{DocumentsWriter, FlushNotifications};
use crate::core::index::frozen_buffered_updates::FrozenBufferedUpdates;
use crate::core::index::index_file_deleter::IndexFileDeleter;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::merge_policy::{
    MergeContext, MergeReaderSR, MergeSpecificationNoReader, OneMergeBase, OneMergeSR,
};
use crate::core::index::merge_scheduler::{MergeScheduler, MergeSource};
use crate::core::index::merge_state::{DocMap, DocMapEnum2};
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_infos::{SegmentInfos, get_last_commit_segments_file_name};
use crate::core::store::directory::Directory;
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::long_supplier::LongSupplier;
use parking_lot::{Condvar, Mutex, MutexGuard};
use std::sync::Arc;

pub struct IndexWriter<D, L, B>
where
    D: Directory,
    L: LiveIndexWriterConfig,
    B: IndexWriterBase,
{
    pub(crate) enable_test_points: bool,
    // when unrecoverable disaster strikes, we populate this with the reason that we had to close
    // IndexWriter
    tragedy: TragicException,
    // original user directory
    pub(crate) directory_orig: Arc<D>,
    // wrapped with additional checks
    pub(crate) directory: Arc<IndexWriterDir<D>>,
    // last changeCount that was committed
    last_commit_change_count: AtomicI64,
    pending_seq_no: AtomicI64,
    pending_commit_change_count: AtomicI64,
    // TODO IMPORTANT 必须要用Mutext封装吗
    pub(crate) global_field_number_map: FieldNumbersLock,
    doc_writer: DocumentsWriter<D, FlushNotificationsImpl>,
    event_queue: Arc<EventQueue>,
    write_doc_values_lock: Mutex<()>,

    pub(crate) closed: Arc<AtomicBool>,
    closing: AtomicBool,

    maybe_merge: AtomicBool,
    merge_source: IndexWriterMergeSource,

    did_message_state: bool,
    flush_count: AtomicI32,
    flush_deletes_count: AtomicI32,
    reader_pool: ReaderPool<D, LongSupplierImpl>,
    buffered_updates_stream: Arc<BufferedUpdatesStream>,
    merge_finished_gen: AtomicI64,
    pub(crate) config: L,
    pub(crate) pending_num_docs: Arc<AtomicI64>,
    soft_deletes_enabled: bool,
    info_stream: InfoStreamMT,
    pub(crate) inner: Mutex<Inner<D>>,
    pausing: Condvar,
    pub(crate) sub: Option<B>,
    commit_lock: Mutex<CommitInner<D>>,
    full_flush_lock: Mutex<()>,
}
pub type IndexWriterDir<D> = LockValidatingDirectoryWrapper<Arc<D>>;

pub struct Inner<D>
where
    D: Directory,
{
    pub(crate) segment_infos: SegmentInfos<D>,
    // After SegmentCommitInfo removed from `segment_infos`,
    // It's ownership move to `dropped_segment_commit_infos` for some uses,
    deleter: IndexFileDeleter<D>,
    // list of segmentInfo we will fall back to if the commit fails
    rollback_segments: Vec<SegmentCommitInfo<D>>,
    // increments every time a change is completed
    change_count: i64,
    commit_user_data: Option<HashMap<String, String>>,
    pending_merges: VecDeque<OneMergeSR<D>>,
    running_merges: HashSet<MergeStat>,
    merge_exceptions: Vec<MergeStat>,
    merge_gen: i64,
    // used by forceMerge to note those needing merging
    segments_to_merge: HashMap<String, Option<bool>>,
    merges: Merges,
    merging_segments: HashSet<String>,
    merge_max_num_segments: i32,
}

pub struct CommitInner<D>
where
    D: Directory,
{
    pending_commit: Option<SegmentInfos<D>>,
    files_to_commit: Option<Vec<String>>,
    start_commit_time: Instant,
}
impl<D, L, B> Drop for IndexWriter<D, L, B>
where
    D: Directory,
    L: LiveIndexWriterConfig,
    B: IndexWriterBase,
{
    fn drop(&mut self) {
        // TODO 其他close需要用到IndexWriter的字段都需要在这里处理
        match self.event_queue.close(self) {
            Ok(_) => {},
            Err(e) => {
                eprintln!("IndexWriter drop: event_queue close error: {}", e);
            },
        }
    }
}
pub type DefaultIndexWriterType<D> = IndexWriter<D, IndexWriterConfig, EmptyIndexWriterBase>;
impl<D, L> IndexWriter<D, L, EmptyIndexWriterBase>
where
    D: Directory,
    L: LiveIndexWriterConfig,
{
    pub fn new(d: Arc<D>, conf: L) -> Result<Self> {
        Self::with_sub(d, conf, Some(EmptyIndexWriterBase))
    }
}

impl<D, L, B> IndexWriter<D, L, B>
where
    D: Directory,
    L: LiveIndexWriterConfig,
    B: IndexWriterBase,
{
    pub fn with_sub(d: Arc<D>, mut conf: L, sub: Option<B>) -> Result<Self> {
        let enable_test_points = sub.as_ref().unwrap().is_enable_test_points();
        let info_stream = conf.get_info_stream();
        let soft_deletes_enabled = conf.get_soft_deletes_field().is_some();

        // obtain the write.lock. If the user configured a timeout,
        // we wrap with a sleeper and this might take some time.
        let write_lock = d.obtain_lock(WRITE_LOCK_NAME)?;
        let result = (|| {
            let directory_orig = d.clone();
            let directory = Arc::new(LockValidatingDirectoryWrapper::new(d.clone(), write_lock));

            let mode = conf.get_open_mode();
            let (index_exists, create) = match mode {
                OpenMode::Create => {
                    let exists = directory_reader_util::index_exists(directory.as_ref())?;
                    (exists, true)
                },
                OpenMode::Append => (true, false),
                OpenMode::CreateOrAppend => {
                    let exists = directory_reader_util::index_exists(directory.as_ref())?;
                    (exists, !exists)
                },
            };

            // If index is too old, reading the segments will throw
            // IndexFormatTooOldException.

            let files = directory.list_all()?;

            // Set up our initial SegmentInfos:
            let commit = conf.get_index_commit();
            let reader = commit.as_ref().map(|c| c.get_reader());
            let mut change_count = 0;
            // TODO IMPORTANT 这里的SegmentInfos 这里不需要初始哈
            let mut segment_infos = SegmentInfos::new(conf.get_index_created_version_major())?;
            let is_reader_some = reader.is_some();
            let is_commit_some = commit.is_some();
            let did_message_state = false;
            let rollback_segments = Vec::new();
            let (reader, has_commit, _commit_dir, commit_files) = match commit {
                Some(c) => (
                    c.get_reader(),
                    true,
                    Some(c.get_directory().clone()),
                    Some(c.get_segments_file_name().to_string()),
                ),
                None => (None, false, None, None),
            };
            if create {
                if has_commit {
                    // We cannot both open from a commit point and create:
                    return match conf.get_open_mode() {
                        OpenMode::Create => Err(LuceneError::illegal_argument(
                            "cannot use IndexWriterConfig.setIndexCommit() with OpenMode.CREATE",
                        )),
                        _ => Err(LuceneError::illegal_argument(
                            "cannot use IndexWriterConfig.setIndexCommit() when index has no commit",
                        )),
                    };
                }

                // Try to read first. This is to allow creation
                // against an index that's currently open for
                // searching. In this case we write the next
                // segments_N file with no segments:
                let mut sis: SegmentInfos<D> =
                    SegmentInfos::new(conf.get_index_created_version_major())?;

                if index_exists {
                    let previous = SegmentInfos::read_latest_commit(directory.clone())?;
                    sis.update_generation_version_and_counter(&previous);
                }

                segment_infos = sis;
                let _rollback_segments = segment_infos.create_backup_segment_infos();

                // Record that we have a change (zero out all segments) pending:
                Self::changed(&mut change_count, &mut segment_infos);
            } else if reader.is_some() {
                todo!()
            } else {
                // Init from either the latest commit point, or an explicit prior commit point:

                let last_segments_file = match get_last_commit_segments_file_name(&files)? {
                    Some(f) => f,
                    None => {
                        return Err(LuceneError::index_not_found(format!(
                            "no segments* file found in {}: files: {:?}",
                            directory, files
                        )));
                    },
                };
                // Do not use SegmentInfos.read(Directory) since the spooky
                // retrying it does is not necessary here (we hold the write lock):
                segment_infos = if is_commit_some {
                    // Swap out all segments, but, keep metadata in
                    // SegmentInfos, like version & generation, to
                    // preserve write-once.  This is important if
                    // readers are open against the future commit
                    // points.
                    // TODO
                    // if !Arc::ptr_eq(&commit.get_directory(), directory_orig.as_ref()) {
                    //     return Err(LuceneError::illegal_argument(format!(
                    //         "IndexCommit's directory doesn't match my directory, expected={:?}, got={:?}",
                    //         directory_orig,
                    //         commit.get_directory()
                    //     )));
                    // }
                    let old_infos = SegmentInfos::read_commit(
                        directory_orig.clone(),
                        commit_files.as_ref().unwrap(),
                    )?;
                    Self::changed(&mut change_count, &mut segment_infos);

                    if info_stream.enabled("IW") {
                        info_stream.message(
                            "IW",
                            &format!("init: loaded commit \"{}\"", commit_files.as_ref().unwrap()),
                        );
                    }
                    old_infos
                } else {
                    SegmentInfos::read_commit(directory_orig.clone(), &last_segments_file)?
                };
                // TODO: rollback_segments 未实现
                let _rollback_segments = segment_infos.create_backup_segment_infos();
            }

            let commit_user_data = segment_infos.get_user_data().clone();
            let pending_num_docs = Arc::new(AtomicI64::new(segment_infos.total_max_doc()? as i64));

            // start with previous field numbers, but new FieldInfos
            // NOTE: this is correct even for an NRT reader because we'll pull FieldInfos
            // even for the uncommitted segments:
            let global_field_number_map = Self::get_field_number_map(&conf, &segment_infos)?;

            let fields = global_field_number_map.get_field_names();
            if !create
                && conf.get_parent_field().is_some()
                && !fields.is_empty()
                && !fields.contains(conf.get_parent_field().unwrap())
            {
                return Err(LuceneError::illegal_argument(
                    "can't add a parent field to an already existing index without a parent field",
                ));
            }

            Self::validate_index_sort(&conf, &segment_infos)?;

            let buffered_updates_stream = Arc::new(BufferedUpdatesStream::new(info_stream.clone()));

            let event_queue = Arc::new(EventQueue::new());
            let global_field_number_map = Arc::new(Mutex::new(global_field_number_map));
            let doc_writer = DocumentsWriter::new(
                FlushNotificationsImpl::new(event_queue.clone()),
                segment_infos.get_index_created_version_major(),
                enable_test_points,
                &conf,
            )?;

            let reader_pool = ReaderPool::new(
                directory.clone(),
                directory_orig.clone(),
                info_stream.clone(),
                conf.get_soft_deletes_field(),
                LongSupplierImpl::new(buffered_updates_stream.clone()),
                reader,
                conf.get_index_created_version_major(),
            );

            if conf.get_reader_pooling() {
                reader_pool.enable_reader_pooling();
            }
            let deleter = IndexFileDeleter::new(
                files.clone(),
                directory_orig.clone(),
                directory.clone(),
                conf.get_index_deletion_policy(),
                &mut segment_infos,
                info_stream.clone(),
                index_exists,
                is_reader_some,
            )?;
            // We incRef all files when we return an NRT reader from IW,
            // so all files must exist even in the NRT case:
            debug_assert!(create || Self::files_exist(&segment_infos, &deleter)?);

            if deleter.starting_commit_deleted {
                // Deletion policy deleted the "head" commit point.
                // We have to mark ourselves as changed so that if we
                // are closed w/o any further changes we write a new
                // segments_N file.
                Self::changed(&mut change_count, &mut segment_infos);
            }

            if is_reader_some {
                // We always assume we are carrying over incoming changes when opening from reader:
                segment_infos.changed();
                Self::changed(&mut change_count, &mut segment_infos);
            }

            if info_stream.enabled("IW") {
                info_stream.message(
                    "IW",
                    &format!("init: create={} reader={:?}", create, is_reader_some),
                );
            }
            let iw = Self {
                enable_test_points,
                tragedy: Arc::new(Mutex::new(None)),
                directory_orig,
                directory,
                last_commit_change_count: AtomicI64::new(0),
                pending_seq_no: AtomicI64::new(0),
                pending_commit_change_count: AtomicI64::new(0),
                global_field_number_map,
                doc_writer,
                event_queue,
                write_doc_values_lock: Mutex::new(()),
                closed: Arc::new(AtomicBool::new(false)),
                closing: AtomicBool::new(false),
                maybe_merge: AtomicBool::new(false),
                merge_source: IndexWriterMergeSource,
                did_message_state,
                flush_count: AtomicI32::new(0),
                flush_deletes_count: AtomicI32::new(0),
                reader_pool,
                buffered_updates_stream,
                merge_finished_gen: AtomicI64::new(0),
                config: conf,
                pending_num_docs,
                soft_deletes_enabled,
                info_stream: info_stream.clone(),
                inner: Mutex::new(Inner {
                    segment_infos,
                    deleter,
                    rollback_segments,
                    change_count,
                    commit_user_data: Some(commit_user_data),
                    pending_merges: VecDeque::new(),
                    running_merges: Default::default(),
                    merge_exceptions: Vec::new(),
                    merge_gen: 0,
                    segments_to_merge: HashMap::new(),
                    merges: Merges::new(),
                    merging_segments: HashSet::new(),
                    merge_max_num_segments: 0,
                }),
                pausing: Condvar::new(),
                sub,
                commit_lock: Mutex::new(CommitInner {
                    pending_commit: None,
                    files_to_commit: None,
                    start_commit_time: Instant::now(),
                }),
                full_flush_lock: Mutex::new(()),
            };
            iw.message_state()?;
            Ok(iw)
        })();
        if result.is_err() && info_stream.enabled("IW") {
            // TODO
            let msg = "init: hit exception on init; releasing write lock";

            info_stream.message("IW", msg);
        }
        result
    }

    pub(crate) fn get_index_major_version_created(&self) -> i32 {
        self.inner
            .lock()
            .segment_infos
            .get_index_created_version_major()
    }

    /// Confirms that the incoming index sort (if any) matches the existing index sort (if any).
    fn validate_index_sort(config: &L, segment_infos: &SegmentInfos<D>) -> Result<()> {
        if let Some(index_sort) = config.get_index_sort() {
            for info in segment_infos.iter() {
                let segment_index_sort = info.info.get_index_sort();

                if segment_index_sort.is_none()
                    || !is_congruent_sort(&index_sort, segment_index_sort.as_ref().unwrap())
                {
                    return Err(LuceneError::illegal_argument(format!(
                        "cannot change previous indexSort={} (from segment={}) to new indexSort={}",
                        segment_index_sort.as_ref().unwrap(),
                        info,
                        index_sort
                    )));
                }
            }
        }
        Ok(())
    }

    /// Loads or returns the already loaded the global field number map for this [`SegmentInfos`].
    /// If this [`SegmentInfos`] has no global field number map the returned instance is empty
    fn get_field_number_map(config: &L, segment_infos: &SegmentInfos<D>) -> Result<FieldNumbers> {
        let mut map =
            FieldNumbers::new(config.get_soft_deletes_field(), config.get_parent_field())?;
        for info in segment_infos.iter() {
            let fis = read_field_infos(info)?;
            for fi in fis.iter() {
                map.add_or_get(fi)?;
            }
        }

        Ok(map)
    }
    pub fn get_config(&self) -> &L {
        &self.config
    }
    pub fn get_config_mut(&mut self) -> &mut L {
        &mut self.config
    }
    fn message_state(&self) -> Result<()> {
        // if self.info_stream.enabled("IW") && !self.did_message_state {
        //     self.did_message_state = true;
        //
        //     let msg = format!(
        //         "\ndir={}\nindex={}\nversion={}\n{}",
        //         self.directory_orig,
        //         self.seg_string()?,
        //         *LATEST,
        //         self.config
        //     );
        //
        //     self.info_stream.message("IW", &msg);
        // }
        Ok(())
    }

    fn shut_down(&self) -> Result<()> {
        if self.commit_lock.lock().pending_commit.is_some() {
            return Err(LuceneError::illegal_state(
                "cannot close: prepareCommit was already called with no corresponding call to commit",
            ));
        }
        if self.should_close(true) {
            let result: Result<_> = (|| {
                if self.info_stream.enabled("IW") {
                    self.info_stream.message("IW", "now flush at close");
                }
                self.flush_with_apply_merge_deletes(true, true)?;
                self.wait_for_merges()?;
                self.commit_internal(self.config.get_merge_policy())?;
                Ok(())
            })();
            match result {
                Ok(_) => {
                    // TODO : rollback 未实现
                },
                Err(e) => {
                    // TODO : rollback 未实现
                    return Err(e);
                },
            }
        }
        Ok(())
    }
    /// Closes all open resources and releases the write lock.
    ///
    /// If [`IndexWriterConfig::commit_on_close`](LiveIndexWriterConfig::get_commit_on_close) is `true`, this will attempt to gracefully shut down by:
    /// writing any changes, waiting for any running merges, committing, and closing.
    /// In this case, note that:
    ///
    /// - If you called `prepare_commit` but failed to call `commit`, this method will throw
    ///   `IllegalStateException` and the `IndexWriter` will not be closed.
    /// - If this method throws any other exception, the `IndexWriter` will be closed, but
    ///   changes may have been lost.
    ///
    /// Note that this may be a costly operation, so try to re-use a single writer instead of
    /// frequently closing and opening new ones. See [`commit()`](Self::commit) for caveats about wingite caching done
    /// by some IO devices.
    ///
    /// **NOTE**: You must ensure no other threads are still making changes at the same time
    /// that this method is invoked.
    pub fn close(&self) -> Result<()> {
        if self.config.get_commit_on_close() {
            self.shut_down()?;
        } else {
            // TODO: roll back 未实现
        }
        Ok(())
    }

    // Returns true if this thread should attempt to close, or
    // false if IndexWriter is now closed; else,
    // waits until another thread finishes closing
    fn should_close(&self, wait_for_close: bool) -> bool {
        let mut inner = self.inner.lock();
        loop {
            if !self.closed.load(Ordering::SeqCst) {
                if !self.closing.load(Ordering::SeqCst) {
                    // We get to close
                    self.closing.store(true, Ordering::SeqCst);
                    return true;
                } else if !wait_for_close {
                    return false;
                } else {
                    // Another thread is presently trying to close;
                    // wait until it finishes one way (closes
                    // successfully) or another (fails to close)
                    self.do_wait(&mut inner);
                }
            } else {
                return false;
            }
        }
    }
    pub fn get_directory(&self) -> Arc<D> {
        self.directory_orig.clone()
    }
    /// Deletes the document(s) containing any of the given terms.
    /// All provided deletes are applied and flushed atomically at the same time.
    ///
    /// # Returns
    /// The sequence number for this operation.
    ///
    /// # Errors
    /// - `CorruptIndex` if the index is corrupt.
    /// - `Io` if a low-level IO error occurs.
    pub fn delete_documents_with_terms(&self, terms: Vec<Term>) -> Result<i64> {
        self.do_ensure_open(true)?;
        let res = (|| {
            let seq =
                self.maybe_process_events(self.doc_writer.delete_terms(&self.config, terms)?)?;
            Ok(seq)
        })();

        if let Err(ref e) = res {
            self.tragic_event(e, "deleteDocuments(Term..)");
        }

        res
    }
    /// Deletes the document(s) matching any of the provided queries.
    /// All given deletes are applied and flushed atomically at the same time.
    ///
    /// # Returns
    /// The sequence number for this operation.
    ///
    /// # Errors
    /// - `CorruptIndex` if the index is corrupt.
    /// - `Io` if a low-level IO error occurs.
    pub fn delete_documents_with_queries(&self, queries: Vec<Query>) -> Result<i64> {
        self.do_ensure_open(true)?;

        // LUCENE-6379: Specialize MatchAllDocsQuery
        for query in &queries {
            if matches!(query, Query::MatchAll(_)) {
                // TODO
                // return self.delete_all();
            }
        }

        let res = (|| {
            let seq0 = self.doc_writer.delete_queries(&self.config, queries)?;
            let seq = self.maybe_process_events(seq0)?;
            Ok(seq)
        })();

        if let Err(ref e) = res {
            self.tragic_event(e, "deleteDocuments(Query..)");
        }

        res
    }

    /// Adds a document to this index.
    ///
    /// Note that if an exception is hit (for example, disk full) then the index will remain consistent,
    /// but this document may not have been added. Furthermore, it’s possible the index will have one
    /// segment in non-compound format even when using compound files (when a merge has partially succeeded).
    ///
    /// This method periodically flushes pending documents to the `Directory` (see [flush](Self::flush), and
    /// also periodically triggers segment merges in the index according to the [`MergePolicy`] in use.
    ///
    /// Merges temporarily consume space in the directory. The amount of space required is up to 1× the
    /// size of all segments being merged when no readers/searchers are open against the index, and up
    /// to 2× the size of all segments being merged when readers/searchers are open against the index
    /// (see [`force_merge(int)`](Self::force_merge) for details). The sequence of primitive merge operations performed is
    /// governed by the merge policy.
    ///
    /// Note that each term in the document can be no longer than [`MAX_TERM_LENGTH`] in bytes; otherwise
    /// an `IllegalArgumentException` will be thrown.
    ///
    /// # Returns
    /// The `sequence number` for this operation.
    ///
    /// # Errors
    /// - Returns a `CorruptIndexException` if the index is corrupt.
    /// - Returns an `io::Error` if there is a low-level I/O error.
    pub fn add_document<DF>(&self, doc: DF) -> Result<i64>
    where
        DF: IntoIterator<Item = Fields>,
    {
        self.update_documents_with_term(None, doc)
    }

    /// Atomically adds a block of documents with sequentially assigned document IDs, such that an
    /// external reader will see all or none of the documents.
    ///
    /// **WARNING**: the index does not currently record which documents were added as a block.
    /// Curren,tly this is fine, because merging will preserve a block. The order of documents within a
    /// segment will be preserved, even when child documents within a block are deleted. Most search
    /// features (like result grouping and block joining) require you to mark documents; when these
    /// documents are deleted those features will not work as expected. Adding documents to an existing
    /// block will require you to reindex the entire block.
    ///
    /// However, it’s possible that in the future Lucene may merge more aggressively and re-order
    /// documents (for example, perhaps to obtain better index compression). In that case you may need
    /// to fully re-index your documents at that time.
    ///
    /// See [`add_document(Iterable)`](Self::add_document) for details on index and `IndexWriter` state after an exception,
    /// and flushing/merging temporary free space requirements.
    ///
    /// **NOTE**: tools that do offline splitting of an index (for example, `IndexSplitter` in contrib)
    /// or re-sorting of documents (for example, `IndexSorter` in contrib) are not aware of these
    /// atomically added documents and will likely break them up. Use such tools at your own risk!
    ///
    /// # Returns
    /// The `sequence number` for this operation.
    ///
    /// # Errors
    /// - Returns a `CorruptIndexException` if the index is corrupt.
    /// - Returns an `io::Error` if there is a low-level I/O error.
    ///
    /// @lucene.experimental
    pub fn add_documents<DI, DF>(&self, docs: DI) -> Result<i64>
    where
        DI: IntoIterator<Item = DF>,
        DF: IntoIterator<Item = Fields>,
    {
        self.update_documents(None, docs)
    }
    /// Atomically deletes documents matching the provided `del_term` and adds a block of documents with
    /// sequentially assigned document IDs, such that an external reader will see all or none of the
    /// documents.
    ///
    /// See [`add_documents(Iterable)`](Self::add_documents).
    ///
    /// # Returns
    /// The `sequence number` for this operation.
    ///
    /// # Errors
    /// - Returns a `CorruptIndexException` if the index is corrupt.
    /// - Returns an `io::Error` if there is a low-level I/O error.
    ///
    /// @lucene.experimental
    pub fn update_documents_with_term<T, DF>(&self, del_term: T, docs: DF) -> Result<i64>
    where
        T: Into<Option<Term>>,
        DF: IntoIterator<Item = Fields>,
    {
        let del_node = del_term
            .into()
            .map(|t| Arc::new(DocumentsWriterDeleteQueue::new_node_with_term(t)));

        self.update_documents(del_node, vec![docs])
    }

    /// Similar to [`update_documents(Term, Iterable)`](Self::update_documents_with_term), but takes a query instead of a term to
    /// identify the documents to be updated.
    ///
    /// @lucene.experimental
    pub fn update_documents_with_query<T, DI, DF>(&self, del_query: T, docs: DI) -> Result<i64>
    where
        T: Into<Option<Query>>,
        DI: IntoIterator<Item = DF>,
        DF: IntoIterator<Item = Fields>,
    {
        let del_node = del_query
            .into()
            .map(|q| Arc::new(DocumentsWriterDeleteQueue::new_node_with_query(q)));

        self.update_documents(del_node, docs)
    }

    fn update_documents<DI, DF>(&self, del_node: Option<Arc<Node>>, docs: DI) -> Result<i64>
    where
        DI: IntoIterator<Item = DF>,
        DF: IntoIterator<Item = Fields>,
    {
        self.do_ensure_open(true)?;
        let res: Result<i64> = (|| {
            let seq0 = self.doc_writer.update_documents(docs, del_node, self)?;
            let seq = self.maybe_process_events(seq0)?;
            Ok(seq)
        })();

        if let Err(ref e) = res {
            self.tragic_event(e, "updateDocuments");
            if self.info_stream.enabled("IW") {
                self.info_stream
                    .message("IW", "hit exception updating document");
            }
            self.maybe_close_on_tragic_event()?;
        }
        res
    }
    /// Expert: Atomically updates documents matching the provided `term` with the given
    /// DocValues fields and adds a block of documents with sequentially assigned document IDs,
    /// ensuring that an external reader will see **all or none** of the documents.
    ///
    /// One use of this API is to **retain older versions** of documents instead of replacing them.
    /// Existing documents can be updated to reflect they are no longer current,
    /// while atomically adding new documents at the same time.
    ///
    /// In contrast to [`update_documents`](Self::update_documents),
    /// this method does **not delete** documents in the index matching the given term,
    /// but instead updates them with the specified DocValues fields —
    /// which can be used as a **soft-delete mechanism**.
    ///
    /// See also [`add_documents`](Self::add_documents)
    /// and [`update_documents`](Self::update_documents).
    ///
    /// # Returns
    /// The `sequence number` for this operation.
    ///
    /// # Errors
    /// * [`LuceneError::CorruptIndex`] - If the index is corrupt.
    /// * [`LuceneError::Io`] - If there is a low-level I/O error.
    pub fn soft_update_documents<T, DF>(
        &self,
        term: T,
        docs: DF,
        soft_deletes: Vec<Fields>,
    ) -> Result<i64>
    where
        T: Into<Arc<Term>>,
        DF: IntoIterator<Item = Vec<Fields>>,
    {
        if soft_deletes.is_empty() {
            return Err(LuceneError::illegal_argument(
                "at least one soft delete must be present",
            ));
        }

        let updates = self.build_doc_values_update(term, soft_deletes)?;
        let node = DocumentsWriterDeleteQueue::new_node_with_doc_values(updates);
        self.update_documents(Some(Arc::new(node)), docs)
    }

    /// Drops a segment that has 100% deleted documents.
    pub(crate) fn drop_deleted_segment(&self, seg_id: &str, inner: &mut Inner<D>) -> Result<()> {
        // If a merge has already registered for this
        // segment, we leave it in the readerPool; the
        // merge will skip merging it and will then drop
        // it once it's done:
        if inner.merging_segments.contains(seg_id) {
            // it's possible that we invoke this method more than once for the same SCI
            // we must only remove the docs once!
            return Ok(());
        }

        // it's possible that we invoke this method more than once for the same SCI
        // we must only remove the docs once!
        let (mut drop_pending_docs, max_doc) = match inner.segment_infos.remove(seg_id) {
            Some(sci) => {
                let max_doc = sci.info.max_doc()?;
                (true, max_doc)
            },
            None => (false, -1),
        };
        let res: Result<()> = (|| {
            // this is sneaky - we might hit an exception while dropping a reader, but then we have
            // already
            // removed the segment for the segmentInfo and we lost the pendingDocs update due to that.
            // therefore, we execute the adjustPendingNumDocs in a finally block to account for that.
            drop_pending_docs |= self.reader_pool.drop(seg_id)?;
            Ok(())
        })();

        if drop_pending_docs {
            let dec = -(max_doc as i64);
            self.adjust_pending_num_docs(dec);
        }
        res
    }
    /// Expert: Updates a document by first updating the document(s) containing the given `term`
    /// with the provided DocValues fields, and then adding a new document.
    /// The DocValues update and the addition are **atomic** as observed by readers
    /// of the same index (a flush may only occur after the add).
    ///
    /// One use of this API is to **retain older versions** of documents instead of replacing them.
    /// Existing documents can be updated to reflect they are no longer current,
    /// while atomically adding new documents at the same time.
    ///
    /// In contrast to [`update_document`](Self::update_document),
    /// this method does **not delete** documents in the index matching the given term,
    /// but instead updates them with the specified DocValues fields —
    /// which can be used as a **soft-delete mechanism**.
    ///
    /// See also [`add_documents`](Self::add_documents) and [`update_documents`](Self::update_documents).
    ///
    /// # Returns
    /// The `sequence number` for this operation.
    ///
    /// # Errors
    /// * [`LuceneError::CorruptIndex`] - If the index is corrupt.
    /// * [`LuceneError::Io`] - If there is a low-level I/O error.
    pub fn soft_update_document<T, DF>(
        &self,
        term: T,
        docs: DF,
        soft_deletes: Vec<Fields>,
    ) -> Result<i64>
    where
        T: Into<Arc<Term>>,
        DF: IntoIterator<Item = Fields>,
    {
        if soft_deletes.is_empty() {
            return Err(LuceneError::illegal_argument(
                "at least one soft delete must be present",
            ));
        }

        let updates = self.build_doc_values_update(term, soft_deletes)?;
        let node = DocumentsWriterDeleteQueue::new_node_with_doc_values(updates);
        self.update_documents(Some(Arc::new(node)), vec![docs])
    }

    /// Updates a document's [`NumericDocValues`](crate::core::index::numeric_doc_values::NumericDocValues)
    /// for the given `field` to the specified `value`.
    ///
    /// You can only update fields that already exist in the index —
    /// new fields cannot be added through this method.
    /// Additionally, only fields that were indexed **solely with DocValues**
    /// are eligible for update.
    ///
    /// # Parameters
    /// * `term` - The term to identify the document(s) to be updated.
    /// * `field` - Field name of the [`NumericDocValues`](crate::core::index::numeric_doc_values::NumericDocValues) field.
    /// * `value` - New numeric value for the field.
    ///
    /// # Returns
    /// The `sequence number` for this operation.
    ///
    /// # Errors
    /// * [`LuceneError::CorruptIndex`] - If the index is corrupt.
    /// * [`LuceneError::Io`] - If there is a low-level I/O error.
    pub fn update_numeric_doc_value<T, F>(&self, term: T, field: F, value: i64) -> Result<i64>
    where
        T: Into<Arc<Term>>,
        F: Into<String>,
    {
        let field = field.into();
        self.do_ensure_open(true)?;

        self.global_field_number_map
            .lock()
            .verify_or_create_dv_only_field(&field, &DocValuesType::Numeric, true)?;

        if self.config.get_index_sort_fields().contains(&field) {
            return Err(LuceneError::illegal_argument(format!(
                "cannot update docvalues field involved in the index sort, field={}, sort={}",
                field,
                match self.config.get_index_sort() {
                    Some(s) => s.to_string(),
                    None => "<None>".to_string(),
                }
            )));
        }

        let res = (|| {
            let dv_update = DocValuesUpdate::new(
                DocValuesType::Numeric,
                term,
                field,
                MAX_INT,
                DocValuesUpdateEnum::Numeric(NumericDocValuesUpdate::new(Some(value))),
            );
            let seq = self
                .doc_writer
                .update_doc_values(&self.config, vec![dv_update])?;
            self.maybe_process_events(seq)
        })();

        if let Err(ref e) = res {
            self.tragic_event(e, "updateNumericDocValue");
        }
        res
    }

    /// Updates a document's [`BinaryDocValues`](crate::core::index::binary_doc_values::BinaryDocValues) for the given `field` to the specified `value`.
    ///
    /// You can only update fields that already exist in the index —
    /// new fields cannot be added through this method.
    /// Additionally, only fields that were indexed **solely with DocValues**
    /// are eligible for update.
    ///
    ///
    /// **Note:**
    /// This method currently replaces the existing value of **all** affected
    /// documents with the new value.
    ///
    /// # Parameters
    /// * `term` - The term to identify the document(s) to be updated.
    /// * `field` - Field name of the [`BinaryDocValues`](crate::core::index::binary_doc_values::BinaryDocValues) field.
    /// * `value` - New value for the field.
    ///
    /// # Returns
    /// The `sequence number` for this operation.
    ///
    /// # Errors
    /// * [`LuceneError::CorruptIndex`] - If the index is corrupt.
    /// * [`LuceneError::Io`] - If there is a low-level I/O error.
    pub fn update_binary_doc_value<T, F>(
        &self,
        term: T,
        field: F,
        value: BytesRef<Vec<u8>>,
    ) -> Result<i64>
    where
        T: Into<Arc<Term>>,
        F: Into<String>,
    {
        let field = field.into();
        self.do_ensure_open(true)?;

        self.global_field_number_map
            .lock()
            .verify_or_create_dv_only_field(&field, &DocValuesType::Binary, true)?;

        let res = (|| {
            let dv_update = DocValuesUpdate::new(
                DocValuesType::Binary,
                term,
                field,
                MAX_INT,
                DocValuesUpdateEnum::Binary(BinaryDocValuesUpdate::new(Some(value))),
            );
            let seq = self
                .doc_writer
                .update_doc_values(&self.config, vec![dv_update])?;
            self.maybe_process_events(seq)
        })();

        if let Err(ref e) = res {
            self.tragic_event(e, "updateBinaryDocValue");
        }
        res
    }

    /// Updates documents' DocValues fields to the given values.
    /// Each field update is applied to the set of documents that are associated
    /// with the [`Term`] to the same value.
    ///
    /// All updates are atomically applied and flushed together.
    /// If a doc values field's data is `None`, the existing value is removed
    /// from all documents matching the term.
    ///
    /// # Parameters
    /// * `updates` - The updates to apply.
    ///
    /// # Returns
    /// The `sequence number` for this operation.
    ///
    /// # Errors
    /// * [`LuceneError::CorruptIndex`] - If the index is corrupt.
    /// * [`LuceneError::Io`] - If there is a low-level I/O error.
    pub fn update_doc_values<T>(&self, term: T, updates: Vec<Fields>) -> Result<i64>
    where
        T: Into<Arc<Term>>,
    {
        self.do_ensure_open(true)?;
        let dv_updates = self.build_doc_values_update(term, updates)?;

        let res = (|| {
            let seq = self
                .doc_writer
                .update_doc_values(&self.config, dv_updates)?;
            self.maybe_process_events(seq)
        })();

        if let Err(ref e) = res {
            self.tragic_event(e, "updateDocValues");
        }
        res
    }

    fn build_doc_values_update<T>(
        &self,
        term: T,
        updates: Vec<Fields>,
    ) -> Result<Vec<DocValuesUpdate>>
    where
        T: Into<Arc<Term>>,
    {
        let term = term.into();
        let mut dv_updates = Vec::with_capacity(updates.len());

        for mut f in updates {
            let name = f.name().to_string();
            let dv_type = f.field_type().doc_values_type();
            if *dv_type == DocValuesType::None {
                return Err(LuceneError::illegal_argument(format!(
                    "can only update NUMERIC or BINARY fields! field={}",
                    name
                )));
            }
            // if this field doesn't exist we try to add it.
            // if it exists and the DV type doesn't match or it is not DV only field,
            // we will get an error.
            self.global_field_number_map
                .lock()
                .verify_or_create_dv_only_field(&name, dv_type, false)?;

            if self.config.get_index_sort_fields().contains(&name) {
                return Err(LuceneError::illegal_argument(format!(
                    "cannot update docvalues field involved in the index sort, field={}, sort={}",
                    name,
                    match self.config.get_index_sort() {
                        Some(s) => s.to_string(),
                        None => "<None>".to_string(),
                    }
                )));
            }

            let update = match dv_type {
                DocValuesType::Numeric => {
                    let value = match f.numeric_value()? {
                        Some(v) => match v.to_i64() {
                            Some(n) => n,
                            None => {
                                return Err(LuceneError::illegal_argument(format!(
                                    "numeric value for field={} can not convert to i64: {:?}",
                                    name, v
                                )));
                            },
                        },
                        None => {
                            return Err(LuceneError::illegal_argument(format!(
                                "missing numeric value for field={}",
                                name
                            )));
                        },
                    };
                    let sub_update =
                        DocValuesUpdateEnum::Numeric(NumericDocValuesUpdate::new(Some(value)));
                    DocValuesUpdate::new(
                        DocValuesType::Numeric,
                        term.clone(),
                        name,
                        MAX_INT,
                        sub_update,
                    )
                },
                DocValuesType::Binary => {
                    let bytes = f.take_binary_value()?;
                    let sub_update = DocValuesUpdateEnum::Binary(BinaryDocValuesUpdate::new(bytes));
                    DocValuesUpdate::new(
                        DocValuesType::Binary,
                        term.clone(),
                        name,
                        MAX_INT,
                        sub_update,
                    )
                },
                _ => {
                    return Err(LuceneError::illegal_argument(format!(
                        "can only update NUMERIC or BINARY fields: field={}, type={:?}",
                        name, dv_type
                    )));
                },
            };

            dv_updates.push(update);
        }
        Ok(dv_updates)
    }

    /// Return an unmodifiable set of all field names as visible from this IndexWriter, across all segments of the index.
    pub fn get_field_names(&self) -> HashSet<String> {
        // FieldNumbers#getFieldNames() returns an unmodifiableSet
        self.global_field_number_map.lock().get_field_names()
    }

    #[cfg(test)]
    pub(crate) fn get_segment_count(&self) -> usize {
        let inner = self.inner.lock();
        inner.segment_infos.size()
    }

    #[cfg(test)]
    pub(crate) fn get_num_buffered_documents(&self) -> i32 {
        self.doc_writer.get_num_docs()
    }

    #[cfg(test)]
    pub(crate) fn max_doc(&self, i: i32) -> i32 {
        let inner = self.inner.lock();
        if i >= 0 && (i as usize) < inner.segment_infos.size() {
            inner
                .segment_infos
                .info_idx(i as usize)
                .expect("segment info not found")
                .info
                .max_doc()
                .expect("max doc failed")
        } else {
            -1
        }
    }

    #[cfg(test)]
    pub(crate) fn get_flush_count(&self) -> i32 {
        self.flush_count.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn get_flush_deletes_count(&self) -> i32 {
        self.flush_deletes_count.load(Ordering::Acquire)
    }
    #[cfg(test)]
    pub fn flush_count(&self) -> i32 {
        self.flush_count.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub fn flush_deletes_count(&self) -> i32 {
        self.flush_deletes_count.load(Ordering::Acquire)
    }

    /// Does the actual (time-consuming) work of the merge, but without holding synchronized lock on IndexWriter instance
    fn merge_middle(
        &self,
        merge: &mut OneMergeSR<D>,
        merge_policy: &L::MergePolicy,
    ) -> Result<i32> {
        let mut max_doc = -1;
        self.test_point("mergeMiddleStart");
        {
            let inner = self.inner.lock();
            merge.check_aborted(&inner.segment_infos)?;
        }

        let merge_directory = self
            .config
            .get_merge_scheduler()
            .wrap_for_merge(self.directory.clone())?;

        let context = IOContext::with_merge(merge.get_store_merge_info())?;

        let dir_wrapper = TrackingDirectoryWrapper::new(&merge_directory);
        let mut success = false;
        let res = (|| {
            merge.init_merge_readers(|sci_id: &String| -> Result<MergeReaderSR<D>> {
                let rld = {
                    let inner = self.inner.lock();
                    let sci = inner.segment_infos.info(sci_id).ok_or_else(|| {
                        LuceneError::illegal_state(format!(
                            "segment info with id={} not found",
                            sci_id
                        ))
                    })?;
                    let rld_opt = self.get_pooled_instance(sci, true, None)?;
                    match rld_opt {
                        Some(v) => v,
                        None => {
                            return Err(LuceneError::illegal_state(
                                "failed to get pooled instance for merge",
                            ));
                        },
                    }
                };
                rld.set_is_merging();
                let reader = {
                    let mut inner = self.inner.lock();
                    let sci = inner.segment_infos.info(sci_id).ok_or_else(|| {
                        LuceneError::illegal_state(format!(
                            "segment info with id={} not found",
                            sci_id
                        ))
                    })?;
                    let reader = rld.get_reader_for_merge(&context, sci, &inner.segment_infos)?;
                    inner
                        .deleter
                        .inc_ref_files(reader.reader.get_segment_info().files()?);
                    reader
                };

                Ok(reader)
            })?;
            // Let the merge wrap readers
            let mut merge_readers = Vec::new();
            let _soft_delete_count = SerialCounter::new();
            for merge_reader in merge.get_merge_reader().iter() {
                let reader = &merge_reader.reader;
                let wrapped_reader = merge.wrap_for_merge(reader.clone())?;
                self.validate_merge_reader(&wrapped_reader)?;
                if self.soft_deletes_enabled {
                    // TODO IMPORTANT softDeletesEnabled不支持
                    return Err(LuceneError::unsupported_operation(
                        "softDeletesEnabled is not supported yet",
                    ));
                }
                merge_readers.push(wrapped_reader);
            }

            // let mut reorder_doc_maps = None;
            // Don't reorder if an explicit sort is configured.
            let has_index_sort = self.config.get_index_sort().is_some();
            // Don't reorder if blocks can't be identified using the parent field.
            let has_blocks_but_no_parent_field = {
                let mut any_block = false;
                let mut any_parent_missing = false;

                for r in &merge_readers {
                    if r.get_metadata()?.get_has_blocks() {
                        any_block = true;
                    }

                    if r.get_field_infos()?.get_parent_field().is_none() {
                        any_parent_missing = true;
                    }

                    if any_block && any_parent_missing {
                        break;
                    }
                }
                any_block && any_parent_missing
            };

            let mut reorder_doc_maps = Vec::with_capacity(merge_readers.len());
            let new_merge_readers;
            if !has_index_sort && !has_blocks_but_no_parent_field {
                // Create a merged view of the input segments. This effectively does the merge.
                let merged_view = wrap(merge_readers.clone())?;

                let doc_map_opt = merge.reorder(&merged_view, self.directory.as_ref())?;

                if let Some(doc_map) = doc_map_opt {
                    let mut doc_base = 0;
                    for reader in &merge_readers {
                        let current_doc_base = doc_base;
                        let max_doc = reader.max_doc()?;

                        let dm = DocMapImpl1::new(doc_map.clone(), max_doc, current_doc_base);

                        reorder_doc_maps.push(dm);
                        doc_base += max_doc;
                    }

                    // This makes merging more expensive as it disables some bulk merging optimizations,
                    // so only do this if a non-null DocMap is returned.
                    let v = vec![CodecReaderEnum2::B(wrap_with_doc_map(
                        merged_view,
                        Some(doc_map),
                        None,
                    )?)];
                    new_merge_readers = v
                } else {
                    let mut v = Vec::with_capacity(merge_readers.len());
                    for cr in merge_readers.into_iter() {
                        v.push(CodecReaderEnum2::A(cr));
                    }
                    new_merge_readers = v;
                }
            } else {
                let mut v = Vec::with_capacity(merge_readers.len());
                for cr in merge_readers.into_iter() {
                    v.push(CodecReaderEnum2::A(cr));
                }
                new_merge_readers = v;
            }
            {
                let inner = self.inner.lock();
                merge.check_aborted(&inner.segment_infos)?;
            }
            let doc_maps = {
                let sci = merge.info.as_mut().unwrap();
                let segment_info = Arc::get_mut(&mut sci.info)
                    .ok_or_else(|| LuceneError::illegal_state("Arc not unique"))?;
                let mut merger = SegmentMerger::new(
                    &new_merge_readers,
                    segment_info,
                    self.info_stream.clone(),
                    &dir_wrapper,
                    self.global_field_number_map.clone(),
                    &context,
                )?;
                // TODO merge释放后才能设置
                // sci.set_soft_del_count(soft_delete_count.get() as i32)?;

                let doc_maps = if reorder_doc_maps.is_empty() {
                    let mut v = Vec::with_capacity(merger.merge_state.doc_maps.len());
                    for doc_map in merger.merge_state.doc_maps.iter() {
                        v.push(DocMapEnum2::A(doc_map.clone()))
                    }
                    v
                } else {
                    debug_assert!(merger.merge_state.doc_maps.len() == 1);
                    let compaction_doc_map = merger.merge_state.doc_maps[0].clone();
                    let len = reorder_doc_maps.len();
                    let mut v = Vec::with_capacity(len);
                    for rdm in reorder_doc_maps.into_iter() {
                        v.push(DocMapEnum2::B(DocMapIMpl2::new(
                            compaction_doc_map.clone(),
                            rdm,
                        )));
                    }
                    v
                };
                if merger.should_merge()? {
                    merger.merge()?;
                }
                merger.merge_state.segment_info.set_files(
                    dir_wrapper
                        .get_created_files()
                        .lock()
                        .created_filenames
                        .clone(),
                )?;
                if !merger.should_merge()? {
                    debug_assert!(merger.merge_state.segment_info.max_doc()? == 0);
                    success = self.commit_merge(merge, &doc_maps)?;
                    // TODO IMPORTANT
                    return Ok(0);
                }
                doc_maps
            };
            debug_assert!(merge.info.is_some());
            let sci = merge.info.as_mut().unwrap();
            max_doc = sci.info.max_doc()?;
            debug_assert!(max_doc > 0);

            // Very important to do this before opening the reader
            // because codec must know if prox was written for
            // this segment:
            let use_compound_file;
            {
                let inner = self.inner.lock();
                use_compound_file =
                    merge_policy.use_compound_file(&inner.segment_infos, sci, self)?;
            }
            if use_compound_file {
                success = false;
                let sci = merge.info.as_mut().unwrap();
                let files_to_remove = sci.files()?;

                // NOTE: Creation of the CFS file must be performed with the original
                // directory rather than with the merging directory, so that it is not
                // subject to merge throttling.
                let tracking_cfs_dir = TrackingDirectoryWrapper::new(self.directory.as_ref());

                // We'll need a mutable view of SegmentInfo to pass into create_compound_file.
                // Keep this in a tight scope.
                let cfs_res: Result<i32> = (|| {
                    let segment_info = Arc::get_mut(&mut sci.info)
                        .ok_or_else(|| LuceneError::illegal_state("Arc not unique"))?;

                    let delete_new_files = IOConsumerImpl1::new(self);

                    create_compound_file(
                        &self.info_stream,
                        &tracking_cfs_dir,
                        segment_info,
                        &context,
                        delete_new_files,
                    )?;

                    success = true;
                    Ok(0)
                })();
                if !success {
                    if self.info_stream.enabled("IW") {
                        self.info_stream
                            .message("IW", "hit exception creating compound file during merge");
                    }
                    // Safe: these files must exist
                    let files = sci.files()?;
                    self.delete_new_files(files.iter())?;
                }
                if let Err(e) = cfs_res {
                    let inner = self.inner.lock();
                    if merge.is_aborted() {
                        // This can happen if rollback is called while we were building
                        // our CFS -- fall through to logic below to remove the non-CFS
                        // merged files:
                        if self.info_stream.enabled("IW") {
                            self.info_stream.message(
                                "IW",
                                "hit merge abort exception creating compound file during merge",
                            );
                        }
                        return Ok(0);
                    } else {
                        drop(inner);
                        // TODO IMPORTANT
                        return Err(e);
                    }
                }

                // So that, if we hit exc in deleteNewFiles (next) or in commitMerge (later),
                // we close the per-segment readers in the final clause below:
                success = false;
                {
                    let _inner = self.inner.lock();
                    // delete new non cfs files directly: they were never
                    // registered with IFD
                    self.delete_new_files(files_to_remove.iter())?;
                    if merge.is_aborted() {
                        if self.info_stream.enabled("IW") {
                            self.info_stream
                                .message("IW", "abort merge after building CFS");
                        }
                        // Safe: these files must exist
                        let files = merge.info.as_ref().unwrap().files()?;
                        self.delete_new_files(files.iter())?;
                        return Ok(0);
                    }
                }

                {
                    let sci = merge.info.as_mut().unwrap();
                    let segment_info = Arc::get_mut(&mut sci.info)
                        .ok_or_else(|| LuceneError::illegal_state("Arc not unique"))?;
                    segment_info.set_use_compound_file(true);
                }
            } else {
                // So that, if we hit exc in commitMerge (later), we close the per-segment readers in the
                // final clause below:
                success = false;
            }
            // Have codec write SegmentInfo.  Must do this after
            // creating CFS so that 1) .si isn't slurped into CFS,
            // and 2) .si reflects useCompoundFile=true change
            // above:
            let mut success2 = false;
            let sci = merge.info.as_mut().unwrap();
            {
                let write_res: Result<()> = (|| {
                    let segment_info = Arc::get_mut(&mut sci.info)
                        .ok_or_else(|| LuceneError::illegal_state("Arc not unique"))?;

                    get_default_code().segment_info_format().write(
                        self.directory.as_ref(),
                        segment_info,
                        &context,
                    )?;

                    success2 = true;
                    Ok(())
                })();

                if !success2 {
                    // Safe: these files must exist
                    let files = sci.files()?;
                    self.delete_new_files(files.iter())?;
                }
                write_res?;
            }
            // TODO IMPORTANT IndexReaderWarmer not supported

            if !self.commit_merge(merge, &doc_maps)? {
                // commitMerge will return false if this merge was
                // aborted
                return Ok(0);
            }
            success = true;
            Ok(0)
        })();
        if !success {
            self.close_merge_readers(merge, true, false)?;
        }
        res?;
        Ok(max_doc)
    }

    pub(crate) fn new_segment_name(&self, inner: Option<&mut Inner<D>>) -> String {
        let inner = match inner {
            Some(i) => i,
            None => &mut *self.inner.lock(),
        };

        // Important to increment change_count so that segment_infos
        // is written on close. Otherwise, we could close, re-open,
        // and re-return the same segment name which can cause
        // problems at least with ConcurrentMergeScheduler.
        inner.change_count += 1;
        inner.segment_infos.changed();

        let counter = inner.segment_infos.counter;
        inner.segment_infos.counter += 1;
        let s = BigInt::from(counter).to_str_radix(36);
        format!("_{}", s)
    }
    /// Forces the merge policy to merge segments until there are `<= max_num_segments`.
    /// The actual merges to be executed are determined by the [`MergePolicy`].
    ///
    /// This is a **very costly** operation, especially when you pass a small
    /// `max_num_segments`; usually you should only call this if the index is static
    /// (will no longer be changed).
    ///
    /// Note that this requires free space that is proportional to the size of the
    /// index in your `Directory`: **2×** if you are not using compound file format,
    /// and **3×** if you are. For example, if your index size is 10 MB then you need
    /// an additional 20 MB free for this to complete (30 MB if you're using compound
    /// file format). This is also affected by the [`Codec`] that is used to execute
    /// the merge, and may result in an even larger index. It is also recommended to
    /// call [`IndexWriter::commit`] afterwards, to allow the writer to free up disk
    /// space.
    ///
    /// If some but not all readers are reopened while merging is underway, this may
    /// cause **more than 2×** temporary space to be consumed, since those new readers
    /// will hold open the temporary segments at that time. It is best not to reopen
    /// readers while merging is running.
    ///
    /// The actual temporary usage could be much less than these figures; it depends
    /// on many factors.
    ///
    /// In general, once this completes, the total size of the index will be less
    /// than the size of the starting index. It may be significantly smaller (if
    /// there were many pending deletes) or only slightly smaller.
    ///
    /// If an exception occurs, for example due to running out of disk space, the
    /// index will not be corrupted and no documents will be lost. However, the index
    /// may have been partially merged (some segments were merged but not all), and it
    /// is possible that one of the segments will be left in non-compound format even
    /// when compound file format is enabled. This can happen if the exception occurs
    /// while converting a segment into compound format.
    ///
    /// This call merges only the segments that were present in the index when the
    /// call started. If other threads continue to add documents and flush new
    /// segments, those newly created segments will **not** be merged unless
    /// `force_merge` is called again.
    ///
    /// # Parameters
    ///
    /// * `max_num_segments` — maximum number of segments left in the index after
    ///   merging finishes.
    ///
    /// # Errors
    ///
    /// Returns an error if the index is corrupt or if a low-level I/O error occurs.
    ///
    /// See [`MergePolicy::find_merges`].
    pub fn force_merge(&self, max_num_segments: i32) -> Result<()> {
        self.force_merge_with_wait(max_num_segments, true)
    }
    /// Just like [`IndexWriter::force_merge`], except you can specify whether the call
    /// should block until all merging completes.
    ///
    /// This is only meaningful with a [`MergeScheduler`] that is able to run merges
    /// in background threads.
    pub fn force_merge_with_wait(&self, max_num_segments: i32, do_wait: bool) -> Result<()> {
        self.ensure_open()?;

        if max_num_segments < 1 {
            return Err(LuceneError::illegal_argument(format!(
                "maxNumSegments must be >= 1; got {}",
                max_num_segments
            )));
        }

        if self.info_stream.enabled("IW") {
            self.info_stream.message(
                "IW",
                &format!("forceMerge: index now {}", self.seg_string()?),
            );
            self.info_stream.message("IW", "now flush at forceMerge");
        }

        self.flush_with_apply_merge_deletes(true, true)?;

        {
            let mut inner = self.inner.lock();

            self.reset_merge_exceptions(&mut inner);
            inner.segments_to_merge.clear();
            {
                let Inner {
                    segment_infos,
                    segments_to_merge,
                    ..
                } = &mut *inner;
                for info in segment_infos.iter() {
                    segments_to_merge.insert(info.info.get_id_str(), Some(true));
                }
            }

            inner.merge_max_num_segments = max_num_segments;

            // Now mark all pending & running merges for forced
            // merge:
            let Inner {
                pending_merges,
                segments_to_merge,
                running_merges,
                ..
            } = &mut *inner;
            for merge in pending_merges.iter_mut() {
                merge.stat.max_num_segments = max_num_segments;
                if let Some(info_id) = &merge.stat.info_id {
                    // this can be null since we register the merge under lock before we then do the actual
                    // merge and
                    // set the merge.info in _mergeInit
                    segments_to_merge.insert(info_id.clone(), Some(true));
                }
            }

            let new_running_merges: HashSet<MergeStat> = running_merges
                .iter()
                .map(|m| {
                    let mut m = m.clone();
                    m.max_num_segments = max_num_segments;
                    if let Some(ref id) = m.info_id {
                        // this can be null since we put the merge on runningMerges before we do the actual merge
                        // and set the merge.info in _mergeInit
                        segments_to_merge.insert(id.clone(), Some(true));
                    }
                    m
                })
                .collect();
            inner.running_merges = new_running_merges;
        }

        self.maybe_merge_with_max_num_segments(
            self.config.get_merge_policy(),
            MergeTrigger::Explicit,
            max_num_segments,
        )?;

        if do_wait {
            let mut inner = self.inner.lock();
            loop {
                if let Some(t) = &*self.tragedy.lock() {
                    return Err(LuceneError::illegal_state(format!(
                        "this writer hit an unrecoverable error; cannot complete forceMerge {}",
                        t
                    )));
                }

                if !inner.merge_exceptions.is_empty() {
                    for merge in &inner.merge_exceptions {
                        if merge.max_num_segments != UNBOUNDED_MAX_MERGE_SEGMENTS {
                            return Err(LuceneError::illegal_state(
                                "background merge hit exception",
                            ));
                        }
                    }
                }

                if self.max_num_segments_merges_pending(&inner) {
                    self.test_point("forceMergeBeforeWait");
                    self.do_wait(&mut inner);
                } else {
                    break;
                }
            }

            // If close is called while we are still
            // running, throw an exception so the calling
            // thread will know merging did not
            // complete
            self.ensure_open()?;
        }
        // NOTE: in the ConcurrentMergeScheduler case, when
        // doWait is false, we can return immediately while
        // background threads accomplish the merging
        Ok(())
    }

    /// Returns true if any merges in `pending_merges` or `running_merges`
    /// are max-num-segments merges.
    pub(crate) fn max_num_segments_merges_pending(&self, inner: &Inner<D>) -> bool {
        for merge in inner.pending_merges.iter() {
            if merge.stat.max_num_segments != UNBOUNDED_MAX_MERGE_SEGMENTS {
                return true;
            }
        }

        for merge in inner.running_merges.iter() {
            if merge.max_num_segments != UNBOUNDED_MAX_MERGE_SEGMENTS {
                return true;
            }
        }

        false
    }
    /// **Expert:** Asks the [`MergePolicy`] whether any merges are necessary now and, if so,
    /// runs the requested merges and then iterates (re-checking whether merges are needed)
    /// until no more merges are returned by the merge policy.
    ///
    /// Explicit calls to `maybe_merge()` are usually not necessary. The most common case
    /// is when merge policy parameters have changed.
    ///
    /// This method will call the [`MergePolicy`] with [`MergeTrigger::Explicit`].
    pub fn maybe_merge(&self) -> Result<()> {
        self.maybe_merge_with_max_num_segments(
            self.config.get_merge_policy(),
            MergeTrigger::Explicit,
            UNBOUNDED_MAX_MERGE_SEGMENTS,
        )
    }

    fn maybe_merge_with_max_num_segments(
        &self,
        _merge_policy: &L::MergePolicy,
        _trigger: MergeTrigger,
        _max_num_segments: i32,
    ) -> Result<()> {
        // TODO merge过程还未调试完成
        // self.do_ensure_open(false)?;
        // if self.update_pending_merges(merge_policy, trigger, max_num_segments, None)? {
        //     self.execute_merge(trigger)?;
        // }
        Ok(())
    }

    pub(crate) fn execute_merge(&self, trigger: MergeTrigger) -> Result<()> {
        self.config
            .get_merge_scheduler()
            .merge(&self.merge_source, trigger, self)
    }
    fn update_pending_merges(
        &self,
        merge_policy: &L::MergePolicy,
        trigger: MergeTrigger,
        max_num_segments: i32,
        inner: Option<&mut Inner<D>>,
    ) -> Result<bool> {
        // In case infoStream was disabled on init, but then enabled at some
        // point, try again to log the config here:
        let inner = match inner {
            Some(i) => i,
            None => &mut *self.inner.lock(),
        };
        self.message_state()?;

        debug_assert!(max_num_segments == UNBOUNDED_MAX_MERGE_SEGMENTS || max_num_segments > 0);

        if !inner.merges.are_enabled(inner) {
            return Ok(false);
        }

        // Do not start new merges if disaster struck
        if self.tragedy.lock().is_some() {
            return Ok(false);
        }

        let caching_merge_context = CachingMergeContext::new(self);
        let mut spec_opt: Option<MergeSpecificationNoReader<D>>;

        if max_num_segments != UNBOUNDED_MAX_MERGE_SEGMENTS {
            debug_assert!(
                matches!(
                    trigger,
                    MergeTrigger::Explicit | MergeTrigger::MergeFinished
                ),
                "Expected EXPLICT or MERGE_FINISHED as trigger even with maxNumSegments set but was: {:?}",
                trigger
            );

            spec_opt = merge_policy.find_forced_merges(
                &inner.segment_infos,
                max_num_segments,
                &inner.segments_to_merge,
                Some(inner),
                &caching_merge_context,
            )?;

            if let Some(ref mut spec) = spec_opt {
                for m in &mut spec.merges {
                    m.stat.max_num_segments = max_num_segments;
                }
            }
        } else {
            spec_opt = match trigger {
                MergeTrigger::GetReader | MergeTrigger::Commit => merge_policy
                    .find_full_flush_merges(
                        trigger,
                        &inner.segment_infos,
                        Some(inner),
                        &caching_merge_context,
                    )?,
                MergeTrigger::AddIndexes => {
                    return Err(LuceneError::illegal_state(
                        "Merges with ADD_INDEXES trigger should be called from within the addIndexes() API flow",
                    ));
                },
                _ => merge_policy.find_merges(
                    trigger,
                    &inner.segment_infos,
                    Some(inner),
                    &caching_merge_context,
                )?,
            };
        }

        match spec_opt {
            Some(spec) => {
                for m in spec.merges.into_iter() {
                    self.register_merge(m, inner)?;
                }
                Ok(true)
            },
            _ => Ok(false),
        }
    }
    /// **Expert:** the [`MergeScheduler`] calls this method to retrieve the next merge
    /// requested by the [`MergePolicy`].
    fn get_next_merge(&self) -> Result<Option<OneMergeSR<D>>> {
        let mut inner = self.inner.lock();

        if let Some(t) = &*self.tragedy.lock() {
            return Err(LuceneError::illegal_state(format!(
                "this writer hit an unrecoverable error; cannot merge: {}",
                t
            )));
        }
        match inner.pending_merges.pop_front() {
            Some(merge) => {
                inner.running_merges.insert(merge.stat.clone());
                Ok(Some(merge))
            },
            None => Ok(None),
        }
    }

    /// **Expert:** returns `true` if there are merges waiting to be scheduled.
    pub fn has_pending_merges(&self) -> Result<bool> {
        let inner = self.inner.lock();

        if let Some(t) = &*self.tragedy.lock() {
            return Err(LuceneError::illegal_state(format!(
                "this writer hit an unrecoverable error; cannot merge: {}",
                t
            )));
        }

        Ok(!inner.pending_merges.is_empty())
    }

    pub fn delete_all(&self) -> Result<i64> {
        // TODDO: 未实现
        self.global_field_number_map.lock().clear();
        Ok(0)
    }
    /// Waits for any currently outstanding merges to finish.
    ///
    /// It is guaranteed that any merges started prior to calling this method
    /// will have completed once this method returns.
    pub(crate) fn wait_for_merges(&self) -> Result<()> {
        // TODO: 合并逻辑还未实现
        // self.merge_scheduler
        //     .merge(&self.merge_source, MergeTrigger::Closing)?;
        let inner = self.inner.lock();
        self.do_ensure_open(false)?;
        if self.info_stream.enabled("IW") {
            self.info_stream.message("IW", "waitForMerges");
        }
        // while !inner.pending_merges.is_empty() || !inner.running_merges.is_empty() {
        //     self.do_wait(&mut inner);
        // }
        debug_assert!(
            inner.merging_segments.is_empty(),
            "mergingSegments should be empty here"
        );
        if self.info_stream.enabled("IW") {
            self.info_stream.message("IW", "waitForMerges done");
        }

        Ok(())
    }

    fn checkpoint(&self, inner: &mut Inner<D>) -> Result<()> {
        Self::changed(&mut inner.change_count, &mut inner.segment_infos);
        let (deleter, segment_infos) = {
            let v = &mut *inner;
            (&mut v.deleter, &v.segment_infos)
        };
        deleter.checkpoint(
            segment_infos,
            false,
            self.config.get_index_deletion_policy(),
        )?;
        Ok(())
    }
    /// Checkpoints with IndexFileDeleter, so it's aware of new files, and increments changeCount,
    /// so on close/commit we will write a new segments file, but does NOT bump segmentInfos.version.
    fn check_point_no_sis(&self, inner: &mut Inner<D>) -> Result<()> {
        inner.change_count += 1;
        let (deleter, segment_infos) = {
            let v = &mut *inner;
            (&mut v.deleter, &v.segment_infos)
        };
        deleter.checkpoint(
            segment_infos,
            false,
            self.config.get_index_deletion_policy(),
        )?;
        Ok(())
    }

    /// Called internally if any index state has changed.
    fn changed(change_count: &mut i64, segment_infos: &mut SegmentInfos<D>) {
        *change_count += 1;
        segment_infos.changed()
    }
    fn publish_frozen_updates(
        &self,
        packet: FrozenBufferedUpdates,
        inner: Option<&Inner<D>>,
    ) -> Result<i64> {
        let _guard = match inner {
            Some(i) => i,
            None => &self.inner.lock(),
        };
        debug_assert!(packet.any());
        let (next_gen, packet) = self.buffered_updates_stream.push(packet);
        // Do this as an event so it applies higher in the stack when we are not holding
        // DocumentsWriterFlushQueue.purgeLock:
        let event: EventEnum = EventEnum::E(EventImpl5::new(packet));
        self.event_queue.add(event)?;
        Ok(next_gen)
    }
    /// Atomically adds the segment private delete packet and publishes the flushed segments SegmentInfo to the index writer.
    fn publish_flushed_segment(
        &self,
        mut new_segment: SegmentCommitInfo<D>,
        field_infos: Arc<FieldInfos>,
        packet: Option<FrozenBufferedUpdates>,
        global_packet: Option<FrozenBufferedUpdates>,
        sort_map: Option<Arc<DocMapImpl>>,
    ) -> Result<()> {
        let mut inner = self.inner.lock();
        let mut published = false;
        let max_doc = new_segment.info.max_doc()?;
        let res: Result<()> = (|| {
            // Lock order IW -> BDS
            self.do_ensure_open(false)?;

            if self.info_stream.enabled("IW") {
                self.info_stream
                    .message("IW", &format!("publishFlushedSegment {}", new_segment));
            }

            if let Some(gp) = global_packet
                && gp.any()
            {
                let _ = self.publish_frozen_updates(gp, Some(&inner))?;
            }
            // Publishing the segment must be sync'd on IW -> BDS to make the sure
            // that no merge prunes away the seg. private delete packet
            let packet_any = match packet {
                Some(ref p) => p.any(),
                None => false,
            };
            let next_gen = if packet_any {
                self.publish_frozen_updates(packet.unwrap(), Some(&inner))?
            } else {
                // Since we don't have a delete packet to apply we can get a new
                // generation right away
                let v = self.buffered_updates_stream.get_next_gen();
                // No deletes/updates here, so marked finished immediately:
                self.buffered_updates_stream.finished_segment(v);
                v
            };

            if self.info_stream.enabled("IW") {
                let segs = self.seg_string_from_info(&new_segment)?;
                self.info_stream.message(
                    "IW",
                    &format!("publish sets newSegment delGen={} seg={}", next_gen, segs),
                );
            }
            new_segment.set_buffered_deletes_gen(next_gen)?;
            let new_segment_id = new_segment.info.get_id_str();
            inner.segment_infos.add(new_segment)?;
            published = true;
            self.checkpoint(&mut *inner)?;
            let new_segment = inner.segment_infos.info(&new_segment_id).unwrap();
            if packet_any {
                let _ = self.get_pooled_instance(new_segment, true, sort_map)?;
            }
            // this is a corner case where documents delete them-self with soft deletes. This is used to
            // build delete tombstones etc. in this case we haven't seen any updates to the DV in this
            // fresh flushed segment.
            // if we have seen updates the update code checks if the segment is fully deleted.
            let has_initial_soft_deleted = {
                if let Some(name) = self.config.get_soft_deletes_field() {
                    if let Some(fi) = field_infos.field_info_by_name(name) {
                        fi.get_doc_values_gen() == -1
                            && *fi.get_doc_values_type() != DocValuesType::None
                    } else {
                        false
                    }
                } else {
                    false
                }
            };
            let is_fully_hard_deleted =
                new_segment.get_del_count() == new_segment.info.max_doc()?;
            // we either have a fully hard-deleted segment or one or more docs are soft-deleted. In both
            // cases we need
            // to go and check if they are fully deleted. This has the nice side effect that we now have
            // accurate numbers
            // for the soft delete right after we flushed to disk.
            if has_initial_soft_deleted || is_fully_hard_deleted {
                let rld = self.get_pooled_instance(new_segment, true, None)?;
                let result: Result<()> = (|| {
                    match rld {
                        None => {
                            return Err(LuceneError::illegal_state(
                                "failed to open newly flushed segment",
                            ));
                        },
                        Some(ref rld) => {
                            let new_segment = inner.segment_infos.info(&new_segment_id).unwrap();
                            let is_fully_deleted = self.is_fully_deleted(rld, new_segment)?;
                            if is_fully_deleted {
                                self.drop_deleted_segment(&new_segment_id, &mut *inner)?;
                                self.checkpoint(&mut *inner)?;
                            }
                        },
                    }
                    Ok(())
                })();
                self.release(&rld.unwrap(), &mut *inner)?;
                result?;
            }
            Ok(())
        })();

        if !published {
            self.adjust_pending_num_docs(-(max_doc as i64));
        }
        self.flush_count.fetch_add(1, Ordering::AcqRel);
        if let Some(ref s) = self.sub {
            s.do_after_flush()?
        }

        res
    }
    fn reset_merge_exceptions(&self, inner: &mut MutexGuard<'_, Inner<D>>) {
        inner.merge_exceptions.clear();
        inner.merge_gen += 1;
    }
    pub(crate) fn validate_merge_reader<CR>(&self, leaf: &CR) -> Result<()>
    where
        CR: CodecReader,
    {
        let segment_meta = leaf.get_metadata()?;
        let index_created_version_major = self
            .inner
            .lock()
            .segment_infos
            .get_index_created_version_major();

        if index_created_version_major != segment_meta.get_created_version_major() {
            return Err(LuceneError::illegal_argument(format!(
                "Cannot merge a segment that has been created with major version {} \
             into this index which has been created by major version {}",
                segment_meta.get_created_version_major(),
                index_created_version_major
            )));
        }

        if index_created_version_major >= 7 && segment_meta.get_min_version().is_none() {
            return Err(LuceneError::illegal_state(format!(
                "Indexes created on or after Lucene 7 must record the created version major, \
             but {} hides it",
                leaf
            )));
        }

        let leaf_index_sort = segment_meta.get_sort();
        if let Some(index_sort) = self.config.get_index_sort()
            && leaf_index_sort
                .as_ref()
                .map(|s| !is_congruent_sort(&index_sort, s))
                .unwrap_or(true)
        {
            return Err(LuceneError::illegal_argument(format!(
                "cannot change index sort from {} to {}",
                leaf_index_sort.as_ref().unwrap(),
                index_sort
            )));
        }

        Ok(())
    }

    /// **Expert:** Prepares for commit. This is the first phase of a 2-phase commit.
    /// This method performs all steps necessary to commit changes since this writer was opened:
    /// flushes pending added and deleted docs, syncs the index files, and writes most of the next
    /// `segments_N` file. After calling this you must then call either [`commit()`](Self::commit) to finish the commit,
    /// or [`rollback()`](Self::rollback) to revert the commit and undo all changes made since the writer was opened.
    ///
    /// You can also call [`commit()`](Self::commit) directly without calling `prepare_commit` first, in which case
    /// that method will internally call `prepare_commit`.
    ///
    /// # Returns
    /// The `sequence number` of the last operation in the commit.
    /// All sequence numbers `<=` this value will be reflected in the commit, and all others will not.
    pub(crate) fn prepare_commit(&self) -> Result<i64> {
        self.do_ensure_open(false)?;
        self.pending_seq_no
            .store(self.prepare_commit_internal(None)?, Ordering::Release);
        // we must do this outside of the commitLock else we can deadlock:
        if self.maybe_merge.swap(false, Ordering::AcqRel) {
            self.maybe_merge_with_max_num_segments(
                self.config.get_merge_policy(),
                MergeTrigger::FullFlush,
                UNBOUNDED_MAX_MERGE_SEGMENTS,
            )?;
        }
        Ok(self.pending_seq_no.load(Ordering::Acquire))
    }

    fn prepare_commit_internal(&self, commit_lock: Option<&mut CommitInner<D>>) -> Result<i64> {
        let commit_lock = match commit_lock {
            Some(lock) => lock,
            None => &mut *self.commit_lock.lock(),
        };
        commit_lock.start_commit_time = Instant::now();

        self.do_ensure_open(false)?;
        if self.info_stream.enabled("IW") {
            self.info_stream.message("IW", "prepareCommit: flush");
            self.info_stream.message(
                "IW",
                &format!("  index before flush {}", self.seg_string()?),
            );
        }

        if let Some(t) = &*self.tragedy.lock() {
            return Err(LuceneError::illegal_state(format!(
                "this writer hit an unrecoverable error; cannot commit {}",
                t
            )));
        }

        if commit_lock.pending_commit.is_some() {
            return Err(LuceneError::illegal_state(
                "prepareCommit was already called with no corresponding call to commit",
            ));
        }

        if let Some(ref s) = self.sub {
            s.do_before_flush()?
        }
        self.test_point("startDoFlush");

        // locals (to be filled by the next parts)
        let mut to_commit = None;
        let mut any_changes = false;
        let mut seq_no: i64 = 0;
        // let mut point_in_time_merges: Option<MergeSpecification<D>> = None;
        // let stop_adding_merged_segments = AtomicBool::new(false);
        let max_commit_merge_wait_millis = self.config.get_max_full_flush_merge_wait_millis();
        // This is copied from doFlush, except it's modified to
        // clone & incRef the flushed SegmentInfos inside the
        // sync block:
        let tragic_res: Result<()> = (|| {
            let _guard = self.full_flush_lock.lock();
            let mut flush_success = false;
            let body_res: Result<()> = (|| {
                seq_no = self.doc_writer.flush_all_threads(self, &self.config)?;
                if seq_no < 0 {
                    any_changes = true;
                    seq_no = -seq_no;
                }
                if !any_changes {
                    // prevent double increment since docWriter#doFlush increments the flushcount
                    // if we flushed anything.
                    self.flush_count.fetch_add(1, Ordering::AcqRel);
                }

                self.publish_flushed_segments(true)?;
                // cannot pass triggerMerges=true here else it can lead to deadlock:
                self.process_events(false)?;

                flush_success = true;

                self.apply_all_deletes_and_updates()?;

                {
                    let mut inner = self.inner.lock();
                    self.write_reader_pool(true, &mut *inner)?;
                    if inner.change_count != self.last_commit_change_count.load(Ordering::Acquire) {
                        // There are changes to commit, so we will write a new segments_N in startCommit.
                        // The act of committing is itself an NRT-visible change (an NRT reader that was
                        // just opened before this should see it on reopen) so we increment changeCount
                        // and segments version so a future NRT reopen will see the change:
                        inner.change_count += 1;
                        inner.segment_infos.changed();
                    }
                    if let Some(commit_ud) = &inner.commit_user_data {
                        let v = commit_ud.clone();
                        inner.segment_infos.set_user_data(Some(v), false);
                    }
                    // Must clone the segmentInfos while we still
                    // hold fullFlushLock and while sync'd so that
                    // no partial changes (eg a delete w/o
                    // corresponding to add from an updateDocument) can
                    // sneak into the commit point:
                    to_commit = Some(inner.segment_infos.try_clone()?);
                    self.pending_commit_change_count
                        .store(inner.change_count, Ordering::Release);
                    // This protects the segmentInfos we are now going
                    // to commit.  This is important in case, eg, while
                    // we are trying to sync all referenced files, a
                    // merge completes which would otherwise have
                    // removed the files we are now syncing.
                    inner
                        .deleter
                        .inc_ref_files(to_commit.as_ref().unwrap().files(false)?);

                    if max_commit_merge_wait_millis > 0 {
                        // TODO: 合并为实现
                    }
                }
                Ok(())
            })();
            if body_res.is_err() && self.info_stream.enabled("IW") {
                self.info_stream
                    .message("IW", "hit exception during prepareCommit");
            }
            // Done: finish the full flush!
            self.doc_writer
                .finish_full_flush(flush_success, &self.config)?;
            if let Some(ref s) = self.sub {
                s.do_after_flush()?
            }
            body_res
        })();
        let tragic_res = match tragic_res {
            Err(e) => {
                // TODO IMPORTANT 这里没有处理好嵌套错误
                self.tragic_event(&e, "prepareCommit");
                Err(e)
            },
            Ok(()) => Ok(()),
        };
        self.maybe_close_on_tragic_event()?;
        tragic_res?;
        // TODO: 这里pointInTimeMerges没有实现

        // do this after handling any pointInTimeMerges since the files will have changed if any
        // merges
        // did complete
        commit_lock.files_to_commit = Some(
            to_commit
                .as_ref()
                .unwrap()
                .files(false)?
                .into_iter()
                .collect(),
        );
        let ret = (|| -> Result<i64> {
            if any_changes {
                self.maybe_merge.store(true, Ordering::Release);
            }
            self.start_commit(to_commit, commit_lock)?;
            if commit_lock.pending_commit.is_none() {
                Ok(-1)
            } else {
                Ok(seq_no)
            }
        })();
        match ret {
            Ok(v) => Ok(v),
            Err(t) => {
                let mut inner = self.inner.lock();
                match std::mem::take(&mut commit_lock.files_to_commit) {
                    Some(files_to_commit) => {
                        match inner.deleter.dec_ref(files_to_commit.iter()) {
                            Ok(()) => Err(t),
                            Err(e) => {
                                // TODO IMPORTANT 这里没有处理好嵌套错误
                                Err(LuceneError::illegal_state(format!("{}, {}", t, e)))
                            },
                        }
                    },
                    None => Err(t),
                }
            },
        }
    }
    /// Ensures that all changes in the reader pool are written to disk.
    ///
    /// # Arguments
    ///
    /// * `write_deletes` — if `true`, deletes should also be written to disk.
    pub(crate) fn write_reader_pool(
        &self,
        write_deletes: bool,
        inner: &mut Inner<D>,
    ) -> Result<()> {
        if write_deletes {
            if self.reader_pool.commit(
                &mut inner.segment_infos,
                &self.global_field_number_map.lock(),
            )? {
                self.check_point_no_sis(inner)?;
            }
        } else {
            // only write the docValues
            if self.reader_pool.write_all_doc_values_updates(
                &mut inner.segment_infos.segments,
                &self.global_field_number_map.lock(),
            )? {
                self.checkpoint(inner)?;
            }
        }
        // now do some best effort to check if a segment is fully deleted
        let mut to_drop = Vec::new();

        for info in inner.segment_infos.segments.values() {
            if let Some(rld) = self.reader_pool.get(info, false, None)?
                && rld.is_fully_deleted(info)?
            {
                to_drop.push(info.info.get_id_str());
            }
        }

        for seg_id in &to_drop {
            self.drop_deleted_segment(seg_id, inner)?;
        }
        if !to_drop.is_empty() {
            self.checkpoint(inner)?;
        }

        Ok(())
    }
    /// Sets the iterator that provides the commit user data map at commit time.
    ///
    /// Calling this method is considered a **committable change** and will be
    /// [`commit`](Self::commit) committed even if there are no other changes in this writer.
    /// Note that you must call this method **before** [`prepare_commit`](Self::prepare_commit).
    /// Otherwise it will not be included in the subsequent [`co,mmit`](Self::commit).
    ///
    ///
    /// **NOTE:**
    /// The iterator is *late-binding*: it is only consumed **after** all documents for the
    /// commit have been written to their segments, and **before** the next `segments_N` file
    /// is written.
    pub fn set_live_commit_data<I>(&self, commit_user_data: I)
    where
        I: IntoIterator<Item = (String, String)>,
    {
        self.set_live_commit_data_with_version(commit_user_data, true);
    }
    /// Sets the commit user data iterator, controlling whether to advance the
    /// [`SegmentInfos::get_version`].
    pub fn set_live_commit_data_with_version<I>(
        &self,
        commit_user_data: I,
        do_increment_version: bool,
    ) where
        I: IntoIterator<Item = (String, String)>,
    {
        let mut inner = self.inner.lock();

        inner.commit_user_data = Some(commit_user_data.into_iter().collect());
        if do_increment_version {
            inner.segment_infos.changed();
        }
        inner.change_count += 1;
    }
    pub(crate) fn write_some_doc_values_updates(&self) -> Result<()> {
        if let Some(_guard) = self.write_doc_values_lock.try_lock() {
            let ram_buffer_size_mb = self.config.get_ram_buffer_size_mb();
            // If the reader pool is > 50% of our IW buffer, then write the updates:
            if ram_buffer_size_mb != DISABLE_AUTO_FLUSH as f64 {
                let start_ns = Instant::now();
                let mut ram_bytes_used = self.reader_pool.ram_bytes_used();
                let limit = (0.5 * ram_buffer_size_mb * 1024.0 * 1024.0) as i64;

                if ram_bytes_used > limit {
                    if self.info_stream.enabled("BD") {
                        self.info_stream.message(
                            "BD",
                            &format!(
                                "now write some pending DV updates: {:.2} MB used vs IWC Buffer {:.2} MB",
                                ram_bytes_used as f64 / 1024.0 / 1024.0,
                                ram_buffer_size_mb
                            ),
                        );
                    }
                    // Sort by largest ramBytesUsed:
                    let readers = self.reader_pool.get_readers_by_ram();
                    let mut count = 0;

                    for rld in readers {
                        if ram_bytes_used <= limit {
                            break;
                        }
                        // We need to do before/after because not all RAM in this RAU is used by DV updates,
                        // and
                        // not all of those bytes can be written here:
                        let bytes_used_before = rld.ram_bytes_used.load(Ordering::SeqCst);
                        if bytes_used_before == 0 {
                            continue; // nothing to do here - lets not acquire the lock
                        }
                        // Only acquire IW lock on each write, since this is a time-consuming operation.  This
                        // way
                        // other threads get a chance to run in between our writes.
                        {
                            // It's possible that the segment of a reader returned by readerPool#getReadersByRam
                            // is dropped before being processed here. If it happens, we need to skip that
                            // reader.
                            // this is also best effort to free ram, there might be some other thread writing
                            // this rld concurrently
                            // which wins and then if readerPooling is off this rld will be dropped.
                            let mut inner = self.inner.lock();
                            let info = match inner.segment_infos.info_mut(&rld.info_id) {
                                Some(info) => info,
                                None => Err(LuceneError::illegal_state(
                                    "could not find segment info from IndexWriter#segment_infos",
                                ))?,
                            };
                            if self.reader_pool.get(info, false, None)?.is_none() {
                                continue;
                            }

                            if rld.write_field_updates(
                                &self.directory,
                                &self.global_field_number_map.lock(),
                                self.buffered_updates_stream.get_completed_del_gen(),
                                self.info_stream.as_ref(),
                                info,
                            )? {
                                self.check_point_no_sis(&mut inner)?;
                            }
                        }

                        let bytes_used_after = rld.ram_bytes_used.load(Ordering::SeqCst);
                        ram_bytes_used -= bytes_used_before - bytes_used_after;
                        count += 1;
                    }

                    if self.info_stream.enabled("BD") {
                        self.info_stream.message(
                            "BD",
                            &format!(
                                "done write some DV updates for {} segments: now {:.2} MB used vs IWC Buffer {:.2} MB; took {:.2} sec",
                                count,
                                self.reader_pool.ram_bytes_used() as f64 / 1024.0 / 1024.0,
                                ram_buffer_size_mb,
                                start_ns.elapsed().as_secs_f64(),
                            ),
                        );
                    }
                }
            }
            drop(_guard)
        }
        Ok(())
    }
    pub fn num_deleted_docs(&self, info: &SegmentCommitInfo<D>) -> Result<i32> {
        self.do_ensure_open(false)?;
        self.validate(info)?;
        if let Some(rld) = self.get_pooled_instance(info, false, None)? {
            Ok(rld.get_del_count(info)) // get the full count from here since SCI might change concurrently
        } else {
            let del_count = info.get_del_count_with_soft_deletes(self.soft_deletes_enabled);
            debug_assert!(
                del_count <= info.info.max_doc()?,
                "delCount: {} maxDoc: {}",
                del_count,
                info.info.max_doc()?
            );
            Ok(del_count)
        }
    }

    pub(crate) fn do_ensure_open(&self, fail_if_closing: bool) -> Result<()> {
        if self.closed.load(Ordering::SeqCst)
            || (fail_if_closing && self.closing.load(Ordering::SeqCst))
        {
            let tragedy = self.tragedy.lock();
            let error_opt = tragedy.as_ref();
            match error_opt {
                Some(err) => Err(LuceneError::already_closed(format!("{err}"))),
                None => Err(LuceneError::illegal_state("no tragic error set")),
            }
        } else {
            Ok(())
        }
    }
    pub(crate) fn ensure_open(&self) -> Result<()> {
        self.do_ensure_open(true)
    }
    /// Commits all pending changes (added and deleted documents, segment merges, added indexes, etc.)
    /// to the index, and syncs all referenced index files, such that a reader will see the changes and
    /// the index updates will survive an OS or machine crash or power loss.
    /// Note that this does not wait for any running background merges to finish.
    /// This may be a costly operation, so you should test the cost in your application and do it only when necessary.
    ///
    /// This operation calls `Directory::sync` on the index files. That call should not return until the
    /// file contents and metadata are on stable storage. For `FSDirectory`, this calls the OS’s `fsync`.
    /// However, beware: some hardware devices may cache writes even during `fsync` and return before the
    /// bits are actually on stable storage, to give the appearance of faster performance.
    /// If you have such a device, and it does not have a battery backup (for example), then on power loss
    /// it may still lose data. Lucene cannot guarantee consistency on such devices.
    ///
    /// If nothing was committed, because there were no pending changes, this returns `-1`. Otherwise,
    /// it returns the sequence number such that all indexing operations prior to this sequence will be
    /// included in the commit point, and all other operations will not.
    ///
    /// # See also
    /// [`prepare_commit`](Self::prepare_commit)
    ///
    /// # Returns
    /// The `sequence number` of the last operation in the commit.
    /// All sequence numbers `<=` this value will be reflected in the commit, and all others will not.
    pub fn commit(&self) -> Result<i64> {
        self.ensure_open()?;
        self.commit_internal(self.config.get_merge_policy())
    }

    pub(crate) fn commit_internal(&self, merge_policy: &L::MergePolicy) -> Result<i64> {
        if self.info_stream.enabled("IW") {
            self.info_stream.message("IW", "commit: start");
        }

        let seq_no: i64;

        {
            let commit_lock = &mut *self.commit_lock.lock();
            self.do_ensure_open(false)?;

            if self.info_stream.enabled("IW") {
                self.info_stream.message("IW", "commit: enter lock");
            }

            if commit_lock.pending_commit.is_none() {
                if self.info_stream.enabled("IW") {
                    self.info_stream.message("IW", "commit: now prepare");
                }
                seq_no = self.prepare_commit_internal(Some(commit_lock))?;
            } else {
                if self.info_stream.enabled("IW") {
                    self.info_stream.message("IW", "commit: already prepared");
                }
                seq_no = self.pending_seq_no.load(Ordering::SeqCst);
            }
            self.finish_commit(commit_lock)?;
        }

        if self.maybe_merge.swap(false, Ordering::AcqRel) {
            self.maybe_merge_with_max_num_segments(
                merge_policy,
                MergeTrigger::FullFlush,
                UNBOUNDED_MAX_MERGE_SEGMENTS,
            )?;
        }

        Ok(seq_no)
    }

    pub(crate) fn finish_commit(&self, commit_lock: &mut CommitInner<D>) -> Result<()> {
        let mut commit_completed = false;
        let try_res: Result<()> = (|| {
            let mut inner = self.inner.lock();
            self.do_ensure_open(false)?;

            if let Some(t) = &*self.tragedy.lock() {
                return Err(LuceneError::illegal_state(format!(
                    "this writer hit an unrecoverable error; cannot complete commit {}",
                    t
                )));
            }

            if commit_lock.pending_commit.is_some() {
                debug_assert!(commit_lock.files_to_commit.is_some());
                let mut body_res: Result<()> = (|| {
                    if self.info_stream.enabled("IW") {
                        self.info_stream
                            .message("IW", "commit: pendingCommit != null");
                    }
                    let pending = commit_lock
                        .pending_commit
                        .as_mut()
                        .expect("pending_commit must exist");
                    let committed_segments_file_name =
                        pending.finish_commit(self.directory.as_ref())?;
                    // we committed, if anything goes wrong after this, we are screwed and it's a tragedy:
                    commit_completed = true;

                    if self.info_stream.enabled("IW") {
                        self.info_stream.message(
                            "IW",
                            &format!(
                                "commit: done writing segments file \"{}\"",
                                committed_segments_file_name
                            ),
                        );
                    }
                    // NOTE: don't use this.checkpoint() here, because
                    // we do not want to increment changeCount:
                    inner.deleter.checkpoint(
                        commit_lock.pending_commit.as_ref().unwrap(),
                        true,
                        self.config.get_index_deletion_policy(),
                    )?;

                    // Carry over generation to our master SegmentInfos:
                    inner
                        .segment_infos
                        .update_generation(commit_lock.pending_commit.as_ref().unwrap());

                    self.last_commit_change_count.store(
                        self.pending_commit_change_count.load(Ordering::Acquire),
                        Ordering::Release,
                    );

                    inner.rollback_segments = commit_lock
                        .pending_commit
                        .as_ref()
                        .unwrap()
                        .create_backup_segment_infos()?;

                    Ok(())
                })();

                {
                    self.pausing.notify_all();
                    commit_lock.pending_commit = None;
                    let files = commit_lock.files_to_commit.take().unwrap();

                    body_res = match inner.deleter.dec_ref(files.iter()) {
                        Ok(()) => body_res,
                        Err(e) => {
                            // TODO IMPORTANT 这里没有处理好嵌套错误
                            Err(LuceneError::illegal_state(format!("{:?}, {}", body_res, e)))
                        },
                    }
                }
                body_res?;
            } else {
                debug_assert!(commit_lock.files_to_commit.is_none());
                if self.info_stream.enabled("IW") {
                    self.info_stream
                        .message("IW", "commit: pendingCommit == null; skip");
                }
            }
            Ok(())
        })();

        if let Err(t) = try_res {
            if self.info_stream.enabled("IW") {
                self.info_stream
                    .message("IW", &format!("hit exception during finishCommit: {}", t));
            }
            if commit_completed {
                self.tragic_event(&t, "finishCommit");
            }
            return Err(t);
        }

        if self.info_stream.enabled("IW") {
            self.info_stream.message(
                "IW",
                &format!(
                    "commit: took {:.1} msec",
                    commit_lock.start_commit_time.elapsed().as_millis() as f64
                ),
            );
            self.info_stream.message("IW", "commit: done");
        }

        Ok(())
    }
    /// Moves all in-memory segments to the [`Directory`], but does not commit (fsync) them
    /// (call [`commit`](Self::commit) for that).
    pub fn flush(&self) -> Result<()> {
        self.flush_with_apply_merge_deletes(true, true)
    }
    /// Flushes all in-memory buffered updates (adds and deletes) to the `Directory`.
    ///
    /// # Arguments
    ///
    /// * `trigger_merge` — if `true`, segments may be merged (if deletes or docs were flushed) if necessary.
    /// * `apply_all_deletes` — whether pending deletes should also be applied.
    pub(crate) fn flush_with_apply_merge_deletes(
        &self,
        trigger_merge: bool,
        apply_all_deletes: bool,
    ) -> Result<()> {
        // NOTE: this method cannot be sync'd because
        // maybeMerge() in turn calls mergeScheduler.merge which
        // in turn can take a long time to run and we don't want
        // to hold the lock for that.  In the case of
        // ConcurrentMergeScheduler this can lead to deadlock
        // when it stalls due to too many running merges.

        // We can be called during close, when closing==true, so we must pass false to ensureOpen:
        self.do_ensure_open(false)?;
        if self.do_flush(apply_all_deletes)? && trigger_merge {
            self.maybe_merge_with_max_num_segments(
                self.config.get_merge_policy(),
                MergeTrigger::FullFlush,
                UNBOUNDED_MAX_MERGE_SEGMENTS,
            )?;
        }
        Ok(())
    }
    /// Returns true a segment was flushed or deletes were applied.
    fn do_flush(&self, apply_all_deletes: bool) -> Result<bool> {
        if let Some(t) = &*self.tragedy.lock() {
            return Err(LuceneError::illegal_state(format!(
                "this writer hit an unrecoverable error; cannot flush {}",
                t
            )));
        }

        if let Some(ref s) = self.sub {
            s.do_before_flush()?;
        }

        self.test_point("startDoFlush");
        let mut success = false;

        let res: Result<bool> = (|| {
            if self.info_stream.enabled("IW") {
                self.info_stream.message(
                    "IW",
                    &format!("  start flush: applyAllDeletes={}", apply_all_deletes),
                );
                self.info_stream.message(
                    "IW",
                    &format!("  index before flush {}", self.seg_string()?),
                );
            }

            let any_changes = {
                let _guard = self.full_flush_lock.lock();
                let mut flush_success = false;
                let result: Result<bool> = (|| {
                    let any_changes = self.doc_writer.flush_all_threads(self, &self.config)? < 0;
                    if !any_changes {
                        // flushCount is incremented in flushAllThreads if true
                        self.flush_count.fetch_add(1, Ordering::SeqCst);
                    }
                    self.publish_flushed_segments(true)?;
                    flush_success = true;
                    Ok(any_changes)
                })();
                self.doc_writer
                    .finish_full_flush(flush_success, &self.config)?;
                self.process_events(false)?;
                result?
            };

            if apply_all_deletes {
                self.apply_all_deletes_and_updates()?;
            }

            let any_changes = any_changes | self.maybe_merge.swap(false, Ordering::AcqRel);

            {
                let mut inner = self.inner.lock();
                self.write_reader_pool(apply_all_deletes, &mut *inner)?;
                if let Some(ref s) = self.sub {
                    s.do_after_flush()?;
                }
                success = true;
            }

            Ok(any_changes)
        })();

        if !success {
            if self.info_stream.enabled("IW") {
                self.info_stream.message("IW", "hit exception during flush");
            }
            self.maybe_close_on_tragic_event()?;
        }
        match res {
            Ok(v) => Ok(v),
            Err(t) => {
                self.tragic_event(&t, "doFlush");
                Err(t)
            },
        }
    }

    fn apply_all_deletes_and_updates(&self) -> Result<()> {
        self.flush_deletes_count.fetch_add(1, Ordering::AcqRel);
        if self.info_stream.enabled("IW") {
            self.info_stream.message(
                "IW",
                &format!(
                    "now apply all deletes for all segments buffered updates bytesUsed={} reader pool bytesUsed={}",
                    self.buffered_updates_stream.ram_bytes_used()?,
                    self.reader_pool.ram_bytes_used()
                ),
            );
        }
        self.buffered_updates_stream.wait_apply_all(self)
    }
    #[cfg(test)]
    pub(crate) fn get_docs_writer(&self) -> &DocumentsWriter<D, FlushNotificationsImpl> {
        &self.doc_writer
    }
    /// Return the number of documents currently buffered in RAM.
    pub fn num_ram_docs(&self) -> Result<i32> {
        let _inner = self.inner.lock();
        self.ensure_open()?;
        let v = self.doc_writer.get_num_docs();
        Ok(v)
    }
    fn ensure_valid_merge<CR>(&self, merge: &OneMerge<D, CR>, inner: &Inner<D>) -> Result<()>
    where
        CR: CodecReader,
    {
        for info in &merge.stat.segments {
            if !inner.segment_infos.contains(info) {
                return Err(LuceneError::merge(format!(
                    "MergePolicy selected a segment ({}) that is not in the current index {}",
                    info,
                    self.seg_string()?
                )));
            }
        }

        Ok(())
    }
    /// Carefully merges deletes and updates for the segments we just merged.
    ///
    /// This is tricky because, although merging will clear all deletes (compacts
    /// the documents) and compact all the updates, new deletes and updates may
    /// have been flushed to the segments since the merge was started.
    ///
    /// This method *carries over* such new deletes and updates onto the newly
    /// merged segment, and saves the resulting deletes and DocValues updates
    /// files (incrementing the delete and DV generations for `merge.info`).
    ///
    /// If no deletes were flushed, no new deletes file is saved.
    fn commit_merged_deletes_and_updates<DM>(
        &self,
        merge: &mut OneMergeSR<D>,
        doc_maps: &[DM],
        inner: &mut MutexGuard<'_, Inner<D>>,
    ) -> Result<Arc<ReadersAndUpdates<D>>>
    where
        DM: DocMap,
    {
        self.merge_finished_gen.fetch_add(1, Ordering::AcqRel);

        self.test_point("startCommitMergeDeletes");

        let source_segments = merge.stat.segments.as_slice();
        // Carefully merge deletes that occurred after we
        // started merging:
        let mut min_gen: i64 = i64::MAX;

        let sci = merge.info.as_ref().unwrap();
        // Lazy init (only when we find a delete or update to carry over):
        let merged_deletes_and_updates =
            self.get_pooled_instance(sci, true, None)?.ok_or_else(|| {
                LuceneError::illegal_state("failed to get pooled instance for a merged segment")
            })?;

        let _ = merged_deletes_and_updates.get_del_count(sci);

        // field -> delGen -> dv field updates
        let mut mapped_dv_updates = HashMap::new();
        let mut any_dv_updates = false;
        debug_assert_eq!(source_segments.len(), doc_maps.len());
        for (i, info_id) in source_segments.iter().enumerate() {
            let info = inner.segment_infos.info(info_id).ok_or_else(|| {
                LuceneError::illegal_state(format!("segment info with id={} not found", info_id))
            })?;

            min_gen = std::cmp::min(info.get_buffered_deletes_gen(), min_gen);

            let max_doc = info.info.max_doc()?;

            let rld = self
                .get_pooled_instance(info, false, None)?
                .ok_or_else(|| {
                    LuceneError::illegal_state(format!(
                        "seg={} not found in reader pool",
                        info.info.name
                    ))
                })?;

            let seg_doc_map = &doc_maps[i];

            // TODO IMPORTANT
            // carry over hard deletes
            Self::carry_over_hard_deletes(
                merged_deletes_and_updates.as_ref(),
                max_doc,
                merge.get_merge_reader()[i].hard_live_docs.as_ref(),
                rld.get_hard_live_docs().as_ref(),
                seg_doc_map,
                sci,
            )?;

            // Now carry over all doc values updates that were resolved while we were merging, remapping
            // the docIDs to the newly merged docIDs.
            // We only carry over packets that finished resolving; if any are still running (concurrently),
            // they will detect that our merge completed
            // and re-resolve against the newly merged segment:
            let merging_dv_updates = rld.get_merging_dv_updates();
            for (field, updates_list) in merging_dv_updates {
                let mapped_field = mapped_dv_updates
                    .entry(field.clone())
                    .or_insert_with(HashMap::new);

                for updates in updates_list {
                    if self.buffered_updates_stream.still_running(updates.del_gen) {
                        continue;
                    }
                    // sanity check:
                    debug_assert_eq!(field, updates.field);
                    if let std::collections::hash_map::Entry::Vacant(e) =
                        mapped_field.entry(updates.del_gen)
                    {
                        let v = match updates.type_ {
                            DocValuesType::Numeric => {
                                let sub_update1 = NumericDocValuesFieldUpdates::new()?;
                                DocValuesFieldUpdates::new(
                                    merge.info.as_ref().unwrap().info.max_doc()?,
                                    updates.del_gen,
                                    updates.field.clone(),
                                    sub_update1.sub_type(),
                                    sub_update1,
                                )?
                            },
                            DocValuesType::Binary => {
                                let sub = BinaryDocValuesFieldUpdates::new()?;
                                DocValuesFieldUpdates::new(
                                    merge.info.as_ref().unwrap().info.max_doc()?,
                                    updates.del_gen,
                                    updates.field.clone(),
                                    sub.sub_type(),
                                    sub,
                                )?
                            },
                            _ => {
                                return Err(LuceneError::illegal_state(
                                    "unsupported DocValues type during merge",
                                ));
                            },
                        };
                        e.insert(v);
                    }
                    let mapped_updates = mapped_field.get_mut(&updates.del_gen).unwrap();

                    let mut it = updates.iterator()?;
                    loop {
                        let doc = it.next_doc()?;
                        if doc == NO_MORE_DOCS {
                            break;
                        }
                        let mapped_doc = seg_doc_map.get(doc)?;
                        if mapped_doc != -1 {
                            if it.has_value()? {
                                mapped_updates.add_iterator(mapped_doc, &mut it)?;
                            } else {
                                mapped_updates.reset(mapped_doc)?;
                            }
                            any_dv_updates = true;
                        }
                    }
                }
            }
        }

        if any_dv_updates {
            // Persist the merged DV updates onto the RAU for the merged segment:
            for d in mapped_dv_updates.into_values() {
                for mut updates in d.into_values() {
                    updates.finish()?;
                    merged_deletes_and_updates.add_dv_update(updates)?;
                }
            }
        }
        merge
            .info
            .as_mut()
            .ok_or_else(|| LuceneError::illegal_state("merge.info is none"))?
            .set_buffered_deletes_gen(min_gen)?;

        Ok(merged_deletes_and_updates)
    }
    /// This method carries over hard-deleted documents that are applied to the source segment during a
    /// merge.
    fn carry_over_hard_deletes<DM, B1, B2>(
        merged_readers_and_updates: &ReadersAndUpdates<D>,
        max_doc: i32,
        prev_hard_live_docs: Option<&B1>, // the hard deletes when the merge reader was pulled
        current_hard_live_docs: Option<&B2>, // the current hard deletes
        seg_doc_map: &DM,
        info: &SegmentCommitInfo<D>,
    ) -> Result<()>
    where
        DM: DocMap,
        B1: Bits,
        B2: Bits,
    {
        // if we mix soft and hard deletes, we need to make sure that we only carry over deletes
        // that were not deleted before. Otherwise, the segDocMap doesn't contain a mapping.
        // yet this is also required if any MergePolicy modifies the liveDocs since this is
        // what the segDocMap is build on.
        if let Some(current_hard_live_docs) = current_hard_live_docs {
            let carry_over_delete = |doc_id: usize| -> Result<bool> {
                Ok(seg_doc_map.get(doc_id as i32)? != -1 && !current_hard_live_docs.get(doc_id)?)
            };

            if let Some(prev_hard_live_docs) = prev_hard_live_docs {
                // If we had deletions on starting the merge, we must
                // still have deletions now:
                debug_assert!(prev_hard_live_docs.length() == max_doc as usize);
                debug_assert!(current_hard_live_docs.length() == max_doc as usize);

                // There were deletes on this segment when the merge
                // started.  The merge has collapsed away this
                // deletes, but, if new deletes were flushed since
                // the merge started, we must now carefully keep any
                // newly flushed deletes but mapping them to the new
                // docIDs.

                // Since we copy-on-write, if any new deletes were
                // applied after merging has started, we can just
                // check if the before/after liveDocs have changed.
                // If so, we must carefully merge the liveDocs one
                // doc at a time:
                if current_hard_live_docs.identity() == prev_hard_live_docs.identity() {
                    // This means this segment received new deletes
                    // since we started the merge, so we
                    // must merge them:
                    for j in 0..max_doc as usize {
                        if !(prev_hard_live_docs.get(j)?) {
                            // if the document was deleted before, it better still be deleted!
                            debug_assert!(!(current_hard_live_docs.get(j)?));
                        } else if carry_over_delete(j)? {
                            // the document was deleted while we were merging:
                            merged_readers_and_updates.delete(
                                seg_doc_map.get(j as i32)?,
                                info,
                                None,
                            )?;
                        }
                    }
                }
            } else {
                debug_assert!(current_hard_live_docs.length() == max_doc as usize);
                // This segment had no deletes before, but now it
                // does:
                for j in 0..max_doc {
                    if carry_over_delete(j as usize)? {
                        merged_readers_and_updates.delete(seg_doc_map.get(j)?, info, None)?;
                    }
                }
            }
        }
        Ok(())
    }
    fn commit_merge<DM>(&self, merge: &mut OneMergeSR<D>, doc_maps: &[DM]) -> Result<bool>
    where
        DM: DocMap,
    {
        let mut inner = self.inner.lock();
        merge.on_merge_complete()?;
        self.test_point("startCommitMerge");

        if let Some(t) = &*self.tragedy.lock() {
            return Err(LuceneError::illegal_state(format!(
                "this writer hit an unrecoverable error; cannot complete merge: {}",
                t
            )));
        }

        debug_assert!(merge.register_done);

        // If merge was explicitly aborted, or, if rollback() or
        // rollbackTransaction() had been called since our merge
        // started (which results in an unqualified
        // deleter.refresh() call that will remove any index
        // file that current segments do not reference), we
        // abort this merge
        if merge.is_aborted() {
            // In case we opened and pooled a reader for this
            // segment, drop it now.  This ensures that we close
            // the reader before trying to delete any of its
            // files.  This is not a very big deal, since this
            // reader will never be used by any NRT reader, and
            // another thread is currently running close(false)
            // so it will be dropped shortly anyway, but not
            // doing this makes  MockDirWrapper angry in
            // TestNRTThreads (LUCENE-5434):
            if let Some(ref info) = merge.info {
                self.reader_pool.drop(&info.info.get_id_str())?;
                // Safe: these files must exist
                self.delete_new_files(info.files()?.iter())?;
            } else {
                return Err(LuceneError::illegal_state("merge info is none"));
            }

            return Ok(false);
        }

        let merged_updates = if merge.info.as_ref().unwrap().info.max_doc()? == 0 {
            None
        } else {
            Some(self.commit_merged_deletes_and_updates(merge, doc_maps, &mut inner)?)
        };
        // If the doc store we are using has been closed and
        // is in now compound format (but wasn't when we
        // started), then we will switch to the compound
        // format as well:
        let sci = merge.info.as_ref().unwrap();
        debug_assert!(!inner.segment_infos.contains(&sci.info.get_id_str()));

        let all_deleted = merge.stat.segments.is_empty()
            || sci.info.max_doc()? == 0
            || (merged_updates.is_some()
                && self.is_fully_deleted(merged_updates.as_ref().unwrap().as_ref(), sci)?);

        if self.info_stream.enabled("IW")
            && all_deleted
            && let Some(ref info) = merge.info
        {
            self.info_stream.message(
                "IW",
                &format!("merged segment {} is 100% deleted; skipping insert", info),
            );
        }
        let drop_segment = all_deleted;

        // If we merged no segments then we better be dropping
        // the new segment:
        debug_assert!(!merge.stat.segments.is_empty() || drop_segment);
        debug_assert!(sci.info.max_doc()? != 0 || drop_segment);

        if let Some(merged_updates) = merged_updates {
            let mut success = false;
            let res: Result<()> = (|| {
                if drop_segment {
                    merged_updates.drop_changes();
                }
                // Pass false for assert_live_info because the merged
                // segment is not yet live (only below do we commit it
                // to the segment_infos):
                self.release_with_assert(&merged_updates, false, &mut inner, merge.info.as_mut())?;
                success = true;
                Ok(())
            })();

            if res.is_err() {
                merged_updates.drop_changes();
                self.reader_pool
                    .drop(&merge.info.as_ref().unwrap().info.get_id_str())?;
                return Err(res.err().unwrap());
            }
        }

        // Must do this after reader_pool.release, in case an
        // exception is hit e.g. writing the live docs for the
        // merge segment, in which case we need to abort the
        // merge:
        let merge_id = merge.info.as_ref().unwrap().info.get_id_str();
        inner
            .segment_infos
            .apply_merge_changes(merge, drop_segment)?;
        // Note: merge's SegmentCommitInfo has move to IndexWriter#inner#segment_infos
        let merge_sci = inner.segment_infos.info(&merge_id).ok_or_else(|| {
            LuceneError::illegal_state(
                "merge's SegmentCommitInfo not in IndexWriter#inner#segment_infos",
            )
        })?;

        // Now deduct the deleted docs that we just reclaimed from this
        // merge:
        let del_doc_count = if drop_segment {
            // if we drop the segment we have to reduce the pendingNumDocs by merge.totalMaxDocs since we
            // never drop
            // the docs when we apply deletes if the segment is currently merged.
            merge.total_max_doc
        } else {
            merge.total_max_doc - merge_sci.info.max_doc()?
        };
        debug_assert!(del_doc_count >= 0);
        self.adjust_pending_num_docs(-(del_doc_count as i64));

        if drop_segment {
            debug_assert!(!inner.segment_infos.contains(&merge_sci.info.get_id_str()));
            self.reader_pool.drop(&merge_sci.info.get_id_str())?;
            // Safe: these files must exist
            self.delete_new_files(merge_sci.files()?.iter())?;
        }

        // TODO IMPORTANT close_merge_readers
        {
            self.checkpoint(&mut inner)?;
            // Must close before checkpoint, otherwise IFD won't be
            // able to delete the held-open files from the merge
            // readers:
            self.close_merge_readers(merge, false, drop_segment)?;
        }

        if self.info_stream.enabled("IW") {
            self.info_stream
                .message("IW", &format!("after commitMerge: {}", self.seg_string()?));
        }

        if merge.stat.max_num_segments != UNBOUNDED_MAX_MERGE_SEGMENTS && !drop_segment {
            // cascade the forceMerge:
            inner
                .segments_to_merge
                .entry(merge_id)
                .or_insert(Some(false));
        }

        Ok(true)
    }
    /// Merges the indicated segments, replacing them in the stack with a single segment.
    fn merge(&self, mut merge: OneMergeSR<D>) -> Result<()> {
        let mut success = false;
        let merge_policy = self.config.get_merge_policy();
        let result = (|| -> Result<()> {
            let inner_result = (|| -> Result<()> {
                self.merge_init(&mut merge)?;
                // Note: merge's SegmentCommitInfo has move to IndexWriter#inner#segment_infos
                self.merge_middle(&mut merge, merge_policy)?;
                debug_assert!(merge.info.is_none());
                self.merge_success(&mut merge)?;
                success = true;
                Ok(())
            })();

            {
                let mut inner = self.inner.lock();
                // Readers are already closed in commitMerge if we didn't hit
                // an exc:
                if !success {
                    self.close_merge_readers(&merge, true, false)?;
                }
                self.merge_finish(&mut merge, Some(&mut inner));

                if !success {
                    if self.info_stream.enabled("IW") {
                        self.info_stream.message("IW", "hit exception during merge");
                    }
                } else if !merge.is_aborted()
                    && (merge.stat.max_num_segments != UNBOUNDED_MAX_MERGE_SEGMENTS
                        || (!self.closed.load(SeqCst) && !self.closing.load(SeqCst)))
                {
                    self.update_pending_merges(
                        merge_policy,
                        MergeTrigger::MergeFinished,
                        merge.stat.max_num_segments,
                        Some(&mut inner),
                    )?;
                }
            }
            match inner_result {
                Ok(()) => {},
                Err(e) => {
                    // TODO IMPORTANT handleMergeException
                    return Err(e);
                },
            }
            Ok(())
        })();
        if let Err(e) = result {
            self.tragic_event(&e, "merge");
            return Err(e);
        }
        Ok(())
    }
    fn merge_success(&self, _merge: &mut OneMergeSR<D>) -> Result<()> {
        Ok(())
    }

    /// Checks whether this merge involves any segments already participating in a merge.
    /// If not, this merge is "registered", meaning we record that its segments are now participating in a merge,
    /// and true is returned. Else (the merge conflicts) false is returned.
    fn register_merge(&self, mut merge: OneMergeSR<D>, inner: &mut Inner<D>) -> Result<bool> {
        if merge.register_done {
            return Ok(true);
        }
        debug_assert!(!merge.stat.segments.is_empty());

        if !inner.merges.are_enabled(inner) {
            // TODO: self.abort_one_merge(merge)?;
            return Err(LuceneError::merge_abort("merge is aborted"));
        }

        // TODO IMPORTANT Current Rust implementation, `is_external` is always false
        let is_external = false;

        for info_id in &merge.stat.segments {
            if inner.merging_segments.contains(info_id) {
                return Ok(false);
            }
            if !inner.segment_infos.contains(info_id) {
                return Ok(false);
            }

            if inner.segments_to_merge.contains_key(info_id) {
                merge.stat.max_num_segments = inner.merge_max_num_segments;
            }
        }
        self.ensure_valid_merge(&merge, inner)?;

        merge.stat.merge_gen = inner.merge_gen;
        merge.is_external = is_external;

        // OK it does not conflict; now record that this merge
        // is running (while synchronized) to avoid race
        // condition where two conflicting merges from different
        // threads, start
        if self.info_stream.enabled("IW") {
            let mut builder = String::from("registerMerge merging= [");
            for id in &inner.merging_segments {
                builder.push_str(id);
                builder.push_str(", ");
            }
            builder.push(']');
            self.info_stream.message("IW", &builder);
        }

        for info_id in &merge.stat.segments {
            inner.merging_segments.insert(info_id.clone());
        }

        debug_assert!(merge.estimated_merge_bytes.load(Ordering::Relaxed) == 0);
        debug_assert!(merge.total_merge_bytes.load(Ordering::Relaxed) == 0);

        let mut est_bytes: i64 = 0;
        let mut total_bytes: i64 = 0;

        for info_id in &merge.stat.segments {
            let info = inner.segment_infos.info(info_id).ok_or_else(|| {
                LuceneError::illegal_state("{} not in IndexWriter's segment_infos")
            })?;
            let max_doc = info.info.max_doc()?;
            if max_doc > 0 {
                let del_count = self.num_deleted_docs(info)?;
                debug_assert!(del_count <= max_doc);

                let del_ratio = (del_count as f64) / (max_doc as f64);
                est_bytes += (info.size_in_bytes()? as f64 * (1.0 - del_ratio)) as i64;
                total_bytes += info.size_in_bytes()?;
            }
        }

        merge
            .estimated_merge_bytes
            .store(est_bytes, Ordering::Release);
        merge
            .total_merge_bytes
            .store(total_bytes, Ordering::Release);
        // Merge is now registered
        merge.register_done = true;
        inner.pending_merges.push_back(merge);
        Ok(true)
    }
    /// Does initial setup for a merge, which is fast but holds the synchronized lock on IndexWriter instance.
    pub(crate) fn merge_init(&self, merge: &mut OneMergeSR<D>) -> Result<()> {
        // Make sure any deletes that must be resolved before we commit the merge are complete:
        self.buffered_updates_stream
            .wait_apply_for_merge(&merge.stat.segments, self)?;

        let result = (|| {
            self.merge_init_(merge)?;
            Ok(())
        })();
        if result.is_err() {
            if self.info_stream.enabled("IW") {
                self.info_stream.message("IW", "hit exception in mergeInit");
            }
            self.merge_finish(merge, None);
        }
        result
    }

    fn merge_init_(&self, merge: &mut OneMergeSR<D>) -> Result<()> {
        let mut inner = self.inner.lock();
        self.test_point("startMergeInit");

        debug_assert!(merge.register_done);
        debug_assert!(
            merge.stat.max_num_segments == UNBOUNDED_MAX_MERGE_SEGMENTS
                || merge.stat.max_num_segments > 0
        );

        if let Some(t) = &*self.tragedy.lock() {
            return Err(LuceneError::illegal_state(format!(
                "this writer hit an unrecoverable error; cannot merge: {}",
                t
            )));
        }

        if merge.info.is_some() {
            // mergeInit already done
            return Ok(());
        }

        merge.merge_init();

        if merge.is_aborted() {
            return Ok(());
        }

        if self.info_stream.enabled("IW") {
            self.info_stream.message(
                "IW",
                &format!(
                    "now apply deletes for {} merging segments",
                    merge.stat.segments.len()
                ),
            );
        }

        if self.reader_pool.write_doc_values_updates_for_merge(
            merge.stat.segments.as_ref(),
            &mut inner.segment_infos,
            &self.global_field_number_map.lock(),
        )? {
            self.checkpoint(&mut self.inner.lock())?;
        }

        let mut has_blocks = false;
        for info_id in &merge.stat.segments {
            let info = inner.segment_infos.info(info_id).ok_or_else(|| {
                LuceneError::illegal_state("{} not in IndexWriter's segment_infos")
            })?;
            if info.info.get_has_blocks() {
                has_blocks = true;
                break;
            }
        }
        // Bind a new segment name here, so even with
        // ConcurrentMergePolicy we keep deterministic segment
        // names.
        let merge_segment_name = self.new_segment_name(Some(&mut inner));

        let mut si = SegmentInfo::new(
            self.directory_orig.clone(),
            Some((*LATEST).clone()),
            None,
            merge_segment_name.as_ref(),
            -1,
            false,
            has_blocks,
            HashMap::new(),
            StringHelper::random_id(),
            HashMap::new(),
            self.config.get_index_sort(),
        )?;

        let mut details = HashMap::new();
        details.insert(
            "mergeMaxNumSegments".to_string(),
            merge.stat.max_num_segments.to_string(),
        );
        details.insert(
            "mergeFactor".to_string(),
            merge.stat.segments.len().to_string(),
        );

        set_diagnostics_impl(&mut si, SOURCE_MERGE, Some(details));

        let sci = SegmentCommitInfo::new(si, 0, 0, -1, -1, -1, Some(StringHelper::random_id()))?;
        merge.set_merge_info(sci);

        Ok(())
    }

    /// Does finishing for a merge, which is fast but holds the synchronized lock on IndexWriter instance.
    fn merge_finish(&self, merge: &mut OneMergeSR<D>, inner: Option<&mut Inner<D>>) {
        let inner = match inner {
            Some(i) => i,
            None => &mut *self.inner.lock(),
        };
        // forceMerge, addIndexes or waitForMerges may be waiting
        // on merges to finish.
        self.pausing.notify_all();

        // It's possible we are called twice, e.g. if there was an
        // exception inside mergeInit
        if merge.register_done {
            for seg_id in &merge.stat.segments {
                inner.merging_segments.remove(seg_id);
            }
            merge.register_done = false;
        }

        inner.running_merges.remove(&merge.stat);
    }

    fn close_merge_readers(
        &self,
        _merge: &OneMergeSR<D>,
        _suppress_error: bool,
        _dropper_segment: bool,
    ) -> Result<()> {
        todo!()
    }

    /// Returns a string description of all segments, for debugging.
    fn seg_string(&self) -> Result<String> {
        let inner = self.inner.lock();
        self.seg_string_from_infos(inner.segment_infos.iter())
    }

    fn seg_string_from_infos<'a, I>(&self, infos: I) -> Result<String>
    where
        I: IntoIterator<Item = &'a SegmentCommitInfo<D>>,
        D: 'a,
    {
        let mut result = String::new();
        let mut first = true;
        for info in infos {
            match self.seg_string_from_info(info) {
                Ok(s) => {
                    if !first {
                        result.push(' ');
                    }
                    result.push_str(&s);
                    first = false;
                },
                Err(e) => {
                    return Err(e);
                },
            }
        }
        Ok(result)
    }
    /// Returns a string description of the specified segment, for debugging.
    fn seg_string_from_info(&self, info: &SegmentCommitInfo<D>) -> Result<String> {
        // numDeletedDocs(info) - info.getDelCount(softDeletesEnabled)
        let num_deleted = self.num_deleted_docs(info)?
            - info.get_del_count_with_soft_deletes(self.soft_deletes_enabled);
        Ok(info.to_string_with_pending_del_count(num_deleted))
    }

    fn do_wait(&self, guard: &mut MutexGuard<Inner<D>>) {
        // NOTE: the callers of this method should in theory
        // be able to do simply wait(), but, as a defense
        // against thread timing hazards where notifyAll()
        // fails to be called, we wait for at most 1 second
        // and then return so caller can check if wait
        // conditions are satisfied:
        // wait at most 1s
        self.pausing.wait_for(guard, Duration::from_millis(1000));
    }
    pub(crate) fn files_exist(
        to_sync: &SegmentInfos<D>,
        deleter: &IndexFileDeleter<D>,
    ) -> Result<bool> {
        let files = to_sync.files(false)?;

        for file_name in &files {
            // If this trips it means we are missing a call to
            // .checkpoint somewhere, because by the time we
            // are called, deleter should know about every
            // file referenced by the current head
            // segmentInfos:
            debug_assert!(
                deleter.exists(file_name),
                "IndexFileDeleter doesn't know about file {}",
                file_name
            );
        }

        Ok(true)
    }
    /// Walk through all files referenced by the current segmentInfos and ask the Directory to sync each file,
    /// if it wasn't already. If that succeeds, then we prepare a new segments_N file but do not fully commit it.
    pub(crate) fn start_commit(
        &self,
        mut to_sync: Option<SegmentInfos<D>>,
        commit_lock: &mut CommitInner<D>,
    ) -> Result<()> {
        debug_assert!(commit_lock.files_to_commit.is_some());
        // wrap with Option for easily take ownership
        debug_assert!(to_sync.is_some());
        self.test_point("startStartCommit");
        debug_assert!(commit_lock.pending_commit.is_none());
        if let Some(t) = &*self.tragedy.lock() {
            return Err(LuceneError::illegal_state(format!(
                "this writer hit an unrecoverable error; cannot commit {}",
                t
            )));
        }

        if self.tragedy.lock().is_some() {
            return Err(LuceneError::illegal_state(
                "this writer hit an unrecoverable error; cannot commit",
            ));
        }
        // did to_sync's ownership move to pending_commit?
        // after pending_commit has to_sync's ownership, and error happens, we have to pass to to_sync_error
        let result: Result<()> = (|| {
            if self.info_stream.enabled("IW") {
                self.info_stream.message("IW", "startCommit(): start");
            }

            {
                let mut inner = self.inner.lock();
                let last_commit_change_count = self.last_commit_change_count.load(Ordering::SeqCst);
                if last_commit_change_count > inner.change_count {
                    return Err(LuceneError::illegal_state(format!(
                        "lastCommitChangeCount={} , changeCount={}",
                        last_commit_change_count, inner.change_count
                    )));
                }

                if self.pending_commit_change_count.load(Ordering::SeqCst)
                    == self.last_commit_change_count.load(Ordering::SeqCst)
                {
                    if self.info_stream.enabled("IW") {
                        self.info_stream
                            .message("IW", "  skip startCommit(): no changes pending");
                    }
                    let files = commit_lock.files_to_commit.take().unwrap();
                    inner.deleter.dec_ref(files.iter())?;
                    return Ok(());
                }

                if self.info_stream.enabled("IW") {
                    // TODO
                    // let segs = self.seg_string_with_infos(self.to_live_infos(to_sync, inner)?);
                    // self.info_stream.message(
                    //     "IW",
                    //     &format!("startCommit index={} changeCount={}", segs, change_count),
                    // );
                }
                debug_assert!(Self::files_exist(
                    to_sync.as_ref().unwrap(),
                    &inner.deleter
                )?);
            }

            self.test_point("midStartCommit");

            let mut pending_commit_set = false;

            let res: Result<()> = (|| {
                self.test_point("midStartCommit2");

                {
                    let inner = self.inner.lock();
                    debug_assert!(commit_lock.pending_commit.is_none());
                    debug_assert!(
                        inner.segment_infos.get_generation()
                            == to_sync.as_ref().unwrap().get_generation()
                    );
                    // Exception here means nothing is prepared
                    // (this method unwinds everything it did on
                    // an exception)

                    to_sync
                        .as_mut()
                        .unwrap()
                        .prepare_commit(self.directory.as_ref())?;
                    if self.info_stream.enabled("IW") {
                        let file_name = IndexFileNames::file_name_from_generation(
                            IndexFileNames::PENDING_SEGMENTS,
                            "",
                            to_sync.as_ref().unwrap().get_generation(),
                        );
                        self.info_stream.message(
                            "IW",
                            &format!("startCommit: wrote pending segments file {:?}", file_name),
                        );
                    }

                    pending_commit_set = true;
                    commit_lock.pending_commit = to_sync.take();
                }
                // This call can take a long time -- 10s of seconds
                // or more.  We do it without syncing on this:
                let mut files_to_sync_list = Vec::new();
                let sync_res: Result<()> = (|| {
                    files_to_sync_list = commit_lock
                        .pending_commit
                        .as_ref()
                        .unwrap()
                        .files(false)?
                        .into_iter()
                        .collect();
                    self.directory.sync(&files_to_sync_list)?;
                    Ok(())
                })();

                if let Err(e) = sync_res {
                    pending_commit_set = false;
                    debug_assert!(commit_lock.pending_commit.is_some());
                    commit_lock
                        .pending_commit
                        .as_mut()
                        .unwrap()
                        .rollback_commit(self.directory.as_ref());
                    to_sync = commit_lock.pending_commit.take();
                    return Err(e);
                }

                if self.info_stream.enabled("IW") {
                    self.info_stream
                        .message("IW", &format!("done all syncs: {:?}", files_to_sync_list));
                }

                self.test_point("midStartCommitSuccess");
                Ok(())
            })();

            let res = match res {
                Ok(()) => Ok(()),
                Err(t) => {
                    let mut inner = self.inner.lock();
                    if !pending_commit_set {
                        if self.info_stream.enabled("IW") {
                            self.info_stream
                                .message("IW", "hit exception committing segments file");
                        }
                        let files = commit_lock.files_to_commit.take().unwrap();
                        match inner.deleter.dec_ref(files.iter()) {
                            Ok(()) => Err(t),
                            Err(e) => {
                                // TODO IMPORTANT 这里没有正确的嵌套错误
                                Err(LuceneError::illegal_state(format!("{} {}", e, t)))
                            },
                        }
                    } else {
                        Err(t)
                    }
                },
            };

            {
                let mut inner = self.inner.lock();
                // Have our master segmentInfos record the
                // generations we just prepared.  We do this
                // on error or success so we don't
                // double-write a segments_N file.
                match pending_commit_set {
                    true => {
                        inner
                            .segment_infos
                            .update_generation(commit_lock.pending_commit.as_ref().unwrap());
                    },
                    false => {
                        inner
                            .segment_infos
                            .update_generation(to_sync.as_ref().unwrap());
                    },
                }
            }
            res
        })();
        match result {
            Ok(()) => {},
            Err(e) => {
                self.tragic_event(&e, "startCommit");
                return Err(e);
            },
        }

        self.test_point("finishStartCommit");
        Ok(())
    }

    /// This method should be called on a tragic event, i.e. if a downstream class of the writer hits
    /// an unrecoverable exception. This method does not rethrow the tragic event exception.
    ///
    /// Note: This method will not close the writer, but it can be called from any location without
    /// respecting any lock order.
    ///
    /// @lucene.internal
    fn on_tragic_event(&self, _tragedy: &LuceneError, _location: &str) -> Result<()> {
        // TODO
        Ok(())
    }

    /// This method set the tragic exception unless it's already set and closes the writer if necessary.
    /// Note this method will not rethrow the throwable passed to it.
    fn tragic_event(&self, _tragedy: &LuceneError, _location: &str) {
        // TODO
    }

    fn maybe_close_on_tragic_event(&self) -> Result<()> {
        // TODO
        Ok(())
    }

    pub fn get_tragic_exception(&self) -> TragicException {
        self.tragedy.clone()
    }
    pub(crate) fn is_deleter_closed(&self) -> Result<bool> {
        let inner = self.inner.lock();
        inner.deleter.is_closed(self)
    }

    fn test_point(&self, message: &str) {
        if self.enable_test_points {
            debug_assert!(self.info_stream.enabled("TP"));
            self.info_stream.message("TP", message);
        }
    }
    pub(crate) fn nrt_is_current(&self, version: i64) -> Result<bool> {
        let inner = self.inner.lock();
        self.do_ensure_open(true)?;

        let is_current = version == inner.segment_infos.get_version()
            && !self.doc_writer.any_changes(None)
            && !self.buffered_updates_stream.any()
            && !self.reader_pool.any_doc_values_changes();

        if self.info_stream.enabled("IW") && !is_current {
            self.info_stream.message(
                "IW",
                &format!(
                    "nrtIsCurrent: infoVersion matches: {}; DW changes: {}; BD changes: {}",
                    version == inner.segment_infos.get_version(),
                    self.doc_writer.any_changes(None),
                    self.buffered_updates_stream.any(),
                ),
            );
        }

        Ok(is_current)
    }

    fn delete_new_files<'a, I>(&self, files: I) -> Result<()>
    where
        I: IntoIterator<Item = &'a String>,
    {
        let inner = self.inner.lock();
        inner.deleter.delete_new_files(files)
    }

    fn flush_failed(&self, files: HashSet<String>) -> Result<()> {
        let inner = self.inner.lock();
        inner.deleter.delete_new_files(files.iter())
    }

    fn publish_flushed_segments(&self, forced: bool) -> Result<()> {
        let c = |mut ticket: FlushTicket<D>| {
            let buffered_updates = ticket.take_frozen_updates();
            ticket.mark_published();
            let new_segment = ticket.get_flushed_segment();
            match new_segment {
                // this is a flushed global deletes package - not a segment
                None => {
                    if let Some(buffered_updates) = buffered_updates
                        && buffered_updates.any()
                    {
                        if self.info_stream.enabled("IW") {
                            self.info_stream.message(
                                "IW",
                                &format!("flush: push buffered updates: {buffered_updates:?}"),
                            );
                        }
                        self.publish_frozen_updates(buffered_updates, None)?;
                    }
                },
                Some(seg) => {
                    if self.info_stream.enabled("IW") {
                        self.info_stream.message(
                            "IW",
                            &format!(
                                "publishFlushedSegment seg-private updates={:?}",
                                seg.segment_updates
                            ),
                        );
                    }
                    if seg.segment_updates.is_some() && self.info_stream.enabled("DW") {
                        self.info_stream.message(
                            "IW",
                            &format!(
                                "flush: push buffered seg private updates: {:?}",
                                seg.segment_updates
                            ),
                        );
                    }
                    self.publish_flushed_segment(
                        seg.segment_info.take().unwrap(),
                        seg.field_infos.clone(),
                        seg.segment_updates.take(),
                        buffered_updates,
                        seg.sort_map.take(),
                    )?;
                },
            }
            Ok(())
        };
        self.doc_writer.purge_flush_tickets(forced, c)?;
        Ok(())
    }

    /// Record that the files referenced by this SegmentInfos are still in use.
    pub fn inc_ref_deleter(
        &self,
        segment_infos: &SegmentInfos<D>,
        inner: Option<&mut Inner<D>>,
    ) -> Result<()> {
        let inner = match inner {
            Some(inner) => inner,
            None => &mut *self.inner.lock(),
        };
        self.do_ensure_open(true)?;
        inner.deleter.inc_ref_from_segment(segment_infos, false)?;
        if self.info_stream.enabled("IW") {
            self.info_stream.message(
                "IW",
                &format!(
                    "incRefDeleter for NRT reader version={} segments={}",
                    segment_infos.get_version(),
                    self.seg_string_from_infos(segment_infos.segments.values())?
                ),
            );
        }
        Ok(())
    }
    /// Record that the files referenced by this [`SegmentInfos`] are no longer in use.
    /// Only call this if you are sure you previously called [`Self::inc_ref_deleter`].
    pub fn dec_ref_deleter(
        &self,
        segment_infos: &SegmentInfos<D>,
        inner: Option<&mut Inner<D>>,
    ) -> Result<()> {
        let inner = match inner {
            Some(inner) => inner,
            None => &mut *self.inner.lock(),
        };
        self.do_ensure_open(true)?;
        inner.deleter.dec_ref_from_segment(segment_infos)?;
        if self.info_stream.enabled("IW") {
            self.info_stream.message(
                "IW",
                &format!(
                    "decRefDeleter for NRT reader version={} segments={}",
                    segment_infos.get_version(),
                    self.seg_string_from_infos(segment_infos.segments.values())?
                ),
            );
        }
        Ok(())
    }

    /// Processes all events and might trigger a merge if the given `seq_no` is negative.
    ///
    /// # Arguments
    ///
    /// * `seq_no` — if less than 0, this method will process events; otherwise it's a no-op.
    ///
    /// # Returns
    ///
    /// The given `seq_no` inverted if negative.
    fn maybe_process_events(&self, mut seq_no: i64) -> Result<i64> {
        if seq_no < 0 {
            seq_no = -seq_no;
            self.process_events(true)?;
        }
        Ok(seq_no)
    }

    fn process_events(&self, trigger_merge: bool) -> Result<()> {
        if self.tragedy.lock().is_none() {
            self.event_queue.process_events(self)?;
        }

        if trigger_merge {
            let policy = self.config.get_merge_policy();
            self.maybe_merge_with_max_num_segments(
                policy,
                MergeTrigger::SegmentFlush,
                UNBOUNDED_MAX_MERGE_SEGMENTS,
            )?;
        }
        Ok(())
    }

    /// Anything that will add N docs to the index should reserve first to make sure it's allowed.
    ///
    /// # Errors
    ///
    /// Returns [`LuceneError::IllegalArgument`] if it's not allowed.
    fn reserve_docs(&self, added_num_docs: i64) -> Result<()> {
        debug_assert!(added_num_docs >= 0);

        if self.adjust_pending_num_docs(added_num_docs) > ACTUAL_MAX_DOCS as i64 {
            // Reserve failed: put the docs back and throw error
            self.adjust_pending_num_docs(-added_num_docs);
            return self.too_many_docs(added_num_docs);
        }
        Ok(())
    }
    /// Does a best-effort check, that the current index would accept this many additional docs, but
    /// does not actually reserve them.
    /// # Errors
    ///
    /// Returns [`LuceneError::IllegalArgument`] if there would be too many docs.
    fn test_reserve_docs(&self, added_num_docs: i64) -> Result<()> {
        debug_assert!(added_num_docs >= 0);

        if self.pending_num_docs.load(Ordering::Acquire) + added_num_docs > ACTUAL_MAX_DOCS as i64 {
            return self.too_many_docs(added_num_docs);
        }
        Ok(())
    }
    fn too_many_docs(&self, added_num_docs: i64) -> Result<()> {
        debug_assert!(added_num_docs >= 0);
        Err(LuceneError::illegal_argument(format!(
            "number of documents in the index cannot exceed {} (current document count is {}; added numDocs is {})",
            ACTUAL_MAX_DOCS,
            self.pending_num_docs.load(Ordering::Acquire),
            added_num_docs
        )))
    }
    /// Returns the number of documents in the index including documents are being added (i.e.,
    /// reserved).
    pub fn get_pending_num_docs(&self) -> i64 {
        self.pending_num_docs.load(Ordering::Acquire)
    }
    /// Returns the highest sequence number across all completed operations,
    /// or 0 if no operations have finished yet.
    /// Still in-flight operations (in other threads) are not counted until they finish.
    pub fn get_max_completed_sequence_number(&self) -> Result<i64> {
        self.ensure_open()?;
        Ok(self.doc_writer.get_max_completed_sequence_number())
    }
    fn adjust_pending_num_docs(&self, num_docs: i64) -> i64 {
        let count = self.pending_num_docs.fetch_add(num_docs, Ordering::AcqRel) + num_docs;
        debug_assert!(count >= 0, "pendingNumDocs is negative: {}", count);
        count
    }

    fn is_fully_deleted(
        &self,
        readers_and_updates: &ReadersAndUpdates<D>,
        info: &SegmentCommitInfo<D>,
    ) -> Result<bool> {
        if readers_and_updates.is_fully_deleted(info)? {
            debug_assert!(self.inner.is_locked());
            return Ok(!readers_and_updates
                .keep_fully_deleted_segment(self.config.get_merge_policy(), info)?);
        }
        Ok(false)
    }

    pub(crate) fn release(
        &self,
        readers_and_updates: &ReadersAndUpdates<D>,
        inner: &mut Inner<D>, // Same to Java's Thread.holdsLock(this)
    ) -> Result<()> {
        self.release_with_assert(readers_and_updates, true, inner, None)
    }

    fn release_with_assert(
        &self,
        readers_and_updates: &ReadersAndUpdates<D>,
        assert_live_info: bool,
        inner: &mut Inner<D>, // Same to Java's Thread.holdsLock(this)
        merge_info: Option<&mut SegmentCommitInfo<D>>,
    ) -> Result<()> {
        let info_id = &readers_and_updates.info_id;
        let info = match inner.segment_infos.info_mut(info_id) {
            Some(info) => Some(info),
            None => merge_info,
        };
        if self.reader_pool.release(
            readers_and_updates,
            assert_live_info,
            info,
            &self.global_field_number_map.lock(),
        )? {
            // if we write anything here we have to hold the lock otherwise IDF will delete files
            // underneath us
            self.check_point_no_sis(inner)?;
        }
        Ok(())
    }

    pub(crate) fn get_pooled_instance(
        &self,
        info: &SegmentCommitInfo<D>,
        create: bool,
        sort_map: Option<Arc<DocMapImpl>>,
    ) -> Result<Option<Arc<ReadersAndUpdates<D>>>> {
        self.do_ensure_open(false)?;
        self.reader_pool.get(info, create, sort_map)
    }
    /// Translates a frozen packet of delete term/query, or doc values updates, into their actual
    /// doc IDs in the index, and applies the change. This is a heavy operation and is done concurrently
    /// by incoming indexing threads. This method will return immediately without blocking if another
    /// thread is currently applying the package. To ensure the packet has been applied,
    /// [`IndexWriter::force_apply(FrozenBufferedUpdates)`](Self::force_apply) must be called.
    pub(crate) fn try_apply(&self, updates: &FrozenBufferedUpdates) -> Result<bool> {
        let _guard = updates.as_ref().try_lock();
        if _guard.is_some() {
            self.force_apply(updates)?;
            return Ok(true);
        }
        Ok(true)
    }
    /// Translates a frozen packet of delete term/query, or doc values updates, into their actual
    /// doc IDs in the index, and applies the change.
    /// This is a heavy operation and is done concurrently by incoming indexing threads.
    pub(crate) fn force_apply(&self, updates: &FrozenBufferedUpdates) -> Result<()> {
        let _guard = updates.lock();

        if updates.is_applied() {
            return Ok(());
        }
        let start_ns = Instant::now();
        debug_assert!(updates.any());
        let mut seen_segments: HashSet<String> = HashSet::new();
        let mut iter: i32 = 0;
        let mut total_segment_count: i32 = 0;
        let mut total_del_count: i64 = 0;
        let mut finished = false;

        // Optimistic concurrency: assume we are free to resolve the deletes against all current
        // segments in the index, despite that
        // concurrent merges are running.  Once we are done, we check to see if a merge completed
        // while we were running.  If so, we must retry
        // resolving against the newly merged segment(s).  Eventually no merge finishes while we were
        // running and we are done.
        loop {
            let message_prefix = if iter == 0 {
                String::new()
            } else {
                format!("iter {iter} ")
            };

            let iter_start = Instant::now();
            let merge_gen_start = self.merge_finished_gen.load(Ordering::Acquire);

            let mut del_files: HashSet<String> = HashSet::new();
            let mut seg_states;

            {
                let mut inner = self.inner.lock();
                let v = self.get_infos_to_apply(updates, &inner);
                let keys = match &v {
                    InfoFrom::None => break,
                    InfoFrom::Updates => {
                        vec![updates.private_segment.as_ref().unwrap()]
                    },
                    InfoFrom::All => inner.segment_infos.segments_idx.iter().collect::<Vec<_>>(),
                };
                for id in &keys {
                    let info = inner
                        .segment_infos
                        .info(id)
                        .expect("segment info not found");
                    del_files.extend(info.files()?);
                }
                let v = match v {
                    InfoFrom::None => return Err(LuceneError::unreachable("should not be here")),
                    InfoFrom::Updates => Some(updates.private_segment.as_ref().unwrap()),
                    // all segments
                    InfoFrom::All => None,
                };
                // Must open while holding IW lock so that e.g. segments are not merged
                // away, dropped from 100% deletions, etc., before we can open the readers
                seg_states =
                    self.open_segment_states(v, &mut seen_segments, updates.del_gen(), &mut inner)?;

                if seg_states.is_empty() {
                    if self.info_stream.enabled("BD") {
                        self.info_stream.message("BD", "packet matches no segments");
                    }
                    break;
                }

                if self.info_stream.enabled("BD") {
                    self.info_stream.message(
                        "BD",
                        &format!(
                            "{}now apply del packet ({}) to {} segments, mergeGen {}",
                            message_prefix,
                            self,
                            seg_states.len(),
                            merge_gen_start
                        ),
                    );
                }

                total_segment_count += seg_states.len() as i32;
                // Important, else IFD may try to delete our files while we are still using them,
                // if e.g. a merge finishes on some of the segments we are resolving on:
                inner.deleter.inc_ref_files(&del_files);
            }

            let mut success = false;
            let mut del_count = 0;
            {
                let result: Result<()> = (|| {
                    // don't hold IW monitor lock here so threads are free concurrently resolve
                    // deletes/updates:
                    del_count =
                        updates.apply(&seg_states, &self.inner.lock().segment_infos.segments)?;
                    success = true;
                    Ok(())
                })();
                let mut inner = self.inner.lock();
                self.finish_apply(&mut seg_states, success, del_files, &mut inner)?;

                match result {
                    Ok(_) => {},
                    Err(e) => {
                        return Err(e);
                    },
                }
            }
            // Since we just resolved some more deletes/updates, now is a good time to write them:
            self.write_some_doc_values_updates()?;
            // It's OK to add this here, even if the while loop retries, because delCount only includes
            // newly
            // deleted documents, on the segments we didn't already do in previous iterations:
            total_del_count += del_count;

            if self.info_stream.enabled("BD") {
                self.info_stream.message(
                    "BD",
                    &format!(
                        "{}done inner apply del packet to {} segments; {} new deletes/updates; took {:.3} sec",
                        message_prefix,
                        seg_states.len(),
                        del_count,
                        iter_start.elapsed().as_secs_f64(),
                    ),
                );
            }

            if updates.private_segment.is_some() {
                // No need to retry for a segment-private packet: the merge that folds in our private
                // segment already waits for all deletes to
                // be applied before it kicks off, so this private segment must already not be in the set
                // of merging segments
                break;
            }

            {
                // Must sync on a writer here so that IW.mergeCommit is not running concurrently, so that if
                // we exit, we know mergeCommit will succeed
                // in pulling all our delGens into a merge:
                let _inner = self.inner.lock();
                let merge_gen_cur = self.merge_finished_gen.load(Ordering::Acquire);

                if merge_gen_cur == merge_gen_start {
                    // Must do this while still holding IW lock else a merge could finish and skip carrying
                    // over our updates:

                    // Record that this packet is finished:
                    self.buffered_updates_stream.finished(updates);
                    finished = true;
                    // No merge finished while we were applying, so we are done!
                    break;
                }
                drop(_inner)
            }

            if self.info_stream.enabled("BD") {
                self.info_stream.message(
                    "BD",
                    &format!(
                        "{}concurrent merges finished; move to next iter",
                        message_prefix
                    ),
                );
            }
            // A merge completed while we were running.  In this case, that merge may have picked up
            // some of the updates we did, but not
            // necessarily all of them, so we cycle again, re-applying all our updates to the newly
            // merged segment.

            iter += 1;
        }
        if !finished {
            // Record that this packet is finished:
            self.buffered_updates_stream.finished(updates);
        }

        if self.info_stream.enabled("BD") {
            let mut message = format!(
                "done apply del packet ({}) to {} segments; {} new deletes/updates; took {:.3} sec",
                self,
                total_segment_count,
                total_del_count,
                start_ns.elapsed().as_secs_f64(),
            );
            if iter > 0 {
                message.push_str(&format!("; {} iters due to concurrent merges", iter + 1));
            }
            message.push_str(&format!(
                "; {} packets remain",
                self.buffered_updates_stream.get_pending_updates_count()
            ));
            self.info_stream.message("BD", &message);
        }
        Ok(())
    }

    /// Returns the [`SegmentCommitInfo`]'s id that this packet is supposed to apply its deletes to,
    /// or `None` if the private segment was already merged away.
    fn get_infos_to_apply(&self, updates: &FrozenBufferedUpdates, inner: &Inner<D>) -> InfoFrom {
        if let Some(private_seg) = &updates.private_segment {
            if inner.segment_infos.contains(private_seg) {
                InfoFrom::Updates
            } else {
                if self.info_stream.enabled("BD") {
                    self.info_stream.message(
                        "BD",
                        "private segment already gone; skip processing updates",
                    );
                }
                InfoFrom::None
            }
        } else {
            InfoFrom::All
        }
    }
    pub(crate) fn finish_apply(
        &self,
        seg_states: &mut [SegmentState<D>],
        success: bool,
        del_files: HashSet<String>,
        inner: &mut Inner<D>, // we hold lock
    ) -> Result<()> {
        let close_res = self.close_segment_states(seg_states, success, inner);
        inner.deleter.dec_ref(del_files.iter())?;
        let result = close_res?;

        if result.any_new_deletes {
            self.maybe_merge.store(true, Ordering::Release);
            self.checkpoint(inner)?;
        }

        if let Some(all) = result.all_deleted.as_ref() {
            if self.info_stream.enabled("IW") {
                // let segs = all.join(",");
                // self.info_stream
                //     .message("IW", &format!("drop 100% deleted segments: {}", segs));
            }
            for seg_id in all {
                self.drop_deleted_segment(seg_id, inner)?;
            }
            self.checkpoint(inner)?;
        }

        Ok(())
    }

    /// Close segment states previously opened with `open_segment_states`.
    pub(crate) fn close_segment_states(
        &self,
        seg_states: &mut [SegmentState<D>],
        success: bool,
        inner: &mut Inner<D>, // we hold lock
    ) -> Result<ApplyDeletesResult> {
        let mut all_deleted = Vec::new();
        let mut tot_del_count: i64 = 0;

        let res: Result<()> = (|| {
            for seg_state in seg_states.iter_mut() {
                if success {
                    let info_id = &seg_state.rld.info_id;
                    let info = match inner.segment_infos.info(info_id) {
                        Some(info) => info,
                        None => Err(LuceneError::illegal_state(
                            "could not find segment info from IndexWriter#segment_infos",
                        ))?,
                    };
                    let before = seg_state.start_del_count as i64;
                    let current = seg_state.rld.get_del_count(info) as i64;
                    tot_del_count += current - before;

                    let full_del_count = seg_state.rld.get_del_count(info);
                    debug_assert!(
                        full_del_count <= info.info.max_doc()?,
                        "{} > {}",
                        full_del_count,
                        info.info.max_doc()?
                    );

                    // TODO: 这里没有加入MergePolic的判断
                    if seg_state.rld.is_fully_deleted(info)? {
                        all_deleted.push(
                            seg_state
                                .rld
                                .inner
                                .lock()
                                .reader
                                .as_ref()
                                .unwrap()
                                .original_si_id
                                .clone(),
                        );
                    }
                }
            }
            Ok(())
        })();

        for s in seg_states.iter_mut() {
            let _ = s.close(self, inner);
        }
        res?;

        if self.info_stream.enabled("BD") {
            self.info_stream.message(
                "BD",
                &format!(
                    "closeSegmentStates: {} new deleted documents; pool {} packets; bytesUsed={}",
                    tot_del_count,
                    self.buffered_updates_stream.get_pending_updates_count(),
                    self.reader_pool.ram_bytes_used()
                ),
            );
        }

        let result = ApplyDeletesResult {
            any_new_deletes: tot_del_count > 0,
            all_deleted: if all_deleted.is_empty() {
                None
            } else {
                Some(all_deleted)
            },
        };
        Ok(result)
    }
    /// Tests should use this method to snapshot the current segmentInfos to have a consistent view
    pub(crate) fn clone_segment_infos(&self) -> Result<SegmentInfos<D>> {
        let inner = self.inner.lock();
        inner.segment_infos.try_clone()
    }
    /// Returns accurate [`DocStats`] for this writer.
    /// The `num_docs` for instance can change after `max_doc` is fetched
    /// that causes `num_docs` to be greater than `max_doc` which makes it
    /// hard to get accurate document stats from `IndexWriter`.
    pub fn get_doc_stats(&self) -> Result<DocStats> {
        let inner = self.inner.lock();
        self.ensure_open()?;

        let mut num_docs = self.doc_writer.get_num_docs();
        let mut max_doc = num_docs;

        for info in inner.segment_infos.iter() {
            let seg_max_doc = info.info.max_doc()?;
            max_doc += seg_max_doc;
            num_docs += seg_max_doc - self.num_deleted_docs(info)?;
        }

        debug_assert!(
            max_doc >= num_docs,
            "max_doc is less than num_docs: {} < {}",
            max_doc,
            num_docs
        );

        Ok(DocStats::new(max_doc, num_docs))
    }
    /// Opens SegmentReader and inits SegmentState for each segment.
    pub(crate) fn open_segment_states(
        &self,
        info_from: Option<&String>,
        already_seen: &mut HashSet<String>,
        del_gen: i64,
        inner: &mut Inner<D>, // we hold lock
    ) -> Result<Vec<SegmentState<D>>> {
        let mut seg_states = Vec::new();

        let result: Result<()> = (|| {
            let infos = match info_from {
                // all segments, `segments_idx`'s values are sorted by segment name
                None => inner.segment_infos.segments_idx.iter().collect::<Vec<_>>(),
                Some(it) => vec![it],
            };
            for info_id in infos {
                let info = inner.segment_infos.info(info_id).unwrap();
                if info.get_buffered_deletes_gen() <= del_gen && !already_seen.contains(info_id) {
                    let rld = self
                        .get_pooled_instance(info, true, None)?
                        .expect("should always be Some");
                    let seg_state = SegmentState::new(rld, info)?;
                    seg_states.push(seg_state);
                    already_seen.insert(info_id.clone());
                }
            }
            Ok(())
        })();

        if let Err(e) = result {
            let mut errors = Vec::new();
            for s in seg_states.iter_mut() {
                let res: Result<()> = s.close(self, inner);
                match res {
                    Ok(_) => continue,
                    Err(se) => {
                        errors.push(se);
                    },
                }
            }
            return if errors.is_empty() {
                Err(e)
            } else {
                // TODO IMPORTANT 这里没有正确的嵌套error
                Err(LuceneError::illegal_state(format!("{} {:?}", e, errors)))
            };
        }

        Ok(seg_states)
    }
    fn validate(&self, info: &SegmentCommitInfo<D>) -> Result<()> {
        if !Arc::ptr_eq(&info.info.dir, &self.directory_orig) {
            return Err(LuceneError::illegal_argument(
                "SegmentCommitInfo must be from the same directory",
            ));
        }
        Ok(())
    }
    pub(crate) fn get_reader(
        &self,
        apply_all_deletes: bool,
        write_all_deletes: bool,
    ) -> Result<StandardDirectoryReaderType<D>> {
        self.do_ensure_open(true)?;

        if write_all_deletes && !apply_all_deletes {
            return Err(LuceneError::illegal_argument(
                "applyAllDeletes must be true when writeAllDeletes=true",
            ));
        }

        let _t_start = Instant::now();

        if self.info_stream.enabled("IW") {
            self.info_stream.message("IW", "flush at getReader");
        }

        // Do this up front before flushing so that the readers
        // obtained during this flush are pooled, the first time
        // this method is called:
        self.reader_pool.enable_reader_pooling();

        if let Some(ref s) = self.sub {
            s.do_before_flush()?;
        }

        let mut any_changes: bool = false;
        let max_full_flush_merge_wait_millis = self.config.get_max_full_flush_merge_wait_millis();

        /*
         * for releasing a NRT reader, we must ensure that
         * DW doesn't add any segments or deletes until we are
         * done with creating the NRT DirectoryReader.
         * We release the two-stage full flush after we are done opening the
         * directory reader!
         */
        // let mut on_get_reader_merges = None;
        let _stop_collecting_merged_readers = AtomicBool::new(false);
        // let mut merged_readers =
        //     std::collections::HashMap::new();
        let mut opened_read_only_clones = HashMap::new();

        let mut reader_factory = IOFunctionImpl::new(self, &mut opened_read_only_clones);
        let _opening_segment_infos: Option<SegmentInfos<D>> = None;
        let result1 = (|| {
            /*
            This is the essential part of the getReader method. We need to take care of the following things:
             - flush all currently in-memory DWPTs to disk
             - apply all deletes & updates to new and to the existing DWPTs
             - prevent flushes and applying deletes of concurrently indexing DWPTs to be applied
             - open an SDR on the updated SIS

            In order to prevent concurrent flushes, we call DocumentsWriter#flushAllThreads that swaps out the deleteQueue
            (this enforces a happened before relationship between this and the subsequent full flush) and informs the
            FlushControl (#markForFullFlush()) that it should prevent any new DWPTs from flushing until we are done
            (DocumentsWriter#finishFullFlush(boolean)). All this is guarded by the fullFlushLock to prevent multiple
            full flushes from happening concurrently. Once the DocWriter has initiated a full flush, we can sequentially flush
            and apply deletes & updates to the written segments without worrying about concurrently indexing DWPTs. The important
            aspect is that it all happens between DocumentsWriter#flushAllThread() and DocumentsWriter#finishFullFlush(boolean)
            since once the flush is marked as done deletes start to be applied to the segments on disk without guarantees that
            the corresponding added documents (in the update case) are flushed and visible when opening an SDR.
            */

            let mut success = false;
            {
                let _full_flush_lock = self.full_flush_lock.lock();
                let result2 = (|| {
                    any_changes = self.doc_writer.flush_all_threads(self, &self.config)? < 0;
                    if !any_changes {
                        self.flush_count.fetch_add(1, Ordering::AcqRel);
                    }
                    self.publish_flushed_segments(true)?;
                    self.process_events(false)?;
                    if apply_all_deletes {
                        self.apply_all_deletes_and_updates()?;
                    }
                    let r = {
                        let mut inner = self.inner.lock();
                        // NOTE: we cannot carry doc values updates in memory yet, so we always must write them
                        // through to disk and re-open each
                        // SegmentReader:

                        // TODO: we could instead just clone SIS and pull/incref readers in sync'd block, and
                        // then do this w/o IW's lock?
                        // Must do this sync's on IW to prevent a merge from completing at the last second and
                        // failing to write its DV updates:
                        self.write_reader_pool(write_all_deletes, &mut inner)?;
                        // Prevent segmentInfos from changing while opening the
                        // reader; in theory we could instead do similar retry logic,
                        // just like we do when loading segments_N
                        let r = open_with_reader_function(
                            self,
                            &mut reader_factory,
                            None,
                            &mut inner,
                            apply_all_deletes,
                            write_all_deletes,
                        )?;

                        if max_full_flush_merge_wait_millis > 0 {
                            // TODO IMPORTANT 段的合并未完成
                        }
                        if self.info_stream.enabled("IW") {
                            // self.info_stream.message("IW", format!("return reader version={} reader={}", ));
                        }
                        r
                    };
                    success = true;
                    Ok(r)
                })();
                self.doc_writer.finish_full_flush(success, &self.config)?;
                if success {
                    self.process_events(false)?;
                    if let Some(ref s) = self.sub {
                        s.do_after_flush()?
                    }
                } else if self.info_stream.enabled("IW") {
                    self.info_stream
                        .message("IW", "hit exception during NRT reader");
                }
                result2
            }
        })();
        // TODO: 返回之前需要关闭一些 但是rust Lucene不需要？
        match result1 {
            Ok(v) => Ok(v),
            Err(e) => {
                self.tragic_event(&e, "get_reader");
                Err(e)
            },
        }
    }
    pub(crate) fn get_segment_infos_version(&self) -> i64 {
        let inner = self.inner.lock();
        inner.segment_infos.get_version()
    }
}
/// If `open(IndexWriter)` has been called (ie, this writer is in near
/// real-time mode), then after a merge completes, this class can be invoked to warm the reader on
/// the newly merged segment, before the merge commits. This is not required for near real-time
/// search, but will reduce search latency on opening a new near real-time reader after a merge
/// completes.
///
/// # Experimental
///
/// **NOTE**: `warm(LeafReader)` is called before any deletes have been carried
/// over to the merged segment.
pub trait IndexReaderWarmer {
    fn warm<LR>(reader: LR) -> Result<()>
    where
        LR: LeafReader;
}

struct IOConsumerImpl1<'a, D, L, B>
where
    D: Directory,
    L: LiveIndexWriterConfig,
    B: IndexWriterBase,
{
    index_writer: &'a IndexWriter<D, L, B>,
}
impl<'a, D, L, B> IOConsumerImpl1<'a, D, L, B>
where
    D: Directory,
    L: LiveIndexWriterConfig,
    B: IndexWriterBase,
{
    fn new(index_writer: &'a IndexWriter<D, L, B>) -> Self {
        Self { index_writer }
    }
}
impl<'a, D, L, B> IOConsumer<HashSet<String>> for IOConsumerImpl1<'a, D, L, B>
where
    D: Directory,
    L: LiveIndexWriterConfig,
    B: IndexWriterBase,
{
    fn accept_owner(&mut self, input: HashSet<String>) -> Result<()> {
        self.index_writer.delete_new_files(input.iter())
    }
}

struct DocMapIMpl2<DM1, DM2>
where
    DM1: DocMap,
    DM2: DocMap,
{
    compaction_doc_map: DM1,
    reorder_doc_map: DM2,
}
impl<DM1, DM2> DocMapIMpl2<DM1, DM2>
where
    DM1: DocMap,
    DM2: DocMap,
{
    fn new(compaction_doc_map: DM1, reorder_doc_map: DM2) -> Self {
        Self {
            compaction_doc_map,
            reorder_doc_map,
        }
    }
}

impl<DM1, DM2> DocMap for DocMapIMpl2<DM1, DM2>
where
    DM1: DocMap,
    DM2: DocMap,
{
    fn get(&self, doc_id: i32) -> Result<i32> {
        let intermediate_doc_id = self.reorder_doc_map.get(doc_id)?;
        self.compaction_doc_map.get(intermediate_doc_id)
    }
}

struct DocMapImpl1<DM>
where
    DM: crate::core::index::sorter::DocMap,
{
    doc_map: DM,
    max_doc: i32,
    current_doc_base: i32,
}
impl<DM> DocMapImpl1<DM>
where
    DM: crate::core::index::sorter::DocMap,
{
    fn new(doc_map: DM, max_doc: i32, current_doc_base: i32) -> Self {
        Self {
            doc_map,
            max_doc,
            current_doc_base,
        }
    }
}
impl<DM> DocMap for DocMapImpl1<DM>
where
    DM: crate::core::index::sorter::DocMap,
{
    fn get(&self, doc_id: i32) -> Result<i32> {
        CoreHelper::check_index(doc_id as usize, self.max_doc as usize)?;
        self.doc_map.old_to_new(self.current_doc_base + doc_id)
    }
}

pub(crate) struct BitsImpl<B1, B2>
where
    B1: Bits,
    B2: Bits,
{
    hard_live_docs: B1,
    wrapped_live_docs: B2,
    id: Identity,
}

impl<B1, B2> HasIdentity for BitsImpl<B1, B2>
where
    B1: Bits,
    B2: Bits,
{
    fn identity(&self) -> &Identity {
        &self.id
    }
}

impl<B1, B2> Bits for BitsImpl<B1, B2>
where
    B1: Bits,
    B2: Bits,
{
    fn get(&self, index: usize) -> Result<bool> {
        Ok(self.hard_live_docs.get(index)? && self.wrapped_live_docs.get(index)?)
    }

    fn length(&self) -> usize {
        self.hard_live_docs.length()
    }
}
impl<D, L, B> MergeContext<D> for IndexWriter<D, L, B>
where
    D: Directory,
    L: LiveIndexWriterConfig,
    B: IndexWriterBase,
{
    /// Returns the number of deletes a merge would claim back if the given segment is merged.
    ///
    /// See [`MergePolicy::num_deletes_to_merge`].
    ///
    /// # Parameters
    /// * `info` — the segment to get the number of deletes for.
    fn num_deletes_to_merge(&self, info: &SegmentCommitInfo<D>) -> Result<i32> {
        self.do_ensure_open(false)?;
        self.validate(info)?;

        let merge_policy = self.config.get_merge_policy();

        let num_deletes_to_merge = match self.get_pooled_instance(info, false, None)? {
            Some(rld) => rld.num_deletes_to_merge(merge_policy, info)?,
            None => {
                // If we don't have a pooled instance, just return hard deletes; this is safe.
                info.get_del_count()
            },
        };

        debug_assert!(
            num_deletes_to_merge <= info.info.max_doc()?,
            "numDeletesToMerge: {} > maxDoc: {}",
            num_deletes_to_merge,
            info.info.max_doc()?
        );

        Ok(num_deletes_to_merge)
    }

    /// Obtain the number of deleted docs for a pooled reader.
    ///
    /// If the reader isn't being pooled, the segmentInfo's `delCount` is returned.
    fn num_deleted_docs(&self, info: &SegmentCommitInfo<D>) -> i32 {
        self.do_ensure_open(false).unwrap();
        self.validate(info).unwrap();

        if let Some(rld) = self.get_pooled_instance(info, false, None).unwrap() {
            // get the full count from here since SCI might change concurrently
            rld.get_del_count(info)
        } else {
            let del_count = info.get_del_count_with_soft_deletes(self.soft_deletes_enabled);
            debug_assert!(
                del_count <= info.info.max_doc().unwrap(),
                "delCount: {} maxDoc: {}",
                del_count,
                info.info.max_doc().unwrap()
            );
            del_count
        }
    }

    fn get_info_stream(&self) -> InfoStreamMT {
        self.info_stream.clone()
    }

    /// **Expert:** to be used by a [`MergePolicy`] to avoid selecting merges for segments already
    /// being merged.
    ///
    /// The returned collection is **not cloned**, and thus is only safe to access if you
    /// hold `IndexWriter`'s lock (which you do when `IndexWriter` invokes the `MergePolicy`).
    ///
    /// The returned set is **unmodifiable**.
    fn get_merging_segments(&self, inner: Option<&Inner<D>>) -> HashSet<String> {
        let inner = match inner {
            Some(i) => i,
            None => &*self.inner.lock(),
        };
        inner.merging_segments.clone()
    }
}
pub(crate) struct Merges {
    merges_enabled: bool,
}
impl Merges {
    fn new() -> Self {
        Self {
            merges_enabled: true,
        }
    }
}

impl Merges {
    pub(crate) fn are_enabled<D>(&self, _inner: &Inner<D>) -> bool
    where
        D: Directory,
    {
        self.merges_enabled
    }

    pub(crate) fn disable<D>(&mut self, _inner: &Inner<D>)
    where
        D: Directory,
    {
        self.merges_enabled = false;
    }

    pub(crate) fn enable<D, L, B>(
        &mut self,
        writer: &IndexWriter<D, L, B>,
        _inner: &Inner<D>,
    ) -> Result<()>
    where
        D: Directory,
        L: LiveIndexWriterConfig,
        B: IndexWriterBase,
    {
        writer.ensure_open()?;
        self.merges_enabled = true;
        Ok(())
    }
}

pub(crate) struct IOConsumerImpl<'a, D, L, B>
where
    D: Directory,
    L: LiveIndexWriterConfig,
    B: IndexWriterBase,
{
    deleter: &'a mut IndexFileDeleter<D>,
    merge_readers: &'a mut HashMap<String, Arc<SegmentReader<D>>>,
    reader_factory: &'a mut IOFunctionImpl<'a, D, L, B>,
    stop_collecting_merged_readers: &'a AtomicBool,
}
impl<'a, D, L, B> IOConsumerImpl<'a, D, L, B>
where
    D: Directory,
    L: LiveIndexWriterConfig,
    B: IndexWriterBase,
{
    pub(crate) fn new(
        deleter: &'a mut IndexFileDeleter<D>,
        merge_readers: &'a mut HashMap<String, Arc<SegmentReader<D>>>,
        reader_factory: &'a mut IOFunctionImpl<'a, D, L, B>,
        stop_collecting_merged_readers: &'a AtomicBool,
    ) -> Self {
        Self {
            deleter,
            merge_readers,
            reader_factory,
            stop_collecting_merged_readers,
        }
    }
}
impl<'a, D, L, B> IOConsumer<SegmentCommitInfo<D>> for IOConsumerImpl<'a, D, L, B>
where
    D: Directory,
    L: LiveIndexWriterConfig,
    B: IndexWriterBase,
{
    fn accept_ref(&mut self, sci: &SegmentCommitInfo<D>) -> Result<()> {
        debug_assert!(
            !self.stop_collecting_merged_readers.load(Ordering::Acquire),
            "illegal state  merge reader must be not pulled since we already stopped waiting for merges"
        );
        let apply = self.reader_factory.apply(sci)?;
        self.merge_readers.insert(sci.info.name.clone(), apply);
        // we need to incRef the files of the opened SR otherwise it's possible that
        // another merge
        // removes the segment before we pass it on to the SDR
        self.deleter.inc_ref_files(sci.files()?);
        Ok(())
    }
}

pub(crate) struct IOFunctionImpl<'a, D, L, B>
where
    D: Directory,
    L: LiveIndexWriterConfig,
    B: IndexWriterBase,
{
    writer: &'a IndexWriter<D, L, B>,
    opened_read_only_clones: &'a mut HashMap<String, Arc<SegmentReader<D>>>,
}
impl<'a, D, L, B> IOFunctionImpl<'a, D, L, B>
where
    D: Directory,
    L: LiveIndexWriterConfig,
    B: IndexWriterBase,
{
    pub(crate) fn new(
        writer: &'a IndexWriter<D, L, B>,
        opened_read_only_clones: &'a mut HashMap<String, Arc<SegmentReader<D>>>,
    ) -> Self {
        Self {
            writer,
            opened_read_only_clones,
        }
    }
}
impl<'a, D, L, B> IOFunction<SegmentCommitInfo<D>, Arc<SegmentReader<D>>>
    for IOFunctionImpl<'a, D, L, B>
where
    D: Directory,
    L: LiveIndexWriterConfig,
    B: IndexWriterBase,
{
    fn apply(&mut self, sci: &SegmentCommitInfo<D>) -> Result<Arc<SegmentReader<D>>> {
        let rld = self.writer.get_pooled_instance(sci, true, None)?;
        match rld {
            Some(r) => match r.get_read_only_clone(&IOContext::default_io_context()?, sci)? {
                Some(segment_reader) => {
                    self.opened_read_only_clones
                        .insert(sci.info.name.clone(), segment_reader.clone());
                    Ok(segment_reader)
                },
                None => Err(LuceneError::illegal_state(
                    "should always be able to get read only clone",
                )),
            },
            None => Err(LuceneError::illegal_state(
                "should always be able to get pooled instance",
            )),
        }
    }
}
impl<D, L, B> Display for IndexWriter<D, L, B>
where
    D: Directory,
    L: LiveIndexWriterConfig,
    B: IndexWriterBase,
{
    fn fmt(&self, _f: &mut Formatter<'_>) -> std::fmt::Result {
        todo!()
    }
}
enum InfoFrom {
    None,
    Updates,
    All,
}

/// DocStats for this index
#[derive(Debug, Clone, Copy)]
pub struct DocStats {
    /// The total number of docs in this index, counting docs not yet flushed
    /// (still in the RAM buffer), and also counting deleted docs.
    ///
    /// **NOTE:** buffered deletions are not counted.
    /// If you really need these to be counted you should call [`IndexWriter::commit`] first.
    pub max_doc: i32,

    /// The total number of docs in this index, counting docs not yet flushed
    /// (still in the RAM buffer), but not counting deleted docs.
    pub num_docs: i32,
}

impl DocStats {
    pub fn new(max_doc: i32, num_docs: i32) -> Self {
        Self { max_doc, num_docs }
    }
}

pub trait IndexWriterBase {
    /// A hook for extending classes to execute operations after pending added and deleted documents have been flushed to the Directory
    /// but before the change is committed (new segments_N file written).
    fn do_after_flush(&self) -> Result<()> {
        Ok(())
    }
    /// A hook for extending classes to execute operations before pending added and deleted documents are flushed to the Directory.
    fn do_before_flush(&self) -> Result<()> {
        Ok(())
    }

    fn is_enable_test_points(&self) -> bool {
        false
    }
}
#[derive(Default)]
pub struct EmptyIndexWriterBase;
impl IndexWriterBase for EmptyIndexWriterBase {}
pub(crate) type TragicException = Arc<Mutex<Option<LuceneError>>>;

#[derive(Default)]
pub struct DocMapIndexWriter;
impl DocMap for DocMapIndexWriter {
    fn get(&self, _doc_id: i32) -> Result<i32> {
        todo!()
    }
}

pub(crate) struct FlushNotificationsImpl {
    event_queue: Arc<EventQueue>,
}
impl FlushNotificationsImpl {
    pub fn new(event_queue: Arc<EventQueue>) -> Self {
        Self { event_queue }
    }
}
impl FlushNotifications for FlushNotificationsImpl {
    fn delete_unused_files(&self, files: HashSet<String>) -> Result<()> {
        let event = EventEnum::A(EventImpl1::new(files));
        self.event_queue.add(event)
    }

    fn flush_failed<D>(&self, mut info: SegmentInfo<D>) -> Result<()>
    where
        D: Directory,
    {
        match info.take_files() {
            Ok(files) => {
                let event = EventEnum::B(EventImpl2::new(files));
                self.event_queue.add(event)
            },
            Err(_) => {
                // no-op
                Ok(())
            },
        }
    }

    fn after_segments_flushed<D, L, B>(&self, writer: &IndexWriter<D, L, B>) -> Result<()>
    where
        D: Directory,
        L: LiveIndexWriterConfig,
        B: IndexWriterBase,
    {
        writer.publish_flushed_segments(false)
    }

    fn on_tragic_event<D, L, B>(
        &self,
        event: LuceneError,
        message: &str,
        writer: &IndexWriter<D, L, B>,
    ) -> Result<()>
    where
        D: Directory,
        L: LiveIndexWriterConfig,
        B: IndexWriterBase,
    {
        writer.on_tragic_event(&event, message)
    }

    fn on_deletes_applied(&self) -> Result<()> {
        let event = EventEnum::C(EventImpl3);
        self.event_queue.add(event)
    }

    fn on_ticket_backlog(&self) -> Result<()> {
        let event = EventEnum::D(EventImpl4);
        self.event_queue.add(event)
    }
}

pub(crate) struct LongSupplierImpl {
    stream: Arc<BufferedUpdatesStream>,
}
impl LongSupplierImpl {
    pub fn new(stream: Arc<BufferedUpdatesStream>) -> Self {
        Self { stream }
    }
}
impl LongSupplier for LongSupplierImpl {
    fn get_as_long(&self) -> i64 {
        self.stream.get_completed_del_gen()
    }
}

use crate::core::codecs::field_infos_format::FieldInfosFormat;
use crate::core::codecs::segment_info_format::SegmentInfoFormat;
use crate::core::codecs::{Codec, CompoundFormat, LATEST_CODEC, get_default_code};
use crate::core::document::fields::Fields;
use crate::core::index::binary_doc_values_field_updates::BinaryDocValuesFieldUpdates;
use crate::core::index::buffered_updates::MAX_INT;
use crate::core::index::caching_merge_context::CachingMergeContext;
use crate::core::index::codec_reader::{CodecReader, CodecReaderEnum2};
use crate::core::index::directory_reader::directory_reader_util;
use crate::core::index::doc_values_field_updates::{
    DocValuesFieldIterator, DocValuesFieldUpdates, DocValuesFieldUpdatesBase,
};
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::doc_values_update::{
    BinaryDocValuesUpdate, DocValuesUpdate, DocValuesUpdateEnum, NumericDocValuesUpdate,
};
use crate::core::index::documents_writer_delete_queue::{DocumentsWriterDeleteQueue, Node};
use crate::core::index::documents_writer_flush_queue::FlushTicket;
use crate::core::index::field_infos::{FieldInfos, FieldNumbers, FieldNumbersLock};
use crate::core::index::index_commit::IndexCommit;
use crate::core::index::index_reader::{Identity, IndexReader};
use crate::core::index::index_writer_config::{DISABLE_AUTO_FLUSH, IndexWriterConfig, OpenMode};
use crate::core::index::indexable_field::IndexableField;
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::merge_policy::{MergePolicy, MergeStat, OneMerge};
use crate::core::index::merge_trigger::MergeTrigger;
use crate::core::index::numeric_doc_values_field_updates::NumericDocValuesFieldUpdates;
use crate::core::index::reader_pool::ReaderPool;
use crate::core::index::readers_and_updates::ReadersAndUpdates;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_merger::SegmentMerger;
use crate::core::index::segment_reader::SegmentReader;
use crate::core::index::slow_composite_codec_reader_wrapper::wrap;
use crate::core::index::sorter::DocMapImpl;
use crate::core::index::sorting_codec_reader::wrap_with_doc_map;
use crate::core::index::standard_directory_reader::{
    StandardDirectoryReaderType, open_with_reader_function,
};
use crate::core::index::term::Term;
use crate::core::index::{BytesRef, IndexFileNames};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::search::query::Query;
use crate::core::search::sort::Sort;
use crate::core::store::IOContext;
use crate::core::store::lock_validating_directory_wrapper::LockValidatingDirectoryWrapper;
use crate::core::store::tracking_directory_wrapper::TrackingDirectoryWrapper;
use crate::core::util::accountable::Accountable;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bits::Bits;
use crate::core::util::constants::Constants;
use crate::core::util::info_stream::{InfoStream, InfoStreamMT};
use crate::core::util::io_consumer::IOConsumer;
use crate::core::util::io_function::IOFunction;
use crate::core::util::unicode_util::UnicodeUtil;
use crate::core::util::{
    BYTE_BLOCK_SIZE, CoreHelper, HasIdentity, LATEST, SerialCounter, StringHelper,
};
use crossbeam::queue::SegQueue;
use num_bigint::BigInt;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::{Display, Formatter};
use std::sync::atomic::Ordering::SeqCst;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Maximum number of documents. In Java Lucene, We subtract 128 to ensure
/// it's well below the typical JVM's `ArrayUtil.MAX_ARRAY_LENGTH` and
/// avoid potential overflow issues across JVM implementations.
/// In Rust Lucene, we keep the same value for consistency.
pub const MAX_DOCS: i32 = i32::MAX - 128;
/// Maximum value for the token position in an indexed field.
pub const MAX_POSITION: i32 = i32::MAX - 128;
/// A variable that holds the actual maximum number of documents, which can
/// be adjusted for testing purposes.
pub const ACTUAL_MAX_DOCS: i32 = MAX_DOCS;

pub const MAX_TERM_LENGTH: i32 = BYTE_BLOCK_SIZE - 1;
const UNBOUNDED_MAX_MERGE_SEGMENTS: i32 = -1;
pub const WRITE_LOCK_NAME: &str = "write.lock";
/// Key for the source of a segment in the [`SegmentInfo#get_diagnostics()`](SegmentInfo::get_diagnostics)
pub const SOURCE: &str = "source";
/// Source of a segment which results from a merge of other segments.
pub const SOURCE_MERGE: &str = "merge";
/// Source of a segment which results from a flush.
pub const SOURCE_FLUSH: &str = "flush";
pub const MAX_STORED_STRING_LENGTH: i32 =
    ArrayUtil::MAX_ARRAY_LENGTH as i32 / UnicodeUtil::MAX_UTF8_BYTES_PER_CHAR;
pub(crate) fn get_actual_max_docs() -> i32 {
    ACTUAL_MAX_DOCS
}
/// Convenience overload: no extra details.
pub(crate) fn set_diagnostics<D>(info: &mut SegmentInfo<D>, source: &str)
where
    D: Directory,
{
    set_diagnostics_impl(info, source, None)
}
fn set_diagnostics_impl<D>(
    info: &mut SegmentInfo<D>,
    source: &str,
    details: Option<HashMap<String, String>>,
) where
    D: Directory,
{
    let mut diagnostics = HashMap::new();
    diagnostics.insert("source".to_string(), source.to_string());
    diagnostics.insert("lucene.version".to_string(), LATEST.to_string());
    diagnostics.insert("os".to_string(), Constants::os_name());
    diagnostics.insert("os.arch".to_string(), Constants::os_arch());
    diagnostics.insert("os.version".to_string(), Constants::os_version());
    diagnostics.insert(
        "timestamp".to_string(),
        chrono::Utc::now().timestamp_millis().to_string(),
    );
    if let Some(details) = details {
        for (k, v) in details {
            diagnostics.insert(k, v);
        }
    }
    info.set_diagnostics(diagnostics);
}
/// Returns `true` if `index_sort` is a prefix of `other_sort`.
pub(crate) fn is_congruent_sort(index_sort: &Sort, other_sort: &Sort) -> bool {
    let fields1 = index_sort.get_sort();
    let fields2 = other_sort.get_sort();

    if fields1.len() > fields2.len() {
        return false;
    }

    for (idx, v1) in fields1.iter().enumerate() {
        if fields2[idx] != *v1 {
            return false;
        }
    }
    true
}

// reads latest field infos for the commit
// this is used on IW init and addIndexes(Dir) to create/update the global field map.
// TODO: fix tests abusing this method!
pub(crate) fn read_field_infos<D>(si: &SegmentCommitInfo<D>) -> Result<FieldInfos>
where
    D: Directory,
{
    let codec = get_default_code();
    let reader = codec.field_infos_format();

    if si.has_field_updates() {
        // there are updates, we read latest (always outside CFS)
        let segment_suffix = BigInt::from(si.get_field_infos_gen()).to_str_radix(36);
        reader.read(
            si.info.dir.as_ref(),
            &si.info,
            &segment_suffix,
            &IOContext::read_once_io_context()?,
        )
    } else if si.info.get_use_compound_file() {
        // cfs
        let cfs = codec
            .compound_format()
            .get_compound_reader(si.info.dir.as_ref(), &si.info)?;
        let fis = reader.read(&cfs, &si.info, "", &IOContext::read_once_io_context()?)?;
        Ok(fis)
    } else {
        // no cfs
        reader.read(
            si.info.dir.as_ref(),
            &si.info,
            "",
            &IOContext::read_once_io_context()?,
        )
    }
}
/// NOTE: this method creates a compound file for all files returned by `info.files()`. While,
/// generally, this may include separate norms and deletion files, this `SegmentInfo` must not
/// reference such files when this method is called, because they are not allowed within a compound
/// file.
pub(crate) fn create_compound_file<D, T, D2>(
    info_stream: &InfoStreamMT,
    directory: &TrackingDirectoryWrapper<D>,
    info: &mut SegmentInfo<D2>,
    context: &IOContext,
    mut delete_files: T,
) -> Result<()>
where
    D: Directory,
    D2: Directory,
    T: IOConsumer<HashSet<String>>,
{
    // maybe this check is not needed, but why take the risk?
    if !directory
        .get_created_files()
        .lock()
        .created_filenames
        .is_empty()
    {
        return Err(LuceneError::illegal_state(
            "pass a clean trackingdir for CFS creation",
        ));
    }

    {
        if info_stream.enabled("IW") {
            info_stream.message("IW", "create compound file");
        }
    }
    // Now merge all added files
    let write_result = (|| {
        LATEST_CODEC
            .compound_format()
            .write(directory, info, context)?;
        Ok(())
    })();
    let filename = std::mem::take(&mut directory.get_created_files().lock().created_filenames);
    if write_result.is_err() {
        delete_files.accept_owner(filename)?;
        return write_result;
    }
    // Replace all previous files with the CFS/CFE files:
    info.set_files(filename)?;

    Ok(())
}
struct Permits {
    avail: AtomicUsize,
}
impl Permits {
    const MAX: usize = i32::MAX as usize;

    fn new() -> Self {
        Self {
            avail: AtomicUsize::new(Self::MAX),
        }
    }
    fn try_acquire(&self) -> bool {
        let mut cur = self.avail.load(Ordering::Acquire);
        while cur > 0 {
            match self
                .avail
                .compare_exchange(cur, cur - 1, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return true,
                Err(actual) => cur = actual,
            }
        }
        false
    }
    fn release(&self) {
        self.avail.fetch_add(1, Ordering::Release);
    }
    fn acquire_all(&self) {
        loop {
            let cur = self.avail.load(Ordering::Acquire);
            if cur == Self::MAX {
                let res =
                    self.avail
                        .compare_exchange(Self::MAX, 0, Ordering::AcqRel, Ordering::Acquire);
                if res.is_ok() {
                    break;
                }
            }
            std::thread::yield_now();
        }
    }
    fn release_all(&self) {
        self.avail.store(Self::MAX, Ordering::Release);
    }
    fn available(&self) -> usize {
        self.avail.load(Ordering::Relaxed)
    }
}
pub(crate) struct EventQueue {
    closed: AtomicBool,
    permits: Permits,
    queue: SegQueue<EventEnum>,
    guard: Mutex<()>,
}

impl EventQueue {
    fn new() -> Self {
        Self {
            closed: AtomicBool::new(false),
            permits: Permits::new(),
            queue: SegQueue::new(),
            guard: Mutex::new(()),
        }
    }
    fn acquire(&self) -> Result<()> {
        if !self.permits.try_acquire() {
            return Err(LuceneError::already_closed("queue is closed"));
        }
        if self.closed.load(Ordering::Acquire) {
            self.permits.release();
            return Err(LuceneError::already_closed("queue is closed"));
        }
        Ok(())
    }
    fn add(&self, event: EventEnum) -> Result<()> {
        self.acquire()?;
        self.queue.push(event);
        self.permits.release();
        Ok(())
    }
    fn process_events<D, L, B>(&self, writer: &IndexWriter<D, L, B>) -> Result<()>
    where
        D: Directory,
        L: LiveIndexWriterConfig,
        B: IndexWriterBase,
    {
        self.acquire()?;
        let result = self.process_events_internal(writer);
        self.permits.release();
        result
    }
    fn process_events_internal<D, L, B>(&self, writer: &IndexWriter<D, L, B>) -> Result<()>
    where
        D: Directory,
        L: LiveIndexWriterConfig,
        B: IndexWriterBase,
    {
        debug_assert!(
            (Permits::MAX - self.permits.available()) > 0,
            "must acquire a permit before processing events"
        );

        while let Some(mut event) = self.queue.pop() {
            event.process(writer)?
        }
        Ok(())
    }
    fn close<D, L, B>(&self, writer: &IndexWriter<D, L, B>) -> Result<()>
    where
        D: Directory,
        L: LiveIndexWriterConfig,
        B: IndexWriterBase,
    {
        let _guard = self.guard.lock();
        debug_assert!(
            !self.closed.load(Ordering::Acquire),
            "we should never close this twice"
        );

        self.closed.store(true, Ordering::Release);

        if writer.get_tragic_exception().lock().is_some() {
            while self.queue.pop().is_some() {
                // we are already handling a tragic exception let's drop it all on the floor and return
            }
            return Ok(());
        }
        // now we acquire all the permits to ensure we are the only one processing the queue
        self.permits.acquire_all();

        let result = self.process_events_internal(writer);
        self.permits.release_all();
        drop(_guard);
        result
    }
}

/// Interface for internal atomic events. See [`DocumentsWriter`] for details.
/// Events are executed concurrently and no order is guaranteed. Each event should only rely on
/// the serializability within its `process` method. All actions that must happen before or after
/// a certain action must be encoded inside the [`process(IndexWriter)`](Self::process) method.
trait Event<D>
where
    D: Directory,
{
    /// Processes the event. This method is called by the [`IndexWriter`] passed as the first argument.
    ///
    /// # Arguments
    ///
    /// * `writer` — the [`IndexWriter`] that executes the event.
    fn process<L, B>(&mut self, writer: &IndexWriter<D, L, B>) -> Result<()>
    where
        L: LiveIndexWriterConfig,
        B: IndexWriterBase;
}
struct EventImpl1 {
    files: HashSet<String>,
}
impl EventImpl1 {
    pub fn new(files: HashSet<String>) -> Self {
        Self { files }
    }
}
impl<D> Event<D> for EventImpl1
where
    D: Directory,
{
    fn process<L, B>(&mut self, writer: &IndexWriter<D, L, B>) -> Result<()>
    where
        L: LiveIndexWriterConfig,
        B: IndexWriterBase,
    {
        writer.delete_new_files(self.files.iter())
    }
}

struct EventImpl2 {
    info_files: HashSet<String>,
}
impl EventImpl2 {
    pub fn new(info_files: HashSet<String>) -> Self {
        Self { info_files }
    }
}
impl<D> Event<D> for EventImpl2
where
    D: Directory,
{
    fn process<L, B>(&mut self, writer: &IndexWriter<D, L, B>) -> Result<()>
    where
        L: LiveIndexWriterConfig,
        B: IndexWriterBase,
    {
        writer.flush_failed(std::mem::take(&mut self.info_files))
    }
}

struct EventImpl3;
impl<D> Event<D> for EventImpl3
where
    D: Directory,
{
    fn process<L, B>(&mut self, writer: &IndexWriter<D, L, B>) -> Result<()>
    where
        L: LiveIndexWriterConfig,
        B: IndexWriterBase,
    {
        let result = writer.publish_flushed_segments(true);
        writer.flush_count.fetch_add(1, Ordering::SeqCst);
        result
    }
}
struct EventImpl4;
impl<D> Event<D> for EventImpl4
where
    D: Directory,
{
    fn process<L, B>(&mut self, writer: &IndexWriter<D, L, B>) -> Result<()>
    where
        L: LiveIndexWriterConfig,
        B: IndexWriterBase,
    {
        writer.publish_flushed_segments(true)
    }
}
struct EventImpl5 {
    packet: Arc<FrozenBufferedUpdates>,
}
impl EventImpl5 {
    pub fn new(packet: Arc<FrozenBufferedUpdates>) -> Self {
        Self { packet }
    }
}
impl<D> Event<D> for EventImpl5
where
    D: Directory,
{
    fn process<L, B>(&mut self, writer: &IndexWriter<D, L, B>) -> Result<()>
    where
        L: LiveIndexWriterConfig,
        B: IndexWriterBase,
        B: IndexWriterBase,
    {
        // we call tryApply here since we don't want to block if a refresh or a flush is already
        // applying the
        // packet. The flush will retry this packet anyway to ensure all of them are applied
        match writer.try_apply(&self.packet) {
            Ok(_) => {
                writer.flush_deletes_count.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            Err(e) => {
                match writer.on_tragic_event(&e, "applyUpdatesPacket") {
                    Ok(_) => Err(e),
                    Err(err) => {
                        // TODO 这里没有将e跟err 合并成一个合理的Error
                        Err(LuceneError::illegal_state(format!(
                            "{err} + supper error:{{e}}"
                        )))
                    },
                }
            },
        }
    }
}

enum EventEnum {
    A(EventImpl1),
    B(EventImpl2),
    C(EventImpl3),
    D(EventImpl4),
    E(EventImpl5),
}
impl<D> Event<D> for EventEnum
where
    D: Directory,
{
    fn process<L, B>(&mut self, writer: &IndexWriter<D, L, B>) -> Result<()>
    where
        L: LiveIndexWriterConfig,
        B: IndexWriterBase,
    {
        match self {
            EventEnum::A(e) => e.process(writer),
            EventEnum::B(e) => e.process(writer),
            EventEnum::C(e) => e.process(writer),
            EventEnum::D(e) => e.process(writer),
            EventEnum::E(e) => e.process(writer),
        }
    }
}
#[derive(Default)]
struct IndexWriterMergeSource;
impl MergeSource for IndexWriterMergeSource {
    type OneMerge<D>
        = OneMergeSR<D>
    where
        D: Directory;

    fn get_next_merge<D, L, B>(
        &self,
        writer: &IndexWriter<D, L, B>,
    ) -> Result<Option<Self::OneMerge<D>>>
    where
        D: Directory,
        L: LiveIndexWriterConfig,
        B: IndexWriterBase,
    {
        match writer.get_next_merge()? {
            Some(next_merge) => {
                // todo
                Ok(Some(next_merge))
            },
            None => Ok(None),
        }
    }

    fn on_merge_finished<D, L, B>(
        &self,
        merge: &mut Self::OneMerge<D>,
        writer: &IndexWriter<D, L, B>,
    ) where
        D: Directory,
        L: LiveIndexWriterConfig,
        B: IndexWriterBase,
    {
        writer.merge_finish(merge, None)
    }

    fn has_pending_merges<D, L, B>(&self, writer: &IndexWriter<D, L, B>) -> Result<bool>
    where
        D: Directory,
        L: LiveIndexWriterConfig,
        B: IndexWriterBase,
    {
        writer.has_pending_merges()
    }

    fn merge<D, L, B>(&self, merge: Self::OneMerge<D>, writer: &IndexWriter<D, L, B>) -> Result<()>
    where
        D: Directory,
        L: LiveIndexWriterConfig,
        B: IndexWriterBase,
    {
        writer.merge(merge)
    }
}
