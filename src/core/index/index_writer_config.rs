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
use crate::core::analysis::standard::standard_analyzer::StandardAnalyzer;
use crate::core::codecs::Codecs;
use crate::core::index::flush_policy::FlushPolicyEnum;
use crate::core::index::index_deletion_policy::IndexDeletionPolicyEnum;
use crate::core::index::index_writer::IndexReaderWarmerEnum;
use crate::core::index::index_writer_event_listener::IndexWriterEventListenerEnum;
use crate::core::index::live_index_writer_config::{
  LeafSorter, LiveIndexWriterConfig, LiveIndexWriterConfigBase,
};
use crate::core::index::merge_policy::MergePolicyEnum;
use crate::core::index::merge_scheduler::MergeSchedulerEnum;
use crate::core::search::sort::Sort;
use crate::core::search::sort_field::SortFiledBase;
use crate::core::store::directory::Directory;
use crate::core::util::LATEST;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::info_stream::InfoStreamMT;
use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Holds all configuration that is used to create an [`IndexWriter`](crate::core::index::index_writer::IndexWriter).
///
/// Once an [`IndexWriter`](crate::core::index::index_writer::IndexWriter) has been created with this object, changes to this
/// object will not affect that writer instance. For live changes, use the
/// [`LiveIndexWriterConfig`] returned from the writer configuration API.
///
/// Norm computation is selected per write operation with methods such as
/// [`IndexWriter::add_document_with_similarity`](crate::core::index::index_writer::IndexWriter::add_document_with_similarity).
/// The default write methods use the shared BM25 similarity.
///
/// All setter methods return [`IndexWriterConfig`] to allow settings to be
/// chained conveniently.
pub struct IndexWriterConfig<D>
where
  D: Directory,
{
  pub(crate) base: LiveIndexWriterConfigBase<D>,
}

impl<D> IndexWriterConfig<D>
where
  D: Directory,
{
  /// Creates a new config using the default analyzer.
  ///
  /// By default, [`TieredMergePolicy`](crate::core::index::tiered_merge_policy::TieredMergePolicy)
  /// is used for merging. This merge policy is free to select non-contiguous
  /// merges, which means doc IDs may not remain monotonic over time. If this is
  /// a problem, switch to a log-style merge policy.
  pub fn new() -> Result<Self> {
    Self::with_analyzer(StandardAnalyzer::new())
  }

  /// Creates a new config with the provided analyzer.
  ///
  /// By default, [`TieredMergePolicy`](crate::core::index::tiered_merge_policy::TieredMergePolicy)
  /// is used for merging. This merge policy is free to select non-contiguous
  /// merges, which means doc IDs may not remain monotonic over time. If this is
  /// a problem, switch to a log-style merge policy.
  pub fn with_analyzer<T>(analyzer: T) -> Result<Self>
  where
    T: Into<AnalyzerEnum>,
  {
    Ok(Self {
      base: LiveIndexWriterConfigBase::with_analyzer(analyzer)?,
    })
  }

  /// Sets the RAM buffer size while retaining the construction-time builder.
  pub fn set_ram_buffer_size_mb(&mut self, ram_buffer_size_mb: f64) -> &mut Self {
    LiveIndexWriterConfig::set_ram_buffer_size_mb(&self.base, ram_buffer_size_mb);
    self
  }

  /// Sets the document flush threshold while retaining the construction-time builder.
  pub fn set_max_buffered_docs(&mut self, max_buffered_docs: i32) -> &mut Self {
    LiveIndexWriterConfig::set_max_buffered_docs(&self.base, max_buffered_docs);
    self
  }

  /// Sets compound-file use while retaining the construction-time builder.
  pub fn set_use_compound_file(&mut self, use_compound_file: bool) -> &mut Self {
    LiveIndexWriterConfig::set_use_compound_file(&self.base, use_compound_file);
    self
  }

  /// Sets the merge policy while retaining the construction-time builder.
  pub fn set_merge_policy<T>(&mut self, merge_policy: T) -> &mut Self
  where
    T: Into<MergePolicyEnum<D>>,
  {
    LiveIndexWriterConfig::set_merge_policy(&self.base, merge_policy);
    self
  }

  /// Sets the flushing behavior.
  pub fn set_flush_policy<T>(&mut self, flush_policy: T) -> &mut Self
  where
    T: Into<FlushPolicyEnum>,
  {
    self.base.set_flush_policy(flush_policy);
    self
  }

  /// Sets the stream used for debugging messages.
  pub fn set_info_stream<T>(&mut self, info_stream: T) -> &mut Self
  where
    T: Into<InfoStreamMT>,
  {
    self.base.set_info_stream(info_stream);
    self
  }

  /// Sets if calls to [`IndexWriter::close`](crate::core::index::index_writer::IndexWriter::close) should first commit before closing.
  ///
  /// Use `true` to match the behavior of Lucene 4.x.
  pub fn set_commit_on_close(&mut self, commit_on_close: bool) -> &mut Self {
    self.base.commit_on_close = commit_on_close;
    self
  }

  /// Expert: sets the amount of time to wait for full-flush merges during
  /// commit or getting a reader from the writer.
  ///
  /// If this time is reached, commit proceeds based on segments merged up to
  /// that point. The merges are not aborted and will still run to completion
  /// independent of the commit or get-reader call, like natural segment merges.
  ///
  /// Set to `0` to disable merging on full flush. If a serial merge scheduler is
  /// used and a non-zero timeout is configured, full-flush merges always wait
  /// for the merge to finish without honoring the configured timeout.
  pub fn set_max_full_flush_merge_wait_millis(
    &mut self,
    max_full_flush_merge_wait_millis: i64,
  ) -> &mut Self {
    self
      .base
      .max_full_flush_merge_wait_millis
      .store(max_full_flush_merge_wait_millis, Ordering::SeqCst);
    self
  }

  /// Set the [`Codec`](crate::core::codecs::Codec).
  ///
  /// Only takes effect when [`IndexWriter`](crate::core::index::index_writer::IndexWriter) is first created.
  pub fn set_codec<T>(&mut self, codec: T) -> &mut Self
  where
    T: Into<Codecs>,
  {
    self.base.codec = codec.into();
    self
  }

  /// Specifies [`OpenMode`] of the index.
  ///
  /// Only takes effect when IndexWriter is first created.
  pub fn set_open_mode(&mut self, open_mode: OpenMode) -> &mut Self {
    self.base.open_mode = open_mode;
    self
  }

  /// Expert: sets the compatibility version to use for this index.
  ///
  /// If the index is created, it will use the given major version for
  /// compatibility. It is sometimes useful to set the previous major version for
  /// compatibility because adding indexes only accepts indexes written with the
  /// same major version as the current index. If the index already exists, this
  /// value is ignored. The default value is the major version of the latest
  /// version.
  ///
  /// NOTE: Changing the creation version reduces backward compatibility
  /// guarantees.
  pub fn set_index_created_version_major(
    &mut self,
    index_created_version_major: i32,
  ) -> Result<&mut Self> {
    if index_created_version_major > LATEST.major {
      return Err(LuceneError::illegal_argument(format!(
        "indexCreatedVersionMajor may not be in the future: current major version is {}, but got: {}",
        LATEST.major, index_created_version_major
      )));
    }

    if index_created_version_major < LATEST.major - 1 {
      return Err(LuceneError::illegal_argument(format!(
        "indexCreatedVersionMajor may not be less than the minimum supported version: {}, but got: {}",
        LATEST.major - 1,
        index_created_version_major
      )));
    }

    self.base.created_version_major = index_created_version_major;
    Ok(self)
  }

  /// Expert: allows an optional [`IndexDeletionPolicyEnum`] implementation to
  /// be specified.
  ///
  /// This controls when prior commits are deleted from the index. The default
  /// policy is keep-only-last-commit, which removes all prior commits as soon as
  /// a new commit is done. A custom policy can keep previous point-in-time
  /// commits alive for some time, allowing readers to refresh to the new commit
  /// without having the old commit deleted underneath them.
  ///
  /// This is necessary on filesystems that do not support delete-on-last-close
  /// semantics, which point-in-time search normally relies on.
  ///
  /// Only takes effect when [`IndexWriter`](crate::core::index::index_writer::IndexWriter) is first created.
  pub fn set_index_deletion_policy<T>(&mut self, deletion_policy: T) -> &mut Self
  where
    T: Into<IndexDeletionPolicyEnum<D>>,
  {
    self.base.index_deletion_policy = deletion_policy.into();
    self
  }

  /// Sets the [`Sort`] order to use for all flushed and merged segments.
  pub fn set_index_sort<T>(&mut self, sort: T) -> Result<&mut Self>
  where
    T: Into<Arc<Sort>>,
  {
    let sort = sort.into();
    for sort_field in sort.get_sort() {
      if sort_field.get_index_sorter()?.is_none() {
        return Err(LuceneError::illegal_argument(format!(
          "Cannot sort index with sort field {}",
          sort_field
        )));
      }
    }
    let index_sort_fields: HashSet<String> = sort
      .get_sort()
      .iter()
      .filter_map(|f| f.get_field())
      .map(str::to_string)
      .collect();
    self.base.index_sort_fields = index_sort_fields;
    self.base.index_sort = Some(sort);
    Ok(self)
  }

  /// Expert: sets the merge scheduler used by this writer.
  ///
  /// Only takes effect when [`IndexWriter`](crate::core::index::index_writer::IndexWriter) is first created.
  pub fn set_merge_scheduler<T>(&mut self, merge_scheduler: T) -> &mut Self
  where
    T: Into<MergeSchedulerEnum>,
  {
    let v = merge_scheduler.into();
    self.base.merge_scheduler = v;
    self
  }

  /// Sets the soft deletes field.
  ///
  /// A soft delete field is a doc-values field that marks a document as
  /// soft-deleted if the document has at least one value in that field. A
  /// soft-deleted document is treated as if it has been hard-deleted through the
  /// [`IndexWriter`](crate::core::index::index_writer::IndexWriter) API. Merges reclaim soft-deleted as well as hard-deleted
  /// documents, and index readers obtained from the writer reflect all deleted
  /// documents in their live docs.
  ///
  /// Soft deletes allow documents to be retained across merges if the merge
  /// policy modifies the live docs of a merge reader.
  ///
  /// There is currently no API support to undelete a soft-deleted document; it
  /// must be re-indexed.
  ///
  /// The default is `None`, which disables soft deletes. If soft deletes are
  /// enabled, documents can still be hard-deleted. Hard-deleted documents are
  /// not considered soft-deleted even if they have a value in the soft deletes
  /// field.
  pub fn set_soft_deletes_field<T>(&mut self, soft_deletes_field: T) -> &mut Self
  where
    T: Into<String>,
  {
    let v = soft_deletes_field.into();
    self.base.soft_deletes_field = Some(v);
    self
  }

  /// Set event listener to record key events in [`IndexWriter`](crate::core::index::index_writer::IndexWriter).
  pub fn set_index_writer_event_listener<T>(&mut self, event_listener: T) -> &mut Self
  where
    T: Into<IndexWriterEventListenerEnum>,
  {
    self.base.event_listener = event_listener.into();
    self
  }

  /// Set the merged segment warmer.
  ///
  /// Takes effect on the next merge.
  pub fn set_merged_segment_warmer(
    &mut self,
    merge_segment_warmer: Option<IndexReaderWarmerEnum<D>>,
  ) -> &mut Self {
    LiveIndexWriterConfig::set_merged_segment_warmer(&self.base, merge_segment_warmer);
    self
  }

  /// Returns the current merged segment warmer.
  pub fn get_merged_segment_warmer(&self) -> Option<Arc<IndexReaderWarmerEnum<D>>> {
    self.base.merged_segment_warmer.read().clone()
  }

  /// Sets the parent document field.
  ///
  /// If this optional property is set, [`IndexWriter`](crate::core::index::index_writer::IndexWriter) adds an internal field to
  /// every root document added to the index writer. A document is considered a
  /// parent document if it is the last document in a document block indexed via
  /// block document APIs, and individual documents added via single-document
  /// methods are also considered parent documents.
  ///
  /// This property is optional for indexes that do not use document blocks in
  /// combination with index sorting. In order to maintain the API guarantee that
  /// document order within a block is not altered by [`IndexWriter`](crate::core::index::index_writer::IndexWriter), a marker for
  /// parent documents is required.
  pub fn set_parent_field<T>(&mut self, parent_field: T) -> &mut Self
  where
    T: Into<String>,
  {
    let v = parent_field.into();
    self.base.parent_field = Some(v);
    self
  }

  /// Sets whether [`IndexWriter`](crate::core::index::index_writer::IndexWriter) should pool readers without requiring a
  /// near-real-time reader to have been opened from the writer.
  ///
  /// If set to `false`, [`IndexWriter`](crate::core::index::index_writer::IndexWriter) will still pool readers once a reader is
  /// opened from the writer.
  ///
  /// Only takes effect when [`IndexWriter`](crate::core::index::index_writer::IndexWriter) is first created.
  pub fn set_reader_pooling(&mut self, reader_pooling: bool) -> &mut Self {
    self.base.reader_pooling = reader_pooling;
    self
  }
  /// Set the comparator for sorting leaf readers. A [`DirectoryReader`](crate::core::index::directory_reader::DirectoryReader) opened
  /// from an [`IndexWriter`](crate::core::index::index_writer::IndexWriter) with this configuration will have its leaf readers
  /// sorted with the provided leaf sorter.
  pub fn set_leaf_sorter(&mut self, leaf_sorter: Option<LeafSorter<D>>) -> &mut Self {
    self.base.leaf_sorter = leaf_sorter;
    self
  }

  /// Returns the comparator for sorting leaf readers, or `None` if no leaf
  /// sorter is set.
  pub fn get_leaf_sorter(&self) -> Option<&LeafSorter<D>> {
    self.base.leaf_sorter.as_ref()
  }
}

impl<D: Directory> Deref for IndexWriterConfig<D> {
  type Target = LiveIndexWriterConfigBase<D>;

  fn deref(&self) -> &Self::Target {
    &self.base
  }
}

impl<D: Directory> DerefMut for IndexWriterConfig<D> {
  fn deref_mut(&mut self) -> &mut Self::Target {
    &mut self.base
  }
}

impl<D> Display for IndexWriterConfig<D>
where
  D: Directory,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

/// Specifies the open mode for [`IndexWriter`](crate::core::index::index_writer::IndexWriter).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMode {
  /// Creates a new index or overwrites an existing one.
  Create,

  /// Opens an existing index.
  Append,

  /// Creates a new index if one does not exist, otherwise it opens the index
  /// and documents will be appended.
  CreateOrAppend,
}

/// Denotes a flush trigger is disabled.
pub const DISABLE_AUTO_FLUSH: i32 = -1;

/// Disabled by default (because IndexWriter flushes by RAM usage by default).
pub const DEFAULT_MAX_BUFFERED_DELETE_TERMS: i32 = DISABLE_AUTO_FLUSH;

/// Disabled by default (because IndexWriter flushes by RAM usage by default).
pub const DEFAULT_MAX_BUFFERED_DOCS: i32 = DISABLE_AUTO_FLUSH;

/// Default value is 16 MB (which means flush when buffered docs consume approximately 16 MB RAM).
pub const DEFAULT_RAM_BUFFER_SIZE_MB: f64 = 16.0;

/// Default setting (true) for `set_reader_pooling`.
///
/// We changed this default to true with concurrent deletes/updates (LUCENE-7868),
/// because we will otherwise need to open and close segment readers more frequently.
/// False is still supported, but will have worse performance since readers will
/// be forced to aggressively move all state to disk.
pub const DEFAULT_READER_POOLING: bool = true;

/// Default value is 1945. Change using `set_ram_per_thread_hard_limit_mb`.
pub const DEFAULT_RAM_PER_THREAD_HARD_LIMIT_MB: i32 = 1945;

/// Default value for compound file system for newly written segments (set to `true`).
/// For batch indexing with very large ram buffers use `false`.
pub const DEFAULT_USE_COMPOUND_FILE_SYSTEM: bool = true;

/// Default value for whether calls to [`IndexWriter::close`](crate::core::index::index_writer::IndexWriter::close) include a commit.
pub const DEFAULT_COMMIT_ON_CLOSE: bool = true;

/// Default value for time to wait for merges on commit or getReader (when using a
/// [`MergePolicy`](crate::core::index::merge_policy::MergePolicy) that implements [`MergePolicy::find_full_flush_merges`](crate::core::index::merge_policy::MergePolicy::find_full_flush_merges)).
pub const DEFAULT_MAX_FULL_FLUSH_MERGE_WAIT_MILLIS: i64 = 500;
