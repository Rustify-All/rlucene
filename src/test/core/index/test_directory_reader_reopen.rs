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
use crate::core::index::composite_reader::CompositeReader;
use crate::core::index::index_commit::IndexCommit;
use crate::core::index::index_reader::{CacheHelper, IndexReader};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::{IndexWriterConfig, OpenMode};
use crate::core::index::indexable_field::IndexableField;
use crate::core::index::keep_only_last_commit_deletion_policy::KeepOnlyLastCommitDeletionPolicy;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::log_merge_policy::LogMergePolicy;
use crate::core::index::multi_doc_values::MultiDocValues;
use crate::core::index::no_deletion_policy::NoDeletionPolicy;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::postings_enum::{ALL, PostingsEnum};
use crate::core::index::serial_merge_scheduler::SerialMergeScheduler;
use crate::core::index::snapshot_deletion_policy::SnapshotDeletionPolicy;
use crate::core::index::standard_directory_reader::StandardDirectoryReader;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::index::{directory_reader, field_infos, multi_bits, multi_terms};
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, NO_MORE_DOCS};
use crate::core::search::index_searcher::{self, IndexSearcher};
use crate::core::search::term_query::TermQuery;
use crate::core::store::ByteBuffersDirectory;
use crate::core::store::directory::Directory;
use crate::core::util::bits::Bits;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::dummy::dummy_comparator::DummyComparator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::store::mock_directory_wrapper::{
  Failure, FakeIOException, MockDirectoryWrapper,
};
use crate::test_framework::core::util::lucene_test_case::{
  call_stack_contains_any_of, get_only_leaf_reader, new_directory_shared, new_index_writer_config,
  new_index_writer_config_with_analyzer, new_log_merge_policy,
  new_log_merge_policy_with_merge_factor, new_mock_directory, new_string_field, random,
  random_from_seed,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::RngExt;
use rand_chacha::rand_core::Rng;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

#[allow(dead_code)] // for quick search
struct TestDirectoryReaderReopen;

#[test]
fn test_reopen() -> Result<()> {
  let mut random = random();
  let dir1 = new_directory_shared(&mut random)?;

  let iw = create_index(&mut random, dir1.clone(), false)?;
  let test = DefaultTestReopen::new(dir1.clone());
  perform_default_tests(&mut random, &test, iw)?;

  let dir2 = new_directory_shared(&mut random)?;

  let iw = create_index(&mut random, dir2.clone(), true)?;
  let test = DefaultTestReopen::new(dir2.clone());
  perform_default_tests(&mut random, &test, iw)?;

  Ok(())
}

#[test]
fn test_commit_reopen() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  do_test_reopen_with_commit(&mut random, dir, true)?;
  Ok(())
}

#[test]
fn test_commit_recreate() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  do_test_reopen_with_commit(&mut random, dir, false)?;
  Ok(())
}

fn do_test_reopen_with_commit<R, D>(random: &mut R, dir: Arc<D>, with_reopen: bool) -> Result<()>
where
  R: rand::Rng + ?Sized,
  D: Directory + 'static,
{
  let analyzer = MockAnalyzer::new(random);
  let mut config = new_index_writer_config_with_analyzer(random, analyzer)?;
  config.set_open_mode(OpenMode::Create);
  config.set_merge_scheduler(SerialMergeScheduler::new());
  config.set_merge_policy(new_log_merge_policy(random)?);
  let iwriter = IndexWriter::new(dir.clone(), config)?;
  iwriter.commit()?;
  let mut reader = directory_reader::open(dir.clone())?;

  let m = 3;
  let mut custom_type = FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?;
  custom_type.set_tokenized(false)?;
  let mut custom_type2 = FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?;
  custom_type2.set_tokenized(false)?;
  custom_type2.set_omit_norms(true)?;
  let mut custom_type3 = FieldType::new();
  custom_type3.set_stored(true)?;

  for i in 0..4 {
    for j in 0..m {
      let mut doc = Document::new();
      doc.add(Field::from_string(
        "id",
        format!("{i}_{j}"),
        custom_type.clone(),
      )?);
      doc.add(Field::from_string(
        "id2",
        format!("{i}_{j}"),
        custom_type2.clone(),
      )?);
      doc.add(Field::from_string(
        "id3",
        format!("{i}_{j}"),
        custom_type3.clone(),
      )?);
      iwriter.add_document(doc)?;
      if i > 0 {
        let k = i - 1;
        let n = j + k * m;
        let mut stored_fields = reader.stored_fields()?;
        let previous_iteration_doc = stored_fields.document(n)?;
        let id = previous_iteration_doc.get("id")?;
        assert_eq!(Some(format!("{k}_{j}")), id.map(|value| value.into_owned()));
      }
    }
    iwriter.commit()?;
    if with_reopen {
      if let Some(v) = directory_reader::open_if_changed(&reader)? {
        reader.close()?;
        reader = v;
      }
    } else {
      reader.close()?;
      reader = directory_reader::open(dir.clone())?;
    }
  }

  iwriter.close()?;
  reader.close()?;
  Ok(())
}

#[test]
fn test_thread_safety() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  // NOTE: this also controls the number of threads!
  let n = TestUtil::next_int(&mut random, 20, 40);

  let analyzer = MockAnalyzer::new(&mut random);
  let writer = IndexWriter::new(
    dir.clone(),
    new_index_writer_config_with_analyzer(&mut random, analyzer)?,
  )?;
  for i in 0..n {
    writer.add_document(create_document(i, 3)?)?;
  }
  writer.force_merge(1)?;
  writer.close()?;

  let test = ThreadSafetyTestReopen::new(dir.clone(), n);
  let readers = Arc::new(Mutex::new(Vec::new()));
  let first_reader = Arc::new(directory_reader::open(dir.clone())?);
  let mut reader = first_reader.clone();

  let readers_to_close = Arc::new(Mutex::new(Vec::new()));
  let create_reader_mutex = Arc::new(Mutex::new(()));

  thread::scope(|scope| -> Result<()> {
    let mut threads = Vec::new();
    let mut spawn_error = None;

    for i in 0..n {
      if i % 2 == 0 {
        match directory_reader::open_if_changed(reader.as_ref()) {
          Ok(Some(refreshed)) => {
            {
              let mut readers_to_close = readers_to_close
                .lock()
                .expect("readersToClose mutex poisoned");
              if !readers_to_close
                .iter()
                .any(|existing| Arc::ptr_eq(existing, &reader))
              {
                readers_to_close.push(reader.clone());
              }
            }
            reader = Arc::new(refreshed);
          },
          Ok(None) => {},
          Err(err) => {
            spawn_error = Some(err);
            break;
          },
        }
      }

      let r = reader.clone();
      let index = i;
      let task = Arc::new(ReaderThreadTask::new());

      if i < 4 || (10..14).contains(&i) || i > 18 {
        let readers = readers.clone();
        let readers_to_close = readers_to_close.clone();
        let create_reader_mutex = create_reader_mutex.clone();
        let test = &test;
        let seed = random.random();
        let task_for_thread = task.clone();
        threads.push(ReaderThread::new(
          i,
          scope.spawn(move || -> Result<()> {
            let mut random = random_from_seed(seed);
            while !task_for_thread.stopped.load(Ordering::SeqCst) {
              if index % 2 == 0 {
                // refresh reader synchronized
                let c = {
                  let _guard = create_reader_mutex
                    .lock()
                    .expect("createReaderMutex mutex poisoned");
                  let (c, _) =
                    refresh_reader_with_test(&mut random, &r, Some(test), index, true, None)?;
                  c
                };
                let c = Arc::new(c);
                {
                  let mut readers_to_close = readers_to_close
                    .lock()
                    .expect("readersToClose mutex poisoned");
                  let new_reader = c
                    .new_reader
                    .as_ref()
                    .expect("newReader must be set")
                    .clone();
                  if !readers_to_close
                    .iter()
                    .any(|existing| Arc::ptr_eq(existing, &new_reader))
                  {
                    readers_to_close.push(new_reader);
                  }
                  if !readers_to_close
                    .iter()
                    .any(|existing| Arc::ptr_eq(existing, &c.refreshed_reader))
                  {
                    readers_to_close.push(c.refreshed_reader.clone());
                  }
                }
                readers.lock().expect("readers mutex poisoned").push(c);
                // prevent too many readers
                break;
              } else {
                // not synchronized
                let refreshed = directory_reader::open_if_changed(r.as_ref())?
                  .map(Arc::new)
                  .unwrap_or_else(|| r.clone());

                let searcher = index_searcher::from_reader(refreshed.clone())?;
                let max_doc = refreshed.max_doc()?;
                let hits = searcher
                  .search(
                    TermQuery::new(Term::from_text(
                      "field1",
                      format!("a{}", TestUtil::next_int(&mut random, 0, max_doc - 1)),
                    )),
                    1000,
                  )?
                  .score_docs;
                if let Some(hit) = hits.first() {
                  searcher.stored_fields()?.document(hit.doc)?;
                }
                if !Arc::ptr_eq(&refreshed, &r) {
                  refreshed.close()?;
                }
              }
              {
                let guard = task_for_thread
                  .monitor
                  .lock()
                  .expect("ReaderThreadTask monitor poisoned");
                let (_guard, _) = task_for_thread
                  .cvar
                  .wait_timeout(
                    guard,
                    Duration::from_millis(TestUtil::next_int(&mut random, 1, 100) as u64),
                  )
                  .expect("ReaderThreadTask monitor poisoned");
              }
            }
            Ok(())
          }),
          task,
        ));
      } else {
        let readers = readers.clone();
        let seed = random.random();
        let task_for_thread = task.clone();
        threads.push(ReaderThread::new(
          i,
          scope.spawn(move || -> Result<()> {
            let mut random = random_from_seed(seed);
            while !task_for_thread.stopped.load(Ordering::SeqCst) {
              let c = {
                let readers = readers.lock().expect("readers mutex poisoned");
                if readers.is_empty() {
                  None
                } else {
                  let index = TestUtil::next_int(&mut random, 0, readers.len() as i32 - 1) as usize;
                  Some(readers[index].clone())
                }
              };
              if let Some(c) = c {
                assert_index_equals(
                  c.new_reader
                    .as_ref()
                    .expect("newReader must be set")
                    .as_ref(),
                  c.refreshed_reader.as_ref(),
                )?;
              }

              {
                let guard = task_for_thread
                  .monitor
                  .lock()
                  .expect("ReaderThreadTask monitor poisoned");
                let (_guard, _) = task_for_thread
                  .cvar
                  .wait_timeout(
                    guard,
                    Duration::from_millis(TestUtil::next_int(&mut random, 1, 100) as u64),
                  )
                  .expect("ReaderThreadTask monitor poisoned");
              }
            }
            Ok(())
          }),
          task,
        ));
      }
    }

    if spawn_error.is_none() {
      let lock = Mutex::new(());
      let cvar = Condvar::new();
      let guard = lock
        .lock()
        .expect("TestDirectoryReaderReopen monitor poisoned");
      let (_guard, _) = cvar
        .wait_timeout(guard, Duration::from_millis(1000))
        .expect("TestDirectoryReaderReopen monitor poisoned");
    }

    for thread in &threads {
      thread.stop_thread();
    }

    for thread in threads {
      let name = thread.name.clone();
      match thread.join() {
        Ok(Ok(())) => {},
        Ok(Err(err)) => {
          return Err(LuceneError::illegal_state(format!(
            "Error occurred in thread {name}:\n{err}"
          )));
        },
        Err(_) => {
          return Err(LuceneError::illegal_state(format!(
            "Error occurred in thread {name}: thread panicked"
          )));
        },
      }
    }

    if let Some(err) = spawn_error {
      return Err(err);
    }

    Ok(())
  })?;

  let readers_to_close_snapshot = readers_to_close
    .lock()
    .expect("readersToClose mutex poisoned")
    .clone();

  for reader_to_close in &readers_to_close_snapshot {
    reader_to_close.close()?;
  }

  first_reader.close()?;
  reader.close()?;

  for reader_to_close in &readers_to_close_snapshot {
    assert_reader_closed(reader_to_close.as_ref(), true);
  }

  assert_reader_closed(reader.as_ref(), true);
  assert_reader_closed(first_reader.as_ref(), true);

  dir.close()?;
  Ok(())
}

struct ReaderCouple<D>
where
  D: Directory,
{
  new_reader: Option<Arc<StandardDirectoryReader<D>>>,
  refreshed_reader: Arc<StandardDirectoryReader<D>>,
}

impl<D> ReaderCouple<D>
where
  D: Directory,
{
  fn new(
    new_reader: Option<Arc<StandardDirectoryReader<D>>>,
    refreshed_reader: Arc<StandardDirectoryReader<D>>,
  ) -> Self {
    Self {
      new_reader,
      refreshed_reader,
    }
  }
}

struct ReaderThreadTask {
  stopped: AtomicBool,
  monitor: Mutex<()>,
  cvar: Condvar,
}

impl ReaderThreadTask {
  fn new() -> Self {
    Self {
      stopped: AtomicBool::new(false),
      monitor: Mutex::new(()),
      cvar: Condvar::new(),
    }
  }

  fn stop(&self) {
    self.stopped.store(true, Ordering::SeqCst);
  }
}

struct ReaderThread<'scope> {
  task: Arc<ReaderThreadTask>,
  handle: thread::ScopedJoinHandle<'scope, Result<()>>,
  name: String,
}

impl<'scope> ReaderThread<'scope> {
  fn new(
    name: i32,
    handle: thread::ScopedJoinHandle<'scope, Result<()>>,
    task: Arc<ReaderThreadTask>,
  ) -> Self {
    Self {
      task,
      handle,
      name: name.to_string(),
    }
  }

  fn stop_thread(&self) {
    self.task.stop();
  }

  fn join(self) -> std::thread::Result<Result<()>> {
    self.handle.join()
  }
}

trait TestReopen<D>
where
  D: Directory,
{
  fn open_reader(&self) -> Result<StandardDirectoryReader<D>>;

  fn modify_index<R>(&self, random: &mut R, i: i32) -> Result<Arc<IndexWriter<D>>>
  where
    R: Rng + ?Sized;
}

struct DefaultTestReopen<D>
where
  D: Directory,
{
  dir: Arc<D>,
}

impl<D> DefaultTestReopen<D>
where
  D: Directory + 'static,
{
  fn new(dir: Arc<D>) -> Self {
    Self { dir }
  }
}

impl<D> TestReopen<D> for DefaultTestReopen<D>
where
  D: Directory + 'static,
{
  fn open_reader(&self) -> Result<StandardDirectoryReader<D>> {
    directory_reader::open(self.dir.clone())
  }

  fn modify_index<R>(&self, random: &mut R, i: i32) -> Result<Arc<IndexWriter<D>>>
  where
    R: Rng + ?Sized,
  {
    modify_index(random, i, self.dir.clone())
  }
}

struct ThreadSafetyTestReopen<D>
where
  D: Directory,
{
  dir: Arc<D>,
  n: i32,
}

impl<D> ThreadSafetyTestReopen<D>
where
  D: Directory + 'static,
{
  fn new(dir: Arc<D>, n: i32) -> Self {
    Self { dir, n }
  }
}

impl<D> TestReopen<D> for ThreadSafetyTestReopen<D>
where
  D: Directory + 'static,
{
  fn open_reader(&self) -> Result<StandardDirectoryReader<D>> {
    directory_reader::open(self.dir.clone())
  }

  fn modify_index<R>(&self, random: &mut R, i: i32) -> Result<Arc<IndexWriter<D>>>
  where
    R: Rng + ?Sized,
  {
    let analyzer = MockAnalyzer::new(random);
    let modifier = IndexWriter::new(
      self.dir.clone(),
      IndexWriterConfig::with_analyzer(analyzer)?,
    )?;
    modifier.add_document(create_document(self.n + i, 6)?)?;
    modifier.close()?;
    Ok(modifier)
  }
}

fn perform_default_tests<R, D, T>(random: &mut R, test: &T, iw: Arc<IndexWriter<D>>) -> Result<()>
where
  D: Directory + 'static,
  R: Rng + ?Sized,
  T: TestReopen<D>,
{
  let mut index1 = Arc::new(test.open_reader()?);
  let mut index2 = Arc::new(test.open_reader()?);

  assert_index_equals(index1.as_ref(), index2.as_ref())?;

  // verify that reopen() does not return a new reader instance
  // in case the index has no changes
  let (couple, iw) = refresh_reader(random, &index2, false, iw)?;
  if !Arc::ptr_eq(&couple.refreshed_reader, &index2) {
    panic!("New DirectoryReader instance created during refresh even though index had no changes.");
  }

  let (couple, iw) = refresh_reader_with_test(random, &index2, Some(test), 0, true, Some(iw))?;
  index1.close()?;
  index1 = couple.new_reader.unwrap();

  let index2_refreshed = couple.refreshed_reader;
  if Arc::ptr_eq(&index2_refreshed, &index2) {
    panic!("No new DirectoryReader instance created during refresh.");
  }
  index2.close()?;

  // test if refreshed reader and newly opened reader return equal results
  assert_index_equals(index1.as_ref(), index2_refreshed.as_ref())?;

  index2_refreshed.close()?;
  assert_reader_closed(index2.as_ref(), true);
  assert_reader_closed(index2_refreshed.as_ref(), true);

  index2 = Arc::new(test.open_reader()?);
  let mut writer = iw;
  for i in 1..4 {
    index1.close()?;
    let (couple, iw) =
      refresh_reader_with_test(random, &index2, Some(test), i, true, Some(writer))?;
    writer = iw;
    // refresh DirectoryReader
    index2.close()?;

    index2 = couple.refreshed_reader;
    index1 = couple.new_reader.unwrap();
    assert_index_equals(index1.as_ref(), index2.as_ref())?;
  }

  index1.close()?;
  index2.close()?;
  assert_reader_closed(index1.as_ref(), true);
  assert_reader_closed(index2.as_ref(), true);
  Ok(())
}

fn refresh_reader<R, D>(
  random: &mut R,
  reader: &Arc<StandardDirectoryReader<D>>,
  has_changes: bool,
  iw: Arc<IndexWriter<D>>,
) -> Result<(ReaderCouple<D>, Arc<IndexWriter<D>>)>
where
  D: Directory + 'static,
  R: Rng + ?Sized,
{
  refresh_reader_with_test::<R, D, DefaultTestReopen<D>>(
    random,
    reader,
    None,
    -1,
    has_changes,
    Some(iw),
  )
}

fn refresh_reader_with_test<R, D, T>(
  random: &mut R,
  reader: &Arc<StandardDirectoryReader<D>>,
  test: Option<&T>,
  modify: i32,
  has_changes: bool,
  iw: Option<Arc<IndexWriter<D>>>,
) -> Result<(ReaderCouple<D>, Arc<IndexWriter<D>>)>
where
  D: Directory + 'static,
  R: Rng + ?Sized,
  T: TestReopen<D>,
{
  let mut r = None;
  let iw = if let Some(test) = test {
    let iw = test.modify_index(random, modify)?;
    r = Some(Arc::new(test.open_reader()?));
    iw
  } else {
    iw.ok_or_else(|| LuceneError::illegal_state("missing IndexWriter for refreshReader"))?
  };

  let refreshed_reader = match directory_reader::open_if_changed(reader) {
    Ok(Some(refreshed)) => Arc::new(refreshed),
    Ok(None) => reader.clone(),
    Err(err) => {
      if let Some(reader) = r.as_ref() {
        let _ = reader.close();
      }
      return Err(err);
    },
  };

  if has_changes {
    if Arc::ptr_eq(&refreshed_reader, reader) {
      panic!("No new DirectoryReader instance created during refresh.");
    }
  } else if !Arc::ptr_eq(&refreshed_reader, reader) {
    panic!("New DirectoryReader instance created during refresh even though index had no changes.");
  }

  Ok((ReaderCouple::new(r, refreshed_reader), iw))
}

fn create_index<R, D>(
  random: &mut R,
  dir: Arc<D>,
  multi_segment: bool,
) -> Result<Arc<IndexWriter<D>>>
where
  R: rand::Rng + ?Sized,
  D: Directory + 'static,
{
  let analyzer = MockAnalyzer::new(random);
  let mut config = new_index_writer_config_with_analyzer(random, analyzer)?;
  config.set_merge_policy(LogMergePolicy::log_doc());
  let writer = IndexWriter::new(dir.clone(), config)?;

  for i in 0..100 {
    writer.add_document(create_document(i, 4)?)?;
    if multi_segment && (i % 10) == 0 {
      writer.commit()?;
    }
  }

  if !multi_segment {
    writer.force_merge(1)?;
  }
  writer.close()?;

  let r = directory_reader::open(dir.clone())?;
  if multi_segment {
    assert!((&r).get_context()?.leaves()?.len() > 1);
  } else {
    assert_eq!(1, (&r).get_context()?.leaves()?.len());
  }
  r.close()?;

  Ok(writer)
}

fn modify_index<D, R>(random: &mut R, i: i32, dir: Arc<D>) -> Result<Arc<IndexWriter<D>>>
where
  D: Directory + 'static,
  R: Rng + ?Sized,
{
  match i {
    0 => {
      let analyzer = MockAnalyzer::new(random);
      let writer = IndexWriter::new(dir, IndexWriterConfig::with_analyzer(analyzer)?)?;
      writer.delete_documents_with_terms(vec![Term::from_text("field2", "a11")])?;
      writer.delete_documents_with_terms(vec![Term::from_text("field2", "b30")])?;
      writer.close()?;
      Ok(writer)
    },
    1 => {
      let analyzer = MockAnalyzer::new(random);
      let writer = IndexWriter::new(dir, IndexWriterConfig::with_analyzer(analyzer)?)?;
      writer.force_merge(1)?;
      writer.close()?;
      Ok(writer)
    },
    2 => {
      let analyzer = MockAnalyzer::new(random);
      let writer = IndexWriter::new(dir, IndexWriterConfig::with_analyzer(analyzer)?)?;
      writer.add_document(create_document(101, 4)?)?;
      writer.force_merge(1)?;
      writer.add_document(create_document(102, 4)?)?;
      writer.add_document(create_document(103, 4)?)?;
      writer.close()?;
      Ok(writer)
    },
    3 => {
      let analyzer = MockAnalyzer::new(random);
      let writer = IndexWriter::new(dir, IndexWriterConfig::with_analyzer(analyzer)?)?;
      writer.add_document(create_document(101, 4)?)?;
      writer.close()?;
      Ok(writer)
    },
    _ => {
      let analyzer = MockAnalyzer::new(random);
      let writer = IndexWriter::new(dir, IndexWriterConfig::with_analyzer(analyzer)?)?;
      writer.close()?;
      Ok(writer)
    },
  }
}

fn assert_reader_closed<D>(reader: &StandardDirectoryReader<D>, check_sub_readers: bool)
where
  D: Directory,
{
  assert_eq!(0, reader.get_ref_count());
  if check_sub_readers {
    for sub_reader in reader.get_sequential_sub_readers() {
      assert_eq!(0, sub_reader.get_ref_count());
    }
  }
}

fn assert_index_equals<D>(
  index1: &StandardDirectoryReader<D>,
  index2: &StandardDirectoryReader<D>,
) -> Result<()>
where
  D: Directory,
{
  assert_eq!(index1.num_docs()?, index2.num_docs()?);
  assert_eq!(index1.max_doc()?, index2.max_doc()?);
  assert_eq!(index1.has_deletions()?, index2.has_deletions()?);
  assert_eq!(
    index1.get_context()?.leaves()?.len() == 1,
    index2.get_context()?.leaves()?.len() == 1
  );

  let field_infos1 = field_infos::get_merged_field_infos(index1)?;
  let field_infos2 = field_infos::get_merged_field_infos(index2)?;
  assert_eq!(field_infos1.size(), field_infos2.size());
  for (field_info1, field_info2) in field_infos1.iter().zip(field_infos2.iter()) {
    assert_eq!(field_info1.name, field_info2.name);
  }

  for field_info in field_infos1.iter() {
    let cur_field = &field_info.name;
    let mut norms1 = MultiDocValues::get_norm_values(index1, cur_field)?;
    let mut norms2 = MultiDocValues::get_norm_values(index2, cur_field)?;
    if norms1.is_some() && norms2.is_some() {
      #[allow(clippy::unnecessary_unwrap)]
      let norms1 = norms1.as_mut().unwrap();
      #[allow(clippy::unnecessary_unwrap)]
      let norms2 = norms2.as_mut().unwrap();
      loop {
        let doc_id = norms1.next_doc()?;
        assert_eq!(doc_id, norms2.next_doc()?);
        if doc_id == NO_MORE_DOCS {
          break;
        }
        assert_eq!(norms1.long_value()?, norms2.long_value()?);
      }
    } else {
      assert!(norms1.is_none());
      assert!(norms2.is_none());
    }
  }

  let live_docs1 = multi_bits::get_live_docs(index1)?;
  let live_docs2 = multi_bits::get_live_docs(index2)?;
  for i in 0..index1.max_doc()? {
    assert_eq!(
      live_docs1
        .as_ref()
        .is_none_or(|live_docs| !live_docs.get(i as usize).expect("")),
      live_docs2
        .as_ref()
        .is_none_or(|live_docs| !live_docs.get(i as usize).expect("")),
      "Doc {} only deleted in one index.",
      i
    );
  }

  let mut stored_fields1 = index1.stored_fields()?;
  let mut stored_fields2 = index2.stored_fields()?;
  for i in 0..index1.max_doc()? {
    if live_docs1
      .as_ref()
      .is_none_or(|live_docs| live_docs.get(i as usize).expect(""))
    {
      let doc1 = stored_fields1.document(i)?;
      let doc2 = stored_fields2.document(i)?;
      assert_eq!(doc1.get_fields().len(), doc2.get_fields().len());
      for (field1, field2) in doc1.get_fields().iter().zip(doc2.get_fields().iter()) {
        assert_eq!(field1.name(), field2.name());
        assert_eq!(
          field1.string_value()?.map(|value| value.into_owned()),
          field2.string_value()?.map(|value| value.into_owned())
        );
      }
    }
  }

  let mut fields1: Vec<_> = field_infos::get_indexed_fields(index1)?
    .into_iter()
    .collect();
  let mut fields2: Vec<_> = field_infos::get_indexed_fields(index2)?
    .into_iter()
    .collect();
  fields1.sort();
  fields2.sort();
  let mut fenum2 = fields2.iter();
  for field1 in fields1 {
    assert_eq!(&field1, fenum2.next().unwrap());
    let terms1 = multi_terms::get_terms(index1, &field1)?;
    if terms1.is_none() {
      assert!(multi_terms::get_terms(index2, &field1)?.is_none());
      continue;
    }
    let terms1 = terms1.unwrap();
    let mut enum1 = terms1.iterator()?;

    let terms2 = multi_terms::get_terms(index2, &field1)?;
    assert!(terms2.is_some());
    let terms2 = terms2.unwrap();
    let mut enum2 = terms2.iterator()?;

    while enum1.next()?.is_some() {
      assert_eq!(enum1.term()?, enum2.next()?.unwrap());
      let mut tp1 = enum1.postings_with_flags(None, ALL as i32)?;
      let mut tp2 = enum2.postings_with_flags(None, ALL as i32)?;

      while tp1.next_doc()? != NO_MORE_DOCS {
        assert_ne!(NO_MORE_DOCS, tp2.next_doc()?);
        assert_eq!(tp1.doc_id(), tp2.doc_id());
        let freq = tp1.freq()?;
        assert_eq!(freq, tp2.freq()?);
        for _ in 0..freq {
          assert_eq!(tp1.next_position()?, tp2.next_position()?);
        }
      }
    }
  }
  assert!(fenum2.next().is_none());
  Ok(())
}

fn create_document(n: i32, num_fields: i32) -> Result<Document> {
  let mut value = format!("a{n}");
  let mut doc = Document::new();
  let mut custom_type2 = FieldType::from_ref(&*crate::core::document::text_field::TYPE_STORED)?;
  custom_type2.set_tokenized(false)?;
  custom_type2.set_omit_norms(true)?;
  let mut custom_type3 = FieldType::new();
  custom_type3.set_stored(true)?;
  doc.add(TextField::from_string("field1", value.clone(), Store::Yes)?);
  doc.add(Field::from_string("fielda", value.clone(), custom_type2)?);
  doc.add(Field::from_string("fieldb", value.clone(), custom_type3)?);
  value.push_str(&format!(" b{n}"));
  for i in 1..num_fields {
    doc.add(TextField::from_string(
      format!("field{}", i + 1),
      value.clone(),
      Store::Yes,
    )?);
  }
  Ok(doc)
}

#[test]
fn test_reopen_on_commit() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut field_to_type = HashMap::new();
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  iwc.set_index_deletion_policy(NoDeletionPolicy);
  iwc.set_max_buffered_docs(-1);
  iwc.set_merge_policy(new_log_merge_policy_with_merge_factor(&mut random, 10)?);
  let writer = IndexWriter::new(dir.clone(), iwc)?;
  for i in 0..4 {
    let mut doc = Document::new();
    doc.add(new_string_field(
      &mut random,
      "id",
      i.to_string(),
      Store::No,
      &mut field_to_type,
    )?);
    writer.add_document(doc)?;
    let mut data = HashMap::new();
    data.insert("index".to_string(), i.to_string());
    writer.set_live_commit_data(data);
    writer.commit()?;
  }
  for i in 0..4 {
    writer.delete_documents_with_terms(vec![Term::from_text("id", i.to_string())])?;
    let mut data = HashMap::new();
    data.insert("index".to_string(), (4 + i).to_string());
    writer.set_live_commit_data(data);
    writer.commit()?;
  }
  writer.close()?;

  let mut r = directory_reader::open(dir.clone())?;
  assert_eq!(0, r.num_docs()?);

  let commits = directory_reader::list_commits(dir.clone())?;
  for commit in &commits {
    let r2 = directory_reader::open_if_changed_with_commit(&r, Some(commit))?.unwrap();

    let s = commit.get_user_data();
    let v = if s.is_empty() {
      // First commit created by IW
      -1
    } else {
      s.get("index")
        .ok_or_else(|| LuceneError::illegal_state("missing commit index"))?
        .parse::<i32>()
        .map_err(|err| LuceneError::illegal_state(err.to_string()))?
    };
    if v < 4 {
      assert_eq!(1 + v, r2.num_docs()?);
    } else {
      assert_eq!(7 - v, r2.num_docs()?);
    }
    r.close()?;
    r = r2;
  }
  r.close()?;
  Ok(())
}

#[test]
fn test_open_if_changed_nrt_to_commit() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut field_to_type = HashMap::new();
  // Can't use RIW because it randomly commits:
  let analyzer = MockAnalyzer::new(&mut random);
  let w = IndexWriter::new(
    dir.clone(),
    new_index_writer_config_with_analyzer(&mut random, analyzer)?,
  )?;
  let mut doc = Document::new();
  doc.add(new_string_field(
    &mut random,
    "field",
    "value",
    Store::No,
    &mut field_to_type,
  )?);
  w.add_document(doc.clone())?;
  w.commit()?;
  let commits = directory_reader::list_commits(dir.clone())?;
  assert_eq!(1, commits.len());
  w.add_document(doc)?;
  let r = directory_reader::open_from_writer(&w)?;

  assert_eq!(2, r.num_docs()?);
  let r2 = directory_reader::open_if_changed_with_commit(&r, Some(&commits[0]))?.unwrap();
  r.close()?;
  assert_eq!(1, r2.num_docs()?);
  w.close()?;
  r2.close()?;
  Ok(())
}

#[test]
fn test_over_dec_ref_during_reopen() -> Result<()> {
  let mut random = random();
  let dir = Arc::new(new_mock_directory(&mut random)?);

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  iwc.set_merge_policy(NoMergePolicy::default());
  let w = IndexWriter::new(dir.clone(), iwc)?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "id", Store::No)?);
  w.add_document(doc)?;
  doc = Document::new();
  doc.add(StringField::from_string("id", "id2", Store::No)?);
  w.add_document(doc)?;
  w.commit()?;

  // Open reader w/ one segment w/ 2 docs:
  let r = directory_reader::open(dir.clone())?;

  // Delete 1 doc from the segment:
  // System.out.println("TEST: now delete");
  w.delete_documents_with_terms(vec![Term::from_text("id", "id")])?;
  // System.out.println("TEST: now commit");
  w.commit()?;

  // Fail when reopen tries to open the live docs file:
  dir.fail_on(Box::new(FailOnReadLiveDocs {
    do_fail: false,
    failed: false,
  }));

  // Now reopen:
  // System.out.println("TEST: now reopen");
  match directory_reader::open_if_changed(&r) {
    Ok(_) => panic!("expected FakeIOException"),
    Err(LuceneError::Io { source, .. }) | Err(LuceneError::IoWithPath { source, .. }) => {
      assert!(
        source
          .get_ref()
          .is_some_and(|source| source.is::<FakeIOException>()),
        "expected FakeIOException, got {source}"
      );
    },
    Err(err) => return Err(err),
  }

  let s = index_searcher::from_reader(r)?;
  assert_eq!(1, s.count(TermQuery::new(Term::from_text("id", "id")))?);

  s.get_index_reader().close()?;
  w.close()?;
  dir.as_ref().close()?;
  Ok(())
}

#[test]
fn test_npe_after_invalid_reindex1() -> Result<()> {
  let mut random = random();
  let dir = Arc::new(ByteBuffersDirectory::new());

  let analyzer = MockAnalyzer::new(&mut random);
  let mut config = IndexWriterConfig::with_analyzer(analyzer)?;
  config.set_merge_policy(NoMergePolicy::default());
  let mut w = IndexWriter::new(dir.clone(), config)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "id", Store::No)?);
  w.add_document(doc)?;

  doc = Document::new();
  doc.add(StringField::from_string("id", "id2", Store::No)?);
  w.add_document(doc)?;

  w.delete_documents_with_terms(vec![Term::from_text("id", "id")])?;
  w.commit()?;
  w.close()?;
  drop(w);

  let r = directory_reader::open(dir.clone())?;

  for file_name in dir.list_all()? {
    dir.delete_file(&file_name)?;
  }

  let analyzer = MockAnalyzer::new(&mut random);
  w = IndexWriter::new(dir.clone(), IndexWriterConfig::with_analyzer(analyzer)?)?;

  doc = Document::new();
  doc.add(StringField::from_string("id", "id", Store::No)?);
  doc.add(NumericDocValuesField::new("ndv", 13));
  w.add_document(doc)?;

  doc = Document::new();
  doc.add(StringField::from_string("id", "id2", Store::No)?);
  w.add_document(doc)?;

  w.commit()?;

  doc = Document::new();
  doc.add(StringField::from_string("id", "id2", Store::No)?);
  w.add_document(doc)?;

  w.update_numeric_doc_value(Term::from_text("id", "id"), "ndv", 17)?;
  w.commit()?;
  w.close()?;

  let err = directory_reader::open_if_changed(&r);
  assert!(matches!(err, Err(LuceneError::IllegalState(_))));

  r.close()?;
  Ok(())
}

#[test]
fn test_npe_after_invalid_reindex2() -> Result<()> {
  let mut random = random();
  let dir = Arc::new(ByteBuffersDirectory::new());

  let analyzer = MockAnalyzer::new(&mut random);
  let mut config = IndexWriterConfig::with_analyzer(analyzer)?;
  config.set_merge_policy(NoMergePolicy::default());
  let mut w = IndexWriter::new(dir.clone(), config)?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "id", Store::No)?);
  w.add_document(doc)?;
  doc = Document::new();
  doc.add(StringField::from_string("id", "id2", Store::No)?);
  w.add_document(doc)?;
  w.delete_documents_with_terms(vec![Term::from_text("id", "id")])?;
  w.commit()?;
  w.close()?;
  drop(w);

  let r = directory_reader::open(dir.clone())?;

  for name in dir.list_all()? {
    dir.delete_file(&name)?;
  }

  let analyzer = MockAnalyzer::new(&mut random);
  w = IndexWriter::new(dir.clone(), IndexWriterConfig::with_analyzer(analyzer)?)?;
  doc = Document::new();
  doc.add(StringField::from_string("id", "id", Store::No)?);
  doc.add(NumericDocValuesField::new("ndv", 13));
  w.add_document(doc)?;
  w.commit()?;
  doc = Document::new();
  doc.add(StringField::from_string("id", "id2", Store::No)?);
  w.add_document(doc)?;
  w.commit()?;
  w.close()?;

  let err = directory_reader::open_if_changed(&r);
  assert!(matches!(err, Err(LuceneError::IllegalState(_))));

  r.close()?;
  Ok(())
}

#[test]
fn test_nrt_mdeletes() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  iwc.set_merge_policy(NoMergePolicy::default());
  let snapshotter = SnapshotDeletionPolicy::new(KeepOnlyLastCommitDeletionPolicy);
  iwc.set_index_deletion_policy(snapshotter.clone());
  let writer = IndexWriter::new(dir.clone(), iwc)?;
  writer.commit()?; // make sure all index metadata is written out

  let mut doc = Document::new();
  doc.add(StringField::from_string("key", "value1", Store::Yes)?);
  writer.add_document(doc)?;

  doc = Document::new();
  doc.add(StringField::from_string("key", "value2", Store::Yes)?);
  writer.add_document(doc)?;

  writer.commit()?;

  let ic1 = snapshotter.snapshot()?;

  doc = Document::new();
  doc.add(StringField::from_string("key", "value3", Store::Yes)?);
  writer.update_document_with_term(Term::from_text("key", "value1"), doc)?;

  writer.commit()?;

  let ic2 = snapshotter.snapshot()?;
  let latest = directory_reader::open_from_commit(&ic2)?;
  assert_eq!(2, (&latest).get_context()?.leaves()?.len());

  // This reader will be used for searching against commit point 1
  let oldest = directory_reader::open_if_changed_with_commit(&latest, Some(&ic1))?
    .expect("reader should change");
  assert_eq!(1, (&oldest).get_context()?.leaves()?.len());

  // sharing same core
  assert_eq!(
    (&latest).get_context()?.leaves()?[0]
      .reader()
      .get_core_cache_helper()?
      .expect("core cache helper should exist")
      .get_key(),
    (&oldest).get_context()?.leaves()?[0]
      .reader()
      .get_core_cache_helper()?
      .expect("core cache helper should exist")
      .get_key()
  );

  latest.close()?;
  oldest.close()?;

  snapshotter.release(&ic1)?;
  snapshotter.release(&ic2)?;
  writer.close()?;
  Ok(())
}

#[test]
fn test_nrt_mdeletes2() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  iwc.set_merge_policy(NoMergePolicy::default());
  let snapshotter = SnapshotDeletionPolicy::new(KeepOnlyLastCommitDeletionPolicy);
  iwc.set_index_deletion_policy(snapshotter.clone());
  let writer = IndexWriter::new(dir.clone(), iwc)?;
  writer.commit()?; // make sure all index metadata is written out

  let mut doc = Document::new();
  doc.add(StringField::from_string("key", "value1", Store::Yes)?);
  writer.add_document(doc)?;

  doc = Document::new();
  doc.add(StringField::from_string("key", "value2", Store::Yes)?);
  writer.add_document(doc)?;

  writer.commit()?;

  let ic1 = snapshotter.snapshot()?;

  doc = Document::new();
  doc.add(StringField::from_string("key", "value3", Store::Yes)?);
  writer.update_document_with_term(Term::from_text("key", "value1"), doc)?;

  let latest = directory_reader::open_from_writer(&writer)?;
  assert_eq!(2, (&latest).get_context()?.leaves()?.len());

  // This reader will be used for searching against commit point 1
  let oldest = directory_reader::open_if_changed_with_commit(&latest, Some(&ic1))?
    .expect("reader should change");

  // This reader should not see the deletion:
  assert_eq!(2, oldest.num_docs()?);
  assert!(!oldest.has_deletions()?);

  snapshotter.release(&ic1)?;
  assert_eq!(1, (&oldest).get_context()?.leaves()?.len());

  // sharing same core
  assert_eq!(
    (&latest).get_context()?.leaves()?[0]
      .reader()
      .get_core_cache_helper()?
      .expect("core cache helper should exist")
      .get_key(),
    (&oldest).get_context()?.leaves()?[0]
      .reader()
      .get_core_cache_helper()?
      .expect("core cache helper should exist")
      .get_key()
  );

  latest.close()?;
  oldest.close()?;

  writer.close()?;
  Ok(())
}

#[test]
fn test_nrt_mupdates() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let snapshotter = SnapshotDeletionPolicy::new(KeepOnlyLastCommitDeletionPolicy);
  iwc.set_index_deletion_policy(snapshotter.clone());
  let writer = IndexWriter::new(dir.clone(), iwc)?;
  writer.commit()?; // make sure all index metadata is written out

  let mut doc = Document::new();
  doc.add(StringField::from_string("key", "value1", Store::Yes)?);
  doc.add(NumericDocValuesField::new("dv", 1));
  writer.add_document(doc)?;

  writer.commit()?;

  let ic1 = snapshotter.snapshot()?;

  writer.update_numeric_doc_value(Term::from_text("key", "value1"), "dv", 2)?;

  writer.commit()?;

  let ic2 = snapshotter.snapshot()?;
  let latest = directory_reader::open_from_commit(&ic2)?;
  assert_eq!(1, (&latest).get_context()?.leaves()?.len());

  // This reader will be used for searching against commit point 1
  let oldest = directory_reader::open_if_changed_with_commit(&latest, Some(&ic1))?
    .expect("reader should change");
  assert_eq!(1, (&oldest).get_context()?.leaves()?.len());

  // sharing same core
  assert_eq!(
    (&latest).get_context()?.leaves()?[0]
      .reader()
      .get_core_cache_helper()?
      .expect("core cache helper should exist")
      .get_key(),
    (&oldest).get_context()?.leaves()?[0]
      .reader()
      .get_core_cache_helper()?
      .expect("core cache helper should exist")
      .get_key()
  );

  let oldest_leaf = get_only_leaf_reader(&oldest)?;
  let mut values = oldest_leaf
    .get_numeric_doc_values("dv")?
    .expect("numeric doc values should exist");
  assert_eq!(0, values.next_doc()?);
  assert_eq!(1, values.long_value()?);

  let latest_leaf = get_only_leaf_reader(&latest)?;
  values = latest_leaf
    .get_numeric_doc_values("dv")?
    .expect("numeric doc values should exist");
  assert_eq!(0, values.next_doc()?);
  assert_eq!(2, values.long_value()?);

  latest.close()?;
  oldest.close()?;

  snapshotter.release(&ic1)?;
  snapshotter.release(&ic2)?;
  writer.close()?;
  Ok(())
}

#[test]
fn test_nrt_mupdates2() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(analyzer)?;
  let snapshotter = SnapshotDeletionPolicy::new(KeepOnlyLastCommitDeletionPolicy);
  iwc.set_index_deletion_policy(snapshotter.clone());
  let writer = IndexWriter::new(dir.clone(), iwc)?;
  writer.commit()?; // make sure all index metadata is written out

  let mut doc = Document::new();
  doc.add(StringField::from_string("key", "value1", Store::Yes)?);
  doc.add(NumericDocValuesField::new("dv", 1));
  writer.add_document(doc)?;

  writer.commit()?;

  let ic1 = snapshotter.snapshot()?;

  writer.update_numeric_doc_value(Term::from_text("key", "value1"), "dv", 2)?;

  let latest = directory_reader::open_from_writer(&writer)?;
  assert_eq!(1, (&latest).get_context()?.leaves()?.len());

  // This reader will be used for searching against commit point 1
  let oldest = directory_reader::open_if_changed_with_commit(&latest, Some(&ic1))?
    .expect("reader should change");
  assert_eq!(1, (&oldest).get_context()?.leaves()?.len());

  // sharing same core
  assert_eq!(
    (&latest).get_context()?.leaves()?[0]
      .reader()
      .get_core_cache_helper()?
      .expect("core cache helper should exist")
      .get_key(),
    (&oldest).get_context()?.leaves()?[0]
      .reader()
      .get_core_cache_helper()?
      .expect("core cache helper should exist")
      .get_key()
  );

  let oldest_leaf = get_only_leaf_reader(&oldest)?;
  let mut values = oldest_leaf
    .get_numeric_doc_values("dv")?
    .expect("numeric doc values should exist");
  assert_eq!(0, values.next_doc()?);
  assert_eq!(1, values.long_value()?);

  let latest_leaf = get_only_leaf_reader(&latest)?;
  values = latest_leaf
    .get_numeric_doc_values("dv")?
    .expect("numeric doc values should exist");
  assert_eq!(0, values.next_doc()?);
  assert_eq!(2, values.long_value()?);

  latest.close()?;
  oldest.close()?;

  snapshotter.release(&ic1)?;
  writer.close()?;
  Ok(())
}

#[test]
fn test_delete_index_files_while_reader_still_open() -> Result<()> {
  let mut random = random();
  let dir = Arc::new(ByteBuffersDirectory::new());
  let analyzer = MockAnalyzer::new(&mut random);
  let mut w = IndexWriter::new(dir.clone(), IndexWriterConfig::with_analyzer(analyzer)?)?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("field", "value", Store::No)?);
  w.add_document(doc)?;
  w.close()?;
  drop(w);

  let r = directory_reader::open(dir.clone())?;

  for file in dir.list_all()? {
    dir.delete_file(&file)?;
  }

  let analyzer = MockAnalyzer::new(&mut random);
  let mut config = IndexWriterConfig::with_analyzer(analyzer)?;
  config.set_merge_policy(NoMergePolicy::default());
  w = IndexWriter::new(dir.clone(), config)?;
  doc = Document::new();
  doc.add(StringField::from_string("field", "value", Store::No)?);
  w.add_document(doc)?;

  doc = Document::new();
  doc.add(StringField::from_string("field", "value2", Store::No)?);
  w.add_document(doc.clone())?;

  w.commit()?;

  w.delete_documents_with_terms(vec![Term::from_text("field", "value2")])?;

  w.add_document(doc)?;
  w.close()?;
  let err = directory_reader::open_if_changed(&r);
  assert!(matches!(err, Err(LuceneError::IllegalState(_))));

  r.close()?;
  Ok(())
}

#[test]
fn test_reuse_unchanged_leaf_reader_on_dv_update() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut index_writer_config = new_index_writer_config(&mut random)?;
  index_writer_config.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(dir.clone(), index_writer_config)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "1", Store::Yes)?);
  doc.add(StringField::from_string("version", "1", Store::Yes)?);
  doc.add(NumericDocValuesField::new("some_docvalue", 2));
  writer.add_document(doc)?;
  doc = Document::new();
  doc.add(StringField::from_string("id", "2", Store::Yes)?);
  doc.add(StringField::from_string("version", "1", Store::Yes)?);
  writer.add_document(doc)?;
  writer.commit()?;
  let mut reader = directory_reader::open(dir.clone())?;
  assert_eq!(2, reader.num_docs()?);
  assert_eq!(2, reader.max_doc()?);
  assert_eq!(0, reader.num_deleted_docs()?);

  doc = Document::new();
  doc.add(StringField::from_string("id", "1", Store::Yes)?);
  doc.add(StringField::from_string("version", "2", Store::Yes)?);
  writer.update_doc_values(
    Term::from_text("id", "1"),
    vec![NumericDocValuesField::new("some_docvalue", 1).into()],
  )?;
  writer.commit()?;
  let mut new_reader = directory_reader::open_if_changed(&reader)?.unwrap();
  reader.close()?;
  reader = new_reader;
  assert_eq!(2, reader.num_docs()?);
  assert_eq!(2, reader.max_doc()?);
  assert_eq!(0, reader.num_deleted_docs()?);

  doc = Document::new();
  doc.add(StringField::from_string("id", "3", Store::Yes)?);
  doc.add(StringField::from_string("version", "3", Store::Yes)?);
  writer.update_document_with_term(Some(Term::from_text("id", "3")), doc)?;
  writer.commit()?;

  new_reader = directory_reader::open_if_changed(&reader)?.unwrap();
  assert_eq!(2, new_reader.get_sequential_sub_readers().len());
  assert_eq!(1, reader.get_sequential_sub_readers().len());
  reader.close()?;
  reader = new_reader;
  assert_eq!(3, reader.num_docs()?);
  assert_eq!(3, reader.max_doc()?);
  assert_eq!(0, reader.num_deleted_docs()?);
  reader.close()?;
  writer.close()?;
  Ok(())
}

struct FailOnReadLiveDocs {
  do_fail: bool,
  failed: bool,
}

impl<D> Failure<D> for FailOnReadLiveDocs
where
  D: Directory,
{
  fn eval(&mut self, _dir: &MockDirectoryWrapper<D>) -> Result<()> {
    if !self.failed && call_stack_contains_any_of(&["read_live_docs"]) {
      if cfg!(feature = "test_log_verbose") {
        println!("TEST: now fail; exc:");
      }
      self.failed = true;
      return Err(LuceneError::io(std::io::Error::other(FakeIOException)));
    }
    Ok(())
  }

  fn set_do_fail(&mut self) {
    self.do_fail = true;
  }

  fn clear_do_fail(&mut self) {
    self.do_fail = false;
  }
}
