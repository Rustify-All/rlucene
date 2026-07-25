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
use crate::core::document::long_point::LongPoint;
use crate::core::document::sorted_numeric_doc_values_field::SortedNumericDocValuesField;
use crate::core::document::string_field::StringField;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, new_directory_shared, new_index_writer_config_with_analyzer, new_searcher_with_reader,
  random,
};

use crate::core::search::boolean_query::Builder;
use crate::core::search::field_exists_query::FieldExistsQuery;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::index_sort_sorted_numeric_doc_values_range_query::IndexSortSortedNumericDocValuesRangeQuery;
use crate::core::search::match_no_docs_query::MatchNoDocsQuery;
use crate::core::search::query::{Query, QueryBase};
use crate::core::search::score_doc::ScoreDocLike;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::{Scorer, TwoPhaseState};
use crate::core::search::sort::Sort;
use crate::core::search::sort_field::{SortFieldType, SortFiledBase};
use crate::core::search::sorted_numeric_sort_field::SortedNumericSortField;
use crate::core::search::top_docs::TopDocsLike;
use crate::core::search::weight::Weight;
use crate::core::store::directory::Directory;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::{HasIdentity, TryIntoInt};
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::search::dummy_total_hit_count_collector::DummyTotalHitCountCollector;
use crate::test_framework::core::search::query_utils::QueryUtils;
use crate::test_framework::core::util::test_util::TestUtil;
use rand::{Rng, RngExt};
use std::sync::Arc;

#[allow(dead_code)] // for quick search
struct TestIndexSortSortedNumericDocValuesRangeQuery;
#[test]
fn test_same_hits_as_point_range_query() -> Result<()> {
  let mut random = random();
  let iters = at_least(&mut random, 10);

  for _iter in 0..iters {
    let dir = new_directory_shared(&mut random)?;
    let mock = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(mock)?;

    let reverse = random.random_bool(0.5);
    let mut sort_field = SortedNumericSortField::with_reverse("dv", SortFieldType::Long, reverse)?;

    let enable_missing_value = random.random_bool(0.5);
    if enable_missing_value {
      let missing_value = if random.random_bool(0.5) {
        TestUtil::next_long(&mut random, -100, 10000)
      } else if random.random_bool(0.5) {
        i64::MIN
      } else {
        i64::MAX
      };
      sort_field.set_missing_value(missing_value)?;
    }

    let sort = Sort::with_fields(vec![sort_field])?;
    iwc.set_index_sort(sort)?;

    let iw = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

    let num_docs = at_least(&mut random, 100);
    for _i in 0..num_docs {
      let mut doc = Document::new();
      let num_values = TestUtil::next_int(&mut random, 0, 1);

      for _ in 0..num_values {
        let value = TestUtil::next_long(&mut random, -100, 10000);
        doc.add(SortedNumericDocValuesField::new("dv", value));
        doc.add(LongPoint::new("idx", vec![value])?);
      }

      iw.add_document(&mut random, doc)?;
    }

    if random.random_bool(0.5) {
      iw.delete_documents_with_queries(
        &mut random,
        vec![LongPoint::new_range_query("idx", 0, 10)?.into()],
      )?;
    }

    let reader = iw.get_reader(&mut random)?;
    let searcher = new_searcher_with_reader(reader)?;
    iw.close(&mut random)?;

    for _i in 0..100 {
      let min = if random.random_bool(0.5) {
        i64::MIN
      } else {
        TestUtil::next_long(&mut random, -100, 10000)
      };
      let max = if random.random_bool(0.5) {
        i64::MAX
      } else {
        TestUtil::next_long(&mut random, -100, 10000)
      };

      let q1 = LongPoint::new_range_query("idx", min, max)?;
      let q2 = create_query("dv", min, max);

      assert_same_hits(&searcher, q1, q2, false)?;
    }
  }

  Ok(())
}

fn assert_same_hits<IRC, T1, T2>(
  searcher: &IndexSearcher<IRC>,
  q1: T1,
  q2: T2,
  scores: bool,
) -> Result<()>
where
  IRC: IndexReaderContext + Sync,
  T1: Into<Query>,
  T2: Into<Query>,
{
  let irc = searcher.get_top_reader_context();
  let max_doc = irc.reader().max_doc()?;

  let sort = if scores {
    Arc::new(Sort::get_relevance()?)
  } else {
    Arc::new(Sort::get_index_order()?)
  };

  let td1 = searcher.search_with_sort(q1, max_doc.try_convert()?, sort.clone())?;
  let td2 = searcher.search_with_sort(q2, max_doc.try_convert()?, sort)?;
  assert_eq!(td1.total_hits().value(), td2.total_hits().value());

  for i in 0..td1.score_docs().len() {
    let sd1 = &td1.score_docs()[i];
    let sd2 = &td2.score_docs()[i];

    assert_eq!(sd1.doc(), sd2.doc());

    if scores {
      let diff = (sd1.score() - sd2.score()).abs();
      assert!(diff <= 1e-6, "score diff={} idx={}", diff, i);
    }
  }

  Ok(())
}
#[test]
fn test_equals() -> Result<()> {
  let q1 = create_query("foo", 3, 5);

  QueryUtils::check_equal(&q1, &create_query("foo", 3, 5));
  QueryUtils::check_unequal(&q1, &create_query("foo", 3, 6));
  QueryUtils::check_unequal(&q1, &create_query("foo", 4, 5));
  QueryUtils::check_unequal(&q1, &create_query("bar", 3, 5));

  Ok(())
}

#[test]
fn test_to_string() -> Result<()> {
  let q1 = create_query("foo", 3, 5);

  assert_eq!("foo:[3 TO 5]", q1.to_string("")?);
  assert_eq!("[3 TO 5]", q1.to_string("foo")?);
  assert_eq!("foo:[3 TO 5]", q1.to_string("bar")?);

  Ok(())
}
#[test]
fn test_index_sort_doc_values_with_even_length() -> Result<()> {
  use SortFieldType::*;

  for ty in [Int, Long] {
    test_index_sort_doc_values_with_even_length_inner(true, ty)?;
    test_index_sort_doc_values_with_even_length_inner(false, ty)?;
  }
  Ok(())
}
fn test_index_sort_doc_values_with_even_length_inner(
  reverse: bool,
  field_type: SortFieldType,
) -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;

  let sort_field = SortedNumericSortField::with_reverse("field", field_type, reverse)?;
  iwc.set_index_sort(Sort::with_fields(vec![sort_field])?)?;

  let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

  // even-length doc list = 6 docs
  writer.add_document(&mut random, create_document("field", -80))?;
  writer.add_document(&mut random, create_document("field", -5))?;
  writer.add_document(&mut random, create_document("field", 0))?;
  writer.add_document(&mut random, create_document("field", 0))?;
  writer.add_document(&mut random, create_document("field", 30))?;
  writer.add_document(&mut random, create_document("field", 35))?;

  let reader = writer.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;

  // Test ranges consisting of one value.

  assert_number_of_hits(&searcher, create_query("field", -80, -80), 1)?;
  assert_number_of_hits(&searcher, create_query("field", -5, -5), 1)?;
  assert_number_of_hits(&searcher, create_query("field", 0, 0), 2)?;
  assert_number_of_hits(&searcher, create_query("field", 30, 30), 1)?;
  assert_number_of_hits(&searcher, create_query("field", 35, 35), 1)?;

  assert_number_of_hits(&searcher, create_query("field", -90, -90), 0)?;
  assert_number_of_hits(&searcher, create_query("field", 5, 5), 0)?;
  assert_number_of_hits(&searcher, create_query("field", 40, 40), 0)?;

  // Test the lower end of the document value range.
  assert_number_of_hits(&searcher, create_query("field", -90, -4), 2)?;
  assert_number_of_hits(&searcher, create_query("field", -80, -4), 2)?;
  assert_number_of_hits(&searcher, create_query("field", -70, -4), 1)?;
  assert_number_of_hits(&searcher, create_query("field", -80, -5), 2)?;

  // Test the upper end of the document value range.
  assert_number_of_hits(&searcher, create_query("field", 25, 34), 1)?;
  assert_number_of_hits(&searcher, create_query("field", 25, 35), 2)?;
  assert_number_of_hits(&searcher, create_query("field", 25, 36), 2)?;
  assert_number_of_hits(&searcher, create_query("field", 30, 35), 2)?;

  // Test multiple occurrences of the same value.
  assert_number_of_hits(&searcher, create_query("field", -4, 4), 2)?;
  assert_number_of_hits(&searcher, create_query("field", -4, 0), 2)?;
  assert_number_of_hits(&searcher, create_query("field", 0, 4), 2)?;
  assert_number_of_hits(&searcher, create_query("field", 0, 30), 3)?;

  // Test ranges that span all documents.
  assert_number_of_hits(&searcher, create_query("field", -80, 35), 6)?;
  assert_number_of_hits(&searcher, create_query("field", -90, 40), 6)?;

  writer.close(&mut random)?;
  Ok(())
}

fn assert_number_of_hits<IRC>(
  searcher: &IndexSearcher<IRC>,
  query: impl Into<Query>,
  number_of_hits: i32,
) -> Result<()>
where
  IRC: IndexReaderContext + Sync,
{
  let query = query.into();

  let manager = DummyTotalHitCountCollector::create_manager();
  let total_hits = searcher.search_with_collector_manager(query.clone(), &manager)?;
  assert_eq!(number_of_hits, total_hits);

  let count = searcher.count(query)?;
  assert_eq!(number_of_hits, count);

  Ok(())
}
#[test]
fn test_index_sort_doc_values_with_odd_length() -> Result<()> {
  test_index_sort_doc_values_with_odd_length_inner(false)?;
  test_index_sort_doc_values_with_odd_length_inner(true)?;
  Ok(())
}
fn test_index_sort_doc_values_with_odd_length_inner(reverse: bool) -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;

  let sort_field = SortedNumericSortField::with_reverse("field", SortFieldType::Long, reverse)?;
  iwc.set_index_sort(Sort::with_fields(vec![sort_field])?)?;

  let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

  writer.add_document(&mut random, create_document("field", -80))?;
  writer.add_document(&mut random, create_document("field", -5))?;
  writer.add_document(&mut random, create_document("field", 0))?;
  writer.add_document(&mut random, create_document("field", 0))?;
  writer.add_document(&mut random, create_document("field", 5))?;
  writer.add_document(&mut random, create_document("field", 30))?;
  writer.add_document(&mut random, create_document("field", 35))?;

  let reader = writer.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;

  // Test ranges consisting of one value.
  assert_number_of_hits(&searcher, create_query("field", -80, -80), 1)?;
  assert_number_of_hits(&searcher, create_query("field", -5, -5), 1)?;
  assert_number_of_hits(&searcher, create_query("field", 0, 0), 2)?;
  assert_number_of_hits(&searcher, create_query("field", 5, 5), 1)?;
  assert_number_of_hits(&searcher, create_query("field", 30, 30), 1)?;
  assert_number_of_hits(&searcher, create_query("field", 35, 35), 1)?;

  assert_number_of_hits(&searcher, create_query("field", -90, -90), 0)?;
  assert_number_of_hits(&searcher, create_query("field", 6, 6), 0)?;
  assert_number_of_hits(&searcher, create_query("field", 40, 40), 0)?;

  // Test the lower end of the document value range.
  assert_number_of_hits(&searcher, create_query("field", -90, -4), 2)?;
  assert_number_of_hits(&searcher, create_query("field", -80, -4), 2)?;
  assert_number_of_hits(&searcher, create_query("field", -70, -4), 1)?;
  assert_number_of_hits(&searcher, create_query("field", -80, -5), 2)?;

  // Test the upper end of the document value range.
  assert_number_of_hits(&searcher, create_query("field", 25, 34), 1)?;
  assert_number_of_hits(&searcher, create_query("field", 25, 35), 2)?;
  assert_number_of_hits(&searcher, create_query("field", 25, 36), 2)?;
  assert_number_of_hits(&searcher, create_query("field", 30, 35), 2)?;

  // Test multiple occurrences of the same value.
  assert_number_of_hits(&searcher, create_query("field", -4, 4), 2)?;
  assert_number_of_hits(&searcher, create_query("field", -4, 0), 2)?;
  assert_number_of_hits(&searcher, create_query("field", 0, 4), 2)?;
  assert_number_of_hits(&searcher, create_query("field", 0, 30), 4)?;

  // Test ranges that span all documents.
  assert_number_of_hits(&searcher, create_query("field", -80, 35), 7)?;
  assert_number_of_hits(&searcher, create_query("field", -90, 40), 7)?;

  writer.close(&mut random)?;
  Ok(())
}

#[test]
fn test_index_sort_doc_values_with_single_value() -> Result<()> {
  test_index_sort_doc_values_with_single_value_inner(false)?;
  test_index_sort_doc_values_with_single_value_inner(true)?;
  Ok(())
}

fn test_index_sort_doc_values_with_single_value_inner(reverse: bool) -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;

  let sort_field = SortedNumericSortField::with_reverse("field", SortFieldType::Long, reverse)?;
  iwc.set_index_sort(Sort::with_fields(vec![sort_field])?)?;

  let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

  writer.add_document(&mut random, create_document("field", 42))?;

  let reader = writer.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;

  assert_number_of_hits(&searcher, create_query("field", 42, 43), 1)?;
  assert_number_of_hits(&searcher, create_query("field", 42, 42), 1)?;
  assert_number_of_hits(&searcher, create_query("field", 41, 41), 0)?;
  assert_number_of_hits(&searcher, create_query("field", 43, 43), 0)?;

  writer.close(&mut random)?;
  Ok(())
}

#[test]
fn test_index_sort_missing_values() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(mock)?;

  let mut sort_field = SortedNumericSortField::new("field", SortFieldType::Long)?;
  let missing_value: i64 = random.random();
  sort_field.set_missing_value(missing_value)?;
  let sort = Sort::with_fields(vec![sort_field])?;
  iwc.set_index_sort(sort)?;

  let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

  writer.add_document(&mut random, create_document("field", -80))?;
  writer.add_document(&mut random, create_document("field", -5))?;
  writer.add_document(&mut random, create_document("field", 0))?;
  writer.add_document(&mut random, create_document("field", 35))?;

  writer.add_document(&mut random, create_document("other-field", 0))?;
  writer.add_document(&mut random, create_document("other-field", 10))?;
  writer.add_document(&mut random, create_document("other-field", 20))?;

  let reader = writer.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;

  assert_number_of_hits(&searcher, create_query("field", -70, 0), 2)?;
  assert_number_of_hits(&searcher, create_query("field", -2, 35), 2)?;

  assert_number_of_hits(&searcher, create_query("field", -80, 35), 4)?;
  assert_number_of_hits(&searcher, create_query("field", i64::MIN, i64::MAX), 4)?;

  writer.close(&mut random)?;
  Ok(())
}

#[test]
fn test_no_documents() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;

  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;
  writer.add_document(&mut random, Document::new())?;

  let reader = writer.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;

  let query = create_query("foo", 2, 4);

  let rewritten = searcher.rewrite(query)?;
  let weight = searcher.create_weight(rewritten, ScoreMode::Complete, 1.0)?;

  let leaves = searcher.get_leaf_contexts()?;
  let ctx0 = &leaves[0];

  let scorer_opt = weight.scorer(ctx0, &searcher)?;
  assert!(scorer_opt.is_none());

  writer.close(&mut random)?;
  Ok(())
}

#[test]
fn test_rewrite_exhaustive_range() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;
  writer.add_document(&mut random, Document::new())?;
  let reader = writer.get_reader(&mut random)?;

  let query = create_query("field", i64::MIN, i64::MAX);
  let searcher = new_searcher_with_reader(reader)?;
  let rewritten_query = query.rewrite(&searcher)?;

  assert_eq!(
    Query::FieldExists(FieldExistsQuery::new("field")),
    rewritten_query
  );

  writer.close(&mut random)?;
  Ok(())
}
#[test]
fn test_rewrite_fallback_query() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;
  writer.add_document(&mut random, Document::new())?;
  let reader = writer.get_reader(&mut random)?;

  let fallback_query = Query::Boolean(Builder::new().build());
  let query = Query::IndexSortSortedNumericDocValuesRange(
    IndexSortSortedNumericDocValuesRangeQuery::new("field", 1, 42, fallback_query.clone()),
  );

  let searcher = new_searcher_with_reader(reader)?;
  let id = query.identity().clone();
  let rewritten_query = query.rewrite(&searcher)?;

  assert_ne!(&id, rewritten_query.identity());
  matches!(
    rewritten_query,
    Query::IndexSortSortedNumericDocValuesRange(_)
  );

  let range_query = match rewritten_query {
    Query::IndexSortSortedNumericDocValuesRange(q) => q,
    _ => unreachable!(),
  };

  matches!(*range_query.fallback_query, Query::MatchNoDocs(_));
  writer.close(&mut random)?;
  Ok(())
}
/// Test that the index sort optimization not activated if there is no index sort.
#[test]
fn test_no_index_sort() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;
  writer.add_document(&mut random, create_document("field", 0))?;
  test_index_sort_optimization_deactivated(&mut random, &writer)?;
  writer.close(&mut random)?;
  Ok(())
}

/// Test that the index sort optimization is not activated when the sort is on the wrong field.
#[test]
fn test_index_sort_on_wrong_field() -> Result<()> {
  let mut random = random();
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(mock)?;
  let dir = new_directory_shared(&mut random)?;
  let sort_field = SortedNumericSortField::new("other-field", SortFieldType::Long)?;
  let sort = Sort::with_fields(vec![sort_field])?;
  iwc.set_index_sort(sort)?;
  let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);
  writer.add_document(&mut random, create_document("field", 0))?;
  test_index_sort_optimization_deactivated(&mut random, &writer)?;
  writer.close(&mut random)?;
  Ok(())
}

#[test]
fn test_other_sort_types() -> Result<()> {
  use SortFieldType::{Double, Float};
  let mut random = random();
  for sort_type in [Float, Double] {
    let dir = new_directory_shared(&mut random)?;
    let mock = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(mock)?;
    let sort_field = SortedNumericSortField::new("field", sort_type)?;
    let sort = Sort::with_fields(vec![sort_field])?;
    iwc.set_index_sort(sort)?;
    let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);
    writer.add_document(&mut random, create_document("field", 0))?;
    test_index_sort_optimization_deactivated(&mut random, &writer)?;
    writer.close(&mut random)?;
  }

  Ok(())
}

/// Test that the index sort optimization is not activated when some documents have multiple values.
#[test]
fn test_multi_doc_values() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(mock)?;
  let sort_field = SortedNumericSortField::new("field", SortFieldType::Long)?;
  let sort = Sort::with_fields(vec![sort_field])?;
  iwc.set_index_sort(sort)?;
  let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);
  let mut doc = Document::new();
  doc.add(SortedNumericDocValuesField::new("field", 0));
  doc.add(SortedNumericDocValuesField::new("field", 10));
  writer.add_document(&mut random, doc)?;

  test_index_sort_optimization_deactivated(&mut random, &writer)?;

  writer.close(&mut random)?;
  Ok(())
}

fn test_index_sort_optimization_deactivated<R, D>(
  random: &mut R,
  writer: &RandomIndexWriter<D>,
) -> Result<()>
where
  R: Rng + ?Sized,
  D: Directory + 'static,
{
  let reader = writer.get_reader(random)?;
  let searcher = new_searcher_with_reader(reader)?;
  let query = create_query("field", 0, 0);
  let weight = query.create_weight(&searcher, &ScoreMode::TopScores, 1.0)?;
  for ctx in searcher.get_leaf_contexts()? {
    let mut scorer = weight.scorer(ctx, &searcher)?;
    assert!(
      scorer.as_mut().unwrap().has_two_phase_iterator() == TwoPhaseState::Yes
        || scorer.as_mut().unwrap().two_phase_iterator().is_some()
    );
  }
  Ok(())
}

#[test]
fn test_fallback_count() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(mock)?;
  let index_sort = Sort::with_fields(vec![SortedNumericSortField::new(
    "field",
    SortFieldType::Long,
  )?])?;
  iwc.set_index_sort(index_sort)?;

  let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);
  let mut doc = Document::new();
  doc.add(SortedNumericDocValuesField::new("field", 10));
  writer.add_document(&mut random, doc)?;
  let reader = writer.get_reader(&mut random)?;

  let searcher = new_searcher_with_reader(reader)?;

  let fallback_query = Query::MatchNoDocs(MatchNoDocsQuery::new());
  let query = Query::IndexSortSortedNumericDocValuesRange(
    IndexSortSortedNumericDocValuesRangeQuery::new("another", 1, 42, fallback_query),
  );

  let weight = query.create_weight(&searcher, &ScoreMode::Complete, 1.0)?;
  for ctx in searcher.get_leaf_contexts()? {
    assert_eq!(0, weight.count(ctx, &searcher)?);
  }

  writer.close(&mut random)?;
  Ok(())
}

#[test]
fn test_compare_count() -> Result<()> {
  let mut random = random();
  let iters = at_least(&mut random, 10);

  for _iter in 0..iters {
    let dir = new_directory_shared(&mut random)?;
    let mock = MockAnalyzer::new(&mut random);
    let mut iwc = IndexWriterConfig::with_analyzer(mock)?;
    let mut sort_field = SortedNumericSortField::with_reverse("field", SortFieldType::Long, false)?;
    let enable_missing_value = random.random_bool(0.5);
    if enable_missing_value {
      let missing_value = if random.random_bool(0.5) {
        TestUtil::next_long(&mut random, -100, 10000)
      } else if random.random_bool(0.5) {
        i64::MIN
      } else {
        i64::MAX
      };
      sort_field.set_missing_value(missing_value)?;
    }

    let sort = Sort::with_fields(vec![sort_field])?;
    iwc.set_index_sort(sort)?;

    let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

    let num_docs = at_least(&mut random, 100);
    for _i in 0..num_docs {
      let mut doc = Document::new();
      let num_values = TestUtil::next_int(&mut random, 0, 1);

      for _ in 0..num_values {
        let value = TestUtil::next_long(&mut random, -100, 10000);
        doc = create_sndv_and_point_document("field", value)?;
      }

      writer.add_document(&mut random, doc)?;
    }

    if random.random_bool(0.5) {
      writer.delete_documents_with_queries(
        &mut random,
        vec![LongPoint::new_range_query("field", 0, 10)?.into()],
      )?;
    }

    // Reader + Searcher
    let reader = writer.get_reader(&mut random)?;
    let searcher = new_searcher_with_reader(reader)?;
    writer.close(&mut random)?;

    for _i in 0..100 {
      let min = if random.random_bool(0.5) {
        i64::MIN
      } else {
        TestUtil::next_long(&mut random, -100, 10000)
      };

      let max = if random.random_bool(0.5) {
        i64::MAX
      } else {
        TestUtil::next_long(&mut random, -100, 10000)
      };

      let q1 = LongPoint::new_range_query("field", min, max)?;

      let fallback = LongPoint::new_range_query("field", min, max)?;
      let q2 = IndexSortSortedNumericDocValuesRangeQuery::new("field", min, max, fallback);

      let w1 = q1.create_weight(&searcher, &ScoreMode::Complete, 1.0)?;
      let w2 = q2.create_weight(&searcher, &ScoreMode::Complete, 1.0)?;

      assert_same_count(w1.as_ref(), w2.as_ref(), &searcher)?;
    }
  }

  Ok(())
}

fn assert_same_count<W1, W2, IRC>(
  weight1: &W1,
  weight2: &W2,
  searcher: &IndexSearcher<IRC>,
) -> Result<()>
where
  W1: Weight<IRC> + ?Sized,
  W2: Weight<IRC> + ?Sized,
  IRC: IndexReaderContext,
{
  for ctx in searcher.get_leaf_contexts()? {
    let c1 = weight1.count(ctx, searcher)?;
    let c2 = weight2.count(ctx, searcher)?;
    assert_eq!(c1, c2);
  }
  Ok(())
}

#[test]
fn test_count_boundary() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = IndexWriterConfig::with_analyzer(mock)?;

  let mut sort_field = SortedNumericSortField::new("field", SortFieldType::Long)?;

  let use_lower = random.random_bool(0.5);
  let lower_value = 1_i64;
  let upper_value = 100_i64;

  if use_lower {
    sort_field.set_missing_value(lower_value)?;
  } else {
    sort_field.set_missing_value(upper_value)?;
  }

  let sort = Sort::with_fields(vec![sort_field])?;
  iwc.set_index_sort(sort)?;

  let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

  let first_value = random.random_range(lower_value..upper_value);
  writer.add_document(
    &mut random,
    create_sndv_and_point_document("field", first_value)?,
  )?;
  let second_value = random.random_range(lower_value..upper_value);
  writer.add_document(
    &mut random,
    create_sndv_and_point_document("field", second_value)?,
  )?;
  writer.add_document(&mut random, create_missing_value_document()?)?;

  let reader = writer.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;

  let fallback_query = LongPoint::new_range_query("field", lower_value, upper_value)?;

  let query = IndexSortSortedNumericDocValuesRangeQuery::new(
    "field",
    lower_value,
    upper_value,
    Box::new(fallback_query.into()),
  );

  let weight = query.create_weight(&searcher, &ScoreMode::Complete, 1.0)?;

  let mut count = 0;
  for ctx in searcher.get_leaf_contexts()? {
    count += weight.count(ctx, &searcher)?;
  }

  assert_eq!(2, count);

  writer.close(&mut random)?;
  Ok(())
}

fn create_missing_value_document() -> Result<Document> {
  let mut doc = Document::new();
  doc.add(StringField::from_string("foo", "fox", Store::Yes)?);
  Ok(doc)
}

fn create_sndv_and_point_document<S>(field: S, value: i64) -> Result<Document>
where
  S: Into<String>,
{
  let field = field.into();
  let mut doc = Document::new();
  doc.add(SortedNumericDocValuesField::new(&field, value));
  doc.add(LongPoint::new(&field, vec![value])?);
  Ok(doc)
}

fn create_document<S>(field: S, value: i64) -> Document
where
  S: Into<String>,
{
  let field = field.into();
  let mut doc = Document::new();
  doc.add(SortedNumericDocValuesField::new(&field, value));
  doc
}

fn create_query<S>(
  field: S,
  lower_value: i64,
  upper_value: i64,
) -> IndexSortSortedNumericDocValuesRangeQuery
where
  S: Into<String>,
{
  let field_str = field.into();

  let fallback_query =
    SortedNumericDocValuesField::new_slow_range_query(field_str.clone(), lower_value, upper_value);

  IndexSortSortedNumericDocValuesRangeQuery::new(
    field_str,
    lower_value,
    upper_value,
    Box::new(fallback_query.into()),
  )
}
#[test]
fn test_count_with_bkd_asc() -> Result<()> {
  do_test_count_with_bkd(false)
}

#[test]
fn test_count_with_bkd_desc() -> Result<()> {
  do_test_count_with_bkd(true)
}

fn do_test_count_with_bkd(reverse: bool) -> Result<()> {
  let mut random = random();
  let field_name = "field";

  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;

  let sort_field = SortedNumericSortField::with_reverse(field_name, SortFieldType::Long, reverse)?;
  let sort = Sort::with_fields(vec![sort_field])?;
  iwc.set_index_sort(sort)?;

  let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

  add_doc_with_bkd(&mut random, &writer, field_name, 7, 500)?;
  add_doc_with_bkd(&mut random, &writer, field_name, 5, 600)?;
  add_doc_with_bkd(&mut random, &writer, field_name, 11, 700)?;
  add_doc_with_bkd(&mut random, &writer, field_name, 13, 800)?;
  add_doc_with_bkd(&mut random, &writer, field_name, 9, 900)?;

  writer.flush()?;
  writer.force_merge(&mut random, 1)?;

  let reader = writer.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;
  // Both bounds exist in the dataset
  {
    let fallback = LongPoint::new_range_query(field_name, 7, 9)?;
    let query = IndexSortSortedNumericDocValuesRangeQuery::new(field_name, 7, 9, fallback);
    let weight = query.create_weight(&searcher, &ScoreMode::Complete, 1.0)?;
    for ctx in searcher.get_leaf_contexts()? {
      assert_eq!(1400, weight.count(ctx, &searcher)?);
    }
  }
  // Both bounds do not exist in the dataset
  {
    let fallback = LongPoint::new_range_query(field_name, 6, 10)?;
    let query = IndexSortSortedNumericDocValuesRangeQuery::new(field_name, 6, 10, fallback);
    let weight = query.create_weight(&searcher, &ScoreMode::Complete, 1.0)?;
    for ctx in searcher.get_leaf_contexts()? {
      assert_eq!(1400, weight.count(ctx, &searcher)?);
    }
  }
  // Min bound exists in the dataset, not the max
  {
    let fallback = LongPoint::new_range_query(field_name, 7, 10)?;
    let query = IndexSortSortedNumericDocValuesRangeQuery::new(field_name, 7, 10, fallback);
    let weight = query.create_weight(&searcher, &ScoreMode::Complete, 1.0)?;
    for ctx in searcher.get_leaf_contexts()? {
      assert_eq!(1400, weight.count(ctx, &searcher)?);
    }
  }
  // Min bound doesn't exist in the dataset, max does
  {
    let fallback = LongPoint::new_range_query(field_name, 6, 9)?;
    let query = IndexSortSortedNumericDocValuesRangeQuery::new(field_name, 6, 9, fallback);
    let weight = query.create_weight(&searcher, &ScoreMode::Complete, 1.0)?;
    for ctx in searcher.get_leaf_contexts()? {
      assert_eq!(1400, weight.count(ctx, &searcher)?);
    }
  }
  // Min bound is the min value of the dataset
  {
    let fallback = LongPoint::new_range_query(field_name, 5, 8)?;
    let query = IndexSortSortedNumericDocValuesRangeQuery::new(field_name, 5, 8, fallback);
    let weight = query.create_weight(&searcher, &ScoreMode::Complete, 1.0)?;
    for ctx in searcher.get_leaf_contexts()? {
      assert_eq!(1100, weight.count(ctx, &searcher)?);
    }
  }
  // Min bound is less than min value of the dataset
  {
    let fallback = LongPoint::new_range_query(field_name, 4, 8)?;
    let query = IndexSortSortedNumericDocValuesRangeQuery::new(field_name, 4, 8, fallback);
    let weight = query.create_weight(&searcher, &ScoreMode::Complete, 1.0)?;
    for ctx in searcher.get_leaf_contexts()? {
      assert_eq!(1100, weight.count(ctx, &searcher)?);
    }
  }
  // Max bound is the max value of the dataset
  {
    let fallback = LongPoint::new_range_query(field_name, 10, 13)?;
    let query = IndexSortSortedNumericDocValuesRangeQuery::new(field_name, 10, 13, fallback);
    let weight = query.create_weight(&searcher, &ScoreMode::Complete, 1.0)?;
    for ctx in searcher.get_leaf_contexts()? {
      assert_eq!(1500, weight.count(ctx, &searcher)?);
    }
  }
  // Max bound is greater than max value of the dataset
  {
    let fallback = LongPoint::new_range_query(field_name, 10, 14)?;
    let query = IndexSortSortedNumericDocValuesRangeQuery::new(field_name, 10, 14, fallback);
    let weight = query.create_weight(&searcher, &ScoreMode::Complete, 1.0)?;
    for ctx in searcher.get_leaf_contexts()? {
      assert_eq!(1500, weight.count(ctx, &searcher)?);
    }
  }
  // Everything matches
  {
    let fallback = LongPoint::new_range_query(field_name, 2, 14)?;
    let query = IndexSortSortedNumericDocValuesRangeQuery::new(field_name, 2, 14, fallback);
    let weight = query.create_weight(&searcher, &ScoreMode::Complete, 1.0)?;
    for ctx in searcher.get_leaf_contexts()? {
      assert_eq!(3500, weight.count(ctx, &searcher)?);
    }
  }
  // Bounds equal to min/max values of the dataset, everything matches
  {
    let fallback = LongPoint::new_range_query(field_name, 2, 3)?;
    let query = IndexSortSortedNumericDocValuesRangeQuery::new(field_name, 2, 3, fallback);
    let weight = query.create_weight(&searcher, &ScoreMode::Complete, 1.0)?;
    for ctx in searcher.get_leaf_contexts()? {
      assert_eq!(0, weight.count(ctx, &searcher)?);
    }
  }
  // Bounds are greater than the max value of the dataset
  {
    let fallback = LongPoint::new_range_query(field_name, 14, 15)?;
    let query = IndexSortSortedNumericDocValuesRangeQuery::new(field_name, 14, 15, fallback);
    let weight = query.create_weight(&searcher, &ScoreMode::Complete, 1.0)?;
    for ctx in searcher.get_leaf_contexts()? {
      assert_eq!(0, weight.count(ctx, &searcher)?);
    }
  }

  writer.close(&mut random)?;
  Ok(())
}

#[test]
fn test_random_count_with_bkd_asc() -> Result<()> {
  do_test_random_count_with_bkd(false)
}

#[test]
fn test_random_count_with_bkd_desc() -> Result<()> {
  do_test_random_count_with_bkd(true)
}

fn do_test_random_count_with_bkd(reverse: bool) -> Result<()> {
  let mut random = random();
  let field_name = "field";
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let mut iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let sort_field = SortedNumericSortField::with_reverse(field_name, SortFieldType::Long, reverse)?;
  let sort = Sort::with_fields(vec![sort_field])?;
  iwc.set_index_sort(sort)?;
  let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

  for _i in 0..100 {
    let value = random.random_range(0..1000) as i64;
    let repeat = random.random_range(0..1000);

    add_doc_with_bkd(&mut random, &writer, field_name, value, repeat)?;
  }

  writer.flush()?;
  writer.force_merge(&mut random, 1)?;
  let reader = writer.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;

  for _k in 0..100 {
    let random1 = random.random_range(0..1100) as i64;
    let random2 = random.random_range(0..1100) as i64;

    let low = random1.min(random2);
    let upper = random1.max(random2);

    let range_query = LongPoint::new_range_query(field_name, low, upper)?;
    let index_sort_range_query =
      IndexSortSortedNumericDocValuesRangeQuery::new(field_name, low, upper, range_query.clone());

    let index_sort_range_query_weight =
      index_sort_range_query.create_weight(&searcher, &ScoreMode::Complete, 1.0)?;

    let range_query_weight = range_query.create_weight(&searcher, &ScoreMode::Complete, 1.0)?;

    for ctx in searcher.get_leaf_contexts()? {
      let expected = range_query_weight.count(ctx, &searcher)?;
      let actual = index_sort_range_query_weight.count(ctx, &searcher)?;
      assert_eq!(expected, actual);
    }
  }
  writer.close(&mut random)?;
  Ok(())
}

fn add_doc_with_bkd<R, D>(
  random: &mut R,
  index_writer: &RandomIndexWriter<D>,
  field: &str,
  value: i64,
  repeat: i32,
) -> Result<()>
where
  R: Rng + ?Sized,
  D: Directory + 'static,
{
  for _ in 0..repeat {
    let mut doc = Document::new();
    doc.add(SortedNumericDocValuesField::new(field, value));
    doc.add(LongPoint::new(field, vec![value])?);
    index_writer.add_document(random, doc)?;
  }
  Ok(())
}
