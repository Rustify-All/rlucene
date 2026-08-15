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
use crate::core::document::field::{FieldBase, Store};
use crate::core::document::long_point::LongPoint;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::sorted_numeric_doc_values_field::SortedNumericDocValuesField;
use crate::core::document::string_field::StringField;
use crate::core::document::text_field::TextField;
use crate::core::index::base_composite_reader::{
  BCRStoredFieldsImpl, BCRTermVectorsImpl, BaseCompositeReader, BaseCompositeReaderBase,
};
use crate::core::index::composite_reader::CompositeReader;
use crate::core::index::directory_reader::{self, DirectoryReader, DirectoryReaderBase};
use crate::core::index::doc_values::DocValues;
use crate::core::index::filter_directory_reader::{FilterDirectoryReader, SubReaderWrapper};
use crate::core::index::index_reader::{
  CacheHelper, CompositeReaderContextKind, Identity, IndexReader, IndexReaderBase,
  LeafReaderContextKind,
};
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::leaf_metadata::LeafMetaData;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::{LeafReaderContext, TopParentMeta};
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::term::Term;
use crate::core::index::two_phase_commit::TwoPhaseCommit;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::boolean_query::Builder;
use crate::core::search::collector::Collector;
use crate::core::search::collector_manager::CollectorManager;
use crate::core::search::constant_score_query::ConstantScoreQuery;
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::constant_score_weight::ConstantScoreWeight;
use crate::core::search::disjunction_max_query::DisjunctionMaxQuery;
use crate::core::search::doc_id_set_iterator::AllDISI;
use crate::core::search::explanation::Explanation;
use crate::core::search::index_or_doc_values_query::IndexOrDocValuesQuery;
use crate::core::search::index_searcher::{
  self, IndexSearcher, IndexSearcherHook, set_default_query_cache, set_default_query_caching_policy,
};
use crate::core::search::knn_collector::KnnCollector;
use crate::core::search::lru_query_cache::{
  HASHTABLE_RAM_BYTES_PER_ENTRY, LRUQueryCache, LRUQueryCacheHook, MinSegmentSizePredicate,
};
use crate::core::search::match_all_docs_query::MatchAllDocsQuery;
use crate::core::search::phrase_query::{self, PhraseQuery};
use crate::core::search::query::{
  Query, QueryBase, QueryWeight, QueryWeightSs, QueryWeightSsBulkScorer, QueryWeightSsScorer,
};
use crate::core::search::query_cache::{QueryCache, QueryCacheEnum};
use crate::core::search::query_caching_policy::{QueryCachingPolicy, QueryCachingPolicyEnum};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::searcher_factory::{SearcherFactory, SearcherFactoryHook};
use crate::core::search::searcher_manager::SearcherManager;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::term_query::TermQuery;
use crate::core::search::weight::{DefaultBulkScorer, Weight};
use crate::core::util::HasIdentity;
use crate::core::util::accountable::Accountable;
use crate::core::util::bits::Bits;
use crate::core::util::close::CloseableRef;
use crate::core::util::dummy::dummy_comparator::DummyComparator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::io_utils::IOUtils;
use crate::core::util::predicate::Predicate;
use crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::search::asserting_index_searcher::AssertingIndexSearcher;
use crate::test_framework::core::search::check_hits::CheckHits;
use crate::test_framework::core::search::dummy_total_hit_count_collector::{
  DummyLeafCollectorImpl, DummyTotalHitCountCollector,
};
pub use crate::test_framework::core::search::query::{DVCacheQuery, TestLRUQuery};
use crate::test_framework::core::search::test_lru_query_cache::{
  CachingSearcherFactory, EvictEmptySegmentCacheLRUQueryCache, FineGrainedStatsLRUQueryCache,
  RandomSegmentSkippingPredicate,
};
use crate::test_framework::core::util::lucene_test_case::{
  MaybeCachePolicy, at_least, is_night_mode, new_directory_shared, new_index_writer_config,
  new_searcher_with_reader, random, random_from_seed, rarely,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::{Rng, RngExt};
use std::collections::{HashMap, HashSet};
use std::fmt::{Debug, Display, Formatter};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

#[allow(dead_code)] // for quick search
struct TestLRUQueryCache;

type TestCache = LRUQueryCache<CacheAllSegments>;

#[derive(Clone, Copy)]
struct CacheAllSegments;

impl Predicate<TopParentMeta> for CacheAllSegments {
  fn test(&self, _context: &TopParentMeta) -> Result<bool> {
    Ok(true)
  }
}

struct AlwaysCache;

impl QueryCachingPolicy for AlwaysCache {
  fn on_use(&self, _query: &Query) {}

  fn should_cache(&self, _query: &Query) -> Result<bool> {
    Ok(true)
  }
}

struct NeverCache;

impl QueryCachingPolicy for NeverCache {
  fn on_use(&self, _query: &Query) {}

  fn should_cache(&self, _query: &Query) -> Result<bool> {
    Ok(false)
  }
}

fn always_cache() -> Arc<QueryCachingPolicyEnum> {
  Arc::new(QueryCachingPolicyEnum::custom(AlwaysCache))
}

fn never_cache() -> Arc<QueryCachingPolicyEnum> {
  Arc::new(QueryCachingPolicyEnum::custom(NeverCache))
}

fn set_cache<IRC>(searcher: &mut IndexSearcher<IRC>, cache: Arc<TestCache>)
where
  IRC: IndexReaderContext + 'static,
{
  searcher.set_query_cache(Some(QueryCacheEnum::custom(cache)));
}

fn cached_queries(cache: &TestCache) -> Vec<Query> {
  cache
    .cached_queries()
    .into_iter()
    .map(|query| query.as_ref().clone())
    .collect()
}
#[allow(clippy::mutable_key_type)]
fn cached_query_set(cache: &TestCache) -> HashSet<Query> {
  cached_queries(cache).into_iter().collect()
}

fn string_doc(field: &str, value: &str, store: Store) -> Result<Document> {
  let mut doc = Document::new();
  doc.add(StringField::from_string(field, value, store)?);
  Ok(doc)
}

struct ScoringTotalHitCountCollector {
  in_: DummyTotalHitCountCollector,
}

impl Collector for ScoringTotalHitCountCollector {
  type LeafCollector<'a, IRC>
    = DummyLeafCollectorImpl<'a>
  where
    Self: 'a,
    IRC: IndexReaderContext + 'a;

  fn get_leaf_collector<'a, W, IRC>(
    &'a mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    weight: Option<&W>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Self::LeafCollector<'a, IRC>>
  where
    IRC: IndexReaderContext,
    W: Weight<IRC> + ?Sized,
  {
    self.in_.get_leaf_collector(context, weight, searcher)
  }

  fn score_mode(&self) -> ScoreMode {
    // Will not use the cache because of scores.
    ScoreMode::Complete
  }
}

struct ScoringTotalHitCountCollectorManager;

impl CollectorManager for ScoringTotalHitCountCollectorManager {
  type C = ScoringTotalHitCountCollector;
  type T = i32;

  fn new_collector(&self) -> Result<Self::C> {
    Ok(ScoringTotalHitCountCollector {
      in_: DummyTotalHitCountCollector::new(),
    })
  }

  fn reduce(&self, collectors: Vec<Self::C>) -> Result<Self::T> {
    DummyTotalHitCountCollector::create_manager().reduce(
      collectors
        .into_iter()
        .map(|collector| collector.in_)
        .collect(),
    )
  }
}

#[test]
fn test_concurrency() -> Result<()> {
  let mut random = random();
  let query_cache = Arc::new(LRUQueryCache::with_skip_cache_factor(
    1 + random.random_range(0..20),
    1 + random.random_range(0..10000),
    f32::INFINITY,
    RandomSegmentSkippingPredicate::new(random_from_seed(random.random())),
  )?);
  let dir = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, dir.clone())?;
  let query_caching_policy = Arc::new(QueryCachingPolicyEnum::custom(MaybeCachePolicy::new(
    random_from_seed(random.random()),
  )));
  let searcher_factory = SearcherFactory::with_hook(SearcherFactoryHook::CachingRandom(
    CachingSearcherFactory::new(query_cache.clone(), query_caching_policy),
  ));
  let apply_deletes = random.random_bool(0.5);
  let mgr =
    SearcherManager::with_writer_deletes(&w.w, apply_deletes, false, Some(searcher_factory))?;
  let indexing = AtomicBool::new(true);
  let error = parking_lot::Mutex::new(None);
  let num_docs = at_least(&mut random, 1000);
  let seeds: Vec<u64> = (0..3).map(|_| random.random()).collect();

  let body_result = std::thread::scope(|scope| -> Result<()> {
    let indexer = scope.spawn(|| {
      let mut random = random_from_seed(seeds[0]);
      for i in 0..num_docs {
        if !indexing.load(Ordering::SeqCst) {
          break;
        }
        let result = (|| -> Result<()> {
          let values = ["blue", "red", "yellow"];
          let mut doc = Document::new();
          doc.add(StringField::from_string(
            "color",
            values[random.random_range(0..values.len())],
            Store::No,
          )?);
          w.add_document(&mut random, doc)?;
          if (i & 63) == 0 {
            mgr.maybe_refresh()?;
            if rarely(&mut random) {
              query_cache.clear();
            }
            if rarely(&mut random) {
              let color = values[random.random_range(0..values.len())];
              w.delete_documents_with_terms(&mut random, vec![Term::from_text("color", color)])?;
            }
          }
          Ok(())
        })();
        if let Err(error_value) = result {
          let mut first_error = error.lock();
          if first_error.is_none() {
            *first_error = Some(error_value);
          }
          break;
        }
      }
      indexing.store(false, Ordering::SeqCst);
    });

    let mut search_threads = Vec::with_capacity(2);
    for seed in seeds.iter().skip(1).copied() {
      let indexing = &indexing;
      let error = &error;
      let mgr = &mgr;
      search_threads.push(scope.spawn(move || {
        let mut random = random_from_seed(seed);
        while indexing.load(Ordering::SeqCst) {
          let result = (|| -> Result<()> {
            let searcher = mgr.acquire()?;
            let search_result = (|| -> Result<()> {
              let values = ["blue", "red", "yellow", "green"];
              let value = values[random.random_range(0..values.len())];
              let query = TermQuery::new(Term::from_text("color", value));
              let collector_manager = DummyTotalHitCountCollector::create_manager();
              // will use the cache
              let total_hits_1 =
                searcher.search_with_collector_manager(query.clone(), &collector_manager)?;
              let total_hits_2 = searcher
                .search_with_collector_manager(query, &ScoringTotalHitCountCollectorManager)?;
              assert_eq!(total_hits_2, total_hits_1);
              Ok(())
            })();
            IOUtils::use_or_suppress_result(search_result, mgr.release(searcher))
          })();
          if let Err(error_value) = result {
            let mut first_error = error.lock();
            if first_error.is_none() {
              *first_error = Some(error_value);
            }
          }
        }
      }));
    }

    indexer
      .join()
      .map_err(|_| LuceneError::illegal_state("indexing thread panicked"))?;
    for thread in search_threads {
      thread
        .join()
        .map_err(|_| LuceneError::illegal_state("search thread panicked"))?;
    }

    if let Some(error) = error.lock().take() {
      return Err(error);
    }
    query_cache.assert_consistent()
  });

  let finally_result = (|| -> Result<()> {
    mgr.close()?;
    w.close(&mut random)?;
    dir.close()?;
    query_cache.assert_consistent()
  })();
  finally_result?;
  body_result
}

#[test]
fn test_lru_eviction() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, dir.clone())?;

  w.add_document(&mut random, string_doc("color", "blue", Store::No)?)?;
  w.add_document(&mut random, string_doc("color", "red", Store::No)?)?;
  w.add_document(&mut random, string_doc("color", "green", Store::No)?)?;
  let reader = w.get_reader(&mut random)?;
  let mut searcher = new_searcher_with_reader(reader)?;
  let query_cache = Arc::new(LRUQueryCache::with_skip_cache_factor(
    2,
    100000,
    f32::INFINITY,
    CacheAllSegments,
  )?);

  let blue: Query = TermQuery::new(Term::from_text("color", "blue")).into();
  let red: Query = TermQuery::new(Term::from_text("color", "red")).into();
  let green: Query = TermQuery::new(Term::from_text("color", "green")).into();

  assert_eq!(Vec::<Query>::new(), cached_queries(&query_cache));

  set_cache(&mut searcher, query_cache.clone());
  // the filter is not cached on any segment: no changes
  searcher.set_query_caching_policy(never_cache());
  searcher.search(ConstantScoreQuery::new(green.clone()), 1)?;
  assert_eq!(Vec::<Query>::new(), cached_queries(&query_cache));

  searcher.set_query_caching_policy(always_cache());
  searcher.search(ConstantScoreQuery::new(red.clone()), 1)?;
  assert_eq!(vec![red.clone()], cached_queries(&query_cache));

  searcher.search(ConstantScoreQuery::new(green.clone()), 1)?;
  assert_eq!(
    vec![red.clone(), green.clone()],
    cached_queries(&query_cache)
  );

  searcher.search(ConstantScoreQuery::new(red.clone()), 1)?;
  assert_eq!(
    vec![green.clone(), red.clone()],
    cached_queries(&query_cache)
  );

  searcher.search(ConstantScoreQuery::new(blue.clone()), 1)?;
  assert_eq!(
    vec![red.clone(), blue.clone()],
    cached_queries(&query_cache)
  );

  searcher.search(ConstantScoreQuery::new(blue.clone()), 1)?;
  assert_eq!(
    vec![red.clone(), blue.clone()],
    cached_queries(&query_cache)
  );

  searcher.search(ConstantScoreQuery::new(green.clone()), 1)?;
  assert_eq!(vec![blue, green.clone()], cached_queries(&query_cache));

  searcher.set_query_caching_policy(never_cache());
  searcher.search(ConstantScoreQuery::new(red), 1)?;
  assert_eq!(vec![blue_query(), green], cached_queries(&query_cache));

  w.close(&mut random)
}

fn blue_query() -> Query {
  TermQuery::new(Term::from_text("color", "blue")).into()
}

#[test]
fn test_clear_filter() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, dir.clone())?;

  for i in 0..10 {
    let color = if i % 2 == 0 { "red" } else { "blue" };
    w.add_document(&mut random, string_doc("color", color, Store::No)?)?;
  }
  let reader = w.get_reader(&mut random)?;
  let mut searcher = new_searcher_with_reader(reader)?;

  let query1: Query = TermQuery::new(Term::from_text("color", "blue")).into();
  // different instance yet equal
  let query2: Query = TermQuery::new(Term::from_text("color", "blue")).into();

  let query_cache = Arc::new(LRUQueryCache::with_skip_cache_factor(
    i32::MAX,
    i64::MAX,
    1.0,
    CacheAllSegments,
  )?);
  set_cache(&mut searcher, query_cache.clone());
  searcher.set_query_caching_policy(always_cache());

  let boot = random.random();
  searcher.search(
    crate::core::search::boost_query::BoostQuery::new(
      ConstantScoreQuery::new(query1.clone()),
      boot,
    )?,
    1,
  )?;
  assert_eq!(1, cached_queries(&query_cache).len());

  query_cache.clear_query(&query2);

  assert!(cached_queries(&query_cache).is_empty());
  query_cache.assert_consistent()?;

  w.close(&mut random)
}

// This test makes sure that by making the same assumptions as LRUQueryCache, RAMUsageTester
// computes the same memory usage.
#[test]
#[ignore = "Java @AwaitsFix LUCENE-7595"]
fn test_ram_bytes_used_agrees_with_ram_usage_tester() -> Result<()> {
  // TODO: Restore this Java @AwaitsFix test after a Rust equivalent of RamUsageTester is available.
  Ok(())
}

// Test what happens when the cache contains only filters and doc id sets
// that require very little memory. In that case most of the memory is taken
// by the cache itself, not cache entries, and we want to make sure that
// memory usage is not grossly underestimated.
#[test]
#[ignore = "Java @AwaitsFix LUCENE-7595"]
fn test_ram_bytes_used_constant_entry_overhead() -> Result<()> {
  // TODO: Restore this Java @AwaitsFix test after a Rust equivalent of RamUsageTester is available.
  Ok(())
}

/// DummyQuery with Accountable, pretending to be a memory-eating query
fn accountable_dummy_query() -> Query {
  TestLRUQuery::accountable_dummy().into()
}

#[test]
fn test_caching_accountable_query() -> Result<()> {
  let mut random = random();
  let query_cache = Arc::new(LRUQueryCache::with_skip_cache_factor(
    1000000,
    10000000,
    f32::INFINITY,
    CacheAllSegments,
  )?);

  let dir = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, dir.clone())?;
  for _ in 0..100 {
    w.add_document(&mut random, Document::new())?;
  }
  let reader = w.get_reader(&mut random)?;
  let mut searcher = new_searcher_with_reader(reader)?;
  set_cache(&mut searcher, query_cache.clone());
  searcher.set_query_caching_policy(always_cache());

  let num_queries = 100;
  for _ in 0..num_queries {
    searcher.count(accountable_dummy_query())?;
  }
  let query_ram_bytes_used = num_queries * (10 * QUERY_DEFAULT_RAM_BYTES_USED);
  // make sure the query cache reflects the big queries
  assert!(query_cache.ram_bytes_used()? > query_ram_bytes_used);

  w.close(&mut random)
}

#[test]
fn test_consistency_with_accountable_queries() -> Result<()> {
  let mut random = random();
  let query_cache = Arc::new(LRUQueryCache::with_skip_cache_factor(
    1,
    10000000,
    f32::INFINITY,
    CacheAllSegments,
  )?);

  let dir = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;
  writer.add_document(&mut random, Document::new())?;
  let reader = Arc::new(writer.get_reader(&mut random)?);
  let mut searcher = new_searcher_with_reader(reader.clone())?;
  set_cache(&mut searcher, query_cache.clone());
  searcher.set_query_caching_policy(always_cache());

  query_cache.assert_consistent()?;

  let accountable_query = accountable_dummy_query();
  searcher.count(accountable_query.clone())?;
  assert!(
    query_cache.ram_bytes_used()?
      > HASHTABLE_RAM_BYTES_PER_ENTRY + accountable_query.ram_bytes_used()?
  );
  query_cache.assert_consistent()?;

  query_cache.clear_query(&accountable_query);
  assert_eq!(HASHTABLE_RAM_BYTES_PER_ENTRY, query_cache.ram_bytes_used()?);
  query_cache.assert_consistent()?;

  let core_key = reader.clone().get_context()?.leaves()?[0]
    .reader()
    .get_core_cache_helper()?
    .ok_or_else(|| LuceneError::illegal_state("reader is not cacheable"))?
    .get_key();
  query_cache.clear_core_cache_key(&core_key);
  assert_eq!(0, query_cache.ram_bytes_used()?);
  query_cache.assert_consistent()?;

  reader.close()?;
  writer.close(&mut random)?;
  dir.as_ref().close()
}

#[test]
fn test_on_use() -> Result<()> {
  let mut random = random();
  let query_cache = Arc::new(LRUQueryCache::with_skip_cache_factor(
    3,
    100000,
    f32::INFINITY,
    CacheAllSegments,
  )?);

  let dir = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, dir.clone())?;
  for color in ["red", "blue", "green", "yellow", "red", "blue"] {
    w.add_document(&mut random, string_doc("color", color, Store::No)?)?;
  }
  let reader = w.get_reader(&mut random)?;
  let mut searcher = new_searcher_with_reader(reader)?;

  let actual_counts = Arc::new(parking_lot::Mutex::new(HashMap::<Query, i32>::new()));
  let expected_counts = Arc::new(parking_lot::Mutex::new(HashMap::<Query, i32>::new()));

  struct CountingPolicy {
    counts: Arc<parking_lot::Mutex<HashMap<Query, i32>>>,
  }

  impl QueryCachingPolicy for CountingPolicy {
    fn on_use(&self, query: &Query) {
      let mut counts = self.counts.lock();
      *counts.entry(query.clone()).or_insert(0) += 1;
    }

    fn should_cache(&self, _query: &Query) -> Result<bool> {
      Ok(true)
    }
  }

  let queries: Vec<Query> = ["red", "blue", "green", "yellow"]
    .iter()
    .map(|color| {
      crate::core::search::boost_query::BoostQuery::new(
        TermQuery::new(Term::from_text("color", *color)),
        1.25,
      )
      .map(Into::into)
    })
    .collect::<Result<Vec<Query>>>()?;

  set_cache(&mut searcher, query_cache);
  searcher.set_query_caching_policy(Arc::new(QueryCachingPolicyEnum::custom(CountingPolicy {
    counts: expected_counts.clone(),
  })));
  for i in 0..20 {
    let query = queries[i % queries.len()].clone();
    searcher.search(ConstantScoreQuery::new(query.clone()), 1)?;
    let mut cache_key = query;
    while let Query::Boost(boost) = cache_key {
      cache_key = boost.into_inner();
    }
    let mut counts = actual_counts.lock();
    *counts.entry(cache_key).or_insert(0) += 1;
  }

  assert_eq!(*actual_counts.lock(), *expected_counts.lock());

  w.close(&mut random)
}

#[test]
fn test_stats() -> Result<()> {
  let mut random = random();
  let query_cache = Arc::new(LRUQueryCache::with_skip_cache_factor(
    1,
    10000000,
    1.0,
    CacheAllSegments,
  )?);

  let dir = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, dir.clone())?;

  for color in ["blue", "red", "green", "yellow", "red", "blue"] {
    w.add_document(&mut random, string_doc("color", color, Store::No)?)?;
  }

  let reader = Arc::new(w.get_reader(&mut random)?);
  let mut searcher = new_searcher_with_reader(reader.clone())?;
  let segment_count = searcher.get_leaf_contexts()?.len() as u64;
  let query: Query = TermQuery::new(Term::from_text("color", "red")).into();
  let query2: Query = TermQuery::new(Term::from_text("color", "blue")).into();

  set_cache(&mut searcher, query_cache.clone());
  // first pass, lookups without caching that all miss
  searcher.set_query_caching_policy(never_cache());
  for _ in 0..10 {
    searcher.search(ConstantScoreQuery::new(query.clone()), 1)?;
  }
  assert_eq!(10 * segment_count, query_cache.get_total_count());
  assert_eq!(0, query_cache.get_hit_count());
  assert_eq!(10 * segment_count, query_cache.get_miss_count());
  assert_eq!(0, query_cache.get_cache_count());
  assert_eq!(0, query_cache.get_eviction_count());
  assert_eq!(0, query_cache.get_cache_size());

  // second pass, lookups + caching, only the first one is a miss
  searcher.set_query_caching_policy(always_cache());
  for _ in 0..10 {
    searcher.search(ConstantScoreQuery::new(query.clone()), 1)?;
  }
  assert_eq!(20 * segment_count, query_cache.get_total_count());
  assert_eq!(9 * segment_count, query_cache.get_hit_count());
  assert_eq!(11 * segment_count, query_cache.get_miss_count());
  assert_eq!(segment_count as i64, query_cache.get_cache_count());
  assert_eq!(0, query_cache.get_eviction_count());
  assert_eq!(segment_count as i64, query_cache.get_cache_size());

  // third pass lookups without caching, we only have hits
  searcher.set_query_caching_policy(never_cache());
  for _ in 0..10 {
    searcher.search(ConstantScoreQuery::new(query.clone()), 1)?;
  }
  assert_eq!(30 * segment_count, query_cache.get_total_count());
  assert_eq!(19 * segment_count, query_cache.get_hit_count());
  assert_eq!(11 * segment_count, query_cache.get_miss_count());
  assert_eq!(segment_count as i64, query_cache.get_cache_count());
  assert_eq!(0, query_cache.get_eviction_count());
  assert_eq!(segment_count as i64, query_cache.get_cache_size());

  // fourth pass with a different filter which will trigger evictions since the size is 1
  searcher.set_query_caching_policy(always_cache());
  for _ in 0..10 {
    searcher.search(ConstantScoreQuery::new(query2.clone()), 1)?;
  }
  assert_eq!(40 * segment_count, query_cache.get_total_count());
  assert_eq!(28 * segment_count, query_cache.get_hit_count());
  assert_eq!(12 * segment_count, query_cache.get_miss_count());
  assert_eq!((2 * segment_count) as i64, query_cache.get_cache_count());
  assert_eq!(segment_count as i64, query_cache.get_eviction_count());
  assert_eq!(segment_count as i64, query_cache.get_cache_size());

  // now close, causing evictions due to the closing of segment cores
  reader.close()?;
  w.close(&mut random)?;
  assert_eq!(40 * segment_count, query_cache.get_total_count());
  assert_eq!(28 * segment_count, query_cache.get_hit_count());
  assert_eq!(12 * segment_count, query_cache.get_miss_count());
  assert_eq!((2 * segment_count) as i64, query_cache.get_cache_count());
  assert_eq!((2 * segment_count) as i64, query_cache.get_eviction_count());
  assert_eq!(0, query_cache.get_cache_size());

  dir.close()
}

#[test]
fn test_fine_grained_stats() -> Result<()> {
  let mut random = random();
  let dir_1 = new_directory_shared(&mut random)?;
  let w_1 = RandomIndexWriter::new(&mut random, dir_1.clone())?;
  let dir_2 = new_directory_shared(&mut random)?;
  let w_2 = RandomIndexWriter::new(&mut random, dir_2.clone())?;

  let colors = ["blue", "red", "green", "yellow"];

  let mut field = StringField::from_string("color", "", Store::No)?;
  for writer in [&w_1, &w_2] {
    for _ in 0..10 {
      field.set_string_value(colors[random.random_range(0..colors.len())])?;
      let mut doc = Document::new();
      doc.add(field.clone());
      writer.add_document(&mut random, doc)?;
      if random.random_bool(0.5) {
        writer.get_reader(&mut random)?.close()?;
      }
    }
  }

  let reader_1 = Arc::new(w_1.get_reader(&mut random)?);
  let reader_context_1 = reader_1.clone().get_context()?;
  let segment_count_1 = reader_context_1.leaves()?.len() as i64;
  let mut searcher_1 = new_searcher_with_reader(reader_1.clone())?;

  let reader_2 = Arc::new(w_2.get_reader(&mut random)?);
  let reader_context_2 = reader_2.clone().get_context()?;
  let segment_count_2 = reader_context_2.leaves()?.len() as i64;
  let mut searcher_2 = new_searcher_with_reader(reader_2.clone())?;

  let mut index_id = HashMap::new();
  for context in reader_context_1.leaves()? {
    let cache_helper = context
      .reader()
      .get_core_cache_helper()?
      .ok_or_else(|| LuceneError::illegal_state("reader is not cacheable"))?;
    index_id.insert(cache_helper.get_key(), 1);
  }
  for context in reader_context_2.leaves()? {
    let cache_helper = context
      .reader()
      .get_core_cache_helper()?
      .ok_or_else(|| LuceneError::illegal_state("reader is not cacheable"))?;
    index_id.insert(cache_helper.get_key(), 2);
  }

  let hook = FineGrainedStatsLRUQueryCache::new(index_id);
  let query_cache = Arc::new(
    LRUQueryCache::with_skip_cache_factor(2, 10000000, 1.0, CacheAllSegments)?
      .with_hook(LRUQueryCacheHook::FineGrainedStats(hook.clone())),
  );

  let query: Query = TermQuery::new(Term::from_text("color", "red")).into();
  let query_2: Query = TermQuery::new(Term::from_text("color", "blue")).into();
  let query_3: Query = TermQuery::new(Term::from_text("color", "green")).into();

  for searcher in [&mut searcher_1, &mut searcher_2] {
    set_cache(searcher, query_cache.clone());
    searcher.set_query_caching_policy(always_cache());
  }

  // search on searcher1
  for _ in 0..10 {
    searcher_1.search(ConstantScoreQuery::new(query.clone()), 1)?;
  }
  assert_eq!(9 * segment_count_1, hook.hit_count_1());
  assert_eq!(0, hook.hit_count_2());
  assert_eq!(segment_count_1, hook.miss_count_1());
  assert_eq!(0, hook.miss_count_2());

  // then on searcher2
  for _ in 0..20 {
    searcher_2.search(ConstantScoreQuery::new(query_2.clone()), 1)?;
  }
  assert_eq!(9 * segment_count_1, hook.hit_count_1());
  assert_eq!(19 * segment_count_2, hook.hit_count_2());
  assert_eq!(segment_count_1, hook.miss_count_1());
  assert_eq!(segment_count_2, hook.miss_count_2());

  // now on searcher1 again to trigger evictions
  for _ in 0..30 {
    searcher_1.search(ConstantScoreQuery::new(query_3.clone()), 1)?;
  }
  assert_eq!(segment_count_1, query_cache.get_eviction_count());
  assert_eq!(38 * segment_count_1, hook.hit_count_1());
  assert_eq!(19 * segment_count_2, hook.hit_count_2());
  assert_eq!(2 * segment_count_1, hook.miss_count_1());
  assert_eq!(segment_count_2, hook.miss_count_2());

  // check that the recomputed stats are the same as those reported by the cache
  assert_eq!(
    query_cache.ram_bytes_used()?,
    (segment_count_1 + segment_count_2) * HASHTABLE_RAM_BYTES_PER_ENTRY + hook.ram_bytes_usage()
  );
  assert_eq!(query_cache.get_cache_size(), hook.cache_size());

  reader_1.close()?;
  reader_2.close()?;
  w_1.close(&mut random)?;
  w_2.close(&mut random)?;

  assert_eq!(query_cache.ram_bytes_used()?, hook.ram_bytes_usage());
  assert_eq!(0, hook.cache_size());

  query_cache.clear();
  assert_eq!(0, hook.ram_bytes_usage());
  assert_eq!(0, hook.cache_size());

  dir_1.as_ref().close()?;
  dir_2.as_ref().close()?;
  Ok(())
}

#[test]
fn test_use_rewritten_query_as_cache_key() -> Result<()> {
  let mut random = random();
  let expected_cache_key: Query = TermQuery::new(Term::from_text("foo", "bar")).into();
  let mut query = Builder::new();
  query.add(
    crate::core::search::boost_query::BoostQuery::new(expected_cache_key.clone(), 42f32)?,
    Occur::Must,
  )?;

  let query_cache = Arc::new(LRUQueryCache::with_skip_cache_factor(
    1000000,
    10000000,
    f32::INFINITY,
    CacheAllSegments,
  )?);
  let dir = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, dir.clone())?;
  w.add_document(&mut random, string_doc("foo", "bar", Store::Yes)?)?;
  w.commit(&mut random)?;
  let reader = w.get_reader(&mut random)?;
  let mut searcher = new_searcher_with_reader(reader)?;
  w.close(&mut random)?;

  struct AssertingPolicy {
    expected: Query,
  }

  impl QueryCachingPolicy for AssertingPolicy {
    fn on_use(&self, query: &Query) {
      assert_eq!(&self.expected, query);
    }

    fn should_cache(&self, query: &Query) -> Result<bool> {
      assert_eq!(&self.expected, query);
      Ok(true)
    }
  }

  set_cache(&mut searcher, query_cache);
  searcher.set_query_caching_policy(Arc::new(QueryCachingPolicyEnum::custom(AssertingPolicy {
    expected: expected_cache_key,
  })));
  searcher.count(query.build())?;

  Ok(())
}

#[test]
fn test_boolean_query_caches_sub_clauses() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, dir.clone())?;
  let mut doc = Document::new();
  doc.add(StringField::from_string("foo", "bar", Store::Yes)?);
  doc.add(StringField::from_string("foo", "quux", Store::Yes)?);
  w.add_document(&mut random, doc)?;
  w.commit(&mut random)?;
  let reader = w.get_reader(&mut random)?;
  let mut searcher = new_searcher_with_reader(reader)?;
  w.close(&mut random)?;

  let query_cache = Arc::new(LRUQueryCache::with_skip_cache_factor(
    1000000,
    10000000,
    f32::INFINITY,
    CacheAllSegments,
  )?);
  set_cache(&mut searcher, query_cache.clone());
  searcher.set_query_caching_policy(always_cache());

  let mut bq = Builder::new();
  let must: Query = TermQuery::new(Term::from_text("foo", "bar")).into();
  let filter: Query = TermQuery::new(Term::from_text("foo", "quux")).into();
  let must_not: Query = TermQuery::new(Term::from_text("foo", "foo")).into();
  bq.add(must.clone(), Occur::Must)?;
  bq.add(filter.clone(), Occur::Filter)?;
  bq.add(must_not.clone(), Occur::MustNot)?;

  // same bq but with FILTER instead of MUST
  let mut bq2 = Builder::new();
  bq2.add(must.clone(), Occur::Filter)?;
  bq2.add(filter.clone(), Occur::Filter)?;
  bq2.add(must_not.clone(), Occur::MustNot)?;

  let bq_query: Query = bq.build().into();

  assert_eq!(HashSet::<Query>::new(), cached_query_set(&query_cache));
  searcher.search(bq_query.clone(), 1)?;
  assert_eq!(
    HashSet::from([filter.clone(), must_not.clone()]),
    cached_query_set(&query_cache)
  );

  query_cache.clear();
  assert_eq!(HashSet::<Query>::new(), cached_query_set(&query_cache));
  let bq2_query: Query = bq2.build().into();
  searcher.search(ConstantScoreQuery::new(bq_query), 1)?;
  assert_eq!(
    HashSet::from([bq2_query, must, filter, must_not]),
    cached_query_set(&query_cache)
  );

  Ok(())
}

fn random_term<R>(random: &mut R) -> Term
where
  R: Rng + ?Sized,
{
  let terms = ["foo", "bar", "baz"];
  Term::from_text("foo", terms[random.random_range(0..terms.len())])
}

fn build_random_query<R>(random: &mut R, level: i32) -> Result<Query>
where
  R: Rng + ?Sized,
{
  if level == 10 {
    // at most 10 levels
    return Ok(MatchAllDocsQuery::new().into());
  }
  match random.random_range(0..6) {
    0 => Ok(TermQuery::new(random_term(random)).into()),
    1 => {
      let mut bq = Builder::new();
      let num_clauses = random.random_range(1..=3);
      let mut num_should = 0;
      for _ in 0..num_clauses {
        let occurs = Occur::values();
        let occur = occurs[random.random_range(0..occurs.len())];
        if occur == Occur::Should {
          num_should += 1;
        }
        bq.add(build_random_query(random, level + 1)?, occur)?;
      }
      bq.set_minimum_number_should_match(random.random_range(0..=num_should));
      Ok(bq.build().into())
    },
    2 => {
      let t1 = random_term(random);
      let t2 = random_term(random);
      Ok(
        PhraseQuery::from_bytes(
          random.random_range(0..2),
          t1.field(),
          vec![t1.bytes().clone(), t2.bytes().clone()],
        )?
        .into(),
      )
    },
    3 => Ok(MatchAllDocsQuery::new().into()),
    4 => Ok(ConstantScoreQuery::new(build_random_query(random, level + 1)?).into()),
    5 => {
      let num_queries = random.random_range(1..=3);
      let mut disjuncts = Vec::with_capacity(num_queries);
      for _ in 0..num_queries {
        disjuncts.push(build_random_query(random, level + 1)?);
      }
      Ok(DisjunctionMaxQuery::new(disjuncts, random.random::<f32>())?.into())
    },
    _ => unreachable!(),
  }
}

#[test]
fn test_random() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, dir.clone())?;
  let mut doc = Document::new();
  let mut f = TextField::from_string("foo", "foo", Store::No)?;
  doc.add(f.clone());
  w.add_document(&mut random, doc)?;

  let (max_size, max_ram_bytes_used, iters) = if is_night_mode() {
    (
      random.random_range(1..=10000),
      random.random_range(1..=5_000_000_i64),
      at_least(&mut random, 20000),
    )
  } else {
    (
      random.random_range(1..=1000),
      random.random_range(1..=500_000_i64),
      at_least(&mut random, 2000),
    )
  };

  let seed = random.random();
  let query_cache = Arc::new(LRUQueryCache::with_skip_cache_factor(
    max_size,
    max_ram_bytes_used,
    f32::INFINITY,
    RandomSegmentSkippingPredicate::new(random_from_seed(seed)),
  )?);

  let mut uncached_searcher = None;
  let mut cached_searcher = None;

  for i in 0..iters {
    if i == 0 || random.random_range(0..100) == 1 {
      let values = ["foo", "bar", "bar baz"];
      f.set_string_value(values[random.random_range(0..values.len())])?;
      let mut doc = Document::new();
      doc.add(f.clone());
      w.add_document(&mut random, doc)?;
      if random.random_bool(0.5) {
        let query = build_random_query(&mut random, 0)?;
        w.delete_documents_with_queries(&mut random, vec![query])?;
      }
      let reader = Arc::new(w.get_reader(&mut random)?);
      let mut new_uncached_searcher = new_searcher_with_reader(reader.clone())?;
      new_uncached_searcher.set_query_cache(None);
      let mut new_cached_searcher = new_searcher_with_reader(reader)?;
      new_cached_searcher.set_query_cache(Some(QueryCacheEnum::custom(query_cache.clone())));
      new_cached_searcher.set_query_caching_policy(always_cache());
      uncached_searcher = Some(new_uncached_searcher);
      cached_searcher = Some(new_cached_searcher);
    }

    let q = build_random_query(&mut random, 0)?;
    let uncached_searcher = uncached_searcher
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("uncached searcher should be initialized"))?;
    let cached_searcher = cached_searcher
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("cached searcher should be initialized"))?;
    /*
     * Counts are the same. If the query has already been cached
     * this'll use the O(1) Weight#count method.
     */
    assert_eq!(
      uncached_searcher.count(q.clone())?,
      cached_searcher.count(q.clone())?
    );
    /*
     * Just to make sure we can iterate every time also check that the
     * same docs are returned in the same order.
     */
    let size = 1 + random.random_range(0..1000);
    let uncached_hits = uncached_searcher.search(q.clone(), size)?.score_docs;
    let cached_hits = cached_searcher.search(q.clone(), size)?.score_docs;
    CheckHits::check_equal(&q, &uncached_hits, &cached_hits)?;
    if rarely(&mut random) {
      query_cache.assert_consistent()?;
    }
  }
  query_cache.assert_consistent()?;
  w.close(&mut random)?;
  query_cache.assert_consistent()
}

#[test]
#[ignore = "Java-only: Rust Query cache keys cannot expose mutable Hash/Eq state (Java test is @AwaitsFix)"]
fn test_detect_mutated_queries() -> Result<()> {
  test_not_required_in_rust_lucene!();
}

#[test]
fn test_refuse_to_cache_too_large_entries() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, dir.clone())?;
  for _ in 0..100 {
    w.add_document(&mut random, Document::new())?;
  }
  let reader = w.get_reader(&mut random)?;

  // size of 1 byte
  let seed = random.random();
  let query_cache = Arc::new(LRUQueryCache::with_skip_cache_factor(
    1,
    1,
    f32::INFINITY,
    RandomSegmentSkippingPredicate::new(random_from_seed(seed)),
  )?);
  let mut searcher = new_searcher_with_reader(reader)?;
  searcher.set_query_cache(Some(QueryCacheEnum::custom(query_cache.clone())));
  searcher.set_query_caching_policy(always_cache());

  searcher.count(MatchAllDocsQuery::new())?;
  assert_eq!(0, query_cache.get_cache_count());
  assert_eq!(0, query_cache.get_eviction_count());

  w.close(&mut random)
}

/**
 * Tests CachingWrapperWeight.scorer() propagation of [`QueryCachingPolicy::on_use`] when
 * the first segment is skipped.
 *
 * #f:foo #f:bar causes all frequencies to increment #f:bar #f:foo does not increment the
 * frequency for f:foo
 */
#[test]
fn test_on_use_with_random_first_segment_skipping() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_merge_policy(crate::core::index::no_merge_policy::NoMergePolicy::default());
  let index_writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

  let mut doc = Document::new();
  doc.add(StringField::from_string("f", "bar", Store::No)?);
  index_writer.add_document(&mut random, doc)?;
  if random.random_bool(0.5) {
    index_writer.get_reader(&mut random)?.close()?;
  }

  let mut doc = Document::new();
  doc.add(StringField::from_string("f", "foo", Store::No)?);
  doc.add(StringField::from_string("f", "bar", Store::No)?);
  index_writer.add_document(&mut random, doc)?;
  index_writer.commit(&mut random)?;
  index_writer.close(&mut random)?;

  let reader = directory_reader::open(dir.clone())?;
  let policy = FrequencyCountingPolicy::new();
  let mut index_searcher = new_searcher_with_reader(reader)?;
  let seed = random.random();
  let cache = Arc::new(LRUQueryCache::with_skip_cache_factor(
    100,
    10240,
    f32::INFINITY,
    RandomSegmentSkippingPredicate::new(random_from_seed(seed)),
  )?);
  index_searcher.set_query_cache(Some(QueryCacheEnum::custom(cache)));
  index_searcher.set_query_caching_policy(Arc::new(QueryCachingPolicyEnum::custom(policy.clone())));
  let foo: Query = TermQuery::new(Term::from_text("f", "foo")).into();
  let bar: Query = TermQuery::new(Term::from_text("f", "bar")).into();
  let mut query = Builder::new();
  if random.random_bool(0.5) {
    query.add(foo.clone(), Occur::Filter)?;
    query.add(bar.clone(), Occur::Filter)?;
  } else {
    query.add(bar.clone(), Occur::Filter)?;
    query.add(foo.clone(), Occur::Filter)?;
  }
  let built = query.build();
  index_searcher.search_with_collector_manager(
    built.clone(),
    &DummyTotalHitCountCollector::create_manager(),
  )?;
  assert_eq!(1, policy.frequency(&built.into()));
  assert_eq!(1, policy.frequency(&foo));
  assert_eq!(1, policy.frequency(&bar));

  Ok(())
}

#[derive(Clone)]
struct FrequencyCountingPolicy {
  counts: Arc<parking_lot::Mutex<HashMap<Query, i32>>>,
}

impl FrequencyCountingPolicy {
  fn new() -> Self {
    Self {
      counts: Arc::new(parking_lot::Mutex::new(HashMap::new())),
    }
  }

  pub fn frequency(&self, query: &Query) -> i32 {
    self.counts.lock().get(query).copied().unwrap_or(0)
  }
}

impl QueryCachingPolicy for FrequencyCountingPolicy {
  fn on_use(&self, query: &Query) {
    let mut counts = self.counts.lock();
    *counts.entry(query.clone()).or_insert(0) += 1;
  }

  fn should_cache(&self, _query: &Query) -> Result<bool> {
    Ok(true)
  }
}

#[test]
fn test_propagate_bulk_scorer() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, dir.clone())?;
  w.add_document(&mut random, Document::new())?;
  let reader = w.get_reader(&mut random)?;
  w.close(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;
  let leaf = &searcher.get_leaf_contexts()?[0];
  let scorer_called = Arc::new(AtomicBool::new(false));
  let bulk_scorer_called = Arc::new(AtomicBool::new(false));
  let cache = Arc::new(LRUQueryCache::with_skip_cache_factor(
    1,
    i64::MAX,
    f32::INFINITY,
    CacheAllSegments,
  )?);

  // test that the bulk scorer is propagated when a scorer should not be cached
  let weight =
    searcher.create_weight(MatchAllDocsQuery::new(), ScoreMode::CompleteNoScores, 1.0)?;
  let weight: QueryWeight<_> = Box::new(WeightWrapper::new(
    weight,
    scorer_called.clone(),
    bulk_scorer_called.clone(),
  ));
  let weight = cache.do_cache(weight, never_cache())?;
  let _ = weight.bulk_scorer(leaf, &searcher)?;
  assert!(bulk_scorer_called.load(Ordering::SeqCst));
  assert!(!scorer_called.load(Ordering::SeqCst));
  assert_eq!(0, cache.get_cache_count());
  Ok(())
}

#[test]
fn test_evict_empty_segment_cache() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, dir.clone())?;
  w.add_document(&mut random, Document::new())?;
  let reader = Arc::new(w.get_reader(&mut random)?);
  let mut searcher = new_searcher_with_reader(reader.clone())?;
  let query_cache = Arc::new(
    LRUQueryCache::with_skip_cache_factor(2, 100000, f32::INFINITY, CacheAllSegments)?.with_hook(
      LRUQueryCacheHook::EvictEmptySegmentCache(EvictEmptySegmentCacheLRUQueryCache),
    ),
  );

  set_cache(&mut searcher, query_cache.clone());
  searcher.set_query_caching_policy(always_cache());

  let query: Query = TestLRUQuery::dummy().into();
  searcher.count(query.clone())?;
  assert_eq!(vec![query.clone()], cached_queries(&query_cache));
  query_cache.clear_query(&query);

  reader.close()?; // make sure this does not trigger eviction of segment caches with no entries
  w.close(&mut random)?;
  dir.as_ref().close()
}

#[test]
fn test_min_segment_size_predicate() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut iwc = IndexWriterConfig::new()?;
  iwc.set_merge_policy(crate::core::index::no_merge_policy::NoMergePolicy::default());
  let writer = IndexWriter::new(dir.clone(), iwc)?;
  let new_segment = |num_docs| -> Result<()> {
    for _ in 0..num_docs {
      writer.add_document(Document::new())?;
    }
    writer.flush()?;
    Ok(())
  };
  new_segment(1)?;
  new_segment(4)?;
  new_segment(10)?;
  new_segment(35)?;
  let num_large_segments = random.random_range(2..=40);
  for _ in 0..num_large_segments {
    new_segment(random.random_range(50..=55))?;
  }
  let reader = directory_reader::open_from_writer(&writer)?;
  let searcher = new_searcher_with_reader(reader)?;
  let leaves = searcher.get_leaf_contexts()?;
  for leaf in leaves.iter().take(3) {
    let predicate = MinSegmentSizePredicate::new(random.random_range(1..=i32::MAX));
    assert!(!predicate.test(leaf.top_parent())?);
  }
  for leaf in leaves.iter().skip(3) {
    let leaf = leaf.top_parent();
    let small = MinSegmentSizePredicate::new(random.random_range(60..=i32::MAX));
    assert!(!small.test(leaf)?);
    let big = MinSegmentSizePredicate::new(random.random_range(10..=30));
    assert!(big.test(leaf)?);
  }
  writer.close()
}

// a reader whose sole purpose is to not be cacheable
struct DummyDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  in_: DR,
  base: BaseCompositeReaderBase<DummyFilterLeafReader<DR::LeafReader>>,
  index_base: IndexReaderBase,
}

impl<DR> DummyDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  fn new(in_: DR) -> Result<Self> {
    let wrapper = DummySubReaderWrapper;
    let readers = wrapper.wrap_readers(in_.get_sequential_sub_readers().to_vec())?;
    let index_base = IndexReaderBase::new();
    let base = BaseCompositeReaderBase::new::<DummyComparator>(readers, None, &index_base)?;
    Ok(Self {
      in_,
      base,
      index_base,
    })
  }
}

impl<DR> BaseCompositeReader for DummyDirectoryReader<DR> where DR: DirectoryReader {}

impl<DR> CompositeReader for DummyDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  type LeafReader = DummyFilterLeafReader<DR::LeafReader>;
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
    format!("DummyDirectoryReader({})", self.in_.to_string())
  }
}

impl<DR> IndexReader for DummyDirectoryReader<DR>
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
    Ok(None)
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

impl<DR> Display for DummyDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", CompositeReader::to_string(self))
  }
}

impl<DR> DirectoryReader for DummyDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  type DirectoryReader = DummyDirectoryReader<DR::DirectoryReader>;

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

impl<DR> FilterDirectoryReader for DummyDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  type Delegate = DR;

  fn get_delegate(&self) -> &Self::Delegate {
    &self.in_
  }

  type WrapDirectoryReader = DummyDirectoryReader<DR::DirectoryReader>;

  fn do_wrap_directory_reader(
    &self,
    in_: Option<<Self::Delegate as DirectoryReader>::DirectoryReader>,
  ) -> Result<Option<Self::WrapDirectoryReader>> {
    in_.map(DummyDirectoryReader::new).transpose()
  }
}

struct DummySubReaderWrapper;

impl<LR> SubReaderWrapper<LR> for DummySubReaderWrapper
where
  LR: LeafReader,
{
  type LeafReader1 = Self::LeafReader2;

  fn wrap_readers(&self, readers: Vec<LR>) -> Result<Vec<Self::LeafReader1>> {
    self.default_wrap_readers(readers)
  }

  type LeafReader2 = DummyFilterLeafReader<LR>;

  fn wrap(&self, reader: LR) -> Result<Self::LeafReader2> {
    DummyFilterLeafReader::new(reader)
  }
}

struct DummyFilterLeafReader<LR>
where
  LR: LeafReader,
{
  in_: LR,
  index_base: IndexReaderBase,
}

impl<LR> DummyFilterLeafReader<LR>
where
  LR: LeafReader,
{
  fn new(in_: LR) -> Result<Self> {
    let index_base = IndexReaderBase::new();
    in_.register_parent_reader(&index_base)?;
    Ok(Self { in_, index_base })
  }
}

impl<LR> Clone for DummyFilterLeafReader<LR>
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

impl<LR> Display for DummyFilterLeafReader<LR>
where
  LR: LeafReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "DummyFilterLeafReader({})", self.in_)
  }
}

impl<LR> IndexReader for DummyFilterLeafReader<LR>
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
    Ok(None)
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
    &self.index_base
  }
}

impl<LR> LeafReader for DummyFilterLeafReader<LR>
where
  LR: LeafReader,
{
  type CacheHelper = LR::CacheHelper;

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    Ok(None)
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

  fn get_field_infos(&self) -> Result<Arc<crate::core::index::field_infos::FieldInfos>> {
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

#[test]
fn test_reader_not_suited_for_caching() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_merge_policy(crate::core::index::no_merge_policy::NoMergePolicy::default());
  let w = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);
  w.add_document(&mut random, Document::new())?;
  let reader = Arc::new(DummyDirectoryReader::new(w.get_reader(&mut random)?)?);
  let mut searcher = new_searcher_with_reader(reader.clone())?;
  searcher.set_query_caching_policy(always_cache());

  // don't cache if the reader does not expose a cache helper
  assert!(
    searcher.get_leaf_contexts()?[0]
      .reader()
      .get_core_cache_helper()?
      .is_none()
  );
  let cache = Arc::new(LRUQueryCache::with_skip_cache_factor(
    2,
    10000,
    f32::INFINITY,
    CacheAllSegments,
  )?);
  set_cache(&mut searcher, cache.clone());
  assert_eq!(0, searcher.count(TestLRUQuery::dummy())?);
  assert_eq!(0, cache.get_cache_count());
  reader.close()?;
  w.close(&mut random)?;
  dir.close()
}

// A query that returns null from Weight.getCacheHelper
fn no_cache_query() -> Query {
  TestLRUQuery::no_cache().into()
}

#[test]
fn test_query_not_suited_for_caching() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_merge_policy(crate::core::index::no_merge_policy::NoMergePolicy::default());
  let w = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);
  w.add_document(&mut random, Document::new())?;
  let reader = w.get_reader(&mut random)?;
  let mut searcher = new_searcher_with_reader(reader)?;
  searcher.set_query_caching_policy(always_cache());

  let cache = Arc::new(LRUQueryCache::with_skip_cache_factor(
    2,
    10000,
    f32::INFINITY,
    CacheAllSegments,
  )?);
  set_cache(&mut searcher, cache.clone());

  assert_eq!(0, searcher.count(no_cache_query())?);
  assert_eq!(0, cache.get_cache_count());

  // BooleanQuery wrapping an uncacheable query should also not be cached
  let mut builder = Builder::new();
  builder.add(no_cache_query(), Occur::Must)?;
  builder.add(
    TermQuery::new(Term::from_text("field", "term")),
    Occur::Must,
  )?;
  let bq = builder.build();
  assert_eq!(0, searcher.count(bq)?);
  assert_eq!(0, cache.get_cache_count());

  w.close(&mut random)
}

fn dummy_query2(scorer_created: Arc<AtomicBool>) -> Query {
  TestLRUQuery::dummy2(scorer_created).into()
}

#[test]
fn test_propagates_scorer_supplier() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_merge_policy(crate::core::index::no_merge_policy::NoMergePolicy::default());
  let w = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);
  w.add_document(&mut random, Document::new())?;
  let reader = w.get_reader(&mut random)?;
  let mut searcher = new_searcher_with_reader(reader)?;
  searcher.set_query_caching_policy(never_cache());

  let cache = Arc::new(LRUQueryCache::new(1, 1000)?);
  searcher.set_query_cache(Some(cache.into()));

  let scorer_created = Arc::new(AtomicBool::new(false));
  let query = dummy_query2(scorer_created.clone());
  let rewritten = searcher.rewrite(query)?;
  let weight = searcher.create_weight(rewritten, ScoreMode::CompleteNoScores, 1.0)?;
  let mut supplier = weight
    .scorer_supplier(&searcher.get_leaf_contexts()?[0], &searcher)?
    .unwrap();
  assert!(!scorer_created.load(Ordering::SeqCst));
  supplier.get(
    random.random::<u64>() as i64 & 0x7FFF_FFFF_FFFF_FFFF,
    &searcher.get_leaf_contexts()?[0],
    &searcher,
  )?;
  assert!(scorer_created.load(Ordering::SeqCst));

  w.close(&mut random)
}

#[test]
fn test_doc_values_updates_dont_break_cache() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_merge_policy(crate::core::index::no_merge_policy::NoMergePolicy::default());
  let writer = IndexWriter::new(dir.clone(), iwc)?;
  writer.add_document(Document::new())?;
  writer.commit()?;

  let cache = Arc::new(LRUQueryCache::with_skip_cache_factor(
    1,
    10000,
    f32::INFINITY,
    CacheAllSegments,
  )?);
  let query = DVCacheQuery::new("field");

  {
    let reader = directory_reader::open_from_writer(&writer)?;
    let mut searcher = new_searcher_with_reader(reader)?.with_hook(IndexSearcherHook::Asserting(
      Box::new(AssertingIndexSearcher::new(random.random())),
    ));
    searcher.set_query_caching_policy(always_cache());
    set_cache(&mut searcher, cache.clone());

    assert_eq!(1, searcher.count(query.clone())?);
    assert_eq!(1, query.scorer_created_count());
    assert_eq!(1, searcher.count(query.clone())?);
    assert_eq!(1, query.scorer_created_count()); // should be cached
  }

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("field", 1));
  doc.add(TextField::from_string("text", "text", Store::No)?);
  writer.add_document(doc)?;

  {
    let reader = directory_reader::open_from_writer(&writer)?;
    let mut searcher = new_searcher_with_reader(reader)?.with_hook(IndexSearcherHook::Asserting(
      Box::new(AssertingIndexSearcher::new(random.random())),
    ));
    searcher.set_query_caching_policy(always_cache());
    set_cache(&mut searcher, cache.clone());

    assert_eq!(2, searcher.count(query.clone())?);
    assert_eq!(2, query.scorer_created_count()); // first segment cached
  }

  {
    let reader = directory_reader::open_from_writer(&writer)?;
    let mut searcher = new_searcher_with_reader(reader)?.with_hook(IndexSearcherHook::Asserting(
      Box::new(AssertingIndexSearcher::new(random.random())),
    ));
    searcher.set_query_caching_policy(always_cache());
    set_cache(&mut searcher, cache.clone());

    assert_eq!(2, searcher.count(query.clone())?);
    assert_eq!(2, query.scorer_created_count()); // both segments cached
  }

  writer.update_numeric_doc_value(Term::from_text("text", "text"), "field", 2)?;
  {
    let reader = directory_reader::open_from_writer(&writer)?;
    let mut searcher = new_searcher_with_reader(reader)?.with_hook(IndexSearcherHook::Asserting(
      Box::new(AssertingIndexSearcher::new(random.random())),
    ));
    searcher.set_query_caching_policy(always_cache());
    set_cache(&mut searcher, cache);

    assert_eq!(2, searcher.count(query.clone())?);
    assert_eq!(3, query.scorer_created_count()); // second segment no longer cached due to DV update

    assert_eq!(2, searcher.count(query.clone())?);
    assert_eq!(4, query.scorer_created_count()); // still no caching
  }

  writer.close()
}

#[test]
fn test_query_cache_soft_update() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_soft_deletes_field("soft_delete");
  let w = IndexWriter::new(dir.clone(), iwc)?;
  let query_cache = Arc::new(LRUQueryCache::with_skip_cache_factor(
    10,
    1000 * 1000,
    f32::INFINITY,
    MinSegmentSizePredicate::new(0),
  )?);
  set_default_query_cache(Some(query_cache.clone()));
  set_default_query_caching_policy(always_cache());

  (|| -> Result<()> {
    let sm = SearcherManager::from_writer(&w, Some(SearcherFactory::new()))?;

    let mut doc = Document::new();
    doc.add(StringField::from_string("id", "1", Store::Yes)?);
    w.add_document(doc)?;

    let mut doc = Document::new();
    doc.add(StringField::from_string("id", "2", Store::Yes)?);
    w.add_document(doc)?;

    sm.maybe_refresh_blocking()?;

    let searcher = sm.acquire()?;
    let mut builder = Builder::new();
    builder.add(TermQuery::new(Term::from_text("id", "1")), Occur::Filter)?;
    let query = builder.build();
    searcher.search(query, 10)?;
    assert_eq!(1, query_cache.get_cache_size());
    assert_eq!(0, query_cache.get_eviction_count());

    let soft_delete = true;
    if soft_delete {
      let mut tombstone = Document::new();
      tombstone.add(NumericDocValuesField::new("soft_delete", 1));
      w.soft_update_document(
        Term::from_text("id", "1"),
        tombstone.clone(),
        vec![NumericDocValuesField::new("soft_delete", 1).into()],
      )?;
      w.soft_update_document(
        Term::from_text("id", "2"),
        tombstone,
        vec![NumericDocValuesField::new("soft_delete", 1).into()],
      )?;
    } else {
      w.delete_documents_with_terms(vec![Term::from_text("id", "1")])?;
      w.delete_documents_with_terms(vec![Term::from_text("id", "2")])?;
    }
    sm.maybe_refresh_blocking()?;
    // All docs in the first segment are deleted - we should drop it with the default merge policy.
    sm.release(searcher)?;
    assert_eq!(0, query_cache.get_cache_size());
    assert_eq!(1, query_cache.get_eviction_count());
    sm.close()?;
    w.close()?;
    dir.close()
  })()
}

#[test]
fn test_bulk_scorer_locking() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc
    .set_merge_policy(crate::core::index::no_merge_policy::NoMergePolicy::default())
    // the test framework sometimes sets crazy low values, prevent this since we are
    // indexing many docs
    .set_max_buffered_docs(-1);
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let num_docs = at_least(&mut random, 10);
  let empty_doc = Document::new();
  for _ in 0..num_docs {
    let num_empty_docs = random.random_range(0..5000);
    for _ in 0..=num_empty_docs {
      w.add_document(empty_doc.clone())?;
    }
    let mut doc = Document::new();
    for value in ["foo", "bar", "baz"] {
      if random.random_bool(0.5) {
        doc.add(StringField::from_string("field", value, Store::No)?);
      }
    }
  }
  let num_empty_docs = TestUtil::next_int(&mut random, 3000, 5000);
  for _ in 0..=num_empty_docs {
    w.add_document(empty_doc.clone())?;
  }
  if random.random_bool(0.5) {
    w.force_merge(1)?;
  }

  let reader = Arc::new(directory_reader::open_from_writer(&w)?);
  let no_cache_reader = Arc::new(DummyDirectoryReader::new(reader.clone())?);

  let cache = Arc::new(LRUQueryCache::with_skip_cache_factor(
    1,
    100000,
    f32::INFINITY,
    CacheAllSegments,
  )?);
  let mut searcher = index_searcher::from_reader(reader)?.with_hook(IndexSearcherHook::Asserting(
    Box::new(AssertingIndexSearcher::new(random.random())),
  ));
  set_cache(&mut searcher, cache.clone());
  searcher.set_query_caching_policy(always_cache());

  let mut builder = Builder::new();
  builder
    .add(
      crate::core::search::boost_query::BoostQuery::new(
        TermQuery::new(Term::from_text("field", "foo")),
        3.0,
      )?,
      Occur::Should,
    )?
    .add(
      crate::core::search::boost_query::BoostQuery::new(
        TermQuery::new(Term::from_text("field", "bar")),
        3.0,
      )?,
      Occur::Should,
    )?
    .add(
      crate::core::search::boost_query::BoostQuery::new(
        TermQuery::new(Term::from_text("field", "baz")),
        3.0,
      )?,
      Occur::Should,
    )?;
  let query: Query = ConstantScoreQuery::new(builder.build()).into();

  searcher.search(query.clone(), 1)?;

  let mut no_cache_helper_searcher = index_searcher::from_reader(no_cache_reader.clone())?
    .with_hook(IndexSearcherHook::Asserting(Box::new(
      AssertingIndexSearcher::new(random.random()),
    )));
  set_cache(&mut no_cache_helper_searcher, cache);
  no_cache_helper_searcher.set_query_caching_policy(always_cache());
  no_cache_helper_searcher.search(query, 1)?;

  let t = std::thread::spawn(move || -> Result<()> {
    no_cache_reader.close()?;
    w.close()?;
    dir.close()
  });
  match t.join() {
    Ok(result) => result,
    Err(payload) => std::panic::resume_unwind(payload),
  }
}

#[test]
fn test_skip_caching_for_range_query() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, dir.clone())?;
  let mut doc1 = Document::new();
  doc1.add(StringField::from_string("name", "tom", Store::Yes)?);
  doc1.add(LongPoint::new("age", [15])?);
  doc1.add(SortedNumericDocValuesField::new("age", 15));
  let mut doc2 = Document::new();
  doc2.add(StringField::from_string("name", "alice", Store::Yes)?);
  doc2.add(LongPoint::new("age", [20])?);
  doc2.add(SortedNumericDocValuesField::new("age", 20));
  w.add_documents(&mut random, vec![doc1, doc2])?;
  let reader = w.get_reader(&mut random)?;
  let mut searcher = new_searcher_with_reader(reader)?;
  searcher.set_query_caching_policy(always_cache());
  w.close(&mut random)?;

  // lead cost is 1, cost of subQuery1 is 1, cost of subQuery2 is 2
  let mut bq = Builder::new();
  let sub_query1: Query = TermQuery::new(Term::from_text("name", "tom")).into();
  let sub_query2: Query = IndexOrDocValuesQuery::new(
    LongPoint::new_range_query("age", 10, 30)?,
    SortedNumericDocValuesField::new_slow_range_query("age", 10, 30),
  )
  .into();
  bq.add(sub_query1.clone(), Occur::Filter)?;
  bq.add(sub_query2.clone(), Occur::Filter)?;
  let query = bq.build();
  #[allow(clippy::mutable_key_type)]
  let mut cache_set = HashSet::new();

  // only term query is cached
  let part_cache = Arc::new(LRUQueryCache::with_skip_cache_factor(
    1000000,
    10000000,
    1.0,
    CacheAllSegments,
  )?);
  set_cache(&mut searcher, part_cache.clone());
  searcher.search(query.clone(), 1)?;
  cache_set.insert(sub_query1.clone());
  assert_eq!(cache_set, cached_query_set(&part_cache));

  // both queries are cached
  let all_cache = Arc::new(LRUQueryCache::with_skip_cache_factor(
    1000000,
    10000000,
    f32::INFINITY,
    CacheAllSegments,
  )?);
  set_cache(&mut searcher, all_cache.clone());
  searcher.search(query, 1)?;
  cache_set.insert(sub_query2);
  assert_eq!(cache_set, cached_query_set(&all_cache));

  Ok(())
}

#[test]
fn test_count_delegation() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, dir.clone())?;
  for _ in 0..20 {
    w.add_document(&mut random, string_doc("foo", "bar", Store::No)?)?;
  }
  let reader = w.get_reader(&mut random)?;
  let mut searcher = new_searcher_with_reader(reader)?;
  searcher.set_query_caching_policy(always_cache());

  let q: Query = TermQuery::new(Term::from_text("foo", "bar")).into();
  searcher.count(q.clone())?; // add to cache

  let weight = searcher.create_weight(searcher.rewrite(q)?, ScoreMode::CompleteNoScores, 1.0)?;
  assert_ne!(
    -1,
    weight.count(&searcher.get_leaf_contexts()?[0], &searcher)?
  );

  w.close(&mut random)
}

#[test]
fn test_skip_caching_for_term_query() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, dir.clone())?;
  let mut docs = Vec::new();
  for (name, hobby) in [("tom", "movie"), ("alice", "book"), ("alice", "movie")] {
    let mut doc = Document::new();
    doc.add(StringField::from_string("name", name, Store::Yes)?);
    doc.add(StringField::from_string("hobby", hobby, Store::Yes)?);
    docs.push(doc);
  }
  w.add_documents(&mut random, docs)?;
  let reader = w.get_reader(&mut random)?;
  let mut searcher = new_searcher_with_reader(reader)?;
  struct NonTermPolicy;
  impl QueryCachingPolicy for NonTermPolicy {
    fn on_use(&self, _query: &Query) {}

    fn should_cache(&self, query: &Query) -> Result<bool> {
      Ok(!matches!(query, Query::Term(_)))
    }
  }
  searcher.set_query_caching_policy(Arc::new(QueryCachingPolicyEnum::custom(NonTermPolicy)));
  w.close(&mut random)?;

  // lead cost is 2, cost of subQuery1 is 3, cost of subQuery2 is 2
  let mut inner = Builder::new();
  let inner_sub_query1: Query = TermQuery::new(Term::from_text("hobby", "book")).into();
  let inner_sub_query2: Query = TermQuery::new(Term::from_text("hobby", "movie")).into();
  inner.add(inner_sub_query1, Occur::Should)?;
  inner.add(inner_sub_query2, Occur::Should)?;
  let sub_query1 = inner.build();

  let mut bq = Builder::new();
  let sub_query2: Query = TermQuery::new(Term::from_text("name", "alice")).into();
  bq.add(ConstantScoreQuery::new(sub_query1.clone()), Occur::Filter)?;
  bq.add(sub_query2, Occur::Filter)?;
  let query = bq.build();
  #[allow(clippy::mutable_key_type)]
  let mut cache_set = HashSet::new();

  // both queries are not cached
  let part_cache = Arc::new(LRUQueryCache::with_skip_cache_factor(
    1000000,
    10000000,
    1.0,
    CacheAllSegments,
  )?);
  set_cache(&mut searcher, part_cache.clone());
  searcher.search(query.clone(), 1)?;
  assert_eq!(cache_set, cached_query_set(&part_cache));

  // only subQuery1 is cached
  let all_cache = Arc::new(LRUQueryCache::with_skip_cache_factor(
    1000000,
    10000000,
    f32::INFINITY,
    CacheAllSegments,
  )?);
  set_cache(&mut searcher, all_cache.clone());
  searcher.search(query, 1)?;
  cache_set.insert(sub_query1.into());
  assert_eq!(cache_set, cached_query_set(&all_cache));

  Ok(())
}

#[test]
fn test_cache_has_fast_count() -> Result<()> {
  let mut builder = phrase_query::Builder::new();
  builder.add_term(Term::from_text("words", "alice"))?;
  builder.add_term(Term::from_text("words", "ran"))?;
  let query: Query = builder.build()?.into();

  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_merge_policy(crate::core::index::no_merge_policy::NoMergePolicy::default());
  let w = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);
  let mut doc1 = Document::new();
  doc1.add(TextField::from_string("words", "tom ran", Store::No)?);
  let mut doc2 = Document::new();
  doc2.add(TextField::from_string("words", "alice ran", Store::No)?);
  doc2.add(StringField::from_string("f", "a", Store::No)?);
  let mut doc3 = Document::new();
  doc3.add(TextField::from_string("words", "alice ran", Store::No)?);
  doc3.add(StringField::from_string("f", "b", Store::No)?);
  w.add_documents(&mut random, vec![doc1, doc2, doc3])?;

  {
    let reader = w.get_reader(&mut random)?;
    let mut searcher = new_searcher_with_reader(reader)?;
    searcher.set_query_caching_policy(always_cache());
    let all_cache = Arc::new(LRUQueryCache::with_skip_cache_factor(
      1000000,
      10000000,
      f32::INFINITY,
      CacheAllSegments,
    )?);
    set_cache(&mut searcher, all_cache.clone());
    let weight = searcher.create_weight(query.clone(), ScoreMode::CompleteNoScores, 1.0)?;
    assert_eq!(1, searcher.get_leaf_contexts()?.len());
    let context = &searcher.get_leaf_contexts()?[0];
    // We don't have a fast count before the cache is filled
    assert_eq!(-1, weight.count(context, &searcher)?);
    // Fetch the scorer to populate the cache
    weight.scorer(context, &searcher)?;
    assert_eq!(vec![query.clone()], cached_queries(&all_cache));
    // Now we *do* have a fast count
    assert_eq!(2, weight.count(context, &searcher)?);
  }

  w.delete_documents_with_queries(
    &mut random,
    vec![TermQuery::new(Term::from_text("f", "b")).into()],
  )?;
  {
    let reader = w.get_reader(&mut random)?;
    let mut searcher = new_searcher_with_reader(reader)?;
    searcher.set_query_caching_policy(always_cache());
    let all_cache = Arc::new(LRUQueryCache::with_skip_cache_factor(
      1000000,
      10000000,
      f32::INFINITY,
      CacheAllSegments,
    )?);
    set_cache(&mut searcher, all_cache.clone());
    let weight = searcher.create_weight(query.clone(), ScoreMode::CompleteNoScores, 1.0)?;
    assert_eq!(1, searcher.get_leaf_contexts()?.len());
    let context = &searcher.get_leaf_contexts()?[0];
    // We don't have a fast count before the cache is filled
    assert_eq!(-1, weight.count(context, &searcher)?);
    // Fetch the scorer to populate the cache
    weight.scorer(context, &searcher)?;
    assert_eq!(vec![query], cached_queries(&all_cache));
    // We still don't have a fast count because we have deleted documents
    assert_eq!(-1, weight.count(context, &searcher)?);
  }

  w.close(&mut random)
}

struct WeightWrapper<IRC> {
  in_: Arc<QueryWeight<IRC>>,
  scorer_called: Arc<AtomicBool>,
  bulk_scorer_called: Arc<AtomicBool>,
}

impl<IRC> WeightWrapper<IRC>
where
  IRC: IndexReaderContext,
{
  fn new(
    in_: QueryWeight<IRC>,
    scorer_called: Arc<AtomicBool>,
    bulk_scorer_called: Arc<AtomicBool>,
  ) -> Self {
    Self {
      in_: Arc::new(in_),
      scorer_called,
      bulk_scorer_called,
    }
  }
}

impl<IRC> SegmentCacheable<IRC> for WeightWrapper<IRC>
where
  IRC: IndexReaderContext,
{
  fn is_cacheable(&self, ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    self.in_.is_cacheable(ctx)
  }
}

impl<IRC> Weight<IRC> for WeightWrapper<IRC>
where
  IRC: IndexReaderContext,
{
  fn matches<'a>(
    &'a self,
    context: &'a LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &'a IndexSearcher<IRC>,
  ) -> Result<Option<crate::core::search::query::QueryWeightMatches<'a>>> {
    self.in_.matches(context, doc, searcher)
  }

  fn explain(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Explanation> {
    self.in_.explain(context, doc, searcher)
  }

  fn get_query(&self) -> Arc<Query> {
    self.in_.get_query()
  }

  type ScorerSupplier = QueryWeightSs<IRC>;

  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::ScorerSupplier>> {
    let Some(mut scorer_supplier) = self.in_.scorer_supplier(context, searcher)? else {
      return Ok(None);
    };
    let scorer = scorer_supplier.get(i64::MAX, context, searcher)?;
    let cost = scorer.iterator().cost()?;
    Ok(Some(Box::new(WeightWrapperScorerSupplier {
      in_: self.in_.clone(),
      scorer: Some(scorer),
      cost,
      scorer_called: self.scorer_called.clone(),
      bulk_scorer_called: self.bulk_scorer_called.clone(),
    })))
  }
}

struct WeightWrapperScorerSupplier<IRC> {
  in_: Arc<QueryWeight<IRC>>,
  scorer: Option<QueryWeightSsScorer>,
  cost: i64,
  scorer_called: Arc<AtomicBool>,
  bulk_scorer_called: Arc<AtomicBool>,
}

impl<IRC> ScorerSupplier<IRC> for WeightWrapperScorerSupplier<IRC>
where
  IRC: IndexReaderContext,
{
  type Scorer = QueryWeightSsScorer;
  type BulkScorer = QueryWeightSsBulkScorer;

  fn get(
    &mut self,
    _lead_cost: i64,
    _context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC>,
  ) -> Result<Self::Scorer> {
    self.scorer_called.store(true, Ordering::SeqCst);
    self
      .scorer
      .take()
      .ok_or_else(|| LuceneError::illegal_state("scorer has already been consumed"))
  }

  fn bulk_scorer(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::BulkScorer>> {
    self.bulk_scorer_called.store(true, Ordering::SeqCst);
    self.in_.bulk_scorer(context, searcher)
  }

  fn cost(
    &mut self,
    _context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC>,
  ) -> Result<i64> {
    Ok(self.cost)
  }
}
