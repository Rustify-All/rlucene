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
use crate::core::index::index_reader::{
  IndexReader, IndexReaderContextKind, IndexReaderContextType,
};
use crate::core::index::term::Term;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::searcher_factory::{SearcherFactoryBase, SearcherFactoryDefaults};
use crate::core::search::term_query::TermQuery;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::index::test_concurrent_merge_scheduler::CountDownLatch;
use crate::test_framework::core::util::lucene_test_case::{
  new_searcher_with_wrap, random_from_seed,
};
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

#[allow(dead_code)] // for quick search
struct TestSearcherManager;

pub struct WarmingSearcherFactory {
  warm_called: Arc<AtomicBool>,
  search_threads: Option<usize>,
}

impl WarmingSearcherFactory {
  pub fn new(warm_called: Arc<AtomicBool>, search_threads: Option<usize>) -> Self {
    Self {
      warm_called,
      search_threads,
    }
  }
}

impl<IR> SearcherFactoryBase<IR> for WarmingSearcherFactory
where
  IR: IndexReader + 'static,
  IR::ContextKind: IndexReaderContextKind<Arc<IR>>,
  IndexReaderContextType<Arc<IR>>: Sync + 'static,
{
  fn new_searcher(
    &self,
    reader: Arc<IR>,
    _previous_reader: Option<&Arc<IR>>,
  ) -> Result<IndexSearcher<IndexReaderContextType<Arc<IR>>>> {
    let context = reader.get_context()?;
    let searcher = if let Some(search_threads) = self.search_threads {
      IndexSearcher::with_threads(context, search_threads)?
    } else {
      IndexSearcher::new(context)?
    };
    self.warm_called.store(true, Ordering::Relaxed);
    searcher.search(TermQuery::new(Term::from_text("body", "united")), 10)?;
    Ok(searcher)
  }
}

pub struct BlockingSearcherFactory {
  tried_reopen: Arc<AtomicBool>,
  await_enter_warm: CountDownLatch,
  await_close: CountDownLatch,
  search_threads: Option<usize>,
}

impl BlockingSearcherFactory {
  pub fn new(
    tried_reopen: Arc<AtomicBool>,
    await_enter_warm: CountDownLatch,
    await_close: CountDownLatch,
    search_threads: Option<usize>,
  ) -> Self {
    Self {
      tried_reopen,
      await_enter_warm,
      await_close,
      search_threads,
    }
  }
}

impl<IR> SearcherFactoryBase<IR> for BlockingSearcherFactory
where
  IR: IndexReader + 'static,
  IR::ContextKind: IndexReaderContextKind<Arc<IR>>,
  IndexReaderContextType<Arc<IR>>: Sync + 'static,
{
  fn new_searcher(
    &self,
    reader: Arc<IR>,
    _previous_reader: Option<&Arc<IR>>,
  ) -> Result<IndexSearcher<IndexReaderContextType<Arc<IR>>>> {
    if self.tried_reopen.load(Ordering::SeqCst) {
      self.await_enter_warm.count_down();
      self.await_close.wait();
    }
    let context = reader.get_context()?;
    if let Some(search_threads) = self.search_threads {
      IndexSearcher::with_threads(context, search_threads)
    } else {
      IndexSearcher::new(context)
    }
  }
}

pub struct EvilSearcherFactory<IR> {
  other: IR,
  seed: u64,
}

impl<IR> EvilSearcherFactory<IR>
where
  IR: Clone,
{
  pub fn new(other: IR, seed: u64) -> Self {
    Self { other, seed }
  }
}

impl<IR> SearcherFactoryBase<IR> for EvilSearcherFactory<Arc<IR>>
where
  IR: IndexReader + 'static,
  IR::ContextKind: IndexReaderContextKind<Arc<IR>>,
  IndexReaderContextType<Arc<IR>>: Sync + 'static,
{
  fn new_searcher(
    &self,
    _reader: Arc<IR>,
    _previous_reader: Option<&Arc<IR>>,
  ) -> Result<IndexSearcher<IndexReaderContextType<Arc<IR>>>> {
    let mut random = random_from_seed(self.seed);
    new_searcher_with_wrap(&mut random, self.other.clone(), true)
  }
}

pub struct TrackingSearcherFactoryState<IR> {
  pub called: AtomicI32,
  pub last_reader: Mutex<Option<Arc<IR>>>,
  pub last_previous_reader: Mutex<Option<Arc<IR>>>,
}

impl<IR> Default for TrackingSearcherFactoryState<IR> {
  fn default() -> Self {
    Self {
      called: AtomicI32::new(0),
      last_reader: Mutex::new(None),
      last_previous_reader: Mutex::new(None),
    }
  }
}

pub struct TrackingSearcherFactory<IR> {
  state: Arc<TrackingSearcherFactoryState<IR>>,
}

impl<IR> TrackingSearcherFactory<IR> {
  pub fn new(state: Arc<TrackingSearcherFactoryState<IR>>) -> Self {
    Self { state }
  }
}

impl<IR> SearcherFactoryBase<IR> for TrackingSearcherFactory<IR>
where
  IR: IndexReader + 'static,
  IR::ContextKind: IndexReaderContextKind<Arc<IR>>,
  IndexReaderContextType<Arc<IR>>: Sync + 'static,
{
  fn new_searcher(
    &self,
    reader: Arc<IR>,
    previous_reader: Option<&Arc<IR>>,
  ) -> Result<IndexSearcher<IndexReaderContextType<Arc<IR>>>> {
    self.state.called.fetch_add(1, Ordering::Relaxed);
    *self.state.last_reader.lock() = Some(reader.clone());
    *self.state.last_previous_reader.lock() = previous_reader.cloned();
    SearcherFactoryDefaults::new_searcher(reader, previous_reader)
  }
}
