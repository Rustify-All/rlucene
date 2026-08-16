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
use std::backtrace::Backtrace;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::ErrorKind;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Once};

use crate::codec::bitvectors::hnsw_bit_vectors_format::HnswBitVectorsFormat;
use crate::core::analysis::analyzer::AnalyzerEnum;
use crate::core::codecs::knn_vectors_formats::KnnVectorsFormats;
use crate::core::codecs::lucene99::lucene99_hnsw_scalar_quantized_vectors_format::Lucene99HnswScalarQuantizedVectorsFormat;
use crate::core::codecs::lucene99::lucene99_hnsw_vectors_format::Lucene99HnswVectorsFormat;
use crate::core::document::field::{Field, FieldDataEnum, Store};
use crate::core::document::field_type::FieldType;
use crate::core::index::concurrent_merge_scheduler::{
  ConcurrentMergeScheduler, ConcurrentMergeSchedulerBase, ConcurrentMergeSchedulerHook, Inner,
};
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::{IndexReader, IndexReaderContextType};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::log_merge_policy::{LogMergePolicy, LogMergePolicyBase};
use crate::core::index::merge_policy::{MergePolicy, MergePolicyEnum};
use crate::core::index::merge_scheduler::MergeSource;
use crate::core::index::no_deletion_policy::NoDeletionPolicy;
use crate::core::index::serial_merge_scheduler::SerialMergeScheduler;
use crate::core::index::simple_merged_segment_warmer::SimpleMergedSegmentWarmer;
use crate::core::index::snapshot_deletion_policy::SnapshotDeletionPolicy;
use crate::core::index::tiered_merge_policy::TieredMergePolicy;
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::{BytesRef, CODEC_FILE_PATTERN, IndexFileNames};
#[cfg(test)]
use crate::core::search::index_searcher::IndexSearcherHook;
use crate::core::search::index_searcher::{DefaultIndexSearcher, IndexSearcher};
use crate::core::search::query::Query;
use crate::core::search::query_caching_policy::QueryCachingPolicy;
use crate::core::store::directory::{
  CoreDirEnum, DirEnum, Directory, DirectoryEnum2, DirectoryEnum3, MaybeNrtDirEnum, MockDirWrapper,
  RawDirEnum, SharedLockFactory,
};
use crate::core::store::file_switch_directory::FileSwitchDirectory;
use crate::core::store::flush_info::FlushInfo;
use crate::core::store::fs_lock_factory;
use crate::core::store::lock_factory::LockFactoryEnum;
use crate::core::store::merge_info::MergeInfo;
use crate::core::store::mmap_directory::MMapDirectory;
use crate::core::store::nio_fs_directory::NIOFSDirectory;
use crate::core::store::nrt_caching_directory::NRTCachingDirectory;
use crate::core::store::single_instance_lock_factory::SingleInstanceLockFactory;
use crate::core::store::{
  ByteBuffersDirectory, IO_CONTEXT_DEFAULT, IO_CONTEXT_READ_ONCE, IOContext,
};
use crate::core::util::SliceCopyOps;
use crate::core::util::access::SharedAccessVec;
use crate::core::util::close::CloseableRef;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::info_stream::InfoStreamEnum;
use crate::core::util::print_stream_info_stream::PrintStreamInfoStream;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::alcoholic_merge_policy::AlcoholicMergePolicy;
use crate::test_framework::core::index::mock_index_writer_event_listener::MockIndexWriterEventListener;
use crate::test_framework::core::index::mock_random_merge_policy::MockRandomMergePolicy;
#[cfg(test)]
use crate::test_framework::core::index::test_segment_to_thread_mapping::IntraSliceDocIdOrderWithPartitionsIndexSearcher;
use crate::test_framework::core::store::mock_directory_wrapper::{
  MockDirectoryWrapper, Throttling,
};
use crate::test_framework::core::store::raw_directory_wrapper::RawDirectoryWrapper;
use crate::test_framework::core::util::lucene_test_case::EnvConfig::{
  Multiplier, NightMode, TestSeed,
};
use crate::test_framework::core::util::test_util::TestUtil;
use chrono_tz::{TZ_VARIANTS, Tz, UTC};
use parking_lot::MutexGuard;
use rand::prelude::{SliceRandom, StdRng};
use rand::{Rng, RngExt, SeedableRng};
use tempfile::TempDir;

#[allow(dead_code)] // for quick search
pub struct LuceneTestCase;

thread_local! {
  static EXPECTED_PANIC_DEPTH: Cell<usize> = const { Cell::new(0) };
}

static INSTALL_EXPECTED_PANIC_HOOK: Once = Once::new();

struct ExpectedPanicGuard;

impl ExpectedPanicGuard {
  fn new() -> Self {
    INSTALL_EXPECTED_PANIC_HOOK.call_once(|| {
      let panic_hook = std::panic::take_hook();
      std::panic::set_hook(Box::new(move |panic_info| {
        if !EXPECTED_PANIC_DEPTH.with(|depth| depth.get() > 0) {
          panic_hook(panic_info);
        }
      }));
    });
    EXPECTED_PANIC_DEPTH.with(|depth| depth.set(depth.get() + 1));
    Self
  }
}

impl Drop for ExpectedPanicGuard {
  fn drop(&mut self) {
    EXPECTED_PANIC_DEPTH.with(|depth| depth.set(depth.get() - 1));
  }
}

/// Rust equivalent of `LuceneTestCase.expectThrows` for an expected Java
/// `Error`. The panic hook is suppressed only on the thread that is currently
/// checking the expected panic, since Java does not print expected throwables.
pub(crate) fn expect_panic<T>(f: impl FnOnce() -> T) {
  let guard = ExpectedPanicGuard::new();
  let result = catch_unwind(AssertUnwindSafe(f));
  drop(guard);
  assert!(result.is_err(), "Expected panic was not thrown");
}

pub fn random_vector_format<R>(
  random: &mut R,
  vector_encoding: &VectorEncoding,
) -> Result<KnnVectorsFormats>
where
  R: Rng + ?Sized,
{
  let mut available_formats = vec![
    Lucene99HnswVectorsFormat::new()?.into(),
    Lucene99HnswScalarQuantizedVectorsFormat::new()?.into(),
  ];
  if matches!(vector_encoding, VectorEncoding::BYTE(_)) {
    available_formats.push(HnswBitVectorsFormat::new()?.into());
  }
  Ok(available_formats.remove(random.random_range(0..available_formats.len())))
}

/// Describes the currently supported environment variables used to control
/// Lucene tests.
///
/// Each variant corresponds to an environment variable that configures specific
/// behaviors of the tests. For example, environment variables can be used to
/// control the test mode, random number generator seed, etc.
#[derive(Debug, Clone, Copy)]
pub enum EnvConfig {
  NightMode,
  Multiplier,
  TestSeed,
}

impl fmt::Display for EnvConfig {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    let key = match self {
      NightMode => "tests.nightly",
      Multiplier => "tests.multiplier",
      TestSeed => "tests.seed",
    };
    write!(f, "{}", key)
  }
}

pub const DEFAULT_LINE_DOCS_FILE: &str = "europarl.lines.txt.gz";

#[derive(Clone, Copy)]
pub(crate) enum DirectoryImpl {
  Random,
  NioFsDirectory,
  MMapDirectory,
  ByteBuffersDirectory,
}

pub(crate) const TEST_DIRECTORY: DirectoryImpl = DirectoryImpl::Random;

pub(crate) static TEST_THROTTLING: LazyLock<Throttling> = LazyLock::new(|| {
  if is_night_mode() {
    Throttling::Sometimes
  } else {
    Throttling::Never
  }
});

const FS_DIRECTORIES: [DirectoryImpl; 2] =
  [DirectoryImpl::NioFsDirectory, DirectoryImpl::MMapDirectory];

const CORE_DIRECTORIES: [DirectoryImpl; 3] = [
  FS_DIRECTORIES[0],
  FS_DIRECTORIES[1],
  DirectoryImpl::ByteBuffersDirectory,
];

pub fn is_night_mode() -> bool {
  std::env::var(NightMode.to_string()).is_ok_and(|v| v == "true")
}

pub(crate) fn random_multiplier() -> i32 {
  let multiplier = std::env::var(Multiplier.to_string()).ok();

  multiplier
    .and_then(|v| v.parse::<i32>().ok())
    .unwrap_or(default_random_multiplier())
}

fn default_random_multiplier() -> i32 {
  if is_night_mode() { 2 } else { 1 }
}

/// A [`QueryCachingPolicy`] that randomly caches.
pub(crate) struct MaybeCachePolicy {
  random: parking_lot::Mutex<StdRng>,
}

impl MaybeCachePolicy {
  pub(crate) fn new(random: StdRng) -> Self {
    Self {
      random: parking_lot::Mutex::new(random),
    }
  }
}

impl QueryCachingPolicy for MaybeCachePolicy {
  fn on_use(&self, _query: &Query) {}

  fn should_cache(&self, _query: &Query) -> Result<bool> {
    Ok(self.random.lock().random_bool(0.5))
  }
}

/// Retrieves the seed from the environment variable "tests.seed".
/// If the environment variable is not set or cannot be parsed as a `u64`,
/// it generates a random seed and logs the result.
///
/// # Returns
/// A valid `u64` seed.
pub(crate) fn get_seed_from_env() -> u64 {
  static GLOBAL_SEED: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

  fn current_seed() -> u64 {
    if let Some(seed) = GLOBAL_SEED.get() {
      *seed
    } else if let Ok(seed_str) = std::env::var(TestSeed.to_string()) {
      if let Ok(seed) = seed_str.parse::<u64>() {
        println!("Using Global Seed from environment: '{}'", seed);
        seed
      } else {
        println!("Environment variable tests.seed is invalid: '{}'", seed_str);
        let seed = rand::rng().random_range(0..u64::MAX);
        println!("Generated random seed: {}", seed);
        seed
      }
    } else {
      let seed = rand::rng().random_range(0..u64::MAX);
      println!("Generated random seed: {}", seed);
      seed
    }
  }
  current_seed()
}

pub(crate) fn random() -> StdRng {
  StdRng::seed_from_u64(get_seed_from_env())
}

pub(crate) fn random_from_seed(seed: u64) -> StdRng {
  StdRng::seed_from_u64(seed)
}

pub fn get_only_leaf_reader<IR>(
  reader: IR,
) -> Result<<IndexReaderContextType<IR> as IndexReaderContext>::LeafReader>
where
  IR: IndexReader,
  <IndexReaderContextType<IR> as IndexReaderContext>::LeafReader: Clone,
{
  let irc = reader.get_context()?;
  let sub_readers = irc.leaves()?;
  if sub_readers.len() != 1 {
    return Err(LuceneError::illegal_argument(format!(
      "{} has {} segments instead of exactly one",
      irc.reader(),
      sub_readers.len()
    )));
  }
  Ok(sub_readers[0].reader().clone())
}
pub(crate) fn at_least_usize<R>(random: &mut R, i: usize) -> usize
where
  R: Rng + ?Sized,
{
  debug_assert!(i <= i32::MAX as usize);
  at_least(random, i as i32) as usize
}
/// Returns a number of at least `i`
///
/// The actual number returned will be influenced by whether `TEST_NIGHTLY` is
/// active and `RANDOM_MULTIPLIER`, but also with some random fudge.
pub(crate) fn at_least<R>(random: &mut R, i: i32) -> i32
where
  R: Rng + ?Sized,
{
  let min = i * random_multiplier();
  let max = min + (min / 2);
  TestUtil::next_int(random, min, max)
}

pub(crate) fn rarely<R>(random: &mut R) -> bool
where
  R: Rng + ?Sized,
{
  let mut p = if is_night_mode() { 5 } else { 1 };
  p += (p as f64 * (random_multiplier() as f64).ln()).round() as i32;
  let min = 100 - p.min(20); // Never more than 20% chance
  random.random_range(0..100) >= min
}
pub(crate) fn usually<R>(random: &mut R) -> bool
where
  R: Rng + ?Sized,
{
  !rarely(random)
}

/// Creates a new index writer config with a snapshot deletion policy.
pub(crate) fn new_snapshot_index_writer_config<D, R>(random: &mut R) -> Result<IndexWriterConfig<D>>
where
  D: Directory,
  R: Rng + ?Sized,
{
  let mut config = new_index_writer_config(random)?;
  config.set_index_deletion_policy(SnapshotDeletionPolicy::new(NoDeletionPolicy));
  Ok(config)
}

/// Creates a new index writer config with random defaults.
pub(crate) fn new_index_writer_config<D, R>(random: &mut R) -> Result<IndexWriterConfig<D>>
where
  D: Directory,
  R: Rng + ?Sized,
{
  let mock = MockAnalyzer::new(random);
  new_index_writer_config_with_analyzer(random, mock)
}

/// Creates a new index writer config with random defaults using the specified random.
pub(crate) fn new_index_writer_config_with_analyzer<D, T, R>(
  random: &mut R,
  analyzer: T,
) -> Result<IndexWriterConfig<D>>
where
  D: Directory,
  R: Rng + ?Sized,
  T: Into<AnalyzerEnum>,
{
  let mut config = IndexWriterConfig::with_analyzer(analyzer)?;

  // The Rust test framework does not yet have the suite-level randomized
  // similarity supplied by TestRuleSetupAndRestoreClassEnv, so the config
  // retains the default similarity selected by IndexWriterConfig.
  if std::env::var("tests.verbose").is_ok_and(|value| value == "true") {
    // Even though TestRuleSetupAndRestoreClassEnv calls
    // InfoStream::set_default, we do it again here so that
    // the PrintStreamInfoStream message ID increments so
    // that when there are separate instances of
    // IndexWriter created we see "IW 0", "IW 1", "IW 2",
    // ... instead of just always "IW 0":
    config.set_info_stream(Arc::new(InfoStreamEnum::from(
      PrintStreamInfoStream::stdout(),
    )));
  }

  if rarely(random) {
    config.set_merge_scheduler(SerialMergeScheduler::new());
  } else if rarely(random) {
    let concurrent_merge_scheduler = if random.random_bool(0.5) {
      // Java's TestConcurrentMergeScheduler only overrides the unsupported
      // intra-merge executor, so it maps to the default Rust scheduler.
      ConcurrentMergeScheduler::new()
    } else {
      ConcurrentMergeScheduler::with_hook(
        ConcurrentMergeSchedulerHook::LuceneTestCaseAlwaysProceed(
          AlwaysProceedConcurrentMergeScheduler,
        ),
      )
    };
    let max_thread_count = TestUtil::next_int(random, 1, 4);
    let max_merge_count = TestUtil::next_int(random, max_thread_count, max_thread_count + 4);
    concurrent_merge_scheduler.set_max_merges_and_threads(max_merge_count, max_thread_count)?;
    if random.random_bool(0.5) {
      concurrent_merge_scheduler.disable_auto_io_throttle()?;
      assert!(!concurrent_merge_scheduler.get_auto_io_throttle());
    }
    concurrent_merge_scheduler.set_force_merge_mb_per_sec(10.0 + 10.0 * random.random::<f64>())?;
    config.set_merge_scheduler(concurrent_merge_scheduler);
  } else {
    // Always use consistent settings, else CMS's dynamic (SSD or not)
    // defaults can change, hurting reproducibility. Java randomly chooses
    // TestConcurrentMergeScheduler here, but its only override is the unsupported
    // intra-merge executor, so both choices map to the default Rust scheduler.
    let _ = random.random_bool(0.5);
    let concurrent_merge_scheduler = ConcurrentMergeScheduler::new();

    // Only 1 thread can run at once (should maybe help reproducibility),
    // with up to 3 pending merges before segment-producing threads are
    // stalled:
    concurrent_merge_scheduler.set_max_merges_and_threads(3, 1)?;
    config.set_merge_scheduler(concurrent_merge_scheduler);
  }

  if random.random_bool(0.5) {
    if rarely(random) {
      // crazy value
      config.set_max_buffered_docs(TestUtil::next_int(random, 2, 15));
    } else {
      // reasonable value
      config.set_max_buffered_docs(TestUtil::next_int(random, 16, 1000));
    }
  }

  config.set_merge_policy(new_merge_policy(random)?);

  if rarely(random) {
    config.set_merged_segment_warmer(Some(
      SimpleMergedSegmentWarmer::new(config.get_info_stream()).into(),
    ));
  }
  config.set_use_compound_file(random.random_bool(0.5));
  config.set_reader_pooling(random.random_bool(0.5));
  if rarely(random) {
    config.set_check_pending_flush_update(false);
  }

  if rarely(random) {
    config.set_index_writer_event_listener(MockIndexWriterEventListener::new());
  }
  match random.random_range(0..3) {
    0 => {
      // Disable merge on refresh
      config.set_max_full_flush_merge_wait_millis(0);
    },
    1 => {
      // Very low timeout, merges will likely not be able to run in time
      config.set_max_full_flush_merge_wait_millis(1);
    },
    _ => {
      // Very long timeout, merges will almost always be able to run in time
      config.set_max_full_flush_merge_wait_millis(1000);
    },
  }

  let max_full_flush_merge_wait_millis = if rarely(random) {
    at_least(random, 1000)
  } else {
    at_least(random, 200)
  };
  config.set_max_full_flush_merge_wait_millis(max_full_flush_merge_wait_millis as i64);
  Ok(config)
}

#[derive(Clone)]
pub(crate) struct AlwaysProceedConcurrentMergeScheduler;

impl ConcurrentMergeSchedulerBase for AlwaysProceedConcurrentMergeScheduler {
  fn maybe_stall<MS, D>(
    &self,
    _scheduler: &ConcurrentMergeScheduler,
    _inner: &mut MutexGuard<'_, Inner>,
    _merge_source: &MS,
  ) -> Result<bool>
  where
    MS: MergeSource<D>,
    D: Directory,
  {
    Ok(true)
  }
}

pub fn new_merge_policy<D, R>(r: &mut R) -> Result<MergePolicyEnum<D>>
where
  D: Directory,
  R: Rng + ?Sized,
{
  new_merge_policy_with_mock_mp(r, true)
}
pub fn new_merge_policy_with_mock_mp<D, R>(
  r: &mut R,
  include_mock_mp: bool,
) -> Result<MergePolicyEnum<D>>
where
  D: Directory,
  R: Rng + ?Sized,
{
  if include_mock_mp && rarely(r) {
    Ok(MockRandomMergePolicy::new(r).into())
  } else if r.random_bool(0.5) {
    Ok(new_tiered_merge_policy(r)?.into())
  } else if rarely(r) {
    let time_zone = match std::env::var("tests.timezone") {
      Ok(time_zone) if time_zone != "random" => time_zone.parse::<Tz>().unwrap_or(UTC),
      _ => TZ_VARIANTS[r.random_range(0..TZ_VARIANTS.len())],
    };
    Ok(new_alcoholic_merge_policy(r, time_zone).into())
  } else {
    new_log_merge_policy(r)
  }
}

pub fn new_alcoholic_merge_policy<R>(
  r: &mut R,
  time_zone: Tz,
) -> LogMergePolicy<AlcoholicMergePolicy>
where
  R: Rng + ?Sized,
{
  AlcoholicMergePolicy::new(time_zone, StdRng::seed_from_u64(r.random()))
}

pub fn new_log_merge_policy<D, R>(r: &mut R) -> Result<MergePolicyEnum<D>>
where
  D: Directory,
  R: Rng + ?Sized,
{
  let logmp = if r.random_bool(0.5) {
    let mut v = LogMergePolicy::log_doc();
    set_meta::<D, R>(r, &mut v)?;
    v.into()
  } else {
    let mut v = LogMergePolicy::log_bytes_size();
    set_meta::<D, R>(r, &mut v)?;
    v.into()
  };

  Ok(logmp)
}
fn set_meta<D, R>(r: &mut R, mp: &mut LogMergePolicy<impl LogMergePolicyBase>) -> Result<()>
where
  D: Directory,
  R: Rng + ?Sized,
{
  mp.set_calibrate_size_by_deletes(r.random_bool(0.5));
  mp.set_target_search_concurrency(TestUtil::next_int(r, 1, 16))?;

  if rarely(r) {
    mp.set_merge_factor(TestUtil::next_usize(r, 2, 9))?;
  } else {
    mp.set_merge_factor(TestUtil::next_usize(r, 10, 50))?;
  }

  configure_random::<D, R, _>(r, mp)
}
fn configure_random<D, R, MP>(r: &mut R, merge_policy: &mut MP) -> Result<()>
where
  D: Directory,
  R: Rng + ?Sized,
  MP: MergePolicy<D>,
{
  if r.random_bool(0.5) {
    merge_policy
      .get_base_mut()
      .set_no_cfs_ratio(0.1 + r.random::<f64>() * 0.8)?;
  } else {
    merge_policy
      .get_base_mut()
      .set_no_cfs_ratio(if r.random_bool(0.5) { 1.0 } else { 0.0 })?;
  }

  if rarely(r) {
    merge_policy
      .get_base_mut()
      .set_max_cfs_segment_size_mb(0.2 + r.random::<f64>() * 2.0)?;
  } else {
    merge_policy
      .get_base_mut()
      .set_max_cfs_segment_size_mb(f64::INFINITY)?;
  }

  Ok(())
}

pub fn new_tiered_merge_policy<R>(r: &mut R) -> Result<TieredMergePolicy>
where
  R: Rng + ?Sized,
{
  let mut tmp = TieredMergePolicy::new();
  if rarely(r) {
    tmp.set_max_merge_at_once(TestUtil::next_int(r, 2, 9))?;
  } else {
    tmp.set_max_merge_at_once(TestUtil::next_int(r, 10, 50))?;
  }
  if rarely(r) {
    tmp.set_max_merged_segment_mb(0.2 + r.random::<f64>() * 2.0)?;
  } else {
    tmp.set_max_merged_segment_mb(10.0 + r.random::<f64>() * 100.0)?;
  }
  tmp.set_floor_segment_mb(0.2 + r.random::<f64>() * 2.0)?;
  tmp.set_force_merge_deletes_pct_allowed(r.random::<f64>() * 30.0)?;
  if rarely(r) {
    tmp.set_segments_per_tier(TestUtil::next_int(r, 2, 20) as f64)?;
  } else {
    tmp.set_segments_per_tier(TestUtil::next_int(r, 10, 50) as f64)?;
  }
  if rarely(r) {
    tmp.set_target_search_concurrency(TestUtil::next_int(r, 10, 50))?;
  } else {
    tmp.set_target_search_concurrency(TestUtil::next_int(r, 2, 20))?;
  }

  configure_random::<DirEnum, R, _>(r, &mut tmp)?;
  tmp.set_deletes_pct_allowed(20.0 + r.random::<f64>() * 30.0)?;
  Ok(tmp)
}

pub fn new_log_merge_policy_with_cfs<D, R>(r: &mut R, use_cfs: bool) -> Result<MergePolicyEnum<D>>
where
  D: Directory,
  R: Rng + ?Sized,
{
  let lomp = new_log_merge_policy::<D, R>(r)?;
  let ratio = if use_cfs { 1.0 } else { 0.0 };
  match lomp {
    MergePolicyEnum::LogDoc(mut log_doc) => {
      MergePolicy::<D>::get_base_mut(&mut log_doc).set_no_cfs_ratio(ratio)?;
      Ok(log_doc.into())
    },
    MergePolicyEnum::LogBytesSize(mut log_bytes_size) => {
      MergePolicy::<D>::get_base_mut(&mut log_bytes_size).set_no_cfs_ratio(ratio)?;
      Ok(log_bytes_size.into())
    },
    _ => Err(LuceneError::illegal_argument(
      "Expected a LogMergePolicyEnum variant",
    )),
  }
}

pub fn new_log_merge_policy_with_merge_factor_cfs<D, R>(
  r: &mut R,
  use_cfs: bool,
  merge_factor: i32,
) -> Result<MergePolicyEnum<D>>
where
  D: Directory,
  R: Rng + ?Sized,
{
  let lomp = new_log_merge_policy::<D, R>(r)?;
  let ratio = if use_cfs { 1.0 } else { 0.0 };
  let merge_factor = usize::try_from(merge_factor)
    .map_err(|_| LuceneError::illegal_argument("mergeFactor cannot be less than 2"))?;
  match lomp {
    MergePolicyEnum::LogDoc(mut log_doc) => {
      MergePolicy::<D>::get_base_mut(&mut log_doc).set_no_cfs_ratio(ratio)?;
      log_doc.set_merge_factor(merge_factor)?;
      Ok(log_doc.into())
    },
    MergePolicyEnum::LogBytesSize(mut log_bytes_size) => {
      MergePolicy::<D>::get_base_mut(&mut log_bytes_size).set_no_cfs_ratio(ratio)?;
      log_bytes_size.set_merge_factor(merge_factor)?;
      Ok(log_bytes_size.into())
    },
    _ => Err(LuceneError::illegal_argument(
      "Expected a LogMergePolicyEnum variant",
    )),
  }
}

pub fn new_log_merge_policy_with_merge_factor<D, R>(
  r: &mut R,
  merge_factor: i32,
) -> Result<MergePolicyEnum<D>>
where
  D: Directory,
  R: Rng + ?Sized,
{
  let lomp = new_log_merge_policy::<D, R>(r)?;
  let merge_factor = usize::try_from(merge_factor)
    .map_err(|_| LuceneError::illegal_argument("mergeFactor cannot be less than 2"))?;
  match lomp {
    MergePolicyEnum::LogDoc(mut log_doc) => {
      log_doc.set_merge_factor(merge_factor)?;
      Ok(log_doc.into())
    },
    MergePolicyEnum::LogBytesSize(mut log_bytes_size) => {
      log_bytes_size.set_merge_factor(merge_factor)?;
      Ok(log_bytes_size.into())
    },
    _ => Err(LuceneError::illegal_argument(
      "Expected a LogMergePolicyEnum variant",
    )),
  }
}

pub(crate) fn maybe_change_live_index_writer_config<R, C>(
  _random: &mut R,
  _config: &mut C,
) -> Result<()>
where
  R: Rng + ?Sized,
  C: LiveIndexWriterConfig + ?Sized,
{
  Ok(())
}

pub(crate) fn new_directory_shared<R>(random: &mut R) -> Result<Arc<DirEnum>>
where
  R: Rng + ?Sized,
{
  let dir = new_directory(random)?;
  Ok(Arc::new(dir))
}

pub(crate) fn new_maybe_virus_checking_directory<R>(random: &mut R) -> Result<Arc<DirEnum>>
where
  R: Rng + ?Sized,
{
  if random.random_range(0..5) == 4 {
    // TODO IMPORTANT VirusCheckingFS is not implemented yet, so use the same randomized FS directory without
    // wrapping its temporary path in a virus-checking filesystem.
    new_fs_directory(random, create_temp_dir()?)
  } else {
    new_directory_shared(random)
  }
}

pub(crate) fn new_directory<R>(random: &mut R) -> Result<DirEnum>
where
  R: Rng + ?Sized,
{
  let directory = new_directory_impl(random, TEST_DIRECTORY)?;
  let bare = rarely(random);
  Ok(wrap_directory(random, directory, bare, false))
}

pub(crate) fn new_directory_with_lock_factory<R, T>(
  random: &mut R,
  lock_factory: T,
) -> Result<DirEnum>
where
  R: Rng + ?Sized,
  T: Into<LockFactoryEnum>,
{
  let directory =
    new_directory_impl_with_lock_factory(random, TEST_DIRECTORY, Arc::new(lock_factory.into()))?;
  let bare = rarely(random);
  Ok(wrap_directory(random, directory, bare, false))
}

pub(crate) fn new_mock_directory<R>(random: &mut R) -> Result<MockDirWrapper>
where
  R: Rng + ?Sized,
{
  let directory = new_directory_impl(random, TEST_DIRECTORY)?;
  match wrap_directory(random, directory, false, false) {
    DirEnum::B(directory) => Ok(directory),
    _ => unreachable!("bare=false must create a MockDirectoryWrapper"),
  }
}

pub(crate) fn new_mock_directory_with_lock_factory<R, T>(
  random: &mut R,
  lock_factory: T,
) -> Result<MockDirWrapper>
where
  R: Rng + ?Sized,
  T: Into<LockFactoryEnum>,
{
  let directory =
    new_directory_impl_with_lock_factory(random, TEST_DIRECTORY, Arc::new(lock_factory.into()))?;
  match wrap_directory(random, directory, false, false) {
    DirEnum::B(directory) => Ok(directory),
    _ => unreachable!("bare=false must create a MockDirectoryWrapper"),
  }
}

pub(crate) fn new_mock_fs_directory<R>(random: &mut R, temp_dir: TempDir) -> Result<MockDirWrapper>
where
  R: Rng + ?Sized,
{
  new_mock_fs_directory_with_lock_factory(random, temp_dir, fs_lock_factory::get_default())
}

pub(crate) fn new_mock_fs_directory_with_lock_factory<R, T>(
  random: &mut R,
  temp_dir: TempDir,
  lock_factory: T,
) -> Result<MockDirWrapper>
where
  R: Rng + ?Sized,
  T: Into<LockFactoryEnum>,
{
  match new_fs_directory_with_lock_factory_and_bare(random, temp_dir.keep(), lock_factory, false)? {
    DirEnum::B(directory) => Ok(directory),
    _ => unreachable!("bare=false must create a MockDirectoryWrapper"),
  }
}

pub(crate) fn new_directory_from<R, D>(random: &mut R, source: &D) -> Result<DirEnum>
where
  R: Rng + ?Sized,
  D: Directory + ?Sized,
{
  let directory = new_directory_impl(random, TEST_DIRECTORY)?;
  for file in source.list_all()? {
    if file.starts_with(IndexFileNames::SEGMENTS) || CODEC_FILE_PATTERN.is_match(&file) {
      directory.copy_from(source, &file, &file, &new_io_context(random)?)?;
    }
  }
  let bare = rarely(random);
  Ok(wrap_directory(random, directory, bare, false))
}

pub(crate) fn new_fs_directory<R>(random: &mut R, temp_dir: TempDir) -> Result<Arc<DirEnum>>
where
  R: Rng + ?Sized,
{
  new_fs_directory_from_path(random, temp_dir.keep())
}

pub(crate) fn new_fs_directory_from_path<R>(random: &mut R, path: PathBuf) -> Result<Arc<DirEnum>>
where
  R: Rng + ?Sized,
{
  let bare = rarely(random);
  Ok(Arc::new(new_fs_directory_with_lock_factory_and_bare(
    random,
    path,
    fs_lock_factory::get_default(),
    bare,
  )?))
}

pub(crate) fn new_fs_directory_with_lock_factory<R, T>(
  random: &mut R,
  path: PathBuf,
  lock_factory: T,
) -> Result<DirEnum>
where
  R: Rng + ?Sized,
  T: Into<LockFactoryEnum>,
{
  let bare = rarely(random);
  new_fs_directory_with_lock_factory_and_bare(random, path, lock_factory, bare)
}

fn new_fs_directory_with_lock_factory_and_bare<R, T>(
  random: &mut R,
  path: PathBuf,
  lock_factory: T,
  bare: bool,
) -> Result<DirEnum>
where
  R: Rng + ?Sized,
  T: Into<LockFactoryEnum>,
{
  let directory_impl = match TEST_DIRECTORY {
    DirectoryImpl::NioFsDirectory | DirectoryImpl::MMapDirectory => TEST_DIRECTORY,
    DirectoryImpl::Random | DirectoryImpl::ByteBuffersDirectory => {
      FS_DIRECTORIES[random.random_range(0..FS_DIRECTORIES.len())]
    },
  };

  let directory = new_fs_directory_impl(directory_impl, path, Arc::new(lock_factory.into()))?;
  Ok(wrap_directory(random, directory, bare, true))
}

fn new_file_switch_directory<R>(
  random: &mut R,
  dir1: CoreDirEnum,
  dir2: CoreDirEnum,
) -> Result<RawDirEnum>
where
  R: Rng + ?Sized,
{
  let mut file_extensions = vec![
    "fdt", "fdx", "tim", "tip", "si", "fnm", "pos", "dii", "dim", "nvm", "nvd", "dvm", "dvd",
  ];
  file_extensions.shuffle(random);
  let length = random.random_range(1..=file_extensions.len());
  let primary_extensions = file_extensions[..length]
    .iter()
    .map(|extension| (*extension).to_string())
    .collect::<HashSet<_>>();
  Ok(RawDirEnum::FileSwitch(FileSwitchDirectory::new(
    primary_extensions,
    dir1,
    dir2,
    true,
  )?))
}

fn wrap_directory<R>(random: &mut R, directory: RawDirEnum, bare: bool, filesystem: bool) -> DirEnum
where
  R: Rng + ?Sized,
{
  // IOContext randomization might make NRTCachingDirectory make bad decisions, so avoid
  // using it if the user requested a filesystem directory.
  let directory: MaybeNrtDirEnum = if rarely(random) && !bare && !filesystem {
    DirectoryEnum2::B(NRTCachingDirectory::new(
      directory,
      random.random::<f64>(),
      random.random::<f64>(),
    ))
  } else {
    DirectoryEnum2::A(directory)
  };

  // The Rust test framework does not yet have a suite-level close registry equivalent to
  // closeAfterSuite; directory owners remain responsible for explicitly closing the wrapper.
  if bare {
    DirEnum::A(Box::new(RawDirectoryWrapper::new(random, directory)))
  } else {
    let mock = MockDirectoryWrapper::new(random, directory);
    mock.set_throttling(*TEST_THROTTLING);
    DirEnum::B(MockDirWrapper::from_inner(mock))
  }
}

pub(crate) fn new_string_field<S1, S2, R>(
  random: &mut R,
  name: S1,
  value: S2,
  stored: Store,
  field_to_type: &mut HashMap<String, FieldType>,
) -> Result<Field>
where
  R: Rng + ?Sized,
  S1: Into<String>,
  S2: Into<String>,
{
  let field_type = match stored {
    Store::Yes => crate::core::document::string_field::TYPE_STORED.clone(),
    Store::No => crate::core::document::string_field::TYPE_NOT_STORED.clone(),
  };

  new_field_with_random(
    random,
    name.into(),
    FieldDataEnum::String(value.into()),
    &field_type,
    field_to_type,
  )
}

pub(crate) fn new_string_field_binary<S, R>(
  random: &mut R,
  name: S,
  value: BytesRef<Vec<u8>>,
  stored: Store,
  field_to_type: &mut HashMap<String, FieldType>,
) -> Result<Field>
where
  R: Rng + ?Sized,
  S: Into<String>,
{
  let field_type = match stored {
    Store::Yes => crate::core::document::string_field::TYPE_STORED.clone(),
    Store::No => crate::core::document::string_field::TYPE_NOT_STORED.clone(),
  };

  new_field_with_random(
    random,
    name.into(),
    value.into(),
    &field_type,
    field_to_type,
  )
}
pub(crate) fn new_text_field<S1, S2, R>(
  random: &mut R,
  name: S1,
  value: S2,
  stored: Store,
  field_to_type: &mut HashMap<String, FieldType>,
) -> Result<Field>
where
  R: Rng + ?Sized,
  S1: Into<String>,
  S2: Into<String>,
{
  let field_type = match stored {
    Store::Yes => crate::core::document::text_field::TYPE_STORED.clone(),
    Store::No => crate::core::document::text_field::TYPE_NOT_STORED.clone(),
  };

  new_field_with_random(
    random,
    name,
    FieldDataEnum::String(value.into()),
    &field_type,
    field_to_type,
  )
}
pub(crate) fn new_string_field_string_with_random<S1, S2, R>(
  random: &mut R,
  name: S1,
  value: S2,
  stored: Store,
  field_to_type: &mut HashMap<String, FieldType>,
) -> Result<Field>
where
  R: Rng + ?Sized,
  S1: Into<String>,
  S2: Into<String>,
{
  let field_type = match stored {
    Store::Yes => crate::core::document::string_field::TYPE_STORED.clone(),
    Store::No => crate::core::document::string_field::TYPE_NOT_STORED.clone(),
  };

  new_field_with_random(
    random,
    name,
    FieldDataEnum::String(value.into()),
    &field_type,
    field_to_type,
  )
}
pub(crate) fn new_string_field_binary_with_random<S, R>(
  random: &mut R,
  name: S,
  value: BytesRef<Vec<u8>>,
  stored: Store,
  field_to_type: &mut HashMap<String, FieldType>,
) -> Result<Field>
where
  R: Rng + ?Sized,
  S: Into<String>,
{
  let field_type = match stored {
    Store::Yes => crate::core::document::string_field::TYPE_STORED.clone(),
    Store::No => crate::core::document::string_field::TYPE_NOT_STORED.clone(),
  };
  new_field_with_random(
    random,
    name,
    FieldDataEnum::Binary(value),
    &field_type,
    field_to_type,
  )
}

pub(crate) fn new_field<S, V, R>(
  random: &mut R,
  name: S,
  value: V,
  field_type: &FieldType,
  field_to_type: &mut HashMap<String, FieldType>,
) -> Result<Field>
where
  R: Rng + ?Sized,
  S: Into<String>,
  V: Into<FieldDataEnum>,
{
  new_field_with_random(random, name, value.into(), field_type, field_to_type)
}
// TODO: if we can pull out the "make term vector options
// consistent across all instances of the same field name"
// write-once schema helper type, then we can
// remove the sync here.  We can also fold the random
// "enable norms" (now commented out, below) into that:
pub(crate) fn new_field_with_random<S, R>(
  random: &mut R,
  name: S,
  value: FieldDataEnum,
  field_type: &FieldType,
  field_to_type: &mut HashMap<String, FieldType>,
) -> Result<Field>
where
  R: Rng + ?Sized,
  S: Into<String>,
{
  let name = name.into();

  let map = field_to_type;
  if let Some(prev_type) = map.get(&name) {
    return create_field(&name, value, prev_type.clone());
  }
  // TODO: once all core & test codecs can index
  // offsets, sometimes randomly turn on offsets if we are
  // already indexing positions...
  let mut new_type = FieldType::from_ref(field_type)?;
  if !new_type.stored() && random.random_bool(0.5) {
    new_type.set_stored(true)?; // randomly store it
  }

  if *new_type.index_options() != IndexOptions::None
    && !new_type.store_term_vectors()
    && random.random_bool(0.5)
  {
    new_type.set_store_term_vectors(true)?;

    if !new_type.store_term_vector_positions() && random.random_bool(0.5) {
      new_type.set_store_term_vector_positions(true)?;

      if !new_type.store_term_vector_payloads() {
        new_type.set_store_term_vector_payloads(random.random_bool(0.5))?;
      }
    }

    // Check for strings as offsets are disallowed on binary fields
    if matches!(value, FieldDataEnum::String(_)) && !new_type.store_term_vector_offsets() {
      new_type.set_store_term_vector_offsets(random.random_bool(0.5))?;
    }

    if cfg!(feature = "test_log_verbose") {
      println!(
        "NOTE: LuceneTestCase: upgrade name={} type={:?}",
        name, new_type
      );
    }
  }
  new_type.freeze();
  map.insert(name.clone(), new_type.clone());
  create_field(&name, value, new_type)
}
pub(crate) fn create_field(
  name: &str,
  value: FieldDataEnum,
  field_type: FieldType,
) -> Result<Field> {
  match value {
    FieldDataEnum::String(_) => Ok(Field::new(name, value, field_type)),
    FieldDataEnum::Binary(_) => Ok(Field::new(name, value, field_type)),
    _ => Err(LuceneError::illegal_argument(
      "Unsupported FieldDataEnum variant",
    )),
  }
}

fn new_fs_directory_impl(
  directory_impl: DirectoryImpl,
  path: PathBuf,
  lock_factory: SharedLockFactory,
) -> Result<RawDirEnum> {
  match directory_impl {
    DirectoryImpl::NioFsDirectory => Ok(RawDirEnum::Nio(NIOFSDirectory::with_lock_factory(
      path,
      lock_factory,
    )?)),
    DirectoryImpl::MMapDirectory => Ok(RawDirEnum::MMap(MMapDirectory::with_lock_factory(
      path,
      lock_factory,
    )?)),
    DirectoryImpl::Random | DirectoryImpl::ByteBuffersDirectory => {
      unreachable!("FS directory implementation must be resolved")
    },
  }
}

pub(crate) fn new_directory_impl<R>(
  random: &mut R,
  directory_impl: DirectoryImpl,
) -> Result<RawDirEnum>
where
  R: Rng + ?Sized,
{
  new_directory_impl_with_lock_factory(
    random,
    directory_impl,
    Arc::new(fs_lock_factory::get_default().into()),
  )
}

pub(crate) fn new_directory_impl_with_lock_factory<R>(
  random: &mut R,
  mut directory_impl: DirectoryImpl,
  lf: SharedLockFactory,
) -> Result<RawDirEnum>
where
  R: Rng + ?Sized,
{
  if matches!(directory_impl, DirectoryImpl::Random) {
    if rarely(random) {
      directory_impl = CORE_DIRECTORIES[random.random_range(0..CORE_DIRECTORIES.len())];
    } else if rarely(random) {
      let directory_impl1 = if rarely(random) {
        CORE_DIRECTORIES[random.random_range(0..CORE_DIRECTORIES.len())]
      } else {
        DirectoryImpl::ByteBuffersDirectory
      };
      let directory_impl2 = if rarely(random) {
        CORE_DIRECTORIES[random.random_range(0..CORE_DIRECTORIES.len())]
      } else {
        DirectoryImpl::ByteBuffersDirectory
      };
      let dir1 = match new_directory_impl_with_lock_factory(random, directory_impl1, lf.clone())? {
        RawDirEnum::Nio(directory) => DirectoryEnum3::A(directory),
        RawDirEnum::MMap(directory) => DirectoryEnum3::B(directory),
        RawDirEnum::ByteBuffers(directory) => DirectoryEnum3::C(directory),
        RawDirEnum::FileSwitch(_) => {
          unreachable!("CORE_DIRECTORIES must not create a FileSwitchDirectory")
        },
      };
      let dir2 = match new_directory_impl_with_lock_factory(random, directory_impl2, lf)? {
        RawDirEnum::Nio(directory) => DirectoryEnum3::A(directory),
        RawDirEnum::MMap(directory) => DirectoryEnum3::B(directory),
        RawDirEnum::ByteBuffers(directory) => DirectoryEnum3::C(directory),
        RawDirEnum::FileSwitch(_) => {
          unreachable!("CORE_DIRECTORIES must not create a FileSwitchDirectory")
        },
      };
      return new_file_switch_directory(random, dir1, dir2);
    } else {
      directory_impl = DirectoryImpl::ByteBuffersDirectory;
    }
  }

  match directory_impl {
    DirectoryImpl::Random => unreachable!("random directory implementation must be resolved"),
    DirectoryImpl::NioFsDirectory | DirectoryImpl::MMapDirectory => {
      let prefix = match directory_impl {
        DirectoryImpl::NioFsDirectory => "index-NIOFSDirectory",
        DirectoryImpl::MMapDirectory => "index-MMapDirectory",
        DirectoryImpl::Random | DirectoryImpl::ByteBuffersDirectory => unreachable!(),
      };
      let dir = create_temp_dir_with_prefix(prefix)?;
      new_fs_directory_impl(directory_impl, dir.keep(), lf)
    },
    DirectoryImpl::ByteBuffersDirectory => {
      let lock_factory = if matches!(
        lf.as_ref(),
        LockFactoryEnum::Simple(_) | LockFactoryEnum::Native(_)
      ) {
        Arc::new(SingleInstanceLockFactory::new().into())
      } else {
        lf
      };
      Ok(RawDirEnum::ByteBuffers(
        ByteBuffersDirectory::with_lock_factory(lock_factory),
      ))
    },
  }
}

pub(crate) fn new_io_context<R>(random: &mut R) -> Result<IOContext>
where
  R: Rng + ?Sized,
{
  new_io_context_with_default(random, &IO_CONTEXT_DEFAULT)
}

pub(crate) fn new_io_context_with_default<R>(
  random: &mut R,
  old_context: &IOContext,
) -> Result<IOContext>
where
  R: Rng + ?Sized,
{
  if *old_context == *IO_CONTEXT_READ_ONCE {
    // Don't modify the READONCE SINGLETON
    return Ok(old_context.clone());
  }

  // Generate random parameters
  let random_num_docs: i32 = random.random_range(0..4192);
  let size = random.random_range(0..512) * random_num_docs as i64;

  if let Some(flush_info) = &old_context.flush_info {
    // Always return at least the estimatedSegmentSize of the incoming
    // IOContext
    Ok(IOContext::with_flush(FlushInfo::new(
      random_num_docs,
      size.max(flush_info.get_estimated_segment_size()),
    ))?)
  } else if let Some(merge_info) = &old_context.merge_info {
    // Always return at least the estimatedMergeBytes of the incoming
    // IOContext
    IOContext::with_merge(MergeInfo::new(
      random_num_docs,
      size.max(merge_info.get_estimated_merge_bytes()),
      random.random_bool(0.5), /* Randomly decide if it's an external
                                * merge  */
      random.random_range(1..=100),
    ))
  } else {
    // Make a totally random IOContext, except READONCE which has semantic
    // implications
    let context_type = random.random_range(0..3);
    match context_type {
      0 => Ok(IOContext::default_io_context()?),
      1 => Ok(IOContext::with_merge(MergeInfo::new(
        random_num_docs,
        size,
        true,
        -1,
      ))?),
      2 => Ok(IOContext::with_flush(FlushInfo::new(
        random_num_docs,
        size,
      ))?),
      _ => Ok(IOContext::default_io_context()?),
    }
  }
}
pub fn new_searcher_with_reader<IR>(
  reader: IR,
) -> Result<DefaultIndexSearcher<IndexReaderContextType<IR>>>
where
  IR: IndexReader,
{
  let irc = reader.get_context()?;
  IndexSearcher::new(irc)
}

/// Create a new searcher over the reader. This searcher might randomly use threads.
pub fn new_searcher<IR, R>(
  random: &mut R,
  reader: IR,
) -> Result<DefaultIndexSearcher<IndexReaderContextType<IR>>>
where
  IR: IndexReader,
  R: Rng + ?Sized,
{
  new_searcher_with_wrap(random, reader, true)
}

/// Create a new searcher over the reader. This searcher might randomly use threads.
pub fn new_searcher_with_wrap<IR, R>(
  random: &mut R,
  reader: IR,
  may_be_wrap: bool,
) -> Result<DefaultIndexSearcher<IndexReaderContextType<IR>>>
where
  IR: IndexReader,
  R: Rng + ?Sized,
{
  new_searcher_with_wrap_assert(random, reader, may_be_wrap, true)
}

pub fn new_searcher_with_wrap_assert<IR, R>(
  random: &mut R,
  reader: IR,
  may_be_wrap: bool,
  wrap_with_assertions: bool,
) -> Result<DefaultIndexSearcher<IndexReaderContextType<IR>>>
where
  IR: IndexReader,
  R: Rng + ?Sized,
{
  let threads = random.random_bool(0.5);
  new_searcher_with_threads(random, reader, may_be_wrap, wrap_with_assertions, threads)
}
pub fn new_searcher_with_threads<R, IR>(
  random: &mut R,
  reader: IR,
  _may_be_wrap: bool,
  _wrap_with_assertions: bool,
  use_threads: bool,
) -> Result<DefaultIndexSearcher<IndexReaderContextType<IR>>>
where
  IR: IndexReader,
  R: Rng + ?Sized,
{
  let irc = reader.get_context()?;
  if use_threads {
    let threads = random.random_range(2..=5);
    IndexSearcher::with_threads(irc, threads)
  } else {
    IndexSearcher::new(irc)
  }
}

/// What level of concurrency is supported by the searcher being created
pub enum Concurrency {
  /// No concurrency, meaning an executor won't be provided to the searcher
  None,
  /// Inter-segment concurrency, meaning an executor will be provided to the searcher and slices will be randomly created to concurrently search entire segments
  InterSegment,
  /// Intra-segment concurrency, meaning an executor will be provided to the searcher and slices will be randomly created to concurrently search segment partitions
  IntraSegment,
}

#[cfg(test)]
pub fn new_searcher_with_concurrency<IR, R>(
  random: &mut R,
  reader: IR,
  concurrency: Concurrency,
) -> Result<DefaultIndexSearcher<IndexReaderContextType<IR>>>
where
  IR: IndexReader,
  R: Rng + ?Sized,
{
  let context = reader.get_context()?;
  match concurrency {
    Concurrency::None => IndexSearcher::new(context),
    Concurrency::InterSegment => IndexSearcher::with_threads(context, random.random_range(2..=5)),
    Concurrency::IntraSegment => Ok(
      IndexSearcher::with_threads(context, random.random_range(2..=5))?.with_hook(
        IndexSearcherHook::IntraSliceDocIdOrderWithPartitions(
          IntraSliceDocIdOrderWithPartitionsIndexSearcher,
        ),
      ),
    ),
  }
}

/// Inspects the stack trace to figure out if a method of a specific type
/// called us.
#[inline(never)]
pub(crate) fn call_stack_contains<T>(method_name: &str) -> bool {
  let type_name = std::any::type_name::<T>();
  let type_name = type_name.split('<').next().unwrap_or(type_name);
  let method_name = format!("::{method_name}");
  let helper_name = concat!(module_path!(), "::call_stack_contains");
  Backtrace::force_capture().to_string().lines().any(|frame| {
    !frame.contains(helper_name)
      && frame.contains(type_name)
      && frame.match_indices(&method_name).any(|(index, _)| {
        let suffix = &frame[index + method_name.len()..];
        suffix.is_empty() || suffix.starts_with("::<") || suffix.starts_with("::{closure")
      })
  })
}

/// Inspects the stack trace to figure out if one of the given method names (no
/// type restriction) called us.
#[inline(never)]
pub(crate) fn call_stack_contains_any_of(method_names: &[&str]) -> bool {
  let backtrace = Backtrace::force_capture().to_string();
  let helper_name = concat!(module_path!(), "::call_stack_contains");
  method_names.iter().any(|method_name| {
    let method_name = format!("::{method_name}");
    backtrace.lines().any(|frame| {
      !frame.contains(helper_name)
        && frame.match_indices(&method_name).any(|(index, _)| {
          let suffix = &frame[index + method_name.len()..];
          suffix.is_empty() || suffix.starts_with("::<") || suffix.starts_with("::{closure")
        })
    })
  })
}

/// Inspects the stack trace to figure out if a method of a specific type
/// called us.
#[inline(never)]
pub(crate) fn call_stack_contains_type<T>() -> bool {
  let type_name = std::any::type_name::<T>();
  let type_name = type_name.split('<').next().unwrap_or(type_name);
  let helper_name = concat!(module_path!(), "::call_stack_contains");
  Backtrace::force_capture()
    .to_string()
    .lines()
    .any(|frame| !frame.contains(helper_name) && frame.contains(type_name))
}

pub(crate) fn slow_file_exists(dir: &impl Directory, name: &str) -> Result<bool> {
  match dir.open_input(name, &IOContext::read_once_io_context()?) {
    Ok(input) => {
      input.close()?;
      Ok(true)
    },
    Err(LuceneError::IoWithPath { source, .. }) if source.kind() == ErrorKind::NotFound => {
      Ok(false)
    },
    Err(LuceneError::Io { source, .. }) if source.kind() == ErrorKind::NotFound => Ok(false),
    Err(LuceneError::NoSuchFile(_)) => Ok(false),
    Err(error) => Err(error),
  }
}

pub fn create_temp_dir() -> Result<TempDir> {
  let temp_dir = TempDir::new()?;
  Ok(temp_dir)
}

pub fn create_temp_dir_with_prefix<T>(prefix: T) -> Result<TempDir>
where
  T: Into<String>,
{
  let name = prefix.into();
  let temp_dir = TempDir::with_prefix(name)?;
  Ok(temp_dir)
}

/// Ensures that the MergePolicy has sane values for tests that test with lots of documents.
pub(crate) fn ensure_sane_iwc_on_nightly<D>(conf: &mut IndexWriterConfig<D>) -> Result<()>
where
  D: Directory,
{
  if is_night_mode() {
    conf.set_use_compound_file(true);
    let mp = conf.get_merge_policy_mut();

    match mp {
      MergePolicyEnum::Tiered(mp) => {
        mp.set_max_merged_segment_mb(5000.0)?;
      },
      MergePolicyEnum::LogBytesSize(mp) => {
        mp.set_max_merge_mb(1000.0);
      },
      MergePolicyEnum::LogDoc(mp) => {
        mp.set_max_merge_docs(100000);
      },
      MergePolicyEnum::Alcoholic(mp) => {
        mp.set_max_merge_docs(100000);
      },
      _ => {},
    }

    let no_cfs_ratio = mp.get_base().get_no_cfs_ratio();
    mp.get_base_mut().set_no_cfs_ratio(no_cfs_ratio.max(0.25))?;
  }
  Ok(())
}

/// Creates a `BytesRef` holding UTF-8 bytes for the incoming string,
/// that sometimes uses a non-zero offset and non-zero end-padding to
/// tickle latent bugs that fail to look at `BytesRef.offset`.
pub(crate) fn new_bytes_ref_from_string<R, AV>(random: &mut R, s: &str) -> Result<BytesRef<AV>>
where
  R: Rng + ?Sized,
  AV: SharedAccessVec<u8>,
{
  let bytes = s.as_bytes();
  new_bytes_ref(random, bytes, 0, bytes.len() as i32)
}

/// Creates a copy of the incoming `BytesRef` that sometimes uses a non-zero
/// offset, and non-zero end-padding, to tickle latent bugs that fail to look at
/// `BytesRef.offset`.
pub(crate) fn new_bytes_ref_from_bytes_ref<R, AV>(
  random: &mut R,
  b: &BytesRef<AV>,
) -> Result<BytesRef<AV>>
where
  R: Rng + ?Sized,
  AV: SharedAccessVec<u8>,
{
  assert!(b.is_valid()?);
  b.bytes
    .access(|bytes| new_bytes_ref(random, bytes, b.offset as i32, b.length as i32))
}

/// Creates a random `BytesRef` from the incoming bytes, sometimes using a
/// non-zero offset, and non-zero end-padding, to tickle latent bugs that fail
/// to look at `BytesRef.offset`.
pub(crate) fn new_bytes_ref_from_bytes<R, AV>(
  random: &mut R,
  bytes_in: &[u8],
) -> Result<BytesRef<AV>>
where
  R: Rng + ?Sized,
  AV: SharedAccessVec<u8>,
{
  new_bytes_ref(random, bytes_in, 0, bytes_in.len() as i32)
}

/// Creates a random empty `BytesRef` that sometimes uses a non-zero offset, and
/// non-zero end-padding, to tickle latent bugs that fail to look at
/// `BytesRef.offset`.
pub(crate) fn new_bytes_ref_empty<R, AV>(random: &mut R) -> Result<BytesRef<AV>>
where
  R: Rng + ?Sized,
  AV: SharedAccessVec<u8>,
{
  // Calling the existing `new_bytes_ref` function
  new_bytes_ref(random, &[], 0, 0)
}

/// Creates a random empty `BytesRef`, with at least the requested length of
/// bytes free, that sometimes uses a non-zero offset and non-zero end-padding
/// to tickle latent bugs that fail to look at `BytesRef.offset`.
pub(crate) fn new_bytes_ref_with_length<R, AV>(
  byte_length: i32,
  random: &mut R,
) -> Result<BytesRef<AV>>
where
  R: Rng + ?Sized,
  AV: SharedAccessVec<u8>,
{
  let bytes_in = vec![0u8; byte_length as usize];
  new_bytes_ref(random, &bytes_in, 0, byte_length)
}

/// Creates a copy of the incoming bytes slice that sometimes uses a non-zero
/// `offset`, and non-zero end-padding, to expose latent bugs that fail to
/// account for `BytesRef::offset`.
pub(crate) fn new_bytes_ref<R, AV>(
  random: &mut R,
  bytes_in: &[u8],
  offset: i32,
  length: i32,
) -> Result<BytesRef<AV>>
where
  R: Rng + ?Sized,
  AV: SharedAccessVec<u8>,
{
  assert!(
    bytes_in.len() >= (offset + length) as usize,
    "got offset={} length={} bytesIn.length={}",
    offset,
    length,
    bytes_in.len()
  );
  // Randomly set a non-zero offset
  let start_offset = if random.random_bool(0.5) {
    random.random_range(1..=20)
  } else {
    0
  };

  // Randomly set an end padding (between 1 and 20)
  let end_padding = if random.random_bool(0.5) {
    random.random_range(1..=20)
  } else {
    0
  };

  let mut bytes = vec![0u8; (start_offset + length + end_padding) as usize];

  bytes.copy_from(
    &bytes_in[offset as usize..(offset + length) as usize],
    start_offset as usize,
  );
  // Create a BytesRef and return it
  let vec = AV::from_vec(bytes);
  let it = BytesRef {
    bytes: vec,
    offset: start_offset as usize,
    length: length as usize,
  };
  assert!(it.is_valid()?);

  if random.random_range(1..=17) == 7 {
    return it
      .bytes
      .access(|bytes| new_bytes_ref(random, bytes, it.offset as i32, it.length as i32));
  }
  Ok(it)
}
