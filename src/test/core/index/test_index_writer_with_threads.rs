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
use crate::core::document::field::{Field, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::string_field::StringField;
use crate::core::document::text_field::TextField;
use crate::core::index::BytesRef;
use crate::core::index::concurrent_merge_scheduler::{
  ConcurrentMergeScheduler, ConcurrentMergeSchedulerHook,
};
use crate::core::index::directory_reader;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_writer::{IndexWriter, MAX_TERM_LENGTH};
use crate::core::index::indexing_chain::IndexingChain;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::multi_bits;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::index::term_vectors::TermVectors;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, NO_MORE_DOCS};
use crate::core::store::directory::{Directory, MaybeNrtDirEnum};
use crate::core::util::bits::Bits;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::io_utils::IOUtils;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::index::suppressing_concurrent_merge_scheduler::SuppressingConcurrentMergeScheduler;
use crate::test_framework::core::store::mock_directory_wrapper::{Failure, MockDirectoryWrapper};
use crate::test_framework::core::util::line_file_docs::LineFileDocs;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, call_stack_contains, call_stack_contains_any_of, is_night_mode, new_directory_shared,
  new_index_writer_config_with_analyzer, new_log_merge_policy_with_merge_factor,
  new_mock_directory, random, random_from_seed, rarely,
};
use crate::test_framework::core::util::test_util::TestUtil;
use parking_lot::Mutex;
use rand::Rng;
use rand::RngExt;
use std::io::Error;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;
#[allow(dead_code)] // for quick search
struct TestIndexWriterWithThreads;

const SOFT_DELETES_FIELD: &str = "___soft_deletes";

type MockDirectoryDelegate = MaybeNrtDirEnum;

// Used by test cases below
fn indexer_thread<D>(
  writer: &IndexWriter<D>,
  no_errors: bool,
  sync_start: &Barrier,
  add_count: &AtomicUsize,
) -> Result<()>
where
  D: crate::core::store::directory::Directory + 'static,
{
  sync_start.wait();

  let mut document = Document::new();
  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?;
  custom_type.set_store_term_vectors(true)?;
  custom_type.set_store_term_vector_positions(true)?;
  custom_type.set_store_term_vector_offsets(true)?;

  document.add(Field::new(
    "field",
    "aaa bbb ccc ddd eee fff ggg hhh iii jjj",
    custom_type,
  ));
  document.add(NumericDocValuesField::new("dv", 5));

  let mut id_upto = 0;
  let mut full_count = 0;

  loop {
    let id = id_upto;
    id_upto += 1;
    match writer.update_document_with_term(Term::from_text("id", id.to_string()), document.clone())
    {
      Ok(_) => {
        add_count.fetch_add(1, Ordering::SeqCst);
      },
      Err(error)
        if error.is_io_error()
          && (error.to_string().contains("fake disk full at")
            || error.to_string().contains("now failing on purpose")) =>
      {
        thread::sleep(Duration::from_millis(1));
        if full_count >= 5 {
          break;
        }
        full_count += 1;
      },
      Err(LuceneError::IllegalState(_)) | Err(LuceneError::AlreadyClosed(_)) => {
        // OK: abort closes the writer
        break;
      },
      Err(error) => {
        if no_errors {
          return Err(error);
        }
        break;
      },
    }
  }
  Ok(())
}

// LUCENE-1130: make sure immediate disk full on creating
// an IndexWriter (hit during DWPT#updateDocuments()), with
// multiple threads, is OK:
#[test]
fn test_immediate_disk_full_with_threads() -> Result<()> {
  const NUM_THREADS: usize = 3;
  let num_iterations = if is_night_mode() { 10 } else { 1 };
  let mut random = random();
  for iter in 0..num_iterations {
    let dir = Arc::new(new_mock_directory(&mut random)?);
    let analyzer = MockAnalyzer::new(&mut random);
    let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
    config.set_max_buffered_docs(2);
    let merge_scheduler = ConcurrentMergeScheduler::new();
    merge_scheduler.set_suppress_exceptions();
    config.set_merge_scheduler(merge_scheduler);
    config.set_merge_policy(new_log_merge_policy_with_merge_factor(&mut random, 4)?);
    config.set_commit_on_close(false);
    let writer = IndexWriter::new(dir.clone(), config)?;
    dir.set_max_size_in_bytes(4 * 1024 + 20 * iter as i64);

    let sync_start = Barrier::new(NUM_THREADS + 1);
    let add_counts = (0..NUM_THREADS)
      .map(|_| AtomicUsize::new(0))
      .collect::<Vec<_>>();
    thread::scope(|scope| -> Result<()> {
      let mut threads = Vec::new();
      for add_count in &add_counts {
        let writer = &writer;
        let sync_start = &sync_start;
        threads.push(scope.spawn(move || indexer_thread(writer, true, sync_start, add_count)));
      }
      sync_start.wait();

      for thread in threads {
        // Without fix for LUCENE-1130: one of the threads will hang
        thread.join().expect("thread panicked")?;
      }
      Ok(())
    })?;

    // Make sure once disk space is avail again, we can cleanly close:
    dir.set_max_size_in_bytes(0);
    match writer.commit() {
      Ok(_) => {},
      Err(LuceneError::AlreadyClosed(_)) => {
        // OK: abort closes the writer
        assert!(writer.is_deleter_closed()?);
      },
      Err(error) => return Err(error),
    }
    writer.close()?;
    dir.as_ref().close()?;
  }
  Ok(())
}

// LUCENE-1130: make sure we can close() even while
// threads are trying to add documents. Strictly
// speaking, this isn't valid use of Lucene's APIs, but we
// still want to be robust to this case:
#[test]
fn test_close_with_threads() -> Result<()> {
  const NUM_THREADS: usize = 3;
  let num_iterations = if is_night_mode() { 7 } else { 3 };
  let mut random = random();
  for _ in 0..num_iterations {
    let dir = new_directory_shared(&mut random)?;

    let analyzer = MockAnalyzer::new(&mut random);
    let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
    config.set_max_buffered_docs(10);
    let merge_scheduler = ConcurrentMergeScheduler::new();
    merge_scheduler.set_suppress_exceptions();
    config.set_merge_scheduler(merge_scheduler);
    config.set_merge_policy(new_log_merge_policy_with_merge_factor(&mut random, 4)?);
    config.set_commit_on_close(false);
    let writer = IndexWriter::new(dir.clone(), config)?;

    let sync_start = Barrier::new(NUM_THREADS + 1);
    let add_counts = (0..NUM_THREADS)
      .map(|_| AtomicUsize::new(0))
      .collect::<Vec<_>>();
    thread::scope(|scope| -> Result<()> {
      let mut threads = Vec::new();
      for add_count in &add_counts {
        let writer = &writer;
        let sync_start = &sync_start;
        threads.push(scope.spawn(move || indexer_thread(writer, false, sync_start, add_count)));
      }
      sync_start.wait();

      let mut done = false;
      while !done {
        thread::sleep(Duration::from_millis(100));
        for (thread, add_count) in threads.iter().zip(&add_counts) {
          // only stop when at least one thread has added a doc
          if add_count.load(Ordering::SeqCst) > 0 {
            done = true;
            break;
          } else if thread.is_finished() {
            return Err(LuceneError::illegal_state(
              "thread failed before indexing a single document",
            ));
          }
        }
      }

      let commit_result = writer.commit();
      let close_result = writer.close();
      close_result?;
      commit_result?;

      // Make sure threads that are adding docs are not hung:
      for thread in threads {
        // Without fix for LUCENE-1130: one of the threads will hang
        thread.join().expect("thread panicked")?;
      }
      Ok(())
    })?;

    // Quick test to make sure index is not corrupt:
    let reader = directory_reader::open(dir.clone())?;
    let mut tdocs = TestUtil::docs_with_reader(
      &mut random,
      &reader,
      "field",
      &BytesRef::from_string("aaa"),
      None,
      0,
    )?
    .ok_or_else(|| LuceneError::illegal_state("term field:aaa does not exist"))?;
    let mut count = 0;
    while tdocs.next_doc()? != NO_MORE_DOCS {
      count += 1;
    }
    assert!(count > 0);
    drop(tdocs);
    reader.close()?;
    drop(reader);
    drop(writer);

    let dir = Arc::try_unwrap(dir)
      .map_err(|_| LuceneError::illegal_state("directory still has outstanding references"))?;
    dir.close()?;
  }
  Ok(())
}

// Runs test, with multiple threads, using the specific
// failure to trigger an IOException
fn test_multiple_threads_failure<F>(mut failure: F) -> Result<()>
where
  F: Failure<MockDirectoryDelegate> + Clone + 'static,
{
  const NUM_THREADS: usize = 3;
  let mut random = random();

  for iter in 0..2 {
    if cfg!(feature = "test_log_verbose") {
      println!("TEST: iter={iter}");
    }
    let dir = Arc::new(new_mock_directory(&mut random)?);

    let analyzer = MockAnalyzer::new(&mut random);
    let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
    config.set_max_buffered_docs(2);
    let merge_scheduler = ConcurrentMergeScheduler::new();
    merge_scheduler.set_suppress_exceptions();
    config.set_merge_scheduler(merge_scheduler);
    config.set_merge_policy(new_log_merge_policy_with_merge_factor(&mut random, 4)?);
    config.set_commit_on_close(false);
    let writer = IndexWriter::new(dir.clone(), config)?;

    let sync_start = Barrier::new(NUM_THREADS + 1);
    let add_counts = (0..NUM_THREADS)
      .map(|_| AtomicUsize::new(0))
      .collect::<Vec<_>>();
    thread::scope(|scope| -> Result<()> {
      let mut threads = Vec::new();
      for add_count in &add_counts {
        let writer = &writer;
        let sync_start = &sync_start;
        threads.push(scope.spawn(move || indexer_thread(writer, true, sync_start, add_count)));
      }
      sync_start.wait();

      dir.fail_on(Box::new(failure.clone()));
      failure.set_do_fail();

      for thread in threads {
        thread.join().expect("thread panicked")?;
      }
      Ok(())
    })?;

    let success = match (|| -> Result<()> {
      writer.commit()?;
      writer.close()
    })() {
      Ok(()) => true,
      Err(LuceneError::AlreadyClosed(_)) => {
        // OK: abort closes the writer
        assert!(writer.is_deleter_closed()?);
        false
      },
      Err(error) if error.is_io_error() => {
        if cfg!(feature = "test_log_verbose") {
          eprintln!("{error:?}");
        }
        writer.rollback()?;
        failure.clear_do_fail();
        false
      },
      Err(error) => return Err(error),
    };
    if cfg!(feature = "test_log_verbose") {
      println!("TEST: success={success}");
    }

    if success {
      let reader = directory_reader::open(dir.clone())?;
      let del_docs = multi_bits::get_live_docs(&reader)?;
      let mut stored_fields = reader.stored_fields()?;
      let mut term_vectors = reader.term_vectors()?;
      for j in 0..reader.max_doc()? {
        if match &del_docs {
          Some(del_docs) => !del_docs.get(j as usize)?,
          None => true,
        } {
          stored_fields.document(j)?;
          term_vectors.get(j)?;
        }
      }
      reader.close()?;
    }

    dir.as_ref().close()?;
  }
  Ok(())
}

// Runs test, with one thread, using the specific failure
// to trigger an IOException
fn test_single_thread_failure<F>(mut failure: F) -> Result<()>
where
  F: Failure<MockDirectoryDelegate> + Clone + 'static,
{
  let mut random = random();
  let dir = Arc::new(new_mock_directory(&mut random)?);

  let analyzer = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  config.set_max_buffered_docs(2);
  let merge_scheduler =
    ConcurrentMergeScheduler::with_hook(ConcurrentMergeSchedulerHook::Suppressing(
      SuppressingConcurrentMergeScheduler::writer_closed_or_tragic(),
    ));
  config.set_merge_scheduler(merge_scheduler);
  config.set_commit_on_close(false);
  let writer = IndexWriter::new(dir.clone(), config)?;
  let mut doc = Document::new();
  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?;
  custom_type.set_store_term_vectors(true)?;
  custom_type.set_store_term_vector_positions(true)?;
  custom_type.set_store_term_vector_offsets(true)?;
  doc.add(Field::new(
    "field",
    "aaa bbb ccc ddd eee fff ggg hhh iii jjj",
    custom_type,
  ));

  for _ in 0..6 {
    writer.add_document(doc.clone())?;
  }

  dir.fail_on(Box::new(failure.clone()));
  failure.set_do_fail();
  let result = (|| -> Result<()> {
    writer.add_document(doc.clone())?;
    writer.add_document(doc.clone())?;
    writer.commit()?;
    Ok(())
  })();
  assert!(
    matches!(&result, Err(error) if error.is_io_error()),
    "expected IOException, got {result:?}"
  );

  failure.clear_do_fail();
  let result = (|| -> Result<()> {
    writer.add_document(doc)?;
    writer.commit()?;
    writer.close()
  })();
  assert!(
    matches!(result, Err(LuceneError::AlreadyClosed(_))),
    "expected AlreadyClosed, got {result:?}"
  );

  assert!(writer.is_deleter_closed()?);
  dir.as_ref().close()?;
  Ok(())
}

// Throws IOException during FieldsWriter.flushDocument and during DocumentsWriter.abort
#[derive(Clone)]
struct FailOnlyOnAbortOrFlush {
  only_once: bool,
  do_fail: Arc<AtomicBool>,
}

impl FailOnlyOnAbortOrFlush {
  fn new(only_once: bool) -> Self {
    Self {
      only_once,
      do_fail: Arc::new(AtomicBool::new(false)),
    }
  }
}

impl<D> Failure<D> for FailOnlyOnAbortOrFlush
where
  D: Directory,
{
  fn eval(&mut self, dir: &MockDirectoryWrapper<D>) -> Result<()> {
    // Since we throw exc during abort, eg when IW is
    // attempting to delete files, we will leave
    // leftovers:
    dir.set_assert_no_unrefenced_files_on_close(false);

    if self.do_fail.load(Ordering::SeqCst)
      && call_stack_contains_any_of(&["abort", "finish_document"])
      && !call_stack_contains_any_of(&["merge", "close"])
    {
      if self.only_once {
        self.do_fail.store(false, Ordering::SeqCst);
      }
      return Err(LuceneError::io(Error::other("now failing on purpose")));
    }
    Ok(())
  }

  fn set_do_fail(&mut self) {
    self.do_fail.store(true, Ordering::SeqCst);
  }

  fn clear_do_fail(&mut self) {
    self.do_fail.store(false, Ordering::SeqCst);
  }
}

// LUCENE-1130: make sure initial IOException, and then 2nd
// IOException during rollback(), is OK:
#[test]
fn test_io_exception_during_abort() -> Result<()> {
  test_single_thread_failure(FailOnlyOnAbortOrFlush::new(false))
}

// LUCENE-1130: make sure initial IOException, and then 2nd
// IOException during rollback(), is OK:
#[test]
fn test_io_exception_during_abort_only_once() -> Result<()> {
  test_single_thread_failure(FailOnlyOnAbortOrFlush::new(true))
}

// LUCENE-1130: make sure initial IOException, and then 2nd
// IOException during rollback(), with multiple threads, is OK:
#[test]
fn test_io_exception_during_abort_with_threads() -> Result<()> {
  test_multiple_threads_failure(FailOnlyOnAbortOrFlush::new(false))
}

// LUCENE-1130: make sure initial IOException, and then 2nd
// IOException during rollback(), with multiple threads, is OK:
#[test]
fn test_io_exception_during_abort_with_threads_only_once() -> Result<()> {
  test_multiple_threads_failure(FailOnlyOnAbortOrFlush::new(true))
}

// Throws IOException during DocumentsWriter.writeSegment
#[derive(Clone)]
struct FailOnlyInWriteSegment {
  only_once: bool,
  enabled: Arc<AtomicBool>,
}

impl FailOnlyInWriteSegment {
  fn new(only_once: bool) -> Self {
    Self {
      only_once,
      enabled: Arc::new(AtomicBool::new(false)),
    }
  }
}

impl<D> Failure<D> for FailOnlyInWriteSegment
where
  D: Directory,
{
  fn eval(&mut self, _dir: &MockDirectoryWrapper<D>) -> Result<()> {
    if self.enabled.load(Ordering::SeqCst) && call_stack_contains::<IndexingChain<D>>("flush") {
      if self.only_once {
        self.enabled.store(false, Ordering::SeqCst);
      }
      return Err(LuceneError::io(Error::other("now failing on purpose")));
    }
    Ok(())
  }

  fn set_do_fail(&mut self) {
    self.enabled.store(true, Ordering::SeqCst);
  }

  fn clear_do_fail(&mut self) {
    self.enabled.store(false, Ordering::SeqCst);
  }
}

// LUCENE-1130: test IOException in writeSegment
#[test]
fn test_io_exception_during_write_segment() -> Result<()> {
  test_single_thread_failure(FailOnlyInWriteSegment::new(false))
}

// LUCENE-1130: test IOException in writeSegment
#[test]
fn test_io_exception_during_write_segment_only_once() -> Result<()> {
  test_single_thread_failure(FailOnlyInWriteSegment::new(true))
}

// LUCENE-1130: test IOException in writeSegment, with threads
#[test]
fn test_io_exception_during_write_segment_with_threads() -> Result<()> {
  test_multiple_threads_failure(FailOnlyInWriteSegment::new(false))
}

// LUCENE-1130: test IOException in writeSegment, with threads
#[test]
fn test_io_exception_during_write_segment_with_threads_only_once() -> Result<()> {
  test_multiple_threads_failure(FailOnlyInWriteSegment::new(true))
}

#[test]
fn test_open_two_index_writers_on_different_threads() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let sync_start = Arc::new(Barrier::new(2));

  let results = thread::scope(|scope| {
    let mut handles = Vec::new();
    for thread_id in 0..2 {
      let seed = random.random();
      let dir = dir.clone();
      let sync_start = sync_start.clone();
      handles.push(scope.spawn(move || -> Result<()> {
        let mut random = random_from_seed(seed);
        let mut doc = Document::new();
        doc.add(TextField::from_string("field", "testData", Store::Yes)?);

        sync_start.wait();
        if thread_id == 1 && random.random_bool(0.5) {
          thread::sleep(Duration::from_millis(100));
        }
        let analyzer = MockAnalyzer::new(&mut random);
        let writer = IndexWriter::new(
          dir,
          new_index_writer_config_with_analyzer(&mut random, analyzer)?,
        )?;
        writer.add_document(doc)?;
        writer.close()
      }));
    }

    handles
      .into_iter()
      .map(|handle| handle.join().expect("thread panicked"))
      .collect::<Vec<_>>()
  });

  if results
    .iter()
    .any(|result| matches!(result, Err(LuceneError::LockObtainFailed(_))))
  {
    return Ok(());
  }

  for result in results {
    result?;
  }

  let reader = directory_reader::open(dir)?;
  assert_eq!(2, reader.num_docs()?);
  reader.close()?;
  Ok(())
}

#[test]
fn test_rollback_and_commit_with_threads() -> Result<()> {
  let mut rng = random();
  let dir = new_directory_shared(&mut rng)?;
  let thread_count = TestUtil::next_int(&mut rng, 2, 6) as usize;

  let mut analyzer = MockAnalyzer::new(&mut rng);
  analyzer.set_max_token_length(TestUtil::next_int(&mut rng, 1, MAX_TERM_LENGTH));
  let writer = IndexWriter::new(
    dir.clone(),
    new_index_writer_config_with_analyzer(&mut rng, analyzer)?,
  )?;
  writer.commit()?;

  let writer_ref = Arc::new(Mutex::new(writer));
  let failed = Arc::new(AtomicBool::new(false));
  let rollback_lock = Arc::new(Mutex::new(()));
  let commit_lock = Arc::new(Mutex::new(()));
  let docs = Arc::new(Mutex::new(LineFileDocs::new(&mut rng)?));
  let iters = at_least(&mut rng, 100);

  thread::scope(|scope| -> Result<()> {
    let mut handles = Vec::new();
    for _ in 0..thread_count {
      let seed = rng.random();
      let dir = dir.clone();
      let writer_ref = writer_ref.clone();
      let failed = failed.clone();
      let rollback_lock = rollback_lock.clone();
      let commit_lock = commit_lock.clone();
      let docs = docs.clone();
      handles.push(scope.spawn(move || -> Result<()> {
        let mut random = random_from_seed(seed);

        for _ in 0..iters {
          if failed.load(Ordering::SeqCst) {
            break;
          }

          let result = match random.random_range(0..3) {
            0 => {
              let _rollback_guard = rollback_lock.lock();
              let writer = writer_ref.lock().clone();
              writer.rollback()?;

              let analyzer = MockAnalyzer::new(&mut random);
              let new_writer = IndexWriter::new(
                dir.clone(),
                new_index_writer_config_with_analyzer(&mut random, analyzer)?,
              )?;
              *writer_ref.lock() = new_writer;
              Ok(())
            },
            1 => {
              let _commit_guard = commit_lock.lock();
              let writer = writer_ref.lock().clone();
              let result = (|| -> Result<()> {
                if random.random_bool(0.5) {
                  writer.prepare_commit()?;
                }
                writer.commit()?;
                Ok(())
              })();
              match result {
                Ok(()) | Err(LuceneError::AlreadyClosed(_)) => Ok(()),
                Err(error) => Err(error),
              }
            },
            2 => {
              let writer = writer_ref.lock().clone();
              let doc = docs.lock().next_doc()?;
              match writer.add_document(doc) {
                Ok(_) | Err(LuceneError::AlreadyClosed(_)) => Ok(()),
                Err(error) => Err(error),
              }
            },
            _ => unreachable!(),
          };

          if let Err(error) = result {
            failed.store(true, Ordering::SeqCst);
            return Err(error);
          }
        }
        Ok(())
      }));
    }

    for handle in handles {
      handle.join().expect("thread panicked")?;
    }
    Ok(())
  })?;

  assert!(!failed.load(Ordering::SeqCst));
  writer_ref.lock().close()?;
  Ok(())
}

#[test]
fn test_update_single_doc_with_threads() -> Result<()> {
  let mut random = random();
  let force_merge = rarely(&mut random);
  stress_update_single_doc_with_threads(&mut random, false, force_merge)
}
#[test]
fn test_soft_update_single_doc_with_threads() -> Result<()> {
  let mut random = random();
  let force_merge = rarely(&mut random);
  stress_update_single_doc_with_threads(&mut random, true, force_merge)
}

fn stress_update_single_doc_with_threads<R>(
  random: &mut R,
  use_soft_deletes: bool,
  force_merge: bool,
) -> Result<()>
where
  R: Rng + ?Sized,
{
  let dir = new_directory_shared(random)?;
  let analyzer = MockAnalyzer::new(random);
  let mut config = new_index_writer_config_with_analyzer(random, analyzer)?;
  config
    .set_max_buffered_docs(-1)
    .set_ram_buffer_size_mb(0.00001);
  let writer = Arc::new(RandomIndexWriter::with_soft_deletes(
    random,
    dir.clone(),
    config,
    use_soft_deletes,
  ));
  let num_threads = if is_night_mode() {
    3 + random.random_range(0..3)
  } else {
    3
  };
  let done = Arc::new(AtomicUsize::new(0));
  let barrier = Arc::new(Barrier::new(num_threads + 1));

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "1", Store::No)?);
  writer.update_document_with_term(random, Term::from_text("id", "1"), doc)?;

  let iters_per_thread = 100 + random.random_range(0..2000);
  thread::scope(|scope| -> Result<()> {
    let mut handles = Vec::new();
    for _ in 0..num_threads {
      let writer = writer.clone();
      let done = done.clone();
      let barrier = barrier.clone();
      let seed = random.random();
      handles.push(scope.spawn(move || -> Result<()> {
        let mut thread_random = random_from_seed(seed);
        barrier.wait();
        let result = (|| -> Result<()> {
          for _ in 0..iters_per_thread {
            let mut d = Document::new();
            d.add(StringField::from_string("id", "1", Store::No)?);
            writer.update_document_with_term(&mut thread_random, Term::from_text("id", "1"), d)?;
          }
          Ok(())
        })();
        done.fetch_add(1, Ordering::SeqCst);
        result
      }));
    }

    let mut open = directory_reader::open_from_writer(&writer.w)?;
    assert_eq!(1, open.num_docs()?);
    barrier.wait();
    while done.load(Ordering::SeqCst) < num_threads {
      if force_merge && random.random_bool(0.5) {
        writer.force_merge(random, 1)?;
      }
      if let Some(new_open) = directory_reader::open_if_changed(&open)? {
        open.close()?;
        open = new_open;
      }
      assert_eq!(1, open.num_docs()?);
    }
    open.close()?;

    for handle in handles {
      handle.join().expect("thread panicked")?;
    }
    Ok(())
  })?;

  IOUtils::use_or_suppress_result(writer.close(random), dir.close())
}
