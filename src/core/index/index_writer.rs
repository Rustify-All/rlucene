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
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::merge_policy::{
  DefaultMergeSpecification, MergeContext, MergePolicy, MergeReaderSR, MergeSpecification,
  MergeStat, OneMergeSR,
};
use crate::core::index::merge_scheduler::{MergeScheduler, MergeSource};
use crate::core::index::merge_state::{DocMap, DocMapEnum2};
use crate::core::index::segment_info::SegmentInfo;
#[cfg(debug_assertions)]
use crate::core::index::segment_info::named_for_this_segment;
use crate::core::index::segment_infos::{SegmentInfos, get_last_commit_segments_file_name};
use crate::core::store::directory::Directory;
use crate::core::store::flush_info::FlushInfo;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::counter::{Counter, new_counter};
use crate::core::util::error::lucene_error::{CaughtResult, CaughtResultExt, LuceneError, Result};
use crate::core::util::long_supplier::LongSupplier;
#[cfg(test)]
use crate::test_framework::core::index::test_tragic_index_writer_deadlock::TragicIndexWriter;
use parking_lot::{Condvar, Mutex, MutexGuard, ReentrantMutex};
use std::cell::{Cell, RefCell};
use std::sync::{Arc, OnceLock, Weak};

pub trait IntoFallibleIterator {
  type Item;
  type IntoIter: Iterator<Item = Result<Self::Item>>;

  fn into_fallible_iter(self) -> Self::IntoIter;
}

pub struct InfallibleIterator<I>(I);

impl<I> Iterator for InfallibleIterator<I>
where
  I: Iterator,
{
  type Item = Result<I::Item>;

  fn next(&mut self) -> Option<Self::Item> {
    self.0.next().map(Ok)
  }
}

impl<T> IntoFallibleIterator for T
where
  T: IntoIterator,
{
  type Item = T::Item;
  type IntoIter = InfallibleIterator<T::IntoIter>;

  fn into_fallible_iter(self) -> Self::IntoIter {
    InfallibleIterator(self.into_iter())
  }
}

/// An `IndexWriter` creates and maintains an index.
///
/// The [`OpenMode`] option on [`IndexWriterConfig::set_open_mode`] determines whether a new index
/// is created, or whether an existing index is opened. Note that you can open an index with
/// [`OpenMode::Create`] even while readers are using the index. The old readers will continue to
/// search the "point in time" snapshot they had opened, and won't see the newly created index until
/// they re-open. If [`OpenMode::CreateOrAppend`] is used `IndexWriter` will create a new index if
/// there is not already an index at the provided path and otherwise open the existing index.
///
/// In either case, documents are added with [`Self::add_document`] and removed with
/// [`Self::delete_documents_with_terms`] or [`Self::delete_documents_with_queries`]. A document can
/// be updated with [`Self::update_document_with_term`] (which just deletes and then adds the entire
/// document). When finished adding, deleting and updating documents, [`Self::close`] should be
/// called. <a id="sequence_numbers"></a>
///
/// Each method that changes the index returns a `long` sequence number, which expresses the
/// effective order in which each change was applied. [`Self::commit`] also returns a sequence
/// number, describing which changes are in the commit point and which are not. Sequence numbers are
/// transient (not saved into the index in any way) and only valid within a single `IndexWriter`
/// instance. <a id="flush"></a>
///
/// These changes are buffered in memory and periodically flushed to the [`Directory`] (during the
/// above method calls). A flush is triggered when there are enough added documents since the last
/// flush. Flushing is triggered either by RAM usage of the documents (see
/// [`IndexWriterConfig::set_ram_buffer_size_mb`]) or the number of added documents (see
/// [`IndexWriterConfig::set_max_buffered_docs`]). The default is to flush when RAM usage hits
/// [`IndexWriterConfig::DEFAULT_RAM_BUFFER_SIZE_MB`] MB. For best indexing speed you should flush
/// by RAM usage with a large RAM buffer. In contrast to the other flush options
/// [`IndexWriterConfig::set_ram_buffer_size_mb`] and
/// [`IndexWriterConfig::set_max_buffered_docs`], deleted terms won't trigger a segment flush. Note
/// that flushing just moves the internal buffered state in `IndexWriter` into the index, but these
/// changes are not visible to `IndexReader` until either [`Self::commit`] or [`Self::close`] is
/// called. A flush may also trigger one or more segment merges which by default run with a
/// background thread so as not to block the `add_document` calls (see
/// <a href="#mergePolicy">below</a> for changing the [`MergeScheduler`]).
///
/// Opening an `IndexWriter` creates a lock file for the directory in use. Trying to open another
/// `IndexWriter` on the same directory returns a [`LuceneError::LockObtainFailed`] error.
///
/// Expert: `IndexWriter` allows an optional [`IndexDeletionPolicy`] implementation to be specified.
/// You can use this to control when prior commits are deleted from the index. The default policy is
/// [`KeepOnlyLastCommitDeletionPolicy`] which removes all prior commits as soon as a new commit is
/// done. Creating your own policy can allow you to explicitly keep previous "point in time"
/// commits alive in the index for some time, either because this is useful for your application, or
/// to give readers enough time to refresh to the new commit without having the old commit deleted
/// out from under them. The latter is necessary when multiple computers take turns opening their
/// own `IndexWriter` and `IndexReader`s against a single shared index mounted via remote
/// filesystems like NFS which do not support "delete on last close" semantics. A single computer
/// accessing an index via NFS is fine with the default deletion policy since NFS clients emulate
/// "delete on last close" locally. That said, accessing an index via NFS will likely result in poor
/// performance compared to a local IO device. <a id="mergePolicy"></a>
///
/// Expert: `IndexWriter` allows you to separately change the [`MergePolicy`] and the
/// [`MergeScheduler`]. The [`MergePolicy`] is invoked whenever there are changes to the segments in
/// the index. Its role is to select which merges to do, if any, and return a
/// [`MergePolicy::MergeSpecification`] describing the merges. The default is
/// [`LogByteSizeMergePolicy`]. Then, the [`MergeScheduler`] is invoked with the requested merges
/// and it decides when and how to run the merges. The default is
/// [`ConcurrentMergeScheduler`]. <a id="OOME"></a>
///
/// **NOTE**: if you hit an Error, or disaster strikes during a checkpoint then `IndexWriter`
/// will close itself. This is a defensive measure in case any internal state (buffered documents,
/// deletions, reference counts) were corrupted. Any subsequent calls will return an
/// [`AlreadyClosedError`]. <a id="thread-safety"></a>
///
/// **NOTE**: [`IndexWriter`] instances are completely thread safe, meaning multiple threads can
/// call any of its methods, concurrently. If your application requires external synchronization,
/// you should **not** synchronize on the `IndexWriter` instance as this may cause deadlock; use
/// your own (non-Lucene) objects instead.
///
/// **NOTE**: Rust does not expose Java-style thread interruption. Callers should use explicit
/// cancellation or timeout mechanisms when coordinating work performed by `IndexWriter`.
///
/// Clarification: Check Points (and commits)
///
/// `IndexWriter` writes new index files to the directory without writing a new `segments_N`
/// file which references these new files. It also means that the state of the in memory
/// `SegmentInfos` object is different than the most recent `segments_N` file written to the
/// directory.
///
/// Each time the `SegmentInfos` is changed, and matches the (possibly modified) directory files,
/// we have a new "check point".
/// If the modified/new `SegmentInfos` is written to disk - as a new (generation of)
/// `segments_N` file - this check point is also an `IndexCommit`.
///
/// A new checkpoint always replaces the previous checkpoint and becomes the new "front" of the
/// index. This allows the `IndexFileDeleter` to delete files that are referenced only by stale
/// checkpoints (files that were created since the last commit, but are no longer referenced by the
/// "front" of the index). For this, `IndexFileDeleter` keeps track of the last non commit
/// checkpoint.
pub struct IndexWriter<D>
where
  D: Directory,
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
  pub(crate) global_field_number_map: FieldNumbersLock,
  pub(crate) doc_writer: DocumentsWriter<D, FlushNotificationsImpl>,
  event_queue: Arc<EventQueue>,
  write_doc_values_lock: Mutex<()>,

  pub(crate) closed: AtomicBool,
  closing: AtomicBool,

  self_ref: OnceLock<Weak<IndexWriter<D>>>,
  maybe_merge: AtomicBool,

  flush_count: AtomicI32,
  flush_deletes_count: AtomicI32,
  reader_pool: ReaderPool<D, LongSupplierImpl>,
  buffered_updates_stream: Arc<BufferedUpdatesStream>,
  buffered_updates_stream_lock: Mutex<()>,
  merge_finished_gen: AtomicI64,
  pub(crate) config: IndexWriterConfig<D>,
  pub(crate) pending_num_docs: Arc<AtomicI64>,
  soft_deletes_enabled: bool,
  info_stream: InfoStreamMT,
  pub(crate) inner: Mutex<Inner<D>>,
  pausing: Condvar,
  pub(crate) hooks: Option<IndexWriterHooksEnum>,
  commit_lock: ReentrantMutex<CommitInner<D>>,
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
  pub(crate) commit_user_data: Option<HashMap<String, String>>,
  pending_merges: VecDeque<OneMergeSR<D>>,
  running_merges: HashSet<MergeStat>,
  merge_exceptions: Vec<MergeStat>,
  merge_gen: i64,
  // used by forceMerge to note those needing merging
  segments_to_merge: HashMap<String, Option<bool>>,
  merges: Merges,
  merging_segments: HashSet<String>,
  merge_max_num_segments: i32,
  running_add_indexes_merges: HashSet<String>,
}

pub struct CommitInner<D>
where
  D: Directory,
{
  pending_commit: RefCell<Option<SegmentInfos<D>>>,
  files_to_commit: RefCell<Option<Vec<String>>>,
  start_commit_time: Cell<Instant>,
}

pub type DefaultIndexWriter<D> = Arc<IndexWriter<D>>;
impl<D> IndexWriter<D>
where
  D: Directory,
{
  pub fn new(d: Arc<D>, conf: IndexWriterConfig<D>) -> Result<Arc<Self>>
  where
    D: 'static,
  {
    Self::with_hooks(d, conf, Some(EmptyIndexWriterHooks.into()))
  }
}

impl<D> IndexWriter<D>
where
  D: Directory,
{
  fn arc_self(&self) -> Option<Arc<Self>> {
    self.self_ref.get().and_then(Weak::upgrade)
  }

  fn new_merge_source(&self) -> Result<IndexWriterMergeSource<D>> {
    let writer = self.arc_self().ok_or_else(|| {
      LuceneError::illegal_state(
        "background merges require IndexWriter to be constructed as Arc<IndexWriter>",
      )
    })?;
    Ok(IndexWriterMergeSource::new(writer))
  }

  fn new_add_indexes_merge_source<CR>(&self) -> Result<Arc<AddIndexesMergeSource<D, CR>>>
  where
    CR: CodecReader,
  {
    let writer = self.arc_self().ok_or_else(|| {
      LuceneError::illegal_state(
        "background addIndexes merges require IndexWriter to be constructed as Arc<IndexWriter>",
      )
    })?;
    Ok(Arc::new(AddIndexesMergeSource::new(writer)))
  }

  fn into_arc(self) -> Arc<Self> {
    let writer = Arc::new(self);
    let _ = writer.self_ref.set(Arc::downgrade(&writer));
    writer
  }

  pub fn with_hooks(
    d: Arc<D>,
    conf: IndexWriterConfig<D>,
    sub: Option<IndexWriterHooksEnum>,
  ) -> Result<Arc<Self>>
  where
    D: 'static,
  {
    Self::with_index_commit_and_hook(d, conf, sub, IndexCommitWrapper::default())
  }

  pub fn with_index_commit<IC>(
    d: Arc<D>,
    conf: IndexWriterConfig<D>,
    index_commit: IndexCommitWrapper<'_, IC, D>,
  ) -> Result<Arc<Self>>
  where
    IC: IndexCommit<Directory = Arc<D>>,
    D: 'static,
  {
    Self::with_index_commit_and_hook(d, conf, Some(EmptyIndexWriterHooks.into()), index_commit)
  }

  pub fn with_index_commit_and_hook<IC>(
    d: Arc<D>,
    conf: IndexWriterConfig<D>,
    hooks: Option<IndexWriterHooksEnum>,
    index_commit_wrapper: IndexCommitWrapper<'_, IC, D>,
  ) -> Result<Arc<Self>>
  where
    IC: IndexCommit<Directory = Arc<D>>,
    D: 'static,
  {
    let enable_test_points = hooks.as_ref().unwrap().is_enable_test_points();
    let info_stream = conf.get_info_stream();
    let soft_deletes_enabled = conf.get_soft_deletes_field().is_some();

    // obtain the write.lock. If the user configured a timeout,
    // we wrap with a sleeper and this might take some time.
    let write_lock = d.obtain_lock(WRITE_LOCK_NAME)?;
    let mut directory_for_cleanup = None;
    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      let directory_orig = d.clone();
      let directory = Arc::new(LockValidatingDirectoryWrapper::new(d.clone(), write_lock));
      directory_for_cleanup = Some(directory.clone());

      let mode = conf.get_open_mode();
      let (index_exists, create) = match mode {
        OpenMode::Create => {
          let exists = directory_reader::index_exists(directory.as_ref())?;
          (exists, true)
        },
        OpenMode::Append => (true, false),
        OpenMode::CreateOrAppend => {
          let exists = directory_reader::index_exists(directory.as_ref())?;
          (exists, !exists)
        },
      };

      // If index is too old, reading the segments will return
      // `LuceneError::IndexFormatTooOld`.

      let files = directory.list_all()?;

      let mut change_count = 0;
      let mut segment_infos;
      let _did_message_state = AtomicBool::new(false);
      let rollback_segments;
      let reader = if create {
        if index_commit_wrapper.commit.is_some() {
          return Err(LuceneError::illegal_argument(
            if *conf.get_open_mode() == OpenMode::Create {
              "cannot use IndexCommit with OpenMode.CREATE"
            } else {
              "cannot use IndexCommit when index has no commit"
            },
          ));
        }
        // Try to read first. This is to allow creation
        // against an index that's currently open for
        // searching. In this case we write the next
        // segments_N file with no segments:
        let mut sis: SegmentInfos<D> = SegmentInfos::new(conf.get_index_created_version_major())?;

        if index_exists {
          let previous = SegmentInfos::read_latest_commit(directory.clone())?;
          sis.update_generation_version_and_counter(&previous);
        }

        segment_infos = sis;
        rollback_segments = segment_infos.create_backup_segment_infos()?;

        // Record that we have a change (zero out all segments) pending:
        changed(&mut change_count, &mut segment_infos);
        None
      } else if let Some(reader) = index_commit_wrapper.reader {
        if reader.segment_infos.get_index_created_version_major() < *MIN_SUPPORTED_MAJOR {
          // second line of defence in the case somebody tries to trick us.
          return Err(LuceneError::illegal_argument(format!(
            "createdVersionMajor must be >= {}, got: {}",
            *MIN_SUPPORTED_MAJOR,
            reader.segment_infos.get_index_created_version_major()
          )));
        }
        // Init from an existing already opened NRT or non-NRT reader:

        let commit = index_commit_wrapper.commit.as_ref().ok_or_else(|| {
          LuceneError::illegal_argument("IndexCommit must be provided when opening from reader")
        })?;
        if !reader
          .directory()
          .directory
          .is_same_identity(&*commit.get_directory())
        {
          return Err(LuceneError::illegal_argument(
            "IndexCommit's reader must have the same directory as the IndexCommit",
          ));
        }

        if !reader
          .directory()
          .directory
          .is_same_identity(&directory_orig)
        {
          return Err(LuceneError::illegal_argument(
            "IndexCommit's reader must have the same directory passed to IndexWriter",
          ));
        }

        if reader.segment_infos.get_last_generation() == 0 {
          return Err(LuceneError::illegal_argument(
            "index must already have an initial commit to open from reader",
          ));
        }

        // Must clone because we don't want the incoming NRT reader to "see" any changes this writer
        // now makes:
        segment_infos = reader.segment_infos.try_clone()?;

        let segments_file_name = segment_infos.get_segments_file_name().ok_or_else(|| {
          LuceneError::illegal_argument(
            "the provided reader is stale: it has no segments file associated with it",
          )
        })?;
        let mut last_commit =
          SegmentInfos::read_commit(directory_orig.clone(), &segments_file_name).map_err(|e| {
            if e.is_io_error() {
              let mut error = LuceneError::illegal_argument(format!(
                "the provided reader is stale: its prior commit file \"{}\" is missing from index",
                segments_file_name
              ));
              error.add_suppressed(e);
              error
            } else {
              e
            }
          })?;
        if let Some(writer) = &reader.writer {
          // The old writer better be closed (we have the write lock now!):
          debug_assert!(writer.closed.load(Ordering::SeqCst));

          // In case the old writer wrote further segments (which we are now dropping),
          // update SIS metadata so we remain write-once:
          let inner = writer.inner.lock();
          segment_infos.update_generation_version_and_counter(&inner.segment_infos);
          last_commit.update_generation_version_and_counter(&inner.segment_infos);
        }

        rollback_segments = last_commit.create_backup_segment_infos()?;
        Some(reader)
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
        segment_infos = SegmentInfos::read_commit(directory_orig.clone(), &last_segments_file)?;
        if let Some(commit) = index_commit_wrapper.commit {
          if !commit.get_directory().is_same_identity(&*directory_orig) {
            return Err(LuceneError::illegal_argument(format!(
              "IndexCommit's directory doesn't match my directory, expected={}, got={}",
              directory_orig,
              *commit.get_directory()
            )));
          }

          let old_infos =
            SegmentInfos::read_commit(directory_orig.clone(), commit.get_segments_file_name())?;
          segment_infos.replace(old_infos)?;
          changed(&mut change_count, &mut segment_infos);

          if info_stream.is_enabled("IW") {
            info_stream.message(
              "IW",
              &format!(
                "init: loaded commit \"{}\"",
                commit.get_segments_file_name()
              ),
            )?;
          }
        }
        rollback_segments = segment_infos.create_backup_segment_infos()?;
        None
      };

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
        pending_num_docs.clone(),
        &conf,
      )?;

      let has_reader = reader.is_some();
      let reader_pool = ReaderPool::new(
        directory.clone(),
        directory_orig.clone(),
        &segment_infos,
        info_stream.clone(),
        conf.get_soft_deletes_field().cloned(),
        LongSupplierImpl::new(buffered_updates_stream.clone()),
        reader,
        conf.get_index_created_version_major(),
      )?;

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
        has_reader,
      )?;
      // We incRef all files when we return an NRT reader from IW,
      // so all files must exist even in the NRT case:
      debug_assert!(create || Self::files_exist(&segment_infos, &deleter)?);

      if deleter.starting_commit_deleted {
        // Deletion policy deleted the "head" commit point.
        // We have to mark ourselves as changed so that if we
        // are closed w/o any further changes we write a new
        // segments_N file.
        changed(&mut change_count, &mut segment_infos);
      }

      if has_reader {
        // We always assume we are carrying over incoming changes when opening from reader:
        segment_infos.changed();
        changed(&mut change_count, &mut segment_infos);
      }

      let mut iw = Self {
        enable_test_points,
        tragedy: Arc::new(OnceLock::new()),
        directory_orig,
        directory,
        last_commit_change_count: AtomicI64::new(0),
        pending_seq_no: AtomicI64::new(0),
        pending_commit_change_count: AtomicI64::new(0),
        global_field_number_map,
        doc_writer,
        event_queue,
        write_doc_values_lock: Mutex::new(()),
        closed: AtomicBool::new(false),
        closing: AtomicBool::new(false),
        self_ref: OnceLock::new(),
        maybe_merge: AtomicBool::new(false),
        flush_count: AtomicI32::new(0),
        flush_deletes_count: AtomicI32::new(0),
        reader_pool,
        buffered_updates_stream,
        buffered_updates_stream_lock: Mutex::new(()),
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
          running_add_indexes_merges: HashSet::new(),
        }),
        pausing: Condvar::new(),
        hooks,
        commit_lock: ReentrantMutex::new(CommitInner {
          pending_commit: RefCell::new(None),
          files_to_commit: RefCell::new(None),
          start_commit_time: Cell::new(Instant::now()),
        }),
        full_flush_lock: Mutex::new(()),
      };
      iw.config
        .base
        .merge_scheduler
        .initialize(info_stream.clone(), iw.directory.as_ref())?;
      success = true;
      Ok(iw)
    }));
    if !success && info_stream.is_enabled("IW") {
      info_stream.message("IW", "init: hit exception on init; releasing write lock")?;
    }
    if !success && let Some(directory) = directory_for_cleanup.as_ref() {
      IOUtils::close_while_handling_exception(&directory.write_lock);
    }
    unwrap_caught_result!(result).map(Self::into_arc)
  }

  pub(crate) fn get_index_major_version_created(&self) -> i32 {
    self
      .inner
      .lock()
      .segment_infos
      .get_index_created_version_major()
  }

  /// Confirms that the incoming index sort (if any) matches the existing index sort (if any).
  fn validate_index_sort(
    config: &IndexWriterConfig<D>,
    segment_infos: &SegmentInfos<D>,
  ) -> Result<()> {
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
  fn get_field_number_map(
    config: &IndexWriterConfig<D>,
    segment_infos: &SegmentInfos<D>,
  ) -> Result<FieldNumbers> {
    let mut map = FieldNumbers::new(config.get_soft_deletes_field(), config.get_parent_field())?;
    for info in segment_infos.iter() {
      let fis = read_field_infos(info)?;
      for fi in fis.iter() {
        map.add_or_get(fi)?;
      }
    }

    Ok(map)
  }
  /// Returns the [`IndexWriterConfig`] that was passed to [`IndexWriter::new`]. This returns
  /// a live reference; changes to the config affect this writer instance.
  pub fn get_config(&self) -> &IndexWriterConfig<D> {
    &self.config
  }
  /// Mutable version of [`Self::get_config`].
  #[cfg(test)]
  #[allow(invalid_reference_casting)]
  #[allow(clippy::mut_from_ref)]
  pub fn get_config_mut(&self) -> &mut IndexWriterConfig<D> {
    unsafe { &mut *(&self.config as *const IndexWriterConfig<D> as *mut IndexWriterConfig<D>) }
  }
  /// Gracefully closes (commits, waits for merges), but calls rollback if there's an error so the
  /// [`IndexWriter`] is always closed. This is called from [`close`] when
  /// [`IndexWriterConfig::commit_on_close`] is `true`.
  fn shut_down(&self) -> Result<()>
  where
    D: 'static,
  {
    if self.commit_lock.lock().pending_commit.borrow().is_some() {
      return Err(LuceneError::illegal_state(
        "cannot close: prepareCommit was already called with no corresponding call to commit",
      ));
    }
    if self.should_close(true) {
      let mut result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        if self.info_stream.is_enabled("IW") {
          self.info_stream.message("IW", "now flush at close")?;
        }
        self.flush_with_apply_merge_deletes(true, true)?;
        self.wait_for_merges()?;
        self.commit_internal(self.config.get_merge_policy())?;
        Ok(())
      }));
      if !matches!(&result, Ok(Ok(()))) {
        let rollback_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
          self.rollback_internal(None)
        }));
        result.add_suppressed(
          rollback_result,
          "panic while rolling back after shutdown failure",
        );
        return unwrap_caught_result!(result);
      }
      // if we got that far lets rollback and close
      self.rollback_internal(None)?;
    }
    Ok(())
  }
  /// Closes all open resources and releases the write lock.
  ///
  /// If [`IndexWriterConfig::commit_on_close`](LiveIndexWriterConfig::get_commit_on_close) is `true`, this will attempt to gracefully shut down by:
  /// writing any changes, waiting for any running merges, committing, and closing.
  /// In this case, note that:
  ///
  /// - If you called `prepare_commit` but failed to call `commit`, this method returns
  ///   [`LuceneError::IllegalState`] and the `IndexWriter` is not closed.
  /// - If this method returns any other error, the `IndexWriter` is closed, but
  ///   changes may have been lost.
  ///
  /// Note that this may be a costly operation, so try to re-use a single writer instead of
  /// frequently closing and opening new ones. See [`commit()`](Self::commit) for caveats about wingite caching done
  /// by some IO devices.
  ///
  /// **NOTE**: You must ensure no other threads are still making changes at the same time
  /// that this method is invoked.
  pub fn close(&self) -> Result<()>
  where
    D: 'static,
  {
    if self.config.get_commit_on_close() {
      self.shut_down()?;
    } else {
      self.rollback()?;
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

  /// Returns the [`Directory`] used by this index.
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
  pub fn delete_documents_with_terms(&self, terms: Vec<Term>) -> Result<i64>
  where
    D: 'static,
  {
    self.do_ensure_open(true)?;
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<i64> {
      let seq = self.maybe_process_events(self.doc_writer.delete_terms(&self.config, terms)?)?;
      Ok(seq)
    }));

    if res.is_err() {
      self.tragic_event(&res, "deleteDocuments(Term..)", None)?;
    }
    unwrap_caught_result!(res)
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
  pub fn delete_documents_with_queries(&self, queries: Vec<Query>) -> Result<i64>
  where
    D: 'static,
  {
    self.do_ensure_open(true)?;

    // LUCENE-6379: Specialize MatchAllDocsQuery
    for query in &queries {
      if matches!(query, Query::MatchAllDocs(_)) {
        return self.delete_all();
      }
    }

    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<i64> {
      let seq0 = self.doc_writer.delete_queries(&self.config, queries)?;
      let seq = self.maybe_process_events(seq0)?;
      Ok(seq)
    }));

    if res.is_err() {
      self.tragic_event(&res, "deleteDocuments(Query..)", None)?;
    }
    unwrap_caught_result!(res)
  }

  /// Adds a document to this index.
  ///
  /// Note that if an error is hit (for example, disk full) then the index will remain consistent,
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
  /// Each term in the document can be no longer than [`MAX_TERM_LENGTH`] bytes; otherwise this
  /// method returns [`LuceneError::IllegalArgument`].
  ///
  /// # Returns
  /// The `sequence number` for this operation.
  ///
  /// # Errors
  /// - Returns [`LuceneError::CorruptIndex`] if the index is corrupt.
  /// - Returns an I/O error if a low-level I/O operation fails.
  pub fn add_document<DF>(&self, doc: DF) -> Result<i64>
  where
    DF: IntoFallibleIterator<Item = Fields>,
    D: 'static,
  {
    self.update_document_with_term(None, doc)
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
  /// See [`add_document(Iterable)`](Self::add_document) for details on index and `IndexWriter` state after an error,
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
  /// - Returns [`LuceneError::CorruptIndex`] if the index is corrupt.
  /// - Returns an `io::Error` if there is a low-level I/O error.
  pub fn add_documents<DI, DF>(&self, docs: DI) -> Result<i64>
  where
    DI: IntoFallibleIterator<Item = DF>,
    DF: IntoFallibleIterator<Item = Fields>,
    D: 'static,
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
  /// - Returns [`LuceneError::CorruptIndex`] if the index is corrupt.
  /// - Returns an `io::Error` if there is a low-level I/O error.
  pub fn update_document_with_term<T, DF>(&self, del_term: T, docs: DF) -> Result<i64>
  where
    T: Into<Option<Term>>,
    DF: IntoFallibleIterator<Item = Fields>,
    D: 'static,
  {
    let del_node = del_term
      .into()
      .map(|t| Arc::new(DocumentsWriterDeleteQueue::new_node_with_term(t)));

    self.update_documents(del_node, vec![docs])
  }
  /// Atomically deletes documents matching the provided delTerm and adds a block of documents with
  /// sequentially assigned document IDs, such that an external reader will see all or none of the
  /// documents.
  ///
  /// See [`Self::add_documents`].
  ///
  /// Returns the sequence number for this operation.
  ///
  /// # Errors
  ///
  /// Returns [`LuceneError::CorruptIndex`] if the index is corrupt.
  ///
  /// Returns an error if there is a low-level IO error.
  ///
  /// # Experimental
  ///
  /// This API is experimental and might change in incompatible ways in the next release.
  pub fn update_documents_with_term<T, DI, DF>(&self, del_term: T, docs: DI) -> Result<i64>
  where
    T: Into<Option<Term>>,
    DI: IntoFallibleIterator<Item = DF>,
    DF: IntoFallibleIterator<Item = Fields>,
    D: 'static,
  {
    let del_node = del_term
      .into()
      .map(|t| Arc::new(DocumentsWriterDeleteQueue::new_node_with_term(t)));

    self.update_documents(del_node, docs)
  }
  /// Similar to [`update_documents(Term, Iterable)`](Self::update_document_with_term), but takes a query instead of a term to
  /// identify the documents to be updated.
  pub fn update_documents_with_query<T, DI, DF>(&self, del_query: T, docs: DI) -> Result<i64>
  where
    T: Into<Option<Query>>,
    DI: IntoFallibleIterator<Item = DF>,
    DF: IntoFallibleIterator<Item = Fields>,
    D: 'static,
  {
    let del_node = del_query
      .into()
      .map(|q| Arc::new(DocumentsWriterDeleteQueue::new_node_with_query(q)));

    self.update_documents(del_node, docs)
  }

  fn update_documents<DI, DF>(&self, del_node: Option<Arc<Node>>, docs: DI) -> Result<i64>
  where
    DI: IntoFallibleIterator<Item = DF>,
    DF: IntoFallibleIterator<Item = Fields>,
    D: 'static,
  {
    self.do_ensure_open(true)?;
    let mut success = false;
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<i64> {
      let body_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<i64> {
          let seq0 = self.doc_writer.update_documents(docs, del_node, self)?;
          self.maybe_process_events(seq0)
        }));
      if body_result.is_err() {
        self.tragic_event(&body_result, "updateDocuments", None)?;
      }
      let seq_no = unwrap_caught_result!(body_result)?;
      success = true;
      Ok(seq_no)
    }));

    if !success {
      if self.info_stream.is_enabled("IW") {
        self
          .info_stream
          .message("IW", "hit exception updating document")?;
      }
      self.maybe_close_on_tragic_event(None)?;
    }
    unwrap_caught_result!(res)
  }
  /// Expert: Atomically updates documents matching the provided `term` with the given
  /// DocValues fields and adds a block of documents with sequentially assigned document IDs,
  /// ensuring that an external reader will see **all or none** of the documents.
  ///
  /// One use of this API is to **retain older versions** of documents instead of replacing them.
  /// Existing documents can be updated to reflect they are no longer current,
  /// while atomically adding new documents at the same time.
  ///
  /// In contrast to `update_documents`,
  /// this method does **not delete** documents in the index matching the given term,
  /// but instead updates them with the specified DocValues fields —
  /// which can be used as a **soft-delete mechanism**.
  ///
  /// See also [`add_documents`](Self::add_documents)
  /// and `update_documents`.
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
    DF: IntoFallibleIterator<Item = Vec<Fields>>,
    D: 'static,
  {
    if soft_deletes.is_empty() {
      return Err(LuceneError::illegal_argument(
        "at least one soft delete must be present",
      ));
    }

    let updates = self.build_doc_values_update(Some(term), soft_deletes)?;
    let node = DocumentsWriterDeleteQueue::new_node_with_doc_values(updates);
    self.update_documents(Some(Arc::new(node)), docs)
  }
  /// Expert: attempts to delete by document ID, as long as the provided reader is a near-real-time
  /// reader (from [`DirectoryReader::open`]). If the provided reader is an NRT reader obtained from
  /// this writer, and its segment has not been merged away, then the delete succeeds and this method
  /// returns a valid (> 0) sequence number; else, it returns -1 and the caller must then separately
  /// delete by [`Term`] or [`Query`].
  ///
  /// **NOTE**: this method can only delete documents visible to the currently open NRT reader.
  /// If you need to delete documents indexed after opening the NRT reader you must use
  /// [`Self::delete_documents_with_terms`].
  pub fn try_delete_document<IR>(&self, reader: IR, doc_id: i32) -> Result<i64>
  where
    IR: IndexReader,
    IndexReaderContextType<IR>: IndexReaderContext<LeafReader = DefaultLeafReader<D>>,
  {
    let mut inner = self.inner.lock();
    self.try_modify_document(reader, doc_id, &DocModifierImpl1, &mut inner)
  }

  /// Expert: attempts to update doc values by document ID, as long as the provided reader is a
  /// near-real-time reader (from [`DirectoryReader::open`]). If the provided reader is an NRT
  /// reader obtained from this writer, and its segment has not been merged away, then the update
  /// succeeds and this method returns a valid (> 0) sequence number; else, it returns -1 and the
  /// caller must then either retry the update and resolve the document again. If a doc values
  /// field data is `None` the existing value is removed from all documents matching the term.
  /// This can be used to un-delete a soft-deleted document since this method will apply the
  /// field update even if the document is marked as deleted.
  ///
  /// **NOTE**: this method can only update documents visible to the currently open NRT reader.
  /// If you need to update documents indexed after opening the NRT reader you must use
  /// [`Self::update_doc_values`].
  pub fn try_update_doc_value<IR>(
    &self,
    reader: IR,
    doc_id: i32,
    fields: Vec<Fields>,
  ) -> Result<i64>
  where
    IR: IndexReader,
    IndexReaderContextType<IR>: IndexReaderContext<LeafReader = DefaultLeafReader<D>>,
  {
    let mut inner = self.inner.lock();
    let dv_updates = self.build_doc_values_update(None::<Arc<Term>>, fields)?;
    let modifier = DocModifierImpl2 { dv_updates };
    self.try_modify_document(reader, doc_id, &modifier, &mut inner)
  }

  fn try_modify_document<DM, IR>(
    &self,
    reader: IR,
    doc_id: i32,
    to_apply: &DM,
    inner: &mut Inner<D>,
  ) -> Result<i64>
  where
    DM: DocModifier,
    IR: IndexReader,
    IndexReaderContextType<IR>: IndexReaderContext<LeafReader = DefaultLeafReader<D>>,
  {
    use crate::core::index::reader_util::ReaderUtil;

    let context = reader.get_context()?;
    let leaves = context.leaves()?;
    let sub_index = ReaderUtil::sub_index_with_leaves(doc_id, leaves);
    let leaf_ctx = &leaves[sub_index];
    let leaf_reader = leaf_ctx.reader();
    let leaf_doc_id = doc_id - leaf_ctx.doc_base as i32;
    debug_assert!(leaf_doc_id >= 0);
    debug_assert!(leaf_doc_id < leaf_reader.max_doc()?);

    let info_id_owned = leaf_reader.get_original_segment_info_id().to_string();
    let info_id = info_id_owned.as_str();
    if let Some(info) = inner.segment_infos.index_of_live(info_id) {
      let rld_opt = self.get_pooled_instance(info, false)?;
      if let Some(rld) = rld_opt {
        let _guard = self.buffered_updates_stream_lock.lock();
        to_apply.run(leaf_doc_id, info_id, &rld, self, inner)?;
        return Ok(self.doc_writer.get_next_sequence_number());
      }
    }
    Ok(-1)
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
    let (mut drop_pending_docs, max_doc) = match inner.segment_infos.remove_with_id(seg_id) {
      Some(sci) => {
        let max_doc = sci.info.max_doc()?;
        (true, max_doc)
      },
      None => (false, -1),
    };
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      // this is sneaky - we might hit an error while dropping a reader, but then we have
      // already
      // removed the segment for the segmentInfo and we lost the pendingDocs update due to that.
      // therefore, we execute the adjustPendingNumDocs in a finally block to account for that.
      let dropped_reader = self.reader_pool.drop(seg_id, &mut inner.segment_infos)?;
      drop_pending_docs |= dropped_reader;
      Ok(())
    }));

    if drop_pending_docs {
      let dec = -(max_doc as i64);
      self.adjust_pending_num_docs(dec);
    }
    unwrap_caught_result!(res)
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
  /// In contrast to `update_document`,
  /// this method does **not delete** documents in the index matching the given term,
  /// but instead updates them with the specified DocValues fields —
  /// which can be used as a **soft-delete mechanism**.
  ///
  /// See also [`add_documents`](Self::add_documents) and `update_documents`.
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
    DF: IntoFallibleIterator<Item = Fields>,
    D: 'static,
  {
    if soft_deletes.is_empty() {
      return Err(LuceneError::illegal_argument(
        "at least one soft delete must be present",
      ));
    }

    let updates = self.build_doc_values_update(Some(term), soft_deletes)?;
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
    D: 'static,
  {
    let field = field.into();
    self.do_ensure_open(true)?;

    self
      .global_field_number_map
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

    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
    }));

    if res.is_err() {
      self.tragic_event(&res, "updateNumericDocValue", None)?;
    }
    unwrap_caught_result!(res)
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
    D: 'static,
  {
    let field = field.into();
    self.do_ensure_open(true)?;

    self
      .global_field_number_map
      .lock()
      .verify_or_create_dv_only_field(&field, &DocValuesType::Binary, true)?;

    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
    }));

    if res.is_err() {
      self.tragic_event(&res, "updateBinaryDocValue", None)?;
    }
    unwrap_caught_result!(res)
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
    D: 'static,
  {
    self.do_ensure_open(true)?;
    let dv_updates = self.build_doc_values_update(Some(term), updates)?;

    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      let seq = self
        .doc_writer
        .update_doc_values(&self.config, dv_updates)?;
      self.maybe_process_events(seq)
    }));

    if res.is_err() {
      self.tragic_event(&res, "updateDocValues", None)?;
    }
    unwrap_caught_result!(res)
  }

  fn build_doc_values_update<T>(
    &self,
    term: Option<T>,
    updates: Vec<Fields>,
  ) -> Result<Vec<DocValuesUpdate>>
  where
    T: Into<Arc<Term>>,
  {
    let term: Arc<Term> = match term {
      Some(t) => t.into(),
      None => Arc::new(Term::new("", BytesRef::new())),
    };
    let mut dv_updates = Vec::with_capacity(updates.len());

    for mut f in updates {
      let name = f.name().to_string();
      let field_type = f.field_type();
      let dv_type = *field_type.doc_values_type();
      if dv_type == DocValuesType::None {
        return Err(LuceneError::illegal_argument(format!(
          "can only update NUMERIC or BINARY fields! field={}",
          name
        )));
      }
      // if this field doesn't exist we try to add it.
      // if it exists and the DV type doesn't match or it is not DV only field,
      // we will get an error.
      self
        .global_field_number_map
        .lock()
        .verify_or_create_dv_only_field(&name, &dv_type, false)?;

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
              Some(n) => Some(n),
              None => {
                return Err(LuceneError::illegal_argument(format!(
                  "numeric value for field={} can not convert to i64: {:?}",
                  name, v
                )));
              },
            },
            None => None,
          };
          let sub_update = DocValuesUpdateEnum::Numeric(NumericDocValuesUpdate::new(value));
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
    // `FieldNumbers::get_field_names` returns an immutable set.
    self.global_field_number_map.lock().get_field_names()
  }

  #[cfg(test)]
  pub fn get_segment_count(&self) -> usize {
    let inner = self.inner.lock();
    inner.segment_infos.size()
  }

  #[cfg(test)]
  pub fn get_num_buffered_documents(&self) -> i32 {
    self.doc_writer.get_num_docs()
  }
  // For test purposes.
  #[cfg(test)]
  pub fn get_buffered_delete_terms_size(&self) -> Result<i32> {
    self.doc_writer.get_buffered_delete_terms_size()
  }
  /// Returns true if this index has deletions (including buffered deletions). Note that this will
  /// return true if there are buffered Term/Query deletions, even if it turns out those buffered
  /// deletions don't match any documents.
  pub fn has_deletions(&self) -> Result<bool> {
    let inner = self.inner.lock();
    self.ensure_open()?;
    if self.buffered_updates_stream.any() || self.doc_writer.any_deletions() || {
      self.reader_pool.any_deletions(&inner.segment_infos)?
    } {
      return Ok(true);
    }

    for info in inner.segment_infos.iter() {
      if info.has_deletions() {
        return Ok(true);
      }
    }

    Ok(false)
  }

  #[cfg(test)]
  pub(crate) fn max_doc(&self, i: i32) -> i32 {
    let inner = self.inner.lock();
    if i >= 0 && (i as usize) < inner.segment_infos.size() {
      inner
        .segment_infos
        .info(i as usize)
        .expect("segment info not found")
        .info
        .max_doc()
        .expect("max doc failed")
    } else {
      -1
    }
  }

  #[cfg(test)]
  pub fn get_flush_count(&self) -> i32 {
    self.flush_count.load(Ordering::SeqCst)
  }

  #[cfg(test)]
  pub fn get_flush_deletes_count(&self) -> i32 {
    self.flush_deletes_count.load(Ordering::SeqCst)
  }
  #[cfg(test)]
  pub fn flush_count(&self) -> i32 {
    self.flush_count.load(Ordering::SeqCst)
  }

  #[cfg(test)]
  pub fn flush_deletes_count(&self) -> i32 {
    self.flush_deletes_count.load(Ordering::SeqCst)
  }

  /// Performs the time-consuming merge work without holding the `IndexWriter` lock.
  fn merge_middle<P>(&self, merge: &mut OneMergeSR<D>, merge_policy: &P) -> Result<i32>
  where
    P: MergePolicy<D> + ?Sized,
  {
    let mut max_doc = -1;
    self.test_point("mergeMiddleStart")?;
    merge.check_aborted()?;

    let merge_directory = self
      .config
      .get_merge_scheduler()
      .wrap_for_merge(self.directory.clone())?;

    let context = IOContext::with_merge(merge.get_store_merge_info())?;

    let dir_wrapper = TrackingDirectoryWrapper::new(&merge_directory);
    let mut success = false;
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<i32> {
      merge.init_merge_readers(|sci_id: &String| -> Result<MergeReaderSR<D>> {
        let rld = {
          let inner = self.inner.lock();
          let sci = inner.segment_infos.index_of(sci_id).ok_or_else(|| {
            LuceneError::illegal_state(format!("segment info with id={} not found", sci_id))
          })?;
          let rld_opt = self.get_pooled_instance(sci, true)?;
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
          let Inner {
            segment_infos,
            deleter,
            ..
          } = &mut *inner;
          let sci = segment_infos.index_of(sci_id).ok_or_else(|| {
            LuceneError::illegal_state(format!("segment info with id={} not found", sci_id))
          })?;
          rld.get_reader_for_merge(&context, sci, |reader| {
            deleter.inc_ref_files(reader.reader.get_segment_info().files()?)
          })?
        };

        Ok(reader)
      })?;
      // Let the merge wrap readers
      let mut merge_readers = Vec::new();
      let soft_delete_count = new_counter(false);
      {
        let merge_reader = merge.get_merge_reader();
        for merge_reader in merge_reader.iter() {
          let reader = &merge_reader.reader;
          #[cfg(test)]
          let wrapped_reader = merge.wrap_for_merge_for_test(reader.clone())?;
          #[cfg(not(test))]
          let wrapped_reader = merge.wrap_for_merge(reader.clone())?;
          #[cfg(test)]
          let is_wrapped = wrapped_reader.is_wrapped();
          #[cfg(not(test))]
          let is_wrapped = !Arc::ptr_eq(reader, &wrapped_reader);
          self.validate_merge_reader(&wrapped_reader)?;
          let mut live_docs_wrapped_reader = None;
          if self.soft_deletes_enabled {
            // If we don't have a wrapped reader we won't preserve any soft-deletes.
            if is_wrapped {
              let hard_live_docs = merge_reader.hard_live_docs.as_ref();
              // We only need to do this accounting if we have mixed deletes.
              if let Some(hard_live_docs) = hard_live_docs {
                let wrapped_live_docs = wrapped_reader.get_live_docs()?;
                let hard_delete_counter = new_counter(false);
                self.count_soft_deletes(
                  &wrapped_reader,
                  wrapped_live_docs.as_ref(),
                  Some(hard_live_docs),
                  &soft_delete_count,
                  &hard_delete_counter,
                )?;
                let hard_delete_count: i32 = hard_delete_counter.get().try_convert()?;
                // Wrap the wrapped reader again if we have excluded some hard-deleted docs.
                if hard_delete_count > 0 {
                  let hard_live_docs = hard_live_docs.clone();
                  let live_docs = match wrapped_live_docs {
                    Some(wrapped_live_docs) => {
                      FilterCodecReaderBits::mixed(hard_live_docs, wrapped_live_docs)
                    },
                    None => FilterCodecReaderBits::Hard(hard_live_docs),
                  };
                  let num_docs = wrapped_reader.num_docs()? - hard_delete_count;
                  live_docs_wrapped_reader = Some((live_docs, num_docs));
                }
              } else {
                let carry_over_soft_deletes = reader.get_segment_info().get_soft_del_count()
                  - wrapped_reader.num_deleted_docs()?;
                debug_assert!(
                  carry_over_soft_deletes >= 0,
                  "carry-over soft-deletes must be positive"
                );
                debug_assert!(
                  self.assert_soft_deletes_count(&wrapped_reader, carry_over_soft_deletes)?
                );
                soft_delete_count.add_and_get(i64::from(carry_over_soft_deletes));
              }
            }
          }
          let wrapped_reader = match live_docs_wrapped_reader {
            Some((live_docs, num_docs)) => {
              wrap_live_docs(wrapped_reader, Some(live_docs), num_docs)
            },
            None => CodecReaderImpl::new(wrapped_reader),
          };
          merge_readers.push(wrapped_reader);
        }
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

        #[cfg(test)]
        let doc_map_opt = merge.reorder_for_test(&merged_view, self.directory.as_ref())?;
        #[cfg(not(test))]
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
          // so only do this if a present DocMap is returned.
          let v = vec![ReorderedMergeCodecReader::B(wrap_with_doc_map(
            merged_view,
            doc_map,
            None,
          )?)];
          new_merge_readers = v
        } else {
          let mut v = Vec::with_capacity(merge_readers.len());
          for cr in merge_readers.into_iter() {
            v.push(ReorderedMergeCodecReader::A(cr));
          }
          new_merge_readers = v;
        }
      } else {
        let mut v = Vec::with_capacity(merge_readers.len());
        for cr in merge_readers.into_iter() {
          v.push(ReorderedMergeCodecReader::A(cr));
        }
        new_merge_readers = v;
      }

      let doc_maps = {
        merge.check_aborted()?;
        let sci = merge.info.as_mut().unwrap();
        let soft_delete_count = soft_delete_count.get().try_convert()?;
        sci.set_soft_del_count_without_check(soft_delete_count);
        let del_count = sci.get_del_count();
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
        validate_soft_del_count(
          del_count,
          merger.merge_state.segment_info.max_doc()?,
          soft_delete_count,
        )?;

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
          return Ok(0);
        }
        doc_maps
      };

      debug_assert!(merge.info.is_some());
      let sci = merge.info.as_mut().unwrap();
      max_doc = sci.info.max_doc()?;
      #[cfg(test)]
      merge.stat.set_max_doc(max_doc);
      debug_assert!(max_doc > 0);

      // Very important to do this before opening the reader
      // because codec must know if prox was written for
      // this segment:
      let use_compound_file;
      {
        let inner = self.inner.lock();
        use_compound_file = merge_policy.use_compound_file(&inner.segment_infos, sci, self)?;
      }
      if use_compound_file {
        success = false;
        let files_to_remove = merge.info.as_ref().unwrap().files()?;

        // NOTE: Creation of the CFS file must be performed with the original
        // directory rather than with the merging directory, so that it is not
        // subject to merge throttling.
        let tracking_cfs_dir = TrackingDirectoryWrapper::new(self.directory.as_ref());

        // We'll need a mutable view of SegmentInfo to pass into create_compound_file.
        // Keep this in a tight scope.
        let cfs_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
          let sci = merge.info.as_mut().unwrap();
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
          Ok(())
        }));
        let mut cfs_aborted = false;
        let catch_result = match cfs_res {
          Ok(Ok(())) => Ok(Ok(())),
          cfs_res => {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
              let mut inner = self.inner.lock();
              if merge.is_aborted() {
                // This can happen if rollback is called while we were building
                // our CFS -- fall through to logic below to remove the non-CFS
                // merged files:
                if self.info_stream.is_enabled("IW") {
                  self.info_stream.message(
                    "IW",
                    "hit merge abort exception creating compound file during merge",
                  )?;
                }
                cfs_aborted = true;
                Ok(())
              } else {
                self.handle_merge_exception(cfs_res, merge, Some(&mut inner))
              }
            }))
          },
        };
        let finally_result = (|| -> Result<()> {
          if !success {
            if self.info_stream.is_enabled("IW") {
              self
                .info_stream
                .message("IW", "hit exception creating compound file during merge")?;
            }
            // Safe: these files must exist
            let files = merge.info.as_ref().unwrap().files()?;
            self.delete_new_files(files.iter(), None)?;
          }
          Ok(())
        })();
        IOUtils::finally_caught_result(catch_result, Ok(finally_result))?;
        if cfs_aborted {
          return Ok(0);
        }

        // So that, if we hit exc in deleteNewFiles (next) or in commitMerge (later),
        // we close the per-segment readers in the final clause below:
        success = false;
        {
          let inner = self.inner.lock();
          // delete new non cfs files directly: they were never
          // registered with IFD
          self.delete_new_files(files_to_remove.iter(), Some(&inner))?;
          if merge.is_aborted() {
            if self.info_stream.is_enabled("IW") {
              self
                .info_stream
                .message("IW", "abort merge after building CFS")?;
            }
            // Safe: these files must exist
            let files = merge.info.as_ref().unwrap().files()?;
            self.delete_new_files(files.iter(), Some(&inner))?;
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
        let write_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
          let segment_info = Arc::get_mut(&mut sci.info)
            .ok_or_else(|| LuceneError::illegal_state("Arc not unique"))?;

          self.config.get_codec().segment_info_format().write(
            self.directory.as_ref(),
            segment_info,
            &context,
          )?;

          success2 = true;
          Ok(())
        }));

        if !success2 {
          // Safe: these files must exist
          let files = sci.files()?;
          self.delete_new_files(files.iter(), None)?;
        }
        unwrap_caught_result!(write_res)?;
      }

      let merged_segment_warmer = self.config.get_merged_segment_warmer();
      if self.reader_pool.is_reader_pooling_enabled()
        && let Some(merged_segment_warmer) = merged_segment_warmer
      {
        let info = merge
          .info
          .as_ref()
          .ok_or_else(|| LuceneError::illegal_state("merge info is None"))?;
        let rld = self.get_pooled_instance(info, true)?.ok_or_else(|| {
          LuceneError::illegal_state("failed to get pooled instance for merged segment warmer")
        })?;
        let sr = {
          let mut inner = rld.inner.lock();
          readers_and_updates::get_reader(
            &IOContext::default_io_context()?,
            info,
            &mut inner,
            rld.index_created_version_major,
          )?;
          inner.reader.as_ref().unwrap().clone()
        };
        let warm_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
          merged_segment_warmer.warm(&sr)
        }));

        let finally_result = {
          let mut inner = self.inner.lock();
          match rld.release(sr.as_ref(), None) {
            Ok(()) => self.release(rld.as_ref(), &mut inner),
            Err(e) => Err(e),
          }
        };

        IOUtils::finally_caught_result(warm_result, Ok(finally_result))?;
      }

      if !self.commit_merge(merge, &doc_maps)? {
        // commitMerge will return false if this merge was
        // aborted
        return Ok(0);
      }
      success = true;
      Ok(0)
    }));
    if !success {
      self.close_merge_readers(merge, true, false, None)?;
    }
    unwrap_caught_result!(res)?;
    Ok(max_doc)
  }
  fn add_merge_exception<CR>(&self, merge: &OneMerge<D, CR>, inner: Option<&mut Inner<D>>)
  where
    CR: CodecReader,
  {
    let inner = match inner {
      Some(inner) => inner,
      None => &mut *self.inner.lock(),
    };
    debug_assert!(merge.stat.has_exception());
    if !inner.merge_exceptions.contains(&merge.stat) && inner.merge_gen == merge.stat.merge_gen {
      inner.merge_exceptions.push(merge.stat.clone());
    }
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
  /// If an error occurs, for example due to running out of disk space, the
  /// index will not be corrupted and no documents will be lost. However, the index
  /// may have been partially merged (some segments were merged but not all), and it
  /// is possible that one of the segments will be left in non-compound format even
  /// when compound file format is enabled. This can happen if the error occurs
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
  pub fn force_merge(&self, max_num_segments: i32) -> Result<()>
  where
    D: 'static,
  {
    self.force_merge_with_wait(max_num_segments, true)
  }

  /// Forces merging of all segments that have deleted documents. The actual merges to be executed
  /// are determined by the `MergePolicy`. For example, the default `TieredMergePolicy`
  /// will only pick a segment if the percentage of deleted docs is over 10%.
  ///
  /// This is often a horribly costly operation; rarely is it warranted.
  ///
  /// To see how many deletions you have pending in your index, call [`IndexReader::num_deleted_docs`].
  ///
  /// **NOTE**: this method first flushes a new segment (if there are indexed documents), and
  /// applies all buffered deletes.
  pub fn force_merge_deletes(&self) -> Result<()>
  where
    D: 'static,
  {
    self.force_merge_deletes_with_wait(true)
  }

  /// Just like [`Self::force_merge_deletes`], except you can specify whether the call should block
  /// until the operation completes. This is only meaningful with a [`MergeScheduler`] that is
  /// able to run merges in background threads.
  pub fn force_merge_deletes_with_wait(&self, do_wait: bool) -> Result<()>
  where
    D: 'static,
  {
    self.ensure_open()?;
    self.flush_with_apply_merge_deletes(true, true)?;

    if self.info_stream.is_enabled("IW") {
      self.info_stream.message(
        "IW",
        &format!("forceMergeDeletes: index now {}", self.seg_string(None)?),
      )?;
    }

    let merge_policy = self.config.get_merge_policy();
    let caching_merge_context = CachingMergeContext::new(self);
    let requested_merges = {
      let mut inner = self.inner.lock();
      let spec = merge_policy.find_forced_deletes_merges(
        &inner.segment_infos,
        Some(&inner),
        &caching_merge_context,
      )?;

      match spec {
        Some(spec) => {
          let merge_stats: Vec<MergeStat> = spec.merges.iter().map(|m| m.stat.clone()).collect();
          for merge in spec.merges {
            let _ = self.register_merge(merge, &mut inner)?;
          }
          Some(merge_stats)
        },
        None => None,
      }
    };

    let merge_source = self.new_merge_source()?;
    self
      .config
      .get_merge_scheduler()
      .merge(merge_source, MergeTrigger::Explicit)?;

    if let Some(requested_merges) = requested_merges.filter(|_| do_wait) {
      let mut inner = self.inner.lock();
      loop {
        if let Some(t) = self.tragedy.get() {
          let mut error = LuceneError::illegal_state(
            "this writer hit an unrecoverable error; cannot complete forceMergeDeletes",
          );
          error.add_suppressed(t.clone());
          return Err(error);
        }

        let running = requested_merges.iter().any(|merge_stat| {
          inner
            .pending_merges
            .iter()
            .any(|merge| merge.stat == *merge_stat)
            || inner.running_merges.contains(merge_stat)
        });

        if running {
          self.do_wait(&mut inner);
        } else {
          break;
        }
      }
    }

    Ok(())
  }
  /// Just like `IndexWriter::force_merge`, except you can specify whether the call
  /// should block until all merging completes.
  ///
  /// This is only meaningful with a [`MergeScheduler`] that is able to run merges
  /// in background threads.
  pub fn force_merge_with_wait(&self, max_num_segments: i32, do_wait: bool) -> Result<()>
  where
    D: 'static,
  {
    self.ensure_open()?;

    if max_num_segments < 1 {
      return Err(LuceneError::illegal_argument(format!(
        "maxNumSegments must be >= 1; got {}",
        max_num_segments
      )));
    }

    if self.info_stream.is_enabled("IW") {
      self.info_stream.message(
        "IW",
        &format!("forceMerge: index now {}", self.seg_string(None)?),
      )?;
      self.info_stream.message("IW", "now flush at forceMerge")?;
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
          segments_to_merge.insert(info.info.get_id_key().to_string(), Some(true));
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
        merge.stat.set_max_num_segments(max_num_segments);
        if let Some(info_id) = merge.stat.info_id() {
          // this can be None since we register the merge under lock before we then do the actual
          // merge and
          // set the merge.info in _mergeInit
          segments_to_merge.insert(info_id, Some(true));
        }
      }

      for merge in running_merges.iter() {
        merge.set_max_num_segments(max_num_segments);
        if let Some(info_id) = merge.info_id() {
          // this can be None since we put the merge on runningMerges before we do the actual merge
          // and set the merge.info in _mergeInit
          segments_to_merge.insert(info_id, Some(true));
        }
      }
    }

    self.maybe_merge_with_max_num_segments(
      self.config.get_merge_policy(),
      MergeTrigger::Explicit,
      max_num_segments,
    )?;

    if do_wait {
      let mut inner = self.inner.lock();
      loop {
        if let Some(t) = self.tragedy.get() {
          let mut error = LuceneError::illegal_state(
            "this writer hit an unrecoverable error; cannot complete forceMerge",
          );
          error.add_suppressed(t.clone());
          return Err(error);
        }

        if !inner.merge_exceptions.is_empty() {
          // Forward any exceptions in background merge
          // threads to the current thread:
          for merge in &inner.merge_exceptions {
            if merge.max_num_segments() != UNBOUNDED_MAX_MERGE_SEGMENTS {
              let mut error = LuceneError::from(std::io::Error::other(format!(
                "background merge hit exception: {}",
                Self::segment_ids_to_string(&merge.segments)
              )));
              if let Some(exception) = merge
                .get_exception()
                .and_then(|result| result.caught_failure("panic from a background merge"))
              {
                error.add_suppressed(exception);
              }
              return Err(error);
            }
          }
        }

        if self.max_num_segments_merges_pending(&inner) {
          self.test_point("forceMergeBeforeWait")?;
          self.do_wait(&mut inner);
        } else {
          break;
        }
      }

      // If close is called while we are still
      // running, return an error so the calling
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
      if merge.stat.max_num_segments() != UNBOUNDED_MAX_MERGE_SEGMENTS {
        return true;
      }
    }

    for merge in inner.running_merges.iter() {
      if merge.max_num_segments() != UNBOUNDED_MAX_MERGE_SEGMENTS {
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
  pub fn maybe_merge(&self) -> Result<()>
  where
    D: 'static,
  {
    self.maybe_merge_with_max_num_segments(
      self.config.get_merge_policy(),
      MergeTrigger::Explicit,
      UNBOUNDED_MAX_MERGE_SEGMENTS,
    )
  }

  fn maybe_merge_with_max_num_segments<P>(
    &self,
    merge_policy: &P,
    trigger: MergeTrigger,
    max_num_segments: i32,
  ) -> Result<()>
  where
    P: MergePolicy<D> + ?Sized,
    D: 'static,
  {
    self.do_ensure_open(false)?;
    if self
      .update_pending_merges(merge_policy, trigger, max_num_segments, None)?
      .is_some()
    {
      self.execute_merge(trigger)?;
    }
    Ok(())
  }

  pub(crate) fn execute_merge(&self, trigger: MergeTrigger) -> Result<()>
  where
    D: 'static,
  {
    let merge_source = self.new_merge_source()?;
    self
      .config
      .get_merge_scheduler()
      .merge(merge_source, trigger)
  }
  fn update_pending_merges<P>(
    &self,
    merge_policy: &P,
    trigger: MergeTrigger,
    max_num_segments: i32,
    inner: Option<&mut Inner<D>>,
  ) -> Result<Option<UpdatePendingMergesResult<D>>>
  where
    P: MergePolicy<D> + ?Sized,
  {
    // In case infoStream was disabled on init, but then enabled at some
    // point, try again to log the config here:
    let inner = match inner {
      Some(i) => i,
      None => &mut *self.inner.lock(),
    };

    debug_assert!(max_num_segments == UNBOUNDED_MAX_MERGE_SEGMENTS || max_num_segments > 0);

    if !inner.merges.are_enabled() {
      return Ok(None);
    }

    // Do not start new merges if disaster struck
    if self.tragedy.get().is_some() {
      return Ok(None);
    }

    let caching_merge_context = CachingMergeContext::new(self);
    let mut spec_opt: Option<DefaultMergeSpecification<D>>;

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
        max_num_segments.try_convert()?,
        &inner.segments_to_merge,
        Some(inner),
        &caching_merge_context,
      )?;

      if let Some(ref mut spec) = spec_opt {
        for m in &mut spec.merges {
          m.stat.set_max_num_segments(max_num_segments);
        }
      }
    } else {
      spec_opt = match trigger {
        MergeTrigger::GetReader | MergeTrigger::Commit => merge_policy.find_full_flush_merges(
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
        let mut registered_merges = Vec::new();
        let mut rejected_merges = Vec::new();
        for m in spec.merges.into_iter() {
          match self.register_merge(m, inner)? {
            RegisterMergeResult::Registered(merge_stat) => {
              registered_merges.push(merge_stat);
            },
            RegisterMergeResult::Rejected(merge) => {
              rejected_merges.push(*merge);
            },
          }
        }
        Ok(Some(UpdatePendingMergesResult {
          registered_merges,
          rejected_merges,
        }))
      },
      _ => Ok(None),
    }
  }
  /// **Expert:** the [`MergeScheduler`] calls this method to retrieve the next merge
  /// requested by the [`MergePolicy`].
  fn get_next_merge(&self) -> Result<Option<OneMergeSR<D>>> {
    let mut inner = self.inner.lock();

    if let Some(t) = self.tragedy.get() {
      let mut error =
        LuceneError::illegal_state("this writer hit an unrecoverable error; cannot merge");
      error.add_suppressed(t.clone());
      return Err(error);
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

    if let Some(t) = self.tragedy.get() {
      let mut error =
        LuceneError::illegal_state("this writer hit an unrecoverable error; cannot merge");
      error.add_suppressed(t.clone());
      return Err(error);
    }

    Ok(!inner.pending_merges.is_empty())
  }

  fn rollback_internal(&self, commit_lock: Option<&CommitInner<D>>) -> Result<()>
  where
    D: 'static,
  {
    // Make sure no commit is running, else e.g. we can close while another thread is still
    // fsync'ing.
    match commit_lock {
      Some(commit_lock) => self.rollback_internal_no_commit(commit_lock)?,
      None => {
        let commit_lock = self.commit_lock.lock();
        self.rollback_internal_no_commit(&commit_lock)?;
      },
    }

    debug_assert!({
      let pending_num_docs = self.pending_num_docs.load(Ordering::SeqCst);
      let total_max_doc = self.inner.lock().segment_infos.total_max_doc()? as i64;
      pending_num_docs == total_max_doc
    });
    Ok(())
  }

  fn rollback_internal_no_commit(&self, commit_lock: &CommitInner<D>) -> Result<()>
  where
    D: 'static,
  {
    if self.info_stream.is_enabled("IW") {
      self.info_stream.message("IW", "rollback")?;
    }

    let cleanup_and_notify = || -> Result<()> {
      self.closed.store(true, Ordering::SeqCst);
      self.closing.store(false, Ordering::SeqCst);
      self.pausing.notify_all();
      Ok(())
    };

    let mut throwable = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      {
        let mut inner = self.inner.lock();
        // must be synced otherwise register merge might return an error if merges
        // change concurrently; abort_merges is synced as well.
        self.abort_merges(&mut inner)?;
        debug_assert!(
          inner.merging_segments.is_empty(),
          "we aborted all merges but still have merging segments: {:?}",
          inner.merging_segments
        );
      }

      if self.info_stream.is_enabled("IW") {
        self
          .info_stream
          .message("IW", "rollback: done finish merges")?;
      }

      // Must pre-close in case it increments change_count so that we can then
      // set it to false before calling rollback_internal.
      self.config.get_merge_scheduler().close()?;

      self.doc_writer.close()?;
      self.doc_writer.abort(self.get_config())?;
      self.doc_writer.flush_control.wait_for_flush();
      self.publish_flushed_segments(true)?;
      self.event_queue.close(self)?;

      let mut inner = self.inner.lock();

      let mut pending_commit = commit_lock.pending_commit.borrow_mut();
      if pending_commit.is_some() {
        let dec_result = {
          let pending_commit = pending_commit.as_mut().unwrap();
          pending_commit.rollback_commit(self.directory.as_ref());
          std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            inner.deleter.dec_ref_from_segment(pending_commit)
          }))
        };
        *pending_commit = None;
        self.pausing.notify_all();
        unwrap_caught_result!(dec_result)?;
      }

      let total_max_doc = inner.segment_infos.total_max_doc()?;
      // Keep the same segmentInfos instance but replace all
      // of its SegmentInfo instances so IFD below will remove
      // any segments we flushed since the last commit:
      let rollback_segments = inner.rollback_segments.clone();
      inner
        .segment_infos
        .rollback_segment_infos(rollback_segments)?;
      let rollback_max_doc = inner.segment_infos.total_max_doc()?;
      // now we need to adjust this back to the rolled back SI but don't set it to the absolute
      // value
      // otherwise we might hide internal bugsf
      self.adjust_pending_num_docs(-((total_max_doc - rollback_max_doc) as i64));

      if self.info_stream.is_enabled("IW") {
        self.info_stream.message(
          "IW",
          &format!(
            "rollback: infos={}",
            self.seg_string_from_infos(inner.segment_infos.iter())?
          ),
        )?;
      }

      self.test_point("rollback before checkpoint")?;
      // Ask deleter to locate unreferenced files & remove
      // them ... only when we are not experiencing a tragedy, else
      // these methods return ACE:
      if self.tragedy.get().is_none() {
        let (deleter, segment_infos) = {
          let v = &mut *inner;
          (&mut v.deleter, &v.segment_infos)
        };
        deleter.checkpoint(
          segment_infos,
          false,
          self.config.get_index_deletion_policy(),
        )?;
        inner.deleter.refresh()?;
        inner.deleter.close()?;
      }

      self
        .last_commit_change_count
        .store(inner.change_count, Ordering::SeqCst);
      // Don't bother saving any changes in our segmentInfos
      self.reader_pool.close(&mut inner.segment_infos)?;
      // Must set closed while inside same sync block where we call deleter.refresh, else
      // concurrent threads may try to sneak a flush in,
      // after we leave this sync block and before we enter the sync block in the finally clause
      // below that sets closed:
      self.closed.store(true, Ordering::SeqCst);

      IOUtils::close(0..2, |operation| match operation {
        0 => IOUtils::close_one_ref(self.writer_lock()),
        1 => cleanup_and_notify(),
        _ => unreachable!(),
      })?;
      Ok(())
    }));

    if matches!(&throwable, Ok(Ok(()))) {
      return Ok(());
    }

    let cleanup_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      IOUtils::close_while_handling_error(0..2, |operation| match operation {
        0 => self.config.get_merge_scheduler().close(),
        1 => {
          let mut inner = self.inner.lock();

          let mut pending_commit = commit_lock.pending_commit.borrow_mut();
          if pending_commit.is_some() {
            let pending_result =
              std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
                let pending_commit = pending_commit.as_mut().unwrap();
                pending_commit.rollback_commit(self.directory.as_ref());
                inner.deleter.dec_ref_from_segment(pending_commit)?;
                Ok(())
              }));
            *pending_commit = None;
            throwable.add_suppressed(pending_result, "panic while rolling back pending commit");
          }

          IOUtils::close_while_handling_error(0..4, |operation| match operation {
            0 => self.reader_pool.close(&mut inner.segment_infos),
            1 => inner.deleter.close(),
            2 => IOUtils::close_one_ref(self.writer_lock()),
            3 => cleanup_and_notify(),
            _ => unreachable!(),
          })
        },
        _ => unreachable!(),
      })
    }));
    throwable.add_suppressed(cleanup_result, "panic while cleaning up rollback");

    if throwable.is_err() {
      let tragic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        self.tragic_event(&throwable, "rollbackInternal", Some(commit_lock))
      }));
      throwable.add_suppressed(tragic_result, "panic while handling tragic rollback event");
    }

    unwrap_caught_result!(throwable)
  }
  fn writer_lock(&self) -> &D::Lock {
    &self.directory.write_lock
  }
  /// Delete all documents in the index.
  ///
  /// This method will drop all buffered documents and will remove all segments from the index.
  /// This change will not be visible until `commit` has been called. This method can be
  /// rolled back using `rollback`.
  ///
  /// NOTE: this method is much faster than using `delete_documents(new MatchAllDocsQuery())`. Yet,
  /// this method also has different semantics compared to `delete_documents` since
  /// internal data-structures are cleared as well as all segment information is forcefully dropped
  /// anti-viral semantics like omitting norms are reset or doc value types are cleared. Essentially
  /// a call to `delete_all` is equivalent to creating a new [`IndexWriter`] with
  /// [`OpenMode::Create`] which a delete query only marks documents as deleted.
  ///
  /// NOTE: this method will forcefully abort all merges in progress. If other threads are running
  /// `force_merge`, `add_indexes` or `force_merge_deletes` methods,
  /// they may receive `MergeAbortedError` errors.
  ///
  /// Returns the sequence number for this operation.
  pub fn delete_all(&self) -> Result<i64>
  where
    D: 'static,
  {
    self.ensure_open()?;

    // Remove any buffered docs. Hold the full flush lock to prevent concurrent commits / NRT
    // reopens from getting in our way and doing unnecessary work.
    /* hold the full flush lock to prevent concurrency commits / NRT reopens to
     * get in our way and do unnecessary work. -- if we don't lock this here we might
     * get in trouble if */
    /*
     * We first abort and trash everything we have in-memory
     * and keep the thread-states locked, the lockAndAbortAll operation
     * also guarantees "point in time semantics" ie. the checkpoint that we need in terms
     * of logical happens-before relationship in the DW. So we do
     * abort all in memory structures
     * We also drop global field numbering before during abort to make
     * sure it's just like a fresh index.
     */
    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<i64> {
      let _full_flush_guard = self.full_flush_lock.lock();
      let mut finalizer = self.doc_writer.lock_and_abort_all(&self.config)?;
      let operation_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<i64> {
          self.process_events(false)?;

          let mut inner = self.inner.lock();

          // Abort any running merges.
          let abort_result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
              self.abort_merges(&mut inner)?;
              debug_assert!(
                !inner.merges.are_enabled(),
                "merges should be disabled - who enabled them?"
              );
              debug_assert!(
                inner.merging_segments.is_empty(),
                "found merging segments but merges are disabled: {:?}",
                inner.merging_segments
              );
              Ok(())
            }));

          inner.merges.enable(self)?;
          unwrap_caught_result!(abort_result)?;

          self.adjust_pending_num_docs(-(inner.segment_infos.total_max_doc()? as i64));

          // Remove all segments.
          inner.segment_infos.clear();

          // Ask deleter to locate unreferenced files & remove them:
          let (deleter, segment_infos) = {
            let v = &mut *inner;
            (&mut v.deleter, &v.segment_infos)
          };
          deleter.checkpoint(
            segment_infos,
            false,
            self.config.get_index_deletion_policy(),
          )?;

          // Don't bother saving any changes in our segmentInfos.
          self.reader_pool.drop_all(&mut inner.segment_infos)?;

          // Mark that the index has changed.
          inner.change_count += 1;
          inner.segment_infos.changed();
          self.global_field_number_map.lock().clear();
          success = true;
          Ok(self.doc_writer.get_next_sequence_number())
        }));
      let body_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<i64> {
          if !success && self.info_stream.is_enabled("IW") {
            self
              .info_stream
              .message("IW", "hit exception during deleteAll")?;
          }
          unwrap_caught_result!(operation_result)
        }));
      let close_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| finalizer.close()));
      IOUtils::use_or_suppress_caught_result(body_result, close_result)
    }));

    if result.is_err() {
      self.tragic_event(&result, "deleteAll", None)?;
    }
    unwrap_caught_result!(result)
  }
  /// Aborts running merges. Be careful when using this method: when you abort a long-running merge,
  /// you lose a lot of work that must later be redone.
  fn abort_merges(&self, inner: &mut MutexGuard<'_, Inner<D>>) -> Result<()> {
    inner.merges.disable();

    // Abort all pending & running merges:
    let mut pending_merges = std::mem::take(&mut inner.pending_merges);
    IOUtils::close(pending_merges.make_contiguous().iter_mut(), |merge| {
      if self.info_stream.is_enabled("IW") {
        self.info_stream.message(
          "IW",
          &format!(
            "now abort pending merge {}",
            Self::segment_ids_to_string(&merge.stat.segments)
          ),
        )?;
      }
      self.abort_one_merge(merge, inner)?;
      self.merge_finish(&merge.stat, Some(inner));
      Ok(())
    })?;

    for merge_stat in &inner.running_merges {
      if self.info_stream.is_enabled("IW") {
        self.info_stream.message(
          "IW",
          &format!(
            "now abort running merge {}",
            Self::segment_ids_to_string(&merge_stat.segments)
          ),
        )?;
      }
      merge_stat.set_aborted();
    }

    // We wait here to make all merges stop. It should not take very long because they
    // periodically check if they are aborted.
    while !inner.running_merges.is_empty() || !inner.running_add_indexes_merges.is_empty() {
      if self.info_stream.is_enabled("IW") {
        self.info_stream.message(
          "IW",
          &format!(
            "now wait for {} running merge/s to abort; currently running addIndexes: {}",
            inner.running_merges.len(),
            inner.running_add_indexes_merges.len()
          ),
        )?;
      }
      self.do_wait(inner);
    }

    self.pausing.notify_all();
    if self.info_stream.is_enabled("IW") {
      self
        .info_stream
        .message("IW", "all running merges have aborted")?;
    }
    Ok(())
  }

  fn segment_ids_to_string(ids: &[String]) -> String {
    ids.join(" ")
  }
  /// Waits for any currently outstanding merges to finish.
  ///
  /// It is guaranteed that any merges started prior to calling this method
  /// will have completed once this method returns.
  pub(crate) fn wait_for_merges(&self) -> Result<()>
  where
    D: 'static,
  {
    let merge_source = self.new_merge_source()?;
    self
      .config
      .get_merge_scheduler()
      .merge(merge_source, MergeTrigger::Closing)?;
    let mut inner = self.inner.lock();
    self.do_ensure_open(false)?;
    if self.info_stream.is_enabled("IW") {
      self.info_stream.message("IW", "waitForMerges")?;
    }
    while !inner.pending_merges.is_empty() || !inner.running_merges.is_empty() {
      self.do_wait(&mut inner);
    }
    debug_assert!(
      inner.merging_segments.is_empty(),
      "mergingSegments should be empty here"
    );
    if self.info_stream.is_enabled("IW") {
      self.info_stream.message("IW", "waitForMerges done")?;
    }

    Ok(())
  }

  /// Revisits the configured deletion policy and removes any now-unused commit files.
  ///
  /// This is useful for deletion policies that keep commits alive until application state changes,
  /// such as [`SnapshotDeletionPolicy`](crate::core::index::snapshot_deletion_policy::SnapshotDeletionPolicy).
  pub fn delete_unused_files(&self) -> Result<()>
  where
    D: 'static,
  {
    let mut inner = self.inner.lock();
    self.do_ensure_open(true)?;
    inner
      .deleter
      .revisit_policy(self.config.get_index_deletion_policy())
  }

  fn checkpoint(&self, inner: &mut Inner<D>) -> Result<()> {
    changed(&mut inner.change_count, &mut inner.segment_infos);
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
    let (next_gen, packet) = self.buffered_updates_stream.push(packet)?;
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
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      // Lock order IW -> BDS
      self.do_ensure_open(false)?;

      if self.info_stream.is_enabled("IW") {
        self
          .info_stream
          .message("IW", &format!("publishFlushedSegment {}", new_segment))?;
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
        self.buffered_updates_stream.finished_segment(v)?;
        v
      };

      if self.info_stream.is_enabled("IW") {
        let segs = self.seg_string_from_info(&new_segment)?;
        self.info_stream.message(
          "IW",
          &format!("publish sets newSegment delGen={} seg={}", next_gen, segs),
        )?;
      }
      new_segment.set_buffered_deletes_gen(next_gen)?;
      let new_segment_id = new_segment.info.get_id_key().to_string();
      inner.segment_infos.add(new_segment)?;
      published = true;
      self.checkpoint(&mut *inner)?;
      let new_segment = inner.segment_infos.index_of(&new_segment_id).unwrap();
      if packet_any && let Some(sort_map) = sort_map {
        let _ = self.get_pooled_instance_with_sort_map(new_segment, true, sort_map)?;
      }
      // this is a corner case where documents delete them-self with soft deletes. This is used to
      // build delete tombstones etc. in this case we haven't seen any updates to the DV in this
      // fresh flushed segment.
      // if we have seen updates the update code checks if the segment is fully deleted.
      let has_initial_soft_deleted = {
        if let Some(name) = self.config.get_soft_deletes_field() {
          if let Some(fi) = field_infos.field_info_by_name(name)? {
            fi.get_doc_values_gen() == -1 && *fi.get_doc_values_type() != DocValuesType::None
          } else {
            false
          }
        } else {
          false
        }
      };
      let is_fully_hard_deleted = new_segment.get_del_count() == new_segment.info.max_doc()?;
      // we either have a fully hard-deleted segment or one or more docs are soft-deleted. In both
      // cases we need
      // to go and check if they are fully deleted. This has the nice side effect that we now have
      // accurate numbers
      // for the soft delete right after we flushed to disk.
      if has_initial_soft_deleted || is_fully_hard_deleted {
        let rld = self.get_pooled_instance(new_segment, true)?;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
          match rld {
            None => {
              return Err(LuceneError::illegal_state(
                "failed to open newly flushed segment",
              ));
            },
            Some(ref rld) => {
              let new_segment = inner.segment_infos.index_of(&new_segment_id).unwrap();
              let is_fully_deleted = self.is_fully_deleted(rld, new_segment, &inner)?;
              if is_fully_deleted {
                self.drop_deleted_segment(&new_segment_id, &mut *inner)?;
                self.checkpoint(&mut *inner)?;
              }
            },
          }
          Ok(())
        }));
        self.release(&rld.unwrap(), &mut *inner)?;
        unwrap_caught_result!(result)?;
      }
      Ok(())
    }));

    if !published {
      self.adjust_pending_num_docs(-(max_doc as i64));
    }
    self.flush_count.fetch_add(1, Ordering::SeqCst);
    if let Some(ref s) = self.hooks {
      s.do_after_flush()?
    }

    unwrap_caught_result!(res)
  }
  fn reset_merge_exceptions(&self, inner: &mut MutexGuard<'_, Inner<D>>) {
    inner.merge_exceptions.clear();
    inner.merge_gen += 1;
  }

  pub(crate) fn no_dup_dirs<SD>(&self, dirs: &[Arc<SD>]) -> Result<()>
  where
    SD: Directory,
  {
    let mut seen_dir_ids = HashSet::with_capacity(dirs.len());
    let self_dir_id = self.directory_orig.identity().clone();
    for dir in dirs {
      let dir_id = dir.identity().clone();
      if dir_id == self_dir_id {
        return Err(LuceneError::illegal_argument(
          "Cannot add directory to itself",
        ));
      }
      if !seen_dir_ids.insert(dir_id) {
        return Err(LuceneError::illegal_argument(format!(
          "Directory {} appears more than once",
          dir
        )));
      }
    }

    Ok(())
  }
  fn acquire_write_locks<SD>(&self, dirs: &[Arc<SD>]) -> Result<Vec<SD::Lock>>
  where
    SD: Directory,
  {
    let mut locks = Vec::with_capacity(dirs.len());
    for dir in dirs {
      let mut success = false;
      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        locks.push(dir.obtain_lock(WRITE_LOCK_NAME)?);
        success = true;
        Ok(())
      }));
      if !success {
        IOUtils::close_while_handling_error(&locks, CloseableRef::close)?;
      }
      unwrap_caught_result!(result)?;
    }
    Ok(locks)
  }
  /// Copies all segments from the provided directories into this index without re-merging them.
  ///
  /// This is the Rust counterpart of Lucene's `addIndexes(Directory...)` and follows the
  /// same coarse-grained flow:
  /// flush current in-memory changes, validate incoming commits, copy segment files as-is,
  /// reserve document ids, publish the new segments, and finally trigger merges if needed.
  pub fn add_indexes_from_directory<SD>(&self, dirs: &[Arc<SD>]) -> Result<i64>
  where
    D: 'static,
    SD: Directory + 'static,
  {
    self.ensure_open()?;
    self.no_dup_dirs(dirs)?;

    let locks = self.acquire_write_locks(dirs)?;
    let index_sort = self.config.get_index_sort();
    let mut success_top = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<i64> {
      if self.info_stream.is_enabled("IW") {
        self
          .info_stream
          .message("IW", "flush at addIndexes(Directory...)")?;
      }
      self.flush_with_apply_merge_deletes(false, true)?;

      let mut total_max_doc = 0_i64;
      let mut commits = Vec::with_capacity(dirs.len());
      let index_created_version_major = self.get_index_major_version_created();

      for dir in dirs {
        if self.info_stream.is_enabled("IW") {
          self
            .info_stream
            .message("IW", &format!("addIndexes: process directory {}", dir))?;
        }
        let sis = SegmentInfos::read_latest_commit(dir.clone())?;
        if index_created_version_major != sis.get_index_created_version_major() {
          return Err(LuceneError::illegal_argument(format!(
            "Cannot use add_indexes(Directory...) with indexes that have been created by a different Lucene version. The current index was generated by Lucene {} while one of the directories contains an index that was generated with Lucene {}",
            index_created_version_major,
            sis.get_index_created_version_major()
          )));
        }
        total_max_doc += i64::from(sis.total_max_doc()?);
        commits.push(sis);
      }

      self.test_reserve_docs(total_max_doc)?;

      let mut infos: Vec<SegmentCommitInfo<D>> = Vec::new();
      let mut success = false;
      let copy_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        for sis in &commits {
          for info in sis.iter() {
            debug_assert!(
              !infos.iter().any(|new_info| std::ptr::eq(
                (new_info as *const SegmentCommitInfo<D>).cast::<()>(),
                (info as *const SegmentCommitInfo<SD>).cast::<()>(),
              )),
              "dup info dir={} name={}",
              info.info.dir,
              info.info.name
            );

            let segment_index_sort = info.info.get_index_sort();
            if let Some(index_sort) = index_sort.as_ref()
              && segment_index_sort
                .as_ref()
                .map(|sort| !is_congruent_sort(index_sort, sort))
                .unwrap_or(true)
            {
              return Err(LuceneError::illegal_argument(format!(
                "cannot change index sort from {} to {}",
                segment_index_sort
                  .as_ref()
                  .map(ToString::to_string)
                  .unwrap_or_else(|| "null".to_string()),
                index_sort
              )));
            }

            let new_seg_name = self.new_segment_name(None);
            if self.info_stream.is_enabled("IW") {
              self.info_stream.message(
                "IW",
                &format!(
                  "addIndexes: process segment origName={} newName={} info={}",
                  info.info.name, new_seg_name, info
                ),
              )?;
            }

            let context =
              IOContext::with_flush(FlushInfo::new(info.info.max_doc()?, info.size_in_bytes()?))?;

            let fis = read_field_infos(info)?;
            for fi in fis.iter() {
              self.global_field_number_map.lock().add_or_get(fi)?;
            }
            infos.push(self.copy_segment_as_is(info, &new_seg_name, &context)?);
          }
        }
        success = true;
        Ok(())
      }));

      if !success {
        for sipc in &infos {
          self.delete_new_files(sipc.files()?.iter(), None)?;
        }
      }
      unwrap_caught_result!(copy_result)?;

      let seq_no = {
        let mut inner = self.inner.lock();
        success = false;
        let publish_result =
          std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<i64> {
            self.ensure_open()?;
            self.reserve_docs(total_max_doc)?;
            let seq_no = self.doc_writer.get_next_sequence_number();
            success = true;
            Ok(seq_no)
          }));
        if !success {
          for sipc in &infos {
            self.delete_new_files(sipc.files()?.iter(), Some(&inner))?;
          }
        }
        let seq_no = unwrap_caught_result!(publish_result)?;
        inner.segment_infos.add_all(infos)?;
        self.checkpoint(&mut inner)?;
        seq_no
      };
      success_top = true;
      Ok(seq_no)
    }));

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<i64> {
      if result.is_err() {
        self.tragic_event(&result, "addIndexes(Directory...)", None)?;
      }
      unwrap_caught_result!(result)
    }));
    if success_top {
      IOUtils::close_refs(&locks)?;
    } else {
      IOUtils::close_while_handling_error(&locks, CloseableRef::close)?;
    }
    let seq_no = unwrap_caught_result!(result)?;
    self.maybe_merge()?;
    Ok(seq_no)
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
      let old_sort = leaf_index_sort
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_else(|| "None".to_string());
      return Err(LuceneError::illegal_argument(format!(
        "cannot change index sort from {} to {}",
        old_sort, index_sort
      )));
    }

    Ok(())
  }

  /// Merges the provided indexes into this index.
  ///
  /// The provided `IndexReader`s are not closed.
  ///
  /// See `Self::add_indexes` for details on transactional semantics, temporary free space
  /// required in the `Directory`, and non-CFS segments on an error.
  ///
  /// **NOTE:** empty segments are dropped by this method and not added to this index.
  ///
  /// **NOTE:** provided `LeafReader`s are merged as specified by the
  /// `MergePolicy::find_merges(CodecReader...)` API. Default behavior is to merge all provided
  /// readers into a single segment. Customize this by implementing the `find_merge` API in your
  /// custom merge policy.
  ///
  /// # Returns
  ///
  /// The [sequence number](#sequence_number) for this operation.
  ///
  /// # Errors
  ///
  /// Returns:
  /// - [`LuceneError::CorruptIndex`] if the index is corrupt
  /// - an error if there is a low-level IO error
  /// - [`LuceneError::IllegalArgument`] if `add_indexes` would cause the index to exceed `MAX_DOCS`
  pub fn add_indexes_from_codec_readers<CR>(&self, readers: Vec<CR>) -> Result<i64>
  where
    D: 'static,
    CR: CodecReader + 'static,
    OneMerge<D, Arc<CR>>: Send + 'static,
  {
    self.ensure_open()?;
    let readers: Vec<_> = readers.into_iter().map(Arc::new).collect();
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<i64> {
      let mut num_docs = 0_i64;
      for leaf in &readers {
        self.validate_merge_reader(leaf)?;
        let field_infos = leaf.get_field_infos()?;
        {
          let global_field_number_map = self.global_field_number_map.lock();
          for fi in field_infos.iter() {
            global_field_number_map.verify_field_info(fi)?;
          }
        }
        num_docs += i64::from(leaf.num_docs()?);
      }
      self.test_reserve_docs(num_docs)?;

      {
        let inner = self.inner.lock();
        self.ensure_open()?;
        if !inner.merges.are_enabled() {
          return Err(LuceneError::already_closed(
            "this IndexWriter is closed. Cannot execute add_indexes(CodecReaders...) API",
          ));
        }
      }
      let merge_policy = self.config.get_merge_policy();
      let (mut merges, merge_stats) = match merge_policy.find_merges_readers(readers)? {
        Some(spec) if !spec.merges.is_empty() => {
          let merge_source = self.new_add_indexes_merge_source::<Arc<CR>>()?;
          let mut merge_stats = Vec::with_capacity(spec.merges.len());
          {
            let mut inner = self.inner.lock();
            for om in spec.merges {
              merge_stats.push(om.stat.clone());
              merge_source.register_merge(om, &mut inner);
            }
          }
          let schedule_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self
              .config
              .get_merge_scheduler()
              .merge(merge_source.clone(), MergeTrigger::AddIndexes)
          }));
          let abort_result = {
            let mut inner = self.inner.lock();
            if !matches!(&schedule_result, Ok(Ok(()))) || !inner.merges.are_enabled() {
              merge_source.abort_pending_merges(&mut inner)
            } else {
              Ok(())
            }
          };
          let run_result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
              IOUtils::use_or_suppress_caught_result(schedule_result, Ok(abort_result))?;
              MergeSpecification::<D, Arc<CR>>::await_(&merge_stats);
              Ok(())
            }));
          let merges = merge_source.take_processed_merges();
          if !matches!(&run_result, Ok(Ok(()))) {
            for merge in &merges {
              if let Some(merge_info) = merge.info.as_ref() {
                self.delete_new_files(merge_info.files()?.iter(), None)?;
              }
            }
          }
          unwrap_caught_result!(run_result)?;
          (merges, merge_stats)
        },
        None => {
          if self.info_stream.is_enabled("IW") {
            self.info_stream.message(
              "addIndexes(CodecReaders...)",
              "received None mergeSpecification from MergePolicy. No indexes to add, returning..",
            )?;
          }
          return Ok(self.doc_writer.get_next_sequence_number());
        },
        Some(_) => {
          if self.info_stream.is_enabled("IW") {
            self.info_stream.message(
              "addIndexes(CodecReaders...)",
              "received empty mergeSpecification from MergePolicy. No indexes to add, returning..",
            )?;
          }
          return Ok(self.doc_writer.get_next_sequence_number());
        },
      };

      let merge_success = merge_stats
        .iter()
        .all(|merge| merge.has_completed_successfully().unwrap_or(false));

      if !merge_success {
        for merge in &merges {
          if let Some(merge_info) = merge.info.as_ref() {
            self.delete_new_files(merge_info.files()?.iter(), None)?;
          }
        }
      }

      if merge_success {
        let mut infos = Vec::new();
        let mut total_docs = 0_i64;
        for merge in &mut merges {
          total_docs += i64::from(merge.total_max_doc);
          if let Some(merge_info) = merge.info.take() {
            infos.push(merge_info);
          }
        }

        let seq_no = {
          let mut inner = self.inner.lock();
          if !infos.is_empty() {
            let mut register_segment_success = false;
            let register_segment_result =
              std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
                self.ensure_open()?;
                self.reserve_docs(total_docs)?;
                register_segment_success = true;
                Ok(())
              }));
            if !register_segment_success {
              for sipc in &infos {
                self.delete_new_files(sipc.files()?.iter(), Some(&inner))?;
              }
            }
            unwrap_caught_result!(register_segment_result)?;
            inner.segment_infos.add_all(infos)?;
            self.checkpoint(&mut inner)?;
          }
          self.doc_writer.get_next_sequence_number()
        };
        Ok(seq_no)
      } else {
        if self.info_stream.is_enabled("IW") {
          self.info_stream.message(
            "addIndexes(CodecReaders...)",
            "failed to successfully merge all provided readers.",
          )?;
        }
        for merge in &merges {
          if merge.is_aborted() {
            return Err(LuceneError::merge_abort("merge was aborted."));
          }
          if let Some(result) = merge.get_exception() {
            return IOUtils::rethrow_always(result);
          }
        }
        Err(LuceneError::illegal_state(
          "failed to successfully merge all provided readers in addIndexes(CodecReader...)",
        ))
      }
    }));
    if res.is_err() {
      self.tragic_event(&res, "addIndexes(CodecReader...)", None)?;
    }
    let seq_no = unwrap_caught_result!(res)?;
    self.maybe_merge()?;
    Ok(seq_no)
  }

  /// Runs a single merge operation for [`IndexWriter::add_indexes(CodecReader...)`].
  ///
  /// Merges and creates a `SegmentInfo`, for the readers grouped together in provided `OneMerge`.
  ///
  /// # Arguments
  ///
  /// * `merge` - OneMerge object initialized from readers.
  ///
  /// # Errors
  ///
  /// Returns an error if there is a low-level IO error.
  fn add_indexes_reader_merge<CR>(&self, merge: &mut OneMerge<D, CR>) -> Result<()>
  where
    CR: CodecReader + Clone,
    D: 'static,
  {
    merge.merge_init();
    merge.check_aborted()?;

    let mut num_docs = 0_i64;
    if self.info_stream.is_enabled("IW") {
      self
        .info_stream
        .message("IW", "flush at addIndexes(CodecReader...)")?;
    }
    self.flush_with_apply_merge_deletes(false, true)?;

    let merged_name = self.new_segment_name(None);
    let merge_directory = self
      .config
      .get_merge_scheduler()
      .wrap_for_merge(self.directory.clone())?;

    let mut num_soft_deleted = 0;
    let mut has_blocks = false;
    {
      let merge_reader = merge.get_merge_reader();
      for reader in merge_reader.iter() {
        let leaf = &reader.reader;
        num_docs += i64::from(leaf.num_docs()?);
        let v = leaf.get_context()?;
        let contexts = v.leaves()?;
        for context in contexts {
          has_blocks |= context.reader().get_metadata()?.has_blocks
        }

        if self.soft_deletes_enabled {
          let field = self.config.get_soft_deletes_field().ok_or_else(|| {
            LuceneError::illegal_state(
              "soft_deletes_enabled is true but soft_deletes_field is not configured",
            )
          })?;

          let mut soft_deleted_docs = get_doc_values_doc_id_set_iterator(field, leaf)?;

          num_soft_deleted +=
            count_soft_deletes(soft_deleted_docs.as_mut(), reader.hard_live_docs.as_ref())?;
        }
      }
    }
    // Best-effort up front check:
    self.test_reserve_docs(num_docs)?;

    let context = IOContext::with_merge(MergeInfo::new(
      num_docs.try_convert()?,
      -1,
      false,
      UNBOUNDED_MAX_MERGE_SEGMENTS,
    ))?;

    let mut tracking_dir = TrackingDirectoryWrapper::new(merge_directory);
    let mut seg_info = SegmentInfo::new(
      self.directory_orig.clone(),
      Some((*LATEST).clone()),
      None,
      &merged_name,
      -1,
      false,
      has_blocks,
      Some(self.config.get_codec().clone()),
      HashMap::new(),
      StringHelper::random_id(),
      HashMap::new(),
      self.config.get_index_sort(),
    )?;
    let readers = {
      let merge_reader = merge.get_merge_reader();
      let mut readers = Vec::with_capacity(merge_reader.len());
      for mr in merge_reader.iter() {
        let wrapped_reader = merge.wrap_for_merge(mr.reader.clone())?;
        readers.push(wrapped_reader);
      }
      readers
    };
    // Don't reorder if an explicit sort is configured.
    let has_index_sort = self.config.get_index_sort().is_some();
    // Don't reorder if blocks can't be identified using the parent field.
    let mut has_blocks_but_no_parent_field = false;
    for reader in &readers {
      if reader.get_metadata()?.get_has_blocks()
        && reader.get_field_infos()?.get_parent_field().is_none()
      {
        has_blocks_but_no_parent_field = true;
        break;
      }
    }
    let new_merge_readers;
    if !has_index_sort && !has_blocks_but_no_parent_field && !readers.is_empty() {
      let merged_reader = wrap(readers.clone())?;
      let doc_map_opt = merge.reorder(&merged_reader, self.directory.as_ref())?;
      if let Some(doc_map) = doc_map_opt {
        new_merge_readers = vec![ReorderedMergeCodecReader::B(wrap_with_doc_map(
          merged_reader,
          doc_map,
          None,
        )?)];
      } else {
        let mut v = Vec::with_capacity(readers.len());
        for reader in readers {
          v.push(ReorderedMergeCodecReader::A(reader));
        }
        new_merge_readers = v;
      }
    } else {
      let mut v = Vec::with_capacity(readers.len());
      for reader in readers {
        v.push(ReorderedMergeCodecReader::A(reader));
      }
      new_merge_readers = v;
    }

    let mut merger = SegmentMerger::new(
      &new_merge_readers,
      &mut seg_info,
      self.info_stream.clone(),
      &tracking_dir,
      self.global_field_number_map.clone(),
      &context,
    )?;

    if !merger.should_merge()? {
      return Ok(());
    }
    merge.check_aborted()?;
    {
      let mut inner = self.inner.lock();
      inner.running_add_indexes_merges.insert(merger.id.clone());
    }
    merge.set_merge_start_time(Instant::now());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      merger.merge()?;
      Ok(())
    }));
    {
      let mut inner = self.inner.lock();
      inner.running_add_indexes_merges.remove(&merger.id);
      self.pausing.notify_all();
    }
    unwrap_caught_result!(result)?;

    let mut sci = SegmentCommitInfo::new(
      seg_info,
      0,
      num_soft_deleted,
      -1,
      -1,
      -1,
      Some(StringHelper::random_id()),
    );
    Arc::get_mut(&mut sci.info)
      .ok_or_else(|| LuceneError::illegal_state("Arc not unique"))?
      .set_files(tracking_dir.take_created_files())?;
    set_diagnostics_impl(
      Arc::get_mut(&mut sci.info).ok_or_else(|| LuceneError::illegal_state("Arc not unique"))?,
      SOURCE_ADDINDEXES_READERS,
      None,
    );

    let use_compound_file = {
      let inner = self.inner.lock();
      merge.check_aborted()?;
      self
        .config
        .get_merge_policy()
        .use_compound_file(&inner.segment_infos, &sci, self)?
    };

    if use_compound_file {
      let files_to_delete = sci.files()?;
      let info =
        Arc::get_mut(&mut sci.info).ok_or_else(|| LuceneError::illegal_state("Arc not unique"))?;
      let tracking_cfs_dir = TrackingDirectoryWrapper::new(self.directory.as_ref());
      create_compound_file(
        &self.info_stream,
        &tracking_cfs_dir,
        info,
        &context,
        IOConsumerImpl1::new(self),
      )?;
      self.delete_new_files(files_to_delete.iter(), None)?;
      info.set_use_compound_file(true);
    }

    merge.set_merge_info(sci);
    let info = merge
      .get_merge_info_mut()
      .ok_or_else(|| LuceneError::illegal_state("merge info is none"))?;
    let info =
      Arc::get_mut(&mut info.info).ok_or_else(|| LuceneError::illegal_state("Arc not unique"))?;
    self
      .config
      .get_codec()
      .segment_info_format()
      .write(&tracking_dir, info, &context)?;
    info.add_files(tracking_dir.take_created_files())?;

    Ok(())
  }
  /// Copies the segment files as-is into the IndexWriter's directory.
  fn copy_segment_as_is<SD>(
    &self,
    info: &SegmentCommitInfo<SD>,
    seg_name: &str,
    context: &IOContext,
  ) -> Result<SegmentCommitInfo<D>>
  where
    SD: Directory,
  {
    let mut new_info = SegmentInfo::new(
      self.directory_orig.clone(),
      info.info.get_version_ref().cloned(),
      info.info.get_min_version(),
      seg_name,
      info.info.max_doc()?,
      info.info.get_use_compound_file(),
      info.info.get_has_blocks(),
      Some(info.info.get_codec()?.clone()),
      info.info.get_diagnostics().clone(),
      *info.info.get_id(),
      info.info.get_attributes()?.clone(),
      info.info.get_index_sort(),
    )?;

    new_info.set_files(info.info.files()?.clone())?;

    let mut new_info_per_commit = SegmentCommitInfo::new(
      new_info,
      info.get_del_count(),
      info.get_soft_del_count(),
      info.get_del_gen(),
      info.get_field_infos_gen(),
      info.get_doc_values_gen(),
      info.get_id().copied(),
    );
    new_info_per_commit.set_field_infos_files(info.get_field_infos_files().clone());
    new_info_per_commit.set_doc_values_updates_files(info.get_doc_values_updates_files().clone());
    #[cfg(debug_assertions)]
    {
      let mut copied_files = HashSet::new();
      let mut success = false;
      let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        for file in info.files()? {
          let new_filename = named_for_this_segment(seg_name, file.clone());
          self
            .directory
            .copy_from(info.info.dir.as_ref(), &file, &new_filename, context)?;
          copied_files.insert(new_filename);
        }
        success = true;
        Ok(())
      }));
      if !success {
        self.delete_new_files(copied_files.iter(), None)?;
      }
      unwrap_caught_result!(result)?;
      debug_assert_eq!(
        copied_files,
        new_info_per_commit.files()?,
        "copiedFiles={copied_files:?} vs newInfoPerCommit.files()={:?}",
        new_info_per_commit.files()?
      );
    }

    Ok(new_info_per_commit)
  }

  /// Expert: Flushes the next pending writer per thread buffer if available or the largest active
  /// non-pending writer per thread buffer in the calling thread. This can be used to flush documents
  /// to disk outside of an indexing thread. In contrast to [`Self::flush`] this won't mark all
  /// currently active indexing buffers as flush-pending.
  ///
  /// Note: this method is best-effort and might not flush any segments to disk. If there is a
  /// full flush happening concurrently multiple segments might have been flushed. Users of this API
  /// can access the [`IndexWriter`]'s current memory consumption via `Self::ram_bytes_used`.
  ///
  /// Returns `true` iff this method flushed at least one segment to disk.
  pub fn flush_next_buffer(&self) -> Result<bool>
  where
    D: 'static,
  {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<bool> {
      let body_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<bool> {
          if self.doc_writer.flush_one_dwpt(self)? {
            self.process_events(true)?;
            Ok(true)
          } else {
            Ok(false)
          }
        }));
      if body_result.is_err() {
        self.tragic_event(&body_result, "flushNextBuffer", None)?;
      }
      unwrap_caught_result!(body_result)
    }));

    self.maybe_close_on_tragic_event(None)?;
    unwrap_caught_result!(result)
  }

  fn prepare_commit_internal(&self, commit_lock: Option<&CommitInner<D>>) -> Result<i64>
  where
    D: 'static,
  {
    let commit_guard = if commit_lock.is_none() {
      Some(self.commit_lock.lock())
    } else {
      None
    };
    let commit_lock = match commit_lock {
      Some(lock) => lock,
      None => commit_guard.as_deref().unwrap(),
    };
    commit_lock.start_commit_time.set(Instant::now());

    self.do_ensure_open(false)?;
    if self.info_stream.is_enabled("IW") {
      self.info_stream.message("IW", "prepareCommit: flush")?;
      self.info_stream.message(
        "IW",
        &format!("  index before flush {}", self.seg_string(None)?),
      )?;
    }

    if let Some(t) = self.tragedy.get() {
      let mut error =
        LuceneError::illegal_state("this writer hit an unrecoverable error; cannot commit");
      error.add_suppressed(t.clone());
      return Err(error);
    }

    if commit_lock.pending_commit.borrow().is_some() {
      return Err(LuceneError::illegal_state(
        "prepareCommit was already called with no corresponding call to commit",
      ));
    }

    if let Some(ref s) = self.hooks {
      s.do_before_flush()?
    }
    self.test_point("startDoFlush")?;

    // locals (to be filled by the next parts)
    let mut to_commit: Option<Arc<Mutex<SegmentInfos<D>>>> = None;
    let mut any_changes = false;
    let mut seq_no: i64 = 0;
    let mut point_in_time_merges: Option<UpdatePendingMergesResult<D>> = None;
    let stop_adding_merged_segments = Arc::new(AtomicBool::new(false));
    let max_commit_merge_wait_millis = self.config.get_max_full_flush_merge_wait_millis();
    // This is copied from doFlush, except it's modified to
    // clone & incRef the flushed SegmentInfos inside the
    // sync block:
    let tragic_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      let flush_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
          let _guard = self.full_flush_lock.lock();
          let mut flush_success = false;
          let mut success = false;
          let body_res =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
              seq_no = self.doc_writer.flush_all_threads(self, &self.config)?;
              if seq_no < 0 {
                any_changes = true;
                seq_no = -seq_no;
              }
              if !any_changes {
                // Prevent a double increment because `doc_writer::do_flush` increments the flush count.
                // if we flushed anything.
                self.flush_count.fetch_add(1, Ordering::SeqCst);
              }

              self.publish_flushed_segments(true)?;
              // cannot pass triggerMerges=true here else it can lead to deadlock:
              self.process_events(false)?;

              flush_success = true;

              self.apply_all_deletes_and_updates()?;

              {
                let mut inner = self.inner.lock();
                self.write_reader_pool(true, &mut *inner)?;
                if inner.change_count != self.last_commit_change_count.load(Ordering::SeqCst) {
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
                let commit_infos = Arc::new(Mutex::new(inner.segment_infos.try_clone()?));
                to_commit = Some(commit_infos.clone());
                self
                  .pending_commit_change_count
                  .store(inner.change_count, Ordering::SeqCst);
                // This protects the segmentInfos we are now going
                // to commit.  This is important in case, eg, while
                // we are trying to sync all referenced files, a
                // merge completes which would otherwise have
                // removed the files we are now syncing.
                inner
                  .deleter
                  .inc_ref_files(commit_infos.lock().files(false)?)?;

                if max_commit_merge_wait_millis > 0 {
                  // We can safely call preparePointInTimeMerge since writeReaderPool(true) above wrote
                  // all necessary files to disk and checkpointed them.
                  point_in_time_merges = self.prepare_point_in_time_merge(
                    commit_infos,
                    stop_adding_merged_segments.clone(),
                    MergeTrigger::Commit,
                    PointInTimeMergeFinished::NoOp,
                    &mut inner,
                  )?;
                }
              }
              success = true;
              Ok(())
            }));
          if !success && self.info_stream.is_enabled("IW") {
            self
              .info_stream
              .message("IW", "hit exception during prepareCommit")?;
          }
          // Done: finish the full flush!
          self
            .doc_writer
            .finish_full_flush(flush_success, &self.config)?;
          if let Some(ref s) = self.hooks {
            s.do_after_flush()?
          }
          unwrap_caught_result!(body_res)
        }));

      if flush_result.is_err() {
        self.tragic_event(&flush_result, "prepareCommit", Some(commit_lock))?;
      }
      unwrap_caught_result!(flush_result)
    }));
    self.maybe_close_on_tragic_event(Some(commit_lock))?;
    unwrap_caught_result!(tragic_res)?;

    if let Some(ref point_in_time_merges) = point_in_time_merges {
      let event_listener = self.config.get_index_writer_event_listener();
      let merge_states = point_in_time_merges.merge_states();
      event_listener.begin_merge_on_full_flush(&merge_states);

      {
        let merge_source = self.new_merge_source()?;
        self
          .config
          .get_merge_scheduler()
          .merge(merge_source, MergeTrigger::Commit)?;
      }
      point_in_time_merges.await_merges(Duration::from_millis(max_commit_merge_wait_millis as u64));

      if self.info_stream.is_enabled("IW") {
        self
          .info_stream
          .message("IW", "done waiting for merges during commit")?;
      }
      event_listener.end_merge_on_full_flush(&merge_states);

      {
        // we need to do this under lock since merge_finished above is also called under the IW lock
        let _inner = self.inner.lock();
        stop_adding_merged_segments.store(true, Ordering::SeqCst);
      }
    }

    // do this after handling any pointInTimeMerges since the files will have changed if any
    // merges
    // did complete
    let to_commit = to_commit.ok_or_else(|| LuceneError::illegal_state("toCommit is none"))?;
    *commit_lock.files_to_commit.borrow_mut() =
      Some(to_commit.lock().files(false)?.into_iter().collect());
    let mut ret = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<i64> {
      if any_changes {
        self.maybe_merge.store(true, Ordering::SeqCst);
      }
      let to_commit_for_commit = to_commit.lock().try_clone()?;
      self.start_commit(Some(to_commit_for_commit), commit_lock)?;
      if commit_lock.pending_commit.borrow().is_none() {
        Ok(-1)
      } else {
        Ok(seq_no)
      }
    }));
    if !matches!(&ret, Ok(Ok(_))) {
      let dec_ref_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut inner = self.inner.lock();
        let files_to_commit = commit_lock.files_to_commit.borrow_mut().take();
        match files_to_commit {
          Some(files_to_commit) => inner.deleter.dec_ref(files_to_commit.iter()),
          None => Ok(()),
        }
      }));
      ret.add_suppressed(
        dec_ref_result,
        "panic while releasing files after prepare commit failure",
      );
    }
    unwrap_caught_result!(ret)
  }

  fn prepare_point_in_time_merge(
    &self,
    merging_segment_infos: Arc<Mutex<SegmentInfos<D>>>,
    stop_collecting_merge_results: Arc<AtomicBool>,
    trigger: MergeTrigger,
    merge_finished: PointInTimeMergeFinished<D>,
    inner: &mut Inner<D>,
  ) -> Result<Option<UpdatePendingMergesResult<D>>>
  where
    D: 'static,
  {
    debug_assert!(
      matches!(trigger, MergeTrigger::GetReader | MergeTrigger::Commit),
      "illegal trigger: {:?}",
      trigger
    );

    let mut point_in_time_merges = self.update_pending_merges(
      &OneMergeWrappingMergePolicy::new(
        self.config.get_merge_policy().clone(),
        PointInTimeOneMerge::<D, DefaultLeafReader<D>>::new(
          merging_segment_infos,
          stop_collecting_merge_results,
          trigger,
          merge_finished,
        ),
      ),
      trigger,
      UNBOUNDED_MAX_MERGE_SEGMENTS,
      Some(inner),
    )?;

    if let Some(ref mut point_in_time_merges) = point_in_time_merges {
      let mut close_readers = true;
      let init_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        point_in_time_merges.for_each_merge_mut(inner, |merge, segment_infos, deleter| {
          let context = IOContext::with_merge(merge.get_store_merge_info())?;
          merge.init_merge_readers(|sci_id: &String| -> Result<MergeReaderSR<D>> {
            let sci = segment_infos.index_of(sci_id).ok_or_else(|| {
              LuceneError::illegal_state(format!("segment info with id={} not found", sci_id))
            })?;
            let rld = self.get_pooled_instance(sci, true)?.ok_or_else(|| {
              LuceneError::illegal_state("failed to get pooled instance for merge")
            })?;
            // Calling setIsMerging is important since it causes the ReadersAndUpdates to record
            // all doc values updates in a separate map in order to be applied to the merged
            // segment after it's done.
            rld.set_is_merging();
            rld.get_reader_for_merge(&context, sci, |reader| {
              deleter.inc_ref_files(reader.reader.get_segment_info().files()?)
            })
          })
        })?;

        close_readers = false;
        Ok(())
      }));

      if close_readers {
        point_in_time_merges.for_each_merge_mut_removing_pending(inner, |merge, inner| {
          let abort_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.abort_one_merge(merge, inner)
          }));
          self.merge_finish(&merge.stat, Some(inner));
          unwrap_caught_result!(abort_result)
        })?;
      }
      unwrap_caught_result!(init_result)?;
    }

    Ok(point_in_time_merges)
  }

  /// Ensures that all changes in the reader pool are written to disk.
  ///
  /// # Arguments
  ///
  /// * `write_deletes` — if `true`, deletes should also be written to disk.
  pub(crate) fn write_reader_pool(&self, write_deletes: bool, inner: &mut Inner<D>) -> Result<()> {
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
        &mut inner.segment_infos,
        &self.global_field_number_map.lock(),
      )? {
        self.checkpoint(inner)?;
      }
    }
    // now do some best effort to check if a segment is fully deleted
    let mut to_drop = Vec::new();

    for info in inner.segment_infos.iter() {
      if let Some(rld) = self.reader_pool.get(info, false, None)?
        && self.is_fully_deleted(rld.as_ref(), info, inner)?
      {
        to_drop.push(info.info.get_id_key().to_string());
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
  /// Note that you must call this method **before** `prepare_commit`.
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
  /// Returns the commit user data previously set with
  /// [`Self::set_live_commit_data`], or `None` if nothing has been set yet.
  pub fn get_live_commit_data(&self) -> Option<HashMap<String, String>> {
    let inner = self.inner.lock();
    inner.commit_user_data.clone()
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
          if self.info_stream.is_enabled("BD") {
            self.info_stream.message(
              "BD",
              &format!(
                "now write some pending DV updates: {:.2} MB used vs IWC Buffer {:.2} MB",
                ram_bytes_used as f64 / 1024.0 / 1024.0,
                ram_buffer_size_mb
              ),
            )?;
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
              // A reader returned by `reader_pool::get_readers_by_ram`
              // is dropped before being processed here. If it happens, we need to skip that
              // reader.
              // this is also best effort to free ram, there might be some other thread writing
              // this rld concurrently
              // which wins and then if readerPooling is off this rld will be dropped.
              let mut inner = self.inner.lock();
              let Some(info) = inner.segment_infos.index_of_mut(&rld.info_id) else {
                continue;
              };
              if self.reader_pool.get(info, false, None)?.is_none() {
                continue;
              }

              if rld.write_field_updates(
                &self.directory,
                &self.global_field_number_map.lock(),
                self.buffered_updates_stream.get_completed_del_gen(),
                self.info_stream.as_ref(),
                Some(info),
              )? {
                self.check_point_no_sis(&mut inner)?;
              }
            }

            let bytes_used_after = rld.ram_bytes_used.load(Ordering::SeqCst);
            ram_bytes_used -= bytes_used_before - bytes_used_after;
            count += 1;
          }

          if self.info_stream.is_enabled("BD") {
            self.info_stream.message(
                            "BD",
                            &format!(
                                "done write some DV updates for {} segments: now {:.2} MB used vs IWC Buffer {:.2} MB; took {:.2} sec",
                                count,
                                self.reader_pool.ram_bytes_used() as f64 / 1024.0 / 1024.0,
                                ram_buffer_size_mb,
                                start_ns.elapsed().as_secs_f64(),
                            ),
                        )?;
          }
        }
      }
      drop(_guard)
    }
    Ok(())
  }
  /// Expert: obtains the number of deleted docs in the given segment, buffering deletes
  /// for the segment if it hasn't been loaded yet.
  pub fn num_deleted_docs(&self, info: &SegmentCommitInfo<D>) -> Result<i32> {
    self.do_ensure_open(false)?;
    self.validate(info)?;
    if let Some(rld) = self.get_pooled_instance(info, false)? {
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
  /// Used internally to return an [`AlreadyClosedError`] if this `IndexWriter` has been closed
  /// or is in the process of closing.
  ///
  /// # Parameters
  /// * `fail_if_closing` - if true, also fail when `IndexWriter` is in the process of closing
  ///   (`closing=true`) but not yet done closing (`closed=false`).
  ///
  /// # Errors
  /// Returns an [`AlreadyClosedError`] if this `IndexWriter` is closed or in the process of
  /// closing.
  pub(crate) fn do_ensure_open(&self, fail_if_closing: bool) -> Result<()> {
    if self.closed.load(Ordering::SeqCst)
      || (fail_if_closing && self.closing.load(Ordering::SeqCst))
    {
      let error_opt = self.tragedy.get();
      let message = "this IndexWriter is closed";
      match error_opt {
        Some(err) => {
          let mut error = LuceneError::already_closed(message);
          error.add_suppressed(err.clone());
          Err(error)
        },
        None => Err(LuceneError::already_closed(message)),
      }
    } else {
      Ok(())
    }
  }
  pub(crate) fn ensure_open(&self) -> Result<()> {
    self.do_ensure_open(true)
  }

  /// Returns true if there may be changes that have not been committed. There
  /// are cases where this may return true when there are no actual "real"
  /// changes to the index, for example if you've deleted by Term or Query but
  /// that Term or Query does not match any documents. Also, if a merge kicked
  /// off as a result of flushing a new segment during [`commit`](Self::commit),
  /// or a concurrent merged finished, this method may return true right after
  /// you had just called [`commit`](Self::commit).
  pub fn has_uncommitted_changes(&self) -> Result<bool> {
    let change_count = self.inner.lock().change_count;
    Ok(
      change_count != self.last_commit_change_count.load(Ordering::SeqCst)
        || self.has_changes_in_ram()?,
    )
  }

  /// Returns true if there are any changes or deletes that are not flushed or
  /// applied.
  pub(crate) fn has_changes_in_ram(&self) -> Result<bool> {
    Ok(self.doc_writer.any_changes()? || self.buffered_updates_stream.any())
  }

  pub(crate) fn commit_internal<P>(&self, merge_policy: &P) -> Result<i64>
  where
    P: MergePolicy<D> + ?Sized,
    D: 'static,
  {
    if self.info_stream.is_enabled("IW") {
      self.info_stream.message("IW", "commit: start")?;
    }

    let seq_no: i64;

    {
      let commit_lock = self.commit_lock.lock();
      self.do_ensure_open(false)?;

      if self.info_stream.is_enabled("IW") {
        self.info_stream.message("IW", "commit: enter lock")?;
      }

      if commit_lock.pending_commit.borrow().is_none() {
        if self.info_stream.is_enabled("IW") {
          self.info_stream.message("IW", "commit: now prepare")?;
        }
        seq_no = self.prepare_commit_internal(Some(&commit_lock))?;
      } else {
        if self.info_stream.is_enabled("IW") {
          self.info_stream.message("IW", "commit: already prepared")?;
        }
        seq_no = self.pending_seq_no.load(Ordering::SeqCst);
      }
      self.finish_commit(&commit_lock)?;
    }

    if self.maybe_merge.swap(false, Ordering::SeqCst) {
      self.maybe_merge_with_max_num_segments(
        merge_policy,
        MergeTrigger::FullFlush,
        UNBOUNDED_MAX_MERGE_SEGMENTS,
      )?;
    }

    Ok(seq_no)
  }

  pub(crate) fn finish_commit(&self, commit_lock: &CommitInner<D>) -> Result<()>
  where
    D: 'static,
  {
    let mut commit_completed = false;
    let try_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      let mut inner = self.inner.lock();
      self.do_ensure_open(false)?;

      if let Some(t) = self.tragedy.get() {
        let mut error = LuceneError::illegal_state(
          "this writer hit an unrecoverable error; cannot complete commit",
        );
        error.add_suppressed(t.clone());
        return Err(error);
      }

      let mut pending_commit = commit_lock.pending_commit.borrow_mut().take();
      match pending_commit.as_mut() {
        Some(pending) => {
          let body_res =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
              if self.info_stream.is_enabled("IW") {
                self
                  .info_stream
                  .message("IW", "commit: pendingCommit != null")?;
              }

              let committed_segments_file_name = pending.finish_commit(self.directory.as_ref())?;
              // we committed, if anything goes wrong after this, we are screwed and it's a tragedy:
              commit_completed = true;

              if self.info_stream.is_enabled("IW") {
                self.info_stream.message(
                  "IW",
                  &format!(
                    "commit: done writing segments file \"{}\"",
                    committed_segments_file_name
                  ),
                )?;
              }

              // NOTE: don't use this.checkpoint() here, because
              // we do not want to increment changeCount:
              inner
                .deleter
                .checkpoint(pending, true, self.config.get_index_deletion_policy())?;

              // Carry over generation to our master SegmentInfos:
              inner.segment_infos.update_generation(pending);

              self.last_commit_change_count.store(
                self.pending_commit_change_count.load(Ordering::SeqCst),
                Ordering::SeqCst,
              );

              inner.rollback_segments = pending.create_backup_segment_infos()?;

              Ok(())
            }));

          let files_to_commit = commit_lock.files_to_commit.borrow_mut().take();
          let close_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            files_to_commit
              .as_ref()
              .ok_or_else(|| LuceneError::illegal_state("no files"))
              .and_then(|files| inner.deleter.dec_ref(files.iter()))
          }));

          self.pausing.notify_all();

          IOUtils::use_or_suppress_caught_result(body_res, close_res)?;
        },
        None => {
          debug_assert!(commit_lock.files_to_commit.borrow().is_none());
          if self.info_stream.is_enabled("IW") {
            self
              .info_stream
              .message("IW", "commit: pendingCommit == null; skip")?;
          }
        },
      }

      Ok(())
    }));

    if !matches!(&try_res, Ok(Ok(()))) {
      if self.info_stream.is_enabled("IW")
        && let Some(failure) = try_res.caught_failure("panic while finishing commit")
      {
        self.info_stream.message(
          "IW",
          &format!("hit exception during finishCommit: {}", failure),
        )?;
      }
      if commit_completed {
        self.tragic_event(&try_res, "finishCommit", Some(commit_lock))?;
      }
    }
    unwrap_caught_result!(try_res)?;

    if self.info_stream.is_enabled("IW") {
      self.info_stream.message(
        "IW",
        &format!(
          "commit: took {:.1} msec",
          commit_lock.start_commit_time.get().elapsed().as_millis() as f64
        ),
      )?;
      self.info_stream.message("IW", "commit: done")?;
    }

    Ok(())
  }
  /// Moves all in-memory segments to the [`Directory`], but does not commit (fsync) them
  /// (call [`commit`](Self::commit) for that).
  pub fn flush(&self) -> Result<()>
  where
    D: 'static,
  {
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
  ) -> Result<()>
  where
    D: 'static,
  {
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
  fn do_flush(&self, apply_all_deletes: bool) -> Result<bool>
  where
    D: 'static,
  {
    if let Some(t) = self.tragedy.get() {
      let mut error =
        LuceneError::illegal_state("this writer hit an unrecoverable error; cannot flush");
      error.add_suppressed(t.clone());
      return Err(error);
    }

    if let Some(ref s) = self.hooks {
      s.do_before_flush()?;
    }

    self.test_point("startDoFlush")?;

    let mut success = false;
    let body_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<bool> {
      if self.info_stream.is_enabled("IW") {
        self.info_stream.message(
          "IW",
          &format!("  start flush: applyAllDeletes={}", apply_all_deletes),
        )?;
        self.info_stream.message(
          "IW",
          &format!("  index before flush {}", self.seg_string(None)?),
        )?;
      }

      let any_changes = {
        let _guard = self.full_flush_lock.lock();
        let mut flush_success = false;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<bool> {
          let any_changes = self.doc_writer.flush_all_threads(self, &self.config)? < 0;
          if !any_changes {
            // flushCount is incremented in flushAllThreads if true
            self.flush_count.fetch_add(1, Ordering::SeqCst);
          }
          self.publish_flushed_segments(true)?;
          flush_success = true;
          Ok(any_changes)
        }));
        self
          .doc_writer
          .finish_full_flush(flush_success, &self.config)?;
        self.process_events(false)?;
        unwrap_caught_result!(result)?
      };

      if apply_all_deletes {
        self.apply_all_deletes_and_updates()?;
      }

      let any_changes = any_changes | self.maybe_merge.swap(false, Ordering::SeqCst);

      {
        let mut inner = self.inner.lock();
        self.write_reader_pool(apply_all_deletes, &mut *inner)?;
        if let Some(ref s) = self.hooks {
          s.do_after_flush()?;
        }
        success = true;
      }

      Ok(any_changes)
    }));

    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<bool> {
      if body_result.is_err() {
        self.tragic_event(&body_result, "doFlush", None)?;
      }
      unwrap_caught_result!(body_result)
    }));

    if !success {
      if self.info_stream.is_enabled("IW") {
        self
          .info_stream
          .message("IW", "hit exception during flush")?;
      }
      self.maybe_close_on_tragic_event(None)?;
    }
    unwrap_caught_result!(res)
  }

  fn apply_all_deletes_and_updates(&self) -> Result<()>
  where
    D: 'static,
  {
    self.flush_deletes_count.fetch_add(1, Ordering::SeqCst);
    if self.info_stream.is_enabled("IW") {
      self.info_stream.message(
                "IW",
                &format!(
                    "now apply all deletes for all segments buffered updates bytesUsed={} reader pool bytesUsed={}",
                    self.buffered_updates_stream.ram_bytes_used()?,
                    self.reader_pool.ram_bytes_used()
                ),
            )?;
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
          self.seg_string(Some(inner))?
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
    self.merge_finished_gen.fetch_add(1, Ordering::SeqCst);

    self.test_point("startCommitMergeDeletes")?;

    let source_segments = merge.stat.segments.as_slice();
    // Carefully merge deletes that occurred after we
    // started merging:
    let mut min_gen: i64 = i64::MAX;

    let sci = merge.info.as_ref().unwrap();
    // Lazy init (only when we find a delete or update to carry over):
    let merged_deletes_and_updates = self.get_pooled_instance(sci, true)?.ok_or_else(|| {
      LuceneError::illegal_state("failed to get pooled instance for a merged segment")
    })?;

    let _ = merged_deletes_and_updates.get_del_count(sci);

    // field -> delGen -> dv field updates
    let mut mapped_dv_updates = HashMap::new();
    let mut any_dv_updates = false;
    debug_assert_eq!(source_segments.len(), doc_maps.len());
    for (i, info_id) in source_segments.iter().enumerate() {
      let info = inner.segment_infos.index_of(info_id).ok_or_else(|| {
        LuceneError::illegal_state(format!("segment info with id={} not found", info_id))
      })?;

      min_gen = std::cmp::min(info.get_buffered_deletes_gen(), min_gen);

      let max_doc = info.info.max_doc()?;

      let rld = self.get_pooled_instance(info, false)?.ok_or_else(|| {
        LuceneError::illegal_state(format!("seg={} not found in reader pool", info.info.name))
      })?;

      let seg_doc_map = &doc_maps[i];

      // carry over hard deletes
      let merge_reader = merge.get_merge_reader();
      Self::carry_over_hard_deletes(
        merged_deletes_and_updates.as_ref(),
        max_doc,
        merge_reader[i].hard_live_docs.as_ref(),
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
          if let std::collections::hash_map::Entry::Vacant(e) = mapped_field.entry(updates.del_gen)
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
                let sub_update2 = BinaryDocValuesFieldUpdates::new()?;
                DocValuesFieldUpdates::new(
                  merge.info.as_ref().unwrap().info.max_doc()?,
                  updates.del_gen,
                  updates.field.clone(),
                  sub_update2.sub_type(),
                  sub_update2,
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
        if current_hard_live_docs.identity() != prev_hard_live_docs.identity() {
          // This means this segment received new deletes
          // since we started the merge, so we
          // must merge them:
          for j in 0..max_doc as usize {
            if !(prev_hard_live_docs.get(j)?) {
              // if the document was deleted before, it better still be deleted!
              debug_assert!(!(current_hard_live_docs.get(j)?));
            } else if carry_over_delete(j)? {
              // the document was deleted while we were merging:
              merged_readers_and_updates.delete(seg_doc_map.get(j as i32)?, info, None)?;
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
    merge.on_merge_complete(&mut inner)?;
    self.test_point("startCommitMerge")?;

    if let Some(t) = self.tragedy.get() {
      let mut error =
        LuceneError::illegal_state("this writer hit an unrecoverable error; cannot complete merge");
      error.add_suppressed(t.clone());
      return Err(error);
    }

    debug_assert!(merge.stat.register_done.load(Ordering::Acquire));

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
        self
          .reader_pool
          .drop(info.info.get_id_key(), &mut inner.segment_infos)?;
        // Safe: these files must exist
        self.delete_new_files(info.files()?.iter(), Some(&inner))?;
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
    debug_assert!(!inner.segment_infos.contains(sci.info.get_id_key()));

    let all_deleted = merge.stat.segments.is_empty()
      || sci.info.max_doc()? == 0
      || (merged_updates.is_some()
        && self.is_fully_deleted(merged_updates.as_ref().unwrap().as_ref(), sci, &inner)?);

    if self.info_stream.is_enabled("IW")
      && all_deleted
      && let Some(ref info) = merge.info
    {
      self.info_stream.message(
        "IW",
        &format!("merged segment {} is 100% deleted; skipping insert", info),
      )?;
    }
    let drop_segment = all_deleted;

    // If we merged no segments then we better be dropping
    // the new segment:
    debug_assert!(!merge.stat.segments.is_empty() || drop_segment);
    debug_assert!(sci.info.max_doc()? != 0 || drop_segment);

    if let Some(merged_updates) = merged_updates {
      let mut success = false;
      let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        if drop_segment {
          merged_updates.drop_changes();
        }
        // Pass false for assert_live_info because the merged
        // segment is not yet live (only below do we commit it
        // to the segment_infos):
        self.release_with_assert(&merged_updates, false, &mut inner, merge.info.as_mut())?;
        success = true;
        Ok(())
      }));

      if !success {
        merged_updates.drop_changes();
        let info_id = merge.info.as_ref().unwrap().info.get_id_key();
        self.reader_pool.drop(info_id, &mut inner.segment_infos)?;
      }
      unwrap_caught_result!(res)?;
    }

    // Must do this after reader_pool.release, in case an
    // error is hit e.g. writing the live docs for the
    // merge segment, in which case we need to abort the
    // merge:
    let merge_id = merge.info.as_ref().unwrap().info.get_id_key().to_string();
    inner
      .segment_infos
      .apply_merge_changes(merge, drop_segment)?;

    // Now deduct the deleted docs that we just reclaimed from this
    // merge:
    let del_doc_count = if drop_segment {
      // if we drop the segment we have to reduce the pendingNumDocs by merge.totalMaxDocs since we
      // never drop
      // the docs when we apply deletes if the segment is currently merged.
      merge.total_max_doc
    } else {
      // The merge's SegmentCommitInfo has moved to `IndexWriter::inner::segment_infos`.
      let merge_sci = inner.segment_infos.index_of(&merge_id).ok_or_else(|| {
        LuceneError::illegal_state(
          "merge's SegmentCommitInfo not in IndexWriter#inner#segment_infos",
        )
      })?;
      merge.total_max_doc - merge_sci.info.max_doc()?
    };
    debug_assert!(del_doc_count >= 0);
    self.adjust_pending_num_docs(-(del_doc_count as i64));

    if drop_segment {
      let merge_sci = merge
        .info
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("merge info is none"))?;
      debug_assert!(!inner.segment_infos.contains(merge_sci.info.get_id_key()));
      let merge_info_id = merge_sci.info.get_id_key().to_string();
      self
        .reader_pool
        .drop(&merge_info_id, &mut inner.segment_infos)?;
      // Safe: these files must exist
      self.delete_new_files(merge_sci.files()?.iter(), Some(&inner))?;
    }

    {
      // Must close before checkpoint, otherwise IFD won't be
      // able to delete the held-open files from the merge
      // readers:
      let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        self.close_merge_readers(merge, false, drop_segment, Some(&mut inner))
      }));
      let checkpoint_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.checkpoint(&mut inner)));
      IOUtils::use_or_suppress_caught_result(close_result, checkpoint_result)?;
    }

    if self.info_stream.is_enabled("IW") {
      self.info_stream.message(
        "IW",
        &format!("after commitMerge: {}", self.seg_string(Some(&inner))?),
      )?;
    }

    if merge.stat.max_num_segments() != UNBOUNDED_MAX_MERGE_SEGMENTS && !drop_segment {
      // cascade the forceMerge:
      inner
        .segments_to_merge
        .entry(merge_id)
        .or_insert(Some(false));
    }

    Ok(true)
  }

  fn handle_merge_exception<CR>(
    &self,
    result: CaughtResult,
    merge: &OneMerge<D, CR>,
    inner: Option<&mut Inner<D>>,
  ) -> Result<()>
  where
    CR: CodecReader,
  {
    if matches!(&result, Ok(Ok(()))) {
      return Err(LuceneError::illegal_argument(
        "merge result must contain a failure",
      ));
    }
    if self.info_stream.is_enabled("IW") {
      self.info_stream.message(
        "IW",
        &format!(
          "handleMergeException: merge={} exc={}",
          Self::segment_ids_to_string(&merge.stat.segments),
          result.display()
        ),
      )?;
    }

    // Set the exception on the merge, so if
    // forceMerge is waiting on us it sees the root
    // cause exception:
    merge.set_exception(result);
    self.add_merge_exception(merge, inner);
    let result = merge
      .get_exception()
      .ok_or_else(|| LuceneError::illegal_state("merge exception must be present"))?;

    if matches!(&result, Ok(Err(LuceneError::MergeAborted(_)))) {
      // We can ignore this exception (it happens when
      // deleteAll or rollback is called), unless the
      // merge involves segments from external directories,
      // in which case we must throw it so, for example, the
      // rollbackTransaction code in addIndexes* is
      // executed.
      if merge.is_external {
        return unwrap_caught_result!(result);
      }
      Ok(())
    } else {
      IOUtils::rethrow_always(result)
    }
  }

  /// Merges the indicated segments, replacing them in the stack with a single segment.
  fn merge(&self, merge: &mut OneMergeSR<D>) -> Result<()>
  where
    D: 'static,
  {
    #[cfg(test)]
    if let Some(s) = &self.hooks {
      s.do_before_merge(&merge.stat)?;
    }
    let mut success = false;
    let merge_policy = self.config.get_merge_policy();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      let inner_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
          let merge_result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
              self.merge_init(merge)?;
              // The merge's SegmentCommitInfo has moved to `IndexWriter::inner::segment_infos`.
              self.merge_middle(merge, merge_policy)?;
              self.merge_success(merge)?;
              success = true;
              Ok(())
            }));
          match merge_result {
            Ok(Ok(())) => Ok(()),
            merge_result => self.handle_merge_exception(merge_result, merge, None),
          }
        }));

      let finally_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
          let mut inner = self.inner.lock();
          // Readers are already closed in commitMerge if we didn't hit
          // an exc:
          if !success {
            self.close_merge_readers(merge, true, false, Some(&mut inner))?;
          }
          self.merge_finish(&merge.stat, Some(&mut inner));

          if !success {
            if self.info_stream.is_enabled("IW") {
              self
                .info_stream
                .message("IW", "hit exception during merge")?;
            }
          } else if !merge.is_aborted()
            && (merge.stat.max_num_segments() != UNBOUNDED_MAX_MERGE_SEGMENTS
              || (!self.closed.load(SeqCst) && !self.closing.load(SeqCst)))
          {
            self.update_pending_merges(
              merge_policy,
              MergeTrigger::MergeFinished,
              merge.stat.max_num_segments(),
              Some(&mut inner),
            )?;
          }
          Ok(())
        }));
      IOUtils::finally_caught_result(inner_result, finally_result)
    }));
    if !matches!(&result, Ok(Ok(()))) {
      self.tragic_event(&result, "merge", None)?;
    }
    unwrap_caught_result!(result)
  }
  fn merge_success(&self, _merge: &OneMergeSR<D>) -> Result<()> {
    #[cfg(test)]
    if let Some(hooks) = &self.hooks {
      hooks.merge_success(&_merge.stat)?;
    }
    Ok(())
  }
  fn abort_one_merge(&self, merge: &mut OneMergeSR<D>, inner: &mut Inner<D>) -> Result<()> {
    merge.set_aborted()?;
    self.close_merge_readers(merge, true, false, Some(inner))
  }

  /// Checks whether this merge involves any segments already participating in a merge.
  /// If not, this merge is "registered", meaning we record that its segments are now participating in a merge,
  /// and its [`MergeStat`] is returned. Else (the merge conflicts) the original merge is returned.
  fn register_merge(
    &self,
    mut merge: OneMergeSR<D>,
    inner: &mut Inner<D>,
  ) -> Result<RegisterMergeResult<D>> {
    if self.register_merge_state(&mut merge, inner)? {
      let merge_stat = merge.stat.clone();
      inner.pending_merges.push_back(merge);
      Ok(RegisterMergeResult::Registered(merge_stat))
    } else {
      Ok(RegisterMergeResult::Rejected(Box::new(merge)))
    }
  }

  fn register_merge_state(&self, merge: &mut OneMergeSR<D>, inner: &mut Inner<D>) -> Result<bool> {
    if merge.stat.register_done.load(Ordering::Acquire) {
      return Ok(true);
    }
    debug_assert!(!merge.stat.segments.is_empty());

    if !inner.merges.are_enabled() {
      self.abort_one_merge(merge, inner)?;
      return Err(LuceneError::merge_abort(format!(
        "merge is aborted: {}",
        Self::segment_ids_to_string(&merge.stat.segments)
      )));
    }

    let mut is_external = false;

    for info_id in &merge.stat.segments {
      if inner.merging_segments.contains(info_id) {
        return Ok(false);
      }
      if !inner.segment_infos.contains(info_id) {
        return Ok(false);
      }

      let info = inner
        .segment_infos
        .index_of_live(info_id)
        .ok_or_else(|| LuceneError::illegal_state(format!("segment {info_id} is not live")))?;
      if !info.info.dir.is_same_identity(&self.directory_orig) {
        is_external = true;
      }

      if inner.segments_to_merge.contains_key(info_id) {
        merge
          .stat
          .set_max_num_segments(inner.merge_max_num_segments);
      }
    }
    self.ensure_valid_merge(merge, inner)?;

    merge.stat.merge_gen = inner.merge_gen;
    merge.is_external = is_external;

    // OK it does not conflict; now record that this merge
    // is running while the lock is held to avoid a race.
    // condition where two conflicting merges from different
    // threads, start
    if self.info_stream.is_enabled("IW") {
      let mut builder = String::from("registerMerge merging= [");
      for id in &inner.merging_segments {
        builder.push_str(id);
        builder.push_str(", ");
      }
      builder.push(']');
      self.info_stream.message("IW", &builder)?;
    }

    for info_id in &merge.stat.segments {
      inner.merging_segments.insert(info_id.clone());
    }

    debug_assert!(merge.estimated_merge_bytes.load(Ordering::SeqCst) == 0);
    debug_assert!(merge.total_merge_bytes.load(Ordering::SeqCst) == 0);

    let mut est_bytes: i64 = 0;
    let mut total_bytes: i64 = 0;

    for info_id in &merge.stat.segments {
      let info = inner
        .segment_infos
        .index_of(info_id)
        .ok_or_else(|| LuceneError::illegal_state("{} not in IndexWriter's segment_infos"))?;
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
      .store(est_bytes, Ordering::SeqCst);
    merge.total_merge_bytes.store(total_bytes, Ordering::SeqCst);
    // Merge is now registered
    merge.stat.register_done.store(true, Ordering::Release);
    Ok(true)
  }
  /// Performs fast initial merge setup while holding the `IndexWriter` lock.
  pub(crate) fn merge_init(&self, merge: &mut OneMergeSR<D>) -> Result<()>
  where
    D: 'static,
  {
    // Make sure any deletes that must be resolved before we commit the merge are complete:
    self
      .buffered_updates_stream
      .wait_apply_for_merge(&merge.stat.segments, self)?;

    let mut success = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      self.merge_init_(merge)?;
      success = true;
      Ok(())
    }));
    if !success {
      if self.info_stream.is_enabled("IW") {
        self
          .info_stream
          .message("IW", "hit exception in mergeInit")?;
      }
      self.merge_finish(&merge.stat, None);
    }
    unwrap_caught_result!(result)
  }

  fn merge_init_(&self, merge: &mut OneMergeSR<D>) -> Result<()> {
    let mut inner = self.inner.lock();
    self.test_point("startMergeInit")?;

    debug_assert!(merge.stat.register_done.load(Ordering::Acquire));
    debug_assert!(
      merge.stat.max_num_segments() == UNBOUNDED_MAX_MERGE_SEGMENTS
        || merge.stat.max_num_segments() > 0
    );

    if let Some(t) = self.tragedy.get() {
      let mut error =
        LuceneError::illegal_state("this writer hit an unrecoverable error; cannot merge");
      error.add_suppressed(t.clone());
      return Err(error);
    }

    if merge.info.is_some() {
      // mergeInit already done
      return Ok(());
    }

    merge.merge_init();

    if merge.is_aborted() {
      return Ok(());
    }

    if self.info_stream.is_enabled("IW") {
      self.info_stream.message(
        "IW",
        &format!(
          "now apply deletes for {} merging segments",
          merge.stat.segments.len()
        ),
      )?;
    }

    if self.reader_pool.write_doc_values_updates_for_merge(
      merge.stat.segments.as_ref(),
      &mut inner.segment_infos,
      &self.global_field_number_map.lock(),
    )? {
      self.checkpoint(&mut inner)?;
    }

    let mut has_blocks = false;
    for info_id in &merge.stat.segments {
      let info = inner
        .segment_infos
        .index_of(info_id)
        .ok_or_else(|| LuceneError::illegal_state("{} not in IndexWriter's segment_infos"))?;
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
      Some(self.config.get_codec().clone()),
      HashMap::new(),
      StringHelper::random_id(),
      HashMap::new(),
      self.config.get_index_sort(),
    )?;

    let mut details = HashMap::new();
    details.insert(
      "mergeMaxNumSegments".to_string(),
      merge.stat.max_num_segments().to_string(),
    );
    details.insert(
      "mergeFactor".to_string(),
      merge.stat.segments.len().to_string(),
    );

    set_diagnostics_impl(&mut si, SOURCE_MERGE, Some(details));

    let sci = SegmentCommitInfo::new(si, 0, 0, -1, -1, -1, Some(StringHelper::random_id()));
    merge.set_merge_info(sci);

    Ok(())
  }

  /// Performs fast merge finalization while holding the `IndexWriter` lock.
  fn merge_finish(&self, merge: &MergeStat, inner: Option<&mut Inner<D>>) {
    let inner = match inner {
      Some(i) => i,
      None => &mut *self.inner.lock(),
    };
    // forceMerge, addIndexes or waitForMerges may be waiting
    // on merges to finish.
    self.pausing.notify_all();

    // It's possible we are called twice, e.g. if there was an
    // error inside mergeInit
    if merge.register_done.load(Ordering::Acquire) {
      for seg_id in &merge.segments {
        inner.merging_segments.remove(seg_id);
      }
      merge.register_done.store(false, Ordering::Release);
    }

    inner.running_merges.remove(merge);
  }

  fn close_merge_readers(
    &self,
    merge: &mut OneMergeSR<D>,
    suppress_error: bool,
    dropper_segment: bool,
    inner: Option<&mut Inner<D>>,
  ) -> Result<()> {
    let inner = match inner {
      Some(inner) => inner,
      None => &mut *self.inner.lock(),
    };
    if !merge.has_finished() {
      let drop = !suppress_error;
      let uses_pooled_readers = merge.uses_pooled_readers;
      let c = |inner: &mut Inner<D>, mr: &MergeReaderSR<D>| {
        let sr = &mr.reader;
        if uses_pooled_readers {
          let original_si = inner
            .segment_infos
            .index_of(sr.get_original_segment_info_id())
            .ok_or_else(|| {
              LuceneError::illegal_state("merge reader's original segment info could not be found")
            })?;
          match self.get_pooled_instance(original_si, false)? {
            Some(rld) => {
              if drop {
                rld.drop_changes();
              } else {
                rld.drop_merging_updates(None);
              }
              rld.release(sr.as_ref(), None)?;
              self.release(rld.as_ref(), inner)?;
              if drop {
                self
                  .reader_pool
                  .drop(&rld.info_id, &mut inner.segment_infos)?;
              }
            },
            None => {
              return Err(LuceneError::illegal_state(
                "merging reader could not found in reader pool",
              ));
            },
          }
        }
        inner.deleter.dec_ref(&sr.get_segment_info().files()?)?;
        Ok(())
      };
      merge.close(inner, !suppress_error, dropper_segment, c)?;
    } else {
      debug_assert!(
        merge.get_merge_reader().is_empty(),
        "we are done but still have readers"
      );
      debug_assert!(
        suppress_error,
        "can't be done and not suppressing exceptions"
      )
    }
    Ok(())
  }

  // utility routines for tests
  pub(crate) fn newest_segment(&self) -> Option<SegmentCommitInfo<D>> {
    let inner = self.inner.lock();
    let size = inner.segment_infos.size();
    if size > 0 {
      inner.segment_infos.info(size - 1).cloned()
    } else {
      None
    }
  }

  /// Returns a string description of all segments, for debugging.
  pub(crate) fn seg_string(&self, inner: Option<&Inner<D>>) -> Result<String> {
    let inner = match inner {
      Some(inner) => inner,
      None => &mut *self.inner.lock(),
    };
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
    commit_lock: &CommitInner<D>,
  ) -> Result<()>
  where
    D: 'static,
  {
    debug_assert!(commit_lock.files_to_commit.borrow().is_some());
    // wrap with Option for easily take ownership
    debug_assert!(to_sync.is_some());
    self.test_point("startStartCommit")?;
    debug_assert!(commit_lock.pending_commit.borrow().is_none());
    if let Some(t) = self.tragedy.get() {
      let mut error =
        LuceneError::illegal_state("this writer hit an unrecoverable error; cannot commit");
      error.add_suppressed(t.clone());
      return Err(error);
    }
    // did to_sync's ownership move to pending_commit?
    // after pending_commit has to_sync's ownership, and error happens, we have to pass to to_sync_error
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      if self.info_stream.is_enabled("IW") {
        self.info_stream.message("IW", "startCommit(): start")?;
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
          if self.info_stream.is_enabled("IW") {
            self
              .info_stream
              .message("IW", "  skip startCommit(): no changes pending")?;
          }
          let files = commit_lock.files_to_commit.borrow_mut().take().unwrap();
          inner.deleter.dec_ref(files.iter())?;
          return Ok(());
        }

        debug_assert!(Self::files_exist(
          to_sync.as_ref().unwrap(),
          &inner.deleter
        )?);
      }

      self.test_point("midStartCommit")?;

      let mut pending_commit_set = false;

      let mut res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        self.test_point("midStartCommit2")?;

        {
          let inner = self.inner.lock();
          debug_assert!(commit_lock.pending_commit.borrow().is_none());
          debug_assert!(
            inner.segment_infos.get_generation() == to_sync.as_ref().unwrap().get_generation()
          );
          // Error here means nothing is prepared
          // (this method unwinds everything it did on
          // an error)

          to_sync
            .as_mut()
            .unwrap()
            .prepare_commit(self.directory.as_ref())?;
          if self.info_stream.is_enabled("IW") {
            let file_name = IndexFileNames::file_name_from_generation(
              IndexFileNames::PENDING_SEGMENTS,
              "",
              to_sync.as_ref().unwrap().get_generation(),
            );
            self.info_stream.message(
              "IW",
              &format!("startCommit: wrote pending segments file {:?}", file_name),
            )?;
          }

          pending_commit_set = true;
          *commit_lock.pending_commit.borrow_mut() = to_sync.take();
        }
        // This call can take a long time -- 10s of seconds
        // or more.  We do it without syncing on this:
        let mut files_to_sync_list = Vec::new();
        let mut success = false;
        let sync_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
          files_to_sync_list = {
            let pending_commit = commit_lock.pending_commit.borrow();
            pending_commit
              .as_ref()
              .unwrap()
              .files(false)?
              .into_iter()
              .collect()
          };
          self.directory.sync(&files_to_sync_list)?;
          success = true;
          Ok(())
        }));

        if !success {
          pending_commit_set = false;
          debug_assert!(commit_lock.pending_commit.borrow().is_some());
          let mut pending_commit = commit_lock.pending_commit.borrow_mut().take().unwrap();
          pending_commit.rollback_commit(self.directory.as_ref());
          to_sync = Some(pending_commit);
        }
        unwrap_caught_result!(sync_res)?;

        if self.info_stream.is_enabled("IW") {
          self
            .info_stream
            .message("IW", &format!("done all syncs: {:?}", files_to_sync_list))?;
        }

        self.test_point("midStartCommitSuccess")?;
        Ok(())
      }));

      if !matches!(&res, Ok(Ok(()))) && !pending_commit_set {
        let mut inner = self.inner.lock();
        let message_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
          if self.info_stream.is_enabled("IW") {
            self
              .info_stream
              .message("IW", "hit exception committing segments file")?;
          }
          Ok(())
        }));
        if matches!(&message_result, Ok(Ok(()))) {
          let files = commit_lock.files_to_commit.borrow_mut().take().unwrap();
          let dec_ref_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            inner.deleter.dec_ref(files.iter())
          }));
          res.add_suppressed(
            dec_ref_result,
            "panic while releasing files after start commit failure",
          );
        } else {
          res = message_result;
        }
      }

      {
        let mut inner = self.inner.lock();
        // Have our master segmentInfos record the
        // generations we just prepared.  We do this
        // on error or success so we don't
        // double-write a segments_N file.
        match pending_commit_set {
          true => {
            let pending_commit = commit_lock.pending_commit.borrow();
            inner
              .segment_infos
              .update_generation(pending_commit.as_ref().unwrap());
          },
          false => {
            inner
              .segment_infos
              .update_generation(to_sync.as_ref().unwrap());
          },
        }
      }
      unwrap_caught_result!(res)
    }));
    if result.is_err() {
      self.tragic_event(&result, "startCommit", Some(commit_lock))?;
    }
    unwrap_caught_result!(result)?;
    self.test_point("finishStartCommit")
  }

  /// This method should be called on a tragic event, such as when a downstream writer component hits
  /// an unrecoverable error. This method does not return the tragic event error.
  ///
  /// Note: This method will not close the writer, but it can be called from any location without
  /// respecting any lock order.
  fn on_tragic_event<T>(&self, result: &CaughtResult<T>, location: &str) -> Result<()> {
    let tragedy = result
      .caught_failure(&format!("panic inside {location}"))
      .ok_or_else(|| LuceneError::illegal_argument("tragic result must contain a failure"))?;
    // This is not supposed to be tragic: IW is supposed to catch this and
    // ignore, because it means we asked the merge to abort:
    debug_assert!(!matches!(&tragedy, LuceneError::MergeAborted(_)));

    if self.info_stream.is_enabled("IW") {
      self.info_stream.message(
        "IW",
        &format!("hit tragic {:?} inside {}", tragedy, location),
      )?;
    }
    let _ = self.tragedy.set(tragedy);
    Ok(())
  }

  /// This method set the tragic error unless it's already set and closes the writer if necessary.
  /// Note this method will not return the throwable passed to it.
  fn tragic_event<T>(
    &self,
    result: &CaughtResult<T>,
    location: &str,
    commit_lock: Option<&CommitInner<D>>,
  ) -> Result<()>
  where
    D: 'static,
  {
    let tragic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      self.on_tragic_event(result, location)
    }));
    self.maybe_close_on_tragic_event(commit_lock)?;
    unwrap_caught_result!(tragic_result)
  }

  fn maybe_close_on_tragic_event(&self, commit_lock: Option<&CommitInner<D>>) -> Result<()>
  where
    D: 'static,
  {
    if self.tragedy.get().is_some() && self.should_close(false) {
      self.rollback_internal(commit_lock)?;
    }

    Ok(())
  }

  /// Returns the shared tragedy state, which contains an unrecoverable error if one occurred.
  pub fn get_tragic_exception(&self) -> TragicException {
    self.tragedy.clone()
  }

  /// Returns `true` if this `IndexWriter` is still open.
  pub fn is_open(&self) -> bool {
    !self.closing.load(Ordering::SeqCst) && !self.closed.load(Ordering::SeqCst)
  }

  fn test_point(&self, message: &str) -> Result<()> {
    if self.enable_test_points {
      debug_assert!(self.info_stream.is_enabled("TP"));
      self.info_stream.message("TP", message)?;
    }
    Ok(())
  }
  pub(crate) fn nrt_is_current(&self, version: i64) -> Result<bool> {
    let inner = self.inner.lock();
    self.do_ensure_open(true)?;

    let is_current = version == inner.segment_infos.get_version()
      && !self.doc_writer.any_changes()?
      && !self.buffered_updates_stream.any()
      && !self.reader_pool.any_doc_values_changes();

    if self.info_stream.is_enabled("IW") && !is_current {
      self.info_stream.message(
        "IW",
        &format!(
          "nrtIsCurrent: infoVersion matches: {}; DW changes: {}; BD changes: {}",
          version == inner.segment_infos.get_version(),
          self.doc_writer.any_changes()?,
          self.buffered_updates_stream.any(),
        ),
      )?;
    }

    Ok(is_current)
  }

  pub(crate) fn is_closed(&self) -> bool {
    let _inner = self.inner.lock();
    self.closed.load(Ordering::SeqCst)
  }

  pub(crate) fn is_deleter_closed(&self) -> Result<bool> {
    let inner = self.inner.lock();
    inner.deleter.is_closed(self)
  }

  fn delete_new_files<'a, I>(&self, files: I, inner: Option<&Inner<D>>) -> Result<()>
  where
    I: IntoIterator<Item = &'a String>,
  {
    let inner = match inner {
      Some(i) => i,
      None => &*self.inner.lock(),
    };
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
            if self.info_stream.is_enabled("IW") {
              self.info_stream.message(
                "IW",
                &format!("flush: push buffered updates: {buffered_updates:?}"),
              )?;
            }
            self.publish_frozen_updates(buffered_updates, None)?;
          }
        },
        Some(seg) => {
          if self.info_stream.is_enabled("IW") {
            self.info_stream.message(
              "IW",
              &format!(
                "publishFlushedSegment seg-private updates={:?}",
                seg.segment_updates
              ),
            )?;
          }
          if seg.segment_updates.is_some() && self.info_stream.is_enabled("DW") {
            self.info_stream.message(
              "IW",
              &format!(
                "flush: push buffered seg private updates: {:?}",
                seg.segment_updates
              ),
            )?;
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
    if self.info_stream.is_enabled("IW") {
      self.info_stream.message(
        "IW",
        &format!(
          "incRefDeleter for NRT reader version={} segments={}",
          segment_infos.get_version(),
          self.seg_string_from_infos(segment_infos.segments.iter())?
        ),
      )?;
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
    if self.info_stream.is_enabled("IW") {
      self.info_stream.message(
        "IW",
        &format!(
          "decRefDeleter for NRT reader version={} segments={}",
          segment_infos.get_version(),
          self.seg_string_from_infos(segment_infos.segments.iter())?
        ),
      )?;
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
  fn maybe_process_events(&self, mut seq_no: i64) -> Result<i64>
  where
    D: 'static,
  {
    if seq_no < 0 {
      seq_no = -seq_no;
      self.process_events(true)?;
    }
    Ok(seq_no)
  }

  fn process_events(&self, trigger_merge: bool) -> Result<()>
  where
    D: 'static,
  {
    if self.tragedy.get().is_none() {
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

    if self.adjust_pending_num_docs(added_num_docs) > get_actual_max_docs() as i64 {
      // Reserve failed: put the docs back and return error
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

    if self.pending_num_docs.load(Ordering::SeqCst) + added_num_docs > get_actual_max_docs() as i64
    {
      return self.too_many_docs(added_num_docs);
    }
    Ok(())
  }
  fn too_many_docs(&self, added_num_docs: i64) -> Result<()> {
    debug_assert!(added_num_docs >= 0);
    Err(LuceneError::illegal_argument(format!(
      "number of documents in the index cannot exceed {} (current document count is {}; added numDocs is {})",
      get_actual_max_docs(),
      self.pending_num_docs.load(Ordering::SeqCst),
      added_num_docs
    )))
  }
  /// Returns the number of documents in the index including documents are being added (i.e.,
  /// reserved).
  pub fn get_pending_num_docs(&self) -> i64 {
    self.pending_num_docs.load(Ordering::SeqCst)
  }
  /// Returns the highest sequence number across all completed operations,
  /// or 0 if no operations have finished yet.
  /// Still in-flight operations (in other threads) are not counted until they finish.
  pub fn get_max_completed_sequence_number(&self) -> Result<i64> {
    self.ensure_open()?;
    Ok(self.doc_writer.get_max_completed_sequence_number())
  }
  fn adjust_pending_num_docs(&self, num_docs: i64) -> i64 {
    let count = self.pending_num_docs.fetch_add(num_docs, Ordering::SeqCst) + num_docs;
    debug_assert!(count >= 0, "pendingNumDocs is negative: {}", count);
    count
  }

  fn is_fully_deleted(
    &self,
    readers_and_updates: &ReadersAndUpdates<D>,
    info: &SegmentCommitInfo<D>,
    _inner: &Inner<D>, // Same to Java's Thread.holdsLock(this)
  ) -> Result<bool> {
    if readers_and_updates.is_fully_deleted(info)? {
      return Ok(
        !readers_and_updates.keep_fully_deleted_segment(self.config.get_merge_policy(), info)?,
      );
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
    if self.reader_pool.release(
      readers_and_updates,
      assert_live_info,
      &mut inner.segment_infos,
      merge_info,
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
  ) -> Result<Option<Arc<ReadersAndUpdates<D>>>> {
    self.get_pooled_instance_helper(info, create, None)
  }

  pub(crate) fn get_pooled_instance_with_sort_map(
    &self,
    info: &SegmentCommitInfo<D>,
    create: bool,
    sort_map: Arc<DocMapImpl>,
  ) -> Result<Option<Arc<ReadersAndUpdates<D>>>> {
    self.get_pooled_instance_helper(info, create, Some(sort_map))
  }

  fn get_pooled_instance_helper(
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
  pub(crate) fn try_apply(&self, updates: &FrozenBufferedUpdates) -> Result<bool>
  where
    D: 'static,
  {
    let _guard = updates.as_ref().try_lock();
    if _guard.is_some() {
      self.force_apply(updates)?;
      return Ok(true);
    }
    Ok(false)
  }
  /// Translates a frozen packet of delete term/query, or doc values updates, into their actual
  /// doc IDs in the index, and applies the change.
  /// This is a heavy operation and is done concurrently by incoming indexing threads.
  pub(crate) fn force_apply(&self, updates: &FrozenBufferedUpdates) -> Result<()>
  where
    D: 'static,
  {
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
      let merge_gen_start = self.merge_finished_gen.load(Ordering::SeqCst);

      let mut del_files: HashSet<String> = HashSet::new();
      let mut seg_states;

      {
        let mut inner = self.inner.lock();
        let v = self.get_infos_to_apply(updates, &inner)?;
        match &v {
          InfoFrom::None => break,
          InfoFrom::Updates => {
            let info_id = updates.private_segment.as_deref().unwrap();
            let info = inner.segment_infos.index_of_live(info_id).ok_or_else(|| {
              LuceneError::illegal_state(format!("{} not in IndexWriter's segment_infos", info_id))
            })?;
            del_files.extend(info.files()?);
          },
          InfoFrom::All => {
            for info in inner.segment_infos.iter() {
              del_files.extend(info.files()?);
            }
          },
        }
        let v = match v {
          InfoFrom::None => return Err(LuceneError::unreachable("")),
          InfoFrom::Updates => Some(updates.private_segment.clone().unwrap()),
          // all segments
          InfoFrom::All => None,
        };
        // Must open while holding IW lock so that e.g. segments are not merged
        // away, dropped from 100% deletions, etc., before we can open the readers
        seg_states =
          self.open_segment_states(v, &mut seen_segments, updates.del_gen(), &mut inner)?;

        if seg_states.is_empty() {
          if self.info_stream.is_enabled("BD") {
            self
              .info_stream
              .message("BD", "packet matches no segments")?;
          }
          break;
        }

        if self.info_stream.is_enabled("BD") {
          self.info_stream.message(
            "BD",
            &format!(
              "{}now apply del packet ({}) to {} segments, mergeGen {}",
              message_prefix,
              self,
              seg_states.len(),
              merge_gen_start
            ),
          )?;
        }

        total_segment_count += seg_states.len() as i32;
        // Important, else IFD may try to delete our files while we are still using them,
        // if e.g. a merge finishes on some of the segments we are resolving on:
        inner.deleter.inc_ref_files(&del_files)?;
      }

      let mut success = false;
      let mut del_count = 0;
      {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
          // don't hold IW monitor lock here so threads are free concurrently resolve
          // deletes/updates:
          del_count = updates.apply(&seg_states, &self.inner.lock().segment_infos)?;
          success = true;
          Ok(())
        }));
        let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
          let mut inner = self.inner.lock();
          self.finish_apply(&mut seg_states, success, del_files, &mut inner)
        }));
        IOUtils::use_or_suppress_caught_result(result, close_result)?;
      }
      // Since we just resolved some more deletes/updates, now is a good time to write them:
      self.write_some_doc_values_updates()?;
      // It's OK to add this here, even if the while loop retries, because delCount only includes
      // newly
      // deleted documents, on the segments we didn't already do in previous iterations:
      total_del_count += del_count;

      if self.info_stream.is_enabled("BD") {
        self.info_stream.message(
          "BD",
          &format!(
            "{}done inner apply del packet to {} segments; {} new deletes/updates; took {:.3} sec",
            message_prefix,
            seg_states.len(),
            del_count,
            iter_start.elapsed().as_secs_f64(),
          ),
        )?;
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
        let merge_gen_cur = self.merge_finished_gen.load(Ordering::SeqCst);

        if merge_gen_cur == merge_gen_start {
          // Must do this while still holding IW lock else a merge could finish and skip carrying
          // over our updates:

          // Record that this packet is finished:
          self.buffered_updates_stream.finished(updates)?;
          finished = true;
          // No merge finished while we were applying, so we are done!
          break;
        }
        drop(_inner)
      }

      if self.info_stream.is_enabled("BD") {
        self.info_stream.message(
          "BD",
          &format!(
            "{}concurrent merges finished; move to next iter",
            message_prefix
          ),
        )?;
      }
      // A merge completed while we were running.  In this case, that merge may have picked up
      // some of the updates we did, but not
      // necessarily all of them, so we cycle again, re-applying all our updates to the newly
      // merged segment.

      iter += 1;
    }
    if !finished {
      // Record that this packet is finished:
      self.buffered_updates_stream.finished(updates)?;
    }

    if self.info_stream.is_enabled("BD") {
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
      self.info_stream.message("BD", &message)?;
    }
    Ok(())
  }

  /// Returns the [`SegmentCommitInfo`]'s id that this packet is supposed to apply its deletes to,
  /// or `None` if the private segment was already merged away.
  fn get_infos_to_apply(
    &self,
    updates: &FrozenBufferedUpdates,
    inner: &Inner<D>,
  ) -> Result<InfoFrom> {
    if let Some(private_seg) = &updates.private_segment {
      if inner.segment_infos.contains(private_seg) {
        Ok(InfoFrom::Updates)
      } else {
        if self.info_stream.is_enabled("BD") {
          self.info_stream.message(
            "BD",
            "private segment already gone; skip processing updates",
          )?;
        }
        Ok(InfoFrom::None)
      }
    } else {
      Ok(InfoFrom::All)
    }
  }
  pub(crate) fn finish_apply(
    &self,
    seg_states: &mut [SegmentState<D>],
    success: bool,
    del_files: HashSet<String>,
    inner: &mut Inner<D>, // we hold lock
  ) -> Result<()> {
    let close_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      self.close_segment_states(seg_states, success, inner)
    }));
    inner.deleter.dec_ref(del_files.iter())?;
    let result = unwrap_caught_result!(close_res)?;

    if result.any_deletes() {
      self.maybe_merge.store(true, Ordering::SeqCst);
      self.checkpoint(inner)?;
    }

    if let Some(all) = result.all_deleted() {
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

    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      if success {
        let infos_by_id: HashMap<_, _> = inner
          .segment_infos
          .iter()
          .iter()
          .chain(inner.segment_infos.dropped_segment_commit_infos.values())
          .map(|info| (info.info.get_id_key(), info))
          .collect();
        for seg_state in seg_states.iter_mut() {
          let info_id = &seg_state.rld.info_id;
          let info = match infos_by_id.get(info_id.as_str()).copied() {
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

          if seg_state.rld.is_fully_deleted(info)?
            && !self
              .get_config()
              .get_merge_policy()
              .keep_fully_deleted_segment(|| Ok(seg_state.reader.clone()))?
          {
            all_deleted.push(seg_state.reader.get_original_segment_info_id().to_string());
          }
        }
      }
      Ok(())
    }));

    IOUtils::close(seg_states.iter_mut(), |state| state.close(self, inner))?;
    unwrap_caught_result!(res)?;

    if self.info_stream.is_enabled("BD") {
      self.info_stream.message(
        "BD",
        &format!(
          "closeSegmentStates: {} new deleted documents; pool {} packets; bytesUsed={}",
          tot_del_count,
          self.buffered_updates_stream.get_pending_updates_count(),
          self.reader_pool.ram_bytes_used()
        ),
      )?;
    }

    let result = ApplyDeletesResult {
      any_deletes: tot_del_count > 0,
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
    info_from: Option<String>,
    already_seen: &mut HashSet<String>,
    del_gen: i64,
    inner: &mut Inner<D>, // we hold lock
  ) -> Result<Vec<SegmentState<D>>> {
    let mut seg_states = Vec::new();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      match info_from {
        // all segments, `segments_idx`'s values are sorted by segment name
        None => {
          for info in inner.segment_infos.iter() {
            let info_id = info.info.get_id_key();
            if info.get_buffered_deletes_gen() <= del_gen && !already_seen.contains(info_id) {
              let rld = self
                .get_pooled_instance(info, true)?
                .ok_or_else(|| LuceneError::illegal_state("should not None"))?;
              let seg_state = SegmentState::new(rld, info)?;
              seg_states.push(seg_state);
              already_seen.insert(info_id.to_string());
            }
          }
        },
        Some(info_id) => {
          let info = inner.segment_infos.index_of_live(&info_id).ok_or_else(|| {
            LuceneError::illegal_state(format!("{} not in IndexWriter's segment_infos", info_id))
          })?;
          if info.get_buffered_deletes_gen() <= del_gen && !already_seen.contains(&info_id) {
            let rld = self
              .get_pooled_instance(info, true)?
              .ok_or_else(|| LuceneError::illegal_state("should not None"))?;
            let seg_state = SegmentState::new(rld, info)?;
            seg_states.push(seg_state);
            already_seen.insert(info_id);
          }
        },
      }
      Ok(())
    }));

    if !matches!(&result, Ok(Ok(()))) {
      let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        IOUtils::close(seg_states.iter_mut(), |state| state.close(self, inner))
      }));
      IOUtils::use_or_suppress_caught_result(result, close_result)?;
      unreachable!();
    }

    Ok(seg_states)
  }
  fn validate(&self, info: &SegmentCommitInfo<D>) -> Result<()> {
    if !info.info.dir.is_same_identity(&self.directory_orig) {
      return Err(LuceneError::illegal_argument(
        "SegmentCommitInfo must be from the same directory",
      ));
    }
    Ok(())
  }
  /// Expert: returns a readonly reader, covering all committed as well as un-committed changes to
  /// the index. This provides "near real-time" searching, in that changes made during an
  /// `IndexWriter` session can be quickly made available for searching without closing the writer nor
  /// calling [`Self::commit`].
  ///
  /// Note that this is functionally equivalent to calling [`Self::flush`] and then opening a new
  /// reader. But the turnaround time of this method should be faster since it avoids the potentially
  /// costly [`Self::commit`].
  ///
  /// You must close the [`IndexReader`] returned by this method once you are done using it.
  ///
  /// It's *near* real-time because there is no hard guarantee on how quickly you can get a new reader
  /// after making changes with `IndexWriter`. You'll have to experiment in your situation to determine
  /// if it's fast enough. As this is a new and experimental feature, please report back on your
  /// findings so we can learn, improve and iterate.
  ///
  /// The resulting reader supports [`DirectoryReader::open_if_changed`], but that call will simply
  /// forward back to this method (though this may change in the future).
  ///
  /// The very first time this method is called, this writer instance will make every effort to pool
  /// the readers that it opens for doing merges, applying deletes, etc. This means additional
  /// resources (RAM, file descriptors, CPU time) will be consumed.
  ///
  /// For lower latency on reopening a reader, you should call
  /// [`IndexWriterConfig::set_merged_segment_warmer`] to pre-warm a newly merged segment before it's
  /// committed to the index. This is important for minimizing index-to-search delay after a large
  /// merge.
  ///
  /// If an `add_indexes*` call is running in another thread, then this reader will only search those
  /// segments from the foreign index that have been successfully copied over, so far.
  ///
  /// **NOTE**: Once the writer is closed, any outstanding readers may continue to be used. However, if
  /// you attempt to reopen any of those readers, you'll return an [`AlreadyClosedError`].
  ///
  /// # Returns
  /// `IndexReader` that covers entire index plus all changes made so far by this `IndexWriter`
  /// instance.
  ///
  /// # Errors
  /// Returns an error if there is a low-level I/O error.
  ///
  /// # Experimental
  ///
  /// This API is experimental and might change in incompatible ways in the next release.
  pub(crate) fn get_reader(
    self: &Arc<Self>,
    apply_all_deletes: bool,
    write_all_deletes: bool,
  ) -> Result<StandardDirectoryReader<D>>
  where
    D: 'static,
  {
    self.do_ensure_open(true)?;

    if write_all_deletes && !apply_all_deletes {
      return Err(LuceneError::illegal_argument(
        "applyAllDeletes must be true when writeAllDeletes=true",
      ));
    }

    let t_start = Instant::now();

    if self.info_stream.is_enabled("IW") {
      self.info_stream.message("IW", "flush at getReader")?;
    }

    // Do this up front before flushing so that the readers
    // obtained during this flush are pooled, the first time
    // this method is called:
    self.reader_pool.enable_reader_pooling();

    if let Some(ref s) = self.hooks {
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
    let mut on_get_reader_merges: Option<UpdatePendingMergesResult<D>> = None;
    let stop_collecting_merged_readers = Arc::new(AtomicBool::new(false));
    let merged_readers = Arc::new(Mutex::new(HashMap::new()));
    let opened_read_only_clones = Arc::new(Mutex::new(HashMap::new()));
    let mut reader_factory = IOFunctionImpl::new(
      Arc::downgrade(self),
      Arc::clone(&opened_read_only_clones),
      max_full_flush_merge_wait_millis,
    );
    let mut on_get_reader_merge_resources = None;
    let mut opening_segment_infos: Option<Arc<Mutex<SegmentInfos<D>>>> = None;
    let mut reader: Option<StandardDirectoryReader<D>> = None;
    let mut success2 = false;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
      || -> Result<StandardDirectoryReader<D>> {
        let body_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
          || -> Result<StandardDirectoryReader<D>> {
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

            {
              let _full_flush_lock = self.full_flush_lock.lock();
              let mut success = false;
              let flush_result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
                  any_changes = self.doc_writer.flush_all_threads(self, &self.config)? < 0;
                  if !any_changes {
                    self.flush_count.fetch_add(1, Ordering::SeqCst);
                  }
                  self.publish_flushed_segments(true)?;
                  self.process_events(false)?;
                  if apply_all_deletes {
                    self.apply_all_deletes_and_updates()?;
                  }
                  {
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
                    reader = Some(open_with_reader_function(
                      self,
                      &mut reader_factory,
                      None,
                      &mut inner,
                      apply_all_deletes,
                      write_all_deletes,
                    )?);

                    if self.info_stream.is_enabled("IW") {
                      let reader = reader.as_ref().ok_or_else(|| {
                        LuceneError::illegal_state("near-real-time reader is missing")
                      })?;
                      self.info_stream.message(
                        "IW",
                        &format!(
                          "return reader version={} reader={}",
                          reader.get_version()?,
                          reader
                        ),
                      )?;
                    }

                    if max_full_flush_merge_wait_millis > 0 {
                      // we take the SIS from the reader which has already pruned away fully deleted readers
                      // this makes pulling the readers below after the merge simpler since we can be safe
                      // that
                      // they are not closed. Every segment has a corresponding SR in the SDR we opened if
                      // we use
                      // this SIS
                      // we need to do this rather complicated management of SRs and infos since we can't
                      // wait for merges
                      // while we hold the fullFlushLock since the merge might hit a tragic event and that
                      // must not be reported
                      // while holding that lock. Merging outside of the lock ie. after calling
                      // docWriter.finishFullFlush(boolean) would
                      // yield wrong results because deletes might sneak in during the merge
                      let r = reader.as_ref().ok_or_else(|| {
                        LuceneError::illegal_state("near-real-time reader is missing")
                      })?;
                      let opening_segment_infos_for_merge =
                        Arc::new(Mutex::new(r.get_segment_infos().try_clone()?));
                      opening_segment_infos = Some(opening_segment_infos_for_merge.clone());
                      on_get_reader_merges = self.prepare_point_in_time_merge(
                        opening_segment_infos_for_merge,
                        stop_collecting_merged_readers.clone(),
                        MergeTrigger::GetReader,
                        PointInTimeMergeFinished::GetReader(Arc::new(
                          PointInTimeGetReaderMergeFinished::new(
                            reader_factory.clone(),
                            merged_readers.clone(),
                            stop_collecting_merged_readers.clone(),
                          ),
                        )),
                        &mut inner,
                      )?;
                      let writer = Arc::clone(self);
                      let stop_collecting_merged_readers =
                        Arc::clone(&stop_collecting_merged_readers);
                      let merged_readers = Arc::clone(&merged_readers);
                      on_get_reader_merge_resources = Some(move || {
                        let mut inner = writer.inner.lock();
                        stop_collecting_merged_readers.store(true, Ordering::SeqCst);
                        let merged_readers = merged_readers.lock();
                        IOUtils::close(merged_readers.values(), |reader| {
                          let dec_ref_result =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                              reader
                                .get_segment_info()
                                .files()
                                .and_then(|files| inner.deleter.dec_ref(files.iter()))
                            }));
                          reader.close()?;
                          unwrap_caught_result!(dec_ref_result)
                        })
                      });
                    }
                  };
                  success = true;
                  Ok(())
                }));
              self.doc_writer.finish_full_flush(success, &self.config)?;
              if success {
                self.process_events(false)?;
                if let Some(ref s) = self.hooks {
                  s.do_after_flush()?
                }
              } else if self.info_stream.is_enabled("IW") {
                self
                  .info_stream
                  .message("IW", "hit exception during NRT reader")?;
              }
              unwrap_caught_result!(flush_result)?;
            }

            if let Some(ref on_get_reader_merges) = on_get_reader_merges {
              let opening_segment_infos = opening_segment_infos.as_ref().ok_or_else(|| {
                LuceneError::illegal_state("point-in-time getReader SegmentInfos is none")
              })?;
              if let Some(merged_reader) = self.finish_get_reader_merge(
                stop_collecting_merged_readers.clone(),
                merged_readers.clone(),
                opened_read_only_clones.clone(),
                opening_segment_infos.clone(),
                apply_all_deletes,
                write_all_deletes,
                on_get_reader_merges,
                max_full_flush_merge_wait_millis,
              )? {
                let old_reader = reader
                  .replace(merged_reader)
                  .ok_or_else(|| LuceneError::illegal_state("near-real-time reader is missing"))?;
                old_reader.close()?;
              }
            }

            any_changes |= self.maybe_merge.swap(false, Ordering::SeqCst);
            if any_changes {
              self.maybe_merge_with_max_num_segments(
                self.config.get_merge_policy(),
                MergeTrigger::FullFlush,
                UNBOUNDED_MAX_MERGE_SEGMENTS,
              )?;
            }

            if self.info_stream.is_enabled("IW") {
              self.info_stream.message(
                "IW",
                &format!("getReader took {} ms", t_start.elapsed().as_millis()),
              )?;
            }
            let reader = reader
              .take()
              .ok_or_else(|| LuceneError::illegal_state("near-real-time reader is missing"))?;
            success2 = true;
            Ok(reader)
          },
        ));
        if body_result.is_err() {
          self.tragic_event(&body_result, "getReader", None)?;
        }
        unwrap_caught_result!(body_result)
      },
    ));

    if !success2 {
      let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        IOUtils::close_while_handling_error(0..2, |operation| match operation {
          0 => match reader.take() {
            Some(reader) => reader.close(),
            None => Ok(()),
          },
          1 => match on_get_reader_merge_resources.as_mut() {
            Some(resource) => resource(),
            None => Ok(()),
          },
          _ => unreachable!(),
        })
      }));
      self.maybe_close_on_tragic_event(None)?;
      unwrap_caught_result!(close_result)?;
    } else if let Some(resource) = on_get_reader_merge_resources.as_mut() {
      resource()?;
    }
    unwrap_caught_result!(result)
  }
  #[allow(clippy::too_many_arguments)]
  fn finish_get_reader_merge(
    self: &Arc<Self>,
    stop_collecting_merged_readers: Arc<AtomicBool>,
    merged_readers: Arc<Mutex<HashMap<String, DefaultLeafReader<D>>>>,
    opened_read_only_clones: Arc<Mutex<HashMap<String, DefaultLeafReader<D>>>>,
    opening_segment_infos: Arc<Mutex<SegmentInfos<D>>>,
    apply_all_deletes: bool,
    write_all_deletes: bool,
    point_in_time_merges: &UpdatePendingMergesResult<D>,
    max_commit_merge_wait_millis: i64,
  ) -> Result<Option<StandardDirectoryReader<D>>>
  where
    D: 'static,
  {
    let merge_source = self.new_merge_source()?;
    self
      .config
      .get_merge_scheduler()
      .merge(merge_source, MergeTrigger::GetReader)?;
    point_in_time_merges.await_merges(Duration::from_millis(max_commit_merge_wait_millis as u64));

    let mut inner = self.inner.lock();
    stop_collecting_merged_readers.store(true, Ordering::SeqCst);
    let reader = self.maybe_reopen_merged_nrt_reader(
      &merged_readers,
      &opened_read_only_clones,
      &opening_segment_infos,
      apply_all_deletes,
      write_all_deletes,
      &mut inner,
    )?;
    {
      let mut merged_readers = merged_readers.lock();
      IOUtils::close(merged_readers.values(), |reader| reader.close())?;
      merged_readers.clear();
    }
    Ok(reader)
  }
  #[allow(clippy::too_many_arguments)]
  fn maybe_reopen_merged_nrt_reader(
    self: &Arc<Self>,
    merged_readers: &Arc<Mutex<HashMap<String, DefaultLeafReader<D>>>>,
    opened_read_only_clones: &Arc<Mutex<HashMap<String, DefaultLeafReader<D>>>>,
    opening_segment_infos: &Arc<Mutex<SegmentInfos<D>>>,
    apply_all_deletes: bool,
    write_all_deletes: bool,
    inner: &mut Inner<D>,
  ) -> Result<Option<StandardDirectoryReader<D>>>
  where
    D: 'static,
  {
    if merged_readers.lock().is_empty() {
      return Ok(None);
    }

    let opening_segment_infos = Some(&*opening_segment_infos.lock());
    let mut files = Vec::new();
    let mut reader_function =
      MergedNRTReaderFunction::new(merged_readers, opened_read_only_clones, &mut files);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      open_with_reader_function(
        self,
        &mut reader_function,
        opening_segment_infos,
        inner,
        apply_all_deletes,
        write_all_deletes,
      )
    }));
    inner.deleter.dec_ref(files.iter())?;
    unwrap_caught_result!(result).map(Some)
  }

  /// Counts soft-deleted and hard-deleted documents in the given reader.
  /// Updates the provided counters.
  ///
  /// Corresponds to Java: IndexWriter.countSoftDeletes(CodecReader, Bits, Bits, Counter, Counter)
  fn count_soft_deletes<L>(
    &self,
    reader: &L,
    wrapped_live_docs: Option<&impl Bits>,
    hard_live_docs: Option<&impl Bits>,
    soft_delete_counter: &impl Counter,
    hard_delete_counter: &impl Counter,
  ) -> Result<()>
  where
    L: LeafReader,
  {
    let soft_deletes_field = self.config.get_soft_deletes_field().ok_or_else(|| {
      LuceneError::illegal_state(
        "soft_deletes_enabled is true but soft_deletes_field is not configured",
      )
    })?;
    let mut hard_delete_count = 0_i64;
    let mut soft_deletes_count = 0_i64;
    let mut soft_deleted_docs = get_doc_values_doc_id_set_iterator(soft_deletes_field, reader)?;
    if let Some(ref mut docs) = soft_deleted_docs {
      loop {
        let doc_id = docs.next_doc()?;
        if doc_id == NO_MORE_DOCS {
          break;
        }
        let is_wrapped_live = match wrapped_live_docs {
          Some(bits) => bits.get(doc_id as usize)?,
          None => true,
        };
        if is_wrapped_live {
          let is_hard_live = match hard_live_docs {
            Some(bits) => bits.get(doc_id as usize)?,
            None => true,
          };
          if is_hard_live {
            soft_deletes_count += 1;
          } else {
            hard_delete_count += 1;
          }
        }
      }
    }
    soft_delete_counter.add_and_get(soft_deletes_count);
    hard_delete_counter.add_and_get(hard_delete_count);
    Ok(())
  }

  /// Asserts that the soft delete count in the given reader matches the expected count.
  ///
  /// Corresponds to Java: IndexWriter.assertSoftDeletesCount(CodecReader, int)
  fn assert_soft_deletes_count<L>(&self, reader: &L, expected_count: i32) -> Result<bool>
  where
    L: LeafReader,
  {
    let count = new_counter(false);
    let hard_deletes = new_counter(false);
    let live_docs = reader.get_live_docs()?;
    self.count_soft_deletes(
      reader,
      live_docs.as_ref(),
      None::<&FixedBitSet>,
      &count,
      &hard_deletes,
    )?;
    let actual = count.get() as i32;
    debug_assert!(
      actual == expected_count,
      "soft-deletes count mismatch expected: {expected_count} but actual: {actual}"
    );
    Ok(true)
  }

  pub(crate) fn get_segment_infos_version(&self) -> i64 {
    let inner = self.inner.lock();
    inner.segment_infos.get_version()
  }
}
/// Called internally if any index state has changed.
pub(crate) fn changed<D>(change_count: &mut i64, segment_infos: &mut SegmentInfos<D>) {
  *change_count += 1;
  segment_infos.changed()
}
impl<D> TwoPhaseCommit for IndexWriter<D>
where
  D: Directory + 'static,
{
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
  fn prepare_commit(&self) -> Result<i64> {
    self.ensure_open()?;
    self
      .pending_seq_no
      .store(self.prepare_commit_internal(None)?, Ordering::SeqCst);
    // we must do this outside of the commitLock else we can deadlock:
    if self.maybe_merge.swap(false, Ordering::SeqCst) {
      self.maybe_merge_with_max_num_segments(
        self.config.get_merge_policy(),
        MergeTrigger::FullFlush,
        UNBOUNDED_MAX_MERGE_SEGMENTS,
      )?;
    }
    Ok(self.pending_seq_no.load(Ordering::SeqCst))
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
  /// `prepare_commit`
  ///
  /// # Returns
  /// The `sequence number` of the last operation in the commit.
  /// All sequence numbers `<=` this value will be reflected in the commit, and all others will not.
  fn commit(&self) -> Result<i64> {
    self.ensure_open()?;
    self.commit_internal(self.config.get_merge_policy())
  }
  /// Close the `IndexWriter` without committing any changes that have occurred since the last
  /// commit, or since it was opened if commit hasn't been called.
  fn rollback(&self) -> Result<()> {
    // don't call ensureOpen here: this acts like "close()" in closeable.

    // Ensure that only one thread actually gets to do the
    // closing, and make sure no commit is also in progress:
    if self.should_close(true) {
      self.rollback_internal(None)?;
    }
    Ok(())
  }
}
pub struct IndexCommitWrapper<'a, IC, D>
where
  IC: IndexCommit<Directory = Arc<D>>,
  D: Directory,
{
  pub(crate) commit: Option<IC>,
  pub(crate) reader: Option<&'a StandardDirectoryReader<D>>,
}
impl<'a, IC, D> IndexCommitWrapper<'a, IC, D>
where
  IC: IndexCommit<Directory = Arc<D>>,
  D: Directory,
{
  pub fn new(commit: Option<IC>, reader: Option<&'a StandardDirectoryReader<D>>) -> Result<Self> {
    Ok(Self { commit, reader })
  }
}
impl<'a, D> Default for IndexCommitWrapper<'a, DummyIndexCommit<D>, D>
where
  D: Directory,
{
  fn default() -> Self {
    Self {
      commit: None,
      reader: None,
    }
  }
}
/// If `open(IndexWriter)` has been called (ie, this writer is in near
/// real-time mode), then after a merge completes, this callback can warm the reader on
/// the newly merged segment, before the merge commits. This is not required for near real-time
/// search, but will reduce search latency on opening a new near real-time reader after a merge
/// completes.
///
/// # Experimental
///
/// **NOTE**: `warm(LeafReader)` is called before any deletes have been carried
/// over to the merged segment.
pub trait IndexReaderWarmer<D>
where
  D: Directory,
{
  fn warm(&self, reader: &DefaultLeafReader<D>) -> Result<()>;
}

impl<D, F> IndexReaderWarmer<D> for F
where
  D: Directory,
  F: Fn(&DefaultLeafReader<D>) -> Result<()> + Send + Sync,
{
  fn warm(&self, reader: &DefaultLeafReader<D>) -> Result<()> {
    self(reader)
  }
}

pub type CustomIndexReaderWarmer<D> = Box<dyn IndexReaderWarmer<D> + Send + Sync>;

pub enum IndexReaderWarmerEnum<D>
where
  D: Directory,
{
  SimpleMergedSegmentWarmer(SimpleMergedSegmentWarmer),
  Custom(CustomIndexReaderWarmer<D>),
}

impl<D> IndexReaderWarmerEnum<D>
where
  D: Directory,
{
  pub fn custom<T>(warmer: T) -> Self
  where
    T: IndexReaderWarmer<D> + Send + Sync + 'static,
  {
    Self::Custom(Box::new(warmer))
  }
}

impl<D> From<SimpleMergedSegmentWarmer> for IndexReaderWarmerEnum<D>
where
  D: Directory,
{
  fn from(warmer: SimpleMergedSegmentWarmer) -> Self {
    Self::SimpleMergedSegmentWarmer(warmer)
  }
}

impl<D> From<CustomIndexReaderWarmer<D>> for IndexReaderWarmerEnum<D>
where
  D: Directory,
{
  fn from(warmer: CustomIndexReaderWarmer<D>) -> Self {
    Self::Custom(warmer)
  }
}

impl<D> IndexReaderWarmer<D> for IndexReaderWarmerEnum<D>
where
  D: Directory,
{
  fn warm(&self, reader: &DefaultLeafReader<D>) -> Result<()> {
    match self {
      Self::SimpleMergedSegmentWarmer(warmer) => warmer.warm(reader),
      Self::Custom(warmer) => warmer.warm(reader),
    }
  }
}

struct IOConsumerImpl1<'a, D>
where
  D: Directory,
{
  index_writer: &'a IndexWriter<D>,
}
impl<'a, D> IOConsumerImpl1<'a, D>
where
  D: Directory,
{
  fn new(index_writer: &'a IndexWriter<D>) -> Self {
    Self { index_writer }
  }
}
impl<'a, D> IOConsumer<HashSet<String>> for IOConsumerImpl1<'a, D>
where
  D: Directory,
{
  fn accept(&mut self, input: HashSet<String>) -> Result<()> {
    self.index_writer.delete_new_files(input.iter(), None)
  }
}

struct DocMapIMpl2<DM1, DM2> {
  compaction_doc_map: DM1,
  reorder_doc_map: DM2,
}
impl<DM1, DM2> DocMapIMpl2<DM1, DM2> {
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

struct DocMapImpl1<DM> {
  doc_map: DM,
  max_doc: i32,
  current_doc_base: i32,
}
impl<DM> DocMapImpl1<DM> {
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
    CoreHelper::check_index(doc_id, self.max_doc)?;
    self.doc_map.old_to_new(self.current_doc_base + doc_id)
  }
}

impl<D> MergeContext<D> for IndexWriter<D>
where
  D: Directory,
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

    let num_deletes_to_merge = match self.get_pooled_instance(info, false)? {
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

    if let Some(rld) = self.get_pooled_instance(info, false).unwrap() {
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
  pub(crate) fn are_enabled(&self) -> bool {
    self.merges_enabled
  }

  pub(crate) fn disable(&mut self) {
    self.merges_enabled = false;
  }

  pub(crate) fn enable<D>(&mut self, writer: &IndexWriter<D>) -> Result<()>
  where
    D: Directory,
  {
    writer.ensure_open()?;
    self.merges_enabled = true;
    Ok(())
  }
}

pub(crate) struct IOConsumerImpl<'a, D>
where
  D: Directory,
{
  inner: &'a mut Inner<D>,
  merge_readers: &'a mut HashMap<String, DefaultLeafReader<D>>,
  reader_factory: &'a mut IOFunctionImpl<D>,
  stop_collecting_merged_readers: &'a AtomicBool,
}
impl<'a, D> IOConsumerImpl<'a, D>
where
  D: Directory,
{
  pub(crate) fn new(
    inner: &'a mut Inner<D>,
    merge_readers: &'a mut HashMap<String, DefaultLeafReader<D>>,
    reader_factory: &'a mut IOFunctionImpl<D>,
    stop_collecting_merged_readers: &'a AtomicBool,
  ) -> Self {
    Self {
      inner,
      merge_readers,
      reader_factory,
      stop_collecting_merged_readers,
    }
  }
}
impl<'a, D> IOConsumer<SegmentCommitInfo<D>> for IOConsumerImpl<'a, D>
where
  D: Directory,
{
  fn accept_ref(&mut self, sci: &SegmentCommitInfo<D>) -> Result<()> {
    debug_assert!(
      !self.stop_collecting_merged_readers.load(Ordering::SeqCst),
      "illegal state  merge reader must be not pulled since we already stopped waiting for merges"
    );
    let apply = self.reader_factory.apply(sci, self.inner)?;
    self.merge_readers.insert(sci.info.name.clone(), apply);
    // we need to incRef the files of the opened SR otherwise it's possible that
    // another merge
    // removes the segment before we pass it on to the SDR
    self.inner.deleter.inc_ref_files(sci.files()?)?;
    Ok(())
  }
}

pub(crate) struct IOFunctionImpl<D>
where
  D: Directory,
{
  writer: Weak<IndexWriter<D>>,
  opened_read_only_clones: Arc<Mutex<HashMap<String, DefaultLeafReader<D>>>>,
  max_full_flush_merge_wait_millis: i64,
}
impl<D> Clone for IOFunctionImpl<D>
where
  D: Directory,
{
  fn clone(&self) -> Self {
    Self {
      writer: self.writer.clone(),
      opened_read_only_clones: self.opened_read_only_clones.clone(),
      max_full_flush_merge_wait_millis: self.max_full_flush_merge_wait_millis,
    }
  }
}
impl<D> IOFunctionImpl<D>
where
  D: Directory,
{
  pub(crate) fn new(
    writer: Weak<IndexWriter<D>>,
    opened_read_only_clones: Arc<Mutex<HashMap<String, DefaultLeafReader<D>>>>,
    max_full_flush_merge_wait_millis: i64,
  ) -> Self {
    Self {
      writer,
      opened_read_only_clones,
      max_full_flush_merge_wait_millis,
    }
  }
}
impl<D> IOFunction<SegmentCommitInfo<D>, Inner<D>, DefaultLeafReader<D>> for IOFunctionImpl<D>
where
  D: Directory,
{
  fn apply(
    &mut self,
    sci: &SegmentCommitInfo<D>,
    inner: &mut Inner<D>,
  ) -> Result<DefaultLeafReader<D>> {
    let writer = self
      .writer
      .upgrade()
      .ok_or_else(|| LuceneError::illegal_state("point-in-time getReader writer is gone"))?;
    let rld = writer
      .get_pooled_instance(sci, true)?
      .ok_or_else(|| LuceneError::illegal_state("should always be able to get pooled instance"))?;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      rld
        .get_read_only_clone(&IOContext::default_io_context()?, sci)?
        .ok_or_else(|| LuceneError::illegal_state("should always be able to get read only clone"))
        .inspect(|segment_reader| {
          if self.max_full_flush_merge_wait_millis > 0 {
            self
              .opened_read_only_clones
              .lock()
              .insert(sci.info.name.clone(), segment_reader.clone());
          }
        })
    }));
    writer.release(rld.as_ref(), inner)?;
    unwrap_caught_result!(result)
  }
}

struct MergedNRTReaderFunction<'a, D>
where
  D: Directory,
{
  merged_readers: &'a Arc<Mutex<HashMap<String, DefaultLeafReader<D>>>>,
  opened_read_only_clones: &'a Arc<Mutex<HashMap<String, DefaultLeafReader<D>>>>,
  files: &'a mut Vec<String>,
}
impl<'a, D> MergedNRTReaderFunction<'a, D>
where
  D: Directory,
{
  fn new(
    merged_readers: &'a Arc<Mutex<HashMap<String, DefaultLeafReader<D>>>>,
    opened_read_only_clones: &'a Arc<Mutex<HashMap<String, DefaultLeafReader<D>>>>,
    files: &'a mut Vec<String>,
  ) -> Self {
    Self {
      merged_readers,
      opened_read_only_clones,
      files,
    }
  }
}
impl<'a, D> IOFunction<SegmentCommitInfo<D>, Inner<D>, DefaultLeafReader<D>>
  for MergedNRTReaderFunction<'a, D>
where
  D: Directory,
{
  fn apply(
    &mut self,
    sci: &SegmentCommitInfo<D>,
    _inner: &mut Inner<D>,
  ) -> Result<DefaultLeafReader<D>> {
    if let Some(remove) = self.merged_readers.lock().remove(&sci.info.name) {
      self.files.extend(remove.get_segment_info().files()?);
      return Ok(remove);
    }

    let reader = self
      .opened_read_only_clones
      .lock()
      .remove(&sci.info.name)
      .ok_or_else(|| LuceneError::illegal_state("point-in-time reader clone is missing"))?;
    reader.inc_ref()?;
    Ok(reader)
  }
}
impl<D> Display for IndexWriter<D>
where
  D: Directory,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
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

pub trait IndexWriterHooks {
  /// A hook for implementations to execute operations before a merge begins.
  #[cfg(test)]
  fn do_before_merge(&self, _merge: &MergeStat) -> Result<()> {
    Ok(())
  }

  /// A hook for implementations to execute operations after a merge succeeds.
  #[cfg(test)]
  fn merge_success(&self, _merge: &MergeStat) -> Result<()> {
    Ok(())
  }

  /// A hook for implementations to execute operations after pending added and deleted documents have been flushed to the directory.
  /// but before the change is committed (new segments_N file written).
  fn do_after_flush(&self) -> Result<()> {
    Ok(())
  }
  /// A hook for implementations to execute operations before pending added and deleted documents are flushed to the directory.
  fn do_before_flush(&self) -> Result<()> {
    Ok(())
  }

  fn is_enable_test_points(&self) -> bool {
    false
  }
}
#[derive(Default)]
pub struct EmptyIndexWriterHooks;
impl IndexWriterHooks for EmptyIndexWriterHooks {}

pub type CustomIndexWriterHooks = Box<dyn IndexWriterHooks + Send + Sync>;
pub enum IndexWriterHooksEnum {
  EmptyIndexWriterHooks(EmptyIndexWriterHooks),
  Custom(CustomIndexWriterHooks),
  #[cfg(test)]
  TestTragicIndexWriterDeadlock(TragicIndexWriter),
}
impl IndexWriterHooksEnum {
  pub fn custom<B>(base: B) -> Self
  where
    B: IndexWriterHooks + Send + Sync + 'static,
  {
    Self::Custom(Box::new(base))
  }
}
impl From<EmptyIndexWriterHooks> for IndexWriterHooksEnum {
  fn from(base: EmptyIndexWriterHooks) -> Self {
    Self::EmptyIndexWriterHooks(base)
  }
}
impl IndexWriterHooks for IndexWriterHooksEnum {
  #[cfg(test)]
  fn do_before_merge(&self, merge: &MergeStat) -> Result<()> {
    match self {
      Self::EmptyIndexWriterHooks(inner) => inner.do_before_merge(merge),
      Self::Custom(inner) => inner.do_before_merge(merge),
      Self::TestTragicIndexWriterDeadlock(inner) => inner.do_before_merge(merge),
    }
  }

  #[cfg(test)]
  fn merge_success(&self, merge: &MergeStat) -> Result<()> {
    match self {
      Self::EmptyIndexWriterHooks(inner) => inner.merge_success(merge),
      Self::Custom(inner) => inner.merge_success(merge),
      Self::TestTragicIndexWriterDeadlock(inner) => inner.merge_success(merge),
    }
  }

  fn do_after_flush(&self) -> Result<()> {
    match self {
      Self::EmptyIndexWriterHooks(inner) => inner.do_after_flush(),
      Self::Custom(inner) => inner.do_after_flush(),
      #[cfg(test)]
      Self::TestTragicIndexWriterDeadlock(inner) => inner.do_after_flush(),
    }
  }

  fn do_before_flush(&self) -> Result<()> {
    match self {
      Self::EmptyIndexWriterHooks(inner) => inner.do_before_flush(),
      Self::Custom(inner) => inner.do_before_flush(),
      #[cfg(test)]
      Self::TestTragicIndexWriterDeadlock(inner) => inner.do_before_flush(),
    }
  }

  fn is_enable_test_points(&self) -> bool {
    match self {
      Self::EmptyIndexWriterHooks(inner) => inner.is_enable_test_points(),
      Self::Custom(inner) => inner.is_enable_test_points(),
      #[cfg(test)]
      Self::TestTragicIndexWriterDeadlock(inner) => inner.is_enable_test_points(),
    }
  }
}
pub(crate) type TragicException = Arc<OnceLock<LuceneError>>;

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

  fn flush_failed<D>(&self, mut info: SegmentInfo<D>) -> Result<()> {
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

  fn after_segments_flushed<D>(&self, writer: &IndexWriter<D>) -> Result<()>
  where
    D: Directory,
  {
    writer.publish_flushed_segments(false)
  }

  fn on_tragic_event<D>(
    &self,
    event: &CaughtResult,
    message: &str,
    writer: &IndexWriter<D>,
  ) -> Result<()>
  where
    D: Directory,
  {
    writer.on_tragic_event(event, message)
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
use crate::core::codecs::{Codec, CompoundFormat};
use crate::core::document::fields::Fields;
use crate::core::index::binary_doc_values_field_updates::BinaryDocValuesFieldUpdates;
use crate::core::index::buffered_updates::MAX_INT;
use crate::core::index::caching_merge_context::CachingMergeContext;
use crate::core::index::codec_reader::{CodecReader, ReorderedMergeCodecReader};
use crate::core::index::directory_reader::DirectoryReader;
use crate::core::index::doc_values_field_updates::{
  DocValuesFieldIterator, DocValuesFieldUpdates, DocValuesFieldUpdatesBase,
  DocValuesFieldUpdatesBaseEnum,
};
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::doc_values_update::{
  BinaryDocValuesUpdate, DocValuesUpdate, DocValuesUpdateEnum, NumericDocValuesUpdate,
};
use crate::core::index::documents_writer_delete_queue::{DocumentsWriterDeleteQueue, Node};
use crate::core::index::documents_writer_flush_queue::FlushTicket;
use crate::core::index::dummy::dummy_doc_map_sorter::DummyDocMap;
use crate::core::index::dummy::dummy_index_commit::DummyIndexCommit;
use crate::core::index::field_infos::{FieldInfos, FieldNumbers, FieldNumbersLock};
use crate::core::index::filter_codec_reader::{
  CodecReaderImpl, FilterCodecReaderBits, wrap_live_docs,
};
use crate::core::index::index_commit::IndexCommit;
use crate::core::index::index_reader::{IndexReader, IndexReaderContextType};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer_config::{DISABLE_AUTO_FLUSH, IndexWriterConfig, OpenMode};
use crate::core::index::index_writer_event_listener::IndexWriterEventListener;
use crate::core::index::indexable_field::IndexableField;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::merge_policy::{
  MergeReader, OneMerge, OneMergeBase, OneMergeDefaults, OneMergeHook,
};
use crate::core::index::merge_trigger::MergeTrigger;
use crate::core::index::numeric_doc_values_field_updates::NumericDocValuesFieldUpdates;
use crate::core::index::one_merge_wrapping_merge_policy::{
  OneMergeUnaryOperatorBase, OneMergeWrappingMergePolicy,
};
use crate::core::index::pending_soft_deletes::count_soft_deletes;
use crate::core::index::reader_pool::ReaderPool;
use crate::core::index::readers_and_updates::ReadersAndUpdates;
use crate::core::index::segment_commit_info::{SegmentCommitInfo, validate_soft_del_count};
use crate::core::index::segment_merger::SegmentMerger;
use crate::core::index::segment_reader::DefaultLeafReader;
use crate::core::index::simple_merged_segment_warmer::SimpleMergedSegmentWarmer;
use crate::core::index::slow_composite_codec_reader_wrapper::wrap;
use crate::core::index::sorter::DocMapImpl;
use crate::core::index::sorting_codec_reader::wrap_with_doc_map;
use crate::core::index::standard_directory_reader::{
  StandardDirectoryReader, open_with_reader_function,
};
use crate::core::index::term::Term;
use crate::core::index::tiered_merge_policy::SegmentDocAndID;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::index::{BytesRef, IndexFileNames};
use crate::core::index::{directory_reader, readers_and_updates};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::field_exists_query::get_doc_values_doc_id_set_iterator;
use crate::core::search::query::Query;
use crate::core::search::sort::Sort;
use crate::core::store::IOContext;
use crate::core::store::lock_validating_directory_wrapper::LockValidatingDirectoryWrapper;
use crate::core::store::merge_info::MergeInfo;
use crate::core::store::tracking_directory_wrapper::TrackingDirectoryWrapper;
use crate::core::util::accountable::Accountable;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bits::Bits;
use crate::core::util::constants::Constants;
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::info_stream::{InfoStream, InfoStreamMT};
use crate::core::util::io_consumer::IOConsumer;
use crate::core::util::io_function::IOFunction;
use crate::core::util::unicode_util::UnicodeUtil;
use crate::core::util::{
  BYTE_BLOCK_SIZE, CoreHelper, HasIdentity, IOUtils, LATEST, MIN_SUPPORTED_MAJOR, StringHelper,
  TryIntoInt,
};
#[cfg(test)]
use crate::test_framework::core::internal::index_writer_access::IndexWriterAccess;
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
#[cfg(not(test))]
static ACTUAL_MAX_DOCS: AtomicI32 = AtomicI32::new(MAX_DOCS);
#[cfg(test)]
thread_local! {
  static ACTUAL_MAX_DOCS: Cell<i32> = const { Cell::new(MAX_DOCS) };
}

pub const MAX_TERM_LENGTH: i32 = BYTE_BLOCK_SIZE - 2;
const UNBOUNDED_MAX_MERGE_SEGMENTS: i32 = -1;
pub const WRITE_LOCK_NAME: &str = "write.lock";
/// Key for the source of a segment in [`SegmentInfo::get_diagnostics`].
pub const SOURCE: &str = "source";
/// Source of a segment which results from a merge of other segments.
pub const SOURCE_MERGE: &str = "merge";
/// Source of a segment which results from `addIndexes(CodecReader...)`.
pub const SOURCE_ADDINDEXES_READERS: &str = "addIndexes(CodecReader...)";
/// Source of a segment which results from a flush.
pub const SOURCE_FLUSH: &str = "flush";
pub const MAX_STORED_STRING_LENGTH: i32 =
  ArrayUtil::MAX_ARRAY_LENGTH as i32 / UnicodeUtil::MAX_UTF8_BYTES_PER_CHAR;
pub(crate) fn get_actual_max_docs() -> i32 {
  #[cfg(test)]
  {
    ACTUAL_MAX_DOCS.with(Cell::get)
  }
  #[cfg(not(test))]
  {
    ACTUAL_MAX_DOCS.load(Ordering::Relaxed)
  }
}
pub(crate) fn set_max_docs(max_docs: i32) -> Result<()> {
  if max_docs > MAX_DOCS {
    return Err(LuceneError::illegal_argument(format!(
      "maxDocs must be <= IndexWriter.MAX_DOCS={MAX_DOCS}; got: {max_docs}"
    )));
  }
  #[cfg(test)]
  {
    ACTUAL_MAX_DOCS.with(|actual_max_docs| actual_max_docs.set(max_docs));
  }
  #[cfg(not(test))]
  {
    ACTUAL_MAX_DOCS.store(max_docs, Ordering::Relaxed);
  }
  Ok(())
}
/// Convenience overload: no extra details.
pub(crate) fn set_diagnostics<D>(info: &mut SegmentInfo<D>, source: &str) {
  set_diagnostics_impl(info, source, None)
}
fn set_diagnostics_impl<D>(
  info: &mut SegmentInfo<D>,
  source: &str,
  details: Option<HashMap<String, String>>,
) {
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
pub(crate) fn read_field_infos<D>(si: &SegmentCommitInfo<D>) -> Result<FieldInfos>
where
  D: Directory,
{
  let codec = si.info.get_codec()?;
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
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      reader.read(&cfs, &si.info, "", &IOContext::read_once_io_context()?)
    }));
    let close_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cfs.close()));
    IOUtils::use_or_suppress_caught_result(result, close_result)
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
    if info_stream.is_enabled("IW") {
      info_stream.message("IW", "create compound file")?;
    }
  }
  // Now merge all added files
  let mut success = false;
  let write_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
    let codec = info.get_codec()?.clone();
    codec.compound_format().write(directory, info, context)?;
    success = true;
    Ok(())
  }));
  let filename = std::mem::take(&mut directory.get_created_files().lock().created_filenames);
  if !success {
    delete_files.accept(filename)?;
    return unwrap_caught_result!(write_result);
  }
  unwrap_caught_result!(write_result)?;
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
        let res = self
          .avail
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
  pub(crate) fn new() -> Self {
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
  pub(crate) fn add(&self, event: EventEnum) -> Result<()> {
    self.acquire()?;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      self.queue.push(event);
    }));
    self.permits.release();
    unwrap_caught_result!(result);
    Ok(())
  }
  pub(crate) fn process_events<D>(&self, writer: &IndexWriter<D>) -> Result<()>
  where
    D: Directory + 'static,
  {
    self.acquire()?;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      self.process_events_internal(writer)
    }));
    self.permits.release();
    unwrap_caught_result!(result)
  }
  fn process_events_internal<D>(&self, writer: &IndexWriter<D>) -> Result<()>
  where
    D: Directory + 'static,
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
  pub(crate) fn close<D>(&self, writer: &IndexWriter<D>) -> Result<()>
  where
    D: Directory + 'static,
  {
    let _guard = self.guard.lock();
    debug_assert!(
      !self.closed.load(Ordering::Acquire),
      "we should never close this twice"
    );

    self.closed.store(true, Ordering::Release);

    if writer.get_tragic_exception().get().is_some() {
      while self.queue.pop().is_some() {
        // we are already handling a tragic error let's drop it all on the floor and return
      }
      return Ok(());
    }
    // now we acquire all the permits to ensure we are the only one processing the queue
    self.permits.acquire_all();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      self.process_events_internal(writer)
    }));
    self.permits.release_all();
    drop(_guard);
    unwrap_caught_result!(result)
  }
}

/// Trait for internal atomic events. See [`DocumentsWriter`] for details.
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
  fn process(&mut self, writer: &IndexWriter<D>) -> Result<()>;
}
pub(crate) struct EventImpl1 {
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
  fn process(&mut self, writer: &IndexWriter<D>) -> Result<()> {
    writer.delete_new_files(self.files.iter(), None)
  }
}

pub(crate) struct EventImpl2 {
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
  fn process(&mut self, writer: &IndexWriter<D>) -> Result<()> {
    writer.flush_failed(std::mem::take(&mut self.info_files))
  }
}

pub(crate) struct EventImpl3;
impl<D> Event<D> for EventImpl3
where
  D: Directory,
{
  fn process(&mut self, writer: &IndexWriter<D>) -> Result<()> {
    let result = writer.publish_flushed_segments(true);
    writer.flush_count.fetch_add(1, Ordering::SeqCst);
    result
  }
}
pub(crate) struct EventImpl4;
impl<D> Event<D> for EventImpl4
where
  D: Directory,
{
  fn process(&mut self, writer: &IndexWriter<D>) -> Result<()> {
    writer.publish_flushed_segments(true)
  }
}
pub(crate) struct EventImpl5 {
  packet: Arc<FrozenBufferedUpdates>,
}
impl EventImpl5 {
  pub fn new(packet: Arc<FrozenBufferedUpdates>) -> Self {
    Self { packet }
  }
}
impl<D> Event<D> for EventImpl5
where
  D: Directory + 'static,
{
  fn process(&mut self, writer: &IndexWriter<D>) -> Result<()> {
    // we call tryApply here since we don't want to block if a refresh or a flush is already
    // applying the
    // packet. The flush will retry this packet anyway to ensure all of them are applied
    let mut result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      writer.try_apply(&self.packet)
    }));
    if !matches!(&result, Ok(Ok(_))) {
      let tragic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        writer.on_tragic_event(&result, "applyUpdatesPacket")
      }));
      result.add_suppressed(
        tragic_result,
        "panic while handling tragic apply updates packet event",
      );
    }
    unwrap_caught_result!(result)?;
    writer.flush_deletes_count.fetch_add(1, Ordering::SeqCst);
    Ok(())
  }
}

#[cfg(test)]
pub(crate) struct EventImplTest {
  executed: Arc<AtomicI32>,
}

#[cfg(test)]
impl EventImplTest {
  pub(crate) fn new(executed: Arc<AtomicI32>) -> Self {
    Self { executed }
  }
}

#[cfg(test)]
impl<D> Event<D> for EventImplTest
where
  D: Directory,
{
  fn process(&mut self, _writer: &IndexWriter<D>) -> Result<()> {
    self.executed.fetch_add(1, Ordering::SeqCst);
    Ok(())
  }
}

pub(crate) enum EventEnum {
  A(EventImpl1),
  B(EventImpl2),
  C(EventImpl3),
  D(EventImpl4),
  E(EventImpl5),
  #[cfg(test)]
  Test(EventImplTest),
}
impl<D> Event<D> for EventEnum
where
  D: Directory + 'static,
{
  fn process(&mut self, writer: &IndexWriter<D>) -> Result<()> {
    match self {
      EventEnum::A(e) => e.process(writer),
      EventEnum::B(e) => e.process(writer),
      EventEnum::C(e) => e.process(writer),
      EventEnum::D(e) => e.process(writer),
      EventEnum::E(e) => e.process(writer),
      #[cfg(test)]
      EventEnum::Test(e) => e.process(writer),
    }
  }
}
struct IndexWriterMergeSource<D>
where
  D: Directory,
{
  writer: Arc<IndexWriter<D>>,
}

impl<D> Clone for IndexWriterMergeSource<D>
where
  D: Directory,
{
  fn clone(&self) -> Self {
    Self {
      writer: self.writer.clone(),
    }
  }
}

impl<D> IndexWriterMergeSource<D>
where
  D: Directory,
{
  fn new(writer: Arc<IndexWriter<D>>) -> Self {
    Self { writer }
  }

  fn writer(&self) -> &IndexWriter<D> {
    self.writer.as_ref()
  }
}

impl<D> MergeSource<D> for IndexWriterMergeSource<D>
where
  D: Directory,
{
  type Reader = DefaultLeafReader<D>;

  fn get_next_merge(&self) -> Result<Option<OneMergeSR<D>>> {
    self.writer().get_next_merge()
  }

  fn on_merge_finished(&self, merge: &MergeStat, inner: Option<&mut Inner<D>>) {
    self.writer().merge_finish(merge, inner)
  }

  fn has_pending_merges(&self, inner: Option<&mut Inner<D>>) -> Result<bool> {
    if let Some(t) = self.writer().tragedy.get() {
      let mut error =
        LuceneError::illegal_state("this writer hit an unrecoverable error; cannot merge");
      error.add_suppressed(t.clone());
      return Err(error);
    }
    match inner {
      Some(inner) => Ok(!inner.pending_merges.is_empty()),
      None => self.writer().has_pending_merges(),
    }
  }

  fn merge(&self, mut merge: OneMergeSR<D>) -> Result<()>
  where
    D: 'static,
  {
    self.writer().merge(&mut merge)
  }
}
struct AddIndexesMergeSource<D, CR>
where
  D: Directory,
  CR: CodecReader,
{
  writer: Arc<IndexWriter<D>>,
  pending_merges: Mutex<VecDeque<OneMerge<D, CR>>>,
  processed_merges: Mutex<Vec<OneMerge<D, CR>>>,
}

impl<D, CR> AddIndexesMergeSource<D, CR>
where
  D: Directory,
  CR: CodecReader,
{
  fn new(writer: Arc<IndexWriter<D>>) -> Self {
    Self {
      writer,
      pending_merges: Mutex::new(VecDeque::new()),
      processed_merges: Mutex::new(Vec::new()),
    }
  }

  fn writer(&self) -> &IndexWriter<D> {
    self.writer.as_ref()
  }

  fn register_merge(&self, merge: OneMerge<D, CR>, _inner: &mut MutexGuard<'_, Inner<D>>) {
    self.pending_merges.lock().push_back(merge);
  }

  fn has_pending_merges_locked(&self, inner: &Inner<D>) -> bool {
    inner.merges.are_enabled() && !self.pending_merges.lock().is_empty()
  }

  fn on_merge_finished_locked(&self, merge: &MergeStat, inner: &mut Inner<D>) {
    inner.running_merges.remove(merge);
  }
  fn take_processed_merges(&self) -> Vec<OneMerge<D, CR>> {
    let mut processed_merges = self.processed_merges.lock();
    std::mem::take(&mut *processed_merges)
  }

  fn abort_pending_merges(&self, inner: &mut Inner<D>) -> Result<()> {
    let mut pending_merges = {
      let mut pending_merges = self.pending_merges.lock();
      std::mem::take(&mut *pending_merges)
    };
    let result = IOUtils::close(pending_merges.make_contiguous().iter_mut(), |merge| {
      if self.writer().info_stream.is_enabled("IW") {
        self
          .writer()
          .info_stream
          .message("IW", "now abort pending addIndexes merge")?;
      }
      merge.set_aborted()?;
      merge.close(inner, false, false, |_, _| Ok(()))?;
      self.on_merge_finished_locked(&merge.stat, inner);
      Ok(())
    });
    self.processed_merges.lock().extend(pending_merges);
    result
  }
}

impl<D, CR> MergeSource<D> for Arc<AddIndexesMergeSource<D, CR>>
where
  D: Directory + 'static,
  CR: CodecReader + Clone + 'static,
  OneMerge<D, CR>: Send + 'static,
{
  type Reader = CR;

  fn get_next_merge(&self) -> Result<Option<OneMerge<D, CR>>> {
    let mut inner = self.writer().inner.lock();
    if !self.has_pending_merges_locked(&inner) {
      return Ok(None);
    }
    let merge = self
      .pending_merges
      .lock()
      .pop_front()
      .ok_or_else(|| LuceneError::illegal_state("should have pending merges"))?;
    inner.running_merges.insert(merge.stat.clone());
    Ok(Some(merge))
  }

  fn on_merge_finished(&self, merge: &MergeStat, inner: Option<&mut Inner<D>>) {
    match inner {
      Some(inner) => self.on_merge_finished_locked(merge, inner),
      None => {
        let mut inner = self.writer().inner.lock();
        self.on_merge_finished_locked(merge, &mut inner);
      },
    }
  }

  fn has_pending_merges(&self, inner: Option<&mut Inner<D>>) -> Result<bool> {
    match inner {
      Some(inner) => Ok(self.has_pending_merges_locked(inner)),
      None => {
        let inner = self.writer().inner.lock();
        Ok(self.has_pending_merges_locked(&inner))
      },
    }
  }

  fn merge(&self, mut merge: OneMerge<D, CR>) -> Result<()>
  where
    D: 'static,
  {
    let mut success = false;
    let merge_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      self.writer().add_indexes_reader_merge(&mut merge)
    }));
    let result = match merge_result {
      Ok(Ok(())) => {
        success = true;
        Ok(Ok(()))
      },
      merge_result => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        self
          .writer()
          .handle_merge_exception(merge_result, &merge, None)
      })),
    };
    {
      let mut inner = self.writer().inner.lock();
      let mut processed_merges = self.processed_merges.lock();
      processed_merges.push(merge);
      let merge = processed_merges.last_mut().unwrap();
      merge.close(&mut inner, success, false, |_, _| Ok(()))?;
      self.on_merge_finished_locked(&merge.stat, &mut inner);
    }
    unwrap_caught_result!(result)
  }
}
/// DocModifier trait — equivalent to Java's private interface `DocModifier`
/// in `IndexWriter`.
pub(crate) trait DocModifier {
  fn run<D>(
    &self,
    doc_id: i32,
    info_id: &str,
    readers_and_updates: &ReadersAndUpdates<D>,
    writer: &IndexWriter<D>,
    inner: &mut Inner<D>,
  ) -> Result<()>
  where
    D: Directory;
}
#[derive(Default)]
struct DocModifierImpl1;
impl DocModifier for DocModifierImpl1 {
  fn run<D>(
    &self,
    left_doc_id: i32,
    info_id: &str,
    readers_and_updates: &ReadersAndUpdates<D>,
    writer: &IndexWriter<D>,
    inner: &mut Inner<D>,
  ) -> Result<()>
  where
    D: Directory,
  {
    let info = inner
      .segment_infos
      .index_of(info_id)
      .ok_or_else(|| LuceneError::illegal_argument(format!("invalid info id: {info_id}")))?;
    if readers_and_updates.delete(left_doc_id, info, None)? {
      if writer.is_fully_deleted(readers_and_updates, info, inner)? {
        writer.drop_deleted_segment(readers_and_updates.get_info_id(), inner)?;
        writer.checkpoint(inner)?;
      }
      // Must bump changeCount so if no other changes
      // happened, we still commit this change:
      changed(&mut inner.change_count, &mut inner.segment_infos);
    }

    Ok(())
  }
}

/// DocModifierImpl2: applies doc values updates to a document, following the Java tryUpdateDocValue lambda.
struct DocModifierImpl2 {
  dv_updates: Vec<DocValuesUpdate>,
}

impl DocModifier for DocModifierImpl2 {
  fn run<D>(
    &self,
    leaf_doc_id: i32,
    info_id: &str,
    readers_and_updates: &ReadersAndUpdates<D>,
    writer: &IndexWriter<D>,
    inner: &mut Inner<D>,
  ) -> Result<()>
  where
    D: Directory,
  {
    let next_gen = writer.buffered_updates_stream.get_next_gen();

    let max_doc = {
      let info = inner
        .segment_infos
        .index_of(info_id)
        .ok_or_else(|| LuceneError::illegal_argument(format!("invalid info id: {info_id}")))?;
      info.info.max_doc()?
    };

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      let mut field_updates_map: HashMap<
        String,
        DocValuesFieldUpdates<DocValuesFieldUpdatesBaseEnum>,
      > = HashMap::new();

      for update in &self.dv_updates {
        let sub: DocValuesFieldUpdatesBaseEnum = match update.doc_values_type {
          DocValuesType::Numeric => NumericDocValuesFieldUpdates::new()?.into(),
          DocValuesType::Binary => BinaryDocValuesFieldUpdates::new()?.into(),
          _ => {
            return Err(LuceneError::unsupported_operation(format!(
              "typ: {} is not supported",
              update.doc_values_type
            )));
          },
        };
        let doc_values_field_updates = field_updates_map
          .entry(update.field.clone())
          .or_insert_with(|| {
            DocValuesFieldUpdates::new(max_doc, next_gen, update.field.clone(), sub.sub_type(), sub)
              .unwrap()
          });

        if update.has_value() {
          match &update.sub_update {
            DocValuesUpdateEnum::Numeric(n) => {
              doc_values_field_updates.add_value(leaf_doc_id, n.get_value())?;
            },
            DocValuesUpdateEnum::Binary(b) => {
              doc_values_field_updates.add_byte_ref(leaf_doc_id, b.get_value())?;
            },
          }
        } else {
          doc_values_field_updates.reset(leaf_doc_id)?;
        }
      }

      for mut updates in field_updates_map.into_values() {
        updates.finish()?;
        readers_and_updates.add_dv_update(updates)?;
      }
      Ok(())
    }));

    writer.buffered_updates_stream.finished_segment(next_gen)?;

    unwrap_caught_result!(result)?;
    // Must bump changeCount so if no other changes
    // happened, we still commit this change:
    changed(&mut inner.change_count, &mut inner.segment_infos);

    Ok(())
  }
}

pub(crate) enum PointInTimeMergeFinished<D>
where
  D: Directory,
{
  NoOp,
  GetReader(Arc<PointInTimeGetReaderMergeFinished<D>>),
}

pub(crate) struct PointInTimeGetReaderMergeFinished<D>
where
  D: Directory,
{
  reader_factory: IOFunctionImpl<D>,
  merged_readers: Arc<Mutex<HashMap<String, DefaultLeafReader<D>>>>,
  stop_collecting_merged_readers: Arc<AtomicBool>,
}

impl<D> Clone for PointInTimeMergeFinished<D>
where
  D: Directory,
{
  fn clone(&self) -> Self {
    match self {
      Self::NoOp => Self::NoOp,
      Self::GetReader(merge_finished) => Self::GetReader(merge_finished.clone()),
    }
  }
}

impl<D> Clone for PointInTimeGetReaderMergeFinished<D>
where
  D: Directory,
{
  fn clone(&self) -> Self {
    Self {
      reader_factory: self.reader_factory.clone(),
      merged_readers: self.merged_readers.clone(),
      stop_collecting_merged_readers: self.stop_collecting_merged_readers.clone(),
    }
  }
}

impl<D> PointInTimeGetReaderMergeFinished<D>
where
  D: Directory,
{
  pub(crate) fn new(
    reader_factory: IOFunctionImpl<D>,
    merged_readers: Arc<Mutex<HashMap<String, DefaultLeafReader<D>>>>,
    stop_collecting_merged_readers: Arc<AtomicBool>,
  ) -> Self {
    Self {
      reader_factory,
      merged_readers,
      stop_collecting_merged_readers,
    }
  }

  fn accept(&self, inner: &mut Inner<D>, sci: &SegmentCommitInfo<D>) -> Result<()> {
    debug_assert!(
      !self.stop_collecting_merged_readers.load(Ordering::SeqCst),
      "illegal state  merge reader must be not pulled since we already stopped waiting for merges"
    );
    let mut reader_factory = self.reader_factory.clone();
    let apply = reader_factory.apply(sci, inner)?;
    self
      .merged_readers
      .lock()
      .insert(sci.info.name.clone(), apply);
    // We need to incRef the files of the opened SegmentReader otherwise it's possible that another
    // merge removes the segment before we pass it on to the StandardDirectoryReader.
    inner.deleter.inc_ref_files(sci.files()?)?;
    Ok(())
  }
}

impl<D> PointInTimeMergeFinished<D>
where
  D: Directory,
{
  fn accept(&self, inner: &mut Inner<D>, info: &SegmentCommitInfo<D>) -> Result<()> {
    match self {
      Self::NoOp => Ok(()),
      Self::GetReader(merge_finished) => merge_finished.accept(inner, info),
    }
  }
}

pub struct PointInTimeOneMerge<D, CR>
where
  D: Directory,
  CR: CodecReader,
{
  wrapped: Box<OneMergeHook<D, CR>>,
  merging_segment_infos: Arc<Mutex<SegmentInfos<D>>>,
  stop_collecting_merge_results: Arc<AtomicBool>,
  trigger: MergeTrigger,
  merge_finished: PointInTimeMergeFinished<D>,
  orig_info: Mutex<Option<SegmentCommitInfo<D>>>,
  only_once: AtomicBool,
}
impl<D, CR> Clone for PointInTimeOneMerge<D, CR>
where
  D: Directory,
  CR: CodecReader,
{
  fn clone(&self) -> Self {
    Self {
      wrapped: Box::new(OneMergeHook::Default),
      merging_segment_infos: self.merging_segment_infos.clone(),
      stop_collecting_merge_results: self.stop_collecting_merge_results.clone(),
      trigger: self.trigger,
      merge_finished: self.merge_finished.clone(),
      orig_info: Mutex::new(None),
      only_once: AtomicBool::new(false),
    }
  }
}
impl<D> OneMergeUnaryOperatorBase<D> for PointInTimeOneMerge<D, DefaultLeafReader<D>>
where
  D: Directory,
{
  fn apply(&self, mut merge: OneMergeSR<D>) -> Result<OneMergeSR<D>> {
    let mut new_hook = self.clone();
    let to_wrap = merge.replace_hook(OneMergeHook::Default);
    new_hook.set_wrap(to_wrap);
    merge.replace_hook(OneMergeHook::PointInTime(Box::new(new_hook)));
    Ok(merge)
  }
}

impl<D, CR> PointInTimeOneMerge<D, CR>
where
  D: Directory,
  CR: CodecReader,
{
  pub(crate) fn new(
    merging_segment_infos: Arc<Mutex<SegmentInfos<D>>>,
    stop_collecting_merge_results: Arc<AtomicBool>,
    trigger: MergeTrigger,
    merge_finished: PointInTimeMergeFinished<D>,
  ) -> Self {
    Self {
      wrapped: Box::new(OneMergeHook::default()),
      merging_segment_infos,
      stop_collecting_merge_results,
      trigger,
      merge_finished,
      orig_info: Mutex::new(None),
      only_once: AtomicBool::new(false),
    }
  }

  pub(crate) fn set_wrap(&mut self, to_wrap: OneMergeHook<D, CR>) {
    *self.wrapped = to_wrap;
  }
}

impl<D, CR> OneMergeBase<D, CR> for PointInTimeOneMerge<D, CR>
where
  D: Directory,
  CR: CodecReader,
{
  fn merge_finished(
    &self,
    inner: &mut Inner<D>,
    stat: &MergeStat,
    success: bool,
    segment_dropped: bool,
  ) -> Result<()> {
    if !segment_dropped && success && !self.stop_collecting_merge_results.load(Ordering::SeqCst) {
      let orig_info = self
        .orig_info
        .lock()
        .clone()
        .ok_or_else(|| LuceneError::illegal_state("point-in-time merge origInfo is none"))?;

      if self.trigger == MergeTrigger::Commit {
        inner.deleter.inc_ref_files(orig_info.files()?)?;
      }

      let merged_segment_ids: HashSet<String> = stat.segments.iter().cloned().collect();
      let mut to_commit_merged_away_segments = Vec::new();
      {
        let mut merging_segment_infos = self.merging_segment_infos.lock();
        for sci in merging_segment_infos.iter() {
          if merged_segment_ids.contains(sci.info.get_id_key()) {
            to_commit_merged_away_segments.push(SegmentDocAndID::new(
              sci.info.get_id_key().to_string(),
              sci.info.max_doc()?,
            ));
            if self.trigger == MergeTrigger::Commit {
              let files = sci.files()?;
              inner.deleter.dec_ref(files.iter())?;
            }
          }
        }

        let mut applicable_merge = OneMerge::<D, CR>::new(to_commit_merged_away_segments)?;
        applicable_merge.info = Some(orig_info.clone());
        let segment_counter = i64::from_str_radix(orig_info.info.name.trim_start_matches('_'), 36)?;
        merging_segment_infos.counter =
          std::cmp::max(merging_segment_infos.counter, segment_counter + 1);
        merging_segment_infos.apply_merge_changes(&mut applicable_merge, false)?;
      }
    }

    self
      .wrapped
      .merge_finished(inner, stat, success, segment_dropped)?;
    OneMergeDefaults::merge_finished(inner, stat, success, segment_dropped)
  }

  fn wrap_for_merge(&self, reader: CR) -> Result<CR> {
    self.wrapped.wrap_for_merge(reader)
  }

  fn reorder<CR1, D1>(&self, reader: &CR1, dir: D1) -> Result<Option<DummyDocMap>>
  where
    CR1: CodecReader,
    D1: Directory,
  {
    self.wrapped.reorder(reader, dir)
  }

  fn set_merge_info(
    &self,
    stat: &MergeStat,
    merge_info: &mut Option<SegmentCommitInfo<D>>,
    info: SegmentCommitInfo<D>,
  ) {
    self.wrapped.set_merge_info(stat, merge_info, info);
  }

  fn on_merge_complete(
    &self,
    inner: &mut Inner<D>,
    stat: &MergeStat,
    merge_info: &Option<SegmentCommitInfo<D>>,
    is_aborted: bool,
  ) -> Result<()> {
    let info = merge_info
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("point-in-time merge info is none"))?;
    if !self.stop_collecting_merge_results.load(Ordering::SeqCst)
      && !is_aborted
      && info.info.max_doc()? > 0
    {
      self.merge_finished.accept(inner, info)?;
      *self.orig_info.lock() = Some(info.clone());
    }
    self
      .wrapped
      .on_merge_complete(inner, stat, merge_info, is_aborted)?;
    OneMergeDefaults::on_merge_complete(inner, stat, merge_info, is_aborted)
  }

  fn init_merge_readers<F>(
    &self,
    merge_readers: &mut Vec<MergeReader<CR>>,
    stat: &MergeStat,
    reader_factory: F,
  ) -> Result<()>
  where
    F: FnMut(&String) -> Result<MergeReader<CR>>,
  {
    if self
      .only_once
      .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
      .is_ok()
    {
      OneMergeDefaults::init_merge_readers(merge_readers, stat, reader_factory)
    } else {
      Ok(())
    }
  }
}

#[cfg(test)]
pub(crate) mod tests {
  use super::*;
  use std::sync::LazyLock;

  pub(crate) struct IndexWriterAccessImpl;

  pub(crate) static INDEX_WRITER_ACCESS: LazyLock<IndexWriterAccessImpl> =
    LazyLock::new(|| IndexWriterAccessImpl);

  impl IndexWriterAccess for IndexWriterAccessImpl {
    fn seg_string<D>(&self, iw: &IndexWriter<D>) -> Result<String>
    where
      D: Directory,
    {
      iw.seg_string(None)
    }

    fn get_segment_count<D>(&self, iw: &IndexWriter<D>) -> usize
    where
      D: Directory,
    {
      iw.get_segment_count()
    }

    fn is_closed<D>(&self, iw: &IndexWriter<D>) -> bool
    where
      D: Directory,
    {
      iw.closed.load(Ordering::SeqCst)
    }

    fn get_reader<D>(
      &self,
      iw: &Arc<IndexWriter<D>>,
      apply_deletions: bool,
      write_all_deletes: bool,
    ) -> Result<StandardDirectoryReader<D>>
    where
      D: Directory + 'static,
    {
      iw.get_reader(apply_deletions, write_all_deletes)
    }

    fn get_doc_writer_thread_pool_size<D>(&self, iw: &IndexWriter<D>) -> usize
    where
      D: Directory,
    {
      iw.doc_writer.flush_control.per_thread_pool.size()
    }

    fn is_deleter_closed<D>(&self, iw: &IndexWriter<D>) -> Result<bool>
    where
      D: Directory,
    {
      iw.is_deleter_closed()
    }

    fn newest_segment<D>(&self, iw: &IndexWriter<D>) -> Option<SegmentCommitInfo<D>>
    where
      D: Directory,
    {
      iw.newest_segment()
    }
  }
}
enum RegisterMergeResult<D>
where
  D: Directory,
{
  Registered(MergeStat),
  Rejected(Box<OneMergeSR<D>>),
}

#[derive(Default)]
struct UpdatePendingMergesResult<D>
where
  D: Directory,
{
  /// Handles for merges that were successfully registered and moved to `pending_merges`.
  registered_merges: Vec<MergeStat>,
  /// Merges returned by the merge policy that could not be registered.
  rejected_merges: Vec<OneMergeSR<D>>,
}

impl<D> UpdatePendingMergesResult<D>
where
  D: Directory,
{
  fn for_each_merge_mut<F>(&mut self, inner: &mut Inner<D>, mut consumer: F) -> Result<()>
  where
    F: FnMut(&mut OneMergeSR<D>, &SegmentInfos<D>, &mut IndexFileDeleter<D>) -> Result<()>,
  {
    let pending_merges = &mut inner.pending_merges;
    let segment_infos = &inner.segment_infos;
    let deleter = &mut inner.deleter;

    for merge_stat in &self.registered_merges {
      let merge = pending_merges
        .iter_mut()
        .find(|merge| merge.stat.id.eq(&merge_stat.id))
        .ok_or_else(|| LuceneError::illegal_state("point-in-time merge is not pending"))?;
      consumer(merge, segment_infos, deleter)?;
    }

    for merge in &mut self.rejected_merges {
      consumer(merge, segment_infos, deleter)?;
    }

    Ok(())
  }

  fn for_each_merge_mut_removing_pending<F>(
    &mut self,
    inner: &mut Inner<D>,
    mut consumer: F,
  ) -> Result<()>
  where
    F: FnMut(&mut OneMergeSR<D>, &mut Inner<D>) -> Result<()>,
  {
    let registered_merge_count = self.registered_merges.len();
    let merge_count = registered_merge_count + self.rejected_merges.len();
    IOUtils::close(0..merge_count, |index| {
      if index < registered_merge_count {
        let merge_stat = &self.registered_merges[index];
        let position = inner
          .pending_merges
          .iter()
          .position(|merge| merge.stat.id.eq(&merge_stat.id))
          .ok_or_else(|| LuceneError::illegal_state("point-in-time merge is not pending"))?;
        let mut merge = inner
          .pending_merges
          .remove(position)
          .ok_or_else(|| LuceneError::illegal_state("point-in-time merge is not pending"))?;
        consumer(&mut merge, inner)
      } else {
        consumer(
          &mut self.rejected_merges[index - registered_merge_count],
          inner,
        )
      }
    })
  }

  fn await_merges(&self, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    for merge_stat in &self.registered_merges {
      if !merge_stat.await_until(deadline) {
        return false;
      }
    }

    for merge in &self.rejected_merges {
      if !merge.stat.await_until(deadline) {
        return false;
      }
    }

    true
  }

  fn merge_states(&self) -> Vec<MergeStat> {
    let mut merge_states =
      Vec::with_capacity(self.registered_merges.len() + self.rejected_merges.len());
    merge_states.extend(self.registered_merges.iter().cloned());
    merge_states.extend(self.rejected_merges.iter().map(|merge| merge.stat.clone()));
    merge_states
  }
}
