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
use crate::core::index::index_reader::{IndexReader, IndexReaderContextType};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::query_timeout::QueryTimeoutEnum;
use crate::core::index::reader_util::ReaderUtil;
use crate::core::index::term::Term;
use crate::core::index::terms::{Terms, get_terms};
use crate::core::search::QueryCache;
use crate::core::search::bulk_scorer::BulkScorer;
use crate::core::search::collection_statistics::CollectionStatistics;
use crate::core::search::collector::Collector;
use crate::core::search::collector_manager::CollectorManager;
use crate::core::search::constant_score_query::ConstantScoreQuery;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::explanation::Explanation;
use crate::core::search::field_doc::FieldDoc;
use crate::core::search::leaf_collector::LeafCollector;
use crate::core::search::lru_query_cache::LRUQueryCache;
use crate::core::search::query::{IntoQuery, Query, QueryBase, QueryRef, QueryWeight};
use crate::core::search::query_cache::QueryCacheEnum;
use crate::core::search::query_caching_policy::{QueryCachingPolicyArc, QueryCachingPolicyEnum};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_doc::ScoreDoc;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::similarities_impl::bm25_similarity::BM25Similarity;
use crate::core::search::similarities_impl::similarities::Similarity;

use crate::core::search::sort::Sort;
use crate::core::search::task_executor::TaskExecutor;
use crate::core::search::term_statistics::TermStatistics;
use crate::core::search::time_limiting_bulk_scorer::TimeLimitingBulkScorer;
use crate::core::search::top_docs::{TopDocs, TopDocsLike};
use crate::core::search::top_field_collector::populate_scores;
use crate::core::search::top_field_collector_manager::TopFieldCollectorManager;
use crate::core::search::top_field_docs::TopFieldDocs;
use crate::core::search::top_score_doc_collector_manager::TopScoreDocCollectorManager;
use crate::core::search::total_hit_count_collector_manager::TotalHitCountCollectorManager;
use crate::core::search::usage_tracking_query_caching_policy::UsageTrackingQueryCachingPolicy;
use crate::core::search::weight::Weight;
use crate::core::util::automation::byte_run_automaton::ByteRunAutomaton;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
#[cfg(test)]
use crate::test_framework::core::index::test_segment_to_thread_mapping::IntraSliceDocIdOrderWithPartitionsIndexSearcher;
#[cfg(test)]
use crate::test_framework::core::search::asserting_index_searcher::AssertingIndexSearcher;
#[cfg(test)]
use crate::test_framework::core::search::scorer_index_searcher::ScorerIndexSearcherHook;
#[cfg(test)]
use crate::test_framework::core::search::shard_searching_test_base::ShardIndexSearcherHook;
#[cfg(test)]
use crate::test_framework::core::search::test_boolean_query::CountingIndexSearcher;
#[cfg(test)]
use crate::test_framework::core::search::test_boolean_rewrites::NoRewriteIndexSearcher;
#[cfg(test)]
use crate::test_framework::core::search::test_custom_searcher_sort::CustomSearcher;
#[cfg(test)]
use crate::test_framework::core::search::test_index_searcher::{
  GetSlicesIndexSearcher, SegmentPartitionsSameSliceIndexSearcher,
  SlicesOffloadedToExecutorIndexSearcher,
};
use parking_lot::Mutex;
#[cfg(not(test))]
use parking_lot::RwLock;
use rayon::ThreadPool;
use smallvec::SmallVec;
use std::borrow::Borrow;
#[cfg(test)]
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, OnceLock};
use sysinfo::System;

const DEFAULT_MAX_CLAUSE_COUNT: usize = 1024;
#[cfg(not(test))]
static MAX_CLAUSE_COUNT: AtomicUsize = AtomicUsize::new(DEFAULT_MAX_CLAUSE_COUNT);
#[cfg(test)]
thread_local! {
  static MAX_CLAUSE_COUNT: Cell<usize> = const { Cell::new(DEFAULT_MAX_CLAUSE_COUNT) };
}
const TOTAL_HITS_THRESHOLD: usize = 1000;
/// Thresholds for index slice allocation logic.
/// To change the default, extend IndexSearcher and use custom values
const MAX_DOCS_PER_SLICE: i32 = 250000;
const MAX_SEGMENTS_PER_SLICE: usize = 5;
pub static MAX_CACHED_QUERIES: i32 = 1000;
pub static MAX_RAM_BYTES_USED: LazyLock<i64> = LazyLock::new(|| {
  let mut sys = System::new();
  sys.refresh_memory();
  let total_mem_bytes = sys.total_memory() * 1024;
  let five_percent = total_mem_bytes / 20;
  debug_assert!(five_percent <= i64::MAX as u64);
  std::cmp::min(32 * (1 << 20), five_percent as i64)
});
pub type DefaultQueryCache = LRUQueryCache;

fn new_default_query_cache() -> Option<Arc<DefaultQueryCache>> {
  Some(Arc::new(expect_invariant!(
    LRUQueryCache::new(MAX_CACHED_QUERIES, *MAX_RAM_BYTES_USED),
    "the built-in query cache limits are valid"
  )))
}

fn new_default_caching_policy() -> Arc<QueryCachingPolicyEnum> {
  Arc::new(
    expect_invariant!(
      UsageTrackingQueryCachingPolicy::new(),
      "the built-in query caching policy configuration is valid"
    )
    .into(),
  )
}

#[cfg(not(test))]
static DEFAULT_QUERY_CACHE: LazyLock<RwLock<Option<Arc<DefaultQueryCache>>>> =
  LazyLock::new(|| RwLock::new(new_default_query_cache()));
#[cfg(not(test))]
static DEFAULT_CACHING_POLICY: LazyLock<RwLock<Arc<QueryCachingPolicyEnum>>> =
  LazyLock::new(|| RwLock::new(new_default_caching_policy()));

#[cfg(test)]
thread_local! {
  static DEFAULT_QUERY_CACHE: RefCell<Option<Arc<DefaultQueryCache>>> =
    RefCell::new(new_default_query_cache());
  static DEFAULT_CACHING_POLICY: RefCell<Arc<QueryCachingPolicyEnum>> =
    RefCell::new(new_default_caching_policy());
}

fn default_query_cache() -> Option<Arc<DefaultQueryCache>> {
  #[cfg(not(test))]
  {
    DEFAULT_QUERY_CACHE.read().clone()
  }
  #[cfg(test)]
  {
    DEFAULT_QUERY_CACHE.with(|query_cache| query_cache.borrow().clone())
  }
}

fn default_caching_policy() -> Arc<QueryCachingPolicyEnum> {
  #[cfg(not(test))]
  {
    DEFAULT_CACHING_POLICY.read().clone()
  }
  #[cfg(test)]
  {
    DEFAULT_CACHING_POLICY.with(|policy| policy.borrow().clone())
  }
}

/// Expert: sets the default query cache instance.
pub fn set_default_query_cache(query_cache: Option<Arc<DefaultQueryCache>>) {
  #[cfg(not(test))]
  {
    *DEFAULT_QUERY_CACHE.write() = query_cache;
  }
  #[cfg(test)]
  {
    DEFAULT_QUERY_CACHE.with(|default_query_cache| {
      *default_query_cache.borrow_mut() = query_cache;
    });
  }
}

/// Expert: sets the default query caching policy instance.
pub fn set_default_query_caching_policy(query_caching_policy: Arc<QueryCachingPolicyEnum>) {
  #[cfg(not(test))]
  {
    *DEFAULT_CACHING_POLICY.write() = query_caching_policy;
  }
  #[cfg(test)]
  {
    DEFAULT_CACHING_POLICY.with(|default_policy| {
      *default_policy.borrow_mut() = query_caching_policy;
    });
  }
}

pub struct IndexSearcher<
  IRC: 'static,
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> where
  SIM: Similarity,
{
  hook: IndexSearcherHook,
  pub reader_context: IRC,
  similarity: Arc<SIM>,
  leaf_slices: OnceLock<Arc<Vec<LeafSlice>>>,
  // Serializes fallible initialization; cached reads use leaf_slices directly.
  slice_init_lock: Mutex<()>,
  query_timeout: Option<Arc<QueryTimeoutEnum>>,
  query_caching_policy: Arc<QueryCachingPolicyEnum>,
  query_cache: Option<QueryCacheEnum<IRC, SIM>>,
  // Used internally for load balancing threads executing for the query.
  task_executor: TaskExecutor,
  // partialResult may be set on one of the threads of the executor. It may be correct to not make
  // Joining these threads establishes the required happens-before relationship, but using an atomic
  // value also makes cross-thread visibility explicit.
  // shouldn't hurt either.
  partial_result: AtomicBool,
}
pub type DefaultIndexSearcher<
  IRC,
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> = IndexSearcher<IRC, SIM>;

impl<IRC> DefaultIndexSearcher<IRC>
where
  IRC: IndexReaderContext + 'static,
{
  pub fn new(context: IRC) -> Result<Self> {
    Self::new_with_executor(context, None, Arc::clone(get_default_similarity()))
  }

  /// Runs searches for each segment separately, using the provided executor.
  pub fn with_executor(context: IRC, executor: Arc<ThreadPool>) -> Result<Self> {
    Self::new_with_executor(
      context,
      Some(executor),
      Arc::clone(get_default_similarity()),
    )
  }
}

impl<IRC, SIM> IndexSearcher<IRC, SIM>
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
{
  /// Creates a searcher with a concrete similarity type and an optional executor.
  ///
  /// The similarity and its scorer type are preserved throughout weight construction
  /// and per-document scoring. Pass `None` to execute searches on the calling thread.
  pub fn new_with_executor(
    context: IRC,
    executor: Option<Arc<ThreadPool>>,
    similarity: Arc<SIM>,
  ) -> Result<Self> {
    debug_assert!(
      context.base().is_top_level,
      "IndexSearcher's ReaderContext must be topLevel for reader {}",
      context.reader()
    );

    let leaf_slices = if executor.is_none() {
      OnceLock::from(single_threaded_slices(&context)?)
    } else {
      OnceLock::new()
    };
    let task_executor = match executor {
      Some(executor) => TaskExecutor::new(executor),
      None => TaskExecutor::direct(),
    };
    Ok(Self {
      hook: IndexSearcherHook::Default,
      reader_context: context,
      similarity,
      leaf_slices,
      slice_init_lock: Mutex::new(()),
      query_timeout: None,
      query_caching_policy: default_caching_policy(),
      query_cache: default_query_cache().map(Into::into),
      task_executor,
      partial_result: AtomicBool::new(false),
    })
  }
}
pub fn from_reader<IR>(reader: IR) -> Result<DefaultIndexSearcher<IndexReaderContextType<IR>>>
where
  IR: IndexReader,
{
  IndexSearcher::new(reader.get_context()?)
}

pub fn from_reader_with_executor<IR>(
  reader: IR,
  executor: Arc<ThreadPool>,
) -> Result<DefaultIndexSearcher<IndexReaderContextType<IR>>>
where
  IR: IndexReader,
{
  IndexSearcher::with_executor(reader.get_context()?, executor)
}

static DEFAULT_SIMILARITY: LazyLock<Arc<BM25Similarity>> =
  LazyLock::new(|| Arc::new(BM25Similarity::default()));

/// Shared default similarity used by searchers and default document-writing APIs.
/// Clone the Arc when ownership is needed, or borrow it for a single operation.
pub fn get_default_similarity() -> &'static Arc<BM25Similarity> {
  &DEFAULT_SIMILARITY
}

impl<IRC, SIM> IndexSearcher<IRC, SIM>
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
{
  #[cfg(test)]
  pub(crate) fn with_hook(mut self, hook: IndexSearcherHook) -> Self {
    self.hook = hook;
    self
  }

  pub fn stored_fields(&self) -> Result<<IRC::IndexReader as IndexReader>::StoredFields> {
    self.reader_context.reader().stored_fields()
  }

  /// Replaces the similarity with another value of the same concrete type.
  /// Construct a new searcher to select a different similarity type.
  pub fn set_similarity<T>(&mut self, similarity: T)
  where
    T: Into<Arc<SIM>>,
  {
    self.similarity = similarity.into();
  }

  /// Returns the [`TaskExecutor`] that this searcher relies on to execute concurrent operations.
  pub fn get_task_executor(&self) -> &TaskExecutor {
    &self.task_executor
  }

  #[cfg(test)]
  pub fn set_offloaded_slice_counter(&mut self, counter: Arc<AtomicUsize>) {
    self.task_executor.set_offloaded_task_counter(counter);
  }

  pub fn get_slices(&self) -> Result<Arc<Vec<LeafSlice>>> {
    if let Some(slices) = self.leaf_slices.get() {
      Ok(Arc::clone(slices))
    } else {
      self.compute_and_cache_slices()
    }
  }

  fn compute_and_cache_slices(&self) -> Result<Arc<Vec<LeafSlice>>> {
    let _guard = self.slice_init_lock.lock();
    if self.leaf_slices.get().is_none() {
      let leaves = self.reader_context.leaves()?;
      let res = self.hook.slices(self, leaves)?;
      // Enforce that there aren't multiple leaf partitions within the same leaf slice pointing to the
      // same leaf context. It is a requirement that [`Collector::get_leaf_collector(LeafReaderContext)`]
      // gets called once per leaf context.
      //
      // Also, it does not make sense to partition a segment to then search those partitions as part of
      // the same slice, because the goal of partitioning is parallel searching which happens at the
      // slice level.
      for leaf_slice in &res {
        if leaf_slice.partitions.len() <= 1 {
          continue;
        }
        enforce_distinct_leaves(leaf_slice)?;
      }

      self.leaf_slices.get_or_init(|| Arc::new(res));
    }
    self
      .leaf_slices
      .get()
      .cloned()
      .ok_or_else(|| LuceneError::illegal_state("leaf slices are missing"))
  }

  pub fn search_after_score<Q>(
    &self,
    after: Option<ScoreDoc>,
    query: Q,
    num_hits: usize,
  ) -> Result<TopDocs<ScoreDoc>>
  where
    Self: Sync,
    Q: IntoQuery,
  {
    self
      .hook
      .search_after_score(self, after, query.into_query(), num_hits)
  }
  /// Get the configured [`QueryTimeout`](crate::core::index::query_timeout::QueryTimeout) for all searches that run through this [`IndexSearcher`],
  /// or `None` if not set.
  pub fn get_timeout(&self) -> Option<Arc<QueryTimeoutEnum>> {
    self.query_timeout.clone()
  }
  /// Set a [`QueryTimeout`](crate::core::index::query_timeout::QueryTimeout) for all searches that run through this [`IndexSearcher`].
  pub fn set_timeout<T>(&mut self, query_timeout: T)
  where
    T: Into<QueryTimeoutEnum>,
  {
    self.query_timeout = Some(Arc::new(query_timeout.into()))
  }
  pub fn search<Q>(&self, query: Q, n: usize) -> Result<TopDocs<ScoreDoc>>
  where
    Self: Sync,
    Q: IntoQuery,
  {
    self.hook.search(self, query.into_query(), n)
  }
  /// Search implementation with arbitrary sorting, plus control over whether hit scores and max
  /// score should be computed.
  /// Finds the top `n` hits for `query`, sorting the hits by the criteria in `sort`.
  /// If `do_doc_scores` is `true`, the score of each hit will be computed and returned.
  /// If `do_max_score` is `true`, the maximum score over all collected hits will be computed.
  ///
  /// # Errors
  /// Returns a [`LuceneErrorKind::TooManyClauses`] if a query would exceed
  /// [`get_max_clause_count()`] clauses.
  pub fn search_with_sort_score<T, Q>(
    &self,
    query: Q,
    n: usize,
    sort: T,
    do_doc_scores: bool,
  ) -> Result<TopFieldDocs>
  where
    T: Into<Arc<Sort>>,
    Self: Sync,
    Q: IntoQuery,
  {
    self.search_after_field_with_score(None, query, n, sort, do_doc_scores)
  }
  /// Search implementation with arbitrary sorting.
  ///
  /// * `query` — The query to search for
  /// * `n` — Return only the top `n` results
  /// * `sort` — The [`Sort`] object
  ///
  /// # Returns
  /// The top docs, sorted according to the supplied [`Sort`] instance.
  ///
  /// # Errors
  /// Returns an error if a low-level I/O error occurs.
  pub fn search_with_sort<T, Q>(&self, query: Q, n: usize, sort: T) -> Result<TopFieldDocs>
  where
    T: Into<Arc<Sort>>,
    Self: Sync,
    Q: IntoQuery,
  {
    self
      .hook
      .search_with_sort(self, query.into_query(), n, sort.into())
  }

  pub fn get_top_reader_context(&self) -> &IRC {
    &self.reader_context
  }
  pub fn get_similarity(&self) -> Arc<SIM> {
    self.similarity.clone()
  }

  pub(crate) fn similarity(&self) -> &SIM {
    &self.similarity
  }

  /// Count how many documents match the given query.
  /// May be faster than counting number of hits by collecting all matches,
  /// as the number of hits is retrieved from the index statistics when possible.
  pub fn count<Q>(&self, query: Q) -> Result<i32>
  where
    Self: Sync,
    Q: IntoQuery,
  {
    self.hook.count(self, query.into_query())
  }

  pub fn search_after_field_with_score<Q, T>(
    &self,
    after: Option<FieldDoc>,
    query: Q,
    num_hits: usize,
    sort: T,
    do_doc_scores: bool,
  ) -> Result<TopFieldDocs>
  where
    Q: IntoQuery,
    T: Into<Arc<Sort>>,
    Self: Sync,
  {
    self.do_search_after_field(after, query, num_hits, sort, do_doc_scores)
  }
  pub fn search_after<Q, T>(
    &self,
    after: Option<FieldDoc>,
    query: Q,
    num_hits: usize,
    sort: T,
  ) -> Result<TopFieldDocs>
  where
    Q: IntoQuery,
    T: Into<Arc<Sort>>,
    Self: Sync,
  {
    self.do_search_after_field(after, query, num_hits, sort, false)
  }

  fn do_search_after_field<Q, T>(
    &self,
    after: Option<FieldDoc>,
    query: Q,
    num_hits: usize,
    sort: T,
    do_doc_scores: bool,
  ) -> Result<TopFieldDocs>
  where
    Q: IntoQuery,
    T: Into<Arc<Sort>>,
    Self: Sync,
  {
    let limit = std::cmp::max(1, self.reader_context.reader().max_doc()?);

    if let Some(ref a) = after
      && a.base.doc >= limit
    {
      return Err(LuceneError::illegal_argument(format!(
        "after.doc exceeds the number of documents in the reader: after.doc={} limit={}",
        a.base.doc, limit
      )));
    }

    let capped_num_hits = std::cmp::min(num_hits, limit as usize);
    let sort = sort.into();
    let rewritten_sort = match sort.rewrite(self)? {
      Some(rewritten_sort) => Arc::new(rewritten_sort),
      None => sort,
    };
    let (mut top_field_docs, score_query) = if let Some(after) = after {
      let manager = TopFieldCollectorManager::with_after(
        rewritten_sort,
        capped_num_hits,
        after,
        TOTAL_HITS_THRESHOLD,
      )?;
      let query = query.into_query();
      let score_query = do_doc_scores.then(|| query.clone());
      (
        self.search_with_collector_manager(query, &manager)?,
        score_query,
      )
    } else {
      let manager =
        TopFieldCollectorManager::new(rewritten_sort, capped_num_hits, TOTAL_HITS_THRESHOLD)?;
      let query = query.into_query();
      let score_query = do_doc_scores.then(|| query.clone());
      (
        self.search_with_collector_manager(query, &manager)?,
        score_query,
      )
    };

    if let Some(query) = score_query {
      populate_scores(top_field_docs.score_docs_mut(), self, query)?;
    }

    Ok(top_field_docs)
  }

  pub fn search_with_collector_manager<CM, Q>(
    &self,
    query: Q,
    collector_manager: &CM,
  ) -> Result<CM::T>
  where
    Self: Sync,
    CM: CollectorManager,
    CM::C: Send,
    Q: IntoQuery,
  {
    self
      .hook
      .search_with_collector_manager(self, query.into_query(), collector_manager)
  }
  pub fn search_with_collector<C, Q>(&self, query: Q, collector: &mut C) -> Result<()>
  where
    C: Collector,
    Self: Sync,
    Q: IntoQuery,
  {
    self
      .hook
      .search_with_collector(self, query.into_query(), collector)
  }
  /// Returns true if any search hit the timeout.
  pub fn timeout(&self) -> bool {
    self.partial_result.load(Ordering::SeqCst)
  }
  fn search_with_first_collector<CM>(
    &self,
    query: Query,
    score_mode: ScoreMode,
    collector_manager: &CM,
    first_collector: CM::C,
  ) -> Result<CM::T>
  where
    Self: Sync,
    CM: CollectorManager,
    CM::C: Send,
  {
    let weight = self.create_weight(query, score_mode, 1.0)?;
    let initialized_slices;
    let leaf_slices = match self.leaf_slices.get() {
      Some(slices) => slices,
      None => {
        initialized_slices = self.get_slices()?;
        &initialized_slices
      },
    };
    if leaf_slices.is_empty() {
      debug_assert!(self.reader_context.leaves()?.is_empty());
      return collector_manager.reduce(vec![first_collector]);
    }

    let mut collectors = Vec::with_capacity(leaf_slices.len());
    collectors.push(first_collector);
    for _ in 1..leaf_slices.len() {
      let collector = collector_manager.new_collector()?;
      if score_mode != collector.score_mode() {
        return Err(LuceneError::illegal_state(
          "CollectorManager does not always produce collectors with the same score mode",
        ));
      }
      collectors.push(collector);
    }

    let weight = weight.as_ref();
    let tasks = collectors
      .into_iter()
      .zip(leaf_slices.iter())
      .map(|(mut collector, slice)| {
        move || {
          self.search_partitions(slice.partitions.as_slice(), weight, &mut collector)?;
          Ok(collector)
        }
      });
    let results = self.task_executor.invoke_all(tasks)?;
    collector_manager.reduce(results)
  }

  pub(crate) fn search_partitions<W, C>(
    &self,
    partitions: &[LeafReaderContextPartition],
    weight: &W,
    collector: &mut C,
  ) -> Result<()>
  where
    C: Collector,
    W: Weight<IRC, SIM> + ?Sized,
    Self: Sync,
  {
    self
      .hook
      .search_partitions(self, partitions, weight, collector)
  }
  pub(crate) fn search_leaf<W, C>(
    &self,
    ctx_ord: usize,
    min_doc_id: i32,
    max_doc_id: i32,
    weight: &W,
    collector: &mut C,
  ) -> Result<()>
  where
    C: Collector,
    W: Weight<IRC, SIM> + ?Sized,
    Self: Sync,
  {
    self
      .hook
      .search_leaf(self, ctx_ord, min_doc_id, max_doc_id, weight, collector)
  }
  pub fn rewrite<Q>(&self, query: Q) -> Result<Query>
  where
    Q: IntoQuery,
    Self: Sync,
  {
    self.hook.rewrite(self, query.into_query())
  }

  pub(crate) fn rewrite_with_needs_scores(
    &self,
    original: Query,
    needs_scores: bool,
  ) -> Result<Query>
  where
    Self: Sync,
  {
    if needs_scores {
      self.rewrite(original)
    } else {
      // Take advantage of the few extra rewrite rules of ConstantScoreQuery.
      let v = ConstantScoreQuery::new(original);
      self.rewrite(v)
    }
  }
  /// Returns an Explanation that describes how `doc` scored against `query`.
  ///
  /// This is intended to be used in developing Similarity implementations, and, for good
  /// performance, should not be displayed with every hit. Computing an explanation is as expensive
  /// as executing the query over the entire index.
  pub fn explain<T>(&self, query: T, doc: i32) -> Result<Explanation>
  where
    T: IntoQuery,
    Self: Sync,
  {
    let query = self.rewrite(query.into_query())?;
    let weight = self.create_weight(query, ScoreMode::Complete, 1.0)?;
    self.explain_from_weight(&weight, doc)
  }
  /// Expert: low-level implementation method Returns an Explanation that describes how `doc`
  /// scored against `weight`.
  ///
  /// This is intended to be used in developing Similarity implementations, and, for good
  /// performance, should not be displayed with every hit. Computing an explanation is as expensive
  /// as executing the query over the entire index.
  ///
  /// Applications should call [`IndexSearcher::explain`].
  ///
  /// # Errors
  ///
  /// Returns an error if a query would exceed [`index_searcher::get_max_clause_count`](crate::core::search::index_searcher::get_max_clause_count) clauses.
  pub fn explain_from_weight(&self, weight: &QueryWeight<IRC, SIM>, doc: i32) -> Result<Explanation>
  where
    Self: Sync,
  {
    let leaf_contexts = self.reader_context.leaves()?;
    let n = ReaderUtil::sub_index_with_leaves(doc, leaf_contexts);
    let ctx = &leaf_contexts[n];
    let de_based_doc = doc as usize - ctx.doc_base;

    let live_docs = ctx.reader().get_live_docs()?;
    if let Some(live_docs) = live_docs
      && !live_docs.get(de_based_doc)?
    {
      return Ok(Explanation::no_match_no_details(format!(
        "Document {} is deleted",
        doc
      )));
    }

    weight.explain(ctx, de_based_doc as i32, self)
  }

  #[allow(clippy::type_complexity)]
  pub fn create_weight<T>(
    &self,
    query: T,
    score_mode: ScoreMode,
    boost: f32,
  ) -> Result<QueryWeight<IRC, SIM>>
  where
    T: QueryBase,
    T::Weight<IRC, SIM>: Into<QueryWeight<IRC, SIM>>,
    Self: Sync,
  {
    self.hook.create_weight(self, query, score_mode, boost)
  }

  /// Returns [`TermStatistics`] for a term.
  ///
  /// This method can be overridden, for example, to return a term's statistics across
  /// a distributed collection.
  ///
  /// # Arguments
  ///
  /// * `doc_freq` — The document frequency of the term. It must be greater or equal to 1.
  /// * `total_term_freq` — The total term frequency.
  ///
  /// # Returns
  ///
  /// A [`TermStatistics`] (never `None`).
  ///
  /// **Lucene Experimental**
  pub fn term_statistics<T>(
    &self,
    term: T,
    doc_freq: i32,
    total_term_freq: i64,
  ) -> Result<TermStatistics>
  where
    T: Into<Arc<Term>>,
  {
    self
      .hook
      .term_statistics(self, term.into(), doc_freq, total_term_freq)
  }
  /// Returns [`CollectionStatistics`] for a field, or `None` if the field does not exist
  /// (has no indexed terms).
  ///
  ///
  /// This method can be overridden, for example, to return a field's statistics across
  /// a distributed collection.
  pub fn collection_statistics(&self, field: &str) -> Result<Option<CollectionStatistics>> {
    self.hook.collection_statistics(self, field)
  }
  pub fn get_leaf_contexts(&self) -> Result<&[LeafReaderContext<IRC::LeafReader>]> {
    self.reader_context.leaves()
  }
  pub fn get_index_reader(&self) -> &IRC::IndexReader {
    self.reader_context.reader()
  }

  pub fn set_query_cache(&mut self, query_cache: Option<QueryCacheEnum<IRC, SIM>>) {
    self.query_cache = query_cache;
  }
  pub fn get_query_cache(&self) -> Option<&QueryCacheEnum<IRC, SIM>> {
    self.query_cache.as_ref()
  }

  pub fn get_query_caching_policy(&self) -> Arc<QueryCachingPolicyEnum> {
    self.query_caching_policy.clone()
  }

  pub fn set_query_caching_policy<T>(&mut self, query_caching_policy: T)
  where
    T: QueryCachingPolicyArc,
  {
    self.query_caching_policy = query_caching_policy.into_query_cache_policy_arc();
  }
}

/// Returns the maximum number of clauses permitted, `1024` by default.
///
/// Adding more than the permitted number of clauses returns a [`TooManyClauses`] error.
///
/// Tests can change this value with [`set_max_clause_count`].
pub fn get_max_clause_count() -> usize {
  #[cfg(test)]
  {
    MAX_CLAUSE_COUNT.with(Cell::get)
  }
  #[cfg(not(test))]
  {
    MAX_CLAUSE_COUNT.load(Ordering::Relaxed)
  }
}
/// Set the maximum number of clauses permitted per Query. Default value is 1024.
pub fn set_max_clause_count(value: usize) -> Result<()> {
  if value < 1 {
    return Err(LuceneError::illegal_argument("maxClauseCount must be >= 1"));
  }
  #[cfg(test)]
  {
    MAX_CLAUSE_COUNT.with(|max_clause_count| max_clause_count.set(value));
  }
  #[cfg(not(test))]
  {
    MAX_CLAUSE_COUNT.store(value, Ordering::Relaxed);
  }
  Ok(())
}

fn single_threaded_slices<IRC>(context: &IRC) -> Result<Arc<Vec<LeafSlice>>>
where
  IRC: IndexReaderContext + 'static,
{
  let reader = context.reader();
  let leaf_contexts = context.leaves()?;

  if leaf_contexts.is_empty() {
    Ok(Arc::new(Vec::new()))
  } else {
    let partitions = leaf_contexts
      .iter()
      .map(LeafReaderContextPartition::create_for_entire_segment)
      .collect::<Result<Vec<_>>>()?;

    let slice = LeafSlice {
      partitions,
      max_docs: reader.max_doc()?,
    };
    Ok(Arc::new(vec![slice]))
  }
}

pub fn do_slices<LR>(
  leaves: &[LeafReaderContext<LR>],
  max_docs_per_slice: i32,
  max_segments_per_slice: usize,
  allow_segment_partitions: bool,
) -> Result<Vec<LeafSlice>>
where
  LR: LeafReader,
{
  let mut sorted_leaves: SmallVec<[_; 8]> = SmallVec::with_capacity(leaves.len());

  for ctx in leaves {
    let max_doc = ctx.reader().max_doc()?;
    sorted_leaves.push((ctx, max_doc));
  }
  sorted_leaves.sort_by_key(|leaf| std::cmp::Reverse(leaf.1));

  if allow_segment_partitions {
    let mut grouped_leaf_partitions: SmallVec<[Vec<LeafReaderContextPartition>; 4]> =
      SmallVec::new();
    let mut current_slice_num_docs = 0;
    let mut group: Option<Vec<LeafReaderContextPartition>> = None;

    for (ctx, _) in sorted_leaves {
      let ctx_max_doc = ctx.reader().max_doc()?;
      if ctx_max_doc > max_docs_per_slice {
        debug_assert!(group.is_none());
        // if the segment does not fit in a single slice, we split it into maximum 5 partitions of equal size
        let num_slices = std::cmp::min(
          5,
          (ctx_max_doc + max_docs_per_slice - 1) / max_docs_per_slice,
        );
        let num_docs = ctx_max_doc / num_slices;
        let mut max_doc_id = num_docs;
        let mut min_doc_id = 0;

        for _ in 0..(num_slices - 1) {
          grouped_leaf_partitions.push(vec![LeafReaderContextPartition::create_from_and_to(
            ctx, min_doc_id, max_doc_id,
          )?]);
          min_doc_id = max_doc_id;
          max_doc_id += num_docs;
        }
        // the last slice gets all the remaining docs
        grouped_leaf_partitions.push(vec![LeafReaderContextPartition::create_from_and_to(
          ctx,
          min_doc_id,
          ctx_max_doc,
        )?]);
      } else {
        let group_ref = group.get_or_insert_default();
        group_ref.push(LeafReaderContextPartition::create_for_entire_segment(ctx)?);
        current_slice_num_docs += ctx_max_doc;
        // We only split a segment when it does not fit entirely in a slice. We don't partition
        // the
        // segment that makes the current slice (which holds multiple segments) go over
        // maxDocsPerSlice. This means that a slice either contains multiple entire segments, or a
        // single partition of a segment.
        if group_ref.len() >= max_segments_per_slice || current_slice_num_docs > max_docs_per_slice
        {
          grouped_leaf_partitions.push(std::mem::take(group_ref));
          group = None;
          current_slice_num_docs = 0;
        }
      }
    }

    if let Some(g) = group.take() {
      grouped_leaf_partitions.push(g);
    }

    return Ok(
      grouped_leaf_partitions
        .into_iter()
        .map(LeafSlice::new)
        .collect(),
    );
  }

  let mut grouped_leaves: SmallVec<[Vec<&LeafReaderContext<LR>>; 4]> = SmallVec::new();
  let mut doc_sum: i64 = 0;
  let mut group: Option<Vec<&LeafReaderContext<LR>>> = None;

  for (ctx, _) in sorted_leaves {
    let ctx_max_doc = ctx.reader().max_doc()?;

    if ctx_max_doc > max_docs_per_slice {
      debug_assert!(group.is_none());
      grouped_leaves.push(vec![ctx]);
    } else {
      let group_ref = group.get_or_insert_default();
      group_ref.push(ctx);
      doc_sum += ctx_max_doc as i64;

      if group_ref.len() >= max_segments_per_slice || doc_sum > max_docs_per_slice as i64 {
        grouped_leaves.push(std::mem::take(group_ref));
        group = None;
        doc_sum = 0;
      }
    }
  }

  if let Some(g) = group.take() {
    grouped_leaves.push(g);
  }

  let mut slices = Vec::with_capacity(grouped_leaves.len());

  for contexts in grouped_leaves {
    let mut partitions = Vec::with_capacity(contexts.len());
    for ctx in contexts {
      let partition = LeafReaderContextPartition::create_for_entire_segment(ctx)?;
      partitions.push(partition);
    }
    slices.push(LeafSlice::new(partitions));
  }
  Ok(slices)
}
/// Expert: Creates an array of [`LeafSlice`] each holding a subset of the given leaves.
/// Each [`LeafSlice`] is executed in a single thread.
///
/// By default, segments with more than `MAX_DOCS_PER_SLICE` will get their own thread.
///
///
/// It is possible to leverage intra-segment concurrency by splitting segments into multiple
/// partitions. Such behaviour is not enabled by default as there is still a performance penalty
/// for queries that require segment-level computation ahead of time, such as points/range queries.
///
/// This is an implementation limitation that we expect to improve in future releases,
/// see [the corresponding GitHub issue](https://github.com/apache/lucene/issues/13745).
pub fn slices<LR>(leaves: &[LeafReaderContext<LR>]) -> Result<Vec<LeafSlice>>
where
  LR: LeafReader,
{
  do_slices(leaves, MAX_DOCS_PER_SLICE, MAX_SEGMENTS_PER_SLICE, false)
}

fn enforce_distinct_leaves(leaf_slice: &LeafSlice) -> Result<()> {
  let mut distinct_leaves = HashSet::new();

  for (index, partition) in leaf_slice.partitions.iter().enumerate() {
    let duplicate = if leaf_slice.partitions.len() <= 8 {
      leaf_slice.partitions[..index]
        .iter()
        .any(|previous| previous.ctx == partition.ctx)
    } else {
      !distinct_leaves.insert(partition.ctx)
    };
    if duplicate {
      return Err(LuceneError::illegal_state(
        "The same slice targets multiple leaf partitions of the same leaf reader context. \
                A physical segment should rather get partitioned to be searched concurrently from \
                as many slices as the number of leaf partitions it is split into.",
      ));
    }
  }

  Ok(())
}
/// Returned when an attempt is made to add more than [`get_max_clause_count()`] clauses.
///
/// This typically happens if a [`PrefixQuery`](crate::core::search::prefix_query::PrefixQuery), [`FuzzyQuery`](crate::core::search::fuzzy_query::FuzzyQuery), [`WildcardQuery`](crate::core::search::wildcard_query::WildcardQuery),
/// or [`TermRangeQuery`](crate::core::search::term_range_query::TermRangeQuery) is expanded to many terms during search.
pub struct TooManyClauses;
pub fn new() -> LuceneError {
  with_msg(format!(
    "maxClauseCount is set to {}",
    get_max_clause_count()
  ))
}
pub fn with_msg<FName>(msg: FName) -> LuceneError
where
  FName: Into<String>,
{
  let msg = msg.into();
  LuceneError::too_many_clauses(msg)
}
pub struct TooManyNestedClauses;
pub fn new_nested() -> LuceneError {
  LuceneError::too_many_nested_clauses(format!(
    "Query contains too many nested clauses; maxClauseCount is set to {}",
    get_max_clause_count()
  ))
}

/// Holds information about a specific leaf context and the corresponding range of doc ids to
/// search within. Used to optionally search across partitions of the same segment concurrently.
///
/// A partition instance can be created via [`LeafReaderContextPartition::create_for_entire_segment`],
/// in which case it will target the entire provided [`LeafReaderContext`].
/// A true partition of a segment can be created via
/// [`LeafReaderContextPartition::create_from_and_to`] providing the minimum doc id (inclusive) to
/// search as well as the max doc id (exclusive).
pub struct LeafReaderContextPartition {
  pub min_doc_id: i32,
  pub max_doc_id: i32,
  pub ctx: usize,
  pub doc_base: usize,
  pub ctx_max_doc: i32,
  // we keep track of maxDocs separately because we use NO_MORE_DOCS as upper bound when targeting
  // the entire segment. We use this only in tests.
  max_docs: i32,
}
impl LeafReaderContextPartition {
  pub fn new<LR>(
    leaf_reader_context: &LeafReaderContext<LR>,
    min_doc_id: i32,
    max_doc_id: i32,
    max_docs: i32,
  ) -> Result<Self>
  where
    LR: LeafReader,
  {
    let ctx_max_doc = leaf_reader_context.reader().max_doc()?;
    if min_doc_id >= max_doc_id {
      return Err(LuceneError::illegal_argument(format!(
        "minDocId is greater than or equal to maxDocId: [{}] >= [{}]",
        min_doc_id, max_doc_id
      )));
    }
    if min_doc_id < 0 {
      return Err(LuceneError::illegal_argument(format!(
        "minDocId is lower than 0: [{}]",
        min_doc_id
      )));
    }
    if min_doc_id >= ctx_max_doc {
      return Err(LuceneError::illegal_argument(format!(
        "minDocId is greater than maxDoc: [{}] >= [{}]",
        min_doc_id, ctx_max_doc
      )));
    }

    Ok(Self {
      min_doc_id,
      max_doc_id,
      ctx_max_doc,
      ctx: leaf_reader_context.ord,
      doc_base: leaf_reader_context.doc_base,
      max_docs,
    })
  }
  /// Creates a partition of the provided leaf context that targets the entire segment
  pub fn create_for_entire_segment<LR>(ctx: &LeafReaderContext<LR>) -> Result<Self>
  where
    LR: LeafReader,
  {
    Self::new(ctx, 0, NO_MORE_DOCS, ctx.reader().max_doc()?)
  }

  /// Creates a partition of the provided leaf context that targets a subset of the entire segment,
  /// starting from and including the min doc id provided, until and not including the provided max doc id
  pub fn create_from_and_to<LR>(
    ctx: &LeafReaderContext<LR>,
    min_doc_id: i32,
    max_doc_id: i32,
  ) -> Result<Self>
  where
    LR: LeafReader,
  {
    debug_assert!(max_doc_id != NO_MORE_DOCS);
    Self::new(ctx, min_doc_id, max_doc_id, max_doc_id - min_doc_id)
  }
}
/// A struct holding a subset of the [`IndexSearcher`]'s leaf contexts to be executed within a
/// single thread. A leaf slice holds references to one or more [`LeafReaderContextPartition`]
/// instances. Each partition targets a specific doc id range of a [`LeafReaderContext`].
pub struct LeafSlice {
  /// The leaves that make up this slice.
  pub partitions: Vec<LeafReaderContextPartition>,

  max_docs: i32,
}

impl LeafSlice {
  pub fn new(mut partitions: Vec<LeafReaderContextPartition>) -> Self {
    partitions.sort_by(|a, b| {
      let doc_base_cmp = a.doc_base.cmp(&b.doc_base);
      if doc_base_cmp == std::cmp::Ordering::Equal {
        a.min_doc_id.cmp(&b.min_doc_id)
      } else {
        doc_base_cmp
      }
    });
    let max_docs = partitions.iter().map(|p| p.max_docs).sum();

    Self {
      partitions,
      max_docs,
    }
  }
  /// Returns the total number of docs that a slice targets,
  /// by summing the number of docs that each of its leaf context partitions targets.
  pub fn max_docs(&self) -> i32 {
    self.max_docs
  }
}

#[derive(Default)]
pub(crate) enum IndexSearcherHook {
  #[default]
  Default,
  #[cfg(test)]
  GetSlices(GetSlicesIndexSearcher),
  #[cfg(test)]
  SlicesOffloadedToExecutor(SlicesOffloadedToExecutorIndexSearcher),
  #[cfg(test)]
  SegmentPartitionsSameSlice(SegmentPartitionsSameSliceIndexSearcher),
  #[cfg(test)]
  IntraSliceDocIdOrderWithPartitions(IntraSliceDocIdOrderWithPartitionsIndexSearcher),
  #[cfg(test)]
  NoRewrite(NoRewriteIndexSearcher),
  #[cfg(test)]
  Counting(CountingIndexSearcher),
  #[cfg(test)]
  #[allow(dead_code)]
  Scorer(ScorerIndexSearcherHook),
  #[cfg(test)]
  CustomSearcher(CustomSearcher),
  #[cfg(test)]
  Shard(Box<ShardIndexSearcherHook>),
  #[cfg(test)]
  Asserting(Box<AssertingIndexSearcher>),
}

pub(crate) struct IndexSearcherDefaults;

pub(crate) trait IndexSearcherBase<IRC>
where
  IRC: IndexReaderContext + 'static,
{
  fn slices<SIM>(
    &self,
    _searcher: &IndexSearcher<IRC, SIM>,
    leaves: &[LeafReaderContext<IRC::LeafReader>],
  ) -> Result<Vec<LeafSlice>>
  where
    SIM: Similarity,
  {
    IndexSearcherDefaults::slices(leaves)
  }

  fn count<SIM>(&self, searcher: &IndexSearcher<IRC, SIM>, query: Query) -> Result<i32>
  where
    SIM: Similarity,
    IndexSearcher<IRC, SIM>: Sync,
  {
    IndexSearcherDefaults::count(searcher, query)
  }

  fn search_after_score<SIM>(
    &self,
    searcher: &IndexSearcher<IRC, SIM>,
    after: Option<ScoreDoc>,
    query: Query,
    num_hits: usize,
  ) -> Result<TopDocs<ScoreDoc>>
  where
    SIM: Similarity,
    IndexSearcher<IRC, SIM>: Sync,
  {
    IndexSearcherDefaults::search_after_score(searcher, after, query, num_hits)
  }

  fn search<SIM>(
    &self,
    searcher: &IndexSearcher<IRC, SIM>,
    query: Query,
    n: usize,
  ) -> Result<TopDocs<ScoreDoc>>
  where
    SIM: Similarity,
    IndexSearcher<IRC, SIM>: Sync,
  {
    IndexSearcherDefaults::search(searcher, query, n)
  }

  fn search_with_collector<C, SIM>(
    &self,
    searcher: &IndexSearcher<IRC, SIM>,
    query: Query,
    collector: &mut C,
  ) -> Result<()>
  where
    SIM: Similarity,
    IndexSearcher<IRC, SIM>: Sync,
    C: Collector,
  {
    IndexSearcherDefaults::search_with_collector(searcher, query, collector)
  }

  fn search_with_sort<SIM>(
    &self,
    searcher: &IndexSearcher<IRC, SIM>,
    query: Query,
    n: usize,
    sort: Arc<Sort>,
  ) -> Result<TopFieldDocs>
  where
    SIM: Similarity,
    IndexSearcher<IRC, SIM>: Sync,
  {
    IndexSearcherDefaults::search_with_sort(searcher, query, n, sort)
  }

  fn search_with_collector_manager<CM, SIM>(
    &self,
    searcher: &IndexSearcher<IRC, SIM>,
    query: Query,
    collector_manager: &CM,
  ) -> Result<CM::T>
  where
    SIM: Similarity,
    IndexSearcher<IRC, SIM>: Sync,
    CM: CollectorManager,
    CM::C: Send,
  {
    IndexSearcherDefaults::search_with_collector_manager(searcher, query, collector_manager)
  }

  fn search_partitions<W, C, SIM>(
    &self,
    searcher: &IndexSearcher<IRC, SIM>,
    partitions: &[LeafReaderContextPartition],
    weight: &W,
    collector: &mut C,
  ) -> Result<()>
  where
    SIM: Similarity,
    C: Collector,
    W: Weight<IRC, SIM> + ?Sized,
    IndexSearcher<IRC, SIM>: Sync,
  {
    IndexSearcherDefaults::search_partitions(searcher, partitions, weight, collector)
  }

  fn search_leaf<W, C, SIM>(
    &self,
    searcher: &IndexSearcher<IRC, SIM>,
    ctx_ord: usize,
    min_doc_id: i32,
    max_doc_id: i32,
    weight: &W,
    collector: &mut C,
  ) -> Result<()>
  where
    SIM: Similarity,
    C: Collector,
    W: Weight<IRC, SIM> + ?Sized,
    IndexSearcher<IRC, SIM>: Sync,
  {
    IndexSearcherDefaults::search_leaf(searcher, ctx_ord, min_doc_id, max_doc_id, weight, collector)
  }

  fn rewrite<SIM>(&self, searcher: &IndexSearcher<IRC, SIM>, original: Query) -> Result<Query>
  where
    SIM: Similarity,
    IndexSearcher<IRC, SIM>: Sync,
  {
    IndexSearcherDefaults::rewrite(searcher, original)
  }

  fn create_weight<T, SIM>(
    &self,
    searcher: &IndexSearcher<IRC, SIM>,
    query: T,
    score_mode: ScoreMode,
    boost: f32,
  ) -> Result<QueryWeight<IRC, SIM>>
  where
    SIM: Similarity,
    T: QueryBase,
    T::Weight<IRC, SIM>: Into<QueryWeight<IRC, SIM>>,
    IndexSearcher<IRC, SIM>: Sync,
  {
    IndexSearcherDefaults::create_weight(searcher, query, score_mode, boost)
  }

  fn term_statistics<SIM>(
    &self,
    _searcher: &IndexSearcher<IRC, SIM>,
    term: Arc<Term>,
    doc_freq: i32,
    total_term_freq: i64,
  ) -> Result<TermStatistics>
  where
    SIM: Similarity,
  {
    IndexSearcherDefaults::term_statistics(term, doc_freq, total_term_freq)
  }

  fn collection_statistics<SIM>(
    &self,
    searcher: &IndexSearcher<IRC, SIM>,
    field: &str,
  ) -> Result<Option<CollectionStatistics>>
  where
    SIM: Similarity,
  {
    IndexSearcherDefaults::collection_statistics(searcher, field)
  }
}

impl<IRC> IndexSearcherBase<IRC> for IndexSearcherHook
where
  IRC: IndexReaderContext + 'static,
{
  fn slices<SIM>(
    &self,
    _searcher: &IndexSearcher<IRC, SIM>,
    leaves: &[LeafReaderContext<IRC::LeafReader>],
  ) -> Result<Vec<LeafSlice>>
  where
    SIM: Similarity,
  {
    match self {
      Self::Default => IndexSearcherDefaults::slices(leaves),
      #[cfg(test)]
      Self::GetSlices(hook) => hook.slices(_searcher, leaves),
      #[cfg(test)]
      Self::SlicesOffloadedToExecutor(hook) => hook.slices(_searcher, leaves),
      #[cfg(test)]
      Self::SegmentPartitionsSameSlice(hook) => hook.slices(_searcher, leaves),
      #[cfg(test)]
      Self::IntraSliceDocIdOrderWithPartitions(hook) => hook.slices(_searcher, leaves),
      #[cfg(test)]
      Self::NoRewrite(hook) => hook.slices(_searcher, leaves),
      #[cfg(test)]
      Self::Counting(hook) => hook.slices(_searcher, leaves),
      #[cfg(test)]
      Self::Scorer(hook) => hook.slices(_searcher, leaves),
      #[cfg(test)]
      Self::CustomSearcher(hook) => hook.slices(_searcher, leaves),
      #[cfg(test)]
      Self::Shard(hook) => hook.slices(_searcher, leaves),
      #[cfg(test)]
      Self::Asserting(hook) => hook.slices(_searcher, leaves),
    }
  }

  fn count<SIM>(&self, searcher: &IndexSearcher<IRC, SIM>, query: Query) -> Result<i32>
  where
    SIM: Similarity,
    IndexSearcher<IRC, SIM>: Sync,
  {
    match self {
      Self::Default => IndexSearcherDefaults::count(searcher, query),
      #[cfg(test)]
      Self::GetSlices(hook) => hook.count(searcher, query),
      #[cfg(test)]
      Self::SlicesOffloadedToExecutor(hook) => hook.count(searcher, query),
      #[cfg(test)]
      Self::SegmentPartitionsSameSlice(hook) => hook.count(searcher, query),
      #[cfg(test)]
      Self::IntraSliceDocIdOrderWithPartitions(hook) => hook.count(searcher, query),
      #[cfg(test)]
      Self::NoRewrite(hook) => hook.count(searcher, query),
      #[cfg(test)]
      Self::Counting(hook) => hook.count(searcher, query),
      #[cfg(test)]
      Self::Scorer(hook) => hook.count(searcher, query),
      #[cfg(test)]
      Self::CustomSearcher(hook) => hook.count(searcher, query),
      #[cfg(test)]
      Self::Shard(hook) => hook.count(searcher, query),
      #[cfg(test)]
      Self::Asserting(hook) => hook.count(searcher, query),
    }
  }

  fn search_after_score<SIM>(
    &self,
    searcher: &IndexSearcher<IRC, SIM>,
    after: Option<ScoreDoc>,
    query: Query,
    num_hits: usize,
  ) -> Result<TopDocs<ScoreDoc>>
  where
    SIM: Similarity,
    IndexSearcher<IRC, SIM>: Sync,
  {
    match self {
      Self::Default => IndexSearcherDefaults::search_after_score(searcher, after, query, num_hits),
      #[cfg(test)]
      Self::GetSlices(hook) => hook.search_after_score(searcher, after, query, num_hits),
      #[cfg(test)]
      Self::SlicesOffloadedToExecutor(hook) => {
        hook.search_after_score(searcher, after, query, num_hits)
      },
      #[cfg(test)]
      Self::SegmentPartitionsSameSlice(hook) => {
        hook.search_after_score(searcher, after, query, num_hits)
      },
      #[cfg(test)]
      Self::IntraSliceDocIdOrderWithPartitions(hook) => {
        hook.search_after_score(searcher, after, query, num_hits)
      },
      #[cfg(test)]
      Self::NoRewrite(hook) => hook.search_after_score(searcher, after, query, num_hits),
      #[cfg(test)]
      Self::Counting(hook) => hook.search_after_score(searcher, after, query, num_hits),
      #[cfg(test)]
      Self::Scorer(hook) => hook.search_after_score(searcher, after, query, num_hits),
      #[cfg(test)]
      Self::CustomSearcher(hook) => hook.search_after_score(searcher, after, query, num_hits),
      #[cfg(test)]
      Self::Shard(hook) => hook.search_after_score(searcher, after, query, num_hits),
      #[cfg(test)]
      Self::Asserting(hook) => hook.search_after_score(searcher, after, query, num_hits),
    }
  }

  fn search<SIM>(
    &self,
    searcher: &IndexSearcher<IRC, SIM>,
    query: Query,
    n: usize,
  ) -> Result<TopDocs<ScoreDoc>>
  where
    SIM: Similarity,
    IndexSearcher<IRC, SIM>: Sync,
  {
    match self {
      Self::Default => IndexSearcherDefaults::search(searcher, query, n),
      #[cfg(test)]
      Self::GetSlices(hook) => hook.search(searcher, query, n),
      #[cfg(test)]
      Self::SlicesOffloadedToExecutor(hook) => hook.search(searcher, query, n),
      #[cfg(test)]
      Self::SegmentPartitionsSameSlice(hook) => hook.search(searcher, query, n),
      #[cfg(test)]
      Self::IntraSliceDocIdOrderWithPartitions(hook) => hook.search(searcher, query, n),
      #[cfg(test)]
      Self::NoRewrite(hook) => hook.search(searcher, query, n),
      #[cfg(test)]
      Self::Counting(hook) => hook.search(searcher, query, n),
      #[cfg(test)]
      Self::Scorer(hook) => hook.search(searcher, query, n),
      #[cfg(test)]
      Self::CustomSearcher(hook) => hook.search(searcher, query, n),
      #[cfg(test)]
      Self::Shard(hook) => hook.search(searcher, query, n),
      #[cfg(test)]
      Self::Asserting(hook) => hook.search(searcher, query, n),
    }
  }

  fn search_with_collector<C, SIM>(
    &self,
    searcher: &IndexSearcher<IRC, SIM>,
    query: Query,
    collector: &mut C,
  ) -> Result<()>
  where
    SIM: Similarity,
    IndexSearcher<IRC, SIM>: Sync,
    C: Collector,
  {
    match self {
      Self::Default => IndexSearcherDefaults::search_with_collector(searcher, query, collector),
      #[cfg(test)]
      Self::GetSlices(hook) => hook.search_with_collector(searcher, query, collector),
      #[cfg(test)]
      Self::SlicesOffloadedToExecutor(hook) => {
        hook.search_with_collector(searcher, query, collector)
      },
      #[cfg(test)]
      Self::SegmentPartitionsSameSlice(hook) => {
        hook.search_with_collector(searcher, query, collector)
      },
      #[cfg(test)]
      Self::IntraSliceDocIdOrderWithPartitions(hook) => {
        hook.search_with_collector(searcher, query, collector)
      },
      #[cfg(test)]
      Self::NoRewrite(hook) => hook.search_with_collector(searcher, query, collector),
      #[cfg(test)]
      Self::Counting(hook) => hook.search_with_collector(searcher, query, collector),
      #[cfg(test)]
      Self::Scorer(hook) => hook.search_with_collector(searcher, query, collector),
      #[cfg(test)]
      Self::CustomSearcher(hook) => hook.search_with_collector(searcher, query, collector),
      #[cfg(test)]
      Self::Shard(hook) => hook.search_with_collector(searcher, query, collector),
      #[cfg(test)]
      Self::Asserting(hook) => hook.search_with_collector(searcher, query, collector),
    }
  }

  fn search_with_sort<SIM>(
    &self,
    searcher: &IndexSearcher<IRC, SIM>,
    query: Query,
    n: usize,
    sort: Arc<Sort>,
  ) -> Result<TopFieldDocs>
  where
    SIM: Similarity,
    IndexSearcher<IRC, SIM>: Sync,
  {
    match self {
      Self::Default => IndexSearcherDefaults::search_with_sort(searcher, query, n, sort),
      #[cfg(test)]
      Self::GetSlices(hook) => hook.search_with_sort(searcher, query, n, sort),
      #[cfg(test)]
      Self::SlicesOffloadedToExecutor(hook) => hook.search_with_sort(searcher, query, n, sort),
      #[cfg(test)]
      Self::SegmentPartitionsSameSlice(hook) => hook.search_with_sort(searcher, query, n, sort),
      #[cfg(test)]
      Self::IntraSliceDocIdOrderWithPartitions(hook) => {
        hook.search_with_sort(searcher, query, n, sort)
      },
      #[cfg(test)]
      Self::NoRewrite(hook) => hook.search_with_sort(searcher, query, n, sort),
      #[cfg(test)]
      Self::Counting(hook) => hook.search_with_sort(searcher, query, n, sort),
      #[cfg(test)]
      Self::Scorer(hook) => hook.search_with_sort(searcher, query, n, sort),
      #[cfg(test)]
      Self::CustomSearcher(hook) => hook.search_with_sort(searcher, query, n, sort),
      #[cfg(test)]
      Self::Shard(hook) => hook.search_with_sort(searcher, query, n, sort),
      #[cfg(test)]
      Self::Asserting(hook) => hook.search_with_sort(searcher, query, n, sort),
    }
  }

  fn search_with_collector_manager<CM, SIM>(
    &self,
    searcher: &IndexSearcher<IRC, SIM>,
    query: Query,
    collector_manager: &CM,
  ) -> Result<CM::T>
  where
    SIM: Similarity,
    IndexSearcher<IRC, SIM>: Sync,
    CM: CollectorManager,
    CM::C: Send,
  {
    match self {
      Self::Default => {
        IndexSearcherDefaults::search_with_collector_manager(searcher, query, collector_manager)
      },
      #[cfg(test)]
      Self::GetSlices(hook) => {
        hook.search_with_collector_manager(searcher, query, collector_manager)
      },
      #[cfg(test)]
      Self::SlicesOffloadedToExecutor(hook) => {
        hook.search_with_collector_manager(searcher, query, collector_manager)
      },
      #[cfg(test)]
      Self::SegmentPartitionsSameSlice(hook) => {
        hook.search_with_collector_manager(searcher, query, collector_manager)
      },
      #[cfg(test)]
      Self::IntraSliceDocIdOrderWithPartitions(hook) => {
        hook.search_with_collector_manager(searcher, query, collector_manager)
      },
      #[cfg(test)]
      Self::NoRewrite(hook) => {
        hook.search_with_collector_manager(searcher, query, collector_manager)
      },
      #[cfg(test)]
      Self::Counting(hook) => {
        hook.search_with_collector_manager(searcher, query, collector_manager)
      },
      #[cfg(test)]
      Self::Scorer(hook) => hook.search_with_collector_manager(searcher, query, collector_manager),
      #[cfg(test)]
      Self::CustomSearcher(hook) => {
        hook.search_with_collector_manager(searcher, query, collector_manager)
      },
      #[cfg(test)]
      Self::Shard(hook) => hook.search_with_collector_manager(searcher, query, collector_manager),
      #[cfg(test)]
      Self::Asserting(hook) => {
        hook.search_with_collector_manager(searcher, query, collector_manager)
      },
    }
  }

  fn search_partitions<W, C, SIM>(
    &self,
    searcher: &IndexSearcher<IRC, SIM>,
    partitions: &[LeafReaderContextPartition],
    weight: &W,
    collector: &mut C,
  ) -> Result<()>
  where
    SIM: Similarity,
    C: Collector,
    W: Weight<IRC, SIM> + ?Sized,
    IndexSearcher<IRC, SIM>: Sync,
  {
    match self {
      Self::Default => {
        IndexSearcherDefaults::search_partitions(searcher, partitions, weight, collector)
      },
      #[cfg(test)]
      Self::GetSlices(hook) => hook.search_partitions(searcher, partitions, weight, collector),
      #[cfg(test)]
      Self::SlicesOffloadedToExecutor(hook) => {
        hook.search_partitions(searcher, partitions, weight, collector)
      },
      #[cfg(test)]
      Self::SegmentPartitionsSameSlice(hook) => {
        hook.search_partitions(searcher, partitions, weight, collector)
      },
      #[cfg(test)]
      Self::IntraSliceDocIdOrderWithPartitions(hook) => {
        hook.search_partitions(searcher, partitions, weight, collector)
      },
      #[cfg(test)]
      Self::NoRewrite(hook) => hook.search_partitions(searcher, partitions, weight, collector),
      #[cfg(test)]
      Self::Counting(hook) => hook.search_partitions(searcher, partitions, weight, collector),
      #[cfg(test)]
      Self::Scorer(hook) => hook.search_partitions(searcher, partitions, weight, collector),
      #[cfg(test)]
      Self::CustomSearcher(hook) => hook.search_partitions(searcher, partitions, weight, collector),
      #[cfg(test)]
      Self::Shard(hook) => hook.search_partitions(searcher, partitions, weight, collector),
      #[cfg(test)]
      Self::Asserting(hook) => hook.search_partitions(searcher, partitions, weight, collector),
    }
  }

  fn search_leaf<W, C, SIM>(
    &self,
    searcher: &IndexSearcher<IRC, SIM>,
    ctx_ord: usize,
    min_doc_id: i32,
    max_doc_id: i32,
    weight: &W,
    collector: &mut C,
  ) -> Result<()>
  where
    SIM: Similarity,
    C: Collector,
    W: Weight<IRC, SIM> + ?Sized,
    IndexSearcher<IRC, SIM>: Sync,
  {
    match self {
      Self::Default => IndexSearcherDefaults::search_leaf(
        searcher, ctx_ord, min_doc_id, max_doc_id, weight, collector,
      ),
      #[cfg(test)]
      Self::GetSlices(hook) => {
        hook.search_leaf(searcher, ctx_ord, min_doc_id, max_doc_id, weight, collector)
      },
      #[cfg(test)]
      Self::SlicesOffloadedToExecutor(hook) => {
        hook.search_leaf(searcher, ctx_ord, min_doc_id, max_doc_id, weight, collector)
      },
      #[cfg(test)]
      Self::SegmentPartitionsSameSlice(hook) => {
        hook.search_leaf(searcher, ctx_ord, min_doc_id, max_doc_id, weight, collector)
      },
      #[cfg(test)]
      Self::IntraSliceDocIdOrderWithPartitions(hook) => {
        hook.search_leaf(searcher, ctx_ord, min_doc_id, max_doc_id, weight, collector)
      },
      #[cfg(test)]
      Self::NoRewrite(hook) => {
        hook.search_leaf(searcher, ctx_ord, min_doc_id, max_doc_id, weight, collector)
      },
      #[cfg(test)]
      Self::Counting(hook) => {
        hook.search_leaf(searcher, ctx_ord, min_doc_id, max_doc_id, weight, collector)
      },
      #[cfg(test)]
      Self::Scorer(hook) => {
        hook.search_leaf(searcher, ctx_ord, min_doc_id, max_doc_id, weight, collector)
      },
      #[cfg(test)]
      Self::CustomSearcher(hook) => {
        hook.search_leaf(searcher, ctx_ord, min_doc_id, max_doc_id, weight, collector)
      },
      #[cfg(test)]
      Self::Shard(hook) => {
        hook.search_leaf(searcher, ctx_ord, min_doc_id, max_doc_id, weight, collector)
      },
      #[cfg(test)]
      Self::Asserting(hook) => {
        hook.search_leaf(searcher, ctx_ord, min_doc_id, max_doc_id, weight, collector)
      },
    }
  }

  fn rewrite<SIM>(&self, searcher: &IndexSearcher<IRC, SIM>, original: Query) -> Result<Query>
  where
    SIM: Similarity,
    IndexSearcher<IRC, SIM>: Sync,
  {
    match self {
      Self::Default => IndexSearcherDefaults::rewrite(searcher, original),
      #[cfg(test)]
      Self::GetSlices(hook) => hook.rewrite(searcher, original),
      #[cfg(test)]
      Self::SlicesOffloadedToExecutor(hook) => hook.rewrite(searcher, original),
      #[cfg(test)]
      Self::SegmentPartitionsSameSlice(hook) => hook.rewrite(searcher, original),
      #[cfg(test)]
      Self::IntraSliceDocIdOrderWithPartitions(hook) => hook.rewrite(searcher, original),
      #[cfg(test)]
      Self::NoRewrite(hook) => hook.rewrite(searcher, original),
      #[cfg(test)]
      Self::Counting(hook) => hook.rewrite(searcher, original),
      #[cfg(test)]
      Self::Scorer(hook) => hook.rewrite(searcher, original),
      #[cfg(test)]
      Self::CustomSearcher(hook) => hook.rewrite(searcher, original),
      #[cfg(test)]
      Self::Shard(hook) => hook.rewrite(searcher, original),
      #[cfg(test)]
      Self::Asserting(hook) => hook.rewrite(searcher, original),
    }
  }

  fn create_weight<T, SIM>(
    &self,
    searcher: &IndexSearcher<IRC, SIM>,
    query: T,
    score_mode: ScoreMode,
    boost: f32,
  ) -> Result<QueryWeight<IRC, SIM>>
  where
    SIM: Similarity,
    T: QueryBase,
    T::Weight<IRC, SIM>: Into<QueryWeight<IRC, SIM>>,
    IndexSearcher<IRC, SIM>: Sync,
  {
    match self {
      Self::Default => IndexSearcherDefaults::create_weight(searcher, query, score_mode, boost),
      #[cfg(test)]
      Self::GetSlices(hook) => hook.create_weight(searcher, query, score_mode, boost),
      #[cfg(test)]
      Self::SlicesOffloadedToExecutor(hook) => {
        hook.create_weight(searcher, query, score_mode, boost)
      },
      #[cfg(test)]
      Self::SegmentPartitionsSameSlice(hook) => {
        hook.create_weight(searcher, query, score_mode, boost)
      },
      #[cfg(test)]
      Self::IntraSliceDocIdOrderWithPartitions(hook) => {
        hook.create_weight(searcher, query, score_mode, boost)
      },
      #[cfg(test)]
      Self::NoRewrite(hook) => hook.create_weight(searcher, query, score_mode, boost),
      #[cfg(test)]
      Self::Counting(hook) => hook.create_weight(searcher, query, score_mode, boost),
      #[cfg(test)]
      Self::Scorer(hook) => hook.create_weight(searcher, query, score_mode, boost),
      #[cfg(test)]
      Self::CustomSearcher(hook) => hook.create_weight(searcher, query, score_mode, boost),
      #[cfg(test)]
      Self::Shard(hook) => hook.create_weight(searcher, query, score_mode, boost),
      #[cfg(test)]
      Self::Asserting(hook) => hook.create_weight(searcher, query, score_mode, boost),
    }
  }

  fn term_statistics<SIM>(
    &self,
    _searcher: &IndexSearcher<IRC, SIM>,
    term: Arc<Term>,
    doc_freq: i32,
    total_term_freq: i64,
  ) -> Result<TermStatistics>
  where
    SIM: Similarity,
  {
    match self {
      Self::Default => IndexSearcherDefaults::term_statistics(term, doc_freq, total_term_freq),
      #[cfg(test)]
      Self::GetSlices(hook) => hook.term_statistics(_searcher, term, doc_freq, total_term_freq),
      #[cfg(test)]
      Self::SlicesOffloadedToExecutor(hook) => {
        hook.term_statistics(_searcher, term, doc_freq, total_term_freq)
      },
      #[cfg(test)]
      Self::SegmentPartitionsSameSlice(hook) => {
        hook.term_statistics(_searcher, term, doc_freq, total_term_freq)
      },
      #[cfg(test)]
      Self::IntraSliceDocIdOrderWithPartitions(hook) => {
        hook.term_statistics(_searcher, term, doc_freq, total_term_freq)
      },
      #[cfg(test)]
      Self::NoRewrite(hook) => hook.term_statistics(_searcher, term, doc_freq, total_term_freq),
      #[cfg(test)]
      Self::Counting(hook) => hook.term_statistics(_searcher, term, doc_freq, total_term_freq),
      #[cfg(test)]
      Self::Scorer(hook) => hook.term_statistics(_searcher, term, doc_freq, total_term_freq),
      #[cfg(test)]
      Self::CustomSearcher(hook) => {
        hook.term_statistics(_searcher, term, doc_freq, total_term_freq)
      },
      #[cfg(test)]
      Self::Shard(hook) => hook.term_statistics(_searcher, term, doc_freq, total_term_freq),
      #[cfg(test)]
      Self::Asserting(hook) => hook.term_statistics(_searcher, term, doc_freq, total_term_freq),
    }
  }

  fn collection_statistics<SIM>(
    &self,
    searcher: &IndexSearcher<IRC, SIM>,
    field: &str,
  ) -> Result<Option<CollectionStatistics>>
  where
    SIM: Similarity,
  {
    match self {
      Self::Default => IndexSearcherDefaults::collection_statistics(searcher, field),
      #[cfg(test)]
      Self::GetSlices(hook) => hook.collection_statistics(searcher, field),
      #[cfg(test)]
      Self::SlicesOffloadedToExecutor(hook) => hook.collection_statistics(searcher, field),
      #[cfg(test)]
      Self::SegmentPartitionsSameSlice(hook) => hook.collection_statistics(searcher, field),
      #[cfg(test)]
      Self::IntraSliceDocIdOrderWithPartitions(hook) => hook.collection_statistics(searcher, field),
      #[cfg(test)]
      Self::NoRewrite(hook) => hook.collection_statistics(searcher, field),
      #[cfg(test)]
      Self::Counting(hook) => hook.collection_statistics(searcher, field),
      #[cfg(test)]
      Self::Scorer(hook) => hook.collection_statistics(searcher, field),
      #[cfg(test)]
      Self::CustomSearcher(hook) => hook.collection_statistics(searcher, field),
      #[cfg(test)]
      Self::Shard(hook) => hook.collection_statistics(searcher, field),
      #[cfg(test)]
      Self::Asserting(hook) => hook.collection_statistics(searcher, field),
    }
  }
}

impl IndexSearcherDefaults {
  pub(crate) fn slices<LR>(leaves: &[LeafReaderContext<LR>]) -> Result<Vec<LeafSlice>>
  where
    LR: LeafReader,
  {
    slices(leaves)
  }

  pub(crate) fn count<IRC, SIM>(searcher: &IndexSearcher<IRC, SIM>, query: Query) -> Result<i32>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync,
  {
    let mut query = searcher.rewrite(ConstantScoreQuery::new(query))?;
    if let Query::ConstantScore(csq) = query {
      query = csq.into_inner()
    }

    if let Query::Boolean(boolean_query) = &mut query {
      let has_deletions = searcher.reader_context.reader().has_deletions()?;
      if !has_deletions && boolean_query.is_two_clause_pure_disjunction_with_terms() {
        let [query0, query1, query2] =
          boolean_query.rewrite_two_clause_disjunction_with_terms_for_count(searcher)?;
        let count_term1 = searcher.count(query0)?;
        let count_term2 = searcher.count(query1)?;

        if count_term1 == 0 || count_term2 == 0 {
          return Ok(count_term1.max(count_term2));
        } else if (count_term1.min(count_term2) as f64) / (count_term1.max(count_term2) as f64)
          < 0.1
        {
          return Ok(count_term1 + count_term2 - searcher.count(query2)?);
        }
      }
    }
    let initialized_slices;
    let leaf_slices = match searcher.leaf_slices.get() {
      Some(slices) => slices,
      None => {
        initialized_slices = searcher.get_slices()?;
        &initialized_slices
      },
    };
    let v = TotalHitCountCollectorManager::new(leaf_slices.as_slice());
    searcher.search_with_collector_manager(ConstantScoreQuery::new(query), &v)
  }

  pub(crate) fn search_after_score<IRC, SIM>(
    searcher: &IndexSearcher<IRC, SIM>,
    after: Option<ScoreDoc>,
    query: Query,
    num_hits: usize,
  ) -> Result<TopDocs<ScoreDoc>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync,
  {
    let limit = std::cmp::max(1, searcher.reader_context.reader().max_doc()?);

    if let Some(ref a) = after
      && a.doc >= limit
    {
      return Err(LuceneError::illegal_argument(format!(
        "after.doc exceeds the number of documents in the reader: after.doc={} limit={}",
        a.doc, limit
      )));
    }

    let capped_num_hits = std::cmp::min(num_hits, limit as usize);
    if let Some(after) = after {
      let manager = TopScoreDocCollectorManager::with_after(
        capped_num_hits,
        Some(after),
        TOTAL_HITS_THRESHOLD,
      )?;
      searcher.search_with_collector_manager(query, &manager)
    } else {
      let manager = TopScoreDocCollectorManager::new(capped_num_hits, TOTAL_HITS_THRESHOLD)?;
      searcher.search_with_collector_manager(query, &manager)
    }
  }

  pub(crate) fn search<IRC, SIM>(
    searcher: &IndexSearcher<IRC, SIM>,
    query: Query,
    n: usize,
  ) -> Result<TopDocs<ScoreDoc>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync,
  {
    searcher.search_after_score(None, query, n)
  }

  pub(crate) fn search_with_collector<IRC, C, SIM>(
    searcher: &IndexSearcher<IRC, SIM>,
    query: Query,
    collector: &mut C,
  ) -> Result<()>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync,
    C: Collector,
  {
    let needs_scores = collector.score_mode().needs_scores();
    let query = searcher.rewrite_with_needs_scores(query, needs_scores)?;
    let weight = searcher.create_weight(query, collector.score_mode(), 1.0)?;
    collector.set_weight(Some(&weight))?;
    let leaves = searcher.get_leaf_contexts()?;
    for ctx in leaves {
      searcher.search_leaf(ctx.ord, 0, NO_MORE_DOCS, &weight, collector)?;
    }
    Ok(())
  }

  pub(crate) fn search_with_sort<IRC, SIM>(
    searcher: &IndexSearcher<IRC, SIM>,
    query: Query,
    n: usize,
    sort: Arc<Sort>,
  ) -> Result<TopFieldDocs>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync,
  {
    searcher.search_after_field_with_score(None, query, n, sort, false)
  }

  pub(crate) fn search_with_collector_manager<IRC, CM, SIM>(
    searcher: &IndexSearcher<IRC, SIM>,
    mut query: Query,
    collector_manager: &CM,
  ) -> Result<CM::T>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync,
    CM: CollectorManager,
    CM::C: Send,
  {
    let first_collector = collector_manager.new_collector()?;
    let needs_scores = first_collector.score_mode().needs_scores();
    query = searcher.rewrite_with_needs_scores(query, needs_scores)?;
    let score_mode = first_collector.score_mode();

    searcher.search_with_first_collector(query, score_mode, collector_manager, first_collector)
  }

  pub(crate) fn search_partitions<IRC, W, C, SIM>(
    searcher: &IndexSearcher<IRC, SIM>,
    partitions: &[LeafReaderContextPartition],
    weight: &W,
    collector: &mut C,
  ) -> Result<()>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    C: Collector,
    W: Weight<IRC, SIM> + ?Sized,
    IndexSearcher<IRC, SIM>: Sync,
  {
    collector.set_weight(Some(weight))?;

    for partition in partitions {
      searcher.search_leaf(
        partition.ctx,
        partition.min_doc_id,
        partition.max_doc_id,
        weight,
        collector,
      )?;
    }

    Ok(())
  }

  pub(crate) fn search_leaf<IRC, W, C, SIM>(
    searcher: &IndexSearcher<IRC, SIM>,
    ctx_ord: usize,
    min_doc_id: i32,
    max_doc_id: i32,
    weight: &W,
    collector: &mut C,
  ) -> Result<()>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    C: Collector,
    W: Weight<IRC, SIM> + ?Sized,
    IndexSearcher<IRC, SIM>: Sync,
  {
    let ctx = &searcher.reader_context.leaves()?[ctx_ord];
    let mut leaf_collector = match collector.get_leaf_collector(ctx, Some(weight), searcher) {
      Ok(leaf_collector) => leaf_collector,
      Err(error) if matches!(error.kind(), LuceneErrorKind::CollectionTerminated(_)) => {
        // there is no doc of interest in this reader context
        // continue with the following leaf
        return Ok(());
      },
      Err(e) => return Err(e),
    };

    if let Some(mut scorer_supplier) = weight.scorer_supplier(ctx, searcher)? {
      scorer_supplier.set_top_level_scoring_clause()?;
      let mut scorer = match scorer_supplier.bulk_scorer(ctx, searcher)? {
        Some(scorer) => scorer,
        None => return Err(LuceneError::illegal_state("BulkScorer is None")),
      };
      let bits = ctx.reader().get_live_docs()?;
      let live_docs = bits.as_ref();
      let result: Result<()> = (|| {
        let _ = match searcher.query_timeout {
          None => scorer.score(&mut leaf_collector, live_docs, min_doc_id, max_doc_id)?,
          Some(ref qt) => {
            let mut scorer = TimeLimitingBulkScorer::new(scorer, qt);
            scorer.score(&mut leaf_collector, live_docs, min_doc_id, max_doc_id)?
          },
        };
        Ok(())
      })();

      match result {
        Ok(_) => {},
        Err(error) if matches!(error.kind(), LuceneErrorKind::CollectionTerminated(_)) => {
          // collection was terminated prematurely
          // continue with the following leaf
        },
        Err(error) if matches!(error.kind(), LuceneErrorKind::TimeExceeded(_)) => {
          searcher.partial_result.store(true, Ordering::SeqCst);
        },
        Err(e) => return Err(e),
      }
    }
    // Note: this is called if collection ran successfully, including the above special cases of
    // collection-terminated and time-exceeded errors, but no other error.
    leaf_collector.finish()?;
    Ok(())
  }

  pub(crate) fn rewrite<IRC, SIM>(
    searcher: &IndexSearcher<IRC, SIM>,
    mut query: Query,
  ) -> Result<Query>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync,
  {
    while let Some(rewritten) = query.rewrite(searcher)? {
      query = rewritten;
    }
    query.visit(&mut Self::get_num_clauses_check_visitor())?;
    Ok(query)
  }

  fn get_num_clauses_check_visitor() -> NumClausesCheckVisitor {
    NumClausesCheckVisitor { num_clauses: 0 }
  }

  pub(crate) fn create_weight<IRC, T, SIM>(
    searcher: &IndexSearcher<IRC, SIM>,
    query: T,
    score_mode: ScoreMode,
    boost: f32,
  ) -> Result<QueryWeight<IRC, SIM>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync,
    T: QueryBase,
    T::Weight<IRC, SIM>: Into<QueryWeight<IRC, SIM>>,
  {
    let mut weight: QueryWeight<IRC, SIM> =
      query.create_weight(searcher, &score_mode, boost)?.into();
    if !score_mode.needs_scores()
      && let Some(query_cache) = searcher.query_cache.as_ref()
    {
      weight = query_cache.do_cache(weight, searcher.query_caching_policy.clone())?;
    }

    Ok(weight)
  }

  pub(crate) fn term_statistics(
    term: Arc<Term>,
    doc_freq: i32,
    total_term_freq: i64,
  ) -> Result<TermStatistics> {
    TermStatistics::new(term, doc_freq as i64, total_term_freq)
  }

  pub(crate) fn collection_statistics<IRC, SIM>(
    searcher: &IndexSearcher<IRC, SIM>,
    field: &str,
  ) -> Result<Option<CollectionStatistics>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
  {
    let mut doc_count: i64 = 0;
    let mut sum_total_term_freq: i64 = 0;
    let mut sum_doc_freq: i64 = 0;

    for leaf in searcher.reader_context.leaves()? {
      let reader = leaf.reader();
      let terms = get_terms(reader, field)?;
      doc_count += terms.get_doc_count()? as i64;
      sum_total_term_freq += terms.get_sum_total_term_freq()?;
      sum_doc_freq += terms.get_sum_doc_freq()?;
    }

    if doc_count == 0 {
      return Ok(None);
    }

    let stats = CollectionStatistics::new(
      field,
      searcher.reader_context.reader().max_doc()? as i64,
      doc_count,
      sum_total_term_freq,
      sum_doc_freq,
    )?;

    Ok(Some(stats))
  }
}

struct NumClausesCheckVisitor {
  num_clauses: usize,
}

impl QueryVisitor for NumClausesCheckVisitor {
  type SubVisitor<'a>
    = &'a mut Self
  where
    Self: 'a;

  fn consume_terms<T: Borrow<Term>>(&mut self, _query: QueryRef<'_>, _terms: &[T]) -> Result<()> {
    if self.num_clauses > get_max_clause_count() {
      return Err(new_nested());
    }
    self.num_clauses += 1;
    Ok(())
  }

  fn consume_terms_matching<A>(
    &mut self,
    _query: QueryRef<'_>,
    _field: &str,
    _automaton: A,
  ) -> Result<()>
  where
    A: Fn() -> Result<Option<ByteRunAutomaton>>,
  {
    if self.num_clauses > get_max_clause_count() {
      return Err(new_nested());
    }
    self.num_clauses += 1;
    Ok(())
  }

  fn visit_leaf(&mut self, _query: QueryRef<'_>) -> Result<()> {
    if self.num_clauses > get_max_clause_count() {
      return Err(new_nested());
    }
    self.num_clauses += 1;
    Ok(())
  }

  fn get_sub_visitor<'a>(
    &'a mut self,
    _occur: crate::core::search::boolean_clause::Occur,
    _parent: QueryRef<'_>,
  ) -> Self::SubVisitor<'a> {
    // Return this instance even for MUST_NOT and not an empty QueryVisitor.
    self
  }
}
