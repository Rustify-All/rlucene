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
use crate::core::analysis::analyzer::Analyzer;
use crate::core::document::document::Document;
use crate::core::document::field::Store;
use crate::core::document::field_type::FieldType;
use crate::core::index::base_composite_reader::{
  BCRStoredFieldsImpl, BCRTermVectorsImpl, BaseCompositeReader, BaseCompositeReaderBase,
};
use crate::core::index::composite_reader::CompositeReader;
use crate::core::index::concurrent_merge_scheduler::ConcurrentMergeScheduler;
use crate::core::index::directory_reader::{self, DirectoryReader, DirectoryReaderBase};
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::filter_directory_reader::{FilterDirectoryReader, SubReaderWrapper};
use crate::core::index::filter_leaf_reader::FilterLeafReader;
use crate::core::index::index_reader::{
  CacheHelper, CompositeReaderContextKind, IndexReader, IndexReaderBase, LeafReaderContextKind,
};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::{IndexWriter, MAX_TERM_LENGTH};
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::leaf_metadata::LeafMetaData;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::standard_directory_reader::StandardDirectoryReader;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::search::reference_manager::RefreshListener;
use crate::core::search::searcher_factory::{SearcherFactory, SearcherFactoryHook};
use crate::core::search::searcher_lifetime_manager::{PruneByAge, SearcherLifetimeManager};
use crate::core::search::searcher_manager::SearcherManager;
use crate::core::store::directory::{DirEnum, Directory, MockDirWrapper};
use crate::core::util::bits::Bits;
use crate::core::util::close::CloseableRef;
use crate::core::util::dummy::dummy_comparator::DummyComparator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::index::test_concurrent_merge_scheduler::CountDownLatch;
use crate::test_framework::core::index::threaded_indexing_and_searching_test_case::{
  ThreadedIndexSearcher, ThreadedIndexingAndSearchingTestCase,
  ThreadedIndexingAndSearchingTestCaseState,
};
use crate::test_framework::core::search::test_searcher_manager::{
  BlockingSearcherFactory, EvilSearcherFactory, TrackingSearcherFactory,
  TrackingSearcherFactoryState, WarmingSearcherFactory,
};
use crate::test_framework::core::util::lucene_test_case::{
  at_least, ensure_sane_iwc_on_nightly, is_night_mode, new_directory_shared, new_fs_directory,
  new_index_writer_config, new_index_writer_config_with_analyzer, new_text_field, random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use parking_lot::{Mutex, RwLock};
use rand::prelude::StdRng;
use rand::{RngExt, SeedableRng};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

type TestDirectory = MockDirWrapper;
type TestDirectoryReader = StandardDirectoryReader<TestDirectory>;
type TestIndexSearcher = ThreadedIndexSearcher<TestDirectoryReader>;

#[allow(dead_code)] // for quick search
pub(crate) struct TestSearcherManager {
  state: ThreadedIndexingAndSearchingTestCaseState<TestDirectory>,
  warm_called: Arc<AtomicBool>,
  pruner: OnceLock<PruneByAge>,
  mgr: OnceLock<Arc<SearcherManager<TestDirectoryReader>>>,
  lifetime_mgr: OnceLock<Arc<SearcherLifetimeManager<TestDirectoryReader>>>,
  past_searchers: Mutex<Vec<i64>>,
  is_nrt: AtomicBool,
}

impl TestSearcherManager {
  fn new() -> Self {
    Self {
      state: ThreadedIndexingAndSearchingTestCaseState::new(),
      warm_called: Arc::new(AtomicBool::new(false)),
      pruner: OnceLock::new(),
      mgr: OnceLock::new(),
      lifetime_mgr: OnceLock::new(),
      past_searchers: Mutex::new(Vec::new()),
      is_nrt: AtomicBool::new(false),
    }
  }
}

#[test]
fn test_searcher_manager() -> Result<()> {
  let mut random = random();
  let case = TestSearcherManager::new();
  assert!(
    case
      .pruner
      .set(PruneByAge::new(if is_night_mode() {
        TestUtil::next_int(&mut random, 1, 20) as f64
      } else {
        1.0
      })?)
      .is_ok()
  );
  case.run_test(&mut random, "TestSearcherManager")
}

impl ThreadedIndexingAndSearchingTestCase for TestSearcherManager {
  type Directory = TestDirectory;
  type Reader = TestDirectoryReader;

  fn state(&self) -> &ThreadedIndexingAndSearchingTestCaseState<Self::Directory> {
    &self.state
  }

  fn get_final_searcher(
    &self,
    _random: &mut StdRng,
  ) -> Result<Arc<ThreadedIndexSearcher<Self::Reader>>> {
    if !self.is_nrt.load(Ordering::Relaxed) {
      self.state.writer().commit()?;
    }
    let mgr = self
      .mgr
      .get()
      .expect("SearcherManager has not been initialized");
    assert!(mgr.maybe_refresh()? || mgr.is_searcher_current()?);
    mgr.acquire()
  }

  fn do_after_writer(&self, random: &mut StdRng, search_threads: Option<usize>) -> Result<()> {
    let factory = SearcherFactory::with_hook(SearcherFactoryHook::Warming(
      WarmingSearcherFactory::new(self.warm_called.clone(), search_threads),
    ));
    let mgr = if random.random_bool(0.5) {
      // TODO: can we randomize the applyAllDeletes?  But
      // somehow for final searcher we must apply
      // deletes...
      self.is_nrt.store(true, Ordering::Relaxed);
      SearcherManager::from_writer(&self.state.writer(), Some(factory))?
    } else {
      // SearcherManager needs to see empty commit:
      self.state.writer().commit()?;
      self.is_nrt.store(false, Ordering::Relaxed);
      self
        .state
        .assert_merged_segments_warmed
        .store(false, Ordering::Relaxed);
      SearcherManager::from_directory(self.state.directory(), Some(factory))?
    };
    assert!(self.mgr.set(Arc::new(mgr)).is_ok());
    assert!(
      self
        .lifetime_mgr
        .set(Arc::new(SearcherLifetimeManager::new()))
        .is_ok()
    );
    Ok(())
  }

  fn do_searching(&self, random: &mut StdRng, max_iterations: i32) -> Result<()> {
    let reopen_seed = random.random::<u64>();
    thread::scope(|scope| -> Result<()> {
      let reopen_thread = scope.spawn(move || -> Result<()> {
        let mut random = StdRng::seed_from_u64(reopen_seed);
        let result = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
          let mut iterations = 0;
          while {
            iterations += 1;
            iterations < max_iterations
          } {
            thread::sleep(Duration::from_millis(
              TestUtil::next_int(&mut random, 1, 100) as u64,
            ));
            self.state.writer().commit()?;
            thread::sleep(Duration::from_millis(
              TestUtil::next_int(&mut random, 1, 5) as u64
            ));
            if random.random_bool(0.5) {
              self
                .mgr
                .get()
                .expect("SearcherManager has not been initialized")
                .maybe_refresh_blocking()?;
              self
                .lifetime_mgr
                .get()
                .expect("SearcherLifetimeManager has not been initialized")
                .prune(
                  self
                    .pruner
                    .get()
                    .expect("SearcherLifetimeManager pruner has not been initialized"),
                )?;
            } else if self
              .mgr
              .get()
              .expect("SearcherManager has not been initialized")
              .maybe_refresh()?
            {
              self
                .lifetime_mgr
                .get()
                .expect("SearcherLifetimeManager has not been initialized")
                .prune(
                  self
                    .pruner
                    .get()
                    .expect("SearcherLifetimeManager pruner has not been initialized"),
                )?;
            }
          }
          Ok(())
        }));
        match result {
          Ok(Ok(())) => Ok(()),
          Ok(Err(error)) => {
            self.state.failed.store(true, Ordering::SeqCst);
            Err(error)
          },
          Err(payload) => {
            self.state.failed.store(true, Ordering::SeqCst);
            resume_unwind(payload)
          },
        }
      });

      self.run_search_threads(random, max_iterations)?;
      let _ = reopen_thread.join();
      Ok(())
    })
  }

  fn get_current_searcher(
    &self,
    random: &mut StdRng,
  ) -> Result<Arc<ThreadedIndexSearcher<Self::Reader>>> {
    if random.random_range(0..10) == 7 {
      // NOTE: not best practice to call maybeRefresh
      // synchronous to your search threads, but still we
      // test as apps will presumably do this for
      // simplicity:
      if self
        .mgr
        .get()
        .expect("SearcherManager has not been initialized")
        .maybe_refresh()?
      {
        self
          .lifetime_mgr
          .get()
          .expect("SearcherLifetimeManager has not been initialized")
          .prune(
            self
              .pruner
              .get()
              .expect("SearcherLifetimeManager pruner has not been initialized"),
          )?;
      }
    }

    let mut searcher: Option<Arc<TestIndexSearcher>> = None;
    {
      let mut past_searchers = self.past_searchers.lock();
      while !past_searchers.is_empty() && random.random::<f64>() < 0.25 {
        // 1/4 of the time pull an old searcher, ie, simulate
        // a user doing a follow-on action on a previous
        // search (drilling down/up, clicking next/prev page,
        // etc.)
        let token_index = random.random_range(0..past_searchers.len());
        let token = past_searchers[token_index];
        searcher = self
          .lifetime_mgr
          .get()
          .expect("SearcherLifetimeManager has not been initialized")
          .acquire(token)?;
        if searcher.is_none() {
          // Searcher was pruned
          past_searchers.remove(token_index);
        } else {
          break;
        }
      }
    }

    if searcher.is_none() {
      let current = self
        .mgr
        .get()
        .expect("SearcherManager has not been initialized")
        .acquire()?;
      if current.get_index_reader().num_docs()? != 0 {
        let token = self
          .lifetime_mgr
          .get()
          .expect("SearcherLifetimeManager has not been initialized")
          .record(&current)?;
        let mut past_searchers = self.past_searchers.lock();
        if !past_searchers.contains(&token) {
          past_searchers.push(token);
        }
      }
      searcher = Some(current);
    }

    Ok(searcher.expect("a current or past searcher must be available"))
  }

  fn release_searcher(&self, searcher: Arc<ThreadedIndexSearcher<Self::Reader>>) -> Result<()> {
    searcher.get_index_reader().dec_ref()
  }

  fn do_close(&self) -> Result<()> {
    assert!(self.warm_called.load(Ordering::Relaxed));
    self
      .mgr
      .get()
      .expect("SearcherManager has not been initialized")
      .close()?;
    self
      .lifetime_mgr
      .get()
      .expect("SearcherLifetimeManager has not been initialized")
      .close()
  }

  fn get_directory(&self, directory: Arc<Self::Directory>) -> Arc<Self::Directory> {
    // don't double-checkIndex, we do it ourselves.
    directory.set_check_index_on_close(false);
    directory
  }
}

#[test]
fn test_intermediate_close() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  // Test can deadlock if we use SMS:
  let analyzer = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  config.set_merge_scheduler(ConcurrentMergeScheduler::new());
  let writer = IndexWriter::new(directory.clone(), config)?;
  writer.add_document(Document::new())?;
  writer.commit()?;
  let await_enter_warm = CountDownLatch::new(1);
  let await_close = CountDownLatch::new(1);
  let tried_reopen = Arc::new(AtomicBool::new(false));
  let search_threads = if random.random_bool(0.5) {
    None
  } else {
    Some(2)
  };
  let factory =
    SearcherFactory::with_hook(SearcherFactoryHook::Blocking(BlockingSearcherFactory::new(
      tried_reopen.clone(),
      await_enter_warm.clone(),
      await_close.clone(),
      search_threads,
    )));
  let searcher_manager = Arc::new(if random.random_bool(0.5) {
    SearcherManager::from_directory(directory.clone(), Some(factory))?
  } else {
    SearcherManager::with_writer_deletes(&writer, random.random_bool(0.5), false, Some(factory))?
  });
  let searcher = searcher_manager.acquire()?;
  let num_docs_result = catch_unwind(AssertUnwindSafe(|| searcher.get_index_reader().num_docs()));
  searcher_manager.release(searcher)?;
  match num_docs_result {
    Ok(result) => assert_eq!(1, result?),
    Err(payload) => resume_unwind(payload),
  }
  writer.add_document(Document::new())?;
  writer.commit()?;

  let refresh_manager = searcher_manager.clone();
  let refresh_tried_reopen = tried_reopen.clone();
  let refresh_thread = thread::spawn(move || -> Result<bool> {
    refresh_tried_reopen.store(true, Ordering::SeqCst);
    match refresh_manager.maybe_refresh() {
      Ok(_) => Ok(true),
      // expected
      Err(LuceneError::AlreadyClosed(_)) => Ok(false),
      Err(error) => Err(error),
    }
  });
  await_enter_warm.wait();
  searcher_manager.close()?;
  await_close.count_down();
  let success = match refresh_thread.join() {
    Ok(result) => result?,
    Err(payload) => std::panic::resume_unwind(payload),
  };
  assert!(matches!(
    searcher_manager.acquire(),
    Err(LuceneError::AlreadyClosed(_))
  ));
  assert!(!success);
  assert!(tried_reopen.load(Ordering::SeqCst));
  writer.close()?;
  directory.close()?;
  Ok(())
}

#[test]
fn test_close_twice() -> Result<()> {
  // test that we can close SM twice (per Closeable's contract).
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  let config = IndexWriterConfig::new()?;
  IndexWriter::new(directory.clone(), config)?.close()?;
  let sm = SearcherManager::from_directory(directory.clone(), None)?;
  sm.close()?;
  sm.close()?;
  directory.close()?;
  Ok(())
}

#[test]
fn test_reference_decrement_illegally() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  let analyzer = MockAnalyzer::new(&mut random);
  let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer)?;
  config.set_merge_scheduler(ConcurrentMergeScheduler::new());
  let writer = IndexWriter::new(directory.clone(), config)?;
  let sm =
    SearcherManager::with_writer_deletes(&writer, false, false, Some(SearcherFactory::new()))?;
  writer.add_document(Document::new())?;
  writer.commit()?;
  sm.maybe_refresh_blocking()?;

  let acquire = sm.acquire()?;
  let acquire2 = sm.acquire()?;
  sm.release(acquire)?;
  sm.release(acquire2)?;

  let acquire = sm.acquire()?;
  acquire.get_index_reader().dec_ref()?;
  sm.release(acquire)?;
  assert!(matches!(sm.acquire(), Err(LuceneError::IllegalState(_))));

  // searcher_manager.close(); -- already closed
  writer.close()?;
  directory.close()?;
  Ok(())
}

#[test]
fn test_ensure_open() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  let config = IndexWriterConfig::new()?;
  IndexWriter::new(directory.clone(), config)?.close()?;
  let sm = SearcherManager::from_directory(directory.clone(), None)?;
  let searcher = sm.acquire()?;
  sm.close()?;

  // this should succeed;
  sm.release(searcher)?;

  // this should fail
  assert!(matches!(sm.acquire(), Err(LuceneError::AlreadyClosed(_))));

  // this should fail
  assert!(matches!(
    sm.maybe_refresh(),
    Err(LuceneError::AlreadyClosed(_))
  ));

  directory.close()?;
  Ok(())
}

struct AfterRefreshCalled {
  called: Arc<AtomicBool>,
}

impl RefreshListener for AfterRefreshCalled {
  fn before_refresh(&self) -> Result<()> {
    Ok(())
  }

  fn after_refresh(&self, did_refresh: bool) -> Result<()> {
    if did_refresh {
      self.called.store(true, Ordering::SeqCst);
    }
    Ok(())
  }
}

#[test]
fn test_listener_called() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  let config = IndexWriterConfig::new()?;
  let writer = IndexWriter::new(directory.clone(), config)?;
  let after_refresh_called = Arc::new(AtomicBool::new(false));
  let sm =
    SearcherManager::with_writer_deletes(&writer, false, false, Some(SearcherFactory::new()))?;
  sm.add_listener(Arc::new(AfterRefreshCalled {
    called: after_refresh_called.clone(),
  }));
  writer.add_document(Document::new())?;
  writer.commit()?;
  assert!(!after_refresh_called.load(Ordering::SeqCst));
  sm.maybe_refresh_blocking()?;
  assert!(after_refresh_called.load(Ordering::SeqCst));
  sm.close()?;
  writer.close()?;
  directory.close()?;
  Ok(())
}

#[test]
fn test_evil_searcher_factory() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, directory.clone())?;
  writer.commit(&mut random)?;

  let other = Arc::new(directory_reader::open(directory.clone())?);

  let result = SearcherManager::from_directory(
    directory.clone(),
    Some(SearcherFactory::with_hook(SearcherFactoryHook::Evil(
      EvilSearcherFactory::new(other.clone(), random.random()),
    ))),
  );
  assert!(matches!(result, Err(LuceneError::IllegalState(_))));
  let result = SearcherManager::with_writer_deletes(
    &writer.w,
    random.random_bool(0.5),
    false,
    Some(SearcherFactory::with_hook(SearcherFactoryHook::Evil(
      EvilSearcherFactory::new(other.clone(), random.random()),
    ))),
  );
  assert!(matches!(result, Err(LuceneError::IllegalState(_))));
  writer.close(&mut random)?;
  other.close()?;
  directory.close()?;
  Ok(())
}

#[test]
fn test_maybe_refresh_blocking_lock() -> Result<()> {
  // make sure that maybeRefreshBlocking releases the lock, otherwise other
  // threads cannot obtain it.
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, directory.clone())?;
  writer.close(&mut random)?;

  let searcher_manager = Arc::new(SearcherManager::from_directory(directory.clone(), None)?);
  let refresh_manager = searcher_manager.clone();
  let refresh_thread = thread::spawn(move || refresh_manager.maybe_refresh_blocking());
  match refresh_thread.join() {
    Ok(result) => result?,
    Err(payload) => std::panic::resume_unwind(payload),
  }

  // if maybeRefreshBlocking didn't release the lock, this will fail.
  assert!(searcher_manager.maybe_refresh()?);

  searcher_manager.close()?;
  directory.close()?;
  Ok(())
}

struct MyFilterLeafReader<LR>
where
  LR: LeafReader,
{
  in_: Arc<LR>,
  index_base: Arc<IndexReaderBase>,
}

impl<LR> MyFilterLeafReader<LR>
where
  LR: LeafReader,
{
  fn new(in_: LR) -> Self {
    Self {
      in_: Arc::new(in_),
      index_base: Arc::new(IndexReaderBase::new()),
    }
  }

  fn get_delegate(&self) -> &LR {
    self.in_.as_ref()
  }
}

impl<LR> Clone for MyFilterLeafReader<LR>
where
  LR: LeafReader + Clone,
{
  fn clone(&self) -> Self {
    Self {
      in_: self.in_.clone(),
      index_base: self.index_base.clone(),
    }
  }
}

impl<LR> Display for MyFilterLeafReader<LR>
where
  LR: LeafReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "MyFilterLeafReader({})", self.in_)
  }
}

impl<LR> FilterLeafReader for MyFilterLeafReader<LR> where LR: LeafReader {}

impl<LR> IndexReader for MyFilterLeafReader<LR>
where
  LR: LeafReader,
{
  type ContextKind = LeafReaderContextKind;
  type TermVectors = LR::TermVectors;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    self.in_.term_vectors()
  }

  fn max_doc(&self) -> Result<i32> {
    self.in_.max_doc()
  }

  fn num_docs(&self) -> Result<i32> {
    self.in_.num_docs()
  }

  type StoredFields = LR::StoredFields;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    self.in_.stored_fields()
  }

  fn do_close(&self) -> Result<()> {
    self.in_.close()
  }

  type ReaderCacheHelper = LR::ReaderCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    self.in_.get_reader_cache_helper()
  }

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    IndexReader::doc_freq(&self.in_, term)
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    self.in_.total_term_freq(term)
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    IndexReader::get_sum_doc_freq(&self.in_, field)
  }

  fn get_doc_count(&self, field: &str) -> Result<i32> {
    IndexReader::get_doc_count(&self.in_, field)
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    IndexReader::get_sum_total_term_freq(&self.in_, field)
  }

  fn index_base(&self) -> &IndexReaderBase {
    self.index_base.as_ref()
  }
}

impl<LR> LeafReader for MyFilterLeafReader<LR>
where
  LR: LeafReader,
{
  type CacheHelper = LR::CacheHelper;

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    self.in_.get_core_cache_helper()
  }

  type Terms = LR::Terms;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    self.in_.terms(field)
  }

  type NumericDocValues = LR::NumericDocValues;

  fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>> {
    self.in_.get_numeric_doc_values(field)
  }

  type BinaryDocValues = LR::BinaryDocValues;

  fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
    self.in_.get_binary_doc_values(field)
  }

  type SortedDocValues = LR::SortedDocValues;

  fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
    self.in_.get_sorted_doc_values(field)
  }

  type SortedNumericDocValues = LR::SortedNumericDocValues;

  fn get_sorted_numeric_doc_values(
    &self,
    field: &str,
  ) -> Result<Option<Self::SortedNumericDocValues>> {
    self.in_.get_sorted_numeric_doc_values(field)
  }

  type SortedSetDocValues = LR::SortedSetDocValues;

  fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>> {
    self.in_.get_sorted_set_doc_values(field)
  }

  type NormNumericDocValues = LR::NormNumericDocValues;

  fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>> {
    self.in_.get_norm_values(field)
  }

  type DocValuesSkipper = LR::DocValuesSkipper;

  fn get_doc_values_skipper(&self, field: &str) -> Result<Option<Self::DocValuesSkipper>> {
    self.in_.get_doc_values_skipper(field)
  }

  type FloatVectorValues = LR::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Option<Self::FloatVectorValues>> {
    self.in_.get_float_vector_values(field)
  }

  type ByteVectorValues = LR::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Option<Self::ByteVectorValues>> {
    self.in_.get_byte_vector_values(field)
  }

  fn search_nearest_vectors_f32<B, K>(
    &self,
    field: &str,
    target: Vec<f32>,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
  {
    self
      .in_
      .search_nearest_vectors_f32(field, target, knn_collector, accept_docs)
  }

  fn search_nearest_vectors_u8<B, K>(
    &self,
    field: &str,
    target: Vec<u8>,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
  {
    self
      .in_
      .search_nearest_vectors_u8(field, target, knn_collector, accept_docs)
  }

  fn get_field_infos(&self) -> Result<Arc<FieldInfos>> {
    self.in_.get_field_infos()
  }

  type Bits = LR::Bits;

  fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
    self.in_.get_live_docs()
  }

  type PointValues = LR::PointValues;

  fn get_point_values(&self, field: &str) -> Result<Option<Self::PointValues>> {
    self.in_.get_point_values(field)
  }

  fn check_integrity(&self) -> Result<()> {
    self.in_.check_integrity()
  }

  fn get_metadata(&self) -> Result<&LeafMetaData> {
    self.in_.get_metadata()
  }
}

struct MySubReaderWrapper;

impl<LR> SubReaderWrapper<LR> for MySubReaderWrapper
where
  LR: LeafReader,
{
  type LeafReader1 = Self::LeafReader2;

  fn wrap_readers(&self, readers: Vec<LR>) -> Result<Vec<Self::LeafReader1>> {
    self.default_wrap_readers(readers)
  }

  type LeafReader2 = MyFilterLeafReader<LR>;

  fn wrap(&self, reader: LR) -> Result<Self::LeafReader2> {
    let reader_base = reader.index_base() as *const IndexReaderBase;
    let wrapped = MyFilterLeafReader::new(reader);
    assert_eq!(
      reader_base,
      wrapped.get_delegate().index_base() as *const IndexReaderBase
    );
    Ok(wrapped)
  }
}

struct MyFilterDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  in_: DR,
  base: BaseCompositeReaderBase<MyFilterLeafReader<DR::LeafReader>>,
  index_base: IndexReaderBase,
}

impl<DR> MyFilterDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  fn new(in_: DR) -> Result<Self> {
    let wrapper = MySubReaderWrapper;
    let readers = wrapper.wrap_readers(in_.get_sequential_sub_readers().to_vec())?;
    let base = BaseCompositeReaderBase::new::<DummyComparator>(readers, None)?;
    Ok(Self {
      in_,
      base,
      index_base: IndexReaderBase::new(),
    })
  }

  fn get_delegate(&self) -> &DR {
    &self.in_
  }
}

impl<DR> BaseCompositeReader for MyFilterDirectoryReader<DR> where DR: DirectoryReader {}

impl<DR> CompositeReader for MyFilterDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  type LeafReader = MyFilterLeafReader<DR::LeafReader>;
  type SubReader = Self::LeafReader;

  fn get_sequential_sub_readers(&self) -> &[Self::SubReader] {
    self.base.get_sequential_sub_readers()
  }

  fn visit_leaves<F>(&self, visitor: &mut F) -> Result<()>
  where
    F: FnMut(&Self::LeafReader) -> Result<()>,
  {
    for reader in self.get_sequential_sub_readers() {
      visitor(reader)?;
    }
    Ok(())
  }

  fn to_string(&self) -> String {
    format!("MyFilterDirectoryReader({})", self.in_.to_string())
  }
}

impl<DR> IndexReader for MyFilterDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  type ContextKind = CompositeReaderContextKind;
  type TermVectors = BCRTermVectorsImpl<<Self as CompositeReader>::LeafReader>;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    self.base.term_vector(self)
  }

  fn max_doc(&self) -> Result<i32> {
    Ok(self.base.max_doc())
  }

  fn num_docs(&self) -> Result<i32> {
    self.base.num_docs()
  }

  type StoredFields = BCRStoredFieldsImpl<<Self as CompositeReader>::LeafReader>;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    self.base.stored_fields(self)
  }

  fn do_close(&self) -> Result<()> {
    self.in_.close()
  }

  type ReaderCacheHelper = DR::ReaderCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    self.in_.get_reader_cache_helper()
  }

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    self.base.doc_freq(term, self)
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    self.base.total_term_freq(term, self)
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    self.base.get_sum_doc_freq(field, self)
  }

  fn get_doc_count(&self, field: &str) -> Result<i32> {
    self.base.get_doc_count(field, self)
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    self.base.get_sum_total_term_freq(field, self)
  }

  fn index_base(&self) -> &IndexReaderBase {
    &self.index_base
  }
}

impl<DR> Display for MyFilterDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", CompositeReader::to_string(self))
  }
}

impl<DR> DirectoryReader for MyFilterDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  type DirectoryReader = MyFilterDirectoryReader<DR::DirectoryReader>;

  fn do_open_if_changed(&self) -> Result<Option<Self::DirectoryReader>> {
    self.wrap_directory_reader(self.in_.do_open_if_changed()?)
  }

  fn do_open_if_changed_with_commit<IC>(
    &self,
    commit: Option<&IC>,
  ) -> Result<Option<Self::DirectoryReader>>
  where
    IC: crate::core::index::index_commit::IndexCommit<Directory = Arc<Self::Directory>>,
  {
    self.wrap_directory_reader(self.in_.do_open_if_changed_with_commit(commit)?)
  }

  fn do_open_if_changed_with_deletes(
    &self,
    writer: &Arc<IndexWriter<Self::Directory>>,
    apply_deletes: bool,
  ) -> Result<Option<Self::DirectoryReader>> {
    self.wrap_directory_reader(
      self
        .in_
        .do_open_if_changed_with_deletes(writer, apply_deletes)?,
    )
  }

  fn get_version(&self) -> Result<i64> {
    self.in_.get_version()
  }

  fn is_current(&self) -> Result<bool> {
    self.in_.is_current()
  }

  type IndexCommit = DR::IndexCommit;

  fn get_index_commit(&self) -> Result<Self::IndexCommit> {
    self.in_.get_index_commit()
  }

  type Directory = DR::Directory;

  fn directory(&self) -> &DirectoryReaderBase<Self::Directory> {
    self.in_.directory()
  }
}

impl<DR> FilterDirectoryReader for MyFilterDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  type Delegate = DR;

  fn get_delegate(&self) -> &Self::Delegate {
    &self.in_
  }

  type WrapDirectoryReader = MyFilterDirectoryReader<DR::DirectoryReader>;

  fn do_wrap_directory_reader(
    &self,
    in_: Option<<Self::Delegate as DirectoryReader>::DirectoryReader>,
  ) -> Result<Option<Self::WrapDirectoryReader>> {
    in_.map(MyFilterDirectoryReader::new).transpose()
  }
}

// LUCENE-6087
#[test]
fn test_custom_directory_reader() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, directory.clone())?;
  let nrt_reader = writer.get_reader(&mut random)?;
  let nrt_reader_key = nrt_reader
    .get_reader_cache_helper()?
    .expect("DirectoryReader must have a reader cache helper")
    .get_key();

  let reader = MyFilterDirectoryReader::new(nrt_reader)?;
  assert_eq!(
    nrt_reader_key,
    reader
      .get_delegate()
      .get_reader_cache_helper()?
      .expect("delegate DirectoryReader must have a reader cache helper")
      .get_key()
  );
  assert_eq!(
    nrt_reader_key,
    reader
      .get_reader_cache_helper()?
      .expect("filter DirectoryReader must delegate its reader cache helper")
      .get_key()
  );

  let manager = SearcherManager::new(reader, None)?;
  for _ in 0..10 {
    writer.add_document(&mut random, Document::new())?;
    manager.maybe_refresh()?;
    let searcher = manager.acquire()?;
    let search_result = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
      let _ = searcher.get_index_reader().get_delegate();
      for context in searcher.get_leaf_contexts()? {
        let _ = context.reader().get_delegate();
      }
      Ok(())
    }));
    manager.release(searcher)?;
    match search_result {
      Ok(result) => result?,
      Err(payload) => resume_unwind(payload),
    }
  }
  manager.close()?;
  writer.close(&mut random)?;
  directory.close()?;
  Ok(())
}

#[test]
fn test_previous_reader_is_passed() -> Result<()> {
  let mut random = random();
  let directory = new_directory_shared(&mut random)?;
  let config = new_index_writer_config::<DirEnum, _>(&mut random)?;
  let writer = IndexWriter::new(directory.clone(), config)?;
  writer.add_document(Document::new())?;
  let factory_state = Arc::new(TrackingSearcherFactoryState::default());
  let factory = SearcherFactory::with_hook(SearcherFactoryHook::Tracking(
    TrackingSearcherFactory::new(factory_state.clone()),
  ));
  let searcher_manager =
    SearcherManager::with_writer_deletes(&writer, random.random_bool(0.5), false, Some(factory))?;
  assert_eq!(1, factory_state.called.load(Ordering::Relaxed));
  assert!(factory_state.last_previous_reader.lock().is_none());
  let first_reader = factory_state
    .last_reader
    .lock()
    .as_ref()
    .expect("the initial reader must be passed to SearcherFactory")
    .clone();
  let acquire = searcher_manager.acquire()?;
  assert!(Arc::ptr_eq(&first_reader, acquire.get_index_reader()));
  searcher_manager.release(acquire)?;

  let last_reader = first_reader;
  // refresh
  writer.add_document(Document::new())?;
  assert!(searcher_manager.maybe_refresh()?);

  let acquire = searcher_manager.acquire()?;
  let current_reader = factory_state
    .last_reader
    .lock()
    .as_ref()
    .expect("the refreshed reader must be passed to SearcherFactory")
    .clone();
  assert!(Arc::ptr_eq(&current_reader, acquire.get_index_reader()));
  searcher_manager.release(acquire)?;
  let previous_reader = factory_state
    .last_previous_reader
    .lock()
    .as_ref()
    .expect("the previous reader must be passed on refresh")
    .clone();
  assert!(Arc::ptr_eq(&last_reader, &previous_reader));
  assert!(!Arc::ptr_eq(&current_reader, &last_reader));
  assert_eq!(2, factory_state.called.load(Ordering::Relaxed));
  writer.close()?;
  searcher_manager.close()?;
  directory.close()?;
  Ok(())
}

#[test]
fn test_concurrent_index_close_search_and_refresh() -> Result<()> {
  let mut random = random();
  let directory = new_fs_directory(&mut random, TempDir::new()?)?;
  let mut analyzer = MockAnalyzer::new(&mut random);
  analyzer.set_max_token_length(MAX_TERM_LENGTH);
  let analyzer = Arc::new(analyzer);
  let analyzer_for_config: Box<dyn Analyzer> = Box::new(analyzer.clone());
  let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer_for_config)?;
  ensure_sane_iwc_on_nightly(&mut config)?;
  let writer = IndexWriter::new(directory.clone(), config)?;
  let writer_ref = Arc::new(RwLock::new(writer));

  let manager = SearcherManager::from_writer(&writer_ref.read(), None)?;
  let manager_ref = Arc::new(RwLock::new(Arc::new(manager)));
  let stop = Arc::new(AtomicBool::new(false));
  let index_seed = random.random::<u64>();

  let index_directory = directory.clone();
  let index_writer_ref = writer_ref.clone();
  let index_stop = stop.clone();
  let index_analyzer = analyzer.clone();
  let index_thread = thread::spawn(move || -> Result<()> {
    let mut random = StdRng::seed_from_u64(index_seed);
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<()> {
      let num_docs = if is_night_mode() {
        at_least(&mut random, 20000)
      } else {
        at_least(&mut random, 200)
      };
      let mut field_to_type = HashMap::<String, FieldType>::new();
      for _ in 0..num_docs {
        let writer = index_writer_ref.read().clone();
        let mut document = Document::new();
        let value = TestUtil::random_analysis_string(&mut random, 256, false);
        document.add(new_text_field(
          &mut random,
          "field",
          value,
          Store::Yes,
          &mut field_to_type,
        )?);
        writer.add_document(document)?;
        if random.random_range(0..1000) == 17 {
          if random.random_bool(0.5) {
            writer.close()?;
          } else {
            writer.rollback()?;
          }
          let analyzer_for_config: Box<dyn Analyzer> = Box::new(index_analyzer.clone());
          let mut config = new_index_writer_config_with_analyzer(&mut random, analyzer_for_config)?;
          ensure_sane_iwc_on_nightly(&mut config)?;
          *index_writer_ref.write() = IndexWriter::new(index_directory.clone(), config)?;
        }
      }
      Ok(())
    }));
    index_stop.store(true, Ordering::SeqCst);
    match result {
      Ok(result) => result,
      Err(payload) => resume_unwind(payload),
    }
  });

  let search_manager_ref = manager_ref.clone();
  let search_stop = stop.clone();
  let search_thread = thread::spawn(move || -> Result<()> {
    let mut total_count = 0i64;
    while !search_stop.load(Ordering::SeqCst) {
      let manager = search_manager_ref.read().clone();
      let searcher = match manager.acquire() {
        Ok(searcher) => searcher,
        // ok
        Err(LuceneError::AlreadyClosed(_)) => continue,
        Err(error) => return Err(error),
      };
      total_count += searcher.get_index_reader().max_doc()? as i64;
      manager.release(searcher)?;
    }
    let _ = total_count;
    Ok(())
  });

  let refresh_manager_ref = manager_ref.clone();
  let refresh_stop = stop.clone();
  let refresh_thread = thread::spawn(move || -> Result<()> {
    let mut refresh_count = 0;
    let mut already_closed_count = 0;
    while !refresh_stop.load(Ordering::SeqCst) {
      let manager = refresh_manager_ref.read().clone();
      refresh_count += 1;
      match manager.maybe_refresh_blocking() {
        Ok(()) => {},
        Err(LuceneError::AlreadyClosed(_)) => {
          // ok
          already_closed_count += 1;
          continue;
        },
        Err(error) => return Err(error),
      }
    }
    let _ = (refresh_count, already_closed_count);
    Ok(())
  });

  let close_manager_ref = manager_ref.clone();
  let close_writer_ref = writer_ref.clone();
  let close_stop = stop.clone();
  let close_thread = thread::spawn(move || -> Result<()> {
    let mut close_count = 0;
    let mut already_closed_count = 0;
    while !close_stop.load(Ordering::SeqCst) {
      let manager = close_manager_ref.read().clone();
      manager.close()?;
      close_count += 1;
      while !close_stop.load(Ordering::SeqCst) {
        let writer = close_writer_ref.read().clone();
        match SearcherManager::from_writer(&writer, None) {
          Ok(manager) => {
            *close_manager_ref.write() = Arc::new(manager);
            break;
          },
          Err(LuceneError::AlreadyClosed(_)) => {
            // ok
            already_closed_count += 1;
          },
          Err(error) => return Err(error),
        }
      }
    }
    let _ = (close_count, already_closed_count);
    Ok(())
  });

  for handle in [index_thread, search_thread, refresh_thread, close_thread] {
    match handle.join() {
      Ok(result) => result?,
      Err(payload) => resume_unwind(payload),
    }
  }

  manager_ref.read().close()?;
  writer_ref.read().close()?;
  directory.close()?;
  Ok(())
}
