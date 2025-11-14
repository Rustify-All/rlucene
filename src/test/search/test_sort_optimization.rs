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
use crate::core::document::long_field::{LongField, long_field_type};
use crate::core::document::long_point::LongPoint;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::stored_field::StoredField;
use crate::core::document::string_field::StringField;
use crate::core::index::directory_reader::directory_reader_util;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::sort::Sort;
use crate::core::search::field_comparator::FieldComparatorValue;
use crate::core::search::field_doc::FieldDoc;
use crate::core::search::field_value_hit_queue::TopFieldScoreDoc;
use crate::core::search::match_all_docs_query::MatchAllDocsQuery;
use crate::core::search::score_doc::ScoreDocLike;
use crate::core::search::sort_field::{SortField, SortFieldType, SortFiledBase};
use crate::core::search::sorted_numeric_selector::SortedNumericSelectorType;
use crate::core::search::top_docs::{TopDocsLike, top_docs_util};
use crate::core::search::top_field_collector_manager::TopFieldCollectorManager;
use crate::core::search::total_hits::Relation;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test::util::lucene_test_case::lucene_test_case_util::{
    at_least, is_night_mode, new_directory, new_searcher, new_searcher_with_reader, random,
};
use rand::{Rng, random_bool};
use std::sync::Arc;

#[allow(dead_code)]
struct TestSortOptimization;

#[test]
fn test_long_sort_optimization() -> Result<()> {
    let mut random = random();
    let dir = Arc::new(new_directory(&mut random)?);

    let config = IndexWriterConfig::new();
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

    let reader = Arc::new(directory_reader_util::open_with_writer(&writer)?);
    writer.close()?;
    let mut searcher = new_searcher(reader, true, true, false)?;
    let sort_field = SortField::new(Some("my_field"), SortFieldType::Long)?;
    let sort = Arc::new(Sort::with_fields(vec![sort_field])?);
    let num_hits = 3;
    let total_hits_threshold = 3;
    // simple sort
    {
        let collector_manager =
            TopFieldCollectorManager::new(sort.clone(), num_hits, total_hits_threshold)?;

        let top_docs =
            searcher.search_with_collector_manager(MatchAllDocsQuery, &collector_manager)?;

        assert_eq!(top_docs.score_docs().len(), num_hits as usize);

        for i in 0..num_hits {
            let field_doc = &top_docs.score_docs()[i as usize];
            let fields = &field_doc.fields()?[0];
            let value = *fields.as_i64().expect("should be i64");
            assert_eq!(i as i64, value);
        }

        assert_eq!(
            top_docs.total_hits().relation,
            Relation::GreaterThanOrEqualTo
        );

        assert_non_competitive_hits_are_skipped(
            top_docs.total_hits().value as i64,
            num_docs as i64,
        )?;
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
            searcher.search_with_collector_manager(MatchAllDocsQuery, &collector_manager)?;

        assert_eq!(top_docs.score_docs().len(), num_hits as usize);

        for i in 0..num_hits {
            let field_doc = &top_docs.score_docs()[i as usize];
            let fields = &field_doc.fields()?[0];
            let value = *fields.as_i64().expect("should be i64");
            assert_eq!(after_value + 1 + i as i64, value);
        }

        assert_eq!(
            top_docs.total_hits().relation,
            Relation::GreaterThanOrEqualTo
        );

        assert_non_competitive_hits_are_skipped(
            top_docs.total_hits().value as i64,
            num_docs as i64,
        )?;
    }
    // test that if there is the secondary sort on _score, scores are filled correctly
    {
        let sort_field = SortField::new(Some("my_field"), SortFieldType::Long)?;
        let sort = Sort::with_fields(vec![sort_field, SortField::get_field_score()?])?;

        let collector_manager =
            TopFieldCollectorManager::new(sort, num_hits, total_hits_threshold)?;

        let top_docs =
            searcher.search_with_collector_manager(MatchAllDocsQuery, &collector_manager)?;

        assert_eq!(top_docs.score_docs().len(), num_hits as usize);

        for i in 0..num_hits {
            let field_doc = &top_docs.score_docs()[i as usize];
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

        assert_non_competitive_hits_are_skipped(
            top_docs.total_hits().value as i64,
            num_docs as i64,
        )?;
    }
    // test that if numeric field is a secondary sort, no optimization is run
    {
        let sort_field = SortField::new(Some("my_field"), SortFieldType::Long)?;
        let sort = Sort::with_fields(vec![SortField::get_field_score()?, sort_field])?;

        let collector_manager =
            TopFieldCollectorManager::new(sort, num_hits, total_hits_threshold)?;

        let top_docs =
            searcher.search_with_collector_manager(MatchAllDocsQuery, &collector_manager)?;

        assert_eq!(top_docs.score_docs().len(), num_hits as usize);
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
    let dir = Arc::new(new_directory(&mut random)?);
    let writer = IndexWriter::new(dir.clone(), IndexWriterConfig::new())?;

    let num_docs = at_least(&mut random, 100);
    // "my_field" is not indexed with points
    for i in 0..num_docs {
        let mut doc = Document::new();
        doc.add(NumericDocValuesField::new("my_field", i as i64));
        writer.add_document(doc)?;
    }

    let reader = Arc::new(directory_reader_util::open_with_writer(&writer)?);
    writer.close()?;

    // single-threaded so totalHits is deterministic
    let mut searcher = new_searcher(reader, random.random_bool(0.5), random_bool(0.5), false)?;
    let sort_field = SortField::new(Some("my_field"), SortFieldType::Long)?;
    let sort = Sort::with_fields(vec![sort_field])?;
    let num_hits = 3;
    let total_hits_threshold = 3;

    let collector_manager = TopFieldCollectorManager::new(sort, num_hits, total_hits_threshold)?;
    let top_docs = searcher.search_with_collector_manager(MatchAllDocsQuery, &collector_manager)?;

    // sort still works and returns expected number of docs
    assert_eq!(top_docs.score_docs().len(), num_hits as usize);

    // returns expected values
    for i in 0..num_hits {
        let field_doc = &top_docs.score_docs()[i as usize];
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
    let dir = Arc::new(new_directory(&mut random)?);

    let config = IndexWriterConfig::new();
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

    let reader = Arc::new(directory_reader_util::open_with_writer(&writer)?);
    writer.close()?;
    let mut searcher = new_searcher(reader, random_bool(0.5), random_bool(0.5), false)?;
    let num_hits = 3;
    let total_hits_threshold = 3;

    {
        let mut sort_field = SortField::new(Some("my_field"), SortFieldType::Long)?;
        sort_field.set_missing_value(0i64)?;
        let sort = Sort::with_fields(vec![sort_field])?;
        let collector_manager =
            TopFieldCollectorManager::new(sort, num_hits, total_hits_threshold)?;
        let top_docs =
            searcher.search_with_collector_manager(MatchAllDocsQuery, &collector_manager)?;
        assert_eq!(top_docs.score_docs().len(), num_hits as usize);
        assert_non_competitive_hits_are_skipped(
            top_docs.total_hits().value as i64,
            num_docs as i64,
        )?;
    }

    {
        let mut sf1 = SortField::new(Some("my_field1"), SortFieldType::Long)?;
        let mut sf2 = SortField::new(Some("my_field2"), SortFieldType::Long)?;
        sf1.set_missing_value(0i64)?;
        sf2.set_missing_value(0i64)?;
        let sort = Sort::with_fields(vec![sf1, sf2])?;
        let collector_manager =
            TopFieldCollectorManager::new(sort, num_hits, total_hits_threshold)?;
        let top_docs =
            searcher.search_with_collector_manager(MatchAllDocsQuery, &collector_manager)?;
        assert_eq!(top_docs.score_docs().len(), num_hits as usize);
        assert_eq!(top_docs.total_hits().value as i32, num_docs as i32);
    }

    {
        let mut sort_field = SortField::new(Some("my_field"), SortFieldType::Long)?;
        sort_field.set_missing_value(100i64)?;
        let sort = Sort::with_fields(vec![sort_field])?;
        let collector_manager =
            TopFieldCollectorManager::new(sort, num_hits, total_hits_threshold)?;
        let top_docs =
            searcher.search_with_collector_manager(MatchAllDocsQuery, &collector_manager)?;
        assert_eq!(top_docs.score_docs().len(), num_hits as usize);
        assert_non_competitive_hits_are_skipped(
            top_docs.total_hits().value as i64,
            num_docs as i64,
        )?;
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
        let collector_manager = TopFieldCollectorManager::with_after(
            sort,
            num_hits,
            Some(after),
            total_hits_threshold,
        )?;
        let top_docs =
            searcher.search_with_collector_manager(MatchAllDocsQuery, &collector_manager)?;
        assert_eq!(top_docs.score_docs().len(), num_hits as usize);
        assert_non_competitive_hits_are_skipped(
            top_docs.total_hits().value as i64,
            num_docs as i64,
        )?;
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
        let collector_manager = TopFieldCollectorManager::with_after(
            sort,
            num_hits,
            Some(after),
            total_hits_threshold,
        )?;
        let top_docs =
            searcher.search_with_collector_manager(MatchAllDocsQuery, &collector_manager)?;
        assert_eq!(top_docs.score_docs().len(), num_hits as usize);
        assert_non_competitive_hits_are_skipped(
            top_docs.total_hits().value as i64,
            num_docs as i64,
        )?;
    }

    {
        let after_value: i64 = 3;
        let after = FieldDoc::with_fields(3, f32::NAN, vec![after_value.into()]);
        let mut sort_field = SortField::new(Some("my_field"), SortFieldType::Long)?;
        sort_field.set_missing_value(2i64)?;
        let sort = Sort::with_fields(vec![sort_field])?;
        let collector_manager = TopFieldCollectorManager::with_after(
            sort,
            num_hits,
            Some(after),
            total_hits_threshold,
        )?;
        let top_docs =
            searcher.search_with_collector_manager(MatchAllDocsQuery, &collector_manager)?;
        assert_eq!(top_docs.score_docs().len(), num_hits as usize);

        for i in 0..num_hits {
            let field_doc = &top_docs.score_docs()[i as usize];
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
    let dir = Arc::new(new_directory(&mut random)?);

    let config = IndexWriterConfig::new();
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

    let reader = Arc::new(directory_reader_util::open_with_writer(&writer)?);
    writer.close()?;
    let mut searcher = new_searcher(reader, random_bool(0.5), random_bool(0.5), false)?;
    let num_hits = 3;
    let total_hits_threshold = 3;

    let top_docs1;
    let top_docs2;

    {
        let mut sort_field = SortField::with_reverse(Some("my_field"), SortFieldType::Long, true)?;
        sort_field.set_missing_value(0i64)?;
        let sort = Sort::with_fields(vec![sort_field])?;

        let collector_manager =
            TopFieldCollectorManager::new(sort, num_hits, total_hits_threshold)?;
        top_docs1 =
            searcher.search_with_collector_manager(MatchAllDocsQuery, &collector_manager)?;
        assert_non_competitive_hits_are_skipped(
            top_docs1.total_hits().value as i64,
            num_docs as i64,
        )?;
    }

    {
        let mut sort_field = SortField::with_reverse(Some("my_field"), SortFieldType::Long, true)?;
        sort_field.set_missing_value(0i64)?;
        sort_field.set_optimize_sort_with_points(false);
        let sort = Sort::with_fields(vec![sort_field])?;

        let collector_manager =
            TopFieldCollectorManager::new(sort, num_hits, total_hits_threshold)?;
        top_docs2 =
            searcher.search_with_collector_manager(MatchAllDocsQuery, &collector_manager)?;

        assert_eq!(top_docs1.score_docs().len(), top_docs2.score_docs().len());
        assert_eq!(top_docs1.score_docs().len(), num_hits as usize);

        for i in 0..num_hits {
            let fd1 = &top_docs1.score_docs()[i as usize];
            let fd2 = &top_docs2.score_docs()[i as usize];
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

        let collector_manager =
            TopFieldCollectorManager::new(sort, num_hits, total_hits_threshold)?;
        let top_docs =
            searcher.search_with_collector_manager(MatchAllDocsQuery, &collector_manager)?;
        assert_eq!(top_docs.total_hits().value as i32, num_docs as i32);
    }

    Ok(())
}
#[test]
fn test_sort_optimization_equal_values() -> Result<()> {
    let mut random = random();
    let dir = Arc::new(new_directory(&mut random)?);
    let config = IndexWriterConfig::new();
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

    let reader = Arc::new(directory_reader_util::open_with_writer(&writer)?);
    writer.close()?;

    let mut searcher = new_searcher(reader.clone(), random_bool(0.5), random_bool(0.5), false)?;
    let num_hits = 3;
    let total_hits_threshold = 3;

    {
        let sort_field = SortField::new(Some("my_field1"), SortFieldType::Int)?;
        let sort = Sort::with_fields(vec![sort_field])?;
        let collector_manager =
            TopFieldCollectorManager::new(sort, num_hits, total_hits_threshold)?;
        let top_docs =
            searcher.search_with_collector_manager(MatchAllDocsQuery, &collector_manager)?;

        assert_eq!(top_docs.score_docs().len(), num_hits as usize);

        for i in 0..num_hits {
            let fd = &top_docs.score_docs()[i as usize];
            let fields = fd.fields()?;
            assert_eq!(*fields[0].as_i32().unwrap(), 100);
        }

        if searcher.reader_context.leaves()?.len() == 1 {
            assert_eq!(top_docs.total_hits().value as i32, num_hits + 1);
        }

        assert_non_competitive_hits_are_skipped(
            top_docs.total_hits().value as i64,
            num_docs as i64,
        )?;
    }

    {
        let after_value = 100_i32;
        let after_doc_id = 10 + random.random_range(0..1000);
        let sort_field = SortField::new(Some("my_field1"), SortFieldType::Int)?;
        let sort = Sort::with_fields(vec![sort_field])?;
        let after = FieldDoc::with_fields(after_doc_id, f32::NAN, vec![after_value.into()]);
        let collector_manager = TopFieldCollectorManager::with_after(
            sort,
            num_hits,
            Some(after),
            total_hits_threshold,
        )?;
        let top_docs =
            searcher.search_with_collector_manager(MatchAllDocsQuery, &collector_manager)?;

        assert_eq!(top_docs.score_docs().len(), num_hits as usize);
        for i in 0..num_hits {
            let fd = &top_docs.score_docs()[i as usize];
            let fields = fd.fields()?;
            assert_eq!(*fields[0].as_i32().unwrap(), 100);
            assert!(fd.doc() > after_doc_id);
        }

        assert_non_competitive_hits_are_skipped(
            top_docs.total_hits().value as i64,
            num_docs as i64,
        )?;
    }

    {
        let sf1 = SortField::new(Some("my_field1"), SortFieldType::Int)?;
        let sf2 = SortField::new(Some("my_field2"), SortFieldType::Int)?;
        let sort = Sort::with_fields(vec![sf1, sf2])?;
        let collector_manager =
            TopFieldCollectorManager::new(sort, num_hits, total_hits_threshold)?;
        let top_docs =
            searcher.search_with_collector_manager(MatchAllDocsQuery, &collector_manager)?;

        assert_eq!(top_docs.score_docs().len(), num_hits as usize);

        for i in 0..num_hits {
            let fd = &top_docs.score_docs()[i as usize];
            let fields = fd.fields()?;
            assert_eq!(*fields[0].as_i32().unwrap(), 100);
            assert_eq!(*fields[1].as_i32().unwrap(), i);
        }

        assert_eq!(top_docs.total_hits().value as i32, num_docs);
    }

    Ok(())
}
#[test]
fn test_float_sort_optimization() -> Result<()> {
    let mut random = random();
    let dir = Arc::new(new_directory(&mut random)?);
    let config = IndexWriterConfig::new();
    let writer = IndexWriter::new(dir.clone(), config)?;

    let num_docs = at_least(&mut random, 10_000);
    for i in 0..num_docs {
        let mut doc = Document::new();
        let f = i as f32;
        doc.add(FloatDocValuesField::new("my_field", f));
        doc.add(FloatPoint::new("my_field", vec![f])?);
        writer.add_document(doc)?;
    }

    let reader = Arc::new(directory_reader_util::open_with_writer(&writer)?);
    writer.close()?;

    let mut searcher = new_searcher(reader, random_bool(0.5), random_bool(0.5), false)?;
    let sort_field = SortField::new(Some("my_field"), SortFieldType::Float)?;
    let sort = Sort::with_fields(vec![sort_field])?;
    let num_hits = 3;
    let total_hits_threshold = 3;

    {
        let collector_manager =
            TopFieldCollectorManager::new(sort, num_hits, total_hits_threshold)?;
        let top_docs =
            searcher.search_with_collector_manager(MatchAllDocsQuery, &collector_manager)?;

        assert_eq!(top_docs.score_docs().len(), num_hits as usize);

        for i in 0..num_hits {
            let fd = &top_docs.score_docs()[i as usize];
            let fields = fd.fields()?;
            let v = *fields[0].as_f32().expect("should be f32");
            assert!((v - i as f32).abs() < f32::EPSILON);
        }

        assert_eq!(
            top_docs.total_hits().relation,
            Relation::GreaterThanOrEqualTo
        );

        assert_non_competitive_hits_are_skipped(
            top_docs.total_hits().value as i64,
            num_docs as i64,
        )?;
    }

    Ok(())
}

#[test]
fn test_doc_sort_optimization_multiple_indices() -> Result<()> {
    let mut random = random();
    let num_indices = 3;
    let num_docs_in_index = at_least(&mut random, 50);

    let mut readers = Vec::with_capacity(num_indices);

    for i in 0..num_indices {
        let dir = Arc::new(new_directory(&mut random)?);
        let config = IndexWriterConfig::new();
        let writer = IndexWriter::new(dir.clone(), config)?;
        for doc_id in 0..num_docs_in_index {
            let mut doc = Document::new();
            doc.add(NumericDocValuesField::new(
                "my_field",
                (doc_id as usize * num_indices + i) as i64,
            ));
            writer.add_document(doc)?;
        }
        writer.flush()?;
        writer.close()?;
        let reader = Arc::new(directory_reader_util::open(dir.clone())?);
        readers.push(reader);
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
    let mut total_docs: i64 = 0;
    let mut num_hits = 0;

    loop {
        let mut top_docs_vec = Vec::new();
        for i in 0..num_indices {
            let mut searcher = new_searcher(
                readers[i].clone(),
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
                searcher.search_with_collector_manager(MatchAllDocsQuery, &collector_manager)?;
            for doc_id in 0..top_docs.base.score_docs.len() {
                top_docs.score_docs_mut()[doc_id].set_shard_index(i as i32)
            }
            collected_docs += top_docs.total_hits().value as i64;
            total_docs += num_docs_in_index as i64;
            top_docs_vec.push(top_docs.base)
        }

        let mut merged_top_docs =
            top_docs_util::merge_top_field_docs(sort.as_ref(), size, top_docs_vec)?;
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
    let expected_num_hits = num_docs_in_index as usize * num_indices;
    assert_eq!(expected_num_hits, num_hits);
    assert_non_competitive_hits_are_skipped(collected_docs, total_docs)?;

    Ok(())
}
#[test]
fn test_doc_sort_optimization_with_after() -> Result<()> {
    let mut random = random();
    let dir = Arc::new(new_directory(&mut random)?);
    let writer = IndexWriter::new(dir.clone(), IndexWriterConfig::new())?;

    let num_docs = at_least(&mut random, 150);
    for i in 0..num_docs {
        let doc = Document::new();
        writer.add_document(doc)?;
        if i > 0 && i % 50 == 0 {
            writer.flush()?;
        }
    }

    let reader = Arc::new(directory_reader_util::open_with_writer(&writer)?);
    writer.close()?;
    let mut searcher = new_searcher(reader, random_bool(0.5), random_bool(0.5), false)?;
    let num_hits = 10;
    let total_hits_threshold = 10;
    let search_afters = [3, 10, num_docs - 10];

    for &search_after in &search_afters {
        {
            let sort = Sort::with_fields(vec![SortField::get_field_doc()?])?;
            let after = FieldDoc::with_fields(
                search_after,
                f32::NAN,
                vec![FieldComparatorValue::Int(search_after)],
            );
            let collector_manager = TopFieldCollectorManager::with_after(
                sort,
                num_hits,
                Some(after),
                total_hits_threshold,
            )?;
            let top_docs =
                searcher.search_with_collector_manager(MatchAllDocsQuery, &collector_manager)?;
            let exp_num_hits = if search_after >= (num_docs - num_hits) {
                num_docs - search_after - 1
            } else {
                num_hits
            };
            assert_eq!(exp_num_hits as usize, top_docs.score_docs().len());
            for (i, sd) in top_docs.score_docs().iter().enumerate() {
                assert_eq!(search_after + 1 + i as i32, sd.doc());
            }
            assert_eq!(
                top_docs.total_hits().relation,
                Relation::GreaterThanOrEqualTo
            );
            assert_non_competitive_hits_are_skipped(
                top_docs.total_hits().value as i64,
                num_docs as i64,
            )?;
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
            let collector_manager = TopFieldCollectorManager::with_after(
                sort,
                num_hits,
                Some(after),
                total_hits_threshold,
            )?;
            let top_docs =
                searcher.search_with_collector_manager(MatchAllDocsQuery, &collector_manager)?;
            let exp_num_hits = if search_after >= (num_docs - num_hits) {
                num_docs - search_after - 1
            } else {
                num_hits
            };
            assert_eq!(exp_num_hits as usize, top_docs.score_docs().len());
            for (i, sd) in top_docs.score_docs().iter().enumerate() {
                assert_eq!(search_after + 1 + i as i32, sd.doc());
            }
            assert_eq!(
                top_docs.total_hits().relation,
                Relation::GreaterThanOrEqualTo
            );
            assert_non_competitive_hits_are_skipped(
                top_docs.total_hits().value as i64,
                num_docs as i64,
            )?;
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
            let collector_manager = TopFieldCollectorManager::with_after(
                sort,
                num_hits,
                Some(after),
                total_hits_threshold,
            )?;
            let top_docs =
                searcher.search_with_collector_manager(MatchAllDocsQuery, &collector_manager)?;
            let exp_num_hits = if search_after < num_hits {
                search_after
            } else {
                num_hits
            };
            assert_eq!(exp_num_hits as usize, top_docs.score_docs().len());
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
    let dir = Arc::new(new_directory(&mut random)?);
    let writer = IndexWriter::new(dir.clone(), IndexWriterConfig::new())?;

    let num_docs = if is_night_mode() {
        at_least(&mut random, 50_000)
    } else {
        at_least(&mut random, 5_000)
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

    let reader = Arc::new(directory_reader_util::open_with_writer(&writer)?);
    let mut searcher = new_searcher_with_reader(reader.clone())?;
    writer.close()?;

    let mut visited_hits = 0;
    let mut after: Option<FieldDoc> = None;

    while visited_hits < num_docs {
        let batch = 1 + random.random_range(0..500);
        let sort = Sort::with_fields(vec![SortField::get_field_doc()?])?;
        let top_docs = searcher.search_after(after, MatchAllDocsQuery, batch, sort)?;

        let expected_hits = std::cmp::min(num_docs - visited_hits, batch);
        assert_eq!(expected_hits as usize, top_docs.score_docs().len());

        let last_doc = top_docs.score_docs()[expected_hits as usize - 1].clone();
        match last_doc {
            TopFieldScoreDoc::Field(field_doc) => after = Some(field_doc),
            _ => {
                return Err(LuceneError::illegal_state(
                    "Expected FieldDoc type in TopFieldScoreDoc",
                ));
            },
        }

        for sd in top_docs.score_docs() {
            assert_eq!(visited_hits, sd.doc());
            visited_hits += 1;
        }
    }

    assert_eq!(visited_hits, num_docs);
    Ok(())
}
#[test]
fn test_doc_sort_optimization() -> Result<()> {
    let mut random = random();
    let dir = Arc::new(new_directory(&mut random)?);
    let writer = IndexWriter::new(dir.clone(), IndexWriterConfig::new())?;

    let num_docs = at_least(&mut random, 100);
    let mut seg = 1;
    for i in 0..num_docs {
        let mut doc = Document::new();
        doc.add(LongPoint::new("lf", vec![i as i64])?);
        doc.add(StoredField::with_i32("slf", i)?);
        doc.add(StringField::with_string(
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

    let reader = Arc::new(directory_reader_util::open_with_writer(&writer)?);
    writer.close()?;
    let mut searcher = new_searcher(reader.clone(), random_bool(0.5), random_bool(0.5), false)?;

    let num_hits = 3;
    let total_hits_threshold = 3;
    let sort = Sort::with_fields(vec![SortField::get_field_doc()?])?;

    // sort by _doc should skip all non-competitive documents
    {
        let collector_manager =
            TopFieldCollectorManager::new(sort, num_hits, total_hits_threshold)?;
        let top_docs =
            searcher.search_with_collector_manager(MatchAllDocsQuery, &collector_manager)?;

        assert_eq!(num_hits as usize, top_docs.score_docs().len());
        for (i, sd) in top_docs.score_docs().iter().enumerate() {
            assert_eq!(i as i32, sd.doc());
        }

        assert_eq!(
            top_docs.total_hits().relation,
            Relation::GreaterThanOrEqualTo
        );
        assert_non_competitive_hits_are_skipped(top_docs.total_hits().value as i64, 10)?;
    }
    // TODO 未实现BooleanQuery
    Ok(())
}
#[test]
fn test_doc_sort() -> Result<()> {
    // TODO 未实现BooleanQuery
    Ok(())
}

#[test]
fn test_point_validation() -> Result<()> {
    // TODO 为实现IntRange
    Ok(())
}
#[test]
fn test_max_doc_visited() -> Result<()> {
    let mut random = random();
    let dir = Arc::new(new_directory(&mut random)?);
    let writer = IndexWriter::new(dir.clone(), IndexWriterConfig::new())?;

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

    let reader = Arc::new(directory_reader_util::open_with_writer(&writer)?);
    writer.close()?;
    let mut searcher = new_searcher(reader.clone(), random_bool(0.5), random_bool(0.5), false)?;

    let sort_field = SortField::new(Some("my_field"), SortFieldType::Long)?;
    let sort = Sort::with_fields(vec![sort_field])?;
    let top_docs =
        searcher.search_with_sort(MatchAllDocsQuery, 1 + random.random_range(0..100), sort)?;

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
    // TODO LongPoint.newRangeQuery未实现
    Ok(())
}
#[test]
fn test_sort_optimization_on_sorted_numeric_field() -> Result<()> {
    let mut random = random();
    let dir = Arc::new(new_directory(&mut random)?);
    let writer = IndexWriter::new(dir.clone(), IndexWriterConfig::new())?;

    let num_docs = at_least(&mut random, 5000);
    for _ in 0..num_docs {
        let value = random.random();
        let value2 = random.random();
        let mut doc = Document::new();
        doc.add(LongField::new("my_field", value, Store::No)?);
        doc.add(LongField::new("my_field", value2, Store::No)?);
        writer.add_document(doc)?;
    }

    let reader = Arc::new(directory_reader_util::open_with_writer(&writer)?);
    writer.close()?;
    let mut searcher = new_searcher(reader.clone(), true, true, false)?;

    let selector_type = if random.random_bool(0.5) {
        SortedNumericSelectorType::Min
    } else {
        SortedNumericSelectorType::Max
    };
    let reverse = random.random_bool(0.5);

    let mut sort_field = long_field_type::new_sort_field("my_field", reverse, selector_type)?;
    sort_field.base.set_optimize_sort_with_indexed_data(false);
    let sort = Arc::new(Sort::with_fields(vec![sort_field])?);

    let sort_field2 = long_field_type::new_sort_field("my_field", reverse, selector_type)?;
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
        let top_docs = searcher.search_with_collector_manager(MatchAllDocsQuery, &manager)?;
        let score_docs = top_docs.score_docs();

        let manager2 = TopFieldCollectorManager::with_after(
            sort2.clone(),
            batch,
            after.clone(),
            total_hits_threshold,
        )?;
        let top_docs2 = searcher.search_with_collector_manager(MatchAllDocsQuery, &manager2)?;
        let score_docs2 = top_docs2.score_docs();

        assert_eq!(expected_hits as usize, score_docs.len());
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

        let last_doc = score_docs[expected_hits as usize - 1].clone();
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
fn test_string_sort_optimization() -> Result<()> {
    // TODO
    Ok(())
}
fn test_string_sort_optimization_with_missing_values() -> Result<()> {
    // TODO
    Ok(())
}
fn do_test_string_sort_optimization() -> Result<()> {
    // TODO
    Ok(())
}
fn assert_sort() -> Result<()> {
    // TODO
    Ok(())
}

fn assert_search_hits() -> Result<()> {
    // TODO
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
