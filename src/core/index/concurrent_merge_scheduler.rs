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
use std::cell::RefCell;
use std::cmp;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::thread;
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex, MutexGuard};
use rayon::ThreadPoolBuilder;

use crate::core::index::codec_reader::CodecReader;
use crate::core::index::merge_policy::{MergeStat, OneMerge};
use crate::core::index::merge_rate_limiter::MergeRateLimiter;
use crate::core::index::merge_scheduler::{MergeScheduler, MergeSource};
use crate::core::index::merge_trigger::MergeTrigger;
use crate::core::search::task_executor::TaskExecutor;
use crate::core::store::directory::Directory;
use crate::core::store::rate_limited_directory::RateLimitedDirectory;
use crate::core::store::rate_limiter::RateLimiter;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{
  CaughtResult, CaughtResultExt, LuceneError, LuceneErrorKind, Result,
};
use crate::core::util::info_stream::{InfoStream, InfoStreamMT, get_default_info_stream};
#[cfg(test)]
use crate::test_framework::core::index::suppressing_concurrent_merge_scheduler::SuppressingConcurrentMergeScheduler;
#[cfg(test)]
use crate::test_framework::core::index::test_concurrent_merge_scheduler::{
  HangDuringRollbackConcurrentMergeScheduler, LiveMaxMergeCountConcurrentMergeScheduler,
  MaxMergeCountConcurrentMergeScheduler, MaybeStallCalledConcurrentMergeScheduler,
  MergeThreadMessagesConcurrentMergeScheduler, NoStallMergeThreadsConcurrentMergeScheduler,
  TrackingConcurrentMergeScheduler,
};
#[cfg(test)]
use crate::test_framework::core::index::test_index_file_deleter::FakeFailConcurrentMergeScheduler;
#[cfg(test)]
use crate::test_framework::core::index::test_index_writer::CloseWhileMergeIsRunningConcurrentMergeScheduler;
#[cfg(test)]
use crate::test_framework::core::index::test_index_writer_merge_policy::{
  MergeDvUpdateFileOnCommitConcurrentMergeScheduler,
  MergeDvUpdateFileOnGetReaderConcurrentMergeScheduler,
};
#[cfg(test)]
use crate::test_framework::core::index::test_tragic_index_writer_deadlock::StalledMergesConcurrentMergeScheduler;
#[cfg(test)]
use crate::test_framework::core::util::lucene_test_case::AlwaysProceedConcurrentMergeScheduler;

thread_local! {
  static CURRENT_MERGE_RATE_LIMITER: RefCell<Option<Arc<MergeRateLimiter>>> =
    const { RefCell::new(None) };
}

/**
 * Dynamic default for `max_thread_count` and `max_merge_count`, based on CPU core count.
 * `max_thread_count` is set to `max(1, cpu_core_count/2)`. `max_merge_count`
 * is set to `max_thread_count + 5`.
 */
pub const AUTO_DETECT_MERGES_AND_THREADS: i32 = -1;

/**
 * Used for testing.
 *
 * @lucene.internal
 */
pub const DEFAULT_CPU_CORE_COUNT_PROPERTY: &str = "lucene.cms.override_core_count";

/** Floor for IO write rate limit (we will never go any lower than this) */
const MIN_MERGE_MB_PER_SEC: f64 = 5.0;

/** Ceiling for IO write rate limit (we will never go any higher than this) */
const MAX_MERGE_MB_PER_SEC: f64 = 10240.0;

/** Initial value for IO write rate limit when do_auto_io_throttle is true */
const START_MB_PER_SEC: f64 = 20.0;

/**
 * Merges below this size are not counted in the max_thread_count, i.e. they can freely run in their
 * own thread (up until max_merge_count).
 */
const MIN_BIG_MERGE_MB: i64 = 50;

/// A [`MergeScheduler`] that runs each merge using a separate thread.
///
/// Specify the max number of threads that may run at once, and the maximum number of simultaneous
/// merges with [`ConcurrentMergeScheduler::set_max_merges_and_threads`].
///
/// If the number of merges exceeds the max number of threads then the largest merges are paused
/// until one of the smaller merges completes.
///
/// If more than [`ConcurrentMergeScheduler::get_max_merge_count`] merges are requested then this
/// scheduler will forcefully throttle the incoming threads by pausing until one or more merges
/// complete.
///
/// This scheduler sets defaults based on Rust's view of the CPU count, and it assumes a solid state
/// disk (or similar). If you have a spinning disk and want to maximize performance, use
/// [`ConcurrentMergeScheduler::set_default_max_merges_and_threads`].
#[derive(Clone)]
pub struct ConcurrentMergeScheduler {
  shared: Arc<ConcurrentMergeSchedulerShared>,
  hook: ConcurrentMergeSchedulerHook,
}

struct ConcurrentMergeSchedulerShared {
  inner: Mutex<Inner>,
  changed: Condvar,
  info_stream: Mutex<InfoStreamMT>,
  #[allow(clippy::type_complexity)]
  intra_merge_executor: Mutex<Option<(usize, Arc<TaskExecutor>)>>,
}

pub(crate) struct Inner {
  /** List of currently active merge work. */
  merge_threads: Vec<Arc<MergeThreadState>>,
  active_merge_order: Vec<usize>,
  // Max number of merge threads allowed to be running at once. When there are more merges then
  // this, we forcefully pause the larger ones, letting the smaller ones run, up until
  // max_merge_count merges at which point we forcefully pause incoming threads (that presumably
  // are the ones causing so much merging).
  max_thread_count: i32,
  // Max number of merges we accept before forcefully throttling the incoming threads.
  max_merge_count: i32,
  /** How many merge threads have kicked off (this is use to name them). */
  merge_thread_count: usize,
  /** Current IO writes throttle rate */
  target_mb_per_sec: f64,
  /** true if we should rate-limit writes for each merge */
  do_auto_io_throttle: bool,
  force_merge_mb_per_sec: f64,
  suppress_exceptions: bool,
}

#[derive(Clone, Default)]
pub(crate) enum ConcurrentMergeSchedulerHook {
  #[default]
  Default,
  #[cfg(test)]
  AlwaysParallelIntraMerge,
  #[cfg(test)]
  AssertSmallMergesUseDirectExecutor,
  #[cfg(test)]
  MaxMergeCount(MaxMergeCountConcurrentMergeScheduler),
  #[cfg(test)]
  Tracking(TrackingConcurrentMergeScheduler),
  #[cfg(test)]
  LiveMaxMergeCount(LiveMaxMergeCountConcurrentMergeScheduler),
  #[cfg(test)]
  MaybeStallCalled(MaybeStallCalledConcurrentMergeScheduler),
  #[cfg(test)]
  HangDuringRollback(HangDuringRollbackConcurrentMergeScheduler),
  #[cfg(test)]
  NoStallMergeThreads(NoStallMergeThreadsConcurrentMergeScheduler),
  #[cfg(test)]
  MergeThreadMessages(MergeThreadMessagesConcurrentMergeScheduler),
  #[cfg(test)]
  CloseWhileMergeIsRunning(CloseWhileMergeIsRunningConcurrentMergeScheduler),
  #[cfg(test)]
  Suppressing(SuppressingConcurrentMergeScheduler),
  #[cfg(test)]
  TestIndexFileDeleter(FakeFailConcurrentMergeScheduler),
  #[cfg(test)]
  MergeDvUpdateFileOnGetReader(MergeDvUpdateFileOnGetReaderConcurrentMergeScheduler),
  #[cfg(test)]
  MergeDvUpdateFileOnCommit(MergeDvUpdateFileOnCommitConcurrentMergeScheduler),
  #[cfg(test)]
  StalledMerges(StalledMergesConcurrentMergeScheduler),
  #[cfg(test)]
  LuceneTestCaseAlwaysProceed(AlwaysProceedConcurrentMergeScheduler),
}

pub(crate) struct ConcurrentMergeSchedulerDefaults;

pub(crate) trait ConcurrentMergeSchedulerBase {
  fn get_intra_merge_executor<D, CR>(
    &self,
    scheduler: &ConcurrentMergeScheduler,
    merge: &OneMerge<D, CR>,
  ) -> Result<Arc<TaskExecutor>>
  where
    D: Directory,
    CR: CodecReader,
  {
    ConcurrentMergeSchedulerDefaults::get_intra_merge_executor(scheduler, merge)
  }

  fn update_merge_threads(
    &self,
    scheduler: &ConcurrentMergeScheduler,
    inner: &mut Inner,
  ) -> Result<()> {
    ConcurrentMergeSchedulerDefaults::update_merge_threads(scheduler, inner)
  }

  fn close(&self, scheduler: &ConcurrentMergeScheduler) -> Result<()> {
    ConcurrentMergeSchedulerDefaults::close(scheduler)
  }

  fn merge<MS, D>(
    &self,
    scheduler: &ConcurrentMergeScheduler,
    merge_source: MS,
    trigger: MergeTrigger,
  ) -> Result<()>
  where
    MS: MergeSource<D> + Clone + 'static,
    D: Directory + 'static,
    OneMerge<D, MS::Reader>: Send + 'static,
  {
    ConcurrentMergeSchedulerDefaults::merge(scheduler, merge_source, trigger)
  }

  fn maybe_stall<MS, D>(
    &self,
    scheduler: &ConcurrentMergeScheduler,
    inner: &mut MutexGuard<'_, Inner>,
    merge_source: &MS,
  ) -> Result<bool>
  where
    MS: MergeSource<D>,
    D: Directory,
  {
    ConcurrentMergeSchedulerDefaults::maybe_stall(scheduler, inner, merge_source)
  }

  fn do_stall(&self, scheduler: &ConcurrentMergeScheduler, inner: &mut MutexGuard<'_, Inner>) {
    ConcurrentMergeSchedulerDefaults::do_stall(scheduler, inner)
  }

  fn do_merge<MS, D>(
    &self,
    scheduler: &ConcurrentMergeScheduler,
    merge_source: &MS,
    merge: OneMerge<D, MS::Reader>,
  ) -> Result<()>
  where
    MS: MergeSource<D>,
    D: Directory + 'static,
  {
    ConcurrentMergeSchedulerDefaults::do_merge(scheduler, merge_source, merge)
  }

  fn get_merge_thread<MS, D>(
    &self,
    scheduler: &ConcurrentMergeScheduler,
    inner: &mut Inner,
    merge_source: MS,
    merge: OneMerge<D, MS::Reader>,
  ) -> Result<MergeThread<MS, D>>
  where
    MS: MergeSource<D> + Clone + 'static,
    D: Directory + 'static,
    OneMerge<D, MS::Reader>: Send + 'static,
  {
    ConcurrentMergeSchedulerDefaults::get_merge_thread(scheduler, inner, merge_source, merge)
  }

  fn handle_merge_exception(
    &self,
    scheduler: &ConcurrentMergeScheduler,
    result: CaughtResult,
  ) -> Result<()> {
    ConcurrentMergeSchedulerDefaults::handle_merge_exception(scheduler, result)
  }

  fn target_mb_per_sec_changed(&self, scheduler: &ConcurrentMergeScheduler) {
    ConcurrentMergeSchedulerDefaults::target_mb_per_sec_changed(scheduler)
  }
}

impl ConcurrentMergeSchedulerBase for ConcurrentMergeSchedulerHook {
  fn get_intra_merge_executor<D, CR>(
    &self,
    scheduler: &ConcurrentMergeScheduler,
    merge: &OneMerge<D, CR>,
  ) -> Result<Arc<TaskExecutor>>
  where
    D: Directory,
    CR: CodecReader,
  {
    match self {
      Self::Default => ConcurrentMergeSchedulerDefaults::get_intra_merge_executor(scheduler, merge),
      #[cfg(test)]
      Self::AssertSmallMergesUseDirectExecutor => {
        ConcurrentMergeSchedulerDefaults::get_intra_merge_executor(scheduler, merge)
      },
      #[cfg(test)]
      Self::AlwaysParallelIntraMerge => scheduler.get_parallel_merge_executor(),
      #[cfg(test)]
      Self::MaxMergeCount(hook) => hook.get_intra_merge_executor(scheduler, merge),
      #[cfg(test)]
      Self::Tracking(hook) => hook.get_intra_merge_executor(scheduler, merge),
      #[cfg(test)]
      Self::LiveMaxMergeCount(hook) => hook.get_intra_merge_executor(scheduler, merge),
      #[cfg(test)]
      Self::MaybeStallCalled(hook) => hook.get_intra_merge_executor(scheduler, merge),
      #[cfg(test)]
      Self::HangDuringRollback(hook) => hook.get_intra_merge_executor(scheduler, merge),
      #[cfg(test)]
      Self::NoStallMergeThreads(hook) => hook.get_intra_merge_executor(scheduler, merge),
      #[cfg(test)]
      Self::MergeThreadMessages(hook) => hook.get_intra_merge_executor(scheduler, merge),
      #[cfg(test)]
      Self::CloseWhileMergeIsRunning(hook) => hook.get_intra_merge_executor(scheduler, merge),
      #[cfg(test)]
      Self::Suppressing(hook) => hook.get_intra_merge_executor(scheduler, merge),
      #[cfg(test)]
      Self::TestIndexFileDeleter(hook) => hook.get_intra_merge_executor(scheduler, merge),
      #[cfg(test)]
      Self::MergeDvUpdateFileOnGetReader(hook) => hook.get_intra_merge_executor(scheduler, merge),
      #[cfg(test)]
      Self::MergeDvUpdateFileOnCommit(hook) => hook.get_intra_merge_executor(scheduler, merge),
      #[cfg(test)]
      Self::StalledMerges(hook) => hook.get_intra_merge_executor(scheduler, merge),
      #[cfg(test)]
      Self::LuceneTestCaseAlwaysProceed(hook) => hook.get_intra_merge_executor(scheduler, merge),
    }
  }

  fn update_merge_threads(
    &self,
    scheduler: &ConcurrentMergeScheduler,
    inner: &mut Inner,
  ) -> Result<()> {
    match self {
      Self::Default => ConcurrentMergeSchedulerDefaults::update_merge_threads(scheduler, inner),
      #[cfg(test)]
      Self::AlwaysParallelIntraMerge | Self::AssertSmallMergesUseDirectExecutor => {
        ConcurrentMergeSchedulerDefaults::update_merge_threads(scheduler, inner)
      },
      #[cfg(test)]
      Self::MaxMergeCount(hook) => hook.update_merge_threads(scheduler, inner),
      #[cfg(test)]
      Self::Tracking(hook) => hook.update_merge_threads(scheduler, inner),
      #[cfg(test)]
      Self::LiveMaxMergeCount(hook) => hook.update_merge_threads(scheduler, inner),
      #[cfg(test)]
      Self::MaybeStallCalled(hook) => hook.update_merge_threads(scheduler, inner),
      #[cfg(test)]
      Self::HangDuringRollback(hook) => hook.update_merge_threads(scheduler, inner),
      #[cfg(test)]
      Self::NoStallMergeThreads(hook) => hook.update_merge_threads(scheduler, inner),
      #[cfg(test)]
      Self::MergeThreadMessages(hook) => hook.update_merge_threads(scheduler, inner),
      #[cfg(test)]
      Self::CloseWhileMergeIsRunning(hook) => hook.update_merge_threads(scheduler, inner),
      #[cfg(test)]
      Self::Suppressing(hook) => hook.update_merge_threads(scheduler, inner),
      #[cfg(test)]
      Self::TestIndexFileDeleter(hook) => hook.update_merge_threads(scheduler, inner),
      #[cfg(test)]
      Self::MergeDvUpdateFileOnGetReader(hook) => hook.update_merge_threads(scheduler, inner),
      #[cfg(test)]
      Self::MergeDvUpdateFileOnCommit(hook) => hook.update_merge_threads(scheduler, inner),
      #[cfg(test)]
      Self::StalledMerges(hook) => hook.update_merge_threads(scheduler, inner),
      #[cfg(test)]
      Self::LuceneTestCaseAlwaysProceed(hook) => hook.update_merge_threads(scheduler, inner),
    }
  }

  fn close(&self, scheduler: &ConcurrentMergeScheduler) -> Result<()> {
    match self {
      Self::Default => ConcurrentMergeSchedulerDefaults::close(scheduler),
      #[cfg(test)]
      Self::AlwaysParallelIntraMerge | Self::AssertSmallMergesUseDirectExecutor => {
        ConcurrentMergeSchedulerDefaults::close(scheduler)
      },
      #[cfg(test)]
      Self::MaxMergeCount(hook) => hook.close(scheduler),
      #[cfg(test)]
      Self::Tracking(hook) => hook.close(scheduler),
      #[cfg(test)]
      Self::LiveMaxMergeCount(hook) => hook.close(scheduler),
      #[cfg(test)]
      Self::MaybeStallCalled(hook) => hook.close(scheduler),
      #[cfg(test)]
      Self::HangDuringRollback(hook) => hook.close(scheduler),
      #[cfg(test)]
      Self::NoStallMergeThreads(hook) => hook.close(scheduler),
      #[cfg(test)]
      Self::MergeThreadMessages(hook) => hook.close(scheduler),
      #[cfg(test)]
      Self::CloseWhileMergeIsRunning(hook) => hook.close(scheduler),
      #[cfg(test)]
      Self::Suppressing(hook) => hook.close(scheduler),
      #[cfg(test)]
      Self::TestIndexFileDeleter(hook) => hook.close(scheduler),
      #[cfg(test)]
      Self::MergeDvUpdateFileOnGetReader(hook) => hook.close(scheduler),
      #[cfg(test)]
      Self::MergeDvUpdateFileOnCommit(hook) => hook.close(scheduler),
      #[cfg(test)]
      Self::StalledMerges(hook) => hook.close(scheduler),
      #[cfg(test)]
      Self::LuceneTestCaseAlwaysProceed(hook) => hook.close(scheduler),
    }
  }

  fn merge<MS, D>(
    &self,
    scheduler: &ConcurrentMergeScheduler,
    merge_source: MS,
    trigger: MergeTrigger,
  ) -> Result<()>
  where
    MS: MergeSource<D> + Clone + 'static,
    D: Directory + 'static,
    OneMerge<D, MS::Reader>: Send + 'static,
  {
    match self {
      Self::Default => ConcurrentMergeSchedulerDefaults::merge(scheduler, merge_source, trigger),
      #[cfg(test)]
      Self::AlwaysParallelIntraMerge | Self::AssertSmallMergesUseDirectExecutor => {
        ConcurrentMergeSchedulerDefaults::merge(scheduler, merge_source, trigger)
      },
      #[cfg(test)]
      Self::MaxMergeCount(hook) => hook.merge(scheduler, merge_source, trigger),
      #[cfg(test)]
      Self::Tracking(hook) => hook.merge(scheduler, merge_source, trigger),
      #[cfg(test)]
      Self::LiveMaxMergeCount(hook) => hook.merge(scheduler, merge_source, trigger),
      #[cfg(test)]
      Self::MaybeStallCalled(hook) => hook.merge(scheduler, merge_source, trigger),
      #[cfg(test)]
      Self::HangDuringRollback(hook) => hook.merge(scheduler, merge_source, trigger),
      #[cfg(test)]
      Self::NoStallMergeThreads(hook) => hook.merge(scheduler, merge_source, trigger),
      #[cfg(test)]
      Self::MergeThreadMessages(hook) => hook.merge(scheduler, merge_source, trigger),
      #[cfg(test)]
      Self::CloseWhileMergeIsRunning(hook) => hook.merge(scheduler, merge_source, trigger),
      #[cfg(test)]
      Self::Suppressing(hook) => hook.merge(scheduler, merge_source, trigger),
      #[cfg(test)]
      Self::TestIndexFileDeleter(hook) => hook.merge(scheduler, merge_source, trigger),
      #[cfg(test)]
      Self::MergeDvUpdateFileOnGetReader(hook) => hook.merge(scheduler, merge_source, trigger),
      #[cfg(test)]
      Self::MergeDvUpdateFileOnCommit(hook) => hook.merge(scheduler, merge_source, trigger),
      #[cfg(test)]
      Self::StalledMerges(hook) => hook.merge(scheduler, merge_source, trigger),
      #[cfg(test)]
      Self::LuceneTestCaseAlwaysProceed(hook) => hook.merge(scheduler, merge_source, trigger),
    }
  }

  fn maybe_stall<MS, D>(
    &self,
    scheduler: &ConcurrentMergeScheduler,
    inner: &mut MutexGuard<'_, Inner>,
    merge_source: &MS,
  ) -> Result<bool>
  where
    MS: MergeSource<D>,
    D: Directory,
  {
    match self {
      Self::Default => {
        ConcurrentMergeSchedulerDefaults::maybe_stall(scheduler, inner, merge_source)
      },
      #[cfg(test)]
      Self::AlwaysParallelIntraMerge | Self::AssertSmallMergesUseDirectExecutor => {
        ConcurrentMergeSchedulerDefaults::maybe_stall(scheduler, inner, merge_source)
      },
      #[cfg(test)]
      Self::MaxMergeCount(hook) => hook.maybe_stall(scheduler, inner, merge_source),
      #[cfg(test)]
      Self::Tracking(hook) => hook.maybe_stall(scheduler, inner, merge_source),
      #[cfg(test)]
      Self::LiveMaxMergeCount(hook) => hook.maybe_stall(scheduler, inner, merge_source),
      #[cfg(test)]
      Self::MaybeStallCalled(hook) => hook.maybe_stall(scheduler, inner, merge_source),
      #[cfg(test)]
      Self::HangDuringRollback(hook) => hook.maybe_stall(scheduler, inner, merge_source),
      #[cfg(test)]
      Self::NoStallMergeThreads(hook) => hook.maybe_stall(scheduler, inner, merge_source),
      #[cfg(test)]
      Self::MergeThreadMessages(hook) => hook.maybe_stall(scheduler, inner, merge_source),
      #[cfg(test)]
      Self::CloseWhileMergeIsRunning(hook) => hook.maybe_stall(scheduler, inner, merge_source),
      #[cfg(test)]
      Self::Suppressing(hook) => hook.maybe_stall(scheduler, inner, merge_source),
      #[cfg(test)]
      Self::TestIndexFileDeleter(hook) => hook.maybe_stall(scheduler, inner, merge_source),
      #[cfg(test)]
      Self::MergeDvUpdateFileOnGetReader(hook) => hook.maybe_stall(scheduler, inner, merge_source),
      #[cfg(test)]
      Self::MergeDvUpdateFileOnCommit(hook) => hook.maybe_stall(scheduler, inner, merge_source),
      #[cfg(test)]
      Self::StalledMerges(hook) => hook.maybe_stall(scheduler, inner, merge_source),
      #[cfg(test)]
      Self::LuceneTestCaseAlwaysProceed(hook) => hook.maybe_stall(scheduler, inner, merge_source),
    }
  }

  fn do_stall(&self, scheduler: &ConcurrentMergeScheduler, inner: &mut MutexGuard<'_, Inner>) {
    match self {
      Self::Default => ConcurrentMergeSchedulerDefaults::do_stall(scheduler, inner),
      #[cfg(test)]
      Self::AlwaysParallelIntraMerge | Self::AssertSmallMergesUseDirectExecutor => {
        ConcurrentMergeSchedulerDefaults::do_stall(scheduler, inner)
      },
      #[cfg(test)]
      Self::MaxMergeCount(hook) => hook.do_stall(scheduler, inner),
      #[cfg(test)]
      Self::Tracking(hook) => hook.do_stall(scheduler, inner),
      #[cfg(test)]
      Self::LiveMaxMergeCount(hook) => hook.do_stall(scheduler, inner),
      #[cfg(test)]
      Self::MaybeStallCalled(hook) => hook.do_stall(scheduler, inner),
      #[cfg(test)]
      Self::HangDuringRollback(hook) => hook.do_stall(scheduler, inner),
      #[cfg(test)]
      Self::NoStallMergeThreads(hook) => hook.do_stall(scheduler, inner),
      #[cfg(test)]
      Self::MergeThreadMessages(hook) => hook.do_stall(scheduler, inner),
      #[cfg(test)]
      Self::CloseWhileMergeIsRunning(hook) => hook.do_stall(scheduler, inner),
      #[cfg(test)]
      Self::Suppressing(hook) => hook.do_stall(scheduler, inner),
      #[cfg(test)]
      Self::TestIndexFileDeleter(hook) => hook.do_stall(scheduler, inner),
      #[cfg(test)]
      Self::MergeDvUpdateFileOnGetReader(hook) => hook.do_stall(scheduler, inner),
      #[cfg(test)]
      Self::MergeDvUpdateFileOnCommit(hook) => hook.do_stall(scheduler, inner),
      #[cfg(test)]
      Self::StalledMerges(hook) => hook.do_stall(scheduler, inner),
      #[cfg(test)]
      Self::LuceneTestCaseAlwaysProceed(hook) => hook.do_stall(scheduler, inner),
    }
  }

  fn do_merge<MS, D>(
    &self,
    scheduler: &ConcurrentMergeScheduler,
    merge_source: &MS,
    merge: OneMerge<D, MS::Reader>,
  ) -> Result<()>
  where
    MS: MergeSource<D>,
    D: Directory + 'static,
  {
    match self {
      Self::Default => ConcurrentMergeSchedulerDefaults::do_merge(scheduler, merge_source, merge),
      #[cfg(test)]
      Self::AlwaysParallelIntraMerge => {
        ConcurrentMergeSchedulerDefaults::do_merge(scheduler, merge_source, merge)
      },
      #[cfg(test)]
      Self::AssertSmallMergesUseDirectExecutor => {
        assert!(scheduler.get_intra_merge_executor(&merge)?.is_direct());
        ConcurrentMergeSchedulerDefaults::do_merge(scheduler, merge_source, merge)
      },
      #[cfg(test)]
      Self::MaxMergeCount(hook) => hook.do_merge(scheduler, merge_source, merge),
      #[cfg(test)]
      Self::Tracking(hook) => hook.do_merge(scheduler, merge_source, merge),
      #[cfg(test)]
      Self::LiveMaxMergeCount(hook) => hook.do_merge(scheduler, merge_source, merge),
      #[cfg(test)]
      Self::MaybeStallCalled(hook) => hook.do_merge(scheduler, merge_source, merge),
      #[cfg(test)]
      Self::HangDuringRollback(hook) => hook.do_merge(scheduler, merge_source, merge),
      #[cfg(test)]
      Self::NoStallMergeThreads(hook) => hook.do_merge(scheduler, merge_source, merge),
      #[cfg(test)]
      Self::MergeThreadMessages(hook) => hook.do_merge(scheduler, merge_source, merge),
      #[cfg(test)]
      Self::CloseWhileMergeIsRunning(hook) => hook.do_merge(scheduler, merge_source, merge),
      #[cfg(test)]
      Self::Suppressing(hook) => hook.do_merge(scheduler, merge_source, merge),
      #[cfg(test)]
      Self::TestIndexFileDeleter(hook) => hook.do_merge(scheduler, merge_source, merge),
      #[cfg(test)]
      Self::MergeDvUpdateFileOnGetReader(hook) => hook.do_merge(scheduler, merge_source, merge),
      #[cfg(test)]
      Self::MergeDvUpdateFileOnCommit(hook) => hook.do_merge(scheduler, merge_source, merge),
      #[cfg(test)]
      Self::StalledMerges(hook) => hook.do_merge(scheduler, merge_source, merge),
      #[cfg(test)]
      Self::LuceneTestCaseAlwaysProceed(hook) => hook.do_merge(scheduler, merge_source, merge),
    }
  }

  fn get_merge_thread<MS, D>(
    &self,
    scheduler: &ConcurrentMergeScheduler,
    inner: &mut Inner,
    merge_source: MS,
    merge: OneMerge<D, MS::Reader>,
  ) -> Result<MergeThread<MS, D>>
  where
    MS: MergeSource<D> + Clone + 'static,
    D: Directory + 'static,
    OneMerge<D, MS::Reader>: Send + 'static,
  {
    match self {
      Self::Default => {
        ConcurrentMergeSchedulerDefaults::get_merge_thread(scheduler, inner, merge_source, merge)
      },
      #[cfg(test)]
      Self::AlwaysParallelIntraMerge | Self::AssertSmallMergesUseDirectExecutor => {
        ConcurrentMergeSchedulerDefaults::get_merge_thread(scheduler, inner, merge_source, merge)
      },
      #[cfg(test)]
      Self::MaxMergeCount(hook) => hook.get_merge_thread(scheduler, inner, merge_source, merge),
      #[cfg(test)]
      Self::Tracking(hook) => hook.get_merge_thread(scheduler, inner, merge_source, merge),
      #[cfg(test)]
      Self::LiveMaxMergeCount(hook) => hook.get_merge_thread(scheduler, inner, merge_source, merge),
      #[cfg(test)]
      Self::MaybeStallCalled(hook) => hook.get_merge_thread(scheduler, inner, merge_source, merge),
      #[cfg(test)]
      Self::HangDuringRollback(hook) => {
        hook.get_merge_thread(scheduler, inner, merge_source, merge)
      },
      #[cfg(test)]
      Self::NoStallMergeThreads(hook) => {
        hook.get_merge_thread(scheduler, inner, merge_source, merge)
      },
      #[cfg(test)]
      Self::MergeThreadMessages(hook) => {
        hook.get_merge_thread(scheduler, inner, merge_source, merge)
      },
      #[cfg(test)]
      Self::CloseWhileMergeIsRunning(hook) => {
        hook.get_merge_thread(scheduler, inner, merge_source, merge)
      },
      #[cfg(test)]
      Self::Suppressing(hook) => hook.get_merge_thread(scheduler, inner, merge_source, merge),
      #[cfg(test)]
      Self::TestIndexFileDeleter(hook) => {
        hook.get_merge_thread(scheduler, inner, merge_source, merge)
      },
      #[cfg(test)]
      Self::MergeDvUpdateFileOnGetReader(hook) => {
        hook.get_merge_thread(scheduler, inner, merge_source, merge)
      },
      #[cfg(test)]
      Self::MergeDvUpdateFileOnCommit(hook) => {
        hook.get_merge_thread(scheduler, inner, merge_source, merge)
      },
      #[cfg(test)]
      Self::StalledMerges(hook) => hook.get_merge_thread(scheduler, inner, merge_source, merge),
      #[cfg(test)]
      Self::LuceneTestCaseAlwaysProceed(hook) => {
        hook.get_merge_thread(scheduler, inner, merge_source, merge)
      },
    }
  }

  fn handle_merge_exception(
    &self,
    scheduler: &ConcurrentMergeScheduler,
    result: CaughtResult,
  ) -> Result<()> {
    match self {
      Self::Default => ConcurrentMergeSchedulerDefaults::handle_merge_exception(scheduler, result),
      #[cfg(test)]
      Self::AlwaysParallelIntraMerge | Self::AssertSmallMergesUseDirectExecutor => {
        ConcurrentMergeSchedulerDefaults::handle_merge_exception(scheduler, result)
      },
      #[cfg(test)]
      Self::MaxMergeCount(hook) => hook.handle_merge_exception(scheduler, result),
      #[cfg(test)]
      Self::Tracking(hook) => hook.handle_merge_exception(scheduler, result),
      #[cfg(test)]
      Self::LiveMaxMergeCount(hook) => hook.handle_merge_exception(scheduler, result),
      #[cfg(test)]
      Self::MaybeStallCalled(hook) => hook.handle_merge_exception(scheduler, result),
      #[cfg(test)]
      Self::HangDuringRollback(hook) => hook.handle_merge_exception(scheduler, result),
      #[cfg(test)]
      Self::NoStallMergeThreads(hook) => hook.handle_merge_exception(scheduler, result),
      #[cfg(test)]
      Self::MergeThreadMessages(hook) => hook.handle_merge_exception(scheduler, result),
      #[cfg(test)]
      Self::CloseWhileMergeIsRunning(hook) => hook.handle_merge_exception(scheduler, result),
      #[cfg(test)]
      Self::Suppressing(hook) => hook.handle_merge_exception(scheduler, result),
      #[cfg(test)]
      Self::TestIndexFileDeleter(hook) => hook.handle_merge_exception(scheduler, result),
      #[cfg(test)]
      Self::MergeDvUpdateFileOnGetReader(hook) => hook.handle_merge_exception(scheduler, result),
      #[cfg(test)]
      Self::MergeDvUpdateFileOnCommit(hook) => hook.handle_merge_exception(scheduler, result),
      #[cfg(test)]
      Self::StalledMerges(hook) => hook.handle_merge_exception(scheduler, result),
      #[cfg(test)]
      Self::LuceneTestCaseAlwaysProceed(hook) => hook.handle_merge_exception(scheduler, result),
    }
  }

  fn target_mb_per_sec_changed(&self, scheduler: &ConcurrentMergeScheduler) {
    match self {
      Self::Default => ConcurrentMergeSchedulerDefaults::target_mb_per_sec_changed(scheduler),
      #[cfg(test)]
      Self::AlwaysParallelIntraMerge | Self::AssertSmallMergesUseDirectExecutor => {
        ConcurrentMergeSchedulerDefaults::target_mb_per_sec_changed(scheduler)
      },
      #[cfg(test)]
      Self::MaxMergeCount(hook) => hook.target_mb_per_sec_changed(scheduler),
      #[cfg(test)]
      Self::Tracking(hook) => hook.target_mb_per_sec_changed(scheduler),
      #[cfg(test)]
      Self::LiveMaxMergeCount(hook) => hook.target_mb_per_sec_changed(scheduler),
      #[cfg(test)]
      Self::MaybeStallCalled(hook) => hook.target_mb_per_sec_changed(scheduler),
      #[cfg(test)]
      Self::HangDuringRollback(hook) => hook.target_mb_per_sec_changed(scheduler),
      #[cfg(test)]
      Self::NoStallMergeThreads(hook) => hook.target_mb_per_sec_changed(scheduler),
      #[cfg(test)]
      Self::MergeThreadMessages(hook) => hook.target_mb_per_sec_changed(scheduler),
      #[cfg(test)]
      Self::CloseWhileMergeIsRunning(hook) => hook.target_mb_per_sec_changed(scheduler),
      #[cfg(test)]
      Self::Suppressing(hook) => hook.target_mb_per_sec_changed(scheduler),
      #[cfg(test)]
      Self::TestIndexFileDeleter(hook) => hook.target_mb_per_sec_changed(scheduler),
      #[cfg(test)]
      Self::MergeDvUpdateFileOnGetReader(hook) => hook.target_mb_per_sec_changed(scheduler),
      #[cfg(test)]
      Self::MergeDvUpdateFileOnCommit(hook) => hook.target_mb_per_sec_changed(scheduler),
      #[cfg(test)]
      Self::StalledMerges(hook) => hook.target_mb_per_sec_changed(scheduler),
      #[cfg(test)]
      Self::LuceneTestCaseAlwaysProceed(hook) => hook.target_mb_per_sec_changed(scheduler),
    }
  }
}

impl Default for ConcurrentMergeScheduler {
  fn default() -> Self {
    Self::new()
  }
}

impl ConcurrentMergeScheduler {
  /** Sole constructor, with all settings set to default values. */
  pub fn new() -> Self {
    Self::with_hook(ConcurrentMergeSchedulerHook::Default)
  }

  pub(crate) fn with_hook(hook: ConcurrentMergeSchedulerHook) -> Self {
    let inner = Mutex::new(Inner {
      merge_threads: Vec::new(),
      active_merge_order: Vec::new(),
      max_thread_count: AUTO_DETECT_MERGES_AND_THREADS,
      max_merge_count: AUTO_DETECT_MERGES_AND_THREADS,
      merge_thread_count: 0,
      target_mb_per_sec: START_MB_PER_SEC,
      do_auto_io_throttle: false,
      force_merge_mb_per_sec: f64::INFINITY,
      suppress_exceptions: false,
    });
    Self {
      shared: Arc::new(ConcurrentMergeSchedulerShared {
        inner,
        changed: Condvar::new(),
        info_stream: Mutex::new(get_default_info_stream()),
        intra_merge_executor: Mutex::new(None),
      }),
      hook,
    }
  }

  /**
   * Expert: directly set the maximum number of merge threads and simultaneous merges allowed.
   *
   * - `max_merge_count`: the maximum number of simultaneous merges. If a merge is necessary
   *   yet we already have this many threads running, the incoming thread (calling
   *   `add_document` or `update_document`) blocks until a merge thread completes. Only the
   *   smallest `max_thread_count` merges run at a time.
   * - `max_thread_count`: the maximum number of simultaneous merge threads running at once.
   *   This must be <= `max_merge_count`.
   */
  pub fn set_max_merges_and_threads(
    &self,
    max_merge_count: i32,
    max_thread_count: i32,
  ) -> Result<()> {
    let mut inner = self.shared.inner.lock();
    if max_merge_count == AUTO_DETECT_MERGES_AND_THREADS
      && max_thread_count == AUTO_DETECT_MERGES_AND_THREADS
    {
      inner.max_merge_count = AUTO_DETECT_MERGES_AND_THREADS;
      inner.max_thread_count = AUTO_DETECT_MERGES_AND_THREADS;
    } else if max_merge_count == AUTO_DETECT_MERGES_AND_THREADS
      || max_thread_count == AUTO_DETECT_MERGES_AND_THREADS
    {
      return Err(LuceneError::illegal_argument(
        "both max_merge_count and max_thread_count must be AUTO_DETECT_MERGES_AND_THREADS",
      ));
    } else {
      if max_thread_count < 1 {
        return Err(LuceneError::illegal_argument(
          "max_thread_count should be at least 1",
        ));
      }
      if max_merge_count < 1 {
        return Err(LuceneError::illegal_argument(
          "max_merge_count should be at least 1",
        ));
      }
      if max_thread_count > max_merge_count {
        return Err(LuceneError::illegal_argument(format!(
          "max_thread_count should be <= max_merge_count (= {max_merge_count})"
        )));
      }
      inner.max_thread_count = max_thread_count;
      inner.max_merge_count = max_merge_count;
    }
    Ok(())
  }

  /**
   * Sets max merges and threads to proper defaults for rotational or non-rotational storage.
   *
   * - `spins`: `true` selects defaults for rotational storage; `false` selects defaults for
   *   non-rotational storage such as solid-state drives.
   */
  pub fn set_default_max_merges_and_threads(&self, spins: bool) {
    let mut inner = self.shared.inner.lock();
    if spins {
      inner.max_thread_count = 1;
      inner.max_merge_count = 6;
    } else {
      let mut core_count = thread::available_parallelism()
        .map(|count| count.get() as i32)
        .unwrap_or(1);

      // Let tests override this to help reproducing a failure on a machine that has a different
      // core count than the one where the test originally failed:
      if let Ok(value) = std::env::var(DEFAULT_CPU_CORE_COUNT_PROPERTY)
        && let Ok(parsed) = value.parse::<i32>()
      {
        core_count = parsed;
      }

      // If you are indexing at full throttle, how many merge threads do you need to keep up? It
      // depends: for most data structures, merging is cheaper than indexing/flushing, but for knn
      // vectors, merges can require about as much work as the initial indexing/flushing. Plus
      // documents are indexed/flushed only once, but may be merged multiple times.
      // Here, we assume an intermediate scenario where merging requires about as much work as
      // indexing/flushing overall, so we give half the core count to merges.
      inner.max_thread_count = cmp::max(1, core_count / 2);
      inner.max_merge_count = inner.max_thread_count + 5;
    }
  }

  /**
   * Set the per-merge IO throttle rate for forced merges (default: `f64::INFINITY`).
   */
  pub fn set_force_merge_mb_per_sec(&self, v: f64) -> Result<()> {
    let mut inner = self.shared.inner.lock();
    inner.force_merge_mb_per_sec = v;
    self.update_merge_threads(&mut inner)
  }

  /** Get the per-merge IO throttle rate for forced merges. */
  pub fn get_force_merge_mb_per_sec(&self) -> f64 {
    self.shared.inner.lock().force_merge_mb_per_sec
  }

  /**
   * Turn on dynamic IO throttling, to adaptively rate limit writes bytes/sec to the minimal rate
   * necessary so merges do not fall behind. By default this is disabled and writes are not
   * rate-limited.
   */
  pub fn enable_auto_io_throttle(&self) -> Result<()> {
    let mut inner = self.shared.inner.lock();
    inner.do_auto_io_throttle = true;
    inner.target_mb_per_sec = START_MB_PER_SEC;
    self.update_merge_threads(&mut inner)
  }

  /**
   * Turn off auto IO throttling.
   *
   * See [`Self::enable_auto_io_throttle`].
   */
  pub fn disable_auto_io_throttle(&self) -> Result<()> {
    let mut inner = self.shared.inner.lock();
    inner.do_auto_io_throttle = false;
    self.update_merge_threads(&mut inner)
  }

  /** Returns true if auto IO throttling is currently enabled. */
  pub fn get_auto_io_throttle(&self) -> bool {
    self.shared.inner.lock().do_auto_io_throttle
  }

  /**
   * Returns the currently set per-merge IO writes rate limit, if `enable_auto_io_throttle` was
   * called, else `f64::INFINITY`.
   */
  pub fn get_io_rate_limit_mb_per_sec(&self) -> f64 {
    let inner = self.shared.inner.lock();
    if inner.do_auto_io_throttle {
      inner.target_mb_per_sec
    } else {
      f64::INFINITY
    }
  }

  /**
   * Returns `max_thread_count`.
   *
   * See [`Self::set_max_merges_and_threads`].
   */
  pub fn get_max_thread_count(&self) -> i32 {
    self.shared.inner.lock().max_thread_count
  }

  /** See `set_max_merges_and_threads`. */
  pub fn get_max_merge_count(&self) -> i32 {
    self.shared.inner.lock().max_merge_count
  }

  /** Removes the calling thread from the active merge threads. */
  fn remove_merge_thread(inner: &mut Inner) {
    let removed = if let Some(index) = inner
      .merge_threads
      .iter()
      .position(|merge_thread| merge_thread.is_current_thread())
    {
      inner.merge_threads.remove(index);
      true
    } else {
      false
    };
    debug_assert!(removed, "merge thread was not found");
  }

  /**
   * Called whenever the running merges have changed, to set merge IO limits. This method sorts the
   * merge threads by their merge size in descending order and then pauses/unpauses threads from
   * first to last -- that way, smaller merges are guaranteed to run before larger ones.
   */
  fn update_merge_threads(&self, inner: &mut Inner) -> Result<()> {
    self.hook.update_merge_threads(self, inner)
  }

  pub(crate) fn get_parallel_merge_executor(&self) -> Result<Arc<TaskExecutor>> {
    let num_threads = cmp::max(1, self.shared.inner.lock().max_thread_count) as usize;
    let mut cached = self.shared.intra_merge_executor.lock();
    if let Some((cached_num_threads, executor)) = cached.as_ref()
      && *cached_num_threads == num_threads
    {
      return Ok(Arc::clone(executor));
    }

    let pool = ThreadPoolBuilder::new()
      .num_threads(num_threads)
      .thread_name(|index| format!("Lucene Intra-Merge Thread #{index}"))
      .build()
      .map_err(|error| {
        LuceneError::illegal_state(format!("failed to build intra-merge executor: {error}"))
      })?;
    let executor = Arc::new(TaskExecutor::new(Arc::new(pool)));
    *cached = Some((num_threads, Arc::clone(&executor)));
    Ok(executor)
  }
}

impl ConcurrentMergeSchedulerDefaults {
  pub(crate) fn get_intra_merge_executor<D, CR>(
    scheduler: &ConcurrentMergeScheduler,
    merge: &OneMerge<D, CR>,
  ) -> Result<Arc<TaskExecutor>>
  where
    D: Directory,
    CR: CodecReader,
  {
    // Don't do multithreaded merges for small merges.
    if merge.estimated_merge_bytes.load(Ordering::SeqCst) < MIN_BIG_MERGE_MB * 1024 * 1024 {
      Ok(Arc::new(TaskExecutor::direct()))
    } else {
      scheduler.get_parallel_merge_executor()
    }
  }

  pub(crate) fn update_merge_threads(
    _scheduler: &ConcurrentMergeScheduler,
    inner: &mut Inner,
  ) -> Result<()> {
    // Only look at threads that are alive and not in the process of stopping (i.e. have an active
    // merge):
    let mut thread_idx = 0;
    while thread_idx < inner.merge_threads.len() {
      if !inner.merge_threads[thread_idx].is_alive() {
        // Prune any dead threads
        inner.merge_threads.remove(thread_idx);
        continue;
      }
      thread_idx += 1;
    }

    let active_merges = &mut inner.active_merge_order;
    active_merges.clear();
    active_merges.extend(0..inner.merge_threads.len());

    // Sort the merge threads, largest first:
    active_merges.sort_by(|left, right| {
      inner.merge_threads[*right]
        .estimated_merge_bytes
        .load(Ordering::SeqCst)
        .cmp(
          &inner.merge_threads[*left]
            .estimated_merge_bytes
            .load(Ordering::SeqCst),
        )
    });

    let active_merge_count = active_merges.len();

    let mut big_merge_count = 0;

    for thread_idx in (0..active_merge_count).rev() {
      let merge_thread = &inner.merge_threads[active_merges[thread_idx]];
      if merge_thread.estimated_merge_bytes.load(Ordering::SeqCst) > MIN_BIG_MERGE_MB * 1024 * 1024
      {
        big_merge_count = 1 + thread_idx;
        break;
      }
    }

    for (thread_idx, &merge_idx) in active_merges.iter().enumerate() {
      let merge_thread = &inner.merge_threads[merge_idx];

      // pause the thread if max_thread_count is smaller than the number of merge threads.
      let max_thread_count = cmp::max(0, inner.max_thread_count) as usize;
      let do_pause = thread_idx < big_merge_count.saturating_sub(max_thread_count);

      let new_mb_per_sec = if do_pause {
        0.0
      } else if merge_thread.merge_stat.max_num_segments() != -1 {
        inner.force_merge_mb_per_sec
      } else if !inner.do_auto_io_throttle {
        f64::INFINITY
      } else if merge_thread.estimated_merge_bytes.load(Ordering::SeqCst)
        < MIN_BIG_MERGE_MB * 1024 * 1024
      {
        // Don't rate limit small merges:
        f64::INFINITY
      } else {
        inner.target_mb_per_sec
      };

      merge_thread.rate_limiter.set_mb_per_sec(new_mb_per_sec)?;
    }

    Ok(())
  }
}

impl ConcurrentMergeScheduler {
  fn init_dynamic_defaults<D>(&self, _directory: &D)
  where
    D: Directory,
  {
    let should_init = self.shared.inner.lock().max_thread_count == AUTO_DETECT_MERGES_AND_THREADS;
    if should_init {
      self.set_default_max_merges_and_threads(false);
    }
  }

  fn rate_to_string(mb_per_sec: f64) -> String {
    if mb_per_sec == 0.0 {
      "stopped".to_string()
    } else if mb_per_sec == f64::INFINITY {
      "unlimited".to_string()
    } else {
      format!("{mb_per_sec:.1} MB/sec")
    }
  }
}

impl CloseableRef for ConcurrentMergeScheduler {
  fn close(&self) -> Result<()> {
    self.hook.close(self)
  }
}

impl ConcurrentMergeSchedulerDefaults {
  pub(crate) fn close(scheduler: &ConcurrentMergeScheduler) -> Result<()> {
    scheduler.sync()?;
    scheduler.shared.intra_merge_executor.lock().take();
    Ok(())
  }
}

impl ConcurrentMergeScheduler {
  /**
   * Wait for any running merge threads to finish. This call is not interruptible as used by
   * `close`.
   */
  pub fn sync(&self) -> Result<()> {
    let mut inner = self.shared.inner.lock();
    while inner
      .merge_threads
      .iter()
      .any(|merge_thread| merge_thread.is_alive() && !merge_thread.is_current_thread())
    {
      self.shared.changed.wait(&mut inner);
    }
    Ok(())
  }

  /**
   * Returns the number of merge threads that are alive, ignoring the calling thread if it is a
   * merge thread. Note that this number is <= `merge_threads` size.
   *
   * @lucene.internal
   */
  pub fn merge_thread_count(&self) -> usize {
    let inner = self.shared.inner.lock();
    Self::merge_thread_count_locked(&inner)
  }

  fn merge_thread_count_locked(inner: &Inner) -> usize {
    inner
      .merge_threads
      .iter()
      .filter(|merge_thread| {
        merge_thread.is_alive()
          && !merge_thread.is_current_thread()
          && !merge_thread.rate_limiter.is_aborted()
      })
      .count()
  }

  /// Returns true if messages about merge scheduling are enabled.
  fn verbose(&self) -> bool {
    let info_stream = self.shared.info_stream.lock().clone();
    info_stream.is_enabled("MS")
  }

  /// Outputs the given message. This method assumes [`Self::verbose`] returned true.
  fn message(&self, message: &str) -> Result<()> {
    let info_stream = self.shared.info_stream.lock().clone();
    info_stream.message("MS", message)
  }
}

impl MergeScheduler for ConcurrentMergeScheduler {
  type Directory<D>
    = RateLimitedDirectory<D, Arc<MergeRateLimiter>>
  where
    D: Directory;

  fn wrap_for_merge<D>(&self, in_: D) -> Result<Self::Directory<D>>
  where
    D: Directory,
  {
    let rate_limiter = CURRENT_MERGE_RATE_LIMITER
      .with(|slot| slot.borrow().clone())
      .ok_or_else(|| {
        LuceneError::illegal_state(format!(
          "wrap_for_merge should be called from MergeThread. Current thread: {:?}",
          thread::current().id()
        ))
      })?;

    // Return a wrapped Directory which has rate-limited output.
    // Note: the rate limiter is only per thread. So, if there are multiple merge threads running
    // and throttling is required, each thread will be throttled independently.
    // The implication of this, is that the total IO rate could be higher than the target rate.
    Ok(RateLimitedDirectory::new(in_, rate_limiter))
  }

  fn get_intra_merge_executor<D, CR>(&self, merge: &OneMerge<D, CR>) -> Result<Arc<TaskExecutor>>
  where
    D: Directory,
    CR: CodecReader,
  {
    self.hook.get_intra_merge_executor(self, merge)
  }

  fn initialize<D>(&mut self, info_stream: InfoStreamMT, directory: &D) -> Result<()>
  where
    D: Directory,
  {
    *self.shared.info_stream.lock() = info_stream;
    self.init_dynamic_defaults(directory);
    Ok(())
  }

  fn merge<MS, D>(&self, merge_source: MS, trigger: MergeTrigger) -> Result<()>
  where
    MS: MergeSource<D> + Clone + 'static,
    D: Directory + 'static,
    OneMerge<D, MS::Reader>: Send + 'static,
  {
    self.hook.merge(self, merge_source, trigger)
  }
}

impl ConcurrentMergeSchedulerDefaults {
  pub(crate) fn merge<MS, D>(
    scheduler: &ConcurrentMergeScheduler,
    merge_source: MS,
    trigger: MergeTrigger,
  ) -> Result<()>
  where
    MS: MergeSource<D> + Clone + 'static,
    D: Directory + 'static,
    OneMerge<D, MS::Reader>: Send + 'static,
  {
    let mut inner = scheduler.shared.inner.lock();
    scheduler.merge_locked(&mut inner, merge_source, trigger)
  }
}

impl ConcurrentMergeScheduler {
  fn merge_locked<MS, D>(
    &self,
    inner: &mut MutexGuard<'_, Inner>,
    merge_source: MS,
    trigger: MergeTrigger,
  ) -> Result<()>
  where
    MS: MergeSource<D> + Clone + 'static,
    D: Directory + 'static,
    OneMerge<D, MS::Reader>: Send + 'static,
  {
    if trigger == MergeTrigger::Closing {
      // Disable throttling on close:
      inner.target_mb_per_sec = MAX_MERGE_MB_PER_SEC;
      self.update_merge_threads(inner)?;
    }

    // Iterate, pulling from the IndexWriter's queue of pending merges, until it's empty:
    loop {
      if !self.maybe_stall(inner, &merge_source)? {
        break;
      }

      let merge = match merge_source.get_next_merge()? {
        Some(merge) => merge,
        None => return Ok(()),
      };

      let merge_stat = merge.stat.clone();

      let mut success = false;
      let setup_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
          let new_merge_thread = self.get_merge_thread(inner, merge_source.clone(), merge)?;
          let merge_thread_state = &new_merge_thread.state;
          inner.merge_threads.push(merge_thread_state.clone());
          self.update_io_throttle(inner, merge_thread_state)?;
          new_merge_thread.start(self.clone())?;
          self.update_merge_threads(inner)?;
          success = true;
          Ok(())
        }));

      if !success {
        merge_source.on_merge_finished(&merge_stat, None);
      }
      unwrap_caught_result!(setup_result)?;
    }

    Ok(())
  }

  /**
   * This is invoked by `merge` to possibly stall the incoming thread when there are too many
   * merges running or pending. The default behavior is to force this thread, which is producing too
   * many segments for merging to keep up, to wait until merges catch up. Applications that can take
   * other less drastic measures, such as limiting how many threads are allowed to index, can do
   * nothing here and throttle elsewhere.
   *
   * If this method wants to stall but the calling thread is a merge thread, it should return
   * false to tell caller not to kick off any new merges.
   */
  fn maybe_stall<MS, D>(&self, inner: &mut MutexGuard<'_, Inner>, merge_source: &MS) -> Result<bool>
  where
    MS: MergeSource<D>,
    D: Directory,
  {
    self.hook.maybe_stall(self, inner, merge_source)
  }
}

impl ConcurrentMergeSchedulerDefaults {
  pub(crate) fn maybe_stall<MS, D>(
    scheduler: &ConcurrentMergeScheduler,
    inner: &mut MutexGuard<'_, Inner>,
    merge_source: &MS,
  ) -> Result<bool>
  where
    MS: MergeSource<D>,
    D: Directory,
  {
    let mut start_stall_time = None;

    while merge_source.has_pending_merges(None)?
      && ConcurrentMergeScheduler::merge_thread_count_locked(inner)
        >= inner.max_merge_count as usize
    {
      // This means merging has fallen too far behind: we have already created max_merge_count
      // threads, and now there's at least one more merge pending. Note that only max_thread_count
      // of those created merge threads will actually be running; the rest will be paused (see
      // update_merge_threads). We stall this producer thread to prevent creation of new segments,
      // until merging has caught up:
      if inner
        .merge_threads
        .iter()
        .any(|merge_thread| merge_thread.is_current_thread())
      {
        // Never stall a merge thread since this blocks the thread from finishing and calling
        // update_merge_threads, and blocking it accomplishes nothing anyway (it's not really a
        // segment producer):
        return Ok(false);
      }

      if start_stall_time.is_none() {
        start_stall_time = Some(Instant::now());
      }
      scheduler.do_stall(inner);
    }

    Ok(true)
  }
}

impl ConcurrentMergeScheduler {
  /** Called from `maybe_stall` to pause the calling thread for a bit. */
  fn do_stall(&self, inner: &mut MutexGuard<'_, Inner>) {
    self.hook.do_stall(self, inner)
  }
}

impl ConcurrentMergeSchedulerDefaults {
  pub(crate) fn do_stall(scheduler: &ConcurrentMergeScheduler, inner: &mut MutexGuard<'_, Inner>) {
    // Defensively wait for only .25 seconds in case we are missing a notify/all somewhere:
    scheduler
      .shared
      .changed
      .wait_for(inner, Duration::from_millis(250));
  }
}

impl ConcurrentMergeScheduler {
  /**
   * Does the actual merge, by calling `MergeSource::merge`.
   */
  fn do_merge<MS, D>(&self, merge_source: &MS, merge: OneMerge<D, MS::Reader>) -> Result<()>
  where
    MS: MergeSource<D>,
    D: Directory + 'static,
  {
    self.hook.do_merge(self, merge_source, merge)
  }
}

impl ConcurrentMergeSchedulerDefaults {
  pub(crate) fn do_merge<MS, D>(
    _scheduler: &ConcurrentMergeScheduler,
    merge_source: &MS,
    merge: OneMerge<D, MS::Reader>,
  ) -> Result<()>
  where
    MS: MergeSource<D>,
    D: Directory + 'static,
  {
    merge_source.merge(merge)
  }
}

impl ConcurrentMergeScheduler {
  /** Create and return a new MergeThread */
  fn get_merge_thread<MS, D>(
    &self,
    inner: &mut Inner,
    merge_source: MS,
    merge: OneMerge<D, MS::Reader>,
  ) -> Result<MergeThread<MS, D>>
  where
    MS: MergeSource<D> + Clone + 'static,
    D: Directory + 'static,
    OneMerge<D, MS::Reader>: Send + 'static,
  {
    self.hook.get_merge_thread(self, inner, merge_source, merge)
  }
}

impl ConcurrentMergeSchedulerDefaults {
  pub(crate) fn get_merge_thread<MS, D>(
    _scheduler: &ConcurrentMergeScheduler,
    inner: &mut Inner,
    merge_source: MS,
    merge: OneMerge<D, MS::Reader>,
  ) -> Result<MergeThread<MS, D>>
  where
    MS: MergeSource<D> + Clone + 'static,
    D: Directory + 'static,
    OneMerge<D, MS::Reader>: Send + 'static,
  {
    let name = format!("Lucene Merge Thread #{}", inner.merge_thread_count);
    inner.merge_thread_count += 1;
    let state = Arc::new(MergeThreadState::new(name, &merge));
    Ok(MergeThread::new(state, merge_source, merge))
  }
}

impl ConcurrentMergeScheduler {
  fn run_on_merge_finished<MS, D>(&self, merge_source: MS) -> Result<()>
  where
    MS: MergeSource<D> + Clone + 'static,
    D: Directory + 'static,
    OneMerge<D, MS::Reader>: Send + 'static,
  {
    let mut inner = self.shared.inner.lock();
    // The merge call as well as the merge thread handling in the finally
    // block must be sync'd on CMS otherwise stalling decisions might cause
    // us to miss pending merges.
    debug_assert!(
      inner
        .merge_threads
        .iter()
        .any(|merge_thread| merge_thread.is_current_thread()),
      "caller is not a merge thread"
    );

    // Let CMS run new merges if necessary:
    let merge_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      match self.merge_locked(&mut inner, merge_source, MergeTrigger::MergeFinished) {
        Ok(()) => Ok(()),
        Err(err) if matches!(err.kind(), LuceneErrorKind::AlreadyClosed(_)) => Ok(()),
        Err(err) if err.is_io_error() => {
          let mut unchecked = LuceneError::unchecked_io_error(err.to_string());
          unchecked.add_suppressed(err);
          Err(unchecked)
        },
        Err(err) => Err(err),
      }
    }));
    Self::remove_merge_thread(&mut inner);
    self.update_merge_threads(&mut inner)?;
    // In case we had stalled indexing, we can now wake up
    // and possibly unstall:
    self.shared.changed.notify_all();
    unwrap_caught_result!(merge_result)
  }
}

/** State for a running merge thread tracked by the scheduler. */
struct MergeThreadState {
  name: String,
  owner_id: Mutex<Option<ThreadId>>,
  estimated_merge_bytes: Arc<AtomicI64>,
  merge_start_ns: Arc<Mutex<Instant>>,
  merge_stat: MergeStat,
  rate_limiter: Arc<MergeRateLimiter>,
  alive: AtomicBool,
}

impl MergeThreadState {
  /** Sole constructor. */
  fn new<D, CR>(name: String, merge: &OneMerge<D, CR>) -> Self
  where
    D: Directory,
    CR: CodecReader,
  {
    Self {
      name,
      owner_id: Mutex::new(None),
      estimated_merge_bytes: merge.estimated_merge_bytes.clone(),
      merge_start_ns: merge.merge_start_ns.clone(),
      merge_stat: merge.stat.clone(),
      rate_limiter: Arc::new(MergeRateLimiter::new(merge.get_merge_progress())),
      alive: AtomicBool::new(false),
    }
  }

  fn set_owner_to_current_thread(&self) {
    *self.owner_id.lock() = Some(thread::current().id());
  }

  fn is_current_thread(&self) -> bool {
    self
      .owner_id
      .lock()
      .map(|owner_id| owner_id == thread::current().id())
      .unwrap_or(false)
  }

  fn set_alive(&self, alive: bool) {
    self.alive.store(alive, Ordering::SeqCst);
  }

  fn is_alive(&self) -> bool {
    self.alive.load(Ordering::SeqCst)
  }
}

/** Runs a merge to execute a single merge, then exits. */
pub(crate) struct MergeThread<MS, D>
where
  MS: MergeSource<D>,
  D: Directory,
{
  state: Arc<MergeThreadState>,
  merge_source: MS,
  merge: OneMerge<D, MS::Reader>,
}

impl<MS, D> MergeThread<MS, D>
where
  MS: MergeSource<D> + Clone + 'static,
  D: Directory + 'static,
  OneMerge<D, MS::Reader>: Send + 'static,
{
  /** Sole constructor. */
  fn new(state: Arc<MergeThreadState>, merge_source: MS, merge: OneMerge<D, MS::Reader>) -> Self {
    Self {
      state,
      merge_source,
      merge,
    }
  }

  #[cfg(test)]
  pub(crate) fn name(&self) -> &str {
    &self.state.name
  }

  fn run(self, merge_scheduler: ConcurrentMergeScheduler) -> Result<()> {
    let MergeThread {
      state,
      merge_source,
      merge,
    } = self;

    struct AliveOnExit<'a> {
      state: &'a MergeThreadState,
    }

    impl Drop for AliveOnExit<'_> {
      fn drop(&mut self) {
        self.state.set_alive(false);
      }
    }

    let _alive_on_exit = AliveOnExit { state: &state };
    // Keep the guard outside run_merge so its source, merge and scheduler are
    // dropped before the thread is announced as no longer alive, also on unwind.
    Self::run_merge(&state, merge_source, merge, merge_scheduler)
  }

  fn run_merge(
    state: &MergeThreadState,
    merge_source: MS,
    merge: OneMerge<D, MS::Reader>,
    merge_scheduler: ConcurrentMergeScheduler,
  ) -> Result<()> {
    state.set_owner_to_current_thread();
    let previous =
      CURRENT_MERGE_RATE_LIMITER.with(|slot| slot.borrow_mut().replace(state.rate_limiter.clone()));
    let merge_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
      if merge_scheduler.verbose() {
        merge_scheduler.message(&format!("merge thread {} start", state.name))?;
      }

      merge_scheduler.do_merge(&merge_source, merge)?;
      if merge_scheduler.verbose() {
        merge_scheduler.message(&format!(
          "merge thread {} merge segment [{}] done estSize={:.1} MB (written={:.1} MB) runTime={:.1}s (stopped={:.1}s, paused={:.1}s) rate={}",
          state.name,
          state.merge_stat.name().unwrap_or_else(|| Arc::from("_na_")),
          ConcurrentMergeScheduler::bytes_to_mb(
            state.estimated_merge_bytes.load(Ordering::SeqCst),
          ),
          ConcurrentMergeScheduler::bytes_to_mb(state.rate_limiter.get_total_bytes_written()),
          state.merge_start_ns.lock().elapsed().as_secs_f64(),
          Duration::from_nanos(state.rate_limiter.get_total_stopped_ns()?).as_secs_f64(),
          Duration::from_nanos(state.rate_limiter.get_total_paused_ns()?).as_secs_f64(),
          ConcurrentMergeScheduler::rate_to_string(state.rate_limiter.get_mb_per_sec()),
        ))?;
      }
      merge_scheduler.run_on_merge_finished(merge_source)?;
      if merge_scheduler.verbose() {
        merge_scheduler.message(&format!("merge thread {} end", state.name))?;
      }
      Ok(())
    }));
    CURRENT_MERGE_RATE_LIMITER.with(|slot| {
      *slot.borrow_mut() = previous;
    });

    if !matches!(&merge_result, Ok(Ok(()))) {
      let mut inner = merge_scheduler.shared.inner.lock();
      // `run_on_merge_finished` removes the current merge thread in its cleanup path before
      // returning an error. Only perform this cleanup here when the failure happened before
      // `run_on_merge_finished` was entered.
      if inner
        .merge_threads
        .iter()
        .any(|merge_thread| merge_thread.is_current_thread())
      {
        ConcurrentMergeScheduler::remove_merge_thread(&mut inner);
        merge_scheduler.update_merge_threads(&mut inner)?;
        merge_scheduler.shared.changed.notify_all();
      }
      drop(inner);
      if matches!(
        merge_result
          .as_ref()
          .map(|result| result.as_ref().map_err(LuceneError::kind)),
        Ok(Err(LuceneErrorKind::MergeAborted(_)))
      ) {
        // OK to ignore.
        Ok(())
      } else if !merge_scheduler.shared.inner.lock().suppress_exceptions {
        merge_scheduler.handle_merge_exception(merge_result)
      } else {
        Ok(())
      }
    } else {
      Ok(())
    }
  }

  fn start(self, merge_scheduler: ConcurrentMergeScheduler) -> Result<()> {
    let state = self.state.clone();
    let thread_name = self.state.name.clone();
    // WARNING: this must be set before spawning. After spawn succeeds, the caller immediately
    // runs update_merge_threads; if the new thread has not entered its closure yet, leaving this
    // false would let the scheduler prune a live merge thread.
    state.set_alive(true);
    match thread::Builder::new()
      .name(thread_name)
      .spawn(move || -> Result<()> { self.run(merge_scheduler) })
    {
      Ok(_handle) => Ok(()),
      Err(err) => {
        state.set_alive(false);
        Err(LuceneError::io(err))
      },
    }
  }
}

impl ConcurrentMergeScheduler {
  /** Called when a background merge thread returns an error. */
  fn handle_merge_exception(&self, result: CaughtResult) -> Result<()> {
    self.hook.handle_merge_exception(self, result)
  }

  /** Used for testing */
  #[cfg(test)]
  pub(crate) fn set_suppress_exceptions(&self) {
    self.shared.inner.lock().suppress_exceptions = true;
  }

  /** Used for testing */
  #[cfg(test)]
  pub(crate) fn clear_suppress_exceptions(&self) {
    self.shared.inner.lock().suppress_exceptions = false;
  }
}

impl ConcurrentMergeSchedulerDefaults {
  pub(crate) fn handle_merge_exception(
    _scheduler: &ConcurrentMergeScheduler,
    result: CaughtResult,
  ) -> Result<()> {
    let failure = result
      .caught_failure("panic in merge thread")
      .ok_or_else(|| LuceneError::illegal_argument("merge result must contain a failure"))?;
    let mut merge_error =
      crate::core::util::error::MergeError::new(format!("merge failed: {failure}"));
    merge_error.add_suppressed(failure);
    Err(LuceneErrorKind::Merge(merge_error).into())
  }
}

impl Display for ConcurrentMergeScheduler {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    let inner = self.shared.inner.lock();
    write!(
      f,
      "ConcurrentMergeScheduler: maxThreadCount={}, maxMergeCount={}, ioThrottle={}",
      inner.max_thread_count, inner.max_merge_count, inner.do_auto_io_throttle
    )
  }
}

impl ConcurrentMergeScheduler {
  fn is_backlog(inner: &Inner, now: Instant, merge_thread: &Arc<MergeThreadState>) -> bool {
    let merge_mb = Self::bytes_to_mb(merge_thread.estimated_merge_bytes.load(Ordering::SeqCst));
    for other in &inner.merge_threads {
      if other.is_alive()
        && !Arc::ptr_eq(other, merge_thread)
        && other.estimated_merge_bytes.load(Ordering::SeqCst) >= MIN_BIG_MERGE_MB * 1024 * 1024
        && now
          .saturating_duration_since(*other.merge_start_ns.lock())
          .as_secs_f64()
          > 3.0
      {
        let other_merge_mb = Self::bytes_to_mb(other.estimated_merge_bytes.load(Ordering::SeqCst));
        let ratio = other_merge_mb / merge_mb;
        if ratio > 0.3 && ratio < 3.0 {
          return true;
        }
      }
    }

    false
  }

  /** Tunes IO throttle when a new merge starts. */
  fn update_io_throttle(
    &self,
    inner: &mut Inner,
    new_merge_thread: &Arc<MergeThreadState>,
  ) -> Result<()> {
    if !inner.do_auto_io_throttle {
      return Ok(());
    }

    let merge_mb = Self::bytes_to_mb(
      new_merge_thread
        .estimated_merge_bytes
        .load(Ordering::SeqCst),
    );
    if merge_mb < MIN_BIG_MERGE_MB as f64 {
      // Only watch non-trivial merges for throttling; this is safe because the MP must eventually
      // have to do larger merges:
      return Ok(());
    }

    let now = Instant::now();

    // Simplistic closed-loop feedback control: if we find any other similarly
    // sized merges running, then we are falling behind, so we bump up the
    // IO throttle, else we lower it:
    let new_backlog = Self::is_backlog(inner, now, new_merge_thread);

    let mut cur_backlog = false;

    if !new_backlog {
      if inner.merge_threads.len() > inner.max_thread_count as usize {
        // If there are already more than the maximum merge threads allowed, count that as backlog:
        cur_backlog = true;
      } else {
        // Now see if any still-running merges are backlog'd:
        for merge_thread in &inner.merge_threads {
          if Self::is_backlog(inner, now, merge_thread) {
            cur_backlog = true;
            break;
          }
        }
      }
    }

    if new_backlog {
      // This new merge adds to the backlog: increase IO throttle by 20%
      inner.target_mb_per_sec *= 1.20;
      if inner.target_mb_per_sec > MAX_MERGE_MB_PER_SEC {
        inner.target_mb_per_sec = MAX_MERGE_MB_PER_SEC;
      }
    } else if cur_backlog {
      // We still have an existing backlog; leave the rate as is.
    } else {
      // We are not falling behind: decrease IO throttle by 10%
      inner.target_mb_per_sec /= 1.10;
      if inner.target_mb_per_sec < MIN_MERGE_MB_PER_SEC {
        inner.target_mb_per_sec = MIN_MERGE_MB_PER_SEC;
      }
    }

    let rate = if new_merge_thread.merge_stat.max_num_segments() != -1 {
      inner.force_merge_mb_per_sec
    } else {
      inner.target_mb_per_sec
    };
    new_merge_thread.rate_limiter.set_mb_per_sec(rate)?;
    self.target_mb_per_sec_changed();
    Ok(())
  }

  /** Subtype can override to tweak target_mb_per_sec. */
  fn target_mb_per_sec_changed(&self) {
    self.hook.target_mb_per_sec_changed(self)
  }

  fn bytes_to_mb(bytes: i64) -> f64 {
    bytes as f64 / 1024.0 / 1024.0
  }
}

impl ConcurrentMergeSchedulerDefaults {
  pub(crate) fn target_mb_per_sec_changed(_scheduler: &ConcurrentMergeScheduler) {}
}
