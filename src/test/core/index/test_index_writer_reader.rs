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
use crate::core::document::field::{Field, FieldBase, Store};
use crate::core::document::long_point::LongPoint;
use crate::core::document::string_field::StringField;
use crate::core::index::concurrent_merge_scheduler::ConcurrentMergeScheduler;
use crate::core::index::directory_reader::{self, DirectoryReader};
use crate::core::index::index_commit::IndexCommit;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::{IndexReaderWarmer, IndexReaderWarmerEnum, IndexWriter};
use crate::core::index::index_writer_config::{
  DEFAULT_RAM_BUFFER_SIZE_MB, DISABLE_AUTO_FLUSH, IndexWriterConfig,
};
use crate::core::index::indexable_field::IndexableField;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LeafSorter;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::merge_policy::MergePolicyEnum;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::index::point_values::PointValues;
use crate::core::index::segment_reader::DefaultLeafReader;
use crate::core::index::simple_merged_segment_warmer::SimpleMergedSegmentWarmer;
use crate::core::index::standard_directory_reader::StandardDirectoryReader;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::index_searcher::{self, IndexSearcher};
use crate::core::search::term_query::TermQuery;
use crate::core::store::byte_buffers_directory::ByteBuffersDirectory;
use crate::core::store::directory::{DirEnum, Directory, DirectoryEnum2};
use crate::core::store::single_instance_lock_factory::SingleInstanceLockFactory;
use crate::core::util::Comparator;
use crate::core::util::close::{Closeable, CloseableRef};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::info_stream::{InfoStream, InfoStreamEnum};
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::doc_helper::{DocHelper, STRING_TYPE_STORED_WITH_TVS};
use crate::test_framework::core::index::test_index_writer_reader::{count, create_index_no_close};
use crate::test_framework::core::store::mock_directory_wrapper::{
  Failure, FakeIOException, MockDirectoryWrapper,
};
use crate::test_framework::core::util::lucene_test_case::{
  at_least, call_stack_contains_any_of, is_night_mode, new_directory_shared,
  new_index_writer_config, new_index_writer_config_with_analyzer, new_log_merge_policy,
  new_log_merge_policy_with_merge_factor, new_mock_directory, new_text_field, random,
  random_from_seed,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::RngExt;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::io::Error;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::thread;

#[allow(dead_code)] // for quick search
struct TestIndexWriterReader;

#[test]
fn test_add_close_open() -> Result<()> {
  let mut random = random();
  let dir1 = new_directory_shared(&mut random)?;
  let iwc = new_index_writer_config(&mut random)?;

  let mut writer = IndexWriter::new(dir1.clone(), iwc)?;
  for i in 0..97 {
    let reader = directory_reader::open_from_writer(&writer)?;
    if i == 0 {
      writer.add_document(DocHelper::create_document(
        i,
        "x",
        1 + random.random_range(0..5),
      ))?;
    } else {
      let previous = random.random_range(0..i);
      match random.random_range(0..5) {
        0..=2 => {
          writer.add_document(DocHelper::create_document(
            i,
            "x",
            1 + random.random_range(0..5),
          ))?;
        },
        3 => {
          writer.update_document_with_term(
            Term::from_text("id", previous.to_string()),
            DocHelper::create_document(previous, "x", 1 + random.random_range(0..5)),
          )?;
        },
        4 => {
          writer.delete_documents_with_terms(vec![Term::from_text("id", previous.to_string())])?;
        },
        _ => unreachable!(),
      }
    }
    assert!(!reader.is_current()?);
    reader.close()?;
  }
  writer.force_merge(1)?;
  let mut reader = directory_reader::open_from_writer(&writer)?;
  writer.commit()?;

  assert!(!reader.is_current()?);
  reader.close()?;
  reader = directory_reader::open_from_writer(&writer)?;
  assert!(reader.is_current()?);
  writer.close()?;

  assert!(reader.is_current()?);
  let iwc = new_index_writer_config(&mut random)?;
  drop(writer);
  writer = IndexWriter::new(dir1.clone(), iwc)?;
  assert!(reader.is_current()?);
  writer.add_document(DocHelper::create_document(
    1,
    "x",
    1 + random.random_range(0..5),
  ))?;
  assert!(reader.is_current()?);
  writer.close()?;
  assert!(!reader.is_current()?);
  reader.close()?;
  Ok(())
}

#[test]
fn test_update_document() -> Result<()> {
  let do_full_merge = true;

  let mut random = random();
  let dir1 = new_directory_shared(&mut random)?;
  let mut iwc = new_index_writer_config(&mut random)?;
  if iwc.get_max_buffered_docs() < 20 {
    iwc.set_max_buffered_docs(20);
  }
  iwc.set_merge_policy(NoMergePolicy::default());
  let mut writer = IndexWriter::new(dir1.clone(), iwc)?;

  create_index_no_close(!do_full_merge, "index1", &writer)?;

  let r1 = directory_reader::open_from_writer(&writer)?;
  assert!(r1.is_current()?);

  let mut stored_fields = r1.stored_fields()?;
  let id10 = stored_fields
    .document(10)?
    .get_field("id")
    .expect("id field should exist")
    .string_value()?
    .expect("id field should be stored")
    .into_owned();

  let mut new_doc = stored_fields.document(10)?;
  new_doc.remove_field("id");
  new_doc.add(Field::new(
    "id",
    8000.to_string(),
    STRING_TYPE_STORED_WITH_TVS.clone(),
  ));
  writer.update_document_with_term(Term::from_text("id", id10.clone()), new_doc)?;
  assert!(!r1.is_current()?);

  let r2 = directory_reader::open_from_writer(&writer)?;
  assert!(r2.is_current()?);
  assert_eq!(
    0,
    count(&mut random, &Term::from_text("id", id10.clone()), &r2)?
  );
  assert_eq!(
    1,
    count(&mut random, &Term::from_text("id", 8000.to_string()), &r2)?
  );

  r1.close()?;
  assert!(r2.is_current()?);
  writer.close()?;
  assert!(!r2.is_current()?);

  let r3 = directory_reader::open(dir1.clone())?;
  assert!(r3.is_current()?);
  assert!(!r2.is_current()?);
  assert_eq!(0, count(&mut random, &Term::from_text("id", id10), &r3)?);
  assert_eq!(
    1,
    count(&mut random, &Term::from_text("id", 8000.to_string()), &r3)?
  );
  drop(writer);
  writer = IndexWriter::new(dir1.clone(), new_index_writer_config(&mut random)?)?;
  let mut doc = Document::new();
  doc.add(new_text_field(
    &mut random,
    "field",
    "a b c",
    Store::No,
    &mut Default::default(),
  )?);
  writer.add_document(doc)?;
  assert!(!r2.is_current()?);
  assert!(r3.is_current()?);

  writer.close()?;

  assert!(!r2.is_current()?);
  assert!(!r3.is_current()?);

  r2.close()?;
  r3.close()?;
  Ok(())
}

#[test]
fn test_is_current() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let iwc = new_index_writer_config(&mut random)?;
  let mut field_to_type = HashMap::new();
  let mut writer = IndexWriter::new(dir.clone(), iwc)?;
  let mut doc = Document::new();
  doc.add(new_text_field(
    &mut random,
    "field",
    "a b c",
    Store::No,
    &mut field_to_type,
  )?);
  writer.add_document(doc)?;
  writer.close()?;
  drop(writer);
  let iwc = new_index_writer_config(&mut random)?;
  writer = IndexWriter::new(dir.clone(), iwc)?;
  let mut doc = Document::new();
  doc.add(new_text_field(
    &mut random,
    "field",
    "a b c",
    Store::No,
    &mut field_to_type,
  )?);
  let nrt_reader = directory_reader::open_from_writer(&writer)?;
  assert!(nrt_reader.is_current()?);
  writer.add_document(doc)?;
  assert!(!nrt_reader.is_current()?);
  writer.force_merge(1)?;
  assert!(!nrt_reader.is_current()?);
  nrt_reader.close()?;

  let dir_reader = directory_reader::open(dir.clone())?;
  let nrt_reader = directory_reader::open_from_writer(&writer)?;

  assert!(dir_reader.is_current()?);
  assert!(nrt_reader.is_current()?);
  assert_eq!(2, nrt_reader.max_doc()?);
  assert_eq!(1, dir_reader.max_doc()?);
  writer.close()?;
  assert!(!nrt_reader.is_current()?);
  assert!(!dir_reader.is_current()?);

  dir_reader.close()?;
  nrt_reader.close()?;
  Ok(())
}

#[test]
fn test_add_indexes() -> Result<()> {
  let do_full_merge = false;

  let mut random = random();
  let dir1 = new_directory_shared(&mut random)?;
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_max_full_flush_merge_wait_millis(0);
  if iwc.get_max_buffered_docs() < 20 {
    iwc.set_max_buffered_docs(20);
  }
  iwc.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(dir1.clone(), iwc)?;

  create_index_no_close(!do_full_merge, "index1", &writer)?;
  writer.flush()?;

  let dir2 = new_directory_shared(&mut random)?;
  let writer2 = IndexWriter::new(dir2.clone(), new_index_writer_config(&mut random)?)?;
  create_index_no_close(!do_full_merge, "index2", &writer2)?;
  writer2.close()?;

  let r0 = directory_reader::open_from_writer(&writer)?;
  assert!(r0.is_current()?);
  drop(writer2);
  writer.add_indexes_from_directory(std::slice::from_ref(&dir2))?;
  assert!(!r0.is_current()?);
  r0.close()?;

  let r1 = directory_reader::open_from_writer(&writer)?;
  assert!(r1.is_current()?);

  writer.commit()?;

  assert!(!r1.is_current()?);

  assert_eq!(200, r1.max_doc()?);

  let index2df = r1.doc_freq(&Term::from_text("indexname", "index2"))?;

  assert_eq!(100, index2df);

  let mut stored_fields = r1.stored_fields()?;
  let doc5 = stored_fields.document(5)?;
  assert_eq!("index1", doc5.get("indexname")?.unwrap().as_ref());
  let doc150 = stored_fields.document(150)?;
  assert_eq!("index2", doc150.get("indexname")?.unwrap().as_ref());
  r1.close()?;
  writer.close()?;
  Ok(())
}

#[test]
fn test_add_indexes2() -> Result<()> {
  let do_full_merge = false;

  let mut random = random();
  let dir1 = new_directory_shared(&mut random)?;
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_max_full_flush_merge_wait_millis(0);
  let writer = IndexWriter::new(dir1.clone(), iwc)?;

  let dir2 = new_directory_shared(&mut random)?;
  let mut iwc2 = new_index_writer_config(&mut random)?;
  iwc2.set_max_full_flush_merge_wait_millis(0);
  let writer2 = IndexWriter::new(dir2.clone(), iwc2)?;
  create_index_no_close(!do_full_merge, "index2", &writer2)?;
  writer2.close()?;
  drop(writer2);
  writer.add_indexes_from_directory(std::slice::from_ref(&dir2))?;
  writer.add_indexes_from_directory(std::slice::from_ref(&dir2))?;
  writer.add_indexes_from_directory(std::slice::from_ref(&dir2))?;
  writer.add_indexes_from_directory(std::slice::from_ref(&dir2))?;
  writer.add_indexes_from_directory(std::slice::from_ref(&dir2))?;

  let r1 = directory_reader::open_from_writer(&writer)?;
  assert_eq!(500, r1.max_doc()?);

  r1.close()?;
  writer.close()?;
  Ok(())
}

#[test]
fn test_delete_from_index_writer() -> Result<()> {
  let do_full_merge = true;

  let mut random = random();
  let dir1 = new_directory_shared(&mut random)?;
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_max_full_flush_merge_wait_millis(0);
  let mut writer = IndexWriter::new(dir1.clone(), iwc)?;

  create_index_no_close(!do_full_merge, "index1", &writer)?;
  writer.flush_with_apply_merge_deletes(false, true)?;

  let r1 = directory_reader::open_from_writer(&writer)?;

  let mut stored_fields = r1.stored_fields()?;
  let id10 = stored_fields
    .document(10)?
    .get_field("id")
    .expect("id field should exist")
    .string_value()?
    .expect("id field should be stored")
    .into_owned();

  writer.delete_documents_with_terms(vec![Term::from_text("id", id10.clone())])?;
  let r2 = directory_reader::open_from_writer(&writer)?;
  assert_eq!(
    1,
    count(&mut random, &Term::from_text("id", id10.clone()), &r1)?
  );
  assert_eq!(
    0,
    count(&mut random, &Term::from_text("id", id10.clone()), &r2)?
  );

  let id50 = stored_fields
    .document(50)?
    .get_field("id")
    .expect("id field should exist")
    .string_value()?
    .expect("id field should be stored")
    .into_owned();
  assert_eq!(
    1,
    count(&mut random, &Term::from_text("id", id50.clone()), &r1)?
  );

  writer.delete_documents_with_terms(vec![Term::from_text("id", id50.clone())])?;

  let r3 = directory_reader::open_from_writer(&writer)?;
  assert_eq!(
    0,
    count(&mut random, &Term::from_text("id", id10.clone()), &r3)?
  );
  assert_eq!(0, count(&mut random, &Term::from_text("id", id50), &r3)?);

  let id75 = stored_fields
    .document(75)?
    .get_field("id")
    .expect("id field should exist")
    .string_value()?
    .expect("id field should be stored")
    .into_owned();
  writer.delete_documents_with_queries(vec![
    TermQuery::new(Term::from_text("id", id75.clone())).into(),
  ])?;
  let r4 = directory_reader::open_from_writer(&writer)?;
  assert_eq!(
    1,
    count(&mut random, &Term::from_text("id", id75.clone()), &r3)?
  );
  assert_eq!(0, count(&mut random, &Term::from_text("id", id75), &r4)?);

  r1.close()?;
  r2.close()?;
  r3.close()?;
  r4.close()?;
  writer.close()?;

  drop(writer);
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_max_full_flush_merge_wait_millis(0);
  writer = IndexWriter::new(dir1.clone(), iwc)?;
  let w2r1 = directory_reader::open_from_writer(&writer)?;
  assert_eq!(0, count(&mut random, &Term::from_text("id", id10), &w2r1)?);
  w2r1.close()?;
  writer.close()?;
  Ok(())
}

#[test]
fn test_add_indexes_and_do_deletes_threads() -> Result<()> {
  let mut random = random();
  let num_iter = if is_night_mode() { 2 } else { 1 };
  let num_dirs = if is_night_mode() { 3 } else { 2 };

  let main_directory = new_directory_shared(&mut random)?;
  let main_dir = Arc::new(AddDirectoriesDirectory::A(main_directory));

  let analyzer = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  iwc
    .set_merge_policy(new_log_merge_policy(&mut random)?)
    .set_max_full_flush_merge_wait_millis(0);
  let main_writer = IndexWriter::new(main_dir.clone(), iwc)?;
  TestUtil::reduce_open_files(main_writer.as_ref())?;

  let mut add_dir_threads = AddDirectoriesThreads::new(&mut random, num_iter, main_writer.clone())?;
  add_dir_threads.launch_threads(&mut random, num_dirs);
  add_dir_threads.join_threads()?;

  // assert_eq!(100 + num_dirs * (3 * num_iter / 4) * add_dir_threads.num_threads
  //     * AddDirectoriesThreads::NUM_INIT_DOCS, main_writer.get_doc_stats()?.num_docs);
  assert_eq!(
    add_dir_threads.count.load(AtomicOrdering::SeqCst),
    main_writer.get_doc_stats()?.num_docs as usize
  );

  add_dir_threads.close(true)?;

  assert!(
    add_dir_threads
      .failures
      .lock()
      .expect("failures lock poisoned")
      .is_empty()
  );

  TestUtil::check_index(&mut random, main_dir.as_ref())?;

  let reader = directory_reader::open(main_dir.clone())?;
  assert_eq!(
    add_dir_threads.count.load(AtomicOrdering::SeqCst) as i32,
    reader.num_docs()?
  );
  // assert_eq!(100 + num_dirs * (3 * num_iter / 4) * add_dir_threads.num_threads
  //     * AddDirectoriesThreads::NUM_INIT_DOCS, reader.num_docs()?);
  reader.close()?;

  add_dir_threads.close_dir()?;
  main_dir.close()
}

type AddDirectoriesDirectory =
  DirectoryEnum2<Arc<DirEnum>, Arc<ByteBuffersDirectory<SingleInstanceLockFactory>>>;

struct AddDirectoriesThreads {
  add_dir: Arc<AddDirectoriesDirectory>,
  num_dirs: usize,
  threads: Vec<thread::JoinHandle<()>>,
  main_writer: Arc<IndexWriter<AddDirectoriesDirectory>>,
  failures: Arc<Mutex<Vec<String>>>,
  readers: Vec<Arc<StandardDirectoryReader<AddDirectoriesDirectory>>>,
  count: Arc<AtomicUsize>,
  num_add_indexes: Arc<AtomicUsize>,
}

impl AddDirectoriesThreads {
  const NUM_INIT_DOCS: usize = 100;

  fn new<R>(
    random: &mut R,
    num_dirs: usize,
    main_writer: Arc<IndexWriter<AddDirectoriesDirectory>>,
  ) -> Result<Self>
  where
    R: rand::Rng + ?Sized,
  {
    let add_directory = new_directory_shared(random)?;
    let add_dir = Arc::new(AddDirectoriesDirectory::A(add_directory));
    let analyzer = MockAnalyzer::new(random);
    let mut config = new_index_writer_config_with_analyzer(random, analyzer)?;
    config
      .set_max_full_flush_merge_wait_millis(0)
      .set_max_buffered_docs(2);
    let writer = IndexWriter::new(add_dir.clone(), config)?;
    TestUtil::reduce_open_files(&writer)?;
    for i in 0..Self::NUM_INIT_DOCS {
      writer.add_document(DocHelper::create_document(i as i32, "addindex", 4))?;
    }
    writer.close()?;

    let mut readers = Vec::with_capacity(num_dirs);
    for _ in 0..num_dirs {
      readers.push(Arc::new(directory_reader::open(add_dir.clone())?));
    }

    Ok(Self {
      add_dir,
      num_dirs,
      threads: Vec::new(),
      main_writer,
      failures: Arc::new(Mutex::new(Vec::new())),
      readers,
      count: Arc::new(AtomicUsize::new(0)),
      num_add_indexes: Arc::new(AtomicUsize::new(0)),
    })
  }

  fn join_threads(&mut self) -> Result<()> {
    for thread in self.threads.drain(..) {
      if thread.join().is_err() {
        return Err(LuceneError::illegal_state(
          "addIndexes worker thread panicked",
        ));
      }
    }
    Ok(())
  }

  fn close(&self, do_wait: bool) -> Result<()> {
    if do_wait {
      self.main_writer.close()
    } else {
      self.main_writer.rollback()
    }
  }

  fn close_dir(&self) -> Result<()> {
    for reader in &self.readers {
      reader.close()?;
    }
    self.add_dir.close()
  }

  fn handle(failures: &Mutex<Vec<String>>, error: String) {
    println!("{error}");
    failures.lock().expect("failures lock poisoned").push(error);
  }

  fn launch_threads<R>(&mut self, random: &mut R, num_iter: usize)
  where
    R: rand::Rng + ?Sized,
  {
    let num_threads = if is_night_mode() { 5 } else { 2 };
    for _ in 0..num_threads {
      let seed = random.random::<u64>();
      let add_dir = self.add_dir.clone();
      let main_writer = self.main_writer.clone();
      let failures = self.failures.clone();
      let readers = self.readers.clone();
      let count = self.count.clone();
      let num_add_indexes = self.num_add_indexes.clone();
      let num_dirs = self.num_dirs;
      self.threads.push(thread::spawn(move || {
        let result = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
          let mut random = random_from_seed(seed);
          let mut dirs = Vec::with_capacity(num_dirs);
          for _ in 0..num_dirs {
            dirs.push(Arc::new(AddDirectoriesDirectory::B(TestUtil::ram_copy_of(
              &mut random,
              add_dir.as_ref(),
            )?)));
          }
          // let mut j = 0;
          // loop {
          //   println!("{}: iter j={j}", thread::current().name().unwrap_or("worker"));
          // only do addIndexes
          for j in 0..num_iter {
            Self::do_body(
              j,
              &dirs,
              main_writer.as_ref(),
              &readers,
              num_add_indexes.as_ref(),
              count.as_ref(),
            )?;
          }
          // if num_iter > 0 && j == num_iter { break; }
          // Self::do_body(j, &dirs, ...)?;
          // Self::do_body(5, &dirs, ...)?;
          // }
          Ok(())
        }));
        match result {
          Ok(Ok(())) => {},
          Ok(Err(error)) => Self::handle(failures.as_ref(), format!("{error:?}")),
          Err(payload) => {
            let message = if let Some(message) = payload.downcast_ref::<String>() {
              message.clone()
            } else if let Some(message) = payload.downcast_ref::<&str>() {
              (*message).to_string()
            } else {
              "addIndexes worker panicked".to_string()
            };
            Self::handle(failures.as_ref(), message);
          },
        }
      }));
    }
  }

  fn do_body(
    j: usize,
    dirs: &[Arc<AddDirectoriesDirectory>],
    main_writer: &IndexWriter<AddDirectoriesDirectory>,
    readers: &[Arc<StandardDirectoryReader<AddDirectoriesDirectory>>],
    num_add_indexes: &AtomicUsize,
    count: &AtomicUsize,
  ) -> Result<()> {
    match j % 4 {
      0 => {
        main_writer.add_indexes_from_directory(dirs)?;
        main_writer.force_merge(1)?;
      },
      1 => {
        main_writer.add_indexes_from_directory(dirs)?;
        num_add_indexes.fetch_add(1, AtomicOrdering::SeqCst);
      },
      2 => {
        TestUtil::add_indexes_slowly(main_writer, readers)?;
      },
      3 => {
        main_writer.commit()?;
      },
      _ => unreachable!(),
    }
    count.fetch_add(
      dirs.len() * AddDirectoriesThreads::NUM_INIT_DOCS,
      AtomicOrdering::SeqCst,
    );
    Ok(())
  }
}

#[test]
fn test_index_writer_reopen_segment_full_merge() -> Result<()> {
  do_test_index_writer_reopen_segment(true)
}

#[test]
fn test_index_writer_reopen_segment() -> Result<()> {
  do_test_index_writer_reopen_segment(false)
}

fn do_test_index_writer_reopen_segment(do_full_merge: bool) -> Result<()> {
  // TODO: getAssertNoDeletesDirectory is not implemented, so this currently lacks Java's wrapper
  // assertion that reopened segments expose no deletes.
  let mut random = random();
  let dir1 = new_directory_shared(&mut random)?;
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_max_full_flush_merge_wait_millis(0);
  let mut writer = IndexWriter::new(dir1.clone(), iwc)?;
  let r1 = directory_reader::open_from_writer(&writer)?;
  assert_eq!(0, r1.max_doc()?);
  create_index_no_close(false, "index1", &writer)?;
  writer.flush_with_apply_merge_deletes(!do_full_merge, true)?;

  let iwr1 = directory_reader::open_from_writer(&writer)?;
  assert_eq!(100, iwr1.max_doc()?);

  let r2 = directory_reader::open_from_writer(&writer)?;
  assert_eq!(r2.max_doc()?, 100);
  for x in 10000..10000 + 100 {
    let d = DocHelper::create_document(x, "index1", 5);
    writer.add_document(d)?;
  }
  writer.flush_with_apply_merge_deletes(false, true)?;

  let iwr2 = directory_reader::open_from_writer(&writer)?;
  assert_ne!(iwr2.get_version()?, r1.get_version()?);
  assert_eq!(200, iwr2.max_doc()?);

  let r3 = directory_reader::open_from_writer(&writer)?;
  assert_ne!(r2.get_version()?, r3.get_version()?);
  assert_eq!(200, r3.max_doc()?);

  r1.close()?;
  iwr1.close()?;
  r2.close()?;
  r3.close()?;
  iwr2.close()?;
  writer.close()?;

  drop(writer);
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_max_full_flush_merge_wait_millis(0);
  writer = IndexWriter::new(dir1.clone(), iwc)?;
  let w2r1 = directory_reader::open_from_writer(&writer)?;
  assert_eq!(200, w2r1.max_doc()?);
  w2r1.close()?;
  writer.close()?;
  dir1.close()
}

#[test]
fn test_merge_warmer() -> Result<()> {
  let mut random = random();
  let dir1 = new_directory_shared(&mut random)?;
  let warm_count = Arc::new(AtomicUsize::new(0));
  let cms = ConcurrentMergeScheduler::new();
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  iwc
    .set_max_buffered_docs(2)
    .set_max_full_flush_merge_wait_millis(0)
    .set_merged_segment_warmer(Some(IndexReaderWarmerEnum::custom(CountingWarmer {
      warm_count: warm_count.clone(),
    })))
    .set_merge_scheduler(cms.clone())
    .set_merge_policy(new_log_merge_policy_with_merge_factor(&mut random, 10)?);
  let writer = IndexWriter::new(dir1, iwc)?;

  // create the index
  create_index_no_close(false, "test", &writer)?;

  // get a reader to put writer into near real-time mode
  let r1 = directory_reader::open_from_writer(&writer)?;

  match writer.get_config_mut().get_merge_policy_mut() {
    MergePolicyEnum::LogDoc(merge_policy) => merge_policy.set_merge_factor(2)?,
    MergePolicyEnum::LogBytesSize(merge_policy) => merge_policy.set_merge_factor(2)?,
    _ => panic!("expected LogMergePolicy variant"),
  }

  let num = if is_night_mode() { 100 } else { 10 };
  for i in 0..num {
    writer.add_document(DocHelper::create_document(i, "test", 4))?;
  }
  cms.sync()?;

  assert!(warm_count.load(AtomicOrdering::SeqCst) > 0);
  let count = warm_count.load(AtomicOrdering::SeqCst);

  writer.add_document(DocHelper::create_document(17, "test", 4))?;
  writer.force_merge(1)?;
  assert!(warm_count.load(AtomicOrdering::SeqCst) > count);

  writer.close()?;
  r1.close()?;
  Ok(())
}

#[test]
fn test_after_commit() -> Result<()> {
  let mut random = random();
  let dir1 = new_directory_shared(&mut random)?;
  let cms = ConcurrentMergeScheduler::new();
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  iwc
    .set_merge_scheduler(cms.clone())
    .set_max_full_flush_merge_wait_millis(0);
  let writer = IndexWriter::new(dir1.clone(), iwc)?;
  writer.commit()?;

  // create the index
  create_index_no_close(false, "test", &writer)?;

  // get a reader to put writer into near real-time mode
  let mut r1 = directory_reader::open_from_writer(&writer)?;
  TestUtil::check_index(&mut random, dir1.clone())?;
  writer.commit()?;
  TestUtil::check_index(&mut random, dir1)?;
  assert_eq!(100, r1.num_docs()?);

  for i in 0..10 {
    writer.add_document(DocHelper::create_document(i, "test", 4))?;
  }
  cms.sync()?;

  if let Some(r2) = directory_reader::open_if_changed(&r1)? {
    r1.close()?;
    r1 = r2;
  }
  assert_eq!(110, r1.num_docs()?);
  writer.close()?;
  r1.close()?;
  Ok(())
}

#[test]
fn test_after_close() -> Result<()> {
  let mut random = random();
  let dir1 = new_directory_shared(&mut random)?;
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_max_full_flush_merge_wait_millis(0);
  let writer = IndexWriter::new(dir1.clone(), iwc)?;

  create_index_no_close(false, "test", &writer)?;

  let r = directory_reader::open_from_writer(&writer)?;
  writer.close()?;

  TestUtil::check_index(&mut random, dir1)?;

  assert_eq!(100, r.num_docs()?);
  let q = TermQuery::new(Term::from_text("indexname", "test"));
  let searcher = index_searcher::from_reader(r)?;
  assert_eq!(100, searcher.count(q)?);

  let err = directory_reader::open_if_changed(searcher.reader_context.reader());
  assert!(err.is_err());

  searcher.reader_context.reader().close()?;
  Ok(())
}
#[cfg(feature = "nightly")]
#[test]
#[ignore = "nightly"]
fn test_during_add_indexes() -> Result<()> {
  let mut random = random();
  let dir1 = new_directory_shared(&mut random)?;
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc
    .set_max_full_flush_merge_wait_millis(0)
    .set_merge_policy(new_log_merge_policy_with_merge_factor(&mut random, 2)?);
  let writer = IndexWriter::new(dir1.clone(), iwc)?;

  create_index_no_close(false, "test", writer.as_ref())?;
  writer.commit()?;

  let mut dirs = Vec::new();
  for _ in 0..10 {
    let copy = TestUtil::ram_copy_of(&mut random, dir1.as_ref())?;
    dirs.push(Arc::new(MockDirectoryWrapper::new(&mut random, copy)));
  }

  let mut r = directory_reader::open_from_writer(&writer)?;

  let num_iterations = 10;
  let failures = Arc::new(Mutex::new(Vec::new()));
  let thread_done = Arc::new(std::sync::atomic::AtomicBool::new(false));

  let handle = {
    let writer = writer.clone();
    let dirs = dirs.clone();
    let failures = failures.clone();
    let thread_done = thread_done.clone();
    thread::spawn(move || {
      let result = (|| -> Result<()> {
        let mut count = 0;
        loop {
          count += 1;
          writer.add_indexes_from_directory(&dirs)?;
          writer.maybe_merge()?;
          if count >= num_iterations {
            break;
          }
        }
        Ok(())
      })();
      if let Err(e) = result {
        failures
          .lock()
          .expect("failures lock poisoned")
          .push(format!("{e:?}"));
      }
      thread_done.store(true, AtomicOrdering::SeqCst);
    })
  };

  let mut last_count = 0;
  while !thread_done.load(AtomicOrdering::SeqCst) {
    let r2 = directory_reader::open_if_changed(&r)?;
    if let Some(r2) = r2 {
      r.close()?;
      r = r2;
      let term = Term::from_text("indexname", "test");
      let count = count(&mut random, &term, &r)?;
      assert!(count >= last_count);
      last_count = count;
    }
  }

  handle.join().expect("addIndexes thread panicked");
  let r2 = directory_reader::open_if_changed(&r)?;
  if let Some(r2) = r2 {
    r.close()?;
    r = r2;
  }
  let term = Term::from_text("indexname", "test");
  let count = count(&mut random, &term, &r)?;
  assert!(count >= last_count);

  assert!(failures.lock().expect("failures lock poisoned").is_empty());
  r.close()?;
  writer.close()?;
  Ok(())
}

#[test]
fn test_during_add_delete() -> Result<()> {
  let mut random = random();
  let dir1 = new_directory_shared(&mut random)?;
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_merge_policy(new_log_merge_policy_with_merge_factor(&mut random, 2)?);
  if is_night_mode() {
    iwc.set_ram_buffer_size_mb(DEFAULT_RAM_BUFFER_SIZE_MB);
    iwc.set_max_buffered_docs(DISABLE_AUTO_FLUSH);
  }
  let writer = IndexWriter::new(dir1.clone(), iwc)?;

  create_index_no_close(false, "test", writer.as_ref())?;
  writer.commit()?;

  let mut r = directory_reader::open_from_writer(&writer)?;

  let iters = if is_night_mode() { 1000 } else { 10 };
  let failures = Arc::new(Mutex::new(Vec::new()));

  let num_threads = if is_night_mode() { 5 } else { 2 };
  let remaining_threads = Arc::new(AtomicUsize::new(num_threads));
  let mut threads = Vec::new();
  for _ in 0..num_threads {
    let writer = writer.clone();
    let failures = failures.clone();
    let seed = random.random();
    let remaining_threads = remaining_threads.clone();
    threads.push(thread::spawn(move || {
      let result = (|| -> Result<()> {
        let mut random = random_from_seed(seed);
        let mut count = 0;
        loop {
          for doc_upto in 0..10 {
            writer.add_document(DocHelper::create_document(10 * count + doc_upto, "test", 4))?;
          }
          count += 1;
          let limit = count * 10;
          for _ in 0..5 {
            let x = random.random_range(0..limit);
            writer.delete_documents_with_terms(vec![Term::from_text("field3", format!("b{x}"))])?;
          }
          if count >= iters {
            break;
          }
        }
        Ok(())
      })();
      if let Err(e) = result {
        failures
          .lock()
          .expect("failures lock poisoned")
          .push(format!("{e:?}"));
      }
      remaining_threads.fetch_sub(1, AtomicOrdering::SeqCst);
    }));
  }

  let mut sum = 0;
  while remaining_threads.load(AtomicOrdering::SeqCst) > 0 {
    let r2 = directory_reader::open_if_changed(&r)?;
    if let Some(r2) = r2 {
      r.close()?;
      r = r2;
      let term = Term::from_text("indexname", "test");
      sum += count(&mut random, &term, &r)?;
    }
  }

  for handle in threads {
    handle.join().expect("add/delete thread panicked");
  }
  let r2 = directory_reader::open_if_changed(&r)?;
  if let Some(r2) = r2 {
    r.close()?;
    r = r2;
  }
  let term = Term::from_text("indexname", "test");
  sum += count(&mut random, &term, &r)?;
  assert!(sum > 0);

  assert!(failures.lock().expect("failures lock poisoned").is_empty());
  writer.close()?;

  r.close()?;
  Ok(())
}

#[test]
fn test_force_merge_deletes() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_merge_policy(new_log_merge_policy(&mut random)?);
  let w = IndexWriter::new(dir.clone(), iwc)?;
  let mut field_to_type = HashMap::new();

  let mut doc = Document::new();
  doc.add(new_text_field(
    &mut random,
    "field",
    "a b c",
    Store::No,
    &mut field_to_type,
  )?);
  let mut id = StringField::from_string("id", "", Store::No)?;
  doc.add(id.clone());
  id.set_string_value("0")?;
  let mut doc0 = Document::new();
  doc0.add(new_text_field(
    &mut random,
    "field",
    "a b c",
    Store::No,
    &mut field_to_type,
  )?);
  doc0.add(id.clone());
  w.add_document(doc0)?;
  id.set_string_value("1")?;
  let mut doc1 = Document::new();
  doc1.add(new_text_field(
    &mut random,
    "field",
    "a b c",
    Store::No,
    &mut field_to_type,
  )?);
  doc1.add(id);
  w.add_document(doc1)?;
  w.delete_documents_with_terms(vec![Term::from_text("id", "0")])?;

  let r = directory_reader::open_from_writer(&w)?;
  w.force_merge_deletes()?;
  w.close()?;
  r.close()?;
  drop(w);
  let r = directory_reader::open(dir.clone())?;
  assert_eq!(1, r.num_docs()?);
  assert!(!r.has_deletions()?);
  r.close()?;
  Ok(())
}

#[test]
fn test_deletes_num_docs() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let w = IndexWriter::new(dir.clone(), new_index_writer_config(&mut random)?)?;
  let mut field_to_type = HashMap::new();

  let mut id = StringField::from_string("id", "", Store::No)?;
  id.set_string_value("0")?;
  let mut doc0 = Document::new();
  doc0.add(new_text_field(
    &mut random,
    "field",
    "a b c",
    Store::No,
    &mut field_to_type,
  )?);
  doc0.add(id.clone());
  w.add_document(doc0)?;
  id.set_string_value("1")?;
  let mut doc1 = Document::new();
  doc1.add(new_text_field(
    &mut random,
    "field",
    "a b c",
    Store::No,
    &mut field_to_type,
  )?);
  doc1.add(id);
  w.add_document(doc1)?;
  let mut r = directory_reader::open_from_writer(&w)?;
  assert_eq!(2, r.num_docs()?);
  r.close()?;

  w.delete_documents_with_terms(vec![Term::from_text("id", "0")])?;
  r = directory_reader::open_from_writer(&w)?;
  assert_eq!(1, r.num_docs()?);
  r.close()?;

  w.delete_documents_with_terms(vec![Term::from_text("id", "1")])?;
  r = directory_reader::open_from_writer(&w)?;
  assert_eq!(0, r.num_docs()?);
  r.close()?;

  w.close()?;
  Ok(())
}

#[test]
fn test_empty_index() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let w = IndexWriter::new(dir.clone(), new_index_writer_config(&mut random)?)?;
  let r = directory_reader::open_from_writer(&w)?;
  assert_eq!(0, r.num_docs()?);
  r.close()?;
  w.close()?;
  Ok(())
}

#[test]
fn test_segment_warmer() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let did_warm = Arc::new(AtomicBool::new(false));
  let cms = ConcurrentMergeScheduler::new();
  let mock = MockAnalyzer::new(&mut random);
  let mut merge_policy = new_log_merge_policy_with_merge_factor(&mut random, 10)?;
  match &mut merge_policy {
    MergePolicyEnum::LogDoc(merge_policy) => merge_policy.set_target_search_concurrency(1)?,
    MergePolicyEnum::LogBytesSize(merge_policy) => merge_policy.set_target_search_concurrency(1)?,
    _ => panic!("expected LogMergePolicy variant"),
  }
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  iwc
    .set_max_buffered_docs(2)
    .set_reader_pooling(true)
    .set_merged_segment_warmer(Some(IndexReaderWarmerEnum::custom(AssertingTermWarmer {
      did_warm: did_warm.clone(),
    })))
    .set_merge_scheduler(cms.clone())
    .set_merge_policy(merge_policy);
  let w = IndexWriter::new(dir, iwc)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("foo", "bar", Store::No)?);
  for _ in 0..20 {
    w.add_document(doc.clone())?;
  }
  cms.sync()?;
  w.close()?;
  assert!(did_warm.load(AtomicOrdering::SeqCst));
  Ok(())
}

#[test]
fn test_simple_merged_segment_warmer() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let did_warm = Arc::new(AtomicBool::new(false));
  let info_stream = Arc::new(InfoStreamEnum::Custom(Box::new(SmswInfoStream {
    did_warm: did_warm.clone(),
  })));
  let cms = ConcurrentMergeScheduler::new();
  let mock = MockAnalyzer::new(&mut random);
  let mut merge_policy = new_log_merge_policy_with_merge_factor(&mut random, 10)?;
  match &mut merge_policy {
    MergePolicyEnum::LogDoc(merge_policy) => merge_policy.set_target_search_concurrency(1)?,
    MergePolicyEnum::LogBytesSize(merge_policy) => merge_policy.set_target_search_concurrency(1)?,
    _ => panic!("expected LogMergePolicy variant"),
  }
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  iwc
    .set_max_buffered_docs(2)
    .set_reader_pooling(true)
    .set_info_stream(info_stream.clone())
    .set_merged_segment_warmer(Some(SimpleMergedSegmentWarmer::new(info_stream).into()))
    .set_merge_scheduler(cms.clone())
    .set_merge_policy(merge_policy);
  let w = IndexWriter::new(dir, iwc)?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("foo", "bar", Store::No)?);
  for _ in 0..20 {
    w.add_document(doc.clone())?;
  }
  cms.sync()?;
  w.close()?;
  assert!(did_warm.load(AtomicOrdering::SeqCst));
  Ok(())
}

#[test]
fn test_reopen_after_no_real_change() -> Result<()> {
  let mut random = random();
  let d = new_directory_shared(&mut random)?;
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_max_full_flush_merge_wait_millis(0);
  let w = IndexWriter::new(d.clone(), iwc)?;

  let r = directory_reader::open_from_writer(&w)?;

  let r2 = directory_reader::open_if_changed(&r)?;
  assert!(r2.is_none());

  w.add_document(Document::new())?;
  let r3 = directory_reader::open_if_changed(&r)?;
  assert!(r3.is_some());
  let r3 = r3.unwrap();
  assert!(r3.get_version()? != r.get_version()?);
  assert!(r3.is_current()?);

  w.delete_documents_with_terms(vec![Term::from_text("foo", "bar")])?;

  assert!(!r3.is_current()?);
  let r4 = directory_reader::open_if_changed(&r3)?;
  assert!(r4.is_none());

  w.delete_documents_with_terms(vec![Term::from_text("foo", "bar")])?;
  let r5 = directory_reader::open_if_changed_with_writer(&r3, &w)?;
  assert!(r5.is_none());

  r.close()?;
  r3.close()?;

  w.close()?;
  Ok(())
}

#[test]
fn test_nrt_open_exceptions() -> Result<()> {
  // LUCENE-5262: test that several failed attempts to obtain an NRT reader
  // don't leak file handles.
  let mut random = random();
  let dir = Arc::new(new_mock_directory(&mut random)?);
  let should_fail = Arc::new(AtomicBool::new(false));
  dir.fail_on(Box::new(FailOnGetReadOnlyClone {
    do_fail: false,
    should_fail: should_fail.clone(),
  }));

  let mock = MockAnalyzer::new(&mut random);
  let mut conf = new_index_writer_config_with_analyzer(&mut random, mock)?;
  conf.set_max_full_flush_merge_wait_millis(0);
  conf.set_merge_policy(NoMergePolicy::default()); // prevent merges from getting in the way
  let writer = IndexWriter::new(dir.clone(), conf)?;

  // create a segment and open an NRT reader
  writer.add_document(Document::new())?;
  directory_reader::open_from_writer(&writer)?.close()?;

  // add a new document so a new NRT reader is required
  writer.add_document(Document::new())?;

  // try to obtain an NRT reader twice: first time it fails and closes all the
  // other NRT readers. second time it fails, but also fails to close the
  // other NRT reader, since it is already marked closed!
  for _ in 0..2 {
    should_fail.store(true, AtomicOrdering::SeqCst);
    match directory_reader::open_from_writer(&writer) {
      Err(LuceneError::Io { source, .. }) | Err(LuceneError::IoWithPath { source, .. }) => {
        assert!(
          source
            .get_ref()
            .is_some_and(|source| source.is::<FakeIOException>()),
          "expected FakeIOException, got {source}"
        );
      },
      Err(error) => return Err(error),
      Ok(reader) => {
        reader.close()?;
        panic!("expected FakeIOException");
      },
    }
  }

  writer.close()?;
  dir.as_ref().close()?;
  Ok(())
}

struct FailOnGetReadOnlyClone {
  do_fail: bool,
  should_fail: Arc<AtomicBool>,
}

impl<D> Failure<D> for FailOnGetReadOnlyClone
where
  D: Directory,
{
  fn eval(&mut self, _dir: &MockDirectoryWrapper<D>) -> Result<()> {
    if self.should_fail.load(AtomicOrdering::SeqCst)
      && call_stack_contains_any_of(&["get_read_only_clone"])
    {
      if cfg!(feature = "test_log_verbose") {
        println!("TEST: now fail; exc:");
      }
      self.should_fail.store(false, AtomicOrdering::SeqCst);
      return Err(LuceneError::io(Error::other(FakeIOException)));
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

#[test]
fn test_too_many_segments() -> Result<()> {
  let mut random = random();
  let dir = Arc::new(ByteBuffersDirectory::new());
  // Don't use new_index_writer_config, because we need a "sane" merge policy:
  let mut iwc = IndexWriterConfig::with_analyzer(MockAnalyzer::new(&mut random))?;
  iwc.set_max_full_flush_merge_wait_millis(0);
  let w = IndexWriter::new(dir.clone(), iwc)?;

  // Create 500 segments:
  for i in 0..500 {
    let mut doc = Document::new();
    doc.add(StringField::from_string("id", i.to_string(), Store::No)?);
    w.add_document(doc)?;
    let r = directory_reader::open_from_writer(&w)?;
    let context = (&r).get_context()?;
    // Make sure segment count never exceeds 100:
    assert!(context.leaves()?.len() < 100);
    r.close()?;
  }
  w.close()?;
  dir.close()
}

#[test]
fn test_reopen_nrt_reader_on_commit() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let iwc = IndexWriterConfig::with_analyzer(MockAnalyzer::new(&mut random))?;
  let w = IndexWriter::new(dir.clone(), iwc)?;
  w.add_document(Document::new())?;

  let r1 = directory_reader::open_from_writer(&w)?;
  let r1_context = (&r1).get_context()?;
  assert_eq!(1, r1_context.leaves()?.len());
  w.add_document(Document::new())?;
  w.commit()?;

  let commits = directory_reader::list_commits(dir.clone())?;
  assert_eq!(1, commits.len());
  let r2 = directory_reader::open_if_changed_with_commit(&r1, Some(&commits[0]))?
    .expect("commit should produce changed reader");
  let r2_context = (&r2).get_context()?;
  assert_eq!(2, r2_context.leaves()?.len());

  assert!(Arc::ptr_eq(
    r1_context.leaves()?[0].reader(),
    r2_context.leaves()?[0].reader()
  ));
  r1.close()?;
  r2.close()?;
  w.close()?;
  Ok(())
}

#[test]
fn test_index_reader_writer_with_leaf_sorter() -> Result<()> {
  let mut random = random();
  let field_name = "field1";
  let asc_sort = random.random_bool(0.5);
  let missing_value: i64 = if asc_sort { i64::MAX } else { i64::MIN };

  let point_sorter = Arc::new(PointValueLeafSorter {
    asc_sort,
    field_name: field_name.to_string(),
    missing_value,
  });
  let leaf_sorter = {
    let s = Arc::clone(&point_sorter);
    Some(LeafSorter::Custom(Arc::new(move |a, b| s.compare(a, b))))
  };

  let num_docs = at_least(&mut random, 30);
  let dir = new_directory_shared(&mut random)?;
  let mut iwc = IndexWriterConfig::new()?;
  iwc.set_leaf_sorter(leaf_sorter.clone());
  let writer = IndexWriter::new(dir.clone(), iwc)?;
  for i in 0..num_docs {
    let mut doc = Document::new();
    doc.add(LongPoint::new(
      field_name,
      [random.random_range(1..100) as i64],
    )?);
    writer.add_document(doc)?;
    if i > 0 && i % 10 == 0 {
      writer.flush()?;
    }
  }

  // Test1: test that leafReaders are sorted according to leafSorter provided in
  // IndexWriterConfig
  {
    let reader = directory_reader::open_from_writer(&writer)?;
    assert_leaves_sorted(&reader, &point_sorter)?;

    let first_value: i64 = if asc_sort { 0 } else { 100 };
    for _i in 0..10 {
      let mut doc = Document::new();
      doc.add(LongPoint::new(field_name, [first_value])?);
      writer.add_document(doc)?;
    }
    writer.commit()?;

    let reader2 = directory_reader::open_if_changed(&reader)?.expect("reader should have changed");
    assert_leaves_sorted(&reader2, &point_sorter)?;
    reader.close()?;
    reader2.close()?;
  }

  // Test2: test that leafReaders are sorted according to the provided leafSorter
  // when opened from directory
  {
    let reader = directory_reader::open_with_sorter(dir.clone(), leaf_sorter.clone())?;
    assert_leaves_sorted(&reader, &point_sorter)?;

    let first_value: i64 = if asc_sort { 0 } else { 100 };
    for _i in 0..10 {
      let mut doc = Document::new();
      doc.add(LongPoint::new(field_name, [first_value])?);
      writer.add_document(doc)?;
    }
    writer.commit()?;

    let reader2 = directory_reader::open_if_changed(&reader)?.expect("reader should have changed");
    assert_leaves_sorted(&reader2, &point_sorter)?;
    reader.close()?;
    reader2.close()?;
  }

  // Test3: test that FilterDirectoryReader sorts leaves according
  // to leafSorter of its wrapped reader
  // TODO: AssertingDirectoryReader is not implemented, so Test3 currently exercises the leaf
  // sorter without Java's FilterDirectoryReader wrapper layer.
  {
    let reader = directory_reader::open_with_sorter(dir.clone(), leaf_sorter.clone())?;
    assert_leaves_sorted(&reader, &point_sorter)?;

    let first_value: i64 = if asc_sort { 0 } else { 100 };
    for _i in 0..10 {
      let mut doc = Document::new();
      doc.add(LongPoint::new(field_name, [first_value])?);
      writer.add_document(doc)?;
    }
    writer.commit()?;

    let reader2 = directory_reader::open_if_changed(&reader)?.expect("reader should have changed");
    assert_leaves_sorted(&reader2, &point_sorter)?;
    reader.close()?;
    reader2.close()?;
  }

  // Test4: test that leafReaders are sorted according to the provided leafSorter
  // when opened from commit
  {
    let commits = directory_reader::list_commits(dir.clone())?;
    let latest_commit = &commits[commits.len() - 1];
    let reader = StandardDirectoryReader::open(
      latest_commit.get_directory(),
      Some(latest_commit),
      leaf_sorter.clone(),
    )?;
    assert_leaves_sorted(&reader, &point_sorter)?;

    let first_value: i64 = if asc_sort { 0 } else { 100 };
    for _i in 0..10 {
      let mut doc = Document::new();
      doc.add(LongPoint::new(field_name, [first_value])?);
      writer.add_document(doc)?;
    }
    writer.commit()?;

    let reader2 = directory_reader::open_if_changed(&reader)?.expect("reader should have changed");
    assert_leaves_sorted(&reader2, &point_sorter)?;
    reader.close()?;
    reader2.close()?;
  }

  writer.close()?;
  Ok(())
}

/// Assert that the leaf readers of the provided directory reader are sorted
/// according to the provided leafSorter.
///
/// [Java reference: TestIndexWriterReader.assertLeavesSorted]
fn assert_leaves_sorted<D>(
  reader: &StandardDirectoryReader<D>,
  sorter: &PointValueLeafSorter,
) -> Result<()>
where
  D: Directory,
{
  let context = reader.get_context()?;
  let leaves = context.leaves()?;
  let lrs: Vec<_> = leaves.iter().map(|l| Arc::clone(l.reader())).collect();
  let mut expected = lrs.clone();
  expected.sort_by(|a, b| sorter.compare(a, b).unwrap().cmp(&0));
  for (i, lr) in lrs.iter().enumerate() {
    assert!(
      Arc::ptr_eq(lr, &expected[i]),
      "leaf readers not sorted at index {}",
      i
    );
  }
  Ok(())
}

#[derive(Clone)]
pub struct PointValueLeafSorter {
  asc_sort: bool,
  field_name: String,
  missing_value: i64,
}

impl PointValueLeafSorter {
  fn sort_key<D>(&self, reader: &DefaultLeafReader<D>) -> Result<i64>
  where
    D: Directory,
  {
    let result = (|| -> Result<i64> {
      let Some(points) = reader.get_point_values(&self.field_name)? else {
        return Ok(self.missing_value);
      };
      let sort_value = if self.asc_sort {
        points.get_min_packed_value()?
      } else {
        points.get_max_packed_value()?
      };
      Ok(
        sort_value
          .map(|value| LongPoint::decode_dimension(&value, 0))
          .unwrap_or(self.missing_value),
      )
    })();
    Ok(result.unwrap_or(self.missing_value))
  }
}

impl<D> Comparator<DefaultLeafReader<D>> for PointValueLeafSorter
where
  D: Directory,
{
  const TYPE: &'static str = "PointValueLeafSorter";

  fn compare(&self, a: &DefaultLeafReader<D>, b: &DefaultLeafReader<D>) -> Result<i32> {
    let ord = self.sort_key(a)?.cmp(&self.sort_key(b)?);
    let ord = if self.asc_sort { ord } else { ord.reverse() };

    Ok(match ord {
      Ordering::Less => -1,
      Ordering::Equal => 0,
      Ordering::Greater => 1,
    })
  }
}
struct CountingWarmer {
  warm_count: Arc<AtomicUsize>,
}

impl<D> IndexReaderWarmer<D> for CountingWarmer
where
  D: Directory,
{
  fn warm(&self, _reader: &DefaultLeafReader<D>) -> Result<()> {
    self.warm_count.fetch_add(1, AtomicOrdering::SeqCst);
    Ok(())
  }
}

struct AssertingTermWarmer {
  did_warm: Arc<AtomicBool>,
}

impl<D> IndexReaderWarmer<D> for AssertingTermWarmer
where
  D: Directory + 'static,
{
  fn warm(&self, reader: &DefaultLeafReader<D>) -> Result<()> {
    let searcher = index_searcher::from_reader(reader.clone())?;
    let count = searcher.count(TermQuery::new(Term::from_text("foo", "bar")))?;
    assert_eq!(20, count);
    self.did_warm.store(true, AtomicOrdering::SeqCst);
    Ok(())
  }
}

struct SmswInfoStream {
  did_warm: Arc<AtomicBool>,
}

impl CloseableRef for SmswInfoStream {
  fn close(&self) -> Result<()> {
    Ok(())
  }
}

impl InfoStream for SmswInfoStream {
  fn message(&self, component: &str, _message: &str) -> Result<()> {
    if component == "SMSW" {
      self.did_warm.store(true, AtomicOrdering::SeqCst);
    }
    Ok(())
  }

  fn is_enabled(&self, _component: &str) -> bool {
    true
  }
}
