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
use crate::core::document::knn_float_vector_field::KnnFloatVectorField;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, at_least_usize, new_directory_shared, new_index_writer_config,
  new_searcher_with_reader, random,
};

use crate::core::document::numeric_doc_values_field::NumericDocValuesField;

use crate::core::document::sorted_numeric_doc_values_field::SortedNumericDocValuesField;
use crate::core::document::string_field::StringField;

use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::term::Term;

use crate::core::search::field_exists_query::FieldExistsQuery;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::query::{Query, QueryBase};
use crate::core::search::score_doc::ScoreDocLike;
use crate::core::search::sort::Sort;
use crate::core::search::term_query::TermQuery;
use crate::core::search::top_docs::TopDocsLike;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;

use crate::core::document::binary_point::BinaryPoint;
use crate::core::document::double_doc_values_field::DoubleDocValuesField;
use crate::core::document::field_type::FieldType;
use crate::core::document::long_point::LongPoint;
use crate::core::document::sorted_doc_values_field::SortedDocValuesField;
use crate::core::document::text_field::TextField;
use crate::core::index::BytesRef;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::no_merge_policy::NoMergePolicy;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::boolean_query::Builder;
use crate::core::search::boost_query::BoostQuery;
use crate::core::search::constant_score_query::ConstantScoreQuery;
use crate::core::search::score_mode::ScoreMode;
use crate::core::util::TryIntoInt;
use crate::core::util::bit_set::BitSet;
use crate::core::util::core_helper::CoreHelper;
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::vector_util::VectorUtil;
use crate::test_framework::core::util::test_util::TestUtil;
use rand::RngExt;
use std::sync::Arc;
use std::vec;

#[allow(dead_code)] // for quick search
struct TestFieldExistsQuery;

#[test]
fn test_doc_values_rewrite_with_terms_present() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let config = new_index_writer_config(&mut random)?;
  let iw = RandomIndexWriter::with_config(&mut random, dir.clone(), config);
  let num_docs = at_least(&mut random, 100);

  for _ in 0..num_docs {
    let mut doc = Document::new();
    doc.add(DoubleDocValuesField::new("f", 2.0));
    doc.add(StringField::from_string(
      "f",
      if random.random_bool(0.5) { "yes" } else { "no" },
      Store::No,
    )?);
    iw.add_document(&mut random, doc)?;
  }

  iw.commit(&mut random)?;
  let reader = iw.get_reader(&mut random)?;
  iw.close(&mut random)?;

  let searcher = new_searcher_with_reader(reader)?;
  let query = FieldExistsQuery::new("f");
  let rewritten = query.rewrite(&searcher)?;

  assert!(matches!(rewritten, Some(Query::MatchAllDocs(_))));

  Ok(())
}
#[test]
fn test_doc_values_rewrite_with_point_values_present() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let config = new_index_writer_config(&mut random)?;
  let iw = RandomIndexWriter::with_config(&mut random, dir.clone(), config);
  let num_docs = at_least(&mut random, 100);

  for _ in 0..num_docs {
    let mut doc = Document::new();
    doc.add(BinaryPoint::new("dim", [[0u8; 4], [0u8; 4]])?);
    doc.add(DoubleDocValuesField::new("dim", 2.0));
    iw.add_document(&mut random, doc)?;
  }

  iw.commit(&mut random)?;
  let reader = iw.get_reader(&mut random)?;
  iw.close(&mut random)?;

  let searcher = new_searcher_with_reader(reader)?;
  let query = FieldExistsQuery::new("dim");
  let rewritten = query.rewrite(&searcher)?;

  assert!(matches!(rewritten, Some(Query::MatchAllDocs(_))));

  Ok(())
}
#[test]
fn test_doc_values_no_rewrite() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let config = new_index_writer_config(&mut random)?;
  let iw = RandomIndexWriter::with_config(&mut random, dir.clone(), config);
  let num_docs = at_least(&mut random, 100);

  for _ in 0..num_docs {
    let mut doc = Document::new();
    doc.add(DoubleDocValuesField::new("dim", 2.0));
    doc.add(BinaryPoint::new("dim", [[0u8; 4], [0u8; 4]])?);
    iw.add_document(&mut random, doc)?;
  }

  for _ in 0..num_docs {
    let mut doc = Document::new();
    doc.add(DoubleDocValuesField::new("f", 2.0));
    doc.add(StringField::from_string(
      "f",
      if random.random_bool(0.5) { "yes" } else { "no" },
      Store::No,
    )?);
    iw.add_document(&mut random, doc)?;
  }

  iw.commit(&mut random)?;
  let reader = iw.get_reader(&mut random)?;
  iw.close(&mut random)?;

  let searcher = new_searcher_with_reader(reader)?;

  let rewritten_dim = FieldExistsQuery::new("dim").rewrite(&searcher)?;
  assert!(!matches!(rewritten_dim, Some(Query::MatchAllDocs(_))));

  let rewritten_f = FieldExistsQuery::new("f").rewrite(&searcher)?;
  assert!(!matches!(rewritten_f, Some(Query::MatchAllDocs(_))));

  Ok(())
}

#[test]
fn test_doc_values_no_rewrite_with_doc_values() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let config = new_index_writer_config(&mut random)?;
  let iw = RandomIndexWriter::with_config(&mut random, dir.clone(), config);
  let num_docs = at_least(&mut random, 100);

  for _ in 0..num_docs {
    let mut doc = Document::new();
    doc.add(NumericDocValuesField::new("dv1", 1));
    doc.add(SortedNumericDocValuesField::new("dv2", 1));
    doc.add(SortedNumericDocValuesField::new("dv2", 2));
    iw.add_document(&mut random, doc)?;
  }

  iw.commit(&mut random)?;
  let reader = iw.get_reader(&mut random)?;
  iw.close(&mut random)?;

  let searcher = new_searcher_with_reader(reader)?;

  let rewritten_dv1 = FieldExistsQuery::new("dv1").rewrite(&searcher)?;
  assert!(!matches!(rewritten_dv1, Some(Query::MatchAllDocs(_))));

  let rewritten_dv2 = FieldExistsQuery::new("dv2").rewrite(&searcher)?;
  assert!(!matches!(rewritten_dv2, Some(Query::MatchAllDocs(_))));

  let rewritten_dv3 = FieldExistsQuery::new("dv3").rewrite(&searcher)?;
  assert!(!matches!(rewritten_dv3, Some(Query::MatchAllDocs(_))));

  Ok(())
}

#[test]
fn test_doc_values_random() -> Result<()> {
  let mut random = random();

  let iters = at_least(&mut random, 10);
  for _ in 0..iters {
    let dir = new_directory_shared(&mut random)?;
    let iw = RandomIndexWriter::new(&mut random, dir.clone())?;
    let num_docs = at_least(&mut random, 100);

    for _ in 0..num_docs {
      let mut doc = Document::new();
      let has_value = random.random_bool(0.5);

      if has_value {
        doc.add(NumericDocValuesField::new("dv1", 1));
        doc.add(SortedNumericDocValuesField::new("dv2", 1));
        doc.add(SortedNumericDocValuesField::new("dv2", 2));
        doc.add(StringField::from_string("has_value", "yes", Store::No)?);
      }

      doc.add(StringField::from_string(
        "f",
        if random.random_bool(0.5) { "yes" } else { "no" },
        Store::No,
      )?);

      iw.add_document(&mut random, doc)?;
    }

    if random.random_bool(0.5) {
      iw.delete_documents_with_queries(
        &mut random,
        vec![TermQuery::new(Term::from_text("f", "no")).into()],
      )?;
    }

    iw.commit(&mut random)?;
    let reader = iw.get_reader(&mut random)?;
    let searcher = new_searcher_with_reader(reader)?;
    iw.close(&mut random)?;

    assert_same_matches(
      &searcher,
      TermQuery::new(Term::from_text("has_value", "yes")),
      FieldExistsQuery::new("dv1"),
      false,
    )?;

    assert_same_matches(
      &searcher,
      TermQuery::new(Term::from_text("has_value", "yes")),
      FieldExistsQuery::new("dv2"),
      false,
    )?;
  }

  Ok(())
}

#[test]
fn test_doc_values_approximation() -> Result<()> {
  let mut random = random();
  let iters = at_least(&mut random, 10);

  for _ in 0..iters {
    let dir = new_directory_shared(&mut random)?;
    let config = new_index_writer_config(&mut random)?;
    let iw = RandomIndexWriter::with_config(&mut random, dir.clone(), config);

    let num_docs = at_least(&mut random, 100);
    for _ in 0..num_docs {
      let mut doc = Document::new();
      let has_value = random.random_bool(0.5);
      if has_value {
        doc.add(NumericDocValuesField::new("dv1", 1));
        doc.add(SortedNumericDocValuesField::new("dv2", 1));
        doc.add(SortedNumericDocValuesField::new("dv2", 2));
        doc.add(StringField::from_string("has_value", "yes", Store::No)?);
      }
      doc.add(StringField::from_string(
        "f",
        if random.random_bool(0.5) { "yes" } else { "no" },
        Store::No,
      )?);
      iw.add_document(&mut random, doc)?;
    }
    if random.random_bool(0.5) {
      iw.delete_documents_with_queries(
        &mut random,
        vec![TermQuery::new(Term::from_text("f", "no")).into()],
      )?;
    }

    iw.commit(&mut random)?;
    let reader = iw.get_reader(&mut random)?;
    let searcher = new_searcher_with_reader(reader)?;
    iw.close(&mut random)?;

    let mut ref_builder = Builder::new();
    ref_builder
      .add(TermQuery::new(Term::from_text("f", "yes")), Occur::Must)?
      .add(
        TermQuery::new(Term::from_text("has_value", "yes")),
        Occur::Filter,
      )?;
    let ref_query = ref_builder.build();

    let mut bq1 = Builder::new();
    bq1
      .add(TermQuery::new(Term::from_text("f", "yes")), Occur::Must)?
      .add(FieldExistsQuery::new("dv1"), Occur::Filter)?;
    assert_same_matches(&searcher, ref_query.clone(), bq1.build(), true)?;

    let mut bq2 = Builder::new();
    bq2
      .add(TermQuery::new(Term::from_text("f", "yes")), Occur::Must)?
      .add(FieldExistsQuery::new("dv2"), Occur::Filter)?;
    assert_same_matches(&searcher, ref_query, bq2.build(), true)?;
  }

  Ok(())
}

#[test]
fn test_doc_values_score() -> Result<()> {
  let mut random = random();
  let iters = at_least(&mut random, 10);

  for _ in 0..iters {
    let dir = new_directory_shared(&mut random)?;
    let config = new_index_writer_config(&mut random)?;
    let iw = RandomIndexWriter::with_config(&mut random, dir.clone(), config);

    let num_docs = at_least(&mut random, 100);
    for _ in 0..num_docs {
      let mut doc = Document::new();
      let has_value = random.random_bool(0.5);
      if has_value {
        doc.add(NumericDocValuesField::new("dv1", 1));
        doc.add(SortedNumericDocValuesField::new("dv2", 1));
        doc.add(SortedNumericDocValuesField::new("dv2", 2));
        doc.add(StringField::from_string("has_value", "yes", Store::No)?);
      }
      doc.add(StringField::from_string(
        "f",
        if random.random_bool(0.5) { "yes" } else { "no" },
        Store::No,
      )?);
      iw.add_document(&mut random, doc)?;
    }
    if random.random_bool(0.5) {
      iw.delete_documents_with_queries(
        &mut random,
        vec![TermQuery::new(Term::from_text("f", "no")).into()],
      )?;
    }

    iw.commit(&mut random)?;
    let reader = iw.get_reader(&mut random)?;
    let searcher = new_searcher_with_reader(reader)?;
    iw.close(&mut random)?;

    let boost = random.random::<f32>() * 10.0;

    let ref_query: Query = BoostQuery::new(
      ConstantScoreQuery::new(TermQuery::new(Term::from_text("has_value", "yes"))),
      boost,
    )?
    .into();

    let q1: Query = BoostQuery::new(FieldExistsQuery::new("dv1"), boost)?.into();
    assert_same_matches(&searcher, ref_query.clone(), q1, true)?;

    let q2: Query = BoostQuery::new(FieldExistsQuery::new("dv2"), boost)?.into();
    assert_same_matches(&searcher, ref_query, q2, true)?;
  }

  Ok(())
}

#[test]
fn test_doc_values_missing_field() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let iw = RandomIndexWriter::new(&mut random, dir.clone())?;

  iw.add_document(&mut random, Document::new())?;
  iw.commit(&mut random)?;

  let reader = iw.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;
  iw.close(&mut random)?;

  assert_eq!(0, searcher.count(FieldExistsQuery::new("f"))?);

  Ok(())
}
#[test]
fn test_doc_values_all_docs_have_field() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let iw = RandomIndexWriter::new(&mut random, dir.clone())?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("f", 1));
  iw.add_document(&mut random, doc)?;
  iw.commit(&mut random)?;

  let reader = iw.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;
  iw.close(&mut random)?;

  assert_eq!(1, searcher.count(FieldExistsQuery::new("f"))?);

  Ok(())
}
#[test]
fn test_doc_values_field_exists_but_no_docs_have_field() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let iw = RandomIndexWriter::new(&mut random, dir.clone())?;

  let mut doc = Document::new();
  doc.add(NumericDocValuesField::new("f", 1));
  iw.add_document(&mut random, doc)?;
  iw.commit(&mut random)?;

  iw.add_document(&mut random, Document::new())?;
  iw.commit(&mut random)?;

  let reader = iw.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;
  iw.close(&mut random)?;

  assert_eq!(1, searcher.count(FieldExistsQuery::new("f"))?);

  Ok(())
}
#[test]
fn test_doc_values_query_matches_count() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let config = new_index_writer_config(&mut random)?;
  let w = RandomIndexWriter::with_config(&mut random, dir.clone(), config);

  let random_num_docs = random.random_range(11..=100);
  let mut num_matching_docs = 0i32;

  for i in 0..random_num_docs {
    let mut doc = Document::new();
    // We select most documents randomly but keep two documents:
    //  * #0 ensures we will delete at least one document (with long between 0 and 9)
    //  * #10 ensures we will keep at least one document (with long greater than 9)
    if i == 0 || i == 10 || random.random_bool(0.5) {
      let v = i as i64;
      doc.add(LongPoint::new("long", [v])?);
      doc.add(NumericDocValuesField::new("long", v));
      doc.add(StringField::from_string("string", "value", Store::No)?);
      doc.add(SortedDocValuesField::new(
        "string",
        BytesRef::from_string("value"),
      ));
      num_matching_docs += 1;
    }
    w.add_document(&mut random, doc)?;
  }
  w.force_merge(&mut random, 1)?;

  let reader = w.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;

  assert_same_count(&searcher, "long", num_matching_docs)?;
  assert_same_count(&searcher, "string", num_matching_docs)?;
  assert_same_count(&searcher, "doesNotExist", 0)?;

  // Test that we can't count in O(1) when there are deleted documents
  w.w.get_config()?.set_merge_policy(NoMergePolicy::default());
  w.delete_documents_with_queries(
    &mut random,
    vec![LongPoint::new_range_query("long", 0, 9)?.into()],
  )?;
  let reader2 = w.get_reader(&mut random)?;
  let searcher2 = new_searcher_with_reader(reader2)?;

  let test_query: Query = FieldExistsQuery::new("long").into();
  let weight2 = searcher2.create_weight(test_query, ScoreMode::Complete, 1.0)?;

  let leaf = &searcher2.get_leaf_contexts()?[0];
  assert_eq!(-1, weight2.count(leaf, &searcher2)?);

  searcher.get_index_reader().close()?;
  searcher2.get_index_reader().close()?;

  w.close(&mut random)?;
  Ok(())
}

#[test]
fn test_norms_random() -> Result<()> {
  let mut random = random();

  let iters = at_least(&mut random, 10);
  for _ in 0..iters {
    let dir = new_directory_shared(&mut random)?;
    let iw = RandomIndexWriter::new(&mut random, dir.clone())?;
    let num_docs = at_least(&mut random, 100);

    for _ in 0..num_docs {
      let mut doc = Document::new();
      let has_value = random.random_bool(0.5);

      if has_value {
        doc.add(TextField::from_string("text1", "value", Store::No)?);
        doc.add(StringField::from_string("has_value", "yes", Store::No)?);
      }

      doc.add(StringField::from_string(
        "f",
        if random.random_bool(0.5) { "yes" } else { "no" },
        Store::No,
      )?);

      iw.add_document(&mut random, doc)?;
    }

    if random.random_bool(0.5) {
      iw.delete_documents_with_queries(
        &mut random,
        vec![TermQuery::new(Term::from_text("f", "no")).into()],
      )?;
    }

    iw.commit(&mut random)?;
    let reader = iw.get_reader(&mut random)?;
    let searcher = new_searcher_with_reader(reader)?;
    iw.close(&mut random)?;

    assert_same_matches(
      &searcher,
      TermQuery::new(Term::from_text("has_value", "yes")),
      FieldExistsQuery::new("text1"),
      false,
    )?;
  }

  Ok(())
}
#[test]
fn test_norms_approximation() -> Result<()> {
  let mut random = random();
  let iters = at_least(&mut random, 10);

  for _ in 0..iters {
    let dir = new_directory_shared(&mut random)?;
    let config = new_index_writer_config(&mut random)?;
    let iw = RandomIndexWriter::with_config(&mut random, dir.clone(), config);

    let num_docs = at_least(&mut random, 100);
    for _ in 0..num_docs {
      let mut doc = Document::new();
      let has_value = random.random_bool(0.5);
      if has_value {
        doc.add(TextField::from_string("text1", "value", Store::No)?);
        doc.add(StringField::from_string("has_value", "yes", Store::No)?);
      }
      doc.add(StringField::from_string(
        "f",
        if random.random_bool(0.5) { "yes" } else { "no" },
        Store::No,
      )?);
      iw.add_document(&mut random, doc)?;
    }
    if random.random_bool(0.5) {
      iw.delete_documents_with_queries(
        &mut random,
        vec![TermQuery::new(Term::from_text("f", "no")).into()],
      )?;
    }

    iw.commit(&mut random)?;
    let reader = iw.get_reader(&mut random)?;
    let searcher = new_searcher_with_reader(reader)?;
    iw.close(&mut random)?;

    let ref_query: Query = {
      let mut b = Builder::new();
      b.add(TermQuery::new(Term::from_text("f", "yes")), Occur::Must)?
        .add(
          TermQuery::new(Term::from_text("has_value", "yes")),
          Occur::Filter,
        )?;
      b.build().into()
    };

    let q1: Query = {
      let mut bq1 = Builder::new();
      bq1
        .add(TermQuery::new(Term::from_text("f", "yes")), Occur::Must)?
        .add(FieldExistsQuery::new("text1"), Occur::Filter)?;
      bq1.build().into()
    };

    assert_same_matches(&searcher, ref_query, q1, true)?;
  }

  Ok(())
}

#[test]
fn test_norms_score() -> Result<()> {
  let mut random = random();
  let iters = at_least(&mut random, 10);

  for _ in 0..iters {
    let dir = new_directory_shared(&mut random)?;
    let config = new_index_writer_config(&mut random)?;
    let iw = RandomIndexWriter::with_config(&mut random, dir.clone(), config);

    let num_docs = at_least(&mut random, 100);
    for _ in 0..num_docs {
      let mut doc = Document::new();
      let has_value = random.random_bool(0.5);
      if has_value {
        doc.add(TextField::from_string("text1", "value", Store::No)?);
        doc.add(StringField::from_string("has_value", "yes", Store::No)?);
      }
      doc.add(StringField::from_string(
        "f",
        if random.random_bool(0.5) { "yes" } else { "no" },
        Store::No,
      )?);
      iw.add_document(&mut random, doc)?;
    }

    if random.random_bool(0.5) {
      iw.delete_documents_with_queries(
        &mut random,
        vec![TermQuery::new(Term::from_text("f", "no")).into()],
      )?;
    }

    iw.commit(&mut random)?;
    let reader = iw.get_reader(&mut random)?;
    let searcher = new_searcher_with_reader(reader)?;
    iw.close(&mut random)?;

    let boost = random.random::<f32>() * 10.0;

    let ref_query: Query = BoostQuery::new(
      ConstantScoreQuery::new(TermQuery::new(Term::from_text("has_value", "yes"))),
      boost,
    )?
    .into();

    let q1: Query = BoostQuery::new(FieldExistsQuery::new("text1"), boost)?.into();

    assert_same_matches(&searcher, ref_query, q1, true)?;
  }

  Ok(())
}

#[test]
fn test_norms_missing_field() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let iw = RandomIndexWriter::new(&mut random, dir.clone())?;

  iw.add_document(&mut random, Document::new())?;
  iw.commit(&mut random)?;

  let reader = iw.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;
  iw.close(&mut random)?;

  assert_eq!(0, searcher.count(FieldExistsQuery::new("f"))?);

  Ok(())
}
#[test]
fn test_norms_all_docs_have_field() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let iw = RandomIndexWriter::new(&mut random, dir.clone())?;

  let mut doc = Document::new();
  doc.add(TextField::from_string("f", "value", Store::No)?);
  iw.add_document(&mut random, doc)?;
  iw.commit(&mut random)?;

  let reader = iw.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;
  iw.close(&mut random)?;

  assert_eq!(1, searcher.count(FieldExistsQuery::new("f"))?);

  Ok(())
}
#[test]
fn test_norms_field_exists_but_no_docs_have_field() -> Result<()> {
  let mut random = random();

  let dir = new_directory_shared(&mut random)?;
  let iw = RandomIndexWriter::new(&mut random, dir.clone())?;

  let mut doc = Document::new();
  doc.add(TextField::from_string("f", "value", Store::No)?);
  iw.add_document(&mut random, doc)?;
  iw.commit(&mut random)?;

  iw.add_document(&mut random, Document::new())?;
  iw.commit(&mut random)?;

  let reader = iw.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;
  iw.close(&mut random)?;

  assert_eq!(1, searcher.count(FieldExistsQuery::new("f"))?);

  Ok(())
}
#[test]
fn test_norms_query_matches_count() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, dir.clone())?;

  let random_num_docs = TestUtil::next_int(&mut random, 10, 100);

  let mut no_norms_field_type = FieldType::default();
  no_norms_field_type.set_omit_norms(true)?;
  no_norms_field_type.set_index_options(IndexOptions::Docs)?;

  let mut doc = Document::new();
  doc.add(TextField::from_string("text", "always here", Store::No)?);
  doc.add(TextField::from_string("text_s", "", Store::No)?);
  doc.add(Field::new(
    "text_n",
    "always here",
    no_norms_field_type.clone(),
  ));
  w.add_document(&mut random, &mut doc)?;

  for _i in 1..random_num_docs {
    doc.clear();
    doc.add(TextField::from_string("text", "some text", Store::No)?);
    doc.add(TextField::from_string("text_s", "some text", Store::No)?);
    doc.add(Field::new(
      "text_n",
      "some here",
      no_norms_field_type.clone(),
    ));
    w.add_document(&mut random, &mut doc)?;
  }
  w.force_merge(&mut random, 1)?;

  let reader = w.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;

  assert_norms_count_with_shortcut(&searcher, "text", random_num_docs)?;
  assert_norms_count_with_shortcut(&searcher, "doesNotExist", 0)?;

  let q = FieldExistsQuery::new("text_n");
  assert!(matches!(
    searcher.count(q),
    Err(error) if error.is_illegal_state_error()
  ));
  // docs that have a text field that analyzes to an empty token
  // stream still have a recorded norm value but don't show up in
  // `IndexReader::get_doc_count(field)`, so we cannot use the shortcut for
  // these fields
  assert_norms_count_without_shortcut(&searcher, "text_s", random_num_docs)?;

  // We can still shortcut with deleted docs
  w.w.get_config()?.set_merge_policy(NoMergePolicy::default());
  w.delete_documents_with_terms(&mut random, vec![Term::from_text("text", "text")])?; // deletes all but the first doc

  let reader2 = w.get_reader(&mut random)?;
  let searcher2 = new_searcher_with_reader(reader2)?;
  assert_norms_count_with_shortcut(&searcher2, "text", 1)?;

  searcher.reader_context.reader().close()?;
  searcher2.reader_context.reader().close()?;
  w.close(&mut random)?;
  Ok(())
}
fn assert_norms_count_without_shortcut<IRC, SIM>(
  searcher: &IndexSearcher<IRC, SIM>,
  field: &str,
  expected_count: i32,
) -> Result<()>
where
  SIM: crate::core::search::similarities_impl::similarities::Similarity,
  IRC: IndexReaderContext + Sync,
  crate::core::search::index_searcher::IndexSearcher<IRC, SIM>: Sync,
{
  let q = FieldExistsQuery::new(field);
  let weight = searcher.create_weight(q.clone(), ScoreMode::Complete, 1.0)?;

  let ctxs = searcher.get_leaf_contexts()?;
  assert_eq!(-1, weight.count(&ctxs[0], searcher)?);

  assert_eq!(expected_count, searcher.count(q)?);
  Ok(())
}

fn assert_norms_count_with_shortcut<IRC, SIM>(
  searcher: &IndexSearcher<IRC, SIM>,
  field: &str,
  num_matching_docs: i32,
) -> Result<()>
where
  SIM: crate::core::search::similarities_impl::similarities::Similarity,
  IRC: IndexReaderContext + Sync,
  crate::core::search::index_searcher::IndexSearcher<IRC, SIM>: Sync,
{
  let q = FieldExistsQuery::new(field);

  assert_eq!(num_matching_docs, searcher.count(q.clone())?);

  let weight = searcher.create_weight(q, ScoreMode::Complete, 1.0)?;
  let ctxs = searcher.get_leaf_contexts()?;
  assert_eq!(num_matching_docs, weight.count(&ctxs[0], searcher)?);
  Ok(())
}
#[test]
fn test_knn_vector_random() -> Result<()> {
  let mut random = random();
  let iters = at_least(&mut random, 10);

  for _ in 0..iters {
    let dir = new_directory_shared(&mut random)?;
    let iw = RandomIndexWriter::new(&mut random, dir.clone())?;
    let num_docs = at_least(&mut random, 100);

    for _ in 0..num_docs {
      let mut doc = Document::new();
      let has_value = random.random_bool(0.5);

      if has_value {
        doc.add(KnnFloatVectorField::new(
          "vector",
          random_vector(&mut random, 5),
        )?);
        doc.add(StringField::from_string("has_value", "yes", Store::No)?);
      }
      doc.add(StringField::from_string("field", "value", Store::No)?);
      iw.add_document(&mut random, doc)?;
    }

    if random.random_bool(0.5) {
      iw.delete_documents_with_queries(
        &mut random,
        vec![TermQuery::new(Term::from_text("f", "no")).into()],
      )?;
    }

    iw.commit(&mut random)?;
    let reader = iw.get_reader(&mut random)?;
    let searcher = new_searcher_with_reader(reader)?;
    iw.close(&mut random)?;

    assert_same_matches(
      &searcher,
      TermQuery::new(Term::from_text("has_value", "yes")),
      FieldExistsQuery::new("vector"),
      false,
    )?;

    let boost = random.random::<f32>() * 10.0;
    let ref_query: Query = BoostQuery::new(
      ConstantScoreQuery::new(TermQuery::new(Term::from_text("has_value", "yes"))),
      boost,
    )?
    .into();
    let exists_query: Query = BoostQuery::new(FieldExistsQuery::new("vector"), boost)?.into();
    assert_same_matches(&searcher, ref_query, exists_query, true)?;
  }

  Ok(())
}

#[test]
fn test_knn_vector_missingfield() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let iw = RandomIndexWriter::new(&mut random, dir.clone())?;

  iw.add_document(&mut random, Document::new())?;
  iw.commit(&mut random)?;

  let reader = iw.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;
  iw.close(&mut random)?;

  assert_eq!(0, searcher.count(FieldExistsQuery::new("f"))?);

  Ok(())
}

#[test]
fn test_knn_vector_all_docs_have_field() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let iw = RandomIndexWriter::new(&mut random, dir.clone())?;

  for _ in 0..100 {
    let mut doc = Document::new();
    doc.add(KnnFloatVectorField::new(
      "vector",
      random_vector(&mut random, 5),
    )?);
    iw.add_document(&mut random, doc)?;
  }
  iw.commit(&mut random)?;

  let reader = iw.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;
  iw.close(&mut random)?;

  let query = FieldExistsQuery::new("vector");
  let rewritten = query.rewrite(&searcher)?;
  assert!(matches!(rewritten, Some(Query::MatchAllDocs(_))));
  assert_eq!(100, searcher.count(query)?);

  Ok(())
}

#[test]
fn test_delete_knn_vector() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let iw = RandomIndexWriter::new(&mut random, dir.clone())?;
  let num_docs = at_least_usize(&mut random, 100);

  let all_docs_have_vector = random.random_bool(0.5);
  let mut docs_with_vector = FixedBitSet::new(num_docs);
  for i in 0..num_docs {
    let mut doc = Document::new();
    if all_docs_have_vector || random.random_bool(0.5) {
      doc.add(KnnFloatVectorField::new(
        "vector",
        random_vector(&mut random, 5),
      )?);
      docs_with_vector.set(i)?;
    }
    doc.add(StringField::from_string("id", i.to_string(), Store::No)?);
    iw.add_document(&mut random, doc)?;
  }

  if random.random_bool(0.5) {
    let num_deleted = random.random_range(1..=num_docs);
    for i in 0..num_deleted {
      iw.delete_documents_with_terms(&mut random, vec![Term::new("id", i.to_string())])?;
      docs_with_vector.clear_with_index(i)?;
    }
  }

  let reader = iw.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;
  iw.close(&mut random)?;

  let count = searcher.count(FieldExistsQuery::new("vector"))?;
  assert_eq!(docs_with_vector.cardinality() as i32, count);

  Ok(())
}

#[test]
fn test_knn_vector_conjunction() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let iw = RandomIndexWriter::new(&mut random, dir.clone())?;
  let num_docs = at_least(&mut random, 100);
  let mut num_vectors = 0;

  let all_docs_have_vector = random.random_bool(0.5);
  for i in 0..num_docs {
    let mut doc = Document::new();
    if all_docs_have_vector || random.random_bool(0.5) {
      doc.add(KnnFloatVectorField::new(
        "vector",
        random_vector(&mut random, 5),
      )?);
      num_vectors += 1;
    }
    doc.add(StringField::from_string(
      "field",
      format!("value{}", i % 2),
      Store::No,
    )?);
    iw.add_document(&mut random, doc)?;
  }

  let reader = iw.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;
  iw.close(&mut random)?;

  let occur = if random.random_bool(0.5) {
    Occur::Must
  } else {
    Occur::Filter
  };
  let mut boolean_query = Builder::new();
  boolean_query
    .add(TermQuery::new(Term::from_text("field", "value1")), occur)?
    .add(FieldExistsQuery::new("vector"), Occur::Filter)?;

  let count = searcher.count(boolean_query.build())?;
  assert!(count <= num_vectors);
  if all_docs_have_vector {
    assert_eq!(num_docs / 2, count);
  }

  Ok(())
}

#[test]
fn test_knn_vector_field_exists_but_no_docs_have_field() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let iw = RandomIndexWriter::new(&mut random, dir.clone())?;

  let mut doc = Document::new();
  doc.add(KnnFloatVectorField::new(
    "vector",
    random_vector(&mut random, 3),
  )?);
  iw.add_document(&mut random, doc)?;
  iw.commit(&mut random)?;

  iw.add_document(&mut random, Document::new())?;
  iw.commit(&mut random)?;

  let reader = iw.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;
  iw.close(&mut random)?;

  assert_eq!(1, searcher.count(FieldExistsQuery::new("vector"))?);

  Ok(())
}

fn random_vector<R>(random: &mut R, dim: usize) -> Vec<f32>
where
  R: rand::Rng + ?Sized,
{
  let mut vector = vec![0.0; dim];
  for value in &mut vector {
    *value = random.random::<f32>();
  }
  VectorUtil::l2normalize(&mut vector).expect("random vector should be normalizable");
  vector
}
#[test]
fn test_delete_all_point_docs() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let iw = RandomIndexWriter::new(&mut random, dir.clone())?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "0", Store::No)?);
  doc.add(LongPoint::new("long", [17])?);
  doc.add(NumericDocValuesField::new("long", 17));
  iw.add_document(&mut random, doc)?;

  // add another document before the flush, otherwise the segment only has the document that
  // we are going to delete and the merge simply ignores the segment without carrying over its
  // field infos
  iw.add_document(&mut random, Document::new())?;

  // make sure there are two segments or force merge will be a no-op
  iw.flush()?;
  iw.add_document(&mut random, Document::new())?;
  iw.commit(&mut random)?;

  iw.delete_documents_with_terms(&mut random, vec![Term::from_text("id", "0")])?;
  iw.force_merge(&mut random, 1)?;

  let reader = iw.get_reader(&mut random)?;
  assert!(!reader.has_deletions()?);
  let r = (&reader).get_context()?;
  assert_eq!(1, r.leaves()?.len());

  let searcher = new_searcher_with_reader(reader)?;
  let q = FieldExistsQuery::new("long");
  assert_eq!(0, searcher.count(q)?);

  Ok(())
}
#[test]
fn test_delete_all_term_docs() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let iw = RandomIndexWriter::new(&mut random, dir.clone())?;

  let mut doc = Document::new();
  doc.add(StringField::from_string("id", "0", Store::No)?);
  doc.add(StringField::from_string("str", "foo", Store::No)?);
  doc.add(SortedDocValuesField::new(
    "str",
    BytesRef::from_bytes(b"foo".to_vec()),
  ));
  iw.add_document(&mut random, doc)?;

  // add another document before the flush, otherwise the segment only has the document that
  // we are going to delete and the merge simply ignores the segment without carrying over its
  // field infos
  iw.add_document(&mut random, Document::new())?;

  // make sure there are two segments or force merge will be a no-op
  iw.flush()?;
  iw.add_document(&mut random, Document::new())?;
  iw.commit(&mut random)?;

  iw.delete_documents_with_terms(&mut random, vec![Term::from_text("id", "0")])?;
  iw.force_merge(&mut random, 1)?;

  let reader = iw.get_reader(&mut random)?;
  assert!(!reader.has_deletions()?);
  let r = (&reader).get_context()?;
  assert_eq!(1, r.leaves()?.len());

  let searcher = new_searcher_with_reader(reader)?;
  let q = FieldExistsQuery::new("str");
  assert_eq!(0, searcher.count(q)?);

  Ok(())
}
fn assert_same_matches<IRC, T1, T2, SIM>(
  searcher: &IndexSearcher<IRC, SIM>,
  q1: T1,
  q2: T2,
  scores: bool,
) -> Result<()>
where
  SIM: crate::core::search::similarities_impl::similarities::Similarity,
  IRC: IndexReaderContext + Sync,
  T1: Into<Query>,
  T2: Into<Query>,
  crate::core::search::index_searcher::IndexSearcher<IRC, SIM>: Sync,
{
  let irc = searcher.get_top_reader_context();
  let max_doc = irc.reader().max_doc()?;

  let sort = if scores {
    Arc::new(Sort::get_relevance()?)
  } else {
    Arc::new(Sort::get_index_order()?)
  };

  let max_doc: usize = max_doc.try_convert()?;
  let td1 = searcher.search_with_sort(q1, max_doc, sort.clone())?;
  let td2 = searcher.search_with_sort(q2, max_doc, sort)?;
  assert_eq!(td1.total_hits().value(), td2.total_hits().value());

  for i in 0..td1.score_docs().len() {
    let sd1 = &td1.score_docs()[i];
    let sd2 = &td2.score_docs()[i];

    assert_eq!(sd1.doc(), sd2.doc());

    let score1 = sd1.score();
    let score2 = sd2.score();
    assert!(
      CoreHelper::compare_f32(score1, score2).is_eq()
        || (score1 as f64 - score2 as f64).abs() <= 10e-7,
      "score diff={} idx={}",
      (score1 as f64 - score2 as f64).abs(),
      i
    );
  }

  Ok(())
}
fn assert_same_count<IRC, SIM>(
  searcher: &IndexSearcher<IRC, SIM>,
  field: &str,
  num_matching_docs: i32,
) -> Result<()>
where
  SIM: crate::core::search::similarities_impl::similarities::Similarity,
  IRC: IndexReaderContext + Sync,
  crate::core::search::index_searcher::IndexSearcher<IRC, SIM>: Sync,
{
  let test_query: Query = FieldExistsQuery::new(field).into();
  assert_eq!(searcher.count(test_query.clone())?, num_matching_docs);

  let weight = searcher.create_weight(test_query, ScoreMode::Complete, 1.0)?;
  assert_eq!(
    weight.count(&searcher.get_leaf_contexts()?[0], searcher)?,
    num_matching_docs
  );

  Ok(())
}

use crate::core::search::weight::Weight;
