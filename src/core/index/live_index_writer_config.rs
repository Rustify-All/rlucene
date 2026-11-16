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
use crate::core::analysis::analyzer::AnalyzerEnum;
use crate::core::codecs::{Codecs, codec};
use crate::core::index::flush_by_ram_or_counts_policy::FlushByRamOrCountsPolicy;
use crate::core::index::flush_policy::FlushPolicyEnum;
use crate::core::index::index_deletion_policy::IndexDeletionPolicyEnum;
use crate::core::index::index_writer::IndexReaderWarmerEnum;
use crate::core::index::index_writer_config::{
  DEFAULT_COMMIT_ON_CLOSE, DEFAULT_MAX_BUFFERED_DOCS, DEFAULT_MAX_FULL_FLUSH_MERGE_WAIT_MILLIS,
  DEFAULT_RAM_BUFFER_SIZE_MB, DEFAULT_RAM_PER_THREAD_HARD_LIMIT_MB, DEFAULT_READER_POOLING,
  DEFAULT_USE_COMPOUND_FILE_SYSTEM, OpenMode,
};
use crate::core::index::index_writer_event_listener::{
  IndexWriterEventListenerEnum, NoOpIndexWriterEventListener,
};
use crate::core::index::keep_only_last_commit_deletion_policy::KeepOnlyLastCommitDeletionPolicy;
use crate::core::index::merge_policy::MergePolicyEnum;
use crate::core::index::merge_scheduler::MergeSchedulerEnum;
use crate::core::index::tiered_merge_policy::TieredMergePolicy;
use crate::core::search::sort::Sort;
use crate::core::store::directory::Directory;
use crate::core::util::LATEST;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::info_stream::{InfoStreamMT, get_default_info_stream};
use parking_lot::{MappedRwLockWriteGuard, RwLock};
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU64, Ordering};

use crate::core::index::segment_reader::DefaultLeafReader;
use crate::core::util::comparator::Comparator;

/// Holds all configuration used by [`IndexWriter`](crate::core::index::index_writer::IndexWriter), with a small set of setters
/// for settings that can be changed on an [`IndexWriter`](crate::core::index::index_writer::IndexWriter) instance live.
pub trait LiveIndexWriterConfig: Display {
  type Directory: Directory;

  /// Returns the default analyzer to use for indexing documents.
  fn get_analyzer(&self) -> &AnalyzerEnum;

  /// Returns the [`MergeSchedulerEnum`] set on this configuration.
  fn get_merge_scheduler(&self) -> &MergeSchedulerEnum;

  /// Returns the current [`Codecs`] implementation.
  fn get_codec(&self) -> &Codecs;

  /// Gets the index-time [`Sort`] order applied to all flushed and merged
  /// segments.
  fn get_index_sort(&self) -> Option<&Arc<Sort>>;
  /// Returns the field names involved in the index sort.
  fn get_index_sort_fields(&self) -> &HashSet<String>;

  /// Returns the comparator used to order leaf readers.
  fn get_leaf_sorter(&self) -> Option<&LeafSorter<Self::Directory>>;
  /// Returns `true` iff newly written segments are packed in a compound file.
  ///
  /// The default is `true`.
  fn get_use_compound_file(&self) -> bool;

  /// Returns the soft deletes field, or `None` if soft deletes are disabled.
  fn get_soft_deletes_field(&self) -> Option<&String>;

  /// Returns the [`InfoStreamMT`] used for debugging.
  fn get_info_stream(&self) -> InfoStreamMT;

  /// Returns the parent document field name if configured.
  fn get_parent_field(&self) -> Option<&String>;

  /// Returns the current [`MergePolicyEnum`] in use by this writer.
  ///
  /// The returned handle keeps that policy alive if another thread replaces it.
  /// The configuration lock is released before the policy is used.
  fn get_merge_policy(&self) -> Arc<MergePolicyEnum<Self::Directory>>;
  /// Returns mutable access to the current [`MergePolicyEnum`].
  ///
  /// If a reader retains the old policy, mutation uses a private clone. Release
  /// this guard before calling writer operations that read the merge policy.
  fn get_merge_policy_mut(&self) -> MappedRwLockWriteGuard<'_, MergePolicyEnum<Self::Directory>>;

  /// Returns the configured flushing behavior. The default is
  /// [`FlushByRamOrCountsPolicy`].
  fn get_flush_policy(&self) -> &FlushPolicyEnum;

  /// Returns the RAM buffer size in MB if enabled.
  fn get_ram_buffer_size_mb(&self) -> f64;

  /// Returns the max amount of memory each documents writer thread can consume
  /// before it is forcefully flushed.
  fn get_ram_per_thread_hard_limit_mb(&self) -> i32;

  /// Returns the number of buffered added documents that will trigger a flush if
  /// document-count flushing is enabled.
  fn get_max_buffered_docs(&self) -> i32;

  /// Expert: returns whether indexing threads check for pending flushes on
  /// update in order to help flush indexing buffers to disk.
  ///
  /// Experimental: this API follows the original Lucene experimental status.
  fn get_check_pending_flush_on_update(&self) -> bool;

  /// Expert: sets whether indexing threads check for pending flushes on update
  /// in order to help flush indexing buffers to disk.
  ///
  /// As a consequence, threads that open a changed reader or flush the writer
  /// will be the only threads writing segments to disk unless flushes are
  /// falling behind. If indexing is stalled due to too many pending flushes,
  /// indexing threads will help write pending segment flushes to disk.
  ///
  /// Experimental: this API follows the original Lucene experimental status.
  fn set_check_pending_flush_update(&self, check_pending_flush_on_update: bool) -> &Self;

  /// Returns the [`IndexDeletionPolicyEnum`] specified on this configuration, or
  /// the default keep-only-last-commit deletion policy.
  fn get_index_deletion_policy(&self) -> &IndexDeletionPolicyEnum<Self::Directory>;

  /// Expert: returns the amount of time to wait for merges returned by
  /// [`MergePolicy::find_full_flush_merges`](crate::core::index::merge_policy::MergePolicy::find_full_flush_merges).
  ///
  /// If this time is reached, commit proceeds based on segments merged up to
  /// that point. The merges are not cancelled and may still run to completion
  /// independent of the commit.
  fn get_max_full_flush_merge_wait_millis(&self) -> i64;

  /// Returns the [`IndexWriterEventListenerEnum`] callback that tracks the key
  /// [`IndexWriter`](crate::core::index::index_writer::IndexWriter) operations.
  fn get_index_writer_event_listener(&self) -> &IndexWriterEventListenerEnum;

  /// Returns the current merged segment warmer.
  fn get_merged_segment_warmer(&self) -> Option<Arc<IndexReaderWarmerEnum<Self::Directory>>>;

  /// Returns `true` if [`IndexWriter::close`](crate::core::index::index_writer::IndexWriter::close) should first commit before closing.
  fn get_commit_on_close(&self) -> bool;

  /// Returns the [`OpenMode`] that [`IndexWriter`](crate::core::index::index_writer::IndexWriter) is opened with.
  fn get_open_mode(&self) -> &OpenMode;

  /// Returns the compatibility version to use for this index.
  fn get_index_created_version_major(&self) -> i32;

  /// Returns `true` if [`IndexWriter`](crate::core::index::index_writer::IndexWriter) should pool readers even if opening a
  /// reader from the writer has not been called.
  fn get_reader_pooling(&self) -> bool;

  /// Returns mutable access to the base live configuration storage.
  fn get_base_mut(&mut self) -> &mut LiveIndexWriterConfigBase<Self::Directory>;

  /// Determines the amount of RAM that may be used for buffering added
  /// documents and deletions before they are flushed to the directory.
  ///
  /// Generally, for faster indexing performance it is best to flush by RAM
  /// usage instead of document count and use as large a RAM buffer as possible.
  ///
  /// When this is set, the writer will flush whenever buffered documents and
  /// deletions use this much RAM. Pass [`DISABLE_AUTO_FLUSH`](crate::core::index::index_writer_config::DISABLE_AUTO_FLUSH) to prevent
  /// triggering a flush due to RAM usage. If flushing by document count is also
  /// enabled, the flush is triggered by whichever limit comes first.
  ///
  /// The maximum RAM limit is inherently determined by available memory. An
  /// [`IndexWriter`](crate::core::index::index_writer::IndexWriter) session can consume significantly more memory than this
  /// limit, since the limit only indicates when to flush memory-resident
  /// documents. Flushes may happen concurrently while other threads add
  /// documents, so available memory should be significantly larger than the RAM
  /// buffer used for indexing.
  ///
  /// NOTE: RAM accounting for pending deletions is approximate. Deletes by query
  /// cannot measure individual query RAM usage, so accounting may
  /// underestimate; applications should compensate by committing or refreshing
  /// periodically if needed.
  ///
  /// NOTE: It is not guaranteed that all memory-resident documents are flushed
  /// once this limit is exceeded. Depending on the configured flush policy, only
  /// a subset of buffered documents may be flushed and only part of the RAM
  /// buffer released.
  ///
  /// The default value is [`DEFAULT_RAM_BUFFER_SIZE_MB`].
  ///
  /// Takes effect immediately, but only the next time a document is added,
  /// updated, or deleted.
  fn set_ram_buffer_size_mb(&self, ram_buffer_size_mb: f64) -> &Self;

  /// Determines the minimal number of documents required before buffered
  /// in-memory documents are flushed as a new segment.
  ///
  /// Large values generally give faster indexing. When this is set, the writer
  /// flushes every `max_buffered_docs` added documents. Pass
  /// [`DISABLE_AUTO_FLUSH`](crate::core::index::index_writer_config::DISABLE_AUTO_FLUSH) to prevent triggering a flush due to the number of
  /// buffered documents. If flushing by RAM usage is also enabled, the flush is
  /// triggered by whichever limit comes first.
  ///
  /// Disabled by default because the writer flushes by RAM usage.
  ///
  /// Takes effect immediately, but only the next time a document is added,
  /// updated, or deleted.
  fn set_max_buffered_docs(&self, max_buffered_docs: i32) -> &Self;

  /// Sets whether the [`IndexWriter`](crate::core::index::index_writer::IndexWriter) should pack newly written segments in a
  /// compound file.
  ///
  /// The default is `true`. Use `false` for batch indexing with very large RAM
  /// buffer settings.
  ///
  /// NOTE: To control compound file usage during segment merges, use the
  /// corresponding merge policy settings. This setting only applies to newly
  /// created segments.
  fn set_use_compound_file(&self, use_compound_file: bool) -> &Self;

  /// Expert: sets the [`MergePolicyEnum`] used whenever there are changes to
  /// the segments in the index.
  ///
  /// The merge policy selects which merges to do, if any, and also selects
  /// merges for force merge.
  ///
  /// Takes effect on subsequent merge selections. Any merges in flight or
  /// already registered by the previous merge policy are not affected.
  fn set_merge_policy<T>(&self, merge_policy: T) -> &Self
  where
    T: Into<MergePolicyEnum<Self::Directory>>;

  /// Sets the flushing behavior. The default is [`FlushByRamOrCountsPolicy`].
  fn set_flush_policy<T>(&mut self, flush_policy: T) -> &mut Self
  where
    T: Into<FlushPolicyEnum>,
  {
    let v = flush_policy.into();
    self.get_base_mut().flush_policy = Arc::new(v);
    self
  }

  /// Sets the [`InfoStreamMT`] used for debugging.
  fn set_info_stream<T>(&mut self, info_stream: T) -> &mut Self
  where
    T: Into<InfoStreamMT>,
  {
    self.get_base_mut().info_stream = info_stream.into();
    self
  }

  /// Sets the merged segment warmer.
  ///
  /// Takes effect on the next merge.
  fn set_merged_segment_warmer(
    &self,
    merge_segment_warmer: Option<IndexReaderWarmerEnum<Self::Directory>>,
  ) -> &Self;
}

/// Leaf sorter for sorting leaf readers.
///
/// Implements [`Comparator<DefaultLeafReader<D>>`] via a closure held in
/// the `Custom` variant.
pub enum LeafSorter<D>
where
  D: Directory,
{
  /// A custom comparator provided by the user.
  ///
  /// Uses `Arc<dyn Fn>` because [`Comparator`] is not dyn-compatible
  /// (it has an associated constant `TYPE`).
  Custom(Arc<LeafReaderComparator<D>>),
}
impl<D> Comparator<DefaultLeafReader<D>> for LeafSorter<D>
where
  D: Directory,
{
  const TYPE: &'static str = "LeafSorter";

  fn compare(&self, a: &DefaultLeafReader<D>, b: &DefaultLeafReader<D>) -> Result<i32> {
    match self {
      LeafSorter::Custom(c) => c(a, b),
    }
  }
}

impl<D> Clone for LeafSorter<D>
where
  D: Directory,
{
  fn clone(&self) -> Self {
    match self {
      LeafSorter::Custom(c) => LeafSorter::Custom(Arc::clone(c)),
    }
  }
}

impl<D> LeafSorter<D>
where
  D: Directory,
{
  /// Creates a new [`LeafSorter`] with a custom comparator.
  pub fn custom(comparator: Arc<LeafReaderComparator<D>>) -> Self {
    LeafSorter::Custom(comparator)
  }
}

type LeafReaderComparator<D> =
  dyn Fn(&DefaultLeafReader<D>, &DefaultLeafReader<D>) -> Result<i32> + Send + Sync;
/// Storage for live index writer configuration values.
///
/// These fields mirror the live configuration state that an [`IndexWriter`](crate::core::index::index_writer::IndexWriter) reads
/// while indexing and merging.
pub struct LiveIndexWriterConfigBase<D>
where
  D: Directory,
{
  /// Default analyzer to use for indexing documents.
  pub analyzer: AnalyzerEnum,
  /// RAM buffer size in MB, stored as atomic `f64::to_bits()` for live reads.
  pub ram_buffer_size_mb: AtomicU64,
  /// Number of buffered added documents that triggers a flush.
  pub max_buffered_docs: AtomicI32,
  /// [`IndexDeletionPolicyEnum`] controlling when commit points are deleted.
  pub index_deletion_policy: IndexDeletionPolicyEnum<D>,
  /// True if newly written segment flushes should use compound file format.
  pub use_compound_file: AtomicBool,
  /// [`OpenMode`] that [`IndexWriter`](crate::core::index::index_writer::IndexWriter) is opened with.
  pub open_mode: OpenMode,
  /// [`Codecs`] implementation used to write new segments.
  pub codec: Codecs,
  /// [`InfoStreamMT`] for debugging messages.
  pub info_stream: InfoStreamMT,
  /// [`MergePolicyEnum`] for selecting merges.
  pub merge_policy: RwLock<Arc<MergePolicyEnum<D>>>,
  /// Flushing behavior; the default is [`FlushByRamOrCountsPolicy`].
  pub flush_policy: Arc<FlushPolicyEnum>,
  /// True if readers should be pooled.
  pub reader_pooling: bool,
  /// Hard upper bound on RAM usage for a single thread, after which the segment
  /// is forced to flush.
  pub per_thread_hard_limit_mb: i32,
  /// Compatibility version to use for this index.
  pub created_version_major: i32,
  /// Soft deletes field, or `None` if soft deletes are disabled.
  pub soft_deletes_field: Option<String>,
  /// Amount of time to wait for merges returned by full-flush merge selection.
  pub max_full_flush_merge_wait_millis: AtomicI64,
  /// [`IndexWriterEventListenerEnum`] for recording key [`IndexWriter`](crate::core::index::index_writer::IndexWriter) events.
  pub event_listener: IndexWriterEventListenerEnum,
  /// Warmer called for newly merged segments before they are committed.
  pub merged_segment_warmer: RwLock<Option<Arc<IndexReaderWarmerEnum<D>>>>,
  /// True if calls to [`IndexWriter::close`](crate::core::index::index_writer::IndexWriter::close) should first do a commit.
  pub commit_on_close: bool,
  /// True if an indexing thread should check for pending flushes on update in
  /// order to help with a full flush.
  pub check_pending_flush_on_update: AtomicBool,
  /// Parent document field name.
  pub parent_field: Option<String>,
  /// Sort order to use to write merged segments.
  pub index_sort: Option<Arc<Sort>>,
  /// Field names involved in the index sort.
  pub index_sort_fields: HashSet<String>,
  /// [`MergeSchedulerEnum`] to use for running merges.
  pub merge_scheduler: MergeSchedulerEnum,
  /// Comparator for sorting leaf readers.
  pub leaf_sorter: Option<LeafSorter<D>>,
}
impl<D> LiveIndexWriterConfigBase<D>
where
  D: Directory,
{
  pub fn with_analyzer<T>(analyzer: T) -> Result<Self>
  where
    T: Into<AnalyzerEnum>,
  {
    Ok(Self {
      ram_buffer_size_mb: AtomicU64::new(DEFAULT_RAM_BUFFER_SIZE_MB.to_bits()),
      max_buffered_docs: AtomicI32::new(DEFAULT_MAX_BUFFERED_DOCS),
      index_deletion_policy: KeepOnlyLastCommitDeletionPolicy.into(),
      use_compound_file: AtomicBool::new(DEFAULT_USE_COMPOUND_FILE_SYSTEM),
      open_mode: OpenMode::CreateOrAppend,
      codec: codec::get_default(),
      info_stream: get_default_info_stream(),
      merge_policy: RwLock::new(Arc::new(MergePolicyEnum::Tiered(
        TieredMergePolicy::default(),
      ))),
      flush_policy: Arc::new(FlushByRamOrCountsPolicy::new().into()),
      reader_pooling: DEFAULT_READER_POOLING,
      per_thread_hard_limit_mb: DEFAULT_RAM_PER_THREAD_HARD_LIMIT_MB,
      created_version_major: LATEST.major,
      soft_deletes_field: None,
      max_full_flush_merge_wait_millis: AtomicI64::new(DEFAULT_MAX_FULL_FLUSH_MERGE_WAIT_MILLIS),
      event_listener: NoOpIndexWriterEventListener.into(),
      merged_segment_warmer: RwLock::new(None),
      commit_on_close: DEFAULT_COMMIT_ON_CLOSE,
      check_pending_flush_on_update: AtomicBool::new(true),
      parent_field: None,
      index_sort: None,
      index_sort_fields: HashSet::new(),
      merge_scheduler: MergeSchedulerEnum::default(),
      leaf_sorter: None,
      analyzer: analyzer.into(),
    })
  }
  pub fn new() -> Result<Self> {
    Self::with_analyzer(AnalyzerEnum::default())
  }

  pub fn get_flush_policy(&self) -> &FlushPolicyEnum {
    &self.flush_policy
  }
}

impl<D: Directory> Display for LiveIndexWriterConfigBase<D> {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "{}",
      std::any::type_name::<crate::core::index::index_writer_config::IndexWriterConfig<D>>()
    )
  }
}

impl<D> LiveIndexWriterConfig for LiveIndexWriterConfigBase<D>
where
  D: Directory,
{
  type Directory = D;

  fn get_analyzer(&self) -> &AnalyzerEnum {
    &self.analyzer
  }

  fn get_merge_scheduler(&self) -> &MergeSchedulerEnum {
    &self.merge_scheduler
  }

  fn get_codec(&self) -> &Codecs {
    &self.codec
  }

  fn get_index_sort(&self) -> Option<&Arc<Sort>> {
    self.index_sort.as_ref()
  }

  fn get_index_sort_fields(&self) -> &HashSet<String> {
    &self.index_sort_fields
  }

  fn get_leaf_sorter(&self) -> Option<&LeafSorter<D>> {
    self.leaf_sorter.as_ref()
  }

  fn get_use_compound_file(&self) -> bool {
    self.use_compound_file.load(Ordering::SeqCst)
  }

  fn get_soft_deletes_field(&self) -> Option<&String> {
    self.soft_deletes_field.as_ref()
  }

  fn get_info_stream(&self) -> InfoStreamMT {
    self.info_stream.clone()
  }

  fn get_parent_field(&self) -> Option<&String> {
    self.parent_field.as_ref()
  }

  fn get_merge_policy(&self) -> Arc<MergePolicyEnum<D>> {
    Arc::clone(&self.merge_policy.read())
  }
  fn get_merge_policy_mut(&self) -> parking_lot::MappedRwLockWriteGuard<'_, MergePolicyEnum<D>> {
    parking_lot::RwLockWriteGuard::map(self.merge_policy.write(), Arc::make_mut)
  }

  fn set_merge_policy<T>(&self, merge_policy: T) -> &Self
  where
    T: Into<MergePolicyEnum<D>>,
  {
    let merge_policy = Arc::new(merge_policy.into());
    let previous = std::mem::replace(&mut *self.merge_policy.write(), merge_policy);
    drop(previous);
    self
  }
  fn get_flush_policy(&self) -> &FlushPolicyEnum {
    &self.flush_policy
  }

  fn get_ram_buffer_size_mb(&self) -> f64 {
    f64::from_bits(self.ram_buffer_size_mb.load(Ordering::SeqCst))
  }

  fn set_ram_buffer_size_mb(&self, ram_buffer_size_mb: f64) -> &Self {
    self
      .ram_buffer_size_mb
      .store(ram_buffer_size_mb.to_bits(), Ordering::SeqCst);
    self
  }

  fn get_ram_per_thread_hard_limit_mb(&self) -> i32 {
    self.per_thread_hard_limit_mb
  }

  fn get_max_buffered_docs(&self) -> i32 {
    self.max_buffered_docs.load(Ordering::SeqCst)
  }

  fn set_max_buffered_docs(&self, max_buffered_docs: i32) -> &Self {
    self
      .max_buffered_docs
      .store(max_buffered_docs, Ordering::SeqCst);
    self
  }

  fn set_use_compound_file(&self, use_compound_file: bool) -> &Self {
    self
      .use_compound_file
      .store(use_compound_file, Ordering::SeqCst);
    self
  }

  fn get_check_pending_flush_on_update(&self) -> bool {
    self.check_pending_flush_on_update.load(Ordering::SeqCst)
  }

  fn set_check_pending_flush_update(&self, check_pending_flush_on_update: bool) -> &Self {
    self
      .check_pending_flush_on_update
      .store(check_pending_flush_on_update, Ordering::SeqCst);
    self
  }

  fn get_index_deletion_policy(&self) -> &IndexDeletionPolicyEnum<D> {
    &self.index_deletion_policy
  }

  fn get_max_full_flush_merge_wait_millis(&self) -> i64 {
    self.max_full_flush_merge_wait_millis.load(Ordering::SeqCst)
  }

  fn get_index_writer_event_listener(&self) -> &IndexWriterEventListenerEnum {
    &self.event_listener
  }

  fn get_merged_segment_warmer(&self) -> Option<Arc<IndexReaderWarmerEnum<D>>> {
    self.merged_segment_warmer.read().clone()
  }

  fn set_merged_segment_warmer(
    &self,
    merge_segment_warmer: Option<IndexReaderWarmerEnum<D>>,
  ) -> &Self {
    let warmer = merge_segment_warmer.map(Arc::new);
    let previous = std::mem::replace(&mut *self.merged_segment_warmer.write(), warmer);
    drop(previous);
    self
  }

  fn get_commit_on_close(&self) -> bool {
    self.commit_on_close
  }

  fn get_open_mode(&self) -> &OpenMode {
    &self.open_mode
  }

  fn get_index_created_version_major(&self) -> i32 {
    self.created_version_major
  }

  fn get_reader_pooling(&self) -> bool {
    self.reader_pooling
  }

  fn get_base_mut(&mut self) -> &mut LiveIndexWriterConfigBase<D> {
    self
  }
}
