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
use crate::core::document::float_doc_values_field::FloatDocValuesField;
use crate::core::document::float_point::FloatPoint;
use crate::core::document::int_point::IntPoint;
use crate::core::document::int_range::IntRange;
use crate::core::document::keyword_field::KeywordField;
use crate::core::document::long_field::{LongField, new_sort_field};
use crate::core::document::long_point::LongPoint;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::stored_field::StoredField;
use crate::core::document::string_field::StringField;
use crate::core::index::BytesRef;
use crate::core::index::base_composite_reader::{BaseCompositeReader, BaseCompositeReaderBase};
use crate::core::index::composite_reader::CompositeReader;
use crate::core::index::directory_reader::{self, DirectoryReader, DirectoryReaderBase};
use crate::core::index::dummy::dummy_cache_helper::DummyCacheHelper;
use crate::core::index::dummy::dummy_point_value_base::DummyPointValues;
use crate::core::index::dummy::dummy_terms::DummyTerms;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::filter_directory_reader::{FilterDirectoryReader, SubReaderWrapper};
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader::{
  CompositeReaderContextKind, IndexReader, IndexReaderBase, LeafReaderContextKind,
};
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::leaf_metadata::LeafMetaData;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::stored_fields::StoredFields;
use crate::core::index::term::Term;
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::boolean_query::Builder;
use crate::core::search::field_comparator::FieldComparatorValue;
use crate::core::search::field_doc::FieldDoc;
use crate::core::search::field_value_hit_queue::TopFieldScoreDoc;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::search::match_all_docs_query::MatchAllDocsQuery;
use crate::core::search::query::Query;
use crate::core::search::score_doc::ScoreDocLike;
use crate::core::search::sort::Sort;
use crate::core::search::sort_field::MissingValueEnum::{StringFirst, StringLast};
use crate::core::search::sort_field::{SortField, SortFieldType, SortFiledBase};
use crate::core::search::sorted_numeric_selector::SortedNumericSelectorType;
use crate::core::search::sorted_set_selector::SortedSetSelectorType;
use crate::core::search::term_query::TermQuery;
use crate::core::search::top_docs::{self, TopDocsLike};
use crate::core::search::top_field_collector_manager::TopFieldCollectorManager;
use crate::core::search::top_field_docs::TopFieldDocs;
use crate::core::search::total_hits::Relation;
use crate::core::store::directory::Directory;
use crate::core::util::bits::Bits;
use crate::core::util::dummy::dummy_comparator::DummyComparator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::search::check_hits::CheckHits;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, at_least_usize, is_night_mode, new_directory_shared, new_index_writer_config,
  new_log_merge_policy, new_searcher, new_searcher_with_reader, new_searcher_with_threads, random,
};
use rand::RngExt;
use rand::prelude::SliceRandom;
use rand::random_bool;
use rand_chacha::rand_core::Rng;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

#[allow(dead_code)] // for quick search
struct TestSortOptimization;

#[test]
fn test_long_sort_optimization() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let config = IndexWriterConfig::new()?;
  let writer = IndexWriter::new(dir.clone(), config)?;

  // let num_docs = at_least(&mut random, 10_000);
  let num_docs = 11112;
  for i in 0..num_docs {
    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("my_field", i as i64));
    doc.add(LongPoint::new("my_field", vec![i as i64])?);
    writer.add_document(doc)?;
    if i == 7000 {
      writer.flush()?;
    }
  }

  let reader = directory_reader::open_from_writer(&writer)?;
  writer.close()?;
  let searcher = new_searcher_with_threads(&mut random, reader, true, true, false)?;
  let sort_field = SortField::new(Some("my_field"), SortFieldType::Long)?;
  let sort = Arc::new(Sort::with_fields(vec![sort_field])?);
  let num_hits = 3;
  let total_hits_threshold = 3;
  // simple sort
  {
    let collector_manager =
      TopFieldCollectorManager::new(sort.clone(), num_hits, total_hits_threshold)?;

    let top_docs =
      searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &collector_manager)?;

    assert_eq!(top_docs.score_docs().len(), num_hits);

    for i in 0..num_hits {
      let field_doc = &top_docs.score_docs()[i];
      let fields = &field_doc.fields()?[0];
      let value = *fields.as_i64().expect("should be i64");
      assert_eq!(i as i64, value);
    }

    assert_eq!(
      top_docs.total_hits().relation,
      Relation::GreaterThanOrEqualTo
    );

    assert_non_competitive_hits_are_skipped(top_docs.total_hits().value as i64, num_docs as i64)?;
  }
  // paging sort with after
  {
    let after_value: i64 = 2;
    let after = FieldDoc::with_fields(2, f32::NAN, vec![after_value.into()]);
    let collector_manager = TopFieldCollectorManager::with_after(
      sort.clone(),
      num_hits,
      Some(after),
      total_hits_threshold,
    )?;

    let top_docs =
      searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &collector_manager)?;

    assert_eq!(top_docs.score_docs().len(), num_hits);

    for i in 0..num_hits {
      let field_doc = &top_docs.score_docs()[i];
      let fields = &field_doc.fields()?[0];
      let value = *fields.as_i64().expect("should be i64");
      assert_eq!(after_value + 1 + i as i64, value);
    }

    assert_eq!(
      top_docs.total_hits().relation,
      Relation::GreaterThanOrEqualTo
    );

    assert_non_competitive_hits_are_skipped(top_docs.total_hits().value as i64, num_docs as i64)?;
  }
  // test that if there is the secondary sort on _score, scores are filled correctly
  {
    let sort_field = SortField::new(Some("my_field"), SortFieldType::Long)?;
    let sort = Sort::with_fields(vec![sort_field, SortField::get_field_score()?])?;

    let collector_manager = TopFieldCollectorManager::new(sort, num_hits, total_hits_threshold)?;

    let top_docs =
      searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &collector_manager)?;

    assert_eq!(top_docs.score_docs().len(), num_hits);

    for i in 0..num_hits {
      let field_doc = &top_docs.score_docs()[i];
      let fields = field_doc.fields()?;

      let long_val = *fields[0].as_i64().expect("should be i64");
      assert_eq!(i as i64, long_val);

      let score = *fields[1].as_f32().expect("should be f32");
      assert!((score - 1.0).abs() < 0.001);
    }

    assert_eq!(
      top_docs.total_hits().relation,
      Relation::GreaterThanOrEqualTo
    );

    assert_non_competitive_hits_are_skipped(top_docs.total_hits().value as i64, num_docs as i64)?;
  }
  // test that if numeric field is a secondary sort, no optimization is run
  {
    let sort_field = SortField::new(Some("my_field"), SortFieldType::Long)?;
    let sort = Sort::with_fields(vec![SortField::get_field_score()?, sort_field])?;

    let collector_manager = TopFieldCollectorManager::new(sort, num_hits, total_hits_threshold)?;

    let top_docs =
      searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &collector_manager)?;

    assert_eq!(top_docs.score_docs().len(), num_hits);
    // assert that all documents were collected => optimization was not run
    assert_eq!(top_docs.total_hits().value as i32, num_docs);
  }

  Ok(())
}
/// test that even if a field is not indexed with points, optimized sort still works as expected,
/// although no optimization will be run
#[test]
fn test_long_sort_optimization_on_field_not_indexed_with_points() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;

  let num_docs = at_least(&mut random, 100);
  // "my_field" is not indexed with points
  for i in 0..num_docs {
    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("my_field", i as i64));
    writer.add_document(doc)?;
  }

  let reader = directory_reader::open_from_writer(&writer)?;
  writer.close()?;

  // single-threaded so totalHits is deterministic
  let maybe_wrap = random.random_bool(0.5);
  let searcher =
    new_searcher_with_threads(&mut random, reader, maybe_wrap, random_bool(0.5), false)?;
  let sort_field = SortField::new(Some("my_field"), SortFieldType::Long)?;
  let sort = Sort::with_fields(vec![sort_field])?;
  let num_hits = 3;
  let total_hits_threshold = 3;

  let collector_manager = TopFieldCollectorManager::new(sort, num_hits, total_hits_threshold)?;
  let top_docs =
    searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &collector_manager)?;

  // sort still works and returns expected number of docs
  assert_eq!(top_docs.score_docs().len(), num_hits);

  // returns expected values
  for i in 0..num_hits {
    let field_doc = &top_docs.score_docs()[i];
    let fields = field_doc.fields()?;
    let long_val = *fields[0].as_i64().expect("should be i64");
    assert_eq!(i as i64, long_val);
  }

  // assert that all documents were collected => optimization was not run
  assert_eq!(top_docs.total_hits().value as i32, num_docs);

  Ok(())
}
#[test]
fn test_sort_optimization_with_missing_values() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let config = IndexWriterConfig::new()?;
  let writer = IndexWriter::new(dir.clone(), config)?;

  let num_docs = at_least(&mut random, 10_000);
  for i in 0..num_docs {
    let mut doc = Document::new();
    // miss values on every 500th document
    if i % 500 != 0 {
      doc.add(NumericDocValuesField::new("my_field", i as i64));
      doc.add(LongPoint::new("my_field", vec![i as i64])?);
    }
    writer.add_document(doc)?;
    if i == 7000 {
      writer.flush()?;
    }
  }

  let reader = directory_reader::open_from_writer(&writer)?;
  writer.close()?;
  let searcher = new_searcher_with_threads(
    &mut random,
    reader,
    random_bool(0.5),
    random_bool(0.5),
    false,
  )?;
  let num_hits = 3;
  let total_hits_threshold = 3;

  {
    let mut sort_field = SortField::new(Some("my_field"), SortFieldType::Long)?;
    sort_field.set_missing_value(0i64)?;
    let sort = Sort::with_fields(vec![sort_field])?;
    let collector_manager = TopFieldCollectorManager::new(sort, num_hits, total_hits_threshold)?;
    let top_docs =
      searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &collector_manager)?;
    assert_eq!(top_docs.score_docs().len(), num_hits);
    assert_non_competitive_hits_are_skipped(top_docs.total_hits().value as i64, num_docs as i64)?;
  }

  {
    let mut sf1 = SortField::new(Some("my_field1"), SortFieldType::Long)?;
    let mut sf2 = SortField::new(Some("my_field2"), SortFieldType::Long)?;
    sf1.set_missing_value(0i64)?;
    sf2.set_missing_value(0i64)?;
    let sort = Sort::with_fields(vec![sf1, sf2])?;
    let collector_manager = TopFieldCollectorManager::new(sort, num_hits, total_hits_threshold)?;
    let top_docs =
      searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &collector_manager)?;
    assert_eq!(top_docs.score_docs().len(), num_hits);
    assert_eq!(top_docs.total_hits().value as i32, num_docs);
  }

  {
    let mut sort_field = SortField::new(Some("my_field"), SortFieldType::Long)?;
    sort_field.set_missing_value(100i64)?;
    let sort = Sort::with_fields(vec![sort_field])?;
    let collector_manager = TopFieldCollectorManager::new(sort, num_hits, total_hits_threshold)?;
    let top_docs =
      searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &collector_manager)?;
    assert_eq!(top_docs.score_docs().len(), num_hits);
    assert_non_competitive_hits_are_skipped(top_docs.total_hits().value as i64, num_docs as i64)?;
  }

  {
    let after_value = i64::MAX;
    let after = FieldDoc::with_fields(
      10 + random.random_range(0..1000),
      f32::NAN,
      vec![after_value.into()],
    );
    let mut sort_field = SortField::new(Some("my_field"), SortFieldType::Long)?;
    sort_field.set_missing_value(i64::MAX)?;
    let sort = Sort::with_fields(vec![sort_field])?;
    let collector_manager =
      TopFieldCollectorManager::with_after(sort, num_hits, Some(after), total_hits_threshold)?;
    let top_docs =
      searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &collector_manager)?;
    assert_eq!(top_docs.score_docs().len(), num_hits);
    assert_non_competitive_hits_are_skipped(top_docs.total_hits().value as i64, num_docs as i64)?;
  }

  {
    let after_value = i64::MAX;
    let after = FieldDoc::with_fields(
      10 + random.random_range(0..1000),
      f32::NAN,
      vec![after_value.into()],
    );
    let mut sort_field = SortField::with_reverse(Some("my_field"), SortFieldType::Long, true)?;
    sort_field.set_missing_value(i64::MAX)?;
    let sort = Sort::with_fields(vec![sort_field])?;
    let collector_manager =
      TopFieldCollectorManager::with_after(sort, num_hits, Some(after), total_hits_threshold)?;
    let top_docs =
      searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &collector_manager)?;
    assert_eq!(top_docs.score_docs().len(), num_hits);
    assert_non_competitive_hits_are_skipped(top_docs.total_hits().value as i64, num_docs as i64)?;
  }

  {
    let after_value: i64 = 3;
    let after = FieldDoc::with_fields(3, f32::NAN, vec![after_value.into()]);
    let mut sort_field = SortField::new(Some("my_field"), SortFieldType::Long)?;
    sort_field.set_missing_value(2i64)?;
    let sort = Sort::with_fields(vec![sort_field])?;
    let collector_manager =
      TopFieldCollectorManager::with_after(sort, num_hits, Some(after), total_hits_threshold)?;
    let top_docs =
      searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &collector_manager)?;
    assert_eq!(top_docs.score_docs().len(), num_hits);

    for i in 0..num_hits {
      let field_doc = &top_docs.score_docs()[i];
      let fields = &field_doc.fields()?[0];
      let value = *fields.as_i64().expect("should be i64");
      assert_eq!(after_value + 1 + i as i64, value);
    }

    assert_eq!(
      top_docs.total_hits().relation,
      Relation::GreaterThanOrEqualTo
    );

    let expected_skipped = (7001 - 512 - 1) + (num_docs - 7001);
    assert_non_competitive_hits_are_skipped(
      top_docs.total_hits().value as i64,
      num_docs as i64 - expected_skipped as i64 + 1,
    )?;
  }

  Ok(())
}
#[test]
fn test_numeric_doc_values_optimization_with_missing_values() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let config = IndexWriterConfig::new()?;
  let writer = IndexWriter::new(dir.clone(), config)?;

  let num_docs = at_least(&mut random, 10_000);
  let miss_values_num_docs = num_docs / 2;

  for i in 0..num_docs {
    let mut doc = Document::new();
    if i > miss_values_num_docs {
      doc.add(NumericDocValuesField::new("my_field", i as i64));
      doc.add(LongPoint::new("my_field", vec![i as i64])?);
    }
    writer.add_document(doc)?;
  }

  let reader = directory_reader::open_from_writer(&writer)?;
  writer.close()?;
  let searcher = new_searcher_with_threads(
    &mut random,
    reader,
    random_bool(0.5),
    random_bool(0.5),
    false,
  )?;
  let num_hits = 3;
  let total_hits_threshold = 3;

  let top_docs1;
  let top_docs2;

  {
    let mut sort_field = SortField::with_reverse(Some("my_field"), SortFieldType::Long, true)?;
    sort_field.set_missing_value(0i64)?;
    let sort = Sort::with_fields(vec![sort_field])?;

    let collector_manager = TopFieldCollectorManager::new(sort, num_hits, total_hits_threshold)?;
    top_docs1 =
      searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &collector_manager)?;
    assert_non_competitive_hits_are_skipped(top_docs1.total_hits().value as i64, num_docs as i64)?;
  }

  {
    let mut sort_field = SortField::with_reverse(Some("my_field"), SortFieldType::Long, true)?;
    sort_field.set_missing_value(0i64)?;
    sort_field.set_optimize_sort_with_points(false);
    let sort = Sort::with_fields(vec![sort_field])?;

    let collector_manager = TopFieldCollectorManager::new(sort, num_hits, total_hits_threshold)?;
    top_docs2 =
      searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &collector_manager)?;

    assert_eq!(top_docs1.score_docs().len(), top_docs2.score_docs().len());
    assert_eq!(top_docs1.score_docs().len(), num_hits);

    for i in 0..num_hits {
      let fd1 = &top_docs1.score_docs()[i];
      let fd2 = &top_docs2.score_docs()[i];
      let v1 = fd1.fields()?[0].as_i64().unwrap();
      let v2 = fd2.fields()?[0].as_i64().unwrap();
      assert_eq!(v1, v2);
      assert_eq!(fd1.doc(), fd2.doc());
    }

    assert!(top_docs1.total_hits().value < top_docs2.total_hits().value);
  }

  {
    let mut sf1 = SortField::with_reverse(Some("my_field"), SortFieldType::Long, true)?;
    let mut sf2 = SortField::with_reverse(Some("other"), SortFieldType::Long, true)?;
    sf1.set_missing_value(0i64)?;
    sf2.set_missing_value(0i64)?;
    let sort = Sort::with_fields(vec![sf1, sf2])?;

    let collector_manager = TopFieldCollectorManager::new(sort, num_hits, total_hits_threshold)?;
    let top_docs =
      searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &collector_manager)?;
    assert_eq!(top_docs.total_hits().value as i32, num_docs);
  }

  Ok(())
}
#[test]
fn test_sort_optimization_equal_values() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let config = IndexWriterConfig::new()?;
  let writer = IndexWriter::new(dir.clone(), config)?;

  let num_docs = if is_night_mode() {
    at_least(&mut random, 50_000)
  } else {
    at_least(&mut random, 10_000)
  };

  for i in 1..=num_docs {
    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("my_field1", 100));
    doc.add(IntPoint::new("my_field1", vec![100])?);
    doc.add(NumericDocValuesField::new(
      "my_field2",
      (num_docs - i) as i64,
    ));
    writer.add_document(doc)?;
    if i == 7000 && random.random_bool(0.5) {
      writer.flush()?;
    }
  }

  let reader = directory_reader::open_from_writer(&writer)?;
  writer.close()?;

  let searcher = new_searcher_with_threads(
    &mut random,
    reader,
    random_bool(0.5),
    random_bool(0.5),
    false,
  )?;
  let num_hits = 3;
  let total_hits_threshold = 3;

  {
    let sort_field = SortField::new(Some("my_field1"), SortFieldType::Int)?;
    let sort = Sort::with_fields(vec![sort_field])?;
    let collector_manager = TopFieldCollectorManager::new(sort, num_hits, total_hits_threshold)?;
    let top_docs =
      searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &collector_manager)?;

    assert_eq!(top_docs.score_docs().len(), num_hits);

    for i in 0..num_hits {
      let fd = &top_docs.score_docs()[i];
      let fields = fd.fields()?;
      assert_eq!(*fields[0].as_i32().unwrap(), 100);
    }

    if searcher.reader_context.leaves()?.len() == 1 {
      assert_eq!(top_docs.total_hits().value, num_hits + 1);
    }

    assert_non_competitive_hits_are_skipped(top_docs.total_hits().value as i64, num_docs as i64)?;
  }

  {
    let after_value = 100_i32;
    let after_doc_id = 10 + random.random_range(0..1000);
    let sort_field = SortField::new(Some("my_field1"), SortFieldType::Int)?;
    let sort = Sort::with_fields(vec![sort_field])?;
    let after = FieldDoc::with_fields(after_doc_id, f32::NAN, vec![after_value.into()]);
    let collector_manager =
      TopFieldCollectorManager::with_after(sort, num_hits, Some(after), total_hits_threshold)?;
    let top_docs =
      searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &collector_manager)?;

    assert_eq!(top_docs.score_docs().len(), num_hits);
    for i in 0..num_hits {
      let fd = &top_docs.score_docs()[i];
      let fields = fd.fields()?;
      assert_eq!(*fields[0].as_i32().unwrap(), 100);
      assert!(fd.doc() > after_doc_id);
    }

    assert_non_competitive_hits_are_skipped(top_docs.total_hits().value as i64, num_docs as i64)?;
  }

  {
    let sf1 = SortField::new(Some("my_field1"), SortFieldType::Int)?;
    let sf2 = SortField::new(Some("my_field2"), SortFieldType::Int)?;
    let sort = Sort::with_fields(vec![sf1, sf2])?;
    let collector_manager = TopFieldCollectorManager::new(sort, num_hits, total_hits_threshold)?;
    let top_docs =
      searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &collector_manager)?;

    assert_eq!(top_docs.score_docs().len(), num_hits);

    for i in 0..num_hits {
      let fd = &top_docs.score_docs()[i];
      let fields = fd.fields()?;
      assert_eq!(*fields[0].as_i32().unwrap(), 100);
      assert_eq!(*fields[1].as_i32().unwrap(), i as i32);
    }

    assert_eq!(top_docs.total_hits().value as i32, num_docs);
  }

  Ok(())
}
#[test]
fn test_float_sort_optimization() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let config = IndexWriterConfig::new()?;
  let writer = IndexWriter::new(dir.clone(), config)?;

  let num_docs = at_least(&mut random, 10_000);
  for i in 0..num_docs {
    let mut doc = Document::new();
    let f = i as f32;
    doc.add(FloatDocValuesField::new("my_field", f));
    doc.add(FloatPoint::new("my_field", vec![f])?);
    writer.add_document(doc)?;
  }

  let reader = directory_reader::open_from_writer(&writer)?;
  writer.close()?;

  let searcher = new_searcher_with_threads(
    &mut random,
    reader,
    random_bool(0.5),
    random_bool(0.5),
    false,
  )?;
  let sort_field = SortField::new(Some("my_field"), SortFieldType::Float)?;
  let sort = Sort::with_fields(vec![sort_field])?;
  let num_hits = 3;
  let total_hits_threshold = 3;

  {
    let collector_manager = TopFieldCollectorManager::new(sort, num_hits, total_hits_threshold)?;
    let top_docs =
      searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &collector_manager)?;

    assert_eq!(top_docs.score_docs().len(), num_hits);

    for i in 0..num_hits {
      let fd = &top_docs.score_docs()[i];
      let fields = fd.fields()?;
      let v = *fields[0].as_f32().expect("should be f32");
      assert!((v - i as f32).abs() < f32::EPSILON);
    }

    assert_eq!(
      top_docs.total_hits().relation,
      Relation::GreaterThanOrEqualTo
    );

    assert_non_competitive_hits_are_skipped(top_docs.total_hits().value as i64, num_docs as i64)?;
  }

  Ok(())
}

#[test]
fn test_doc_sort_optimization_multiple_indices() -> Result<()> {
  let mut random = random();
  let num_indices = 3;
  let num_docs_in_index = at_least_usize(&mut random, 50);

  let mut dirs = Vec::with_capacity(num_indices);

  for i in 0..num_indices {
    let dir = new_directory_shared(&mut random)?;
    let config = IndexWriterConfig::new()?;
    let writer = IndexWriter::new(dir.clone(), config)?;
    for doc_id in 0..num_docs_in_index {
      let mut doc = Document::new();
      doc.add(NumericDocValuesField::new(
        "my_field",
        (doc_id * num_indices + i) as i64,
      ));
      writer.add_document(doc)?;
    }
    writer.flush()?;
    writer.close()?;
    dirs.push(dir);
  }

  let size = 7;
  let total_hits_threshold = 7;
  let sort = Arc::new(Sort::with_fields(vec![
    SortField::get_field_doc()?,
    SortField::new(Some("my_field"), SortFieldType::Long)?,
  ])?);

  let mut cur_num_hits;
  let mut after: Option<FieldDoc> = None;
  let mut collected_docs: i64 = 0;
  let mut total_docs = 0;
  let mut num_hits = 0;

  loop {
    let mut top_docs_vec = Vec::new();
    #[allow(clippy::needless_range_loop)]
    for i in 0..num_indices {
      let reader = directory_reader::open(dirs[i].clone())?;
      let searcher = new_searcher_with_threads(
        &mut random,
        reader,
        random_bool(0.5),
        random_bool(0.5),
        false,
      )?;
      let collector_manager = TopFieldCollectorManager::with_after(
        sort.clone(),
        size,
        after.clone(),
        total_hits_threshold,
      )?;
      let mut top_docs =
        searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &collector_manager)?;
      for doc_id in 0..top_docs.base.score_docs.len() {
        top_docs.score_docs_mut()[doc_id].set_shard_index(i as i32)
      }
      collected_docs += top_docs.total_hits().value as i64;
      total_docs += num_docs_in_index as i64;
      top_docs_vec.push(top_docs.base)
    }

    let mut merged_top_docs = top_docs::merge_top_field_docs(sort.as_ref(), size, top_docs_vec)?;
    cur_num_hits = merged_top_docs.score_docs().len();
    num_hits += cur_num_hits;
    if cur_num_hits > 0 {
      let v = std::mem::take(&mut merged_top_docs.score_docs_mut()[cur_num_hits - 1]);
      match v {
        TopFieldScoreDoc::Field(field_doc) => {
          after = Some(field_doc);
        },
        _ => {
          return Err(LuceneError::illegal_state("Expected FieldDoc type"));
        },
      }
    } else {
      break;
    }
  }
  let expected_num_hits = num_docs_in_index * num_indices;
  assert_eq!(expected_num_hits, num_hits);
  assert_non_competitive_hits_are_skipped(collected_docs, total_docs)?;

  Ok(())
}
#[test]
fn test_doc_sort_optimization_with_after() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;

  let num_docs = at_least_usize(&mut random, 150);
  for i in 0..num_docs {
    let doc = Document::new();
    writer.add_document(doc)?;
    if i > 0 && i % 50 == 0 {
      writer.flush()?;
    }
  }

  let reader = directory_reader::open_from_writer(&writer)?;
  writer.close()?;
  let searcher = new_searcher_with_threads(
    &mut random,
    reader,
    random_bool(0.5),
    random_bool(0.5),
    false,
  )?;
  let num_hits = 10;
  let total_hits_threshold = 10;
  let search_afters = [3, 10, num_docs as i32 - 10];

  for &search_after in &search_afters {
    {
      let sort = Sort::with_fields(vec![SortField::get_field_doc()?])?;
      let after = FieldDoc::with_fields(
        search_after,
        f32::NAN,
        vec![FieldComparatorValue::Int(search_after)],
      );
      let collector_manager =
        TopFieldCollectorManager::with_after(sort, num_hits, Some(after), total_hits_threshold)?;
      let top_docs =
        searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &collector_manager)?;
      let exp_num_hits = if search_after as usize >= (num_docs - num_hits) {
        num_docs - (search_after as usize) - 1
      } else {
        num_hits
      };
      assert_eq!(exp_num_hits, top_docs.score_docs().len());
      for (i, sd) in top_docs.score_docs().iter().enumerate() {
        assert_eq!(search_after + 1 + i as i32, sd.doc());
      }
      assert_eq!(
        top_docs.total_hits().relation,
        Relation::GreaterThanOrEqualTo
      );
      assert_non_competitive_hits_are_skipped(top_docs.total_hits().value as i64, num_docs as i64)?;
    }

    // sort by _doc + _score with search after should trigger optimization
    {
      let sort = Sort::with_fields(vec![
        SortField::get_field_doc()?,
        SortField::get_field_score()?,
      ])?;
      let after = FieldDoc::with_fields(
        search_after,
        f32::NAN,
        vec![
          FieldComparatorValue::Int(search_after),
          FieldComparatorValue::Float(1.0),
        ],
      );
      let collector_manager =
        TopFieldCollectorManager::with_after(sort, num_hits, Some(after), total_hits_threshold)?;
      let top_docs =
        searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &collector_manager)?;
      let exp_num_hits = if search_after as usize >= (num_docs - num_hits) {
        num_docs - (search_after as usize) - 1
      } else {
        num_hits
      };
      assert_eq!(exp_num_hits, top_docs.score_docs().len());
      for (i, sd) in top_docs.score_docs().iter().enumerate() {
        assert_eq!(search_after + 1 + i as i32, sd.doc());
      }
      assert_eq!(
        top_docs.total_hits().relation,
        Relation::GreaterThanOrEqualTo
      );
      assert_non_competitive_hits_are_skipped(top_docs.total_hits().value as i64, num_docs as i64)?;
    }

    // sort by _doc desc should not trigger optimization
    {
      let sort_field = SortField::with_reverse(None::<String>, SortFieldType::Doc, true)?;
      let sort = Sort::with_fields(vec![sort_field])?;
      let after = FieldDoc::with_fields(
        search_after,
        f32::NAN,
        vec![FieldComparatorValue::Int(search_after)],
      );
      let collector_manager =
        TopFieldCollectorManager::with_after(sort, num_hits, Some(after), total_hits_threshold)?;
      let top_docs =
        searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &collector_manager)?;
      let exp_num_hits = if (search_after as usize) < num_hits {
        search_after as usize
      } else {
        num_hits
      };
      assert_eq!(exp_num_hits, top_docs.score_docs().len());
      for (i, sd) in top_docs.score_docs().iter().enumerate() {
        assert_eq!(search_after - 1 - i as i32, sd.doc());
      }
      assert_eq!(num_docs as i64, top_docs.total_hits().value as i64);
    }
  }

  Ok(())
}

#[test]
fn test_doc_sort_optimization_with_after_collects_all_docs() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;

  let num_docs = if is_night_mode() {
    at_least_usize(&mut random, 50_000)
  } else {
    at_least_usize(&mut random, 5_000)
  };

  let multiple_segments = random.random_bool(0.5);
  let num_docs_in_segment = num_docs / 10 + random.random_range(0..(num_docs / 10).max(1));

  for i in 1..=num_docs {
    let doc = Document::new();
    writer.add_document(doc)?;
    if multiple_segments && i % num_docs_in_segment == 0 {
      writer.flush()?;
    }
  }
  writer.flush()?;

  let reader = directory_reader::open_from_writer(&writer)?;
  let searcher = new_searcher_with_reader(reader)?;
  writer.close()?;

  let mut visited_hits = 0;
  let mut after: Option<FieldDoc> = None;

  while visited_hits < num_docs {
    let batch = 1 + random.random_range(0..500);
    let sort = Sort::with_fields(vec![SortField::get_field_doc()?])?;
    let top_docs = searcher.search_after(after, MatchAllDocsQuery::new(), batch, sort)?;

    let expected_hits = std::cmp::min(num_docs - visited_hits, batch);
    assert_eq!(expected_hits, top_docs.score_docs().len());

    let last_doc = top_docs.score_docs()[expected_hits - 1].clone();
    match last_doc {
      TopFieldScoreDoc::Field(field_doc) => after = Some(field_doc),
      _ => {
        return Err(LuceneError::illegal_state(
          "Expected FieldDoc type in TopFieldScoreDoc",
        ));
      },
    }

    for sd in top_docs.score_docs() {
      assert_eq!(visited_hits, sd.doc() as usize);
      visited_hits += 1;
    }
  }

  assert_eq!(visited_hits, num_docs);
  Ok(())
}
#[test]
fn test_doc_sort_optimization() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;

  let num_docs = at_least(&mut random, 100);
  let mut seg = 1;
  for i in 0..num_docs {
    let mut doc = Document::new();
    doc.add(LongPoint::new("lf", vec![i as i64])?);
    doc.add(StoredField::from_i32("slf", i)?);
    doc.add(StringField::from_string(
      "tf",
      format!("seg{}", seg),
      Store::Yes,
    )?);
    writer.add_document(doc)?;
    if i > 0 && i % 50 == 0 {
      writer.flush()?;
      seg += 1;
    }
  }

  let reader = directory_reader::open_from_writer(&writer)?;
  writer.close()?;
  let searcher = new_searcher_with_threads(
    &mut random,
    reader,
    random_bool(0.5),
    random_bool(0.5),
    false,
  )?;

  let num_hits = 3;
  let total_hits_threshold = 3;
  let sort = Sort::with_fields(vec![SortField::get_field_doc()?])?;

  // sort by _doc should skip all non-competitive documents
  {
    let collector_manager =
      TopFieldCollectorManager::new(sort.clone(), num_hits, total_hits_threshold)?;
    let top_docs =
      searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &collector_manager)?;

    assert_eq!(num_hits, top_docs.score_docs().len());
    for (i, sd) in top_docs.score_docs().iter().enumerate() {
      assert_eq!(i as i32, sd.doc());
    }

    assert_eq!(
      top_docs.total_hits().relation,
      Relation::GreaterThanOrEqualTo
    );
    assert_non_competitive_hits_are_skipped(top_docs.total_hits().value as i64, 10)?;
  }
  // sort by _doc with a bool query should skip all non-competitive documents
  {
    let collector_manager =
      TopFieldCollectorManager::new(sort.clone(), num_hits, total_hits_threshold)?;

    let lower_range = 40;

    let mut bq = Builder::new();
    bq.add(
      LongPoint::new_range_query("lf", lower_range as i64, i64::MAX)?,
      Occur::Must,
    )?;
    bq.add(TermQuery::new(Term::from_text("tf", "seg1")), Occur::Must)?;

    let top_docs = searcher.search_with_collector_manager(bq.build(), &collector_manager)?;

    assert_eq!(num_hits, top_docs.score_docs().len());

    let mut stored_fields = searcher.stored_fields()?;

    for (i, sd) in top_docs.score_docs().iter().enumerate() {
      let d = stored_fields.document(sd.doc())?;
      assert_eq!(
        &(i + lower_range).to_string(),
        d.get("slf")?.unwrap().as_ref()
      );
      assert_eq!("seg1", d.get("tf")?.unwrap().as_ref());
    }

    assert_eq!(
      top_docs.total_hits().relation,
      Relation::GreaterThanOrEqualTo
    );

    assert_non_competitive_hits_are_skipped(top_docs.total_hits().value as i64, 10)?;
  }
  Ok(())
}
/// Test that sorting on _doc works correctly.
/// This test goes through DefaultBulkSorter::scoreRange, where scorerIterator is BitSetIterator.
/// As a conjunction of this BitSetIterator with DocComparator's iterator, we get BitSetConjunctionDISI.
/// BitSetConjuctionDISI advances based on the DocComparator's iterator, and doesn't consider that its BitSetIterator may have advanced passed a certain doc.
#[test]
fn test_doc_sort() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;

  let num_docs = 4;

  for i in 0..num_docs {
    let mut doc = Document::new();
    doc.add(StringField::from_string(
      "id",
      format!("id{}", i),
      Store::No,
    )?);

    if i < 2 {
      doc.add(LongPoint::new("lf", vec![1])?);
    }

    writer.add_document(doc)?;
  }

  let reader = directory_reader::open_from_writer(&writer)?;
  writer.close()?;

  let may_be_wrap = random.random_bool(0.5);
  let wrap_with_assertions = random.random_bool(0.5);
  let mut searcher = new_searcher_with_threads(
    &mut random,
    reader,
    may_be_wrap,
    wrap_with_assertions,
    false,
  )?;
  searcher.set_query_cache(None);

  let num_hits = 10;
  let total_hits_threshold = 10;
  let sort = Sort::with_fields(vec![SortField::get_field_doc()?])?;

  {
    let collector_manager =
      TopFieldCollectorManager::new(sort.clone(), num_hits, total_hits_threshold)?;

    let mut bq = Builder::new();
    bq.add(LongPoint::new_exact_query("lf", 1)?, Occur::Must)?;
    bq.add(TermQuery::new(Term::from_text("id", "id3")), Occur::MustNot)?;

    let top_docs = searcher.search_with_collector_manager(bq.build(), &collector_manager)?;

    assert_eq!(2, top_docs.score_docs().len());
  }
  Ok(())
}

#[test]
fn test_point_validation() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;

  let mut doc = Document::new();

  doc.add(IntPoint::new("intField", [4])?);
  doc.add(NumericDocValuesField::new("intField", 4i64));

  doc.add(LongPoint::new("longField", [42i64])?);
  doc.add(NumericDocValuesField::new("longField", 42i64));

  doc.add(IntRange::new("intRange", &[1], &[10])?);
  doc.add(NumericDocValuesField::new("intRange", 4i64));

  writer.add_document(&mut random, doc)?;
  let reader = writer.get_reader(&mut random)?;
  writer.close(&mut random)?;

  let searcher = new_searcher(reader, random.random_bool(0.5), random.random_bool(0.5))?;

  let mut long_sort_on_int_field = SortField::new("intField".into(), SortFieldType::Long)?;
  assert!(
    searcher
      .search_with_sort(
        MatchAllDocsQuery::new(),
        1,
        Sort::with_fields(vec![long_sort_on_int_field.clone()])?
      )
      .is_err()
  );

  long_sort_on_int_field.set_optimize_sort_with_indexed_data(false);
  searcher.search_with_sort(
    MatchAllDocsQuery::new(),
    1,
    Sort::with_fields(vec![long_sort_on_int_field])?,
  )?;

  let mut int_sort_on_long_field = SortField::new("longField".into(), SortFieldType::Int)?;
  assert!(
    searcher
      .search_with_sort(
        MatchAllDocsQuery::new(),
        1,
        Sort::with_fields(vec![int_sort_on_long_field.clone()])?
      )
      .is_err()
  );

  int_sort_on_long_field.set_optimize_sort_with_indexed_data(false);
  searcher.search_with_sort(
    MatchAllDocsQuery::new(),
    1,
    Sort::with_fields(vec![int_sort_on_long_field])?,
  )?;

  let mut int_sort_on_int_range_field = SortField::new("intRange".into(), SortFieldType::Int)?;
  assert!(
    searcher
      .search_with_sort(
        MatchAllDocsQuery::new(),
        1,
        Sort::with_fields(vec![int_sort_on_int_range_field.clone()])?
      )
      .is_err()
  );

  int_sort_on_int_range_field.set_optimize_sort_with_indexed_data(false);
  searcher.search_with_sort(
    MatchAllDocsQuery::new(),
    1,
    Sort::with_fields(vec![int_sort_on_int_range_field])?,
  )?;

  Ok(())
}

#[test]
fn test_max_doc_visited() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;

  let num_docs = at_least(&mut random, 10_000);
  let offset = 100 + random.random_range(0..100);
  let smallest_value = 50 + random.random_range(0..50);
  let mut flushed = false;

  for i in 0..num_docs {
    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("my_field", (i as i64) + offset));
    doc.add(LongPoint::new("my_field", vec![(i as i64) + offset])?);
    writer.add_document(doc)?;

    if i >= 5000 && !flushed {
      flushed = true;
      writer.flush()?;

      // Index the smallest value to the first slot of the second segment
      let mut doc = Document::new();
      doc.add(NumericDocValuesField::new(
        "my_field",
        smallest_value as i64,
      ));
      doc.add(LongPoint::new("my_field", vec![smallest_value as i64])?);
      writer.add_document(doc)?;
    }
  }

  let reader = directory_reader::open_from_writer(&writer)?;
  writer.close()?;
  let searcher = new_searcher_with_threads(
    &mut random,
    reader,
    random_bool(0.5),
    random_bool(0.5),
    false,
  )?;

  let sort_field = SortField::new(Some("my_field"), SortFieldType::Long)?;
  let sort = Sort::with_fields(vec![sort_field])?;
  let top_docs = searcher.search_with_sort(
    MatchAllDocsQuery::new(),
    1 + random.random_range(0..100),
    sort,
  )?;

  let fd = match &top_docs.score_docs()[0] {
    TopFieldScoreDoc::Field(f) => f,
    _ => return Err(LuceneError::illegal_state("Expected FieldDoc type")),
  };
  let value = *fd.fields[0].as_i64().expect("Expected i64");
  assert_eq!(value, smallest_value as i64);

  Ok(())
}

#[test]
fn test_random_long() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;

  let mut seq_nos: Vec<i64> = Vec::new();

  let limit = if is_night_mode() { 10000 } else { 1000 };
  let iterations = limit + random.random_range(0..limit);
  let mut seq_no_generator = random.random_range(0..1000) as i64;

  for _ in 0..iterations {
    let copies = if random.random_range(0..100) <= 5 {
      1
    } else {
      1 + random.random_range(0..5)
    };

    for _ in 0..copies {
      seq_nos.push(seq_no_generator);
    }

    seq_nos.push(seq_no_generator);
    seq_no_generator += 1;

    if random.random_range(0..100) <= 5 {
      seq_no_generator += random.random_range(0..10) as i64;
    }
  }

  seq_nos.shuffle(&mut random);

  let mut pending_docs = 0;

  for seq_no in &seq_nos {
    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("seq_no", *seq_no));
    doc.add(LongPoint::new("seq_no", vec![*seq_no])?);

    writer.add_document(doc)?;
    pending_docs += 1;

    if pending_docs > 500 && random.random_range(0..100) <= 5 {
      pending_docs = 0;
      writer.flush()?;
    }
  }

  let reverse = random.random_bool(0.5);

  writer.flush()?;

  if !reverse {
    seq_nos.sort();
  } else {
    seq_nos.sort_by(|a, b| b.cmp(a));
  }

  let reader = directory_reader::open_from_writer(&writer)?;
  writer.close()?;
  let may_be_wrap = random.random_bool(0.5);
  let wrap_with_assertions = random.random_bool(0.5);
  let searcher = new_searcher_with_threads(
    &mut random,
    reader,
    may_be_wrap,
    wrap_with_assertions,
    false,
  )?;

  let sort_field = SortField::with_reverse(Some("seq_no"), SortFieldType::Long, reverse)?;
  let mut visited_hits = 0usize;
  let mut after: Option<FieldDoc> = None;

  // test page search
  while visited_hits < seq_nos.len() {
    let batch = 1 + random.random_range(0..100);

    let query: Query = if random.random_bool(0.5) {
      MatchAllDocsQuery::new().into()
    } else {
      LongPoint::new_range_query("seq_no", 0, i64::MAX)?.into()
    };

    let mut top_docs = searcher.search_after(
      after,
      query,
      batch,
      Sort::with_fields(vec![sort_field.clone()])?,
    )?;

    let expected_hits = std::cmp::min(seq_nos.len() - visited_hits, batch);

    assert_eq!(expected_hits, top_docs.score_docs().len());

    after = top_docs.score_docs()[expected_hits - 1]
      .clone()
      .into_field();

    for sd in top_docs.take_score_docs() {
      let field_doc = sd.as_field().unwrap();
      let expected_seq_no = seq_nos[visited_hits];

      assert_eq!(expected_seq_no, *field_doc.fields[0].as_i64().unwrap());

      visited_hits += 1;
    }
  }

  // test search
  let num_hits = 1 + random.random_range(0..100);

  let manager = TopFieldCollectorManager::with_after(
    Sort::with_fields(vec![sort_field])?,
    num_hits,
    None,
    num_hits,
  )?;

  let mut top_docs = searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &manager)?;

  for (i, sd) in top_docs.take_score_docs().iter().enumerate() {
    let expected_seq_no = seq_nos[i];
    let field_doc = sd.as_field().unwrap();

    assert_eq!(expected_seq_no, *field_doc.fields[0].as_i64().unwrap());
  }

  Ok(())
}
#[test]
fn test_sort_optimization_on_sorted_numeric_field() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;

  let num_docs = at_least_usize(&mut random, 5000);
  for _ in 0..num_docs {
    let value = random.random();
    let value2 = random.random();
    let mut doc = Document::new();
    doc.add(LongField::new("my_field", value, Store::No)?);
    doc.add(LongField::new("my_field", value2, Store::No)?);
    writer.add_document(doc)?;
  }

  let reader = directory_reader::open_from_writer(&writer)?;
  writer.close()?;
  let searcher = new_searcher_with_threads(&mut random, reader, true, true, false)?;

  let selector_type = if random.random_bool(0.5) {
    SortedNumericSelectorType::Min
  } else {
    SortedNumericSelectorType::Max
  };
  let reverse = random.random_bool(0.5);

  let mut sort_field = new_sort_field("my_field", reverse, selector_type)?;
  sort_field.base.set_optimize_sort_with_indexed_data(false);
  let sort = Arc::new(Sort::with_fields(vec![sort_field])?);

  let sort_field2 = new_sort_field("my_field", reverse, selector_type)?;
  let sort2 = Arc::new(Sort::with_fields(vec![sort_field2])?);

  let total_hits_threshold = 3;

  let mut expected_collected_hits: i64 = 0;
  let mut collected_hits: i64 = 0;
  let mut collected_hits2: i64 = 0;
  let mut visited_hits = 0;
  let mut after: Option<FieldDoc> = None;

  while visited_hits < num_docs {
    let batch = 1 + random.random_range(0..100);
    let expected_hits = std::cmp::min(num_docs - visited_hits, batch);

    let manager = TopFieldCollectorManager::with_after(
      sort.clone(),
      batch,
      after.clone(),
      total_hits_threshold,
    )?;
    let top_docs = searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &manager)?;
    let score_docs = top_docs.score_docs();

    let manager2 = TopFieldCollectorManager::with_after(
      sort2.clone(),
      batch,
      after.clone(),
      total_hits_threshold,
    )?;
    let top_docs2 = searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &manager2)?;
    let score_docs2 = top_docs2.score_docs();

    assert_eq!(expected_hits, score_docs.len());
    assert_eq!(score_docs.len(), score_docs2.len());

    for i in 0..score_docs.len() {
      let fd1 = match &score_docs[i] {
        TopFieldScoreDoc::Field(f) => f,
        _ => return Err(LuceneError::illegal_state("Expected FieldDoc type")),
      };
      let fd2 = match &score_docs2[i] {
        TopFieldScoreDoc::Field(f) => f,
        _ => return Err(LuceneError::illegal_state("Expected FieldDoc type")),
      };
      assert_eq!(fd1.fields[0], fd2.fields[0]);
      assert_eq!(fd1.doc(), fd2.doc());
      visited_hits += 1;
    }

    expected_collected_hits += num_docs as i64;
    collected_hits += top_docs.total_hits().value as i64;
    collected_hits2 += top_docs2.total_hits().value as i64;

    let last_doc = score_docs[expected_hits - 1].clone();
    match last_doc {
      TopFieldScoreDoc::Field(fd) => after = Some(fd),
      _ => return Err(LuceneError::illegal_state("Expected FieldDoc type")),
    }
  }

  assert_eq!(visited_hits, num_docs);
  assert_eq!(expected_collected_hits, collected_hits);
  assert!(collected_hits >= collected_hits2);

  Ok(())
}
fn assert_non_competitive_hits_are_skipped(collected_hits: i64, num_docs: i64) -> Result<()> {
  if collected_hits >= num_docs {
    return Err(LuceneError::illegal_state(format!(
      "Expected some non-competitive hits are skipped; got collected_hits={} num_docs={}",
      collected_hits, num_docs
    )));
  }
  Ok(())
}

#[test]
fn test_string_sort_optimization() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;
  let num_docs = at_least(&mut random, 10_000);

  for i in 0..num_docs {
    let mut doc = Document::new();
    let value = BytesRef::from_string(&random.random_range(0..1000).to_string());
    doc.add(KeywordField::from_bytes_ref("my_field", value, Store::No)?);
    writer.add_document(doc)?;
    if i % 2000 == 0 {
      writer.flush()?;
    }
  }

  let reader = Arc::new(directory_reader::open_from_writer(&writer)?);
  writer.close()?;

  do_test_string_sort_optimization(&mut random, reader.clone())?;
  do_test_string_sort_optimization_disabled(reader)?;
  Ok(())
}

#[test]
fn test_string_sort_optimization_with_missing_values() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut iwc = IndexWriterConfig::new()?;
  iwc.set_merge_policy(new_log_merge_policy(&mut random)?);
  let writer = IndexWriter::new(dir.clone(), iwc)?;
  let num_docs = at_least(&mut random, 10_000);

  writer.add_document(Document::new())?;

  for i in 0..(num_docs - 2) {
    if i % 2000 == 0 {
      writer.flush()?;
    }
    let mut doc = Document::new();
    if random.random_range(0..2) == 0 {
      let value = BytesRef::from_string(&random.random_range(0..1000).to_string());
      doc.add(KeywordField::from_bytes_ref("my_field", value, Store::No)?);
    }
    writer.add_document(doc)?;
  }

  writer.flush()?;
  writer.add_document(Document::new())?;

  let reader = Arc::new(directory_reader::open_from_writer(&writer)?);
  writer.close()?;
  do_test_string_sort_optimization(&mut random, reader.clone())?;
  Ok(())
}
fn do_test_string_sort_optimization<DR, R>(random: &mut R, reader: DR) -> Result<()>
where
  DR: DirectoryReader + Clone + 'static + Sync,
  R: Rng + ?Sized,
  <DR as CompositeReader>::LeafReader: Send + Sync,
{
  let num_docs = reader.num_docs()?;
  let num_hits = 5;

  {
    let mut sort_field =
      KeywordField::new_sort_field("my_field", false, SortedSetSelectorType::Min)?;
    sort_field.set_missing_value(StringLast)?;
    let sort = Sort::with_fields(vec![sort_field])?;
    let top_docs = assert_sort(random, reader.clone(), sort, num_hits, None)?;
    assert_non_competitive_hits_are_skipped(top_docs.total_hits().value() as i64, num_docs as i64)?;
  }

  {
    let mut sort_field =
      KeywordField::new_sort_field("my_field", true, SortedSetSelectorType::Min)?;
    sort_field.set_missing_value(StringFirst)?;
    let sort = Sort::with_fields(vec![sort_field])?;
    let top_docs = assert_sort(random, reader.clone(), sort, num_hits, None)?;
    assert_non_competitive_hits_are_skipped(top_docs.total_hits().value() as i64, num_docs as i64)?;
  }

  {
    let mut sort_field =
      KeywordField::new_sort_field("my_field", false, SortedSetSelectorType::Min)?;
    sort_field.set_missing_value(StringFirst)?;
    let sort = Sort::with_fields(vec![sort_field])?;
    assert_sort(random, reader.clone(), sort, num_hits, None)?;
  }

  {
    let mut sort_field =
      KeywordField::new_sort_field("my_field", true, SortedSetSelectorType::Min)?;
    sort_field.set_missing_value(StringLast)?;
    let sort = Sort::with_fields(vec![sort_field])?;
    assert_sort(random, reader.clone(), sort, num_hits, None)?;
  }

  {
    let mut sort_field =
      KeywordField::new_sort_field("my_field", false, SortedSetSelectorType::Min)?;
    sort_field.set_missing_value(StringLast)?;
    let sort = Sort::with_fields(vec![sort_field])?;
    let after_value = BytesRef::from_string(if random.random_bool(0.5) {
      "23"
    } else {
      "230000000"
    });
    let after = FieldDoc::with_fields(2, f32::NAN, vec![after_value.into()]);
    let top_docs = assert_sort(random, reader.clone(), sort, num_hits, Some(after))?;
    assert_non_competitive_hits_are_skipped(top_docs.total_hits().value() as i64, num_docs as i64)?;
  }

  {
    let mut sort_field =
      KeywordField::new_sort_field("my_field", true, SortedSetSelectorType::Min)?;
    sort_field.set_missing_value(StringFirst)?;
    let sort = Sort::with_fields(vec![sort_field])?;
    let after_value = BytesRef::from_string(if random.random_bool(0.5) {
      "17"
    } else {
      "170000000"
    });
    let after = FieldDoc::with_fields(2, f32::NAN, vec![after_value.into()]);
    let top_docs = assert_sort(random, reader.clone(), sort, num_hits, Some(after))?;
    assert_non_competitive_hits_are_skipped(top_docs.total_hits().value() as i64, num_docs as i64)?;
  }

  {
    let mut sort_field =
      KeywordField::new_sort_field("my_field", false, SortedSetSelectorType::Min)?;
    sort_field.set_missing_value(StringFirst)?;
    let sort = Sort::with_fields(vec![sort_field])?;
    let after_value = BytesRef::from_string(if random.random_bool(0.5) {
      "23"
    } else {
      "230000000"
    });
    let after = FieldDoc::with_fields(2, f32::NAN, vec![after_value.into()]);
    let top_docs = assert_sort(random, reader.clone(), sort, num_hits, Some(after))?;
    assert_non_competitive_hits_are_skipped(top_docs.total_hits().value() as i64, num_docs as i64)?;
  }

  {
    let mut sort_field =
      KeywordField::new_sort_field("my_field", true, SortedSetSelectorType::Min)?;
    sort_field.set_missing_value(StringLast)?;
    let sort = Sort::with_fields(vec![sort_field])?;
    let after_value = BytesRef::from_string(if random.random_bool(0.5) {
      "17"
    } else {
      "170000000"
    });
    let after = FieldDoc::with_fields(2, f32::NAN, vec![after_value.into()]);
    let top_docs = assert_sort(random, reader.clone(), sort, num_hits, Some(after))?;
    assert_non_competitive_hits_are_skipped(top_docs.total_hits().value() as i64, num_docs as i64)?;
  }

  {
    let mut sort_field =
      KeywordField::new_sort_field("my_field", false, SortedSetSelectorType::Min)?;
    sort_field.set_missing_value(StringLast)?;
    let sort = Sort::with_fields(vec![sort_field, SortField::get_field_score()?.into()])?;
    let top_docs = assert_sort(random, reader.clone(), sort, num_hits, None)?;
    assert_non_competitive_hits_are_skipped(top_docs.total_hits().value() as i64, num_docs as i64)?;
  }

  {
    let mut sort_field =
      KeywordField::new_sort_field("my_field", false, SortedSetSelectorType::Min)?;
    sort_field.set_missing_value(StringLast)?;
    let sort = Sort::with_fields(vec![SortField::get_field_score()?.into(), sort_field])?;
    let top_docs = assert_sort(random, reader, sort, num_hits, None)?;
    assert_eq!(top_docs.total_hits().value() as i64, num_docs as i64);
  }

  Ok(())
}
fn do_test_string_sort_optimization_disabled<DR>(reader: DR) -> Result<()>
where
  DR: DirectoryReader + Clone + 'static + std::marker::Sync,
  <DR as CompositeReader>::LeafReader: std::marker::Sync,
{
  let mut sort_field = KeywordField::new_sort_field("my_field", false, SortedSetSelectorType::Min)?;
  sort_field.set_missing_value(StringLast)?;
  sort_field.set_optimize_sort_with_indexed_data(false);

  let sort = Sort::with_fields(vec![sort_field])?;
  let num_docs = reader.num_docs()?;
  let num_hits = 5;
  let total_hits_threshold = 5;

  let manager = TopFieldCollectorManager::with_after(sort, num_hits, None, total_hits_threshold)?;
  let searcher = new_searcher_with_reader(reader)?;
  let top_docs = searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &manager)?;

  assert_eq!(num_docs as usize, top_docs.total_hits().value());
  Ok(())
}
fn assert_sort<R, DR>(
  random: &mut R,
  reader: DR,
  sort: Sort,
  n: usize,
  after: Option<FieldDoc>,
) -> Result<TopFieldDocs>
where
  DR: DirectoryReader + Clone + 'static + Sync,
  R: Rng + ?Sized,
  <DR as CompositeReader>::LeafReader: Send + Sync,
{
  let top_docs = assert_search_hits(random, reader.clone(), sort.clone(), n, after.clone())?;
  let mut sort_fields = sort.get_sort().to_vec();
  // A secondary sort on reverse doc ID is the best way to catch bugs if the comparator filters
  // too aggressively
  sort_fields.push(SortField::with_reverse::<String>(None, SortFieldType::Doc, true)?.into());

  let after2 = match after {
    Some(after) => {
      let mut after_fields = after.fields.clone();
      after_fields.push(i32::MAX.into());
      Some(FieldDoc::with_fields(
        after.doc(),
        after.score(),
        after_fields,
      ))
    },
    None => None,
  };

  assert_search_hits(random, reader, Sort::with_fields(sort_fields)?, n, after2)?;
  Ok(top_docs)
}

fn assert_search_hits<R, DR>(
  random: &mut R,
  reader: DR,
  sort: Sort,
  n: usize,
  after: Option<FieldDoc>,
) -> Result<TopFieldDocs>
where
  DR: DirectoryReader + Clone + 'static + std::marker::Sync,
  R: Rng + ?Sized,
  <DR as CompositeReader>::LeafReader: Send + Sync,
{
  let searcher = new_searcher_with_reader(reader.clone())?;
  let query = MatchAllDocsQuery::new();
  let manager = TopFieldCollectorManager::with_after(sort.clone(), n, after.clone(), n)?;
  let top_docs = searcher.search_with_collector_manager(query.clone(), &manager)?;

  let unoptimized_reader = NoIndexDirectoryReader::new(reader)?;
  let unoptimized_searcher =
    new_searcher_with_threads(random, unoptimized_reader, true, true, false)?;
  let unoptimized_top_docs =
    unoptimized_searcher.search_with_collector_manager(query.clone(), &manager)?;

  CheckHits::check_equal(
    &query.into(),
    unoptimized_top_docs.score_docs(),
    top_docs.score_docs(),
  )?;
  Ok(top_docs)
}
#[derive(Default)]
pub struct SubReaderWrapperImpl;
impl<LR> SubReaderWrapper<LR> for SubReaderWrapperImpl
where
  LR: LeafReader,
{
  type LeafReader1 = Self::LeafReader2;

  fn wrap_readers(&self, readers: Vec<LR>) -> Result<Vec<Self::LeafReader1>> {
    self.default_wrap_readers(readers)
  }

  type LeafReader2 = NoIndexLeafReader<LR>;

  fn wrap(&self, reader: LR) -> Result<Self::LeafReader2> {
    Ok(NoIndexLeafReader::new(reader))
  }
}
pub struct NoIndexLeafReader<LR>
where
  LR: LeafReader,
{
  in_: LR,
}
impl<LR> NoIndexLeafReader<LR>
where
  LR: LeafReader,
{
  pub fn new(in_: LR) -> Self {
    Self { in_ }
  }
}

impl<LR> IndexReader for NoIndexLeafReader<LR>
where
  LR: LeafReader,
{
  type ContextKind = LeafReaderContextKind;

  type TermVectors = LR::TermVectors;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    self.ensure_open()?;
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
    self.ensure_open()?;
    self.in_.stored_fields()
  }

  type ReaderCacheHelper = DummyCacheHelper;

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
    self.in_.index_base()
  }
}

impl<LR> Display for NoIndexLeafReader<LR>
where
  LR: LeafReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl<LR> LeafReader for NoIndexLeafReader<LR>
where
  LR: LeafReader,
{
  type CacheHelper = DummyCacheHelper;

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    Ok(None)
  }

  type Terms = DummyTerms;

  fn terms(&self, _field: &str) -> Result<Option<Self::Terms>> {
    Ok(None)
  }

  type NumericDocValues = LR::NumericDocValues;

  fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>> {
    self.ensure_open()?;
    self.in_.get_numeric_doc_values(field)
  }

  type BinaryDocValues = LR::BinaryDocValues;

  fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
    self.ensure_open()?;
    self.in_.get_binary_doc_values(field)
  }

  type SortedDocValues = LR::SortedDocValues;

  fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
    self.ensure_open()?;
    self.in_.get_sorted_doc_values(field)
  }

  type SortedNumericDocValues = LR::SortedNumericDocValues;

  fn get_sorted_numeric_doc_values(
    &self,
    field: &str,
  ) -> Result<Option<Self::SortedNumericDocValues>> {
    self.ensure_open()?;
    self.in_.get_sorted_numeric_doc_values(field)
  }

  type SortedSetDocValues = LR::SortedSetDocValues;

  fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>> {
    self.ensure_open()?;
    self.in_.get_sorted_set_doc_values(field)
  }

  type NormNumericDocValues = LR::NormNumericDocValues;

  fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>> {
    self.ensure_open()?;
    self.in_.get_norm_values(field)
  }

  type DocValuesSkipper = LR::DocValuesSkipper;

  fn get_doc_values_skipper(&self, field: &str) -> Result<Option<Self::DocValuesSkipper>> {
    self.ensure_open()?;
    self.in_.get_doc_values_skipper(field)
  }

  type FloatVectorValues = LR::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Option<Self::FloatVectorValues>> {
    self.ensure_open()?;
    self.in_.get_float_vector_values(field)
  }

  type ByteVectorValues = LR::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Option<Self::ByteVectorValues>> {
    self.ensure_open()?;
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
    let mut new_infos = Vec::with_capacity(self.in_.get_field_infos()?.size());

    for fi in self.in_.get_field_infos()?.iter() {
      let attributes = fi.attributes().lock().attributes.clone();
      let no_index_fi = FieldInfo::new(
        fi.name.clone(),
        fi.number,
        false,
        false,
        false,
        IndexOptions::None,
        *fi.get_doc_values_type(),
        *fi.doc_values_skip_index_type(),
        fi.get_doc_values_gen(),
        attributes,
        0,
        0,
        0,
        0,
        VectorEncoding::FLOAT32(4),
        VectorSimilarityFunction::DotProduct,
        fi.is_soft_deletes_field(),
        fi.is_parent_field(),
      )?;
      new_infos.push(Arc::new(no_index_fi));
    }
    Ok(Arc::new(FieldInfos::new(new_infos)?))
  }

  type Bits = LR::Bits;

  fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
    self.in_.get_live_docs()
  }

  type PointValues = DummyPointValues;

  fn get_point_values(&self, _field: &str) -> Result<Option<Self::PointValues>> {
    Ok(None)
  }

  fn get_metadata(&self) -> Result<&LeafMetaData> {
    self.in_.get_metadata()
  }
}

pub struct NoIndexDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  in_: DR,
  base: BaseCompositeReaderBase<NoIndexLeafReader<DR::LeafReader>>,
}
impl<DR> NoIndexDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  pub fn new(in_: DR) -> Result<Self> {
    let wrap = SubReaderWrapperImpl;
    let leaf_reads = in_.get_sequential_sub_readers().to_vec();
    let wrap_readers = wrap.wrap_readers(leaf_reads)?;
    let base_composite_reader_base: BaseCompositeReaderBase<NoIndexLeafReader<_>> =
      BaseCompositeReaderBase::new::<DummyComparator>(wrap_readers, None)?;
    Ok(Self {
      in_,
      base: base_composite_reader_base,
    })
  }
}

impl<DR> DirectoryReader for NoIndexDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  type DirectoryReader = DR::DirectoryReader;

  fn do_open_if_changed(&self) -> Result<Option<Self::DirectoryReader>> {
    let v = self.in_.do_open_if_changed()?;
    self.wrap_directory_reader(v)
  }

  fn do_open_if_changed_with_commit<IC>(
    &self,
    commit: Option<&IC>,
  ) -> Result<Option<Self::DirectoryReader>>
  where
    IC: crate::core::index::index_commit::IndexCommit<Directory = Arc<Self::Directory>>,
  {
    let v = self.in_.do_open_if_changed_with_commit(commit)?;
    self.wrap_directory_reader(v)
  }

  fn do_open_if_changed_with_deletes(
    &self,
    writer: &Arc<IndexWriter<Self::Directory>>,
    apply_deletes: bool,
  ) -> Result<Option<Self::DirectoryReader>> {
    let v = self
      .in_
      .do_open_if_changed_with_deletes(writer, apply_deletes)?;
    self.wrap_directory_reader(v)
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

impl<DR> BaseCompositeReader for NoIndexDirectoryReader<DR> where DR: DirectoryReader {}

impl<DR> CompositeReader for NoIndexDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  type LeafReader = DR::LeafReader;
  type SubReader = DR::SubReader;

  fn get_sequential_sub_readers(&self) -> &[Self::SubReader] {
    self.in_.get_sequential_sub_readers()
  }

  fn visit_leaves<F>(&self, visitor: &mut F) -> Result<()>
  where
    F: FnMut(&Self::LeafReader) -> Result<()>,
  {
    self.in_.visit_leaves(visitor)
  }
}

impl<DR> IndexReader for NoIndexDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  type ContextKind = CompositeReaderContextKind;

  type TermVectors = DR::TermVectors;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    self.in_.term_vectors()
  }

  fn max_doc(&self) -> Result<i32> {
    self.in_.max_doc()
  }

  fn num_docs(&self) -> Result<i32> {
    self.in_.num_docs()
  }

  type StoredFields = DR::StoredFields;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    self.in_.stored_fields()
  }

  type ReaderCacheHelper = DR::ReaderCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    self.in_.get_reader_cache_helper()
  }

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    self.in_.doc_freq(term)
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    self.in_.total_term_freq(term)
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    self.in_.get_sum_doc_freq(field)
  }

  fn get_doc_count(&self, field: &str) -> Result<i32> {
    self.in_.get_doc_count(field)
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    self.in_.get_sum_total_term_freq(field)
  }

  fn index_base(&self) -> &IndexReaderBase {
    self.in_.index_base()
  }
}

impl<DR> Display for NoIndexDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl<DR> FilterDirectoryReader for NoIndexDirectoryReader<DR>
where
  DR: DirectoryReader,
{
  type Delegate = DR;

  fn get_delegate(&self) -> &Self::Delegate {
    &self.in_
  }

  type WrapDirectoryReader = DR::DirectoryReader;

  fn do_wrap_directory_reader(
    &self,
    _in_: Option<<Self::Delegate as DirectoryReader>::DirectoryReader>,
  ) -> Result<Option<Self::WrapDirectoryReader>> {
    Err(LuceneError::unsupported_operation(""))
  }
}
