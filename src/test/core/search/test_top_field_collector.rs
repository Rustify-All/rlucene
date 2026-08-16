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
use crate::core::index::index_reader::{IndexReader, IndexReaderContextType};
use crate::test_framework::core::util::lucene_test_case::{
  at_least, at_least_usize, new_directory_shared, new_searcher_with_reader,
  new_searcher_with_threads, random,
};
use std::fmt::{Display, Formatter};

use crate::core::document::numeric_doc_values_field::NumericDocValuesField;

use crate::core::index::directory_reader;
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::{DISABLE_AUTO_FLUSH, IndexWriterConfig};
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::search::sort::Sort;

use crate::core::search::collector::Collector;
use crate::core::search::collector_manager::CollectorManager;

use crate::core::search::dummy::dummy_weight::DummyWeight;
use crate::core::search::field_doc::FieldDoc;
use crate::core::search::leaf_collector::LeafCollector;
use crate::core::search::match_all_docs_query::MatchAllDocsQuery;
use crate::core::search::max_score_accumulator::{DEFAULT_INTERVAL, MaxScoreAccumulator};
use crate::core::search::scorable::{ChildScorable, FixedScore, Scorable};
use crate::core::search::score_doc::{ScoreDoc, ScoreDocLike};
use crate::core::search::sort_field::{SortField, SortFieldType};

use crate::core::document::field::{FieldBase, Store};
use crate::core::document::text_field::TextField;
use crate::core::index::term::Term;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::term_query::TermQuery;
use crate::core::search::top_docs::TopDocsLike;
use crate::core::search::top_docs_collector::TopDocsCollector;
use crate::core::search::top_field_collector_manager::TopFieldCollectorManager;
use crate::core::search::total_hits::Relation::{EqualTo, GreaterThanOrEqualTo};
use crate::core::search::total_hits::TotalHits;

use crate::core::document::string_field::StringField;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::boolean_query::Builder;
use crate::core::search::boost_query::BoostQuery;
use crate::core::search::filter_scorable::FilterScorable;

use crate::core::search::query::Query;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::top_field_collector::{
  FieldLeafCollectorEnum, TopFieldCollectorEnum, populate_scores,
};
use crate::core::search::top_field_docs::TopFieldDocs;
use crate::core::search::weight::Weight;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::search::check_hits::CheckHits;
use crate::test_framework::core::util::DefaultIndexSearchCR;
use crate::test_framework::core::util::test_util::TestUtil;
use rand::RngExt;
use rand_chacha::rand_core::Rng;
use std::sync::Arc;

#[allow(dead_code)] // for quick search
struct TestTopFieldCollector;

fn setup() -> Result<DefaultIndexSearchCR> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let iw = RandomIndexWriter::new(&mut random, dir)?;
  let num_docs = at_least(&mut random, 100);
  for _ in 0..num_docs {
    let doc = Document::new();
    iw.add_document(&mut random, doc)?;
  }
  let ir = iw.get_reader(&mut random)?;
  iw.close(&mut random)?;
  let is = new_searcher_with_threads(&mut random, ir, true, true, false)?;
  Ok(is)
}
fn do_search_with_threshold<IR>(
  num_results: usize,
  threshold: usize,
  q: Query,
  sort: Sort,
  index_reader: IR,
) -> Result<TopFieldDocs>
where
  IR: IndexReader + 'static + std::marker::Sync,
  IndexReaderContextType<IR>: 'static + std::marker::Sync,
  IRCLeafReader<IndexReaderContextType<IR>>: std::marker::Sync + Send,
{
  let searcher = new_searcher_with_reader(index_reader)?;

  let manager = TopFieldCollectorManager::with_after(sort, num_results, None, threshold)?;

  searcher.search_with_collector_manager(q, &manager)
}
fn do_concurrent_search_with_threshold<R, IR>(
  random: &mut R,
  num_results: usize,
  threshold: usize,
  q: Query,
  sort: Sort,
  index_reader: IR,
) -> Result<TopFieldDocs>
where
  IR: IndexReader + 'static + std::marker::Sync,
  IndexReaderContextType<IR>: 'static + std::marker::Sync,
  R: Rng + ?Sized,
  IRCLeafReader<IndexReaderContextType<IR>>: std::marker::Sync + Send,
{
  let searcher = new_searcher_with_threads(random, index_reader, true, true, true)?;

  let collector_manager = TopFieldCollectorManager::with_after(sort, num_results, None, threshold)?;

  let top_doc = searcher.search_with_collector_manager(q, &collector_manager)?;

  Ok(top_doc)
}
#[test]
fn test_sort_without_fill_fields() -> Result<()> {
  let is = setup()?;
  let sorts = vec![
    Sort::with_fields(vec![SortField::get_field_doc()?])?,
    Sort::new()?,
  ];
  for sort in sorts {
    let query = MatchAllDocsQuery::new();
    let collector_manager = TopFieldCollectorManager::new(sort, 10, i32::MAX as usize)?;

    let top_docs = is.search_with_collector_manager(query, &collector_manager)?;
    let sd = top_docs.score_docs();

    for j in 1..sd.len() {
      assert_ne!(sd[j].doc(), sd[j - 1].doc());
    }
  }

  Ok(())
}
#[test]
fn test_sort() -> Result<()> {
  let is = setup()?;
  // Two Sort criteria to instantiate the multi/single comparators.
  let sorts = [
    Sort::with_fields(vec![SortField::get_field_doc()?])?,
    Sort::new()?,
  ];

  for sort in sorts {
    let query = MatchAllDocsQuery::new();
    let tdc = TopFieldCollectorManager::with_after(sort, 10, None, i32::MAX as usize)?;
    let top_docs = is.search_with_collector_manager(query, &tdc)?;
    let sd = top_docs.score_docs();

    for doc in sd {
      assert!(
        doc.score().is_nan(),
        "expected NaN score but got {}",
        doc.score()
      );
    }
  }

  Ok(())
}
#[test]
fn test_shared_hitcount_collector() -> Result<()> {
  // 对应 newSearcher(ir, true, true, true)
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let iw = RandomIndexWriter::new(&mut random, dir)?;
  let num_docs = at_least(&mut random, 100);
  for _ in 0..num_docs {
    let doc = Document::new();
    iw.add_document(&mut random, doc)?;
  }
  let ir = Arc::new(iw.get_reader(&mut random)?);
  iw.close(&mut random)?;

  let concurrent_searcher = new_searcher_with_threads(&mut random, ir.clone(), true, true, true)?;
  let single_threaded_searcher =
    new_searcher_with_threads(&mut random, ir.clone(), true, true, false)?;

  // Two Sort criteria to instantiate the multi/single comparators.
  let sorts = [
    Arc::new(Sort::with_fields(vec![SortField::get_field_doc()?])?),
    Arc::new(Sort::new()?),
  ];

  for sort in sorts {
    let tdc = TopFieldCollectorManager::new(sort.clone(), 10, i32::MAX as usize)?;
    let td =
      single_threaded_searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &tdc)?;

    let tsdc = TopFieldCollectorManager::new(sort, 10, i32::MAX as usize)?;
    let td2 = concurrent_searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &tsdc)?;

    let sd = td.score_docs();
    for v in sd {
      assert!(
        v.score().is_nan(),
        "expected NaN score but got {}",
        v.score()
      );
    }

    CheckHits::check_equal(
      &MatchAllDocsQuery::new().into(),
      td.score_docs(),
      td2.score_docs(),
    )?;
  }

  Ok(())
}
#[test]
fn test_sort_without_total_hit_tracking() -> Result<()> {
  let is = setup()?;
  let sort = Arc::new(Sort::with_fields(vec![SortField::get_field_doc()?])?);

  for i in 0..2 {
    let query = MatchAllDocsQuery::new();

    // check that setting trackTotalHits to false does not return an error
    // because the index is not sorted
    let manager = if i % 2 == 0 {
      TopFieldCollectorManager::new(sort.clone(), 10, 1)?
    } else {
      let field_doc = FieldDoc::with_fields(1, f32::NAN, vec![1.into()]);
      TopFieldCollectorManager::with_after(sort.clone(), 10, Some(field_doc), 1)?
    };

    let top_docs = is.search_with_collector_manager(query, &manager)?;
    let sd = top_docs.score_docs();

    for v in sd {
      assert!(v.score().is_nan());
    }
  }

  Ok(())
}

fn document() -> Document {
  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("foo", 3));
  doc
}
#[test]
fn test_total_hits() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let sort = Arc::new(Sort::with_fields(vec![SortField::new(
    Some("foo"),
    SortFieldType::Long,
  )?])?);
  let mut config = IndexWriterConfig::new()?;
  config
    .set_merge_policy(NoMergePolicy::default())
    .set_index_sort(sort.clone())?
    .set_max_buffered_docs(7)
    .set_ram_buffer_size_mb(DISABLE_AUTO_FLUSH as f64);

  let writer = IndexWriter::new(dir.clone(), config)?;
  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("foo", 3));
  for _ in 0..4 {
    writer.add_document(document())?;
  }
  writer.flush()?;
  for _ in 0..6 {
    writer.add_document(document())?;
  }
  writer.flush()?;

  let reader = directory_reader::open_from_writer(&writer)?;
  let searcher = IndexSearcher::new(reader.get_context()?)?;
  let reader = &searcher.reader_context;
  assert_eq!(2, reader.leaves()?.len());
  writer.close()?;

  let dummy_weight = DummyWeight::<_>::new(reader.leaves()?[0].reader().clone());
  for total_hits_threshold in 0..20 {
    let after_variants: [Option<FieldDoc>; 2] = [
      None,
      Some(FieldDoc::with_fields(4, f32::NAN, vec![2_i64.into()])),
    ];
    for after in after_variants {
      let manager =
        TopFieldCollectorManager::with_after(sort.clone(), 2, after.clone(), total_hits_threshold)?;
      let mut collector = manager.new_collector()?;
      let mut scorer = Score::default();

      let leaves = reader.leaves()?;
      let mut leaf_collector1 =
        collector.get_leaf_collector(&leaves[0], Some(&dummy_weight), &searcher)?;
      leaf_collector1.set_scorer(&mut scorer)?;

      scorer.score = 3.0;
      leaf_collector1.collect(0, &mut scorer)?;
      scorer.score = 3.0;
      leaf_collector1.collect(1, &mut scorer)?;

      let mut leaf_collector2 =
        collector.get_leaf_collector(&leaves[1], Some(&dummy_weight), &searcher)?;
      leaf_collector2.set_scorer(&mut scorer)?;

      scorer.score = 3.0;
      if total_hits_threshold < 3 {
        let result = leaf_collector2.collect(1, &mut scorer);
        assert!(matches!(result, Err(LuceneError::CollectionTerminated(_))));

        let top_docs = collector.top_docs()?;
        assert_eq!(
          *top_docs.total_hits(),
          TotalHits::new(3, GreaterThanOrEqualTo)
        );
        continue;
      } else {
        leaf_collector2.collect(1, &mut scorer)?;
      }

      scorer.score = 4.0;
      if total_hits_threshold == 3 {
        let result = leaf_collector2.collect(1, &mut scorer);
        assert!(matches!(result, Err(LuceneError::CollectionTerminated(_))));

        let top_docs = collector.top_docs()?;
        assert_eq!(
          *top_docs.total_hits(),
          TotalHits::new(4, GreaterThanOrEqualTo)
        );
        continue;
      } else {
        leaf_collector2.collect(1, &mut scorer)?;
      }

      let top_docs = collector.top_docs()?;
      assert_eq!(*top_docs.total_hits(), TotalHits::new(4, EqualTo));
    }
  }
  Ok(())
}
#[test]
fn test_set_min_competitive_score() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mut config = IndexWriterConfig::new()?;
  config.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(dir.clone(), config)?;

  for _ in 0..4 {
    writer.add_document(Document::new())?;
  }
  writer.flush()?;
  for _ in 0..2 {
    writer.add_document(Document::new())?;
  }
  writer.flush()?;

  let reader = directory_reader::open_from_writer(&writer)?;
  let searcher = IndexSearcher::new(reader.get_context()?)?;
  let reader = &searcher.reader_context;
  assert_eq!(2, reader.leaves()?.len());
  writer.close()?;
  let dummy_weight = DummyWeight::<_>::new(reader.leaves()?[0].reader().clone());

  let sort = Sort::with_fields(vec![
    SortField::get_field_score()?,
    SortField::new(Some("foo"), SortFieldType::Long)?,
  ])?;

  let mut collector = TopFieldCollectorManager::new(sort, 2, 2)?.new_collector()?;
  let mut scorer = Score::default();

  let leaves = reader.leaves()?;
  let mut leaf_collector =
    collector.get_leaf_collector(&leaves[0], Some(&dummy_weight), &searcher)?;
  leaf_collector.set_scorer(&mut scorer)?;
  assert!(scorer.min_competitive_score.is_none());

  scorer.score = 1.0;
  leaf_collector.collect(0, &mut scorer)?;
  assert!(scorer.min_competitive_score.is_none());

  scorer.score = 2.0;
  leaf_collector.collect(1, &mut scorer)?;
  assert!(scorer.min_competitive_score.is_none());

  scorer.score = 3.0;
  leaf_collector.collect(2, &mut scorer)?;
  assert_eq!(*scorer.min_competitive_score.as_ref().unwrap(), 2.0);

  scorer.score = 0.5;
  scorer.min_competitive_score = Some(f32::NAN);
  leaf_collector.collect(3, &mut scorer)?;
  assert!(scorer.min_competitive_score.as_ref().unwrap().is_nan());

  scorer.score = 4.0;
  leaf_collector.collect(4, &mut scorer)?;
  assert_eq!(*scorer.min_competitive_score.as_ref().unwrap(), 3.0);

  // Make sure the min score is set on scorers on new segments
  let mut scorer = Score::default();
  let mut leaf_collector =
    collector.get_leaf_collector(&leaves[1], Some(&dummy_weight), &searcher)?;
  leaf_collector.set_scorer(&mut scorer)?;
  assert_eq!(*scorer.min_competitive_score.as_ref().unwrap(), 3.0);

  scorer.score = 1.0;
  leaf_collector.collect(0, &mut scorer)?;
  assert_eq!(*scorer.min_competitive_score.as_ref().unwrap(), 3.0);

  scorer.score = 4.0;
  leaf_collector.collect(1, &mut scorer)?;
  assert_eq!(*scorer.min_competitive_score.as_ref().unwrap(), 4.0);

  Ok(())
}
#[test]
fn test_total_hits_with_score() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mut config = IndexWriterConfig::new()?;
  config.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(dir.clone(), config)?;

  for _ in 0..4 {
    writer.add_document(Document::new())?;
  }
  writer.flush()?;
  for _ in 0..6 {
    writer.add_document(Document::new())?;
  }
  writer.flush()?;

  let reader = directory_reader::open_from_writer(&writer)?;
  let searcher = IndexSearcher::new(reader.get_context()?)?;
  let reader = &searcher.reader_context;
  assert_eq!(2, reader.leaves()?.len());
  writer.close()?;
  let dummy_weight = DummyWeight::<_>::new(reader.leaves()?[0].reader().clone());
  for total_hits_threshold in 0..20 {
    let sort = Sort::with_fields(vec![
      SortField::get_field_score()?,
      SortField::new(Some("foo"), SortFieldType::Long)?,
    ])?;

    let mut collector =
      TopFieldCollectorManager::new(sort, 2, total_hits_threshold)?.new_collector()?;
    let mut scorer = Score::default();

    // segment 0
    let leaves = reader.leaves()?;
    let mut lc0 = collector.get_leaf_collector(&leaves[0], Some(&dummy_weight), &searcher)?;
    lc0.set_scorer(&mut scorer)?;

    scorer.score = 3.0;
    lc0.collect(0, &mut scorer)?;
    scorer.score = 3.0;
    lc0.collect(1, &mut scorer)?;

    let mut lc1 = collector.get_leaf_collector(&leaves[1], Some(&dummy_weight), &searcher)?;
    lc1.set_scorer(&mut scorer)?;

    scorer.score = 3.0;
    lc1.collect(1, &mut scorer)?;
    scorer.score = 4.0;
    lc1.collect(1, &mut scorer)?;

    let top_docs = collector.top_docs()?;

    assert_eq!(
      total_hits_threshold < 4,
      scorer.min_competitive_score.is_some()
    );

    let expected = if total_hits_threshold < 4 {
      TotalHits::new(4, GreaterThanOrEqualTo)
    } else {
      TotalHits::new(4, EqualTo)
    };
    assert_eq!(*top_docs.total_hits(), expected);
  }

  Ok(())
}
#[test]
fn test_sort_no_results() -> Result<()> {
  // Two Sort criteria to instantiate the multi/single comparators.
  let sorts = [
    Sort::with_fields(vec![SortField::get_field_doc()?])?,
    Sort::new()?,
  ];

  for sort in sorts {
    let mut collector =
      TopFieldCollectorManager::new(sort, 10, i32::MAX as usize)?.new_collector()?;
    let top_docs = collector.top_docs()?;

    assert_eq!(top_docs.total_hits().value(), 0);
  }

  Ok(())
}

#[test]
fn test_compute_scores_only_once() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;

  let mut doc = Document::new();
  let text = StringField::from_string("text", "foo", Store::No)?;
  doc.add(text);
  let relevance = NumericDocValuesField::new("relevance", 1);
  doc.add(relevance);

  writer.add_document(&mut random, doc.clone())?;

  doc.remove_field("text");
  doc.add(StringField::from_string("text", "bar", Store::No)?);
  writer.add_document(&mut random, doc.clone())?;

  doc.remove_field("text");
  doc.add(StringField::from_string("text", "baz", Store::No)?);
  writer.add_document(&mut random, doc.clone())?;

  let reader = writer.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;

  let foo = BoostQuery::new(TermQuery::new(Term::from_text("text", "foo")), 2.0)?;
  let bar = TermQuery::new(Term::from_text("text", "bar"));
  let baz = BoostQuery::new(TermQuery::new(Term::from_text("text", "baz")), 3.0)?;

  let mut builder = Builder::new();
  builder.add(foo, Occur::Should)?;
  builder.add(bar, Occur::Should)?;
  builder.add(baz, Occur::Should)?;
  let query = builder.build();

  let sorts = vec![
    Sort::with_fields(vec![SortField::get_field_score()?])?,
    Sort::with_fields(vec![SortField::new(Some("f"), SortFieldType::Score)?])?,
  ];

  for sort in sorts {
    let top_field_collector_manager = TopFieldCollectorManager::new(
      sort,
      TestUtil::next_usize(&mut random, 1, 2),
      i32::MAX as usize,
    )?;
    let cm = CollectorManagerImpl::new(top_field_collector_manager);
    searcher.search_with_collector_manager(query.clone(), &cm)?;
  }

  writer.close(&mut random)?;
  Ok(())
}

#[test]
fn test_populate_scores() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, dir.clone())?;

  let mut doc = Document::new();
  let mut field = TextField::from_string("f", "foo bar", Store::No)?;
  doc.add(field.clone());
  let mut sort_field = NumericDocValuesField::new("sort", 0);
  doc.add(sort_field.clone());
  w.add_document(&mut random, doc.clone())?;

  field.set_string_value("")?;
  sort_field.set_long_value(3)?;
  let mut doc = Document::new();
  doc.add(field.clone());
  doc.add(sort_field.clone());
  w.add_document(&mut random, doc)?;

  field.set_string_value("foo foo bar")?;
  sort_field.set_long_value(2)?;
  let mut doc = Document::new();
  doc.add(field.clone());
  doc.add(sort_field.clone());
  w.add_document(&mut random, doc)?;

  w.flush()?;

  field.set_string_value("foo")?;
  sort_field.set_long_value(2)?;
  let mut doc = Document::new();
  doc.add(field.clone());
  doc.add(sort_field.clone());
  w.add_document(&mut random, doc)?;

  field.set_string_value("bar bar bar")?;
  sort_field.set_long_value(0)?;
  let mut doc = Document::new();
  doc.add(field);
  doc.add(sort_field);
  w.add_document(&mut random, doc)?;

  let reader = w.get_reader(&mut random)?;
  w.close(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;

  for query_text in ["foo", "bar"] {
    let query = TermQuery::new(Term::from_text("f", query_text));

    for reverse in [false, true] {
      let mut sorted_by_doc = searcher.search(query.clone(), 10)?.score_docs;
      sorted_by_doc.sort_by_key(|sd: &ScoreDoc| sd.doc());

      let sort = Sort::with_fields(vec![SortField::with_reverse(
        Some("sort"),
        SortFieldType::Long,
        reverse,
      )?])?;
      let r = searcher.search_with_sort(query.clone(), 10, sort)?;
      let sorted_by_field = r.score_docs();
      let mut sorted_by_field_clone = sorted_by_field.to_vec();
      populate_scores(sorted_by_field_clone.as_mut(), &searcher, query.clone())?;

      for i in 0..sorted_by_field_clone.len() {
        assert_eq!(sorted_by_field_clone[i].doc(), sorted_by_field[i].doc());

        let _cloned_field_doc = sorted_by_field_clone[i].fields()?;
        let _field_doc = sorted_by_field[i].fields()?;
        // assert!(std::ptr::eq(cloned_field_doc[i], field_doc[i]));

        let pos = sorted_by_doc
          .binary_search_by_key(&sorted_by_field_clone[i].doc(), |sd: &ScoreDoc| sd.doc())
          .unwrap();

        assert_eq!(sorted_by_field_clone[i].score(), sorted_by_doc[pos].score());
      }
    }
  }

  Ok(())
}
#[test]
fn test_concurrent_min_score() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut config = IndexWriterConfig::new()?;
  config.set_merge_policy(NoMergePolicy::default());
  let w = IndexWriter::new(dir.clone(), config)?;
  let doc = Document::new();
  w.add_documents(vec![doc.clone(); 5])?;
  w.flush()?;
  w.add_documents(vec![doc.clone(); 6])?;
  w.flush()?;
  w.add_documents(vec![doc; 2])?;
  w.flush()?;

  let reader = directory_reader::open_from_writer(&w)?;
  let searcher = IndexSearcher::new(reader.get_context()?)?;
  let reader = &searcher.reader_context;
  assert_eq!(3, reader.leaves()?.len());
  w.close()?;

  let sort = Sort::with_fields(vec![
    SortField::get_field_score()?,
    SortField::get_field_doc()?,
  ])?;
  DEFAULT_INTERVAL.store(0, std::sync::atomic::Ordering::Relaxed);
  let manager = TopFieldCollectorManager::new(sort, 2, 0)?;
  let mut collector = manager.new_collector()?;
  let mut collector2 = manager.new_collector()?;

  assert!(Arc::ptr_eq(
    &collector.min_score_acc().unwrap(),
    &collector2.min_score_acc().unwrap()
  ));
  let min_value_checker = collector.min_score_acc().clone().unwrap();
  // force checking every round
  assert_eq!(min_value_checker.mod_interval, 0);

  // simple test scorer that exposes current `score` and tracks minCompetitiveScore
  let mut scorer = Score::default();
  let mut scorer2 = Score::default();

  let leaves = reader.leaves()?;
  let dummy_weight = DummyWeight::<_>::new(reader.leaves()?[0].reader().clone());

  let mut leaf_collector =
    collector.get_leaf_collector(&leaves[0], Some(&dummy_weight), &searcher)?;
  leaf_collector.set_scorer(&mut scorer)?;
  let mut leaf_collector2 =
    collector2.get_leaf_collector(&leaves[1], Some(&dummy_weight), &searcher)?;
  leaf_collector2.set_scorer(&mut scorer2)?;

  scorer.score = 3.0;
  leaf_collector.collect(0, &mut scorer)?;
  assert_eq!(i64::MIN, min_value_checker.get_raw());
  assert!(scorer.min_competitive_score.is_none());

  scorer2.score = 6.0;
  leaf_collector2.collect(0, &mut scorer2)?;
  assert_eq!(i64::MIN, min_value_checker.get_raw());
  assert!(scorer2.min_competitive_score.is_none());

  scorer.score = 2.0;
  leaf_collector.collect(1, &mut scorer)?;
  assert_eq!(i64::MIN, min_value_checker.get_raw());
  assert!(scorer.min_competitive_score.is_none());

  scorer2.score = 9.0;
  leaf_collector2.collect(1, &mut scorer2)?;
  assert_eq!(i64::MIN, min_value_checker.get_raw());
  assert!(scorer2.min_competitive_score.is_none());

  scorer2.score = 7.0;
  leaf_collector2.collect(2, &mut scorer2)?;
  assert!((MaxScoreAccumulator::to_score(min_value_checker.get_raw()) - 7.0).abs() < f32::EPSILON);
  assert!(scorer.min_competitive_score.is_none());
  assert!((scorer2.min_competitive_score.unwrap() - 7.0).abs() < f32::EPSILON);

  scorer2.score = 1.0;
  leaf_collector2.collect(3, &mut scorer2)?;
  assert!((MaxScoreAccumulator::to_score(min_value_checker.get_raw()) - 7.0).abs() < f32::EPSILON);
  assert!(scorer.min_competitive_score.is_none());
  assert!((scorer2.min_competitive_score.unwrap() - 7.0).abs() < f32::EPSILON);

  scorer.score = 10.0;
  leaf_collector.collect(2, &mut scorer)?;
  assert!((MaxScoreAccumulator::to_score(min_value_checker.get_raw()) - 7.0).abs() < f32::EPSILON);
  assert!((scorer.min_competitive_score.unwrap() - 7.0).abs() < f32::EPSILON);
  assert!((scorer2.min_competitive_score.unwrap() - 7.0).abs() < f32::EPSILON);

  scorer.score = 11.0;
  leaf_collector.collect(3, &mut scorer)?;
  assert!((MaxScoreAccumulator::to_score(min_value_checker.get_raw()) - 10.0).abs() < f32::EPSILON);
  assert!((scorer.min_competitive_score.unwrap() - 10.0).abs() < f32::EPSILON);
  assert!((scorer2.min_competitive_score.unwrap() - 7.0).abs() < f32::EPSILON);

  let mut collector3 = manager.new_collector()?;
  let mut leaf_collector3 =
    collector3.get_leaf_collector(&leaves[2], Some(&dummy_weight), &searcher)?;
  let mut scorer3 = Score::default();
  leaf_collector3.set_scorer(&mut scorer3)?;
  assert!((scorer3.min_competitive_score.unwrap() - 10.0).abs() < f32::EPSILON);

  scorer3.score = 1.0;
  leaf_collector3.collect(0, &mut scorer3)?;
  assert!((MaxScoreAccumulator::to_score(min_value_checker.get_raw()) - 10.0).abs() < f32::EPSILON);
  assert!((scorer3.min_competitive_score.unwrap() - 10.0).abs() < f32::EPSILON);

  scorer.score = 11.0;
  leaf_collector.collect(4, &mut scorer)?;
  assert!((MaxScoreAccumulator::to_score(min_value_checker.get_raw()) - 11.0).abs() < f32::EPSILON);
  assert!((scorer.min_competitive_score.unwrap() - 11.0).abs() < f32::EPSILON);
  assert!((scorer2.min_competitive_score.unwrap() - 7.0).abs() < f32::EPSILON);
  assert!((scorer3.min_competitive_score.unwrap() - 10.0).abs() < f32::EPSILON);

  scorer3.score = 2.0;
  leaf_collector3.collect(1, &mut scorer3)?;
  assert!((MaxScoreAccumulator::to_score(min_value_checker.get_raw()) - 11.0).abs() < f32::EPSILON);
  assert!((scorer.min_competitive_score.unwrap() - 11.0).abs() < f32::EPSILON);
  assert!((scorer2.min_competitive_score.unwrap() - 7.0).abs() < f32::EPSILON);
  assert!((scorer3.min_competitive_score.unwrap() - 11.0).abs() < f32::EPSILON);

  let top_docs = manager.reduce(vec![collector, collector2, collector3])?;
  assert_eq!(11, top_docs.total_hits().value());
  assert_eq!(
    TotalHits::new(11, GreaterThanOrEqualTo),
    *top_docs.total_hits()
  );
  Ok(())
}
#[test]
fn test_random_min_competitive_score() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let w = RandomIndexWriter::new(&mut random, dir.clone())?;

  let num_docs = at_least_usize(&mut random, 1000);

  for _ in 0..num_docs {
    let num_as = 1 + random.random_range(0..5);
    let num_bs = if random.random::<f32>() < 0.5 {
      0
    } else {
      1 + random.random_range(0..5)
    };
    let num_cs = if random.random::<f32>() < 0.1 {
      0
    } else {
      1 + random.random_range(0..5)
    };

    let mut doc = Document::new();

    for _ in 0..num_as {
      doc.add(StringField::from_string("f", "A", Store::No)?);
    }
    for _ in 0..num_bs {
      doc.add(StringField::from_string("f", "B", Store::No)?);
    }
    for _ in 0..num_cs {
      doc.add(StringField::from_string("f", "C", Store::No)?);
    }

    w.add_document(&mut random, doc)?;
  }

  let index_reader = Arc::new(w.get_reader(&mut random)?);
  w.close(&mut random)?;

  let mut builder = Builder::new();
  builder.add(TermQuery::new(Term::from_text("f", "A")), Occur::Must)?;
  builder.add(TermQuery::new(Term::from_text("f", "B")), Occur::Should)?;
  let boolean_query: Query = builder.build().into();

  let queries: Vec<Query> = vec![
    TermQuery::new(Term::from_text("f", "A")).into(),
    TermQuery::new(Term::from_text("f", "B")).into(),
    TermQuery::new(Term::from_text("f", "C")).into(),
    boolean_query,
  ];

  let sort = Sort::with_fields(vec![
    SortField::get_field_score()?,
    SortField::get_field_doc()?,
  ])?;

  for query in queries {
    let tdc = do_concurrent_search_with_threshold(
      &mut random,
      5,
      0,
      query.clone(),
      sort.clone(),
      index_reader.clone(),
    )?;
    let tdc2 = do_search_with_threshold(5, 0, query.clone(), sort.clone(), index_reader.clone())?;

    assert!(tdc.total_hits().value() > 0);
    assert!(tdc2.total_hits().value() > 0);

    CheckHits::check_equal(&query, tdc.score_docs(), tdc2.score_docs())?;
  }
  Ok(())
}
#[test]
fn test_relation_vs_top_docs_count() -> Result<()> {
  let mut random = random();
  let sort = Arc::new(Sort::with_fields(vec![
    SortField::get_field_score()?,
    SortField::get_field_doc()?,
  ])?);

  let dir = new_directory_shared(&mut random)?;
  let mut config = IndexWriterConfig::new()?;
  config.set_merge_policy(NoMergePolicy::default());
  let writer = IndexWriter::new(dir.clone(), config)?;

  let mut doc = Document::new();
  doc.add(TextField::from_string("f", "foo bar", Store::No)?);

  writer.add_documents(vec![doc.clone(); 5])?;
  writer.flush()?;
  writer.add_documents(vec![doc.clone(); 5])?;
  writer.flush()?;

  let reader = writer.get_reader(false, false)?;
  let searcher = IndexSearcher::new(reader.get_context()?)?;

  let manager = TopFieldCollectorManager::with_after(sort.clone(), 2, None, 10)?;
  let top_docs = searcher
    .search_with_collector_manager(TermQuery::new(Term::from_text("f", "foo")), &manager)?;
  assert_eq!(10, top_docs.total_hits().value());
  assert_eq!(EqualTo, top_docs.total_hits().relation());

  let manager = TopFieldCollectorManager::with_after(sort.clone(), 2, None, 2)?;
  let top_docs = searcher
    .search_with_collector_manager(TermQuery::new(Term::from_text("f", "foo")), &manager)?;
  assert!(10 >= top_docs.total_hits().value());
  assert_eq!(GreaterThanOrEqualTo, top_docs.total_hits().relation());

  let manager = TopFieldCollectorManager::with_after(sort.clone(), 10, None, 2)?;
  let top_docs = searcher
    .search_with_collector_manager(TermQuery::new(Term::from_text("f", "foo")), &manager)?;
  assert_eq!(10, top_docs.total_hits().value());
  assert_eq!(EqualTo, top_docs.total_hits().relation());
  writer.close()?;
  Ok(())
}

#[derive(Default)]
struct Score {
  score: f32,
  min_competitive_score: Option<f32>,
}
impl Scorable for Score {
  fn score(&mut self) -> Result<f32> {
    Ok(self.score)
  }

  fn set_min_competitive_score(&mut self, min_score: f32) -> Result<()> {
    self.min_competitive_score = Some(min_score);
    Ok(())
  }

  fn cost(&self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }
}

impl FixedScore for Score {}

struct LeafCollectorImpl<LC> {
  in_: LC,
  current_doc: i32,
}
impl<LC> LeafCollectorImpl<LC>
where
  LC: LeafCollector,
{
  fn new(in_: LC) -> Self {
    Self {
      in_,
      current_doc: -1,
    }
  }
}

impl<LC> Display for LeafCollectorImpl<LC>
where
  LC: LeafCollector,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl<LC> LeafCollector for LeafCollectorImpl<LC>
where
  LC: LeafCollector,
{
  fn set_scorer(&mut self, scorer: &mut dyn Scorable) -> Result<()> {
    let mut s = FilterScorableImpl::new(scorer, self.current_doc);
    self.in_.set_scorer(&mut s)
  }

  fn collect(&mut self, doc: i32, scorer: &mut dyn Scorable) -> Result<()> {
    self.current_doc = doc;
    let mut s = FilterScorableImpl::new(scorer, self.current_doc);
    self.in_.collect(doc, &mut s)?;
    Ok(())
  }
}

struct CollectorImpl {
  top_collector: TopFieldCollectorEnum,
}
impl CollectorImpl {
  fn new(top_collector: TopFieldCollectorEnum) -> Self {
    Self { top_collector }
  }
}
impl Collector for CollectorImpl {
  type LeafCollector<'a, IRC>
    = LeafCollectorImpl<FieldLeafCollectorEnum<'a, IRCLeafReader<IRC>>>
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
    let in_ = self
      .top_collector
      .get_leaf_collector(context, weight, searcher)?;
    let v = LeafCollectorImpl::new(in_);
    Ok(v)
  }

  fn score_mode(&self) -> ScoreMode {
    ScoreMode::Complete
  }
}

struct CollectorManagerImpl {
  top_field_collector_manager: TopFieldCollectorManager,
}
impl CollectorManagerImpl {
  fn new(top_field_collector_manager: TopFieldCollectorManager) -> Self {
    Self {
      top_field_collector_manager,
    }
  }
}
impl CollectorManager for CollectorManagerImpl {
  type C = CollectorImpl;
  type T = ();

  fn new_collector(&self) -> Result<Self::C> {
    let top_collector = self.top_field_collector_manager.new_collector()?;
    Ok(CollectorImpl::new(top_collector))
  }

  fn reduce(&self, _collectors: Vec<Self::C>) -> Result<Self::T> {
    Ok(())
  }
}
pub struct FilterScorableImpl<'a, S>
where
  S: Scorable + ?Sized,
{
  last_computed_doc: i32,
  base: FilterScorable<'a, S>,
  current_doc: i32,
}
impl<'a, S> FilterScorableImpl<'a, S>
where
  S: Scorable + ?Sized,
{
  pub(crate) fn new(s: &'a mut S, current_doc: i32) -> Self {
    let base = FilterScorable::new(s);
    Self {
      last_computed_doc: -1,
      base,
      current_doc,
    }
  }
}
impl<'a, S> Scorable for FilterScorableImpl<'a, S>
where
  S: Scorable + ?Sized,
{
  fn score(&mut self) -> Result<f32> {
    if self.last_computed_doc == self.current_doc {
      return Err(LuceneError::illegal_state(format!(
        "Score computed twice on {}",
        self.current_doc
      )));
    }
    self.last_computed_doc = self.current_doc;
    self.base.in_.score()
  }

  fn smoothing_score(&mut self, _doc_id: i32) -> Result<f32> {
    self.base.in_.score()
  }

  fn set_min_competitive_score(&mut self, min_score: f32) -> Result<()> {
    self.base.set_min_competitive_score(min_score)
  }

  fn get_children(&self) -> Result<Vec<ChildScorable<Box<dyn Scorable>>>> {
    self.base.get_children()
  }

  fn cost(&self) -> Result<i64> {
    self.base.cost()
  }
}

impl<S> crate::core::search::scorable::FixedScore for FilterScorableImpl<'_, S> where
  S: Scorable + ?Sized
{
}
