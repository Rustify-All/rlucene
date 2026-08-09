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

use crate::core::document::document::Document;
use crate::core::document::field::Store;
use crate::core::document::knn_float_vector_field::KnnFloatVectorField;
use crate::core::document::string_field::StringField;
use crate::core::document::text_field::TextField;
use crate::core::index::concurrent_merge_scheduler::{
  AUTO_DETECT_MERGES_AND_THREADS, ConcurrentMergeScheduler, ConcurrentMergeSchedulerHook,
};
use crate::core::index::directory_reader;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::{IndexWriter, IndexWriterHooks, IndexWriterHooksEnum};
use crate::core::index::index_writer_config::{IndexWriterConfig, OpenMode};
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::log_byte_size_merge_policy::LogByteSizeMergePolicy;
use crate::core::index::log_merge_policy::LogMergePolicy;
use crate::core::index::merge_scheduler::MergeSchedulerEnum;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::term::Term;
use crate::core::index::tiered_merge_policy::TieredMergePolicy;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::store::directory::{DirEnum, Directory};
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::info_stream::{InfoStream, InfoStreamEnum};
use crate::core::util::io_utils::IOUtils;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::suppressing_concurrent_merge_scheduler::SuppressingConcurrentMergeScheduler;
use crate::test_framework::core::index::test_concurrent_merge_scheduler::{
  CountDownLatch, HangDuringRollbackConcurrentMergeScheduler,
  LiveMaxMergeCountConcurrentMergeScheduler, LiveMaxMergeCountMergePolicy,
  MaxMergeCountConcurrentMergeScheduler, MaybeStallCalledConcurrentMergeScheduler,
  MergeThreadMessagesConcurrentMergeScheduler, NoStallMergeThreadsConcurrentMergeScheduler,
  TrackingConcurrentMergeScheduler,
};
use crate::test_framework::core::index::test_index_writer::assert_no_unreferenced_files;
use crate::test_framework::core::store::mock_directory_wrapper::{
  Failure, MockDirectoryWrapper, Throttling,
};
use crate::test_framework::core::util::lucene_test_case::{
  call_stack_contains_any_of, is_night_mode, new_directory_shared,
  new_index_writer_config_with_analyzer, new_log_merge_policy,
  new_log_merge_policy_with_merge_factor, new_mock_directory, random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::RngExt;
use std::io::Error;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[allow(dead_code)] // for quick search
struct TestConcurrentMergeScheduler;

#[derive(Clone)]
struct FailOnlyOnFlush {
  do_fail: Arc<AtomicBool>,
  hit_exc: Arc<AtomicBool>,
  test_thread: thread::ThreadId,
}

impl FailOnlyOnFlush {
  fn set_do_fail(&self) {
    self.do_fail.store(true, Ordering::SeqCst);
    self.hit_exc.store(false, Ordering::SeqCst);
  }

  fn clear_do_fail(&self) {
    self.do_fail.store(false, Ordering::SeqCst);
  }
}

impl<D> Failure<D> for FailOnlyOnFlush
where
  D: Directory,
{
  fn eval(&mut self, dir: &MockDirectoryWrapper<D>) -> Result<()> {
    if self.do_fail.load(Ordering::SeqCst)
      && thread::current().id() == self.test_thread
      && call_stack_contains_any_of(&["flush"])
      && !call_stack_contains_any_of(&["close"])
      && dir.state.random_state.lock().random_bool(0.5)
    {
      self.hit_exc.store(true, Ordering::SeqCst);
      return Err(LuceneError::io(Error::other(format!(
        "{:?}: now failing during flush",
        thread::current().id()
      ))));
    }
    Ok(())
  }

  fn set_do_fail(&mut self) {
    self.do_fail.store(true, Ordering::SeqCst);
    self.hit_exc.store(false, Ordering::SeqCst);
  }

  fn clear_do_fail(&mut self) {
    self.do_fail.store(false, Ordering::SeqCst);
  }
}

// Make sure running BG merges still work fine even when
// we are hitting exceptions during flushing.
#[allow(clippy::never_loop)]
#[test]
fn test_flush_exceptions() -> Result<()> {
  let mut random = random();
  let directory = Arc::new(new_mock_directory(&mut random)?);
  let failure = FailOnlyOnFlush {
    do_fail: Arc::new(AtomicBool::new(false)),
    hit_exc: Arc::new(AtomicBool::new(false)),
    test_thread: thread::current().id(),
  };
  directory.fail_on(Box::new(failure.clone()));
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  iwc.set_max_buffered_docs(2);
  if matches!(iwc.get_merge_scheduler(), MergeSchedulerEnum::Concurrent(_)) {
    let merge_scheduler =
      ConcurrentMergeScheduler::with_hook(ConcurrentMergeSchedulerHook::Suppressing(
        SuppressingConcurrentMergeScheduler::writer_closed_or_tragic(),
      ));
    iwc.set_merge_scheduler(merge_scheduler);
  }
  let writer = IndexWriter::new(directory.clone(), iwc)?;

  'outer: for i in 0..10 {
    if cfg!(feature = "test_log_verbose") {
      println!("TEST: iter={i}");
    }

    for j in 0..20 {
      let mut doc = Document::new();
      doc.add(StringField::from_string(
        "id",
        (i * 20 + j).to_string(),
        Store::Yes,
      )?);
      // Add knn float vectors to test parallel merge
      doc.add(KnnFloatVectorField::new(
        "knn",
        vec![random.random::<f32>(), random.random::<f32>()],
      )?);
      writer.add_document(doc)?;
    }

    // must cycle here because sometimes the merge flushes
    // the doc we just added and so there's nothing to
    // flush, and we don't hit the exception
    loop {
      let mut doc = Document::new();
      doc.add(StringField::from_string("id", "", Store::Yes)?);
      doc.add(KnnFloatVectorField::new("knn", vec![0.0, 0.0])?);
      writer.add_document(doc)?;
      failure.set_do_fail();
      match writer.flush() {
        Ok(()) => {
          if failure.hit_exc.load(Ordering::SeqCst) {
            return Err(LuceneError::illegal_state("failed to hit IOException"));
          }
        },
        Err(error) if error.is_io_error() => {
          if cfg!(feature = "test_log_verbose") {
            eprintln!("{error:?}");
          }
          failure.clear_do_fail();
          // make sure we are closed or closing - if we are unlucky a merge does
          // the actual closing for us. this is rare but might happen since the
          // tragicEvent is checked by IFD and that might throw during a merge
          assert!(matches!(
            writer.ensure_open(),
            Err(LuceneError::AlreadyClosed(_))
          ));
          // Abort should have closed the deleter:
          assert!(writer.is_deleter_closed()?);
          writer.close()?; // now wait for the close to actually happen if a merge thread did the
          // close.
          break 'outer;
        },
        Err(error) => return Err(error),
      }
    }
  }

  assert!(!directory_reader::index_exists(directory.as_ref())?);
  directory.as_ref().close()?;
  Ok(())
}

// Test that deletes committed after a merge started and
// before it finishes, are correctly merged back:
#[test]
fn test_delete_merging() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;

  let mut mp = LogMergePolicy::log_doc();
  // Force degenerate merging so we can get a mix of
  // merging of segments with and without deletes at the
  // start:
  mp.set_min_merge_docs(1000);
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  iwc.set_merge_policy(mp);
  iwc.set_merge_scheduler(ConcurrentMergeScheduler::new());
  let writer = IndexWriter::new(directory.clone(), iwc)?;
  TestUtil::reduce_open_files(&writer)?;

  for i in 0..10 {
    if cfg!(feature = "test_log_verbose") {
      println!("\nTEST: cycle");
    }
    for j in 0..100 {
      let mut doc = Document::new();
      doc.add(StringField::from_string(
        "id",
        (i * 100 + j).to_string(),
        Store::Yes,
      )?);
      writer.add_document(doc)?;
    }

    let mut del_id = i;
    while del_id < 100 * (1 + i) {
      if cfg!(feature = "test_log_verbose") {
        println!("TEST: del {del_id}");
      }
      writer.delete_documents_with_terms(vec![Term::from_text("id", del_id.to_string())])?;
      del_id += 10;
    }

    writer.commit()?;
  }

  writer.close()?;
  let reader = directory_reader::open(directory)?;
  // Verify that we did not lose any deletes...
  assert_eq!(450, reader.num_docs()?);
  reader.close()?;
  Ok(())
}

#[test]
fn test_no_extra_files() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  let mut field_types = std::collections::HashMap::new();

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  iwc.set_max_buffered_docs(2);
  iwc.set_merge_scheduler(ConcurrentMergeScheduler::new());
  let mut writer = IndexWriter::new(directory.clone(), iwc)?;

  for _iter in 0..7 {
    for _ in 0..21 {
      let mut doc = Document::new();
      doc.add(
        crate::test_framework::core::util::lucene_test_case::new_text_field(
          &mut random,
          "content",
          "a b c",
          Store::No,
          &mut field_types,
        )?,
      );
      writer.add_document(doc)?;
    }

    writer.close()?;
    assert_no_unreferenced_files(directory.clone(), "testNoExtraFiles")?;

    // Reopen
    let analyzer = MockAnalyzer::new(&mut random);
    let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
    iwc.set_open_mode(OpenMode::Append);
    iwc.set_max_buffered_docs(2);
    iwc.set_merge_scheduler(ConcurrentMergeScheduler::new());
    writer = IndexWriter::new(directory.clone(), iwc)?;
  }

  writer.close()?;
  Ok(())
}

#[test]
fn test_no_wait_close() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  // Force excessive merging:
  iwc
    .set_max_buffered_docs(2)
    .set_merge_policy(new_log_merge_policy_with_merge_factor(&mut random, 100)?)
    .set_commit_on_close(false)
    .set_merge_scheduler(ConcurrentMergeScheduler::new());

  let mut writer = IndexWriter::new(directory.clone(), iwc)?;

  let num_iters = if is_night_mode() { 10 } else { 3 };
  for iter in 0..num_iters {
    for j in 0..201 {
      let mut doc = Document::new();
      doc.add(StringField::from_string(
        "id",
        (iter * 201 + j).to_string(),
        Store::Yes,
      )?);
      doc.add(KnnFloatVectorField::new(
        "knn",
        vec![random.random::<f32>(), random.random::<f32>()],
      )?);
      writer.add_document(doc)?;
    }

    let mut del_id = iter * 201;
    for _ in 0..20 {
      writer.delete_documents_with_terms(vec![Term::from_text("id", del_id.to_string())])?;
      del_id += 5;
    }

    // Force a bunch of merge threads to kick off so we
    // stress out aborting them on close:
    match writer.get_config_mut().get_merge_policy_mut() {
      crate::core::index::merge_policy::MergePolicyEnum::LogDoc(mp) => mp.set_merge_factor(3)?,
      crate::core::index::merge_policy::MergePolicyEnum::LogBytesSize(mp) => {
        mp.set_merge_factor(3)?
      },
      _ => {},
    }
    let mut doc = Document::new();
    doc.add(StringField::from_string(
      "id",
      format!("extra-{iter}"),
      Store::Yes,
    )?);
    doc.add(KnnFloatVectorField::new(
      "knn",
      vec![random.random::<f32>(), random.random::<f32>()],
    )?);
    writer.add_document(doc)?;

    let commit_result = writer.commit();
    let close_result = writer.close();
    commit_result?;
    close_result?;

    let reader = directory_reader::open(directory.clone())?;
    assert_eq!((1 + iter) * 182, reader.num_docs()?);
    reader.close()?;

    // Reopen
    let analyzer = MockAnalyzer::new(&mut random);
    let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
    iwc.set_open_mode(OpenMode::Append);
    iwc.set_merge_policy(new_log_merge_policy_with_merge_factor(&mut random, 100)?);
    // Force excessive merging:
    iwc.set_max_buffered_docs(2);
    iwc.set_commit_on_close(false);
    iwc.set_merge_scheduler(ConcurrentMergeScheduler::new());
    writer = IndexWriter::new(directory.clone(), iwc)?;
  }
  writer.close()?;

  Ok(())
}

// LUCENE-4544
#[test]
fn test_max_merge_count() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  iwc.set_commit_on_close(false);

  let max_merge_count = random.random_range(1..=5);
  let max_merge_threads = random.random_range(1..=max_merge_count);
  let test_scheduler = MaxMergeCountConcurrentMergeScheduler::new(max_merge_count);

  if cfg!(feature = "test_log_verbose") {
    println!("TEST: maxMergeCount={max_merge_count} maxMergeThreads={max_merge_threads}");
  }

  let cms = ConcurrentMergeScheduler::with_hook(ConcurrentMergeSchedulerHook::MaxMergeCount(
    test_scheduler.clone(),
  ));
  cms.set_max_merges_and_threads(max_merge_count, max_merge_threads)?;
  iwc.set_merge_scheduler(cms);
  iwc.set_max_buffered_docs(2);

  let mut tmp = TieredMergePolicy::new();
  tmp.set_max_merge_at_once(2)?;
  tmp.set_segments_per_tier(2.0)?;
  iwc.set_merge_policy(tmp);

  let writer = IndexWriter::new(dir.clone(), iwc)?;
  while test_scheduler.enough_merges_waiting().get_count() != 0 && !test_scheduler.failed() {
    for _ in 0..10 {
      let mut doc = Document::new();
      doc.add(TextField::from_string("field", "field", Store::No)?);
      writer.add_document(doc)?;
    }
  }
  let commit_result = writer.commit();
  let close_result = writer.close();
  close_result?;
  commit_result?;
  dir.as_ref().close()?;
  Ok(())
}

#[test]
fn test_small_merges_don_not_get_threads() -> Result<()> {
  // Rust Lucene does not implement Java Lucene's intra-merge executor selection API.
  test_not_required_in_rust_lucene!();
}

#[test]
fn test_intra_merge_thread_pool_is_limited_by_max_threads() -> Result<()> {
  // Rust Lucene does not implement Java Lucene's intra-merge executor or its thread-pool limits.
  test_not_required_in_rust_lucene!();
}

#[test]
fn test_total_bytes_size() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  if let DirEnum::B(directory) = directory.as_ref() {
    directory.set_throttling(Throttling::Never);
  }
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  iwc.set_max_buffered_docs(5);
  let at_least_one_merge = CountDownLatch::new(1);
  let tracking_scheduler = TrackingConcurrentMergeScheduler::new(at_least_one_merge.clone());
  let cms = ConcurrentMergeScheduler::with_hook(ConcurrentMergeSchedulerHook::Tracking(
    tracking_scheduler.clone(),
  ));
  cms.set_max_merges_and_threads(5, 5)?;
  iwc.set_merge_scheduler(cms);
  // SimpleText is not implemented in Rust Lucene, so no postings format replacement is needed.
  let writer = IndexWriter::new(directory.clone(), iwc)?;
  for i in 0..1000 {
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", i.to_string(), Store::No)?);
    writer.add_document(doc)?;

    if random.random_bool(0.5) {
      writer.delete_documents_with_terms(vec![Term::from_text(
        "id",
        random.random_range(0..=i).to_string(),
      )])?;
    }
  }
  at_least_one_merge.wait();
  assert_ne!(0, tracking_scheduler.total_merged_bytes());
  writer.close()?;
  directory.as_ref().close()?;
  Ok(())
}

#[test]
fn test_invalid_max_merge_count_and_threads() -> Result<()> {
  let cms = ConcurrentMergeScheduler::new();
  assert!(matches!(
    cms
      .set_max_merges_and_threads(AUTO_DETECT_MERGES_AND_THREADS, 3)
      .unwrap_err(),
    LuceneError::IllegalArgument(_)
  ));
  assert!(matches!(
    cms
      .set_max_merges_and_threads(3, AUTO_DETECT_MERGES_AND_THREADS)
      .unwrap_err(),
    LuceneError::IllegalArgument(_)
  ));
  Ok(())
}

#[test]
fn test_live_max_merge_count() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  iwc.set_merge_policy(LiveMaxMergeCountMergePolicy::default());
  iwc.set_max_buffered_docs(2);
  iwc.set_ram_buffer_size_mb(-1.0);

  let test_scheduler = LiveMaxMergeCountConcurrentMergeScheduler::default();
  let cms = ConcurrentMergeScheduler::with_hook(ConcurrentMergeSchedulerHook::LiveMaxMergeCount(
    test_scheduler.clone(),
  ));

  assert_eq!(AUTO_DETECT_MERGES_AND_THREADS, cms.get_max_merge_count());
  assert_eq!(AUTO_DETECT_MERGES_AND_THREADS, cms.get_max_thread_count());

  cms.set_max_merges_and_threads(5, 3)?;
  iwc.set_merge_scheduler(cms.clone());

  let writer = IndexWriter::new(directory.clone(), iwc)?;
  // Makes 100 segments.
  for _ in 0..200 {
    writer.add_document(Document::new())?;
  }

  // No merges should have run so far, because the merge policy does not return natural merges:
  assert_eq!(0, test_scheduler.max_running_merge_count());
  writer.force_merge(1)?;

  // At most 5 merge threads should have launched at once:
  assert!(
    test_scheduler.max_running_merge_count() <= 5,
    "maxRunningMergeCount={}",
    test_scheduler.max_running_merge_count()
  );
  test_scheduler.reset_max_running_merge_count();

  // Makes another 100 segments.
  for _ in 0..200 {
    writer.add_document(Document::new())?;
  }

  cms.set_max_merges_and_threads(1, 1)?;
  writer.force_merge(1)?;

  // At most 1 merge thread should have launched at once:
  assert_eq!(1, test_scheduler.max_running_merge_count());

  writer.close()?;
  directory.as_ref().close()?;
  Ok(())
}

// LUCENE-6063
#[test]
fn test_maybe_stall_called() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  iwc.set_merge_policy(LogMergePolicy::<LogByteSizeMergePolicy>::log_bytes_size());
  let test_scheduler = MaybeStallCalledConcurrentMergeScheduler::default();
  iwc.set_merge_scheduler(ConcurrentMergeScheduler::with_hook(
    ConcurrentMergeSchedulerHook::MaybeStallCalled(test_scheduler.clone()),
  ));
  let writer = IndexWriter::new(dir.clone(), iwc)?;
  writer.add_document(Document::new())?;
  writer.flush()?;
  writer.add_document(Document::new())?;
  writer.force_merge(1)?;
  assert!(test_scheduler.was_called());
  writer.close()?;
  dir.as_ref().close()?;
  Ok(())
}

// LUCENE-6094
#[test]
fn test_hang_during_rollback() -> Result<()> {
  let mut random = random();
  let dir = Arc::new(new_mock_directory(&mut random)?);
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  iwc.set_max_buffered_docs(2);
  let mut mp = LogMergePolicy::log_doc();
  mp.set_merge_factor(2)?;
  iwc.set_merge_policy(mp);
  let merge_start = CountDownLatch::new(1);
  let merge_finish = CountDownLatch::new(1);
  let cms = ConcurrentMergeScheduler::with_hook(ConcurrentMergeSchedulerHook::HangDuringRollback(
    HangDuringRollbackConcurrentMergeScheduler::new(merge_start.clone(), merge_finish.clone()),
  ));
  cms.set_max_merges_and_threads(1, 1)?;
  iwc.set_merge_scheduler(cms);

  let writer = IndexWriter::new(dir.clone(), iwc)?;
  writer.add_document(Document::new())?;
  writer.add_document(Document::new())?;
  // flush

  writer.add_document(Document::new())?;
  writer.add_document(Document::new())?;
  // flush + merge

  // Wait for merge to kick off.
  merge_start.wait();

  let writer_ref = writer.clone();
  let add_documents_thread = thread::spawn(move || -> Result<()> {
    writer_ref.add_document(Document::new())?;
    writer_ref.add_document(Document::new())?;
    // flush

    writer_ref.add_document(Document::new())?;
    // Without the fix for LUCENE-6094 we would hang forever here:
    writer_ref.add_document(Document::new())?;
    // flush + merge

    // Now allow first merge to finish:
    merge_finish.count_down();
    Ok(())
  });

  while writer.get_doc_stats()?.num_docs != 8 {
    thread::sleep(Duration::from_millis(10));
  }

  writer.rollback()?;
  match add_documents_thread.join() {
    Ok(result) => result?,
    Err(payload) => {
      return Err(LuceneError::tragedy_from_panic(
        "panic while adding documents",
        payload.as_ref(),
      ));
    },
  }
  dir.as_ref().close()?;
  Ok(())
}

// LUCENE-10118 : Verify the basic log output from MergeThreads
#[derive(Clone, Default)]
struct MergeThreadInfoStream {
  messages: Arc<Mutex<Vec<String>>>,
}

impl CloseableRef for MergeThreadInfoStream {
  fn close(&self) -> Result<()> {
    Ok(())
  }
}

impl InfoStream for MergeThreadInfoStream {
  fn message(&self, component: &str, message: &str) -> Result<()> {
    if component == "MS" {
      self
        .messages
        .lock()
        .expect("merge thread messages mutex poisoned")
        .push(message.to_string());
    }
    Ok(())
  }

  fn is_enabled(&self, component: &str) -> bool {
    component == "MS"
  }
}

#[test]
fn test_merge_thread_messages() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let test_scheduler = MergeThreadMessagesConcurrentMergeScheduler::default();
  let cms = ConcurrentMergeScheduler::with_hook(ConcurrentMergeSchedulerHook::MergeThreadMessages(
    test_scheduler.clone(),
  ));
  iwc.set_merge_scheduler(cms);

  let info_stream = MergeThreadInfoStream::default();
  iwc.set_info_stream(Arc::new(InfoStreamEnum::Custom(Box::new(
    info_stream.clone(),
  ))));
  iwc.set_max_buffered_docs(2);
  let mut lmp = new_log_merge_policy(&mut random)?;
  match &mut lmp {
    crate::core::index::merge_policy::MergePolicyEnum::LogDoc(lmp) => {
      lmp.set_merge_factor(2)?;
      lmp.set_target_search_concurrency(1)?;
    },
    crate::core::index::merge_policy::MergePolicyEnum::LogBytesSize(lmp) => {
      lmp.set_merge_factor(2)?;
      lmp.set_target_search_concurrency(1)?;
    },
    _ => {
      return Err(LuceneError::illegal_state(
        "expected LogMergePolicy variant",
      ));
    },
  }
  iwc.set_merge_policy(lmp);

  let writer = IndexWriter::new(dir.clone(), iwc)?;
  let mut doc = Document::new();
  doc.add(TextField::from_string("foo", "", Store::No)?);
  writer.add_document(doc)?;
  writer.add_document(Document::new())?;
  // flush
  writer.add_document(Document::new())?;
  writer.add_document(Document::new())?;
  // flush + merge
  writer.close()?;
  dir.as_ref().close()?;

  let merge_thread_names = test_scheduler.merge_thread_names();
  assert!(!merge_thread_names.is_empty());
  let messages = info_stream
    .messages
    .lock()
    .expect("merge thread messages mutex poisoned");
  for name in merge_thread_names {
    let prefix = format!("merge thread {name}");
    let thread_messages: Vec<&String> = messages
      .iter()
      .filter(|line| line.starts_with(&prefix))
      .collect();
    assert!(
      thread_messages.len() >= 3,
      "expected a value equal to or greater than 3, got: {}, threadMsgs={thread_messages:?}",
      thread_messages.len(),
    );
    assert!(
      thread_messages[0].starts_with(&format!("merge thread {name} start")),
      "threadMsgs={thread_messages:?}",
    );
    assert!(
      thread_messages
        .iter()
        .any(|line| line.starts_with(&format!("merge thread {name} merge segment")))
    );
    assert!(
      thread_messages[thread_messages.len() - 1].starts_with(&format!("merge thread {name} end")),
      "threadMsgs={thread_messages:?}",
    );
  }
  Ok(())
}

#[test]
fn test_dynamic_defaults() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let cms = ConcurrentMergeScheduler::new();
  assert_eq!(AUTO_DETECT_MERGES_AND_THREADS, cms.get_max_merge_count());
  assert_eq!(AUTO_DETECT_MERGES_AND_THREADS, cms.get_max_thread_count());
  iwc.set_merge_scheduler(cms.clone());
  iwc.set_max_buffered_docs(2);
  let mut lmp = LogMergePolicy::log_doc();
  lmp.set_merge_factor(2)?;
  iwc.set_merge_policy(lmp);

  let writer = IndexWriter::new(dir, iwc)?;
  writer.add_document(Document::new())?;
  writer.add_document(Document::new())?;
  // flush

  writer.add_document(Document::new())?;
  writer.add_document(Document::new())?;
  // flush + merge

  // CMS should have now set true values:
  assert_ne!(AUTO_DETECT_MERGES_AND_THREADS, cms.get_max_merge_count());
  assert_ne!(AUTO_DETECT_MERGES_AND_THREADS, cms.get_max_thread_count());
  writer.close()?;
  Ok(())
}

#[test]
fn test_reset_to_auto_default() -> Result<()> {
  let cms = ConcurrentMergeScheduler::new();
  assert_eq!(AUTO_DETECT_MERGES_AND_THREADS, cms.get_max_merge_count());
  assert_eq!(AUTO_DETECT_MERGES_AND_THREADS, cms.get_max_thread_count());
  cms.set_max_merges_and_threads(4, 3)?;
  assert_eq!(4, cms.get_max_merge_count());
  assert_eq!(3, cms.get_max_thread_count());

  assert!(matches!(
    cms
      .set_max_merges_and_threads(AUTO_DETECT_MERGES_AND_THREADS, 4)
      .unwrap_err(),
    LuceneError::IllegalArgument(_)
  ));

  assert!(matches!(
    cms
      .set_max_merges_and_threads(4, AUTO_DETECT_MERGES_AND_THREADS)
      .unwrap_err(),
    LuceneError::IllegalArgument(_)
  ));

  cms.set_max_merges_and_threads(
    AUTO_DETECT_MERGES_AND_THREADS,
    AUTO_DETECT_MERGES_AND_THREADS,
  )?;
  assert_eq!(AUTO_DETECT_MERGES_AND_THREADS, cms.get_max_merge_count());
  assert_eq!(AUTO_DETECT_MERGES_AND_THREADS, cms.get_max_thread_count());
  Ok(())
}

#[test]
fn test_spinning_defaults() -> Result<()> {
  let cms = ConcurrentMergeScheduler::new();
  cms.set_default_max_merges_and_threads(true);
  assert_eq!(1, cms.get_max_thread_count());
  assert_eq!(6, cms.get_max_merge_count());
  Ok(())
}

#[test]
fn test_auto_io_throttle_getter() -> Result<()> {
  let cms = ConcurrentMergeScheduler::new();
  assert!(!cms.get_auto_io_throttle());
  cms.enable_auto_io_throttle()?;
  assert!(cms.get_auto_io_throttle());
  cms.disable_auto_io_throttle()?;
  assert!(!cms.get_auto_io_throttle());
  Ok(())
}

#[test]
fn test_non_spinning_defaults() -> Result<()> {
  let cms = ConcurrentMergeScheduler::new();
  cms.set_default_max_merges_and_threads(false);
  let thread_count = cms.get_max_thread_count();
  assert!(thread_count >= 1);
  // assert!(thread_count <= 4);
  assert_eq!(5 + thread_count, cms.get_max_merge_count());
  Ok(())
}

// LUCENE-6197
#[test]
fn test_no_stall_merge_threads() -> Result<()> {
  let mut random = random();
  let dir = Arc::new(new_mock_directory(&mut random)?);

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  iwc.set_merge_policy(NoMergePolicy::default());
  iwc.set_max_buffered_docs(2);
  iwc.set_use_compound_file(true); // reduce open files
  let writer = IndexWriter::new(dir.clone(), iwc)?;
  let num_docs = if is_night_mode() { 1000 } else { 100 };
  for i in 0..num_docs {
    let mut doc = Document::new();
    doc.add(StringField::from_string(
      "field",
      i.to_string(),
      Store::Yes,
    )?);
    writer.add_document(doc)?;
  }
  writer.close()?;

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  let test_scheduler = NoStallMergeThreadsConcurrentMergeScheduler::default();
  let cms = ConcurrentMergeScheduler::with_hook(ConcurrentMergeSchedulerHook::NoStallMergeThreads(
    test_scheduler.clone(),
  ));
  cms.enable_auto_io_throttle()?;
  cms.set_max_merges_and_threads(2, 1)?;
  iwc.set_merge_scheduler(cms);
  iwc.set_max_buffered_docs(2);

  let writer = IndexWriter::new(dir, iwc)?;
  writer.force_merge(1)?;
  writer.close()?;

  assert!(!test_scheduler.failed());
  Ok(())
}

/*
 * This test tries to produce 2 merges running concurrently with 2 segments per merge. While these
 * merges run we kick off a forceMerge that puts a pending merge in the queue but waits for things to happen.
 * While we do this we reduce maxMergeCount to 1. If concurrency in CMS is not right the forceMerge will wait forever
 * since none of the currently running merges picks up the pending merge. This test fails every time.
 */
#[test]
fn test_change_max_merge_county_while_force_merge() -> Result<()> {
  let mut random = random();
  let num_iters = if is_night_mode() { 100 } else { 10 };
  for _ in 0..num_iters {
    let mut mp = LogMergePolicy::log_doc();
    mp.set_merge_factor(2)?;
    let force_merge_waits = Arc::new(CountDownLatch::new(1));
    let merge_threads_start_after_wait = Arc::new(CountDownLatch::new(1));
    let merge_threads_arrived = Arc::new(CountDownLatch::new(2));
    let stream = ChangeMaxMergeCountInfoStream {
      force_merge_waits: force_merge_waits.clone(),
      merge_threads_start_after_wait: merge_threads_start_after_wait.clone(),
      merge_threads_arrived: merge_threads_arrived.clone(),
    };

    let dir = new_directory_shared(&mut random)?;
    let cms = ConcurrentMergeScheduler::new();
    let writer_result = (|| -> Result<_> {
      let mut iwc = IndexWriterConfig::new()?;
      iwc.set_merge_scheduler(cms.clone());
      iwc.set_merge_policy(mp);
      iwc.set_info_stream(InfoStreamEnum::Custom(Box::new(stream)));
      IndexWriter::with_hooks(
        dir.clone(),
        iwc,
        Some(IndexWriterHooksEnum::custom(TestPointsIndexWriterHooks)),
      )
    })();
    let writer = match writer_result {
      Ok(writer) => writer,
      Err(err) => {
        let dir_close_result = match Arc::try_unwrap(dir) {
          Ok(dir) => dir.close(),
          Err(_) => Err(LuceneError::illegal_state(
            "directory still has outstanding references",
          )),
        };
        return IOUtils::use_or_suppress_result(Err(err), dir_close_result);
      },
    };

    let body_result = (|| -> Result<()> {
      cms.set_max_merges_and_threads(2, 2)?;

      let force_merge_thread = {
        let _release_merge_threads = CountDownOnDrop::new(merge_threads_start_after_wait.clone());
        for _ in 0..4 {
          let mut document = Document::new();
          document.add(TextField::from_string(
            "foo",
            "the quick brown fox jumps over the lazy dog",
            Store::Yes,
          )?);
          document.add(TextField::from_string(
            "bar",
            TestUtil::random_realistic_unicode_string_with_len(&mut random, 20),
            Store::Yes,
          )?);
          writer.add_document(document)?;
          writer.flush()?;
        }
        let segment_infos = writer.clone_segment_infos()?;
        assert_eq!(4, writer.get_segment_count(), "{}", segment_infos);
        merge_threads_arrived.wait();
        let writer_ref = writer.clone();
        let force_merge_thread = thread::spawn(move || writer_ref.force_merge(1));
        force_merge_waits.wait();
        cms.set_max_merges_and_threads(1, 1)?;
        force_merge_thread
      };

      while !force_merge_thread.is_finished() {
        thread::sleep(Duration::from_millis(10));
        if cms.merge_thread_count() == 0 && writer.has_pending_merges()? {
          return Err(LuceneError::illegal_state(
            "writer has pending merges but no CMS threads are running",
          ));
        }
      }
      match force_merge_thread.join() {
        Ok(result) => result?,
        Err(payload) => {
          return Err(LuceneError::tragedy_from_panic(
            "panic while force merging",
            payload.as_ref(),
          ));
        },
      }
      assert_eq!(1, writer.get_segment_count());
      Ok(())
    })();

    let close_result = writer.close();
    drop(writer);
    let dir_close_result = match Arc::try_unwrap(dir) {
      Ok(dir) => dir.close(),
      Err(_) => Err(LuceneError::illegal_state(
        "directory still has outstanding references",
      )),
    };
    let close_result = IOUtils::use_or_suppress_result(close_result, dir_close_result);
    IOUtils::use_or_suppress_result(body_result, close_result)?;
  }
  Ok(())
}

struct CountDownOnDrop {
  latch: Arc<CountDownLatch>,
}

impl CountDownOnDrop {
  fn new(latch: Arc<CountDownLatch>) -> Self {
    Self { latch }
  }
}

impl Drop for CountDownOnDrop {
  fn drop(&mut self) {
    self.latch.count_down();
  }
}

struct ChangeMaxMergeCountInfoStream {
  force_merge_waits: Arc<CountDownLatch>,
  merge_threads_start_after_wait: Arc<CountDownLatch>,
  merge_threads_arrived: Arc<CountDownLatch>,
}

impl CloseableRef for ChangeMaxMergeCountInfoStream {
  fn close(&self) -> Result<()> {
    Ok(())
  }
}

impl InfoStream for ChangeMaxMergeCountInfoStream {
  fn message(&self, component: &str, message: &str) -> Result<()> {
    if component == "TP" {
      if message == "mergeMiddleStart" {
        self.merge_threads_arrived.count_down();
        self.merge_threads_start_after_wait.wait();
      } else if message == "forceMergeBeforeWait" {
        self.force_merge_waits.count_down();
      }
    }
    Ok(())
  }

  fn is_enabled(&self, component: &str) -> bool {
    component == "TP"
  }
}

struct TestPointsIndexWriterHooks;

impl IndexWriterHooks for TestPointsIndexWriterHooks {
  fn is_enable_test_points(&self) -> bool {
    true
  }
}
