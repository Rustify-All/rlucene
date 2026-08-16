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
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::dummy::dummy_doc_map_sorter::DummyDocMap;
use crate::core::index::index_reader::Identity;
use crate::core::index::index_writer::{Inner, PointInTimeOneMerge};
use crate::core::index::log_byte_size_merge_policy::LogByteSizeMergePolicy;
use crate::core::index::log_doc_merge_policy::LogDocMergePolicy;
use crate::core::index::log_merge_policy::LogMergePolicy;
use crate::core::index::merge_trigger::MergeTrigger;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::one_merge_wrapping_merge_policy::OneMergeWrappingMergePolicy;
use crate::core::index::segment_commit_info::SegmentCommitInfo;
use crate::core::index::segment_infos::SegmentInfos;
use crate::core::index::segment_reader::DefaultLeafReader;
use crate::core::index::soft_deletes_retention_merge_policy::{
  SoftDeletesRetentionMergePolicy, SoftDeletesRetentionOneMerge,
};
use crate::core::index::tiered_merge_policy::{
  SegmentCommitInfoMeta, SegmentDocAndID, TieredMergePolicy,
};
use crate::core::index::upgrade_index_merge_policy::UpgradeIndexMergePolicy;
use crate::core::store::directory::Directory;
use crate::core::store::merge_info::MergeInfo;
use crate::core::util::error::lucene_error::{CaughtResult, CaughtResultExt, LuceneError, Result};
use crate::core::util::info_stream::{InfoStream, InfoStreamMT};
use crate::core::util::io_utils::IOUtils;
use crate::sandbox::index::merge_on_flush_merge_policy::MergeOnFlushMergePolicy;
#[cfg(test)]
use crate::test_framework::core::index::alcoholic_merge_policy::AlcoholicMergePolicy;
#[cfg(test)]
use crate::test_framework::core::index::force_merge_policy::ForceMergePolicy;
#[cfg(test)]
use crate::test_framework::core::index::merge_policy::{
  KeepFullyDeletedSegmentsMergePolicy, MergeOnXMergePolicy, MockMergePolicy,
  OnlyForceMergeMergePolicy, RangeMergePolicy,
};
#[cfg(test)]
use crate::test_framework::core::index::mock_random_merge_policy::{
  MockRandomMergePolicy, MockRandomOneMerge, MockRandomOneMergeDocMap, MockRandomWrappedReader,
};
#[cfg(test)]
use crate::test_framework::core::index::test_add_indexes::{
  ConcurrentAddIndexesMergePolicy, SetDiagnosticsOneMerge,
};
#[cfg(test)]
use crate::test_framework::core::index::test_concurrent_merge_scheduler::LiveMaxMergeCountMergePolicy;
#[cfg(test)]
use crate::test_framework::core::index::test_index_writer::{
  AbortOnMergeCompleteOneMerge, MergeFinishedOnceOneMerge, SoftUpdatesConcurrentlyMergePolicy,
  SoftUpdatesConcurrentlyOneMerge,
};
#[cfg(test)]
use crate::test_framework::core::index::test_index_writer_merge_policy::{
  ForceMergeDvUpdateMergePolicy, ForceMergeDvUpdateOneMerge, SetDiagnosticsMergePolicy,
  SetMergePolicyDiagnosticsOneMerge,
};
#[cfg(test)]
use crate::test_framework::core::index::test_one_merge_wrapping_merge_policy::PredeterminedMergePolicy;
use parking_lot::{Condvar, Mutex};
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

/// Default ratio for compound file system usage.
/// Set to `1.0`, always use compound file system.
pub(crate) const DEFAULT_NO_CFS_RATIO: f64 = 1.0;
/// Default max segment size in order to use compound file system.
/// Set to `i64::MAX`.
pub(crate) const DEFAULT_MAX_CFS_SEGMENT_SIZE: i64 = i64::MAX;
/// Expert: a `MergePolicy` determines the sequence of primitive merge operations.
///
/// Whenever the segments in an index have been altered by [`IndexWriter`](crate::core::index::index_writer::IndexWriter), either by:
/// - the addition of a newly flushed segment,
/// - the addition of many segments from `addIndexes*` calls, or
/// - a previous merge that may now need to cascade,
///
/// [`IndexWriter`](crate::core::index::index_writer::IndexWriter) invokes [`Self::find_merges`] to give the `MergePolicy` a chance to
/// select merges that are now required.
///
/// This method returns a [`MergeSpecification`] describing the set of merges
/// that should be executed, or `None` if no merges are necessary.
///
/// When `IndexWriter::force_merge`(crate::core::index::index_writer::IndexWriter::force_merge) is called, it invokes
/// [`Self::find_forced_merges`] and the `MergePolicy` should then return the merges
/// required to satisfy that request.
///
/// Note that a policy may return more than one merge at a time.
/// - When using [`SerialMergeScheduler`](crate::core::index::serial_merge_scheduler::SerialMergeScheduler), these merges are run sequentially.
/// - When using `ConcurrentMergeScheduler`, they may run concurrently.
///
/// The default merge policy is [`TieredMergePolicy`].
pub trait MergePolicy<D>: Display
where
  D: Directory,
{
  fn get_base(&self) -> &MergePolicyBase;
  fn get_base_mut(&mut self) -> &mut MergePolicyBase;
  /// Determine what set of merge operations are now necessary on the index.
  /// [`IndexWriter`](crate::core::index::index_writer::IndexWriter) calls this whenever there is a change to the segments.
  /// This method is always called while the [`IndexWriter`](crate::core::index::index_writer::IndexWriter) lock is held, so only
  /// one thread at a time will call this method.
  ///
  /// * `merge_trigger` — the event that triggered the merge  
  /// * `segment_infos` — the total set of segments in the index  
  /// * `merge_context` — the `MergeContext` to find merges on
  fn find_merges<MC>(
    &self,
    merge_trigger: MergeTrigger,
    segment_infos: &SegmentInfos<D>,
    inner: Option<&Inner<D>>,
    merge_context: &MC,
  ) -> Result<Option<DefaultMergeSpecification<D>>>
  where
    MC: MergeContext<D>;

  /// Define the set of merge operations to perform on provided codec readers in
  /// `IndexWriter::add_indexes`.
  ///
  /// The merge operation is required to convert provided readers into segments
  /// that can be added to the writer. This API can be overridden in custom merge
  /// policies to control concurrency for `addIndexes`.
  ///
  /// Default implementation creates a single merge operation for all provided
  /// readers (lowest concurrency). Creating a merge for each reader would provide
  /// the highest level of concurrency possible with the configured merge scheduler.
  ///
  /// * `readers` — codec readers to merge into the main index
  fn find_merges_readers<CR>(&self, readers: Vec<CR>) -> Result<Option<MergeSpecification<D, CR>>>
  where
    CR: CodecReader,
  {
    let mut merge_spec = MergeSpecification::new();
    merge_spec.add(OneMerge::from_codec_readers(readers)?);
    Ok(Some(merge_spec))
  }

  ///   Determine what set of merge operations is necessary in order to merge to
  ///   `<=` the specified segment count. [`IndexWriter`](crate::core::index::index_writer::IndexWriter) calls this when its
  ///   `force_merge` method is invoked. This call always runs while holding the
  ///   [`IndexWriter`](crate::core::index::index_writer::IndexWriter) instance so only one thread at a time will call it.
  ///
  /// * `segment_infos` — the total set of segments in the index  
  /// * `max_segment_count` — requested maximum number of segments  
  /// * `segments_to_merge` — map of `SegmentCommitInfo` → boolean indicating
  ///   which segments must be merged away  
  /// * `merge_context` — the `MergeContext` to find merges on
  fn find_forced_merges<MC>(
    &self,
    segment_infos: &SegmentInfos<D>,
    max_segment_count: usize,
    segments_to_merge: &HashMap<String, Option<bool>>,
    inner: Option<&Inner<D>>,
    merge_context: &MC,
  ) -> Result<Option<DefaultMergeSpecification<D>>>
  where
    MC: MergeContext<D>;

  /// Determine what set of merge operations is necessary in order to expunge all deletes
  /// from the index.
  ///
  /// * `segment_infos` — the total set of segments in the index  
  /// * `merge_context` — the `MergeContext` to find merges on
  fn find_forced_deletes_merges<MC>(
    &self,
    segment_infos: &SegmentInfos<D>,
    inner: Option<&Inner<D>>,
    merge_context: &MC,
  ) -> Result<Option<DefaultMergeSpecification<D>>>
  where
    MC: MergeContext<D>;
  /// Identifies merges that we want to execute **synchronously** on commit.
  /// By default, this will return the same merges as `find_merges`
  /// (“natural merges”) whose segments are all less than the
  /// `max_full_flush_merge_size` (the max segment size for full flushes).
  ///
  /// Any merges returned here will make:
  /// - `IndexWriter::commit`,
  /// - `IndexWriter::prepare_commit` or
  /// - `IndexWriter::get_reader`
  ///
  /// block until the merges complete, or until
  /// `IndexWriterConfig::get_max_full_flush_merge_wait_millis` has elapsed.
  ///
  /// This may be used to merge small segments that have just been flushed,
  /// reducing the number of segments in the point-in-time snapshot. If a merge
  /// does not complete in the allotted time, it will continue to execute and
  /// eventually finish and apply to future point-in-time snapshots, but it will
  /// **not** be reflected in the current one.
  ///
  /// If a [`OneMerge`] in the returned [`MergeSpecification`] includes a segment
  /// that is already included in a registered merge, then
  /// `IndexWriter::commit` or `IndexWriter::prepare_commit` will return an
  /// error. Use [`MergeContext::get_merging_segments`] to determine which
  /// segments are currently registered to merge.
  ///
  /// # Parameters
  ///
  /// * `merge_trigger` — the event that triggered the merge (COMMIT or GET_READER)
  /// * `segment_infos` — the total set of segments in the index (while preparing the commit)
  /// * `merge_context` — the [`MergeContext`] to find merges on, which should be
  ///   used to determine which segments are already in a registered merge
  ///   (see [`MergeContext::get_merging_segments`])
  fn find_full_flush_merges<MC>(
    &self,
    merge_trigger: MergeTrigger,
    segment_infos: &SegmentInfos<D>,
    inner: Option<&Inner<D>>,
    merge_context: &MC,
  ) -> Result<Option<DefaultMergeSpecification<D>>>
  where
    MC: MergeContext<D>,
  {
    // This returns natural merges that contain segments below the minimum size
    let merge_spec = self.find_merges(merge_trigger, segment_infos, inner, merge_context)?;

    match merge_spec {
      None => Ok(None),
      Some(merge_spec) => {
        let mut new_merge_spec = None;
        let segment_infos_by_id: HashMap<_, _> = segment_infos
          .iter()
          .iter()
          .map(|info| (info.info.get_id_key(), info))
          .collect();

        for one_merge in merge_spec.merges.into_iter() {
          let mut below_max_full_flush_size = true;

          for seg_id in &one_merge.stat.segments {
            match segment_infos_by_id.get(seg_id.as_str()).copied() {
              Some(sci) => {
                if self.size(sci, merge_context)? >= self.max_full_flush_merge_size() {
                  below_max_full_flush_size = false;
                  break;
                }
              },
              None => {
                return Err(LuceneError::illegal_state(
                  "could not find SegmentCommitInfo from segment_infos",
                ));
              },
            }
          }

          if below_max_full_flush_size {
            if new_merge_spec.is_none() {
              new_merge_spec = Some(DefaultMergeSpecification::new());
            }
            if let Some(ref mut spec) = new_merge_spec {
              spec.add(one_merge);
            }
          }
        }

        Ok(new_merge_spec)
      },
    }
  }

  /// Returns `true` if a new segment (regardless of its origin) should use the
  /// compound file format.
  ///
  /// The default implementation returns `true` iff:
  ///
  /// - the size of the given `merged_info` is less than or equal to
  ///   [`MergePolicyBase::get_max_cfs_segment_size_mb`], **and**
  /// - the size is less than or equal to `total_index_size * get_no_cfs_ratio()`
  ///
  /// otherwise returns `false`.
  fn use_compound_file<MC>(
    &self,
    infos: &SegmentInfos<D>,
    merged_info: &SegmentCommitInfo<D>,
    merge_context: &MC,
  ) -> Result<bool>
  where
    MC: MergeContext<D>,
  {
    let (no_cfs_ratio, max_cfs_segment_size) = {
      let base = self.get_base();
      if base.get_no_cfs_ratio() == 0.0 {
        return Ok(false);
      }
      (base.get_no_cfs_ratio(), base.max_cfs_segment_size)
    };

    let merged_info_size = self.size(merged_info, merge_context)?;
    if merged_info_size > max_cfs_segment_size {
      return Ok(false);
    }

    if no_cfs_ratio >= 1.0 {
      return Ok(true);
    }
    let mut total_size = 0_i64;

    for sci in infos.iter() {
      total_size += self.size(sci, merge_context)?;
    }
    Ok((merged_info_size as f64) <= no_cfs_ratio * (total_size as f64))
  }

  /// Return the byte size of the provided [`SegmentCommitInfo`], prorated by the
  /// percentage of non-deleted documents that remain.
  fn size<MC>(&self, info: &SegmentCommitInfo<D>, merge_context: &MC) -> Result<i64>
  where
    MC: MergeContext<D>;
  /// Return the maximum size of segments to be included in full-flush merges
  /// by the default implementation of `find_full_flush_merges`.
  fn max_full_flush_merge_size(&self) -> i64 {
    0
  }

  /// Returns `true` if this single info is already fully merged (has no pending
  /// deletes, is in the same directory as the writer, and matches the current
  /// compound file setting).
  fn has_merged<MC>(
    &self,
    infos: &SegmentInfos<D>,
    info: &SegmentCommitInfo<D>,
    merge_context: &MC,
  ) -> Result<bool>
  where
    MC: MergeContext<D>,
  {
    let del_count = merge_context.num_deletes_to_merge(info)?;
    debug_assert!(assert_del_count(del_count, info)?);

    Ok(
      del_count == 0
        && self.use_compound_file(infos, info, merge_context)? == info.info.get_use_compound_file(),
    )
  }
  /// Returns `true` if the segment represented by the given `CodecReader`
  /// should be kept even if it is fully deleted.
  ///
  /// This is useful for testing, or for merge policies that implement
  /// retention rules for soft deletes.
  fn keep_fully_deleted_segment<F>(&self, _reader_supplier: F) -> Result<bool>
  where
    F: Fn() -> Result<DefaultLeafReader<D>>,
  {
    Ok(false)
  }
  /// Returns the number of deletes that a merge would claim on the given segment.
  ///
  /// By default, this returns the sum of:
  /// - the number of deletes on disk, and
  /// - the number of pending deletes.
  ///
  /// Implementations that wrap merge readers may provide this method to reflect
  /// deletes that are carried over into the target segment in the case of soft deletes.
  ///
  /// Soft deletes allow deleted documents to survive across merges so that the
  /// application controls when soft-deleted data is truly removed.
  ///
  /// * `info` — the segment being merged
  /// * `del_count` — the current delete count for this segment
  /// * `reader_supplier` — a supplier for obtaining a [`CodecReader`] of this segment
  fn num_deletes_to_merge<F>(
    &self,
    _info: &SegmentCommitInfo<D>,
    del_count: i32,
    _reader_supplier: &F,
  ) -> Result<i32>
  where
    F: Fn() -> Result<DefaultLeafReader<D>>,
  {
    Ok(del_count)
  }

  /// Builds a string representation of the given [`SegmentCommitInfo`] instances.
  fn seg_string<MC>(&self, merge_context: &MC, infos: &[SegmentCommitInfo<D>]) -> String
  where
    MC: MergeContext<D>,
  {
    infos
      .iter()
      .map(|info| {
        let del = merge_context.num_deleted_docs(info) - info.get_del_count();
        info.to_string_with_pending_del_count(del)
      })
      .collect::<Vec<_>>()
      .join(" ")
  }

  /// Print a debug message to the [`MergeContext`]’s `infoStream`.
  fn message<MC>(&self, message: &str, merge_context: &MC) -> Result<()>
  where
    MC: MergeContext<D>,
  {
    if self.verbose(merge_context) {
      merge_context.get_info_stream().message("MP", message)?;
    }
    Ok(())
  }

  /// Returns `true` if the info-stream is in verbose mode.
  ///
  /// See `message`.
  fn verbose<MC>(&self, merge_context: &MC) -> bool
  where
    MC: MergeContext<D>,
  {
    merge_context.get_info_stream().is_enabled("MP")
  }
}

/// Asserts that the `delCount` for this [`SegmentCommitInfo`] is valid.
pub(crate) fn assert_del_count<D>(del_count: i32, info: &SegmentCommitInfo<D>) -> Result<bool> {
  debug_assert!(del_count >= 0, "delCount must be positive: {}", del_count);
  debug_assert!(
    del_count <= info.info.max_doc()?,
    "delCount: {} must be ≤ maxDoc: {}",
    del_count,
    info.info.max_doc()?
  );
  Ok(true)
}
pub(crate) fn size<D, MC>(info: &SegmentCommitInfo<D>, merge_context: &MC) -> Result<i64>
where
  D: Directory,
  MC: MergeContext<D>,
{
  let byte_size = info.size_in_bytes()?;
  let del_count = merge_context.num_deletes_to_merge(info)?;
  debug_assert!(assert_del_count(del_count, info)?);
  let max_doc = info.info.max_doc()?;
  let del_ratio = if max_doc <= 0 {
    0.0
  } else {
    del_count as f64 / max_doc as f64
  };

  debug_assert!(del_ratio <= 1.0);

  if max_doc <= 0 {
    Ok(byte_size)
  } else {
    Ok((byte_size as f64 * (1.0 - del_ratio)) as i64)
  }
}
pub enum MergePolicyEnum<D>
where
  D: Directory,
{
  No(NoMergePolicy),
  Tiered(TieredMergePolicy),
  LogDoc(LogMergePolicy<LogDocMergePolicy>),
  LogBytesSize(LogMergePolicy<LogByteSizeMergePolicy>),
  #[cfg(test)]
  Alcoholic(LogMergePolicy<AlcoholicMergePolicy>),
  #[cfg(test)]
  Predetermined(PredeterminedMergePolicy<D>),
  OneMergeWrapping(OneMergeWrappingMergePolicy<D>),
  SoftDeletesRetention(SoftDeletesRetentionMergePolicy<D>),
  Upgrade(UpgradeIndexMergePolicy<D>),
  MergeOnFlush(MergeOnFlushMergePolicy<D>),
  #[cfg(test)]
  Force(ForceMergePolicy<MergePolicyEnum<D>>),
  #[cfg(test)]
  OnlyForceMerge(OnlyForceMergeMergePolicy),
  #[cfg(test)]
  ForceMergeDvUpdate(ForceMergeDvUpdateMergePolicy),
  #[cfg(test)]
  SoftUpdatesConcurrently(SoftUpdatesConcurrentlyMergePolicy<D>),
  #[cfg(test)]
  KeepFullyDeletedSegments(KeepFullyDeletedSegmentsMergePolicy<D>),
  #[cfg(test)]
  Range(RangeMergePolicy),
  #[cfg(test)]
  Mock(MockMergePolicy),
  #[cfg(test)]
  MockRandom(MockRandomMergePolicy),
  #[cfg(test)]
  ConcurrentAddIndexes(ConcurrentAddIndexesMergePolicy),
  #[cfg(test)]
  SetDiagnostics(SetDiagnosticsMergePolicy<D>),
  #[cfg(test)]
  MergeOnX(MergeOnXMergePolicy<D>),
  #[cfg(test)]
  LiveMaxMergeCount(LiveMaxMergeCountMergePolicy),
}

impl<D> Clone for MergePolicyEnum<D>
where
  D: Directory,
{
  fn clone(&self) -> Self {
    match self {
      Self::No(mp) => Self::No(mp.clone()),
      Self::Tiered(mp) => Self::Tiered(mp.clone()),
      Self::LogDoc(mp) => Self::LogDoc(mp.clone()),
      Self::LogBytesSize(mp) => Self::LogBytesSize(mp.clone()),
      #[cfg(test)]
      Self::Alcoholic(mp) => Self::Alcoholic(mp.clone()),
      #[cfg(test)]
      Self::Predetermined(mp) => Self::Predetermined(mp.clone()),
      Self::OneMergeWrapping(mp) => Self::OneMergeWrapping(mp.clone()),
      Self::SoftDeletesRetention(mp) => Self::SoftDeletesRetention(mp.clone()),
      Self::Upgrade(mp) => Self::Upgrade(mp.clone()),
      Self::MergeOnFlush(mp) => Self::MergeOnFlush(mp.clone()),
      #[cfg(test)]
      Self::Force(mp) => Self::Force(mp.clone()),
      #[cfg(test)]
      Self::OnlyForceMerge(mp) => Self::OnlyForceMerge(mp.clone()),
      #[cfg(test)]
      Self::ForceMergeDvUpdate(mp) => Self::ForceMergeDvUpdate(mp.clone()),
      #[cfg(test)]
      Self::SoftUpdatesConcurrently(mp) => Self::SoftUpdatesConcurrently(mp.clone()),
      #[cfg(test)]
      Self::KeepFullyDeletedSegments(mp) => Self::KeepFullyDeletedSegments(mp.clone()),
      #[cfg(test)]
      Self::Range(mp) => Self::Range(mp.clone()),
      #[cfg(test)]
      Self::Mock(mp) => Self::Mock(mp.clone()),
      #[cfg(test)]
      Self::MockRandom(mp) => Self::MockRandom(mp.clone()),
      #[cfg(test)]
      Self::ConcurrentAddIndexes(mp) => Self::ConcurrentAddIndexes(mp.clone()),
      #[cfg(test)]
      Self::SetDiagnostics(mp) => Self::SetDiagnostics(mp.clone()),
      #[cfg(test)]
      Self::MergeOnX(mp) => Self::MergeOnX(mp.clone()),
      #[cfg(test)]
      Self::LiveMaxMergeCount(mp) => Self::LiveMaxMergeCount(mp.clone()),
    }
  }
}

impl<D> From<NoMergePolicy> for MergePolicyEnum<D>
where
  D: Directory,
{
  fn from(v: NoMergePolicy) -> Self {
    Self::No(v)
  }
}

impl<D> From<TieredMergePolicy> for MergePolicyEnum<D>
where
  D: Directory,
{
  fn from(v: TieredMergePolicy) -> Self {
    Self::Tiered(v)
  }
}

impl<D> From<LogMergePolicy<LogDocMergePolicy>> for MergePolicyEnum<D>
where
  D: Directory,
{
  fn from(v: LogMergePolicy<LogDocMergePolicy>) -> Self {
    Self::LogDoc(v)
  }
}

impl<D> From<LogMergePolicy<LogByteSizeMergePolicy>> for MergePolicyEnum<D>
where
  D: Directory,
{
  fn from(v: LogMergePolicy<LogByteSizeMergePolicy>) -> Self {
    Self::LogBytesSize(v)
  }
}

#[cfg(test)]
impl<D> From<LogMergePolicy<AlcoholicMergePolicy>> for MergePolicyEnum<D>
where
  D: Directory,
{
  fn from(v: LogMergePolicy<AlcoholicMergePolicy>) -> Self {
    Self::Alcoholic(v)
  }
}

#[cfg(test)]
impl<D> From<MockRandomMergePolicy> for MergePolicyEnum<D>
where
  D: Directory,
{
  fn from(v: MockRandomMergePolicy) -> Self {
    Self::MockRandom(v)
  }
}

impl<D> From<OneMergeWrappingMergePolicy<D>> for MergePolicyEnum<D>
where
  D: Directory,
{
  fn from(v: OneMergeWrappingMergePolicy<D>) -> Self {
    Self::OneMergeWrapping(v)
  }
}

impl<D> From<SoftDeletesRetentionMergePolicy<D>> for MergePolicyEnum<D>
where
  D: Directory,
{
  fn from(v: SoftDeletesRetentionMergePolicy<D>) -> Self {
    Self::SoftDeletesRetention(v)
  }
}

impl<D> From<UpgradeIndexMergePolicy<D>> for MergePolicyEnum<D>
where
  D: Directory,
{
  fn from(v: UpgradeIndexMergePolicy<D>) -> Self {
    Self::Upgrade(v)
  }
}

impl<D> From<MergeOnFlushMergePolicy<D>> for MergePolicyEnum<D>
where
  D: Directory,
{
  fn from(v: MergeOnFlushMergePolicy<D>) -> Self {
    Self::MergeOnFlush(v)
  }
}

#[cfg(test)]
impl<D> From<MergeOnXMergePolicy<D>> for MergePolicyEnum<D>
where
  D: Directory,
{
  fn from(v: MergeOnXMergePolicy<D>) -> Self {
    MergePolicyEnum::MergeOnX(v)
  }
}

#[cfg(test)]
impl<D> From<LiveMaxMergeCountMergePolicy> for MergePolicyEnum<D>
where
  D: Directory,
{
  fn from(value: LiveMaxMergeCountMergePolicy) -> Self {
    Self::LiveMaxMergeCount(value)
  }
}

impl<D> Display for MergePolicyEnum<D>
where
  D: Directory,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self {
      MergePolicyEnum::No(mp) => write!(f, "{}", mp),
      MergePolicyEnum::Tiered(mp) => write!(f, "{}", mp),
      MergePolicyEnum::LogDoc(mp) => write!(f, "{}", mp),
      MergePolicyEnum::LogBytesSize(mp) => write!(f, "{}", mp),
      #[cfg(test)]
      MergePolicyEnum::Alcoholic(mp) => write!(f, "{}", mp),
      #[cfg(test)]
      MergePolicyEnum::Predetermined(mp) => write!(f, "{}", mp),
      MergePolicyEnum::OneMergeWrapping(mp) => write!(f, "{}", mp),
      MergePolicyEnum::SoftDeletesRetention(mp) => write!(f, "{}", mp),
      MergePolicyEnum::Upgrade(mp) => write!(f, "{}", mp),
      MergePolicyEnum::MergeOnFlush(mp) => write!(f, "{}", mp),
      #[cfg(test)]
      MergePolicyEnum::Force(mp) => write!(f, "{}", mp),
      #[cfg(test)]
      MergePolicyEnum::OnlyForceMerge(mp) => write!(f, "{}", mp),
      #[cfg(test)]
      MergePolicyEnum::ForceMergeDvUpdate(mp) => write!(f, "{}", mp),
      #[cfg(test)]
      MergePolicyEnum::SoftUpdatesConcurrently(mp) => write!(f, "{}", mp),
      #[cfg(test)]
      MergePolicyEnum::KeepFullyDeletedSegments(mp) => write!(f, "{}", mp),
      #[cfg(test)]
      MergePolicyEnum::Range(mp) => write!(f, "{}", mp),
      #[cfg(test)]
      MergePolicyEnum::Mock(mp) => write!(f, "{}", mp),
      #[cfg(test)]
      MergePolicyEnum::MockRandom(mp) => write!(f, "{}", mp),
      #[cfg(test)]
      MergePolicyEnum::ConcurrentAddIndexes(mp) => write!(f, "{}", mp),
      #[cfg(test)]
      MergePolicyEnum::SetDiagnostics(mp) => write!(f, "{}", mp),
      #[cfg(test)]
      MergePolicyEnum::MergeOnX(mp) => write!(f, "{}", mp),
      #[cfg(test)]
      MergePolicyEnum::LiveMaxMergeCount(mp) => write!(f, "{}", mp),
    }
  }
}

impl<D> MergePolicy<D> for MergePolicyEnum<D>
where
  D: Directory,
{
  fn get_base(&self) -> &MergePolicyBase {
    match self {
      MergePolicyEnum::No(mp) => MergePolicy::<D>::get_base(mp),
      MergePolicyEnum::Tiered(mp) => MergePolicy::<D>::get_base(mp),
      MergePolicyEnum::LogDoc(mp) => MergePolicy::<D>::get_base(mp),
      MergePolicyEnum::LogBytesSize(mp) => MergePolicy::<D>::get_base(mp),
      #[cfg(test)]
      MergePolicyEnum::Alcoholic(mp) => MergePolicy::<D>::get_base(mp),
      #[cfg(test)]
      MergePolicyEnum::Predetermined(mp) => MergePolicy::<D>::get_base(mp),
      MergePolicyEnum::OneMergeWrapping(mp) => MergePolicy::<D>::get_base(mp),
      MergePolicyEnum::SoftDeletesRetention(mp) => MergePolicy::<D>::get_base(mp),
      MergePolicyEnum::Upgrade(mp) => MergePolicy::<D>::get_base(mp),
      MergePolicyEnum::MergeOnFlush(mp) => MergePolicy::<D>::get_base(mp),
      #[cfg(test)]
      MergePolicyEnum::Force(mp) => MergePolicy::<D>::get_base(mp),
      #[cfg(test)]
      MergePolicyEnum::OnlyForceMerge(mp) => MergePolicy::<D>::get_base(mp),
      #[cfg(test)]
      MergePolicyEnum::ForceMergeDvUpdate(mp) => MergePolicy::<D>::get_base(mp),
      #[cfg(test)]
      MergePolicyEnum::SoftUpdatesConcurrently(mp) => MergePolicy::<D>::get_base(mp),
      #[cfg(test)]
      MergePolicyEnum::KeepFullyDeletedSegments(mp) => MergePolicy::<D>::get_base(mp),
      #[cfg(test)]
      MergePolicyEnum::Range(mp) => MergePolicy::<D>::get_base(mp),
      #[cfg(test)]
      MergePolicyEnum::Mock(mp) => MergePolicy::<D>::get_base(mp),
      #[cfg(test)]
      MergePolicyEnum::MockRandom(mp) => MergePolicy::<D>::get_base(mp),
      #[cfg(test)]
      MergePolicyEnum::ConcurrentAddIndexes(mp) => MergePolicy::<D>::get_base(mp),
      #[cfg(test)]
      MergePolicyEnum::SetDiagnostics(mp) => MergePolicy::<D>::get_base(mp),
      #[cfg(test)]
      MergePolicyEnum::MergeOnX(mp) => MergePolicy::<D>::get_base(mp),
      #[cfg(test)]
      MergePolicyEnum::LiveMaxMergeCount(mp) => MergePolicy::<D>::get_base(mp),
    }
  }

  fn get_base_mut(&mut self) -> &mut MergePolicyBase {
    match self {
      MergePolicyEnum::No(mp) => MergePolicy::<D>::get_base_mut(mp),
      MergePolicyEnum::Tiered(mp) => MergePolicy::<D>::get_base_mut(mp),
      MergePolicyEnum::LogDoc(mp) => MergePolicy::<D>::get_base_mut(mp),
      MergePolicyEnum::LogBytesSize(mp) => MergePolicy::<D>::get_base_mut(mp),
      #[cfg(test)]
      MergePolicyEnum::Alcoholic(mp) => MergePolicy::<D>::get_base_mut(mp),
      #[cfg(test)]
      MergePolicyEnum::Predetermined(mp) => MergePolicy::<D>::get_base_mut(mp),
      MergePolicyEnum::OneMergeWrapping(mp) => MergePolicy::<D>::get_base_mut(mp),
      MergePolicyEnum::SoftDeletesRetention(mp) => MergePolicy::<D>::get_base_mut(mp),
      MergePolicyEnum::Upgrade(mp) => MergePolicy::<D>::get_base_mut(mp),
      MergePolicyEnum::MergeOnFlush(mp) => MergePolicy::<D>::get_base_mut(mp),
      #[cfg(test)]
      MergePolicyEnum::Force(mp) => MergePolicy::<D>::get_base_mut(mp),
      #[cfg(test)]
      MergePolicyEnum::OnlyForceMerge(mp) => MergePolicy::<D>::get_base_mut(mp),
      #[cfg(test)]
      MergePolicyEnum::ForceMergeDvUpdate(mp) => MergePolicy::<D>::get_base_mut(mp),
      #[cfg(test)]
      MergePolicyEnum::SoftUpdatesConcurrently(mp) => MergePolicy::<D>::get_base_mut(mp),
      #[cfg(test)]
      MergePolicyEnum::KeepFullyDeletedSegments(mp) => MergePolicy::<D>::get_base_mut(mp),
      #[cfg(test)]
      MergePolicyEnum::Range(mp) => MergePolicy::<D>::get_base_mut(mp),
      #[cfg(test)]
      MergePolicyEnum::Mock(mp) => MergePolicy::<D>::get_base_mut(mp),
      #[cfg(test)]
      MergePolicyEnum::MockRandom(mp) => MergePolicy::<D>::get_base_mut(mp),
      #[cfg(test)]
      MergePolicyEnum::ConcurrentAddIndexes(mp) => MergePolicy::<D>::get_base_mut(mp),
      #[cfg(test)]
      MergePolicyEnum::SetDiagnostics(mp) => MergePolicy::<D>::get_base_mut(mp),
      #[cfg(test)]
      MergePolicyEnum::MergeOnX(mp) => MergePolicy::<D>::get_base_mut(mp),
      #[cfg(test)]
      MergePolicyEnum::LiveMaxMergeCount(mp) => MergePolicy::<D>::get_base_mut(mp),
    }
  }

  fn find_merges<MC>(
    &self,
    merge_trigger: MergeTrigger,
    segment_infos: &SegmentInfos<D>,
    inner: Option<&Inner<D>>,
    merge_context: &MC,
  ) -> Result<Option<DefaultMergeSpecification<D>>>
  where
    MC: MergeContext<D>,
  {
    match self {
      MergePolicyEnum::No(mp) => mp.find_merges(merge_trigger, segment_infos, inner, merge_context),
      MergePolicyEnum::Tiered(mp) => {
        mp.find_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      MergePolicyEnum::LogDoc(mp) => {
        mp.find_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      MergePolicyEnum::LogBytesSize(mp) => {
        mp.find_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::Alcoholic(mp) => {
        mp.find_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::Predetermined(mp) => {
        mp.find_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      MergePolicyEnum::OneMergeWrapping(mp) => {
        mp.find_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      MergePolicyEnum::SoftDeletesRetention(mp) => {
        mp.find_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      MergePolicyEnum::Upgrade(mp) => {
        mp.find_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      MergePolicyEnum::MergeOnFlush(mp) => {
        mp.find_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::Force(mp) => {
        mp.find_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::OnlyForceMerge(mp) => {
        mp.find_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::ForceMergeDvUpdate(mp) => {
        mp.find_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::SoftUpdatesConcurrently(mp) => {
        mp.find_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::KeepFullyDeletedSegments(mp) => {
        mp.find_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::Range(mp) => {
        mp.find_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::Mock(mp) => {
        mp.find_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::MockRandom(mp) => {
        mp.find_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::ConcurrentAddIndexes(mp) => {
        mp.find_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::SetDiagnostics(mp) => {
        mp.find_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::MergeOnX(mp) => {
        mp.find_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::LiveMaxMergeCount(mp) => {
        mp.find_merges(merge_trigger, segment_infos, inner, merge_context)
      },
    }
  }

  fn find_merges_readers<CR>(&self, readers: Vec<CR>) -> Result<Option<MergeSpecification<D, CR>>>
  where
    CR: CodecReader,
  {
    match self {
      MergePolicyEnum::No(mp) => mp.find_merges_readers(readers),
      MergePolicyEnum::Tiered(mp) => mp.find_merges_readers(readers),
      MergePolicyEnum::LogDoc(mp) => mp.find_merges_readers(readers),
      MergePolicyEnum::LogBytesSize(mp) => mp.find_merges_readers(readers),
      #[cfg(test)]
      MergePolicyEnum::Alcoholic(mp) => mp.find_merges_readers(readers),
      #[cfg(test)]
      MergePolicyEnum::Predetermined(mp) => mp.find_merges_readers(readers),
      MergePolicyEnum::OneMergeWrapping(mp) => mp.find_merges_readers(readers),
      MergePolicyEnum::SoftDeletesRetention(mp) => mp.find_merges_readers(readers),
      MergePolicyEnum::Upgrade(mp) => mp.find_merges_readers(readers),
      MergePolicyEnum::MergeOnFlush(mp) => mp.find_merges_readers(readers),
      #[cfg(test)]
      MergePolicyEnum::Force(mp) => mp.find_merges_readers(readers),
      #[cfg(test)]
      MergePolicyEnum::OnlyForceMerge(mp) => mp.find_merges_readers(readers),
      #[cfg(test)]
      MergePolicyEnum::ForceMergeDvUpdate(mp) => mp.find_merges_readers(readers),
      #[cfg(test)]
      MergePolicyEnum::SoftUpdatesConcurrently(mp) => mp.find_merges_readers(readers),
      #[cfg(test)]
      MergePolicyEnum::KeepFullyDeletedSegments(mp) => mp.find_merges_readers(readers),
      #[cfg(test)]
      MergePolicyEnum::Range(mp) => mp.find_merges_readers(readers),
      #[cfg(test)]
      MergePolicyEnum::Mock(mp) => mp.find_merges_readers(readers),
      #[cfg(test)]
      MergePolicyEnum::MockRandom(mp) => mp.find_merges_readers(readers),
      #[cfg(test)]
      MergePolicyEnum::ConcurrentAddIndexes(mp) => mp.find_merges_readers(readers),
      #[cfg(test)]
      MergePolicyEnum::SetDiagnostics(mp) => mp.find_merges_readers(readers),
      #[cfg(test)]
      MergePolicyEnum::MergeOnX(mp) => mp.find_merges_readers(readers),
      #[cfg(test)]
      MergePolicyEnum::LiveMaxMergeCount(mp) => mp.find_merges_readers(readers),
    }
  }

  fn find_forced_merges<MC>(
    &self,
    segment_infos: &SegmentInfos<D>,
    max_segment_count: usize,
    segments_to_merge: &HashMap<String, Option<bool>>,
    inner: Option<&Inner<D>>,
    merge_context: &MC,
  ) -> Result<Option<DefaultMergeSpecification<D>>>
  where
    MC: MergeContext<D>,
  {
    match self {
      MergePolicyEnum::No(mp) => mp.find_forced_merges(
        segment_infos,
        max_segment_count,
        segments_to_merge,
        inner,
        merge_context,
      ),
      MergePolicyEnum::Tiered(mp) => mp.find_forced_merges(
        segment_infos,
        max_segment_count,
        segments_to_merge,
        inner,
        merge_context,
      ),
      MergePolicyEnum::LogDoc(mp) => mp.find_forced_merges(
        segment_infos,
        max_segment_count,
        segments_to_merge,
        inner,
        merge_context,
      ),
      MergePolicyEnum::LogBytesSize(mp) => mp.find_forced_merges(
        segment_infos,
        max_segment_count,
        segments_to_merge,
        inner,
        merge_context,
      ),
      #[cfg(test)]
      MergePolicyEnum::Alcoholic(mp) => mp.find_forced_merges(
        segment_infos,
        max_segment_count,
        segments_to_merge,
        inner,
        merge_context,
      ),
      #[cfg(test)]
      MergePolicyEnum::Predetermined(mp) => mp.find_forced_merges(
        segment_infos,
        max_segment_count,
        segments_to_merge,
        inner,
        merge_context,
      ),
      MergePolicyEnum::OneMergeWrapping(mp) => mp.find_forced_merges(
        segment_infos,
        max_segment_count,
        segments_to_merge,
        inner,
        merge_context,
      ),
      MergePolicyEnum::SoftDeletesRetention(mp) => mp.find_forced_merges(
        segment_infos,
        max_segment_count,
        segments_to_merge,
        inner,
        merge_context,
      ),
      MergePolicyEnum::Upgrade(mp) => mp.find_forced_merges(
        segment_infos,
        max_segment_count,
        segments_to_merge,
        inner,
        merge_context,
      ),
      MergePolicyEnum::MergeOnFlush(mp) => mp.find_forced_merges(
        segment_infos,
        max_segment_count,
        segments_to_merge,
        inner,
        merge_context,
      ),
      #[cfg(test)]
      MergePolicyEnum::Force(mp) => mp.find_forced_merges(
        segment_infos,
        max_segment_count,
        segments_to_merge,
        inner,
        merge_context,
      ),
      #[cfg(test)]
      MergePolicyEnum::OnlyForceMerge(mp) => mp.find_forced_merges(
        segment_infos,
        max_segment_count,
        segments_to_merge,
        inner,
        merge_context,
      ),
      #[cfg(test)]
      MergePolicyEnum::ForceMergeDvUpdate(mp) => mp.find_forced_merges(
        segment_infos,
        max_segment_count,
        segments_to_merge,
        inner,
        merge_context,
      ),
      #[cfg(test)]
      MergePolicyEnum::SoftUpdatesConcurrently(mp) => mp.find_forced_merges(
        segment_infos,
        max_segment_count,
        segments_to_merge,
        inner,
        merge_context,
      ),
      #[cfg(test)]
      MergePolicyEnum::KeepFullyDeletedSegments(mp) => mp.find_forced_merges(
        segment_infos,
        max_segment_count,
        segments_to_merge,
        inner,
        merge_context,
      ),
      #[cfg(test)]
      MergePolicyEnum::Range(mp) => mp.find_forced_merges(
        segment_infos,
        max_segment_count,
        segments_to_merge,
        inner,
        merge_context,
      ),
      #[cfg(test)]
      MergePolicyEnum::Mock(mp) => mp.find_forced_merges(
        segment_infos,
        max_segment_count,
        segments_to_merge,
        inner,
        merge_context,
      ),
      #[cfg(test)]
      MergePolicyEnum::MockRandom(mp) => mp.find_forced_merges(
        segment_infos,
        max_segment_count,
        segments_to_merge,
        inner,
        merge_context,
      ),
      #[cfg(test)]
      MergePolicyEnum::ConcurrentAddIndexes(mp) => mp.find_forced_merges(
        segment_infos,
        max_segment_count,
        segments_to_merge,
        inner,
        merge_context,
      ),
      #[cfg(test)]
      MergePolicyEnum::SetDiagnostics(mp) => mp.find_forced_merges(
        segment_infos,
        max_segment_count,
        segments_to_merge,
        inner,
        merge_context,
      ),
      #[cfg(test)]
      MergePolicyEnum::MergeOnX(mp) => mp.find_forced_merges(
        segment_infos,
        max_segment_count,
        segments_to_merge,
        inner,
        merge_context,
      ),
      #[cfg(test)]
      MergePolicyEnum::LiveMaxMergeCount(mp) => mp.find_forced_merges(
        segment_infos,
        max_segment_count,
        segments_to_merge,
        inner,
        merge_context,
      ),
    }
  }

  fn find_forced_deletes_merges<MC>(
    &self,
    segment_infos: &SegmentInfos<D>,
    inner: Option<&Inner<D>>,
    merge_context: &MC,
  ) -> Result<Option<DefaultMergeSpecification<D>>>
  where
    MC: MergeContext<D>,
  {
    match self {
      MergePolicyEnum::No(mp) => mp.find_forced_deletes_merges(segment_infos, inner, merge_context),
      MergePolicyEnum::Tiered(mp) => {
        mp.find_forced_deletes_merges(segment_infos, inner, merge_context)
      },
      MergePolicyEnum::LogDoc(mp) => {
        mp.find_forced_deletes_merges(segment_infos, inner, merge_context)
      },
      MergePolicyEnum::LogBytesSize(mp) => {
        mp.find_forced_deletes_merges(segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::Alcoholic(mp) => {
        mp.find_forced_deletes_merges(segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::Predetermined(mp) => {
        mp.find_forced_deletes_merges(segment_infos, inner, merge_context)
      },
      MergePolicyEnum::OneMergeWrapping(mp) => {
        mp.find_forced_deletes_merges(segment_infos, inner, merge_context)
      },
      MergePolicyEnum::SoftDeletesRetention(mp) => {
        mp.find_forced_deletes_merges(segment_infos, inner, merge_context)
      },
      MergePolicyEnum::Upgrade(mp) => {
        mp.find_forced_deletes_merges(segment_infos, inner, merge_context)
      },
      MergePolicyEnum::MergeOnFlush(mp) => {
        mp.find_forced_deletes_merges(segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::Force(mp) => {
        mp.find_forced_deletes_merges(segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::OnlyForceMerge(mp) => {
        mp.find_forced_deletes_merges(segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::ForceMergeDvUpdate(mp) => {
        mp.find_forced_deletes_merges(segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::SoftUpdatesConcurrently(mp) => {
        mp.find_forced_deletes_merges(segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::KeepFullyDeletedSegments(mp) => {
        mp.find_forced_deletes_merges(segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::Range(mp) => {
        mp.find_forced_deletes_merges(segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::Mock(mp) => {
        mp.find_forced_deletes_merges(segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::MockRandom(mp) => {
        mp.find_forced_deletes_merges(segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::ConcurrentAddIndexes(mp) => {
        mp.find_forced_deletes_merges(segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::SetDiagnostics(mp) => {
        mp.find_forced_deletes_merges(segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::MergeOnX(mp) => {
        mp.find_forced_deletes_merges(segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::LiveMaxMergeCount(mp) => {
        mp.find_forced_deletes_merges(segment_infos, inner, merge_context)
      },
    }
  }

  fn find_full_flush_merges<MC>(
    &self,
    merge_trigger: MergeTrigger,
    segment_infos: &SegmentInfos<D>,
    inner: Option<&Inner<D>>,
    merge_context: &MC,
  ) -> Result<Option<DefaultMergeSpecification<D>>>
  where
    MC: MergeContext<D>,
  {
    match self {
      MergePolicyEnum::No(mp) => {
        mp.find_full_flush_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      MergePolicyEnum::Tiered(mp) => {
        mp.find_full_flush_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      MergePolicyEnum::LogDoc(mp) => {
        mp.find_full_flush_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      MergePolicyEnum::LogBytesSize(mp) => {
        mp.find_full_flush_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::Alcoholic(mp) => {
        mp.find_full_flush_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::Predetermined(mp) => {
        mp.find_full_flush_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      MergePolicyEnum::OneMergeWrapping(mp) => {
        mp.find_full_flush_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      MergePolicyEnum::SoftDeletesRetention(mp) => {
        mp.find_full_flush_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      MergePolicyEnum::Upgrade(mp) => {
        mp.find_full_flush_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      MergePolicyEnum::MergeOnFlush(mp) => {
        mp.find_full_flush_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::Force(mp) => {
        mp.find_full_flush_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::OnlyForceMerge(mp) => {
        mp.find_full_flush_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::ForceMergeDvUpdate(mp) => {
        mp.find_full_flush_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::SoftUpdatesConcurrently(mp) => {
        mp.find_full_flush_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::KeepFullyDeletedSegments(mp) => {
        mp.find_full_flush_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::Range(mp) => {
        mp.find_full_flush_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::Mock(mp) => {
        mp.find_full_flush_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::MockRandom(mp) => {
        mp.find_full_flush_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::ConcurrentAddIndexes(mp) => {
        mp.find_full_flush_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::SetDiagnostics(mp) => {
        mp.find_full_flush_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::MergeOnX(mp) => {
        mp.find_full_flush_merges(merge_trigger, segment_infos, inner, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::LiveMaxMergeCount(mp) => {
        mp.find_full_flush_merges(merge_trigger, segment_infos, inner, merge_context)
      },
    }
  }

  fn use_compound_file<MC>(
    &self,
    infos: &SegmentInfos<D>,
    merged_info: &SegmentCommitInfo<D>,
    merge_context: &MC,
  ) -> Result<bool>
  where
    MC: MergeContext<D>,
  {
    match self {
      MergePolicyEnum::No(mp) => mp.use_compound_file(infos, merged_info, merge_context),
      MergePolicyEnum::Tiered(mp) => mp.use_compound_file(infos, merged_info, merge_context),
      MergePolicyEnum::LogDoc(mp) => mp.use_compound_file(infos, merged_info, merge_context),
      MergePolicyEnum::LogBytesSize(mp) => mp.use_compound_file(infos, merged_info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::Alcoholic(mp) => mp.use_compound_file(infos, merged_info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::Predetermined(mp) => mp.use_compound_file(infos, merged_info, merge_context),
      MergePolicyEnum::OneMergeWrapping(mp) => {
        mp.use_compound_file(infos, merged_info, merge_context)
      },
      MergePolicyEnum::SoftDeletesRetention(mp) => {
        mp.use_compound_file(infos, merged_info, merge_context)
      },
      MergePolicyEnum::Upgrade(mp) => mp.use_compound_file(infos, merged_info, merge_context),
      MergePolicyEnum::MergeOnFlush(mp) => mp.use_compound_file(infos, merged_info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::Force(mp) => mp.use_compound_file(infos, merged_info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::OnlyForceMerge(mp) => {
        mp.use_compound_file(infos, merged_info, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::ForceMergeDvUpdate(mp) => {
        mp.use_compound_file(infos, merged_info, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::SoftUpdatesConcurrently(mp) => {
        mp.use_compound_file(infos, merged_info, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::KeepFullyDeletedSegments(mp) => {
        mp.use_compound_file(infos, merged_info, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::Range(mp) => mp.use_compound_file(infos, merged_info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::Mock(mp) => mp.use_compound_file(infos, merged_info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::MockRandom(mp) => mp.use_compound_file(infos, merged_info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::ConcurrentAddIndexes(mp) => {
        mp.use_compound_file(infos, merged_info, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::SetDiagnostics(mp) => {
        mp.use_compound_file(infos, merged_info, merge_context)
      },
      #[cfg(test)]
      MergePolicyEnum::MergeOnX(mp) => mp.use_compound_file(infos, merged_info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::LiveMaxMergeCount(mp) => {
        mp.use_compound_file(infos, merged_info, merge_context)
      },
    }
  }

  fn size<MC>(&self, info: &SegmentCommitInfo<D>, merge_context: &MC) -> Result<i64>
  where
    MC: MergeContext<D>,
  {
    match self {
      MergePolicyEnum::No(mp) => mp.size(info, merge_context),
      MergePolicyEnum::Tiered(mp) => mp.size(info, merge_context),
      MergePolicyEnum::LogDoc(mp) => mp.size(info, merge_context),
      MergePolicyEnum::LogBytesSize(mp) => mp.size(info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::Alcoholic(mp) => mp.size(info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::Predetermined(mp) => mp.size(info, merge_context),
      MergePolicyEnum::OneMergeWrapping(mp) => mp.size(info, merge_context),
      MergePolicyEnum::SoftDeletesRetention(mp) => mp.size(info, merge_context),
      MergePolicyEnum::Upgrade(mp) => mp.size(info, merge_context),
      MergePolicyEnum::MergeOnFlush(mp) => mp.size(info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::Force(mp) => mp.size(info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::OnlyForceMerge(mp) => mp.size(info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::ForceMergeDvUpdate(mp) => mp.size(info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::SoftUpdatesConcurrently(mp) => mp.size(info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::KeepFullyDeletedSegments(mp) => mp.size(info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::Range(mp) => mp.size(info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::Mock(mp) => mp.size(info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::MockRandom(mp) => mp.size(info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::ConcurrentAddIndexes(mp) => mp.size(info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::SetDiagnostics(mp) => mp.size(info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::MergeOnX(mp) => mp.size(info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::LiveMaxMergeCount(mp) => mp.size(info, merge_context),
    }
  }

  fn max_full_flush_merge_size(&self) -> i64 {
    match self {
      MergePolicyEnum::No(mp) => MergePolicy::<D>::max_full_flush_merge_size(mp),
      MergePolicyEnum::Tiered(mp) => MergePolicy::<D>::max_full_flush_merge_size(mp),
      MergePolicyEnum::LogDoc(mp) => MergePolicy::<D>::max_full_flush_merge_size(mp),
      MergePolicyEnum::LogBytesSize(mp) => MergePolicy::<D>::max_full_flush_merge_size(mp),
      #[cfg(test)]
      MergePolicyEnum::Alcoholic(mp) => MergePolicy::<D>::max_full_flush_merge_size(mp),
      #[cfg(test)]
      MergePolicyEnum::Predetermined(mp) => MergePolicy::<D>::max_full_flush_merge_size(mp),
      MergePolicyEnum::OneMergeWrapping(mp) => MergePolicy::<D>::max_full_flush_merge_size(mp),
      MergePolicyEnum::SoftDeletesRetention(mp) => MergePolicy::<D>::max_full_flush_merge_size(mp),
      MergePolicyEnum::Upgrade(mp) => MergePolicy::<D>::max_full_flush_merge_size(mp),
      MergePolicyEnum::MergeOnFlush(mp) => MergePolicy::<D>::max_full_flush_merge_size(mp),
      #[cfg(test)]
      MergePolicyEnum::Force(mp) => MergePolicy::<D>::max_full_flush_merge_size(mp),
      #[cfg(test)]
      MergePolicyEnum::OnlyForceMerge(mp) => MergePolicy::<D>::max_full_flush_merge_size(mp),
      #[cfg(test)]
      MergePolicyEnum::ForceMergeDvUpdate(mp) => MergePolicy::<D>::max_full_flush_merge_size(mp),
      #[cfg(test)]
      MergePolicyEnum::SoftUpdatesConcurrently(mp) => {
        MergePolicy::<D>::max_full_flush_merge_size(mp)
      },
      #[cfg(test)]
      MergePolicyEnum::KeepFullyDeletedSegments(mp) => {
        MergePolicy::<D>::max_full_flush_merge_size(mp)
      },
      #[cfg(test)]
      MergePolicyEnum::Range(mp) => MergePolicy::<D>::max_full_flush_merge_size(mp),
      #[cfg(test)]
      MergePolicyEnum::Mock(mp) => MergePolicy::<D>::max_full_flush_merge_size(mp),
      #[cfg(test)]
      MergePolicyEnum::MockRandom(mp) => MergePolicy::<D>::max_full_flush_merge_size(mp),
      #[cfg(test)]
      MergePolicyEnum::ConcurrentAddIndexes(mp) => MergePolicy::<D>::max_full_flush_merge_size(mp),
      #[cfg(test)]
      MergePolicyEnum::SetDiagnostics(mp) => MergePolicy::<D>::max_full_flush_merge_size(mp),
      #[cfg(test)]
      MergePolicyEnum::MergeOnX(mp) => MergePolicy::<D>::max_full_flush_merge_size(mp),
      #[cfg(test)]
      MergePolicyEnum::LiveMaxMergeCount(mp) => MergePolicy::<D>::max_full_flush_merge_size(mp),
    }
  }

  fn has_merged<MC>(
    &self,
    infos: &SegmentInfos<D>,
    info: &SegmentCommitInfo<D>,
    merge_context: &MC,
  ) -> Result<bool>
  where
    MC: MergeContext<D>,
  {
    match self {
      MergePolicyEnum::No(mp) => mp.has_merged(infos, info, merge_context),
      MergePolicyEnum::Tiered(mp) => mp.has_merged(infos, info, merge_context),
      MergePolicyEnum::LogDoc(mp) => mp.has_merged(infos, info, merge_context),
      MergePolicyEnum::LogBytesSize(mp) => mp.has_merged(infos, info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::Alcoholic(mp) => mp.has_merged(infos, info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::Predetermined(mp) => mp.has_merged(infos, info, merge_context),
      MergePolicyEnum::OneMergeWrapping(mp) => mp.has_merged(infos, info, merge_context),
      MergePolicyEnum::SoftDeletesRetention(mp) => mp.has_merged(infos, info, merge_context),
      MergePolicyEnum::Upgrade(mp) => mp.has_merged(infos, info, merge_context),
      MergePolicyEnum::MergeOnFlush(mp) => mp.has_merged(infos, info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::Force(mp) => mp.has_merged(infos, info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::OnlyForceMerge(mp) => mp.has_merged(infos, info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::ForceMergeDvUpdate(mp) => mp.has_merged(infos, info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::SoftUpdatesConcurrently(mp) => mp.has_merged(infos, info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::KeepFullyDeletedSegments(mp) => mp.has_merged(infos, info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::Range(mp) => mp.has_merged(infos, info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::Mock(mp) => mp.has_merged(infos, info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::MockRandom(mp) => mp.has_merged(infos, info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::ConcurrentAddIndexes(mp) => mp.has_merged(infos, info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::SetDiagnostics(mp) => mp.has_merged(infos, info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::MergeOnX(mp) => mp.has_merged(infos, info, merge_context),
      #[cfg(test)]
      MergePolicyEnum::LiveMaxMergeCount(mp) => mp.has_merged(infos, info, merge_context),
    }
  }

  fn keep_fully_deleted_segment<F>(&self, reader_supplier: F) -> Result<bool>
  where
    F: Fn() -> Result<DefaultLeafReader<D>>,
  {
    match self {
      MergePolicyEnum::No(mp) => mp.keep_fully_deleted_segment(reader_supplier),
      MergePolicyEnum::Tiered(mp) => mp.keep_fully_deleted_segment(reader_supplier),
      MergePolicyEnum::LogDoc(mp) => mp.keep_fully_deleted_segment(reader_supplier),
      MergePolicyEnum::LogBytesSize(mp) => mp.keep_fully_deleted_segment(reader_supplier),
      #[cfg(test)]
      MergePolicyEnum::Alcoholic(mp) => mp.keep_fully_deleted_segment(reader_supplier),
      #[cfg(test)]
      MergePolicyEnum::Predetermined(mp) => mp.keep_fully_deleted_segment(reader_supplier),
      MergePolicyEnum::OneMergeWrapping(mp) => mp.keep_fully_deleted_segment(reader_supplier),
      MergePolicyEnum::SoftDeletesRetention(mp) => mp.keep_fully_deleted_segment(reader_supplier),
      MergePolicyEnum::Upgrade(mp) => mp.keep_fully_deleted_segment(reader_supplier),
      MergePolicyEnum::MergeOnFlush(mp) => mp.keep_fully_deleted_segment(reader_supplier),
      #[cfg(test)]
      MergePolicyEnum::Force(mp) => mp.keep_fully_deleted_segment(reader_supplier),
      #[cfg(test)]
      MergePolicyEnum::OnlyForceMerge(mp) => mp.keep_fully_deleted_segment(reader_supplier),
      #[cfg(test)]
      MergePolicyEnum::ForceMergeDvUpdate(mp) => mp.keep_fully_deleted_segment(reader_supplier),
      #[cfg(test)]
      MergePolicyEnum::SoftUpdatesConcurrently(mp) => {
        mp.keep_fully_deleted_segment(reader_supplier)
      },
      #[cfg(test)]
      MergePolicyEnum::KeepFullyDeletedSegments(mp) => {
        mp.keep_fully_deleted_segment(reader_supplier)
      },
      #[cfg(test)]
      MergePolicyEnum::Range(mp) => mp.keep_fully_deleted_segment(reader_supplier),
      #[cfg(test)]
      MergePolicyEnum::Mock(mp) => mp.keep_fully_deleted_segment(reader_supplier),
      #[cfg(test)]
      MergePolicyEnum::MockRandom(mp) => mp.keep_fully_deleted_segment(reader_supplier),
      #[cfg(test)]
      MergePolicyEnum::ConcurrentAddIndexes(mp) => mp.keep_fully_deleted_segment(reader_supplier),
      #[cfg(test)]
      MergePolicyEnum::SetDiagnostics(mp) => mp.keep_fully_deleted_segment(reader_supplier),
      #[cfg(test)]
      MergePolicyEnum::MergeOnX(mp) => mp.keep_fully_deleted_segment(reader_supplier),
      #[cfg(test)]
      MergePolicyEnum::LiveMaxMergeCount(mp) => mp.keep_fully_deleted_segment(reader_supplier),
    }
  }

  fn num_deletes_to_merge<F>(
    &self,
    info: &SegmentCommitInfo<D>,
    del_count: i32,
    reader_supplier: &F,
  ) -> Result<i32>
  where
    F: Fn() -> Result<DefaultLeafReader<D>>,
  {
    match self {
      MergePolicyEnum::No(mp) => mp.num_deletes_to_merge(info, del_count, reader_supplier),
      MergePolicyEnum::Tiered(mp) => mp.num_deletes_to_merge(info, del_count, reader_supplier),
      MergePolicyEnum::LogDoc(mp) => mp.num_deletes_to_merge(info, del_count, reader_supplier),
      MergePolicyEnum::LogBytesSize(mp) => {
        mp.num_deletes_to_merge(info, del_count, reader_supplier)
      },
      #[cfg(test)]
      MergePolicyEnum::Alcoholic(mp) => mp.num_deletes_to_merge(info, del_count, reader_supplier),
      #[cfg(test)]
      MergePolicyEnum::Predetermined(mp) => {
        mp.num_deletes_to_merge(info, del_count, reader_supplier)
      },
      MergePolicyEnum::OneMergeWrapping(mp) => {
        mp.num_deletes_to_merge(info, del_count, reader_supplier)
      },
      MergePolicyEnum::SoftDeletesRetention(mp) => {
        mp.num_deletes_to_merge(info, del_count, reader_supplier)
      },
      MergePolicyEnum::Upgrade(mp) => mp.num_deletes_to_merge(info, del_count, reader_supplier),
      MergePolicyEnum::MergeOnFlush(mp) => {
        mp.num_deletes_to_merge(info, del_count, reader_supplier)
      },
      #[cfg(test)]
      MergePolicyEnum::Force(mp) => mp.num_deletes_to_merge(info, del_count, reader_supplier),
      #[cfg(test)]
      MergePolicyEnum::OnlyForceMerge(mp) => {
        mp.num_deletes_to_merge(info, del_count, reader_supplier)
      },
      #[cfg(test)]
      MergePolicyEnum::ForceMergeDvUpdate(mp) => {
        mp.num_deletes_to_merge(info, del_count, reader_supplier)
      },
      #[cfg(test)]
      MergePolicyEnum::SoftUpdatesConcurrently(mp) => {
        mp.num_deletes_to_merge(info, del_count, reader_supplier)
      },
      #[cfg(test)]
      MergePolicyEnum::KeepFullyDeletedSegments(mp) => {
        mp.num_deletes_to_merge(info, del_count, reader_supplier)
      },
      #[cfg(test)]
      MergePolicyEnum::Range(mp) => mp.num_deletes_to_merge(info, del_count, reader_supplier),
      #[cfg(test)]
      MergePolicyEnum::Mock(mp) => mp.num_deletes_to_merge(info, del_count, reader_supplier),
      #[cfg(test)]
      MergePolicyEnum::MockRandom(mp) => mp.num_deletes_to_merge(info, del_count, reader_supplier),
      #[cfg(test)]
      MergePolicyEnum::ConcurrentAddIndexes(mp) => {
        mp.num_deletes_to_merge(info, del_count, reader_supplier)
      },
      #[cfg(test)]
      MergePolicyEnum::SetDiagnostics(mp) => {
        mp.num_deletes_to_merge(info, del_count, reader_supplier)
      },
      #[cfg(test)]
      MergePolicyEnum::MergeOnX(mp) => mp.num_deletes_to_merge(info, del_count, reader_supplier),
      #[cfg(test)]
      MergePolicyEnum::LiveMaxMergeCount(mp) => {
        mp.num_deletes_to_merge(info, del_count, reader_supplier)
      },
    }
  }

  fn seg_string<MC>(&self, merge_context: &MC, infos: &[SegmentCommitInfo<D>]) -> String
  where
    MC: MergeContext<D>,
  {
    match self {
      MergePolicyEnum::No(mp) => mp.seg_string(merge_context, infos),
      MergePolicyEnum::Tiered(mp) => mp.seg_string(merge_context, infos),
      MergePolicyEnum::LogDoc(mp) => mp.seg_string(merge_context, infos),
      MergePolicyEnum::LogBytesSize(mp) => mp.seg_string(merge_context, infos),
      #[cfg(test)]
      MergePolicyEnum::Alcoholic(mp) => mp.seg_string(merge_context, infos),
      #[cfg(test)]
      MergePolicyEnum::Predetermined(mp) => mp.seg_string(merge_context, infos),
      MergePolicyEnum::OneMergeWrapping(mp) => mp.seg_string(merge_context, infos),
      MergePolicyEnum::SoftDeletesRetention(mp) => mp.seg_string(merge_context, infos),
      MergePolicyEnum::Upgrade(mp) => mp.seg_string(merge_context, infos),
      MergePolicyEnum::MergeOnFlush(mp) => mp.seg_string(merge_context, infos),
      #[cfg(test)]
      MergePolicyEnum::Force(mp) => mp.seg_string(merge_context, infos),
      #[cfg(test)]
      MergePolicyEnum::OnlyForceMerge(mp) => mp.seg_string(merge_context, infos),
      #[cfg(test)]
      MergePolicyEnum::ForceMergeDvUpdate(mp) => mp.seg_string(merge_context, infos),
      #[cfg(test)]
      MergePolicyEnum::SoftUpdatesConcurrently(mp) => mp.seg_string(merge_context, infos),
      #[cfg(test)]
      MergePolicyEnum::KeepFullyDeletedSegments(mp) => mp.seg_string(merge_context, infos),
      #[cfg(test)]
      MergePolicyEnum::Range(mp) => mp.seg_string(merge_context, infos),
      #[cfg(test)]
      MergePolicyEnum::Mock(mp) => mp.seg_string(merge_context, infos),
      #[cfg(test)]
      MergePolicyEnum::MockRandom(mp) => mp.seg_string(merge_context, infos),
      #[cfg(test)]
      MergePolicyEnum::ConcurrentAddIndexes(mp) => mp.seg_string(merge_context, infos),
      #[cfg(test)]
      MergePolicyEnum::SetDiagnostics(mp) => mp.seg_string(merge_context, infos),
      #[cfg(test)]
      MergePolicyEnum::MergeOnX(mp) => mp.seg_string(merge_context, infos),
      #[cfg(test)]
      MergePolicyEnum::LiveMaxMergeCount(mp) => mp.seg_string(merge_context, infos),
    }
  }

  fn message<MC>(&self, message: &str, merge_context: &MC) -> Result<()>
  where
    MC: MergeContext<D>,
  {
    match self {
      MergePolicyEnum::No(mp) => mp.message(message, merge_context),
      MergePolicyEnum::Tiered(mp) => mp.message(message, merge_context),
      MergePolicyEnum::LogDoc(mp) => mp.message(message, merge_context),
      MergePolicyEnum::LogBytesSize(mp) => mp.message(message, merge_context),
      #[cfg(test)]
      MergePolicyEnum::Alcoholic(mp) => mp.message(message, merge_context),
      #[cfg(test)]
      MergePolicyEnum::Predetermined(mp) => mp.message(message, merge_context),
      MergePolicyEnum::OneMergeWrapping(mp) => mp.message(message, merge_context),
      MergePolicyEnum::SoftDeletesRetention(mp) => mp.message(message, merge_context),
      MergePolicyEnum::Upgrade(mp) => mp.message(message, merge_context),
      MergePolicyEnum::MergeOnFlush(mp) => mp.message(message, merge_context),
      #[cfg(test)]
      MergePolicyEnum::Force(mp) => mp.message(message, merge_context),
      #[cfg(test)]
      MergePolicyEnum::OnlyForceMerge(mp) => mp.message(message, merge_context),
      #[cfg(test)]
      MergePolicyEnum::ForceMergeDvUpdate(mp) => mp.message(message, merge_context),
      #[cfg(test)]
      MergePolicyEnum::SoftUpdatesConcurrently(mp) => mp.message(message, merge_context),
      #[cfg(test)]
      MergePolicyEnum::KeepFullyDeletedSegments(mp) => mp.message(message, merge_context),
      #[cfg(test)]
      MergePolicyEnum::Range(mp) => mp.message(message, merge_context),
      #[cfg(test)]
      MergePolicyEnum::Mock(mp) => mp.message(message, merge_context),
      #[cfg(test)]
      MergePolicyEnum::MockRandom(mp) => mp.message(message, merge_context),
      #[cfg(test)]
      MergePolicyEnum::ConcurrentAddIndexes(mp) => mp.message(message, merge_context),
      #[cfg(test)]
      MergePolicyEnum::SetDiagnostics(mp) => mp.message(message, merge_context),
      #[cfg(test)]
      MergePolicyEnum::MergeOnX(mp) => mp.message(message, merge_context),
      #[cfg(test)]
      MergePolicyEnum::LiveMaxMergeCount(mp) => mp.message(message, merge_context),
    }
  }

  fn verbose<MC>(&self, merge_context: &MC) -> bool
  where
    MC: MergeContext<D>,
  {
    match self {
      MergePolicyEnum::No(mp) => mp.verbose(merge_context),
      MergePolicyEnum::Tiered(mp) => mp.verbose(merge_context),
      MergePolicyEnum::LogDoc(mp) => mp.verbose(merge_context),
      MergePolicyEnum::LogBytesSize(mp) => mp.verbose(merge_context),
      #[cfg(test)]
      MergePolicyEnum::Alcoholic(mp) => mp.verbose(merge_context),
      #[cfg(test)]
      MergePolicyEnum::Predetermined(mp) => mp.verbose(merge_context),
      MergePolicyEnum::OneMergeWrapping(mp) => mp.verbose(merge_context),
      MergePolicyEnum::SoftDeletesRetention(mp) => mp.verbose(merge_context),
      MergePolicyEnum::Upgrade(mp) => mp.verbose(merge_context),
      MergePolicyEnum::MergeOnFlush(mp) => mp.verbose(merge_context),
      #[cfg(test)]
      MergePolicyEnum::Force(mp) => mp.verbose(merge_context),
      #[cfg(test)]
      MergePolicyEnum::OnlyForceMerge(mp) => mp.verbose(merge_context),
      #[cfg(test)]
      MergePolicyEnum::ForceMergeDvUpdate(mp) => mp.verbose(merge_context),
      #[cfg(test)]
      MergePolicyEnum::SoftUpdatesConcurrently(mp) => mp.verbose(merge_context),
      #[cfg(test)]
      MergePolicyEnum::KeepFullyDeletedSegments(mp) => mp.verbose(merge_context),
      #[cfg(test)]
      MergePolicyEnum::Range(mp) => mp.verbose(merge_context),
      #[cfg(test)]
      MergePolicyEnum::Mock(mp) => mp.verbose(merge_context),
      #[cfg(test)]
      MergePolicyEnum::MockRandom(mp) => mp.verbose(merge_context),
      #[cfg(test)]
      MergePolicyEnum::ConcurrentAddIndexes(mp) => mp.verbose(merge_context),
      #[cfg(test)]
      MergePolicyEnum::SetDiagnostics(mp) => mp.verbose(merge_context),
      #[cfg(test)]
      MergePolicyEnum::MergeOnX(mp) => mp.verbose(merge_context),
      #[cfg(test)]
      MergePolicyEnum::LiveMaxMergeCount(mp) => mp.verbose(merge_context),
    }
  }
}

#[derive(Clone)]
pub struct MergePolicyBase {
  /// If the size of the merge segment exceeds this ratio of the total index size
  /// then it will remain in non-compound format.
  pub(crate) no_cfs_ratio: f64,
  /// If the size of the merged segment exceeds this value
  /// then it will not use compound file format.
  pub(crate) max_cfs_segment_size: i64,
}
impl Default for MergePolicyBase {
  fn default() -> Self {
    Self {
      no_cfs_ratio: DEFAULT_NO_CFS_RATIO,
      max_cfs_segment_size: DEFAULT_MAX_CFS_SEGMENT_SIZE,
    }
  }
}
impl MergePolicyBase {
  pub fn new(no_cfs_ratio: f64, max_cfs_segment_size: i64) -> Self {
    Self {
      no_cfs_ratio,
      max_cfs_segment_size,
    }
  }
  /// Returns current `noCFSRatio`.
  ///
  /// See `set_no_cfs_ratio`.
  pub fn get_no_cfs_ratio(&self) -> f64 {
    self.no_cfs_ratio
  }

  /// If a merged segment will be more than this percentage of the total size of the index,
  /// leave the segment as non-compound file even if compound file is enabled.
  ///
  /// Set to `1.0` to always use CFS regardless of merge size.
  pub fn set_no_cfs_ratio(&mut self, ratio: f64) -> Result<()> {
    if !(0.0..=1.0).contains(&ratio) {
      return Err(LuceneError::illegal_argument(format!(
        "noCFSRatio must be 0.0 to 1.0 inclusive; got {}",
        ratio
      )));
    }
    self.no_cfs_ratio = ratio;
    Ok(())
  }

  /// Returns the largest size allowed for a compound file segment (in MB).
  pub fn get_max_cfs_segment_size_mb(&self) -> f64 {
    self.max_cfs_segment_size as f64 / 1024.0 / 1024.0
  }

  /// If a merged segment will be more than this value (MB), leave the segment as non-compound
  /// even if compound file is enabled.
  ///
  /// Set this to `f64::INFINITY` and `noCFSRatio` to `1.0` to always use CFS regardless of size.
  pub fn set_max_cfs_segment_size_mb(&mut self, mut v: f64) -> Result<()> {
    if v < 0.0 {
      return Err(LuceneError::illegal_argument(format!(
        "maxCFSSegmentSizeMB must be >=0 (got {})",
        v
      )));
    }
    v *= 1024.0 * 1024.0;

    self.max_cfs_segment_size = if v > i64::MAX as f64 {
      i64::MAX
    } else {
      v as i64
    };

    Ok(())
  }
}
pub type OneMergeSR<D> = OneMerge<D, DefaultLeafReader<D>>;
/// OneMerge provides the information necessary to perform an individual
/// primitive merge operation, resulting in a single new segment.
///
/// The merge spec includes:
/// - the subset of segments to be merged
/// - whether the new segment should use the compound file format
pub struct OneMerge<D, CR>
where
  D: Directory,
  CR: CodecReader,
{
  hook: OneMergeHook<D, CR>,
  pub(crate) is_external: bool,
  pub(crate) uses_pooled_readers: bool,
  /// Estimated size in bytes of the merged segment.
  pub estimated_merge_bytes: Arc<AtomicI64>,
  /// Sum of sizeInBytes of all SegmentInfos; set by IW.mergeInit
  pub(crate) total_merge_bytes: AtomicI64,
  merge_readers: Vec<MergeReader<CR>>,
  pub(crate) merge_start_ns: Arc<Mutex<Instant>>,
  /// Total number of documents in segments to be merged, not accounting for deletions.
  pub(crate) total_max_doc: i32,
  #[cfg(test)]
  pub(crate) segments: Vec<SegmentDocAndID>,
  pub(crate) stat: MergeStat,
  pub info: Option<SegmentCommitInfo<D>>,
}

#[derive(Clone)]
pub struct MergeStat {
  pub(crate) id: Identity,
  pub(crate) register_done: Arc<AtomicBool>,
  state: Arc<Mutex<MergeStatState>>,
  completion: Arc<MergeCompletion>,
  merge_progress: Arc<OneMergeProgress>,
  /// Segments to be merged.
  /// `SegmentInfo::name` and `SegmentInfo::id`.
  pub(crate) segments: Vec<String>,
  pub(crate) merge_gen: i64,
}
impl Default for MergeStat {
  fn default() -> Self {
    Self {
      id: Identity::new(),
      register_done: Arc::new(AtomicBool::new(false)),
      state: Arc::new(Mutex::new(MergeStatState::default())),
      completion: Arc::new(MergeCompletion::new()),
      merge_progress: Arc::new(OneMergeProgress::new()),
      segments: vec![],
      merge_gen: 0,
    }
  }
}

#[derive(Default)]
struct MergeCompletion {
  /// `None` means the merge is still running or has not started yet.
  /// `Some(success)` means the merge has finished, and `success` records whether it completed
  /// successfully.
  state: Mutex<Option<bool>>,
  completed: Condvar,
}

impl MergeCompletion {
  fn new() -> Self {
    Self {
      state: Mutex::new(None),
      completed: Condvar::new(),
    }
  }

  fn complete(&self, success: bool) -> bool {
    let mut state = self.state.lock();
    if state.is_some() {
      return false;
    }
    *state = Some(success);
    self.completed.notify_all();
    true
  }

  fn is_done(&self) -> bool {
    self.state.lock().is_some()
  }

  fn completed_successfully(&self) -> Option<bool> {
    *self.state.lock()
  }

  fn await_(&self) {
    let mut state = self.state.lock();
    while state.is_none() {
      self.completed.wait(&mut state);
    }
  }

  fn await_until(&self, deadline: Instant) -> bool {
    let mut state = self.state.lock();
    loop {
      if state.is_some() {
        return true;
      }

      let now = Instant::now();
      if now >= deadline {
        return false;
      }

      if self
        .completed
        .wait_for(&mut state, deadline - now)
        .timed_out()
      {
        return state.is_some();
      }
    }
  }
}

#[derive(Default)]
struct MergeStatState {
  max_num_segments: i32,
  info_id: Option<String>,
  name: Option<String>,
  error: Option<CaughtResult>,
  #[cfg(test)]
  max_doc: Option<i32>,
}

impl MergeStatState {
  fn new() -> Self {
    Self {
      max_num_segments: -1,
      info_id: None,
      name: None,
      error: None,
      #[cfg(test)]
      max_doc: None,
    }
  }
}

impl PartialEq for MergeStat {
  fn eq(&self, other: &Self) -> bool {
    self.id.eq(&other.id)
  }
}
impl Eq for MergeStat {}

impl Hash for MergeStat {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.id.hash(state);
  }
}

impl MergeStat {
  pub(crate) fn await_all(merges: &[MergeStat]) {
    for merge in merges {
      merge.await_();
    }
  }

  fn await_all_with_timeout(merges: &[MergeStat], timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    for merge in merges {
      if !merge.await_until(deadline) {
        return false;
      }
    }
    true
  }

  pub(crate) fn await_until(&self, deadline: Instant) -> bool {
    self.completion.await_until(deadline)
  }

  pub(crate) fn await_(&self) {
    self.completion.await_()
  }

  pub(crate) fn complete(&self, success: bool) -> bool {
    self.completion.complete(success)
  }

  pub(crate) fn has_finished(&self) -> bool {
    self.completion.is_done()
  }

  pub(crate) fn has_completed_successfully(&self) -> Option<bool> {
    self.completion.completed_successfully()
  }

  pub(crate) fn has_exception(&self) -> bool {
    self.state.lock().error.is_some()
  }

  pub(crate) fn set_exception(&self, error: CaughtResult) {
    self.state.lock().error = Some(error);
  }

  pub(crate) fn get_exception(&self) -> Option<CaughtResult> {
    self
      .state
      .lock()
      .error
      .as_ref()
      .and_then(|error| error.clone_caught_failure("panic while retrieving a merge exception"))
  }

  pub(crate) fn max_num_segments(&self) -> i32 {
    self.state.lock().max_num_segments
  }

  pub(crate) fn set_max_num_segments(&self, max_num_segments: i32) {
    self.state.lock().max_num_segments = max_num_segments;
  }

  pub(crate) fn info_id(&self) -> Option<String> {
    self.state.lock().info_id.clone()
  }

  pub(crate) fn name(&self) -> Option<String> {
    self.state.lock().name.clone()
  }

  #[cfg(test)]
  pub(crate) fn max_doc(&self) -> Option<i32> {
    self.state.lock().max_doc
  }

  pub(crate) fn set_merge_info(&self, info_id: String, name: String) {
    let mut state = self.state.lock();
    state.info_id = Some(info_id);
    state.name = Some(name);
  }

  #[cfg(test)]
  pub(crate) fn set_max_doc(&self, max_doc: i32) {
    self.state.lock().max_doc = Some(max_doc);
  }

  pub(crate) fn clear_merge_info(&self) {
    let mut state = self.state.lock();
    state.info_id = None;
    state.name = None;
    #[cfg(test)]
    {
      state.max_doc = None;
    }
  }

  pub(crate) fn set_aborted(&self) {
    self.merge_progress.abort();
  }

  pub(crate) fn is_aborted(&self) -> bool {
    self.merge_progress.is_aborted()
  }

  pub(crate) fn set_merge_thread(&self) {
    self.merge_progress.set_merge_thread();
  }
}

impl<D, CR> OneMerge<D, CR>
where
  D: Directory,
  CR: CodecReader,
{
  pub fn new(segments: Vec<SegmentDocAndID>) -> Result<Self> {
    if segments.is_empty() {
      return Err(LuceneError::illegal_state(
        "segments must include at least one segment",
      ));
    }
    let mut v = Vec::with_capacity(segments.len());
    let mut total_max_doc = 0;
    #[cfg(test)]
    let original_segments = segments.clone();
    for s in segments.into_iter() {
      v.push(s.seg_id);
      total_max_doc += s.max_doc
    }

    let merge_progress = Arc::new(OneMergeProgress::new());
    Ok(Self {
      hook: OneMergeHook::<D, CR>::Default,
      is_external: false,
      uses_pooled_readers: true,
      estimated_merge_bytes: Arc::new(AtomicI64::new(0)),
      total_merge_bytes: AtomicI64::new(0),
      merge_readers: Vec::new(),
      merge_start_ns: Arc::new(Mutex::new(Instant::now())),
      total_max_doc,
      #[cfg(test)]
      segments: original_segments,
      stat: MergeStat {
        id: Identity::new(),
        register_done: Arc::new(AtomicBool::new(false)),
        state: Arc::new(Mutex::new(MergeStatState::new())),
        completion: Arc::new(MergeCompletion::new()),
        merge_progress,
        segments: v,
        merge_gen: 0,
      },
      info: None,
    })
  }
  pub fn from_meta(segments: &[SegmentCommitInfoMeta<'_, D>]) -> Result<Self> {
    let mut segments_meta = Vec::with_capacity(segments.len());
    for v in segments {
      segments_meta.push(SegmentDocAndID::new(
        v.seg_info.info.get_id_key().to_string(),
        v.max_doc,
      ))
    }
    Self::new(segments_meta)
  }
  /// Creates wrapping.
  pub(crate) fn from_other(one_merge: OneMerge<D, CR>) -> Self {
    let stat = MergeStat::default();
    let one_merge = Self {
      hook: OneMergeHook::<D, CR>::Default,
      merge_readers: one_merge.merge_readers,
      total_max_doc: one_merge.total_max_doc,
      #[cfg(test)]
      segments: one_merge.segments,
      uses_pooled_readers: one_merge.uses_pooled_readers,
      is_external: false,
      estimated_merge_bytes: Arc::new(AtomicI64::new(0)),
      total_merge_bytes: AtomicI64::new(0),
      merge_start_ns: Arc::new(Mutex::new(Instant::now())),
      stat,
      info: None,
    };
    one_merge.stat.set_max_num_segments(-1);
    one_merge.stat.clear_merge_info();
    one_merge
  }
  /// Create a OneMerge directly from CodecReaders. Used to merge incoming readers in
  /// IndexWriter::add_indexes(reader...). This OneMerge works directly on readers and has an
  /// empty segments list.
  pub fn from_codec_readers(readers: Vec<CR>) -> Result<Self> {
    let mut merge_readers = Vec::with_capacity(readers.len());
    let mut total_docs = 0;

    for r in readers.into_iter() {
      let live_docs = r.get_live_docs()?;
      total_docs += r.num_docs()?;
      merge_readers.push(MergeReader::new(r, live_docs));
    }

    let merge_progress = Arc::new(OneMergeProgress::new());
    Ok(Self {
      hook: OneMergeHook::<D, CR>::Default,
      is_external: false,
      uses_pooled_readers: false,
      estimated_merge_bytes: Arc::new(AtomicI64::new(0)),
      total_merge_bytes: AtomicI64::new(0),
      merge_readers,
      merge_start_ns: Arc::new(Mutex::new(Instant::now())),
      total_max_doc: total_docs,
      #[cfg(test)]
      segments: Vec::new(),
      stat: MergeStat {
        id: Identity::new(),
        register_done: Arc::new(AtomicBool::new(false)),
        state: Arc::new(Mutex::new(MergeStatState::new())),
        completion: Arc::new(MergeCompletion::new()),
        merge_progress,
        segments: Vec::new(),
        merge_gen: 0,
      },
      info: None,
    })
  }
  /// Called by IndexWriter after the merge started and from the thread that will be executing the merge.
  pub fn merge_init(&self) {
    self.stat.set_merge_thread()
  }
  /// Record that an error occurred while executing this merge.
  pub fn set_exception(&self, error: CaughtResult) {
    self.stat.set_exception(error);
  }

  /// Retrieve previous error set by `set_exception`.
  pub fn get_exception(&self) -> Option<CaughtResult> {
    self.stat.get_exception()
  }

  /// Returns the total size in bytes of this merge. Note that this does not indicate the size of
  /// the merged segment, but the input total size. This is only set once the merge is initialized
  /// by `IndexWriter`.
  pub fn total_bytes_size(&self) -> i64 {
    self.total_merge_bytes.load(Ordering::SeqCst)
  }

  pub fn get_store_merge_info(&self) -> MergeInfo {
    MergeInfo::new(
      self.total_max_doc,
      self.estimated_merge_bytes.load(Ordering::SeqCst),
      self.is_external,
      self.stat.max_num_segments(),
    )
  }
  pub fn get_merge_progress(&self) -> Arc<OneMergeProgress> {
    self.stat.merge_progress.clone()
  }

  pub(crate) fn merge_start_time(&self) -> Instant {
    *self.merge_start_ns.lock()
  }

  pub(crate) fn set_merge_start_time(&self, merge_start_time: Instant) {
    *self.merge_start_ns.lock() = merge_start_time;
  }

  pub fn set_aborted(&self) -> Result<()> {
    self.stat.set_aborted();
    Ok(())
  }
  pub fn is_aborted(&self) -> bool {
    self.stat.is_aborted()
  }
  pub fn check_aborted(&self) -> Result<()> {
    if self.is_aborted() {
      return Err(LuceneError::merge_abort("merge is aborted"));
    }
    Ok(())
  }
  pub fn get_merge_reader(&self) -> &[MergeReader<CR>] {
    self.merge_readers.as_slice()
  }

  pub(crate) fn has_finished(&self) -> bool {
    self.stat.has_finished()
  }

  pub(crate) fn has_completed_successfully(&self) -> Option<bool> {
    self.stat.has_completed_successfully()
  }
  pub(crate) fn with_hook(mut self, hook: OneMergeHook<D, CR>) -> Self {
    self.hook = hook;
    self
  }

  pub(crate) fn replace_hook(&mut self, hook: OneMergeHook<D, CR>) -> OneMergeHook<D, CR> {
    std::mem::replace(&mut self.hook, hook)
  }

  pub fn wrap_for_merge(&self, reader: CR) -> Result<CR> {
    <OneMergeHook<D, CR> as OneMergeBase<D, CR>>::wrap_for_merge(&self.hook, reader)
  }

  pub fn reorder<CR1, D1>(&self, reader: &CR1, dir: D1) -> Result<Option<DummyDocMap>>
  where
    CR1: CodecReader,
    D1: Directory,
  {
    <OneMergeHook<D, CR> as OneMergeBase<D, CR>>::reorder::<CR1, D1>(&self.hook, reader, dir)
  }

  pub fn set_merge_info(&mut self, info: SegmentCommitInfo<D>) {
    <OneMergeHook<D, CR> as OneMergeBase<D, CR>>::set_merge_info(
      &self.hook,
      &self.stat,
      &mut self.info,
      info,
    );
  }

  pub fn get_merge_info_mut(&mut self) -> Option<&mut SegmentCommitInfo<D>> {
    self.info.as_mut()
  }

  pub(crate) fn merge_finished(
    &self,
    inner: &mut Inner<D>,
    success: bool,
    segment_dropped: bool,
  ) -> Result<()> {
    <OneMergeHook<D, CR> as OneMergeBase<D, CR>>::merge_finished(
      &self.hook,
      inner,
      &self.stat,
      success,
      segment_dropped,
    )
  }

  pub fn on_merge_complete(&self, inner: &mut Inner<D>) -> Result<()> {
    <OneMergeHook<D, CR> as OneMergeBase<D, CR>>::on_merge_complete(
      &self.hook,
      inner,
      &self.stat,
      &self.info,
      self.is_aborted(),
    )
  }

  pub fn init_merge_readers<F>(&mut self, reader_factory: F) -> Result<()>
  where
    F: FnMut(&String) -> Result<MergeReader<CR>>,
  {
    <OneMergeHook<D, CR> as OneMergeBase<D, CR>>::init_merge_readers::<F>(
      &self.hook,
      &mut self.merge_readers,
      &self.stat,
      reader_factory,
    )
  }

  pub(crate) fn close<F>(
    &mut self,
    inner: &mut Inner<D>,
    success: bool,
    segment_dropped: bool,
    mut reader_consumer: F,
  ) -> Result<()>
  where
    F: FnMut(&mut Inner<D>, &MergeReader<CR>) -> Result<()>,
  {
    if !self.stat.complete(success) {
      return Err(LuceneError::illegal_state("merge has already finished"));
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      self.merge_finished(inner, success, segment_dropped)?;
      Ok(())
    }));
    let merge_readers = std::mem::take(&mut self.merge_readers);
    IOUtils::apply_to_all(&merge_readers, |merge_reader| {
      reader_consumer(inner, merge_reader)
    })?;
    unwrap_caught_result!(result)
  }

  #[cfg(test)]
  pub(crate) fn close_for_test<F>(
    &mut self,
    success: bool,
    _segment_dropped: bool,
    reader_consumer: F,
  ) -> Result<()>
  where
    F: FnMut(&MergeReader<CR>) -> Result<()>,
  {
    match self.hook {
      OneMergeHook::Default => {},
      _ => {
        return Err(LuceneError::illegal_state(
          "close_for_test only supports default OneMerge hooks",
        ));
      },
    }
    if !self.stat.complete(success) {
      return Err(LuceneError::illegal_state("merge has already finished"));
    }
    let merge_readers = std::mem::take(&mut self.merge_readers);
    IOUtils::apply_to_all(&merge_readers, reader_consumer)
  }
}

#[cfg(test)]
impl<D> OneMerge<D, DefaultLeafReader<D>>
where
  D: Directory,
{
  pub(crate) fn wrap_for_merge_for_test(
    &self,
    reader: DefaultLeafReader<D>,
  ) -> Result<MockRandomWrappedReader<D>> {
    match &self.hook {
      OneMergeHook::MockRandom(hook) => hook.wrap_for_merge(reader),
      _ => {
        let wrapped = <OneMergeHook<D, DefaultLeafReader<D>> as OneMergeBase<
          D,
          DefaultLeafReader<D>,
        >>::wrap_for_merge(&self.hook, reader.clone())?;
        let is_wrapped = !Arc::ptr_eq(&reader, &wrapped);
        Ok(MockRandomWrappedReader::unchanged_with_status(
          wrapped, is_wrapped,
        ))
      },
    }
  }

  pub(crate) fn reorder_for_test<CR, D1>(
    &self,
    reader: &CR,
    dir: D1,
  ) -> Result<Option<MockRandomOneMergeDocMap>>
  where
    CR: CodecReader,
    D1: Directory,
  {
    match &self.hook {
      OneMergeHook::MockRandom(hook) => hook.reorder(reader, dir),
      _ => {
        <OneMergeHook<D, DefaultLeafReader<D>> as OneMergeBase<D, DefaultLeafReader<D>>>::reorder(
          &self.hook, reader, dir,
        )
        .map(|doc_map| doc_map.map(MockRandomOneMergeDocMap::Default))
      },
    }
  }
}

#[derive(Default)]
pub(crate) enum OneMergeHook<D, CR>
where
  D: Directory,
  CR: CodecReader,
{
  #[default]
  Default,
  PointInTime(Box<PointInTimeOneMerge<D, CR>>),
  SoftDeletesRetention(SoftDeletesRetentionOneMerge<D, CR>),
  #[cfg(test)]
  MergeFinishedOnce(MergeFinishedOnceOneMerge),
  #[cfg(test)]
  AbortOnMergeComplete(AbortOnMergeCompleteOneMerge),
  #[cfg(test)]
  SetDiagnostics(SetDiagnosticsOneMerge),
  #[cfg(test)]
  SetMergePolicyDiagnostics(SetMergePolicyDiagnosticsOneMerge),
  #[cfg(test)]
  ForceMergeDvUpdate(ForceMergeDvUpdateOneMerge),
  #[cfg(test)]
  SoftUpdatesConcurrently(SoftUpdatesConcurrentlyOneMerge<D, CR>),
  #[cfg(test)]
  MockRandom(Box<MockRandomOneMerge>),
}

pub(crate) struct OneMergeDefaults;

impl OneMergeDefaults {
  pub(crate) fn merge_finished<D>(
    _inner: &mut Inner<D>,
    _stat: &MergeStat,
    _success: bool,
    _segment_dropped: bool,
  ) -> Result<()>
  where
    D: Directory,
  {
    Ok(())
  }

  pub(crate) fn wrap_for_merge<CR>(reader: CR) -> Result<CR>
  where
    CR: CodecReader,
  {
    Ok(reader)
  }

  pub(crate) fn reorder<CR1, D1>(_reader: &CR1, _dir: D1) -> Result<Option<DummyDocMap>>
  where
    CR1: CodecReader,
    D1: Directory,
  {
    Ok(None)
  }

  pub(crate) fn set_merge_info<D>(
    stat: &MergeStat,
    merge_info: &mut Option<SegmentCommitInfo<D>>,
    info: SegmentCommitInfo<D>,
  ) {
    stat.set_merge_info(info.info.get_id_key().to_string(), info.info.name.clone());
    *merge_info = Some(info);
  }

  pub(crate) fn on_merge_complete<D>(
    _inner: &mut Inner<D>,
    _stat: &MergeStat,
    _merge_info: &Option<SegmentCommitInfo<D>>,
    _is_aborted: bool,
  ) -> Result<()>
  where
    D: Directory,
  {
    Ok(())
  }

  pub(crate) fn init_merge_readers<CR, F>(
    merge_readers: &mut Vec<MergeReader<CR>>,
    stat: &MergeStat,
    mut reader_factory: F,
  ) -> Result<()>
  where
    CR: CodecReader,
    F: FnMut(&String) -> Result<MergeReader<CR>>,
  {
    debug_assert!(merge_readers.is_empty());
    debug_assert!(!stat.has_finished(), "merge is already done");
    let mut readers = Vec::with_capacity(stat.segments.len());
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      for seg_id in stat.segments.iter() {
        readers.push(reader_factory(seg_id)?);
      }
      Ok(())
    }));
    *merge_readers = readers;
    unwrap_caught_result!(result)
  }
}

impl<D, CR> OneMergeBase<D, CR> for OneMergeHook<D, CR>
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
    match self {
      Self::Default => OneMergeDefaults::merge_finished(inner, stat, success, segment_dropped),
      Self::PointInTime(hook) => hook.merge_finished(inner, stat, success, segment_dropped),
      Self::SoftDeletesRetention(hook) => {
        hook.merge_finished(inner, stat, success, segment_dropped)
      },
      #[cfg(test)]
      Self::MergeFinishedOnce(hook) => {
        <MergeFinishedOnceOneMerge as OneMergeBase<D, CR>>::merge_finished(
          hook,
          inner,
          stat,
          success,
          segment_dropped,
        )
      },
      #[cfg(test)]
      Self::AbortOnMergeComplete(hook) => {
        <AbortOnMergeCompleteOneMerge as OneMergeBase<D, CR>>::merge_finished(
          hook,
          inner,
          stat,
          success,
          segment_dropped,
        )
      },
      #[cfg(test)]
      Self::SetDiagnostics(hook) => <SetDiagnosticsOneMerge as OneMergeBase<D, CR>>::merge_finished(
        hook,
        inner,
        stat,
        success,
        segment_dropped,
      ),
      #[cfg(test)]
      Self::SetMergePolicyDiagnostics(hook) => {
        <SetMergePolicyDiagnosticsOneMerge as OneMergeBase<D, CR>>::merge_finished(
          hook,
          inner,
          stat,
          success,
          segment_dropped,
        )
      },
      #[cfg(test)]
      Self::ForceMergeDvUpdate(hook) => {
        <ForceMergeDvUpdateOneMerge as OneMergeBase<D, CR>>::merge_finished(
          hook,
          inner,
          stat,
          success,
          segment_dropped,
        )
      },
      #[cfg(test)]
      Self::SoftUpdatesConcurrently(hook) => {
        hook.merge_finished(inner, stat, success, segment_dropped)
      },
      #[cfg(test)]
      Self::MockRandom(_) => {
        OneMergeDefaults::merge_finished(inner, stat, success, segment_dropped)
      },
    }
  }

  fn wrap_for_merge(&self, reader: CR) -> Result<CR> {
    match self {
      Self::Default => OneMergeDefaults::wrap_for_merge(reader),
      Self::PointInTime(hook) => hook.wrap_for_merge(reader),
      Self::SoftDeletesRetention(hook) => hook.wrap_for_merge(reader),
      #[cfg(test)]
      Self::MergeFinishedOnce(hook) => {
        <MergeFinishedOnceOneMerge as OneMergeBase<D, CR>>::wrap_for_merge(hook, reader)
      },
      #[cfg(test)]
      Self::AbortOnMergeComplete(hook) => {
        <AbortOnMergeCompleteOneMerge as OneMergeBase<D, CR>>::wrap_for_merge(hook, reader)
      },
      #[cfg(test)]
      Self::SetDiagnostics(hook) => {
        <SetDiagnosticsOneMerge as OneMergeBase<D, CR>>::wrap_for_merge(hook, reader)
      },
      #[cfg(test)]
      Self::SetMergePolicyDiagnostics(hook) => {
        <SetMergePolicyDiagnosticsOneMerge as OneMergeBase<D, CR>>::wrap_for_merge(hook, reader)
      },
      #[cfg(test)]
      Self::ForceMergeDvUpdate(hook) => {
        <ForceMergeDvUpdateOneMerge as OneMergeBase<D, CR>>::wrap_for_merge(hook, reader)
      },
      #[cfg(test)]
      Self::SoftUpdatesConcurrently(hook) => hook.wrap_for_merge(reader),
      #[cfg(test)]
      Self::MockRandom(_) => OneMergeDefaults::wrap_for_merge(reader),
    }
  }

  fn reorder<CR1, D1>(&self, reader: &CR1, dir: D1) -> Result<Option<DummyDocMap>>
  where
    CR1: CodecReader,
    D1: Directory,
  {
    match self {
      Self::Default => OneMergeDefaults::reorder(reader, dir),
      Self::PointInTime(hook) => hook.reorder(reader, dir),
      Self::SoftDeletesRetention(hook) => hook.reorder(reader, dir),
      #[cfg(test)]
      Self::MergeFinishedOnce(hook) => {
        <MergeFinishedOnceOneMerge as OneMergeBase<D, CR>>::reorder(hook, reader, dir)
      },
      #[cfg(test)]
      Self::AbortOnMergeComplete(hook) => {
        <AbortOnMergeCompleteOneMerge as OneMergeBase<D, CR>>::reorder(hook, reader, dir)
      },
      #[cfg(test)]
      Self::SetDiagnostics(hook) => {
        <SetDiagnosticsOneMerge as OneMergeBase<D, CR>>::reorder(hook, reader, dir)
      },
      #[cfg(test)]
      Self::SetMergePolicyDiagnostics(hook) => {
        <SetMergePolicyDiagnosticsOneMerge as OneMergeBase<D, CR>>::reorder(hook, reader, dir)
      },
      #[cfg(test)]
      Self::ForceMergeDvUpdate(hook) => {
        <ForceMergeDvUpdateOneMerge as OneMergeBase<D, CR>>::reorder(hook, reader, dir)
      },
      #[cfg(test)]
      Self::SoftUpdatesConcurrently(hook) => hook.reorder(reader, dir),
      #[cfg(test)]
      Self::MockRandom(_) => OneMergeDefaults::reorder(reader, dir),
    }
  }

  fn set_merge_info(
    &self,
    stat: &MergeStat,
    merge_info: &mut Option<SegmentCommitInfo<D>>,
    info: SegmentCommitInfo<D>,
  ) {
    match self {
      Self::Default => OneMergeDefaults::set_merge_info(stat, merge_info, info),
      Self::PointInTime(hook) => hook.set_merge_info(stat, merge_info, info),
      Self::SoftDeletesRetention(hook) => hook.set_merge_info(stat, merge_info, info),
      #[cfg(test)]
      Self::MergeFinishedOnce(hook) => {
        <MergeFinishedOnceOneMerge as OneMergeBase<D, CR>>::set_merge_info(
          hook, stat, merge_info, info,
        )
      },
      #[cfg(test)]
      Self::AbortOnMergeComplete(hook) => {
        <AbortOnMergeCompleteOneMerge as OneMergeBase<D, CR>>::set_merge_info(
          hook, stat, merge_info, info,
        )
      },
      #[cfg(test)]
      Self::SetDiagnostics(hook) => {
        <SetDiagnosticsOneMerge as OneMergeBase<D, CR>>::set_merge_info(
          hook, stat, merge_info, info,
        )
      },
      #[cfg(test)]
      Self::SetMergePolicyDiagnostics(hook) => {
        <SetMergePolicyDiagnosticsOneMerge as OneMergeBase<D, CR>>::set_merge_info(
          hook, stat, merge_info, info,
        )
      },
      #[cfg(test)]
      Self::ForceMergeDvUpdate(hook) => {
        <ForceMergeDvUpdateOneMerge as OneMergeBase<D, CR>>::set_merge_info(
          hook, stat, merge_info, info,
        )
      },
      #[cfg(test)]
      Self::SoftUpdatesConcurrently(hook) => hook.set_merge_info(stat, merge_info, info),
      #[cfg(test)]
      Self::MockRandom(_) => OneMergeDefaults::set_merge_info(stat, merge_info, info),
    }
  }

  fn on_merge_complete(
    &self,
    inner: &mut Inner<D>,
    stat: &MergeStat,
    merge_info: &Option<SegmentCommitInfo<D>>,
    is_aborted: bool,
  ) -> Result<()> {
    match self {
      Self::Default => OneMergeDefaults::on_merge_complete(inner, stat, merge_info, is_aborted),
      Self::PointInTime(hook) => hook.on_merge_complete(inner, stat, merge_info, is_aborted),
      Self::SoftDeletesRetention(hook) => {
        hook.on_merge_complete(inner, stat, merge_info, is_aborted)
      },
      #[cfg(test)]
      Self::MergeFinishedOnce(hook) => {
        <MergeFinishedOnceOneMerge as OneMergeBase<D, CR>>::on_merge_complete(
          hook,
          inner,
          stat,
          merge_info,
          is_aborted,
        )
      },
      #[cfg(test)]
      Self::AbortOnMergeComplete(hook) => {
        <AbortOnMergeCompleteOneMerge as OneMergeBase<D, CR>>::on_merge_complete(
          hook,
          inner,
          stat,
          merge_info,
          is_aborted,
        )
      },
      #[cfg(test)]
      Self::SetDiagnostics(hook) => {
        <SetDiagnosticsOneMerge as OneMergeBase<D, CR>>::on_merge_complete(
          hook,
          inner,
          stat,
          merge_info,
          is_aborted,
        )
      },
      #[cfg(test)]
      Self::SetMergePolicyDiagnostics(hook) => {
        <SetMergePolicyDiagnosticsOneMerge as OneMergeBase<D, CR>>::on_merge_complete(
          hook,
          inner,
          stat,
          merge_info,
          is_aborted,
        )
      },
      #[cfg(test)]
      Self::ForceMergeDvUpdate(hook) => {
        <ForceMergeDvUpdateOneMerge as OneMergeBase<D, CR>>::on_merge_complete(
          hook,
          inner,
          stat,
          merge_info,
          is_aborted,
        )
      },
      #[cfg(test)]
      Self::SoftUpdatesConcurrently(hook) => {
        hook.on_merge_complete(inner, stat, merge_info, is_aborted)
      },
      #[cfg(test)]
      Self::MockRandom(_) => {
        OneMergeDefaults::on_merge_complete(inner, stat, merge_info, is_aborted)
      },
    }
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
    match self {
      Self::Default => OneMergeDefaults::init_merge_readers(merge_readers, stat, reader_factory),
      Self::PointInTime(hook) => hook.init_merge_readers(merge_readers, stat, reader_factory),
      Self::SoftDeletesRetention(hook) => {
        hook.init_merge_readers(merge_readers, stat, reader_factory)
      },
      #[cfg(test)]
      Self::MergeFinishedOnce(hook) => {
        <MergeFinishedOnceOneMerge as OneMergeBase<D, CR>>::init_merge_readers(
          hook,
          merge_readers,
          stat,
          reader_factory,
        )
      },
      #[cfg(test)]
      Self::AbortOnMergeComplete(hook) => {
        <AbortOnMergeCompleteOneMerge as OneMergeBase<D, CR>>::init_merge_readers(
          hook,
          merge_readers,
          stat,
          reader_factory,
        )
      },
      #[cfg(test)]
      Self::SetDiagnostics(hook) => {
        <SetDiagnosticsOneMerge as OneMergeBase<D, CR>>::init_merge_readers(
          hook,
          merge_readers,
          stat,
          reader_factory,
        )
      },
      #[cfg(test)]
      Self::SetMergePolicyDiagnostics(hook) => {
        <SetMergePolicyDiagnosticsOneMerge as OneMergeBase<D, CR>>::init_merge_readers(
          hook,
          merge_readers,
          stat,
          reader_factory,
        )
      },
      #[cfg(test)]
      Self::ForceMergeDvUpdate(hook) => {
        <ForceMergeDvUpdateOneMerge as OneMergeBase<D, CR>>::init_merge_readers(
          hook,
          merge_readers,
          stat,
          reader_factory,
        )
      },
      #[cfg(test)]
      Self::SoftUpdatesConcurrently(hook) => {
        hook.init_merge_readers(merge_readers, stat, reader_factory)
      },
      #[cfg(test)]
      Self::MockRandom(_) => {
        OneMergeDefaults::init_merge_readers(merge_readers, stat, reader_factory)
      },
    }
  }
}

pub trait OneMergeBase<D, CR>
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
  ) -> Result<()>;

  fn wrap_for_merge(&self, reader: CR) -> Result<CR>;

  fn reorder<CR1, D1>(&self, reader: &CR1, dir: D1) -> Result<Option<DummyDocMap>>
  where
    CR1: CodecReader,
    D1: Directory;

  fn set_merge_info(
    &self,
    stat: &MergeStat,
    merge_info: &mut Option<SegmentCommitInfo<D>>,
    info: SegmentCommitInfo<D>,
  );

  fn on_merge_complete(
    &self,
    inner: &mut Inner<D>,
    stat: &MergeStat,
    merge_info: &Option<SegmentCommitInfo<D>>,
    is_aborted: bool,
  ) -> Result<()>;

  fn init_merge_readers<F>(
    &self,
    merge_readers: &mut Vec<MergeReader<CR>>,
    stat: &MergeStat,
    reader_factory: F,
  ) -> Result<()>
  where
    F: FnMut(&String) -> Result<MergeReader<CR>>;
}
pub type DefaultMergeSpecification<D> = MergeSpecification<D, DefaultLeafReader<D>>;
pub struct MergeSpecification<D, CR>
where
  D: Directory,
  CR: CodecReader,
{
  /// The subset of segments to be included in the primitive merge.
  pub(crate) merges: Vec<OneMerge<D, CR>>,
}
impl<D, CR> Default for MergeSpecification<D, CR>
where
  D: Directory,
  CR: CodecReader,
{
  fn default() -> Self {
    Self::new()
  }
}

impl<D, CR> MergeSpecification<D, CR>
where
  D: Directory,
  CR: CodecReader,
{
  pub fn new() -> Self {
    Self { merges: Vec::new() }
  }
}
impl<D, CR> MergeSpecification<D, CR>
where
  D: Directory,
  CR: CodecReader,
{
  pub fn add(&mut self, merge: OneMerge<D, CR>) {
    self.merges.push(merge);
  }

  pub fn await_(merges: &[MergeStat]) {
    MergeStat::await_all(merges);
  }

  pub fn await_with_timeout(merges: &[MergeStat], timeout: Duration) -> bool {
    MergeStat::await_all_with_timeout(merges, timeout)
  }
}

/// Reason for pausing the merge thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PauseReason {
  /// Stopped (because of throughput rate set to 0, typically).
  Stopped,
  /// Temporarily paused because of exceeded throughput rate.
  Paused,
  /// Other reason.
  Other,
}
/// Progress and state for an executing merge. This struct encapsulates the
/// logic to pause and resume the merge thread or to abort the merge entirely.
pub struct OneMergeProgress {
  pause_lock: Mutex<()>,
  pausing: Condvar,
  /// Pause times (in nanoseconds) for each [`PauseReason`](PauseReason).
  pause_times: PauseTimes,
  aborted: AtomicBool,
  /// This field is for sanity-check purpos only. Only the same thread that
  //     /// Invoking `OneMerge::merge_init` permits calls to `pause_nanos`.
  /// This is always verified at runtime.
  owner: Mutex<Option<ThreadId>>,
}

#[derive(Default)]

struct PauseTimes {
  stopped: AtomicU64,
  paused: AtomicU64,
  other: AtomicU64,
}

impl Default for OneMergeProgress {
  fn default() -> Self {
    Self::new()
  }
}

impl OneMergeProgress {
  /// Creates a new merge progress info.
  pub fn new() -> Self {
    Self {
      pause_lock: Mutex::new(()),
      pausing: Condvar::new(),
      // Place all the pause reasons in there immediately so that we can
      // simply update values.
      pause_times: PauseTimes::default(),
      aborted: AtomicBool::new(false),
      owner: Mutex::new(None),
    }
  }
  /// Abort the merge this progress tracks at the next possible moment.
  pub fn abort(&self) {
    self.aborted.store(true, Ordering::SeqCst);
    self.wakeup(); // wakeup any paused merge thread.
  }
  /// Return the aborted state of this merge.
  pub fn is_aborted(&self) -> bool {
    self.aborted.load(Ordering::SeqCst)
  }

  /// Pauses the calling thread for at least `pause_nanos` nanoseconds unless
  /// the merge is aborted or the external condition returns `false`, in
  /// which case control returns immediately.
  ///
  /// The external condition is required so that other threads can terminate
  /// the pausing immediately before `pause_nanos` expires. We can't rely
  /// on just `Condvar::wait_timeout_while()` alone because it can return
  /// due to spurious wakeups too.
  ///
  /// # Arguments
  /// - `condition`: The pause condition that should return `false` if
  ///   immediate return from this method is needed. Other threads can wake up
  ///   any sleeping thread by calling [`wakeup()`](OneMergeProgress::wakeup),
  ///   but the thread may sleep for the remainder of the requested time if
  ///   this condition remains `true`.
  pub fn pause_nanos<F>(&self, pause_nanos: u64, reason: PauseReason, condition: F)
  where
    F: Fn() -> bool,
  {
    {
      let owner = self.owner.lock();
      let current_id = thread::current().id();
      debug_assert_eq!(
        *owner,
        Some(current_id),
        "Only owner thread can pause merge"
      );
    }

    let start = Instant::now();
    let deadline = start + Duration::from_nanos(pause_nanos);

    let mut lock = self.pause_lock.lock();
    let pause_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      while !self.aborted.load(Ordering::SeqCst) && condition() {
        let now = Instant::now();
        if now >= deadline {
          break;
        }
        let timeout = deadline - now;
        self.pausing.wait_for(&mut lock, timeout);
      }
    }));
    drop(lock);

    let elapsed = start.elapsed().as_nanos().min(u64::MAX as u128) as u64;
    self.add_pause_time(reason, elapsed);
    resume_caught_panic!(pause_result);
  }

  fn add_pause_time(&self, reason: PauseReason, nanos: u64) {
    match reason {
      PauseReason::Stopped => self.pause_times.stopped.fetch_add(nanos, Ordering::SeqCst),
      PauseReason::Paused => self.pause_times.paused.fetch_add(nanos, Ordering::SeqCst),
      PauseReason::Other => self.pause_times.other.fetch_add(nanos, Ordering::SeqCst),
    };
  }
  /// Request a wakeup for any threads stalled in
  /// [`pauseNanos`](OneMergeProgress::pause_nanos).
  pub fn wakeup(&self) {
    let _lock = self.pause_lock.lock();
    self.pausing.notify_all();
  }
  /// Returns pause reasons and associated times in nanoseconds.
  pub fn get_pause_times(&self) -> HashMap<PauseReason, u64> {
    let mut map = HashMap::new();
    map.insert(
      PauseReason::Stopped,
      self.pause_times.stopped.load(Ordering::SeqCst),
    );
    map.insert(
      PauseReason::Paused,
      self.pause_times.paused.load(Ordering::SeqCst),
    );
    map.insert(
      PauseReason::Other,
      self.pause_times.other.load(Ordering::SeqCst),
    );
    map
  }
  pub fn set_merge_thread(&self) {
    let mut owner = self.owner.lock();
    debug_assert!(owner.is_none());
    *owner = Some(thread::current().id());
  }
}
/// This trait represents the current context of the merge selection process.
/// It allows access to real-time information such as:
/// - the segments currently being merged
/// - how many deletes a segment would reclaim if merged
///
/// This context may be stateful and can change during the execution of a
/// merge policy's selection processes.
pub trait MergeContext<D>
where
  D: Directory,
{
  /// Returns the number of deletes a merge would claim back
  /// if the given segment is merged.
  ///
  /// See [`MergePolicy::num_deletes_to_merge`].
  ///
  /// * `info` — the segment to get the number of deletes for
  fn num_deletes_to_merge(&self, info: &SegmentCommitInfo<D>) -> Result<i32>;

  /// Returns the number of deleted documents in the given segment.
  fn num_deleted_docs(&self, info: &SegmentCommitInfo<D>) -> i32;

  /// Returns the info stream that can be used to log messages.
  fn get_info_stream(&self) -> InfoStreamMT;

  /// Returns an unmodifiable set of segments that are currently merging.
  fn get_merging_segments(&self, inner: Option<&Inner<D>>) -> HashSet<String>;
}

pub type MergeReaderSR<D> = MergeReader<DefaultLeafReader<D>>;
pub struct MergeReader<CR>
where
  CR: CodecReader,
{
  pub(crate) reader: CR,
  pub(crate) hard_live_docs: Option<CR::Bits>,
}
impl<CR> MergeReader<CR>
where
  CR: CodecReader,
{
  pub(crate) fn new(codec_reader: CR, hard_live_docs: Option<CR::Bits>) -> Self {
    Self {
      reader: codec_reader,
      hard_live_docs,
    }
  }
}
