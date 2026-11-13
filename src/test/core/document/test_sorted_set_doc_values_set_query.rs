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
use crate::core::document::sorted_doc_values_field::SortedDocValuesField;
use crate::core::document::string_field::StringField;
use crate::core::index::BytesRef;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::term::Term;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::boolean_query::Builder;
use crate::core::search::boost_query::BoostQuery;
use crate::core::search::constant_score_query::ConstantScoreQuery;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::query::Query;
use crate::core::search::score_doc::ScoreDocLike;
use crate::core::search::sort::Sort;
use crate::core::search::term_query::TermQuery;
use crate::core::search::top_docs::TopDocsLike;
use crate::core::util::TryIntoInt;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, new_directory_shared, new_searcher_with_reader, new_string_field, random,
};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::RngExt;
use std::collections::HashMap;
use std::sync::Arc;

#[allow(dead_code)] // for quick search
struct TestSortedSetDocValuesSetQuery;

#[test]
fn test_missing_terms() -> Result<()> {
  let mut random = random();
  let field_name = "field1";
  let rd = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, rd.clone())?;
  let mut field_to_type = HashMap::new();
  for i in 0..100 {
    let mut doc = Document::new();
    let term = i * 10;
    let term = term.to_string();
    doc.add(new_string_field(
      &mut random,
      field_name,
      term.as_str(),
      Store::Yes,
      &mut field_to_type,
    )?);
    doc.add(SortedDocValuesField::new(
      field_name,
      BytesRef::from_string(&term),
    ));
    w.add_document(&mut random, doc)?;
  }
  let reader = w.get_reader(&mut random)?;
  w.close(&mut random)?;

  let searcher = new_searcher_with_reader(reader)?;
  let num_docs = searcher.get_index_reader().num_docs()?.try_convert()?;

  let terms = vec![BytesRef::from_string("5")];
  let results = searcher
    .search(
      SortedDocValuesField::new_slow_set_query(field_name, terms)?,
      num_docs,
    )?
    .score_docs;
  assert_eq!(0, results.len(), "Must match nothing");

  let terms = vec![BytesRef::from_string("10")];
  let results = searcher
    .search(
      SortedDocValuesField::new_slow_set_query(field_name, terms)?,
      num_docs,
    )?
    .score_docs;
  assert_eq!(1, results.len(), "Must match 1");

  let terms = vec![BytesRef::from_string("10"), BytesRef::from_string("20")];
  let results = searcher
    .search(
      SortedDocValuesField::new_slow_set_query(field_name, terms)?,
      num_docs,
    )?
    .score_docs;
  assert_eq!(2, results.len(), "Must match 2");

  Ok(())
}

#[test]
fn test_equals() -> Result<()> {
  let bar = vec![BytesRef::from_string("bar")];

  let barbar = vec![BytesRef::from_string("bar"), BytesRef::from_string("bar")];

  let barbaz = vec![BytesRef::from_string("bar"), BytesRef::from_string("baz")];

  let bazbar = vec![BytesRef::from_string("baz"), BytesRef::from_string("bar")];

  let baz = vec![BytesRef::from_string("baz")];

  assert_eq!(
    SortedDocValuesField::new_slow_set_query("foo", bar.clone())?,
    SortedDocValuesField::new_slow_set_query("foo", bar.clone())?
  );
  assert_eq!(
    SortedDocValuesField::new_slow_set_query("foo", bar.clone())?,
    SortedDocValuesField::new_slow_set_query("foo", barbar)?
  );
  assert_eq!(
    SortedDocValuesField::new_slow_set_query("foo", barbaz)?,
    SortedDocValuesField::new_slow_set_query("foo", bazbar)?
  );
  assert_ne!(
    SortedDocValuesField::new_slow_set_query("foo", bar.clone())?,
    SortedDocValuesField::new_slow_set_query("foo2", bar.clone())?
  );
  assert_ne!(
    SortedDocValuesField::new_slow_set_query("foo", bar)?,
    SortedDocValuesField::new_slow_set_query("foo", baz)?
  );

  Ok(())
}
#[test]
fn test_duel_terms_query() -> Result<()> {
  let mut random = random();
  let iters = at_least(&mut random, 2);
  for _ in 0..iters {
    let mut all_terms = Vec::new();
    let end = 1 << TestUtil::next_int(&mut random, 1, 10);
    let num_terms = TestUtil::next_int(&mut random, 1, end);
    for _ in 0..num_terms {
      let value = TestUtil::random_analysis_string(&mut random, 10, true);
      all_terms.push(Term::from_text("f", value));
    }
    let dir = new_directory_shared(&mut random)?;
    let iw = RandomIndexWriter::new(&mut random, dir.clone())?;
    let num_docs = at_least(&mut random, 100);
    for _ in 0..num_docs {
      let mut doc = Document::new();
      let term = &all_terms[random.random_range(0..all_terms.len())];
      doc.add(StringField::from_string(
        term.field().to_owned(),
        term.text()?,
        Store::No,
      )?);
      doc.add(SortedDocValuesField::new(
        term.field().to_owned(),
        term.bytes().clone(),
      ));
      iw.add_document(&mut random, doc)?;
    }
    if num_terms > 1 && random.random_bool(0.5) {
      iw.delete_documents_with_queries(
        &mut random,
        vec![Query::from(TermQuery::new(all_terms[0].clone()))],
      )?;
    }
    iw.commit(&mut random)?;
    let reader = iw.get_reader(&mut random)?;
    let searcher = new_searcher_with_reader(reader)?;
    iw.close(&mut random)?;

    if searcher.get_top_reader_context().reader().num_docs()? == 0 {
      continue;
    }

    for _ in 0..100 {
      let boost = random.random::<f32>() * 10.0;
      let end = 1 << TestUtil::next_int(&mut random, 1, 8);
      let num_query_terms = TestUtil::next_int(&mut random, 1, end);
      let mut query_terms = Vec::new();
      for _ in 0..num_query_terms {
        query_terms.push(&all_terms[random.random_range(0..all_terms.len())]);
      }
      let mut bq = Builder::new();
      for term in &query_terms {
        bq.add(TermQuery::new((*term).clone()), Occur::Should)?;
      }
      let q1 = BoostQuery::new(ConstantScoreQuery::new(bq.build()), boost)?;
      let mut bytes_terms = Vec::new();
      for term in &query_terms {
        bytes_terms.push(term.bytes().clone());
      }
      let q2 = BoostQuery::new(
        SortedDocValuesField::new_slow_set_query("f", bytes_terms)?,
        boost,
      )?;
      assert_same_matches(&searcher, q1, q2, true)?;
    }
  }

  Ok(())
}

#[test]
fn test_approximation() -> Result<()> {
  let mut random = random();
  let iters = at_least(&mut random, 2);
  for _ in 0..iters {
    let mut all_terms = Vec::new();
    let end = 1 << TestUtil::next_int(&mut random, 1, 10);
    let num_terms = TestUtil::next_int(&mut random, 1, end);
    for _ in 0..num_terms {
      let value = TestUtil::random_analysis_string(&mut random, 10, true);
      all_terms.push(Term::from_text("f", value));
    }
    let dir = new_directory_shared(&mut random)?;
    let iw = RandomIndexWriter::new(&mut random, dir.clone())?;
    let num_docs = at_least(&mut random, 100);
    for _ in 0..num_docs {
      let mut doc = Document::new();
      let term = &all_terms[random.random_range(0..all_terms.len())];
      doc.add(StringField::from_string(
        term.field().to_owned(),
        term.text()?,
        Store::No,
      )?);
      doc.add(SortedDocValuesField::new(
        term.field().to_owned(),
        term.bytes().clone(),
      ));
      iw.add_document(&mut random, doc)?;
    }
    if num_terms > 1 && random.random_bool(0.5) {
      iw.delete_documents_with_terms(&mut random, vec![all_terms[0].clone()])?;
    }
    iw.commit(&mut random)?;
    let reader = iw.get_reader(&mut random)?;
    let searcher = new_searcher_with_reader(reader)?;
    iw.close(&mut random)?;

    if searcher.get_top_reader_context().reader().num_docs()? == 0 {
      continue;
    }

    for _ in 0..100 {
      let boost = random.random::<f32>() * 10.0;
      let end = 1 << TestUtil::next_int(&mut random, 1, 8);
      let num_query_terms = TestUtil::next_int(&mut random, 1, end);
      let mut query_terms = Vec::new();
      for _ in 0..num_query_terms {
        query_terms.push(&all_terms[random.random_range(0..all_terms.len())]);
      }
      let mut bq = Builder::new();
      for term in &query_terms {
        bq.add(TermQuery::new((*term).clone()), Occur::Should)?;
      }
      let q1 = BoostQuery::new(ConstantScoreQuery::new(bq.build()), boost)?;
      let mut bytes_terms = Vec::new();
      for term in &query_terms {
        bytes_terms.push(term.bytes().clone());
      }
      let q2 = BoostQuery::new(
        SortedDocValuesField::new_slow_set_query("f", bytes_terms)?,
        boost,
      )?;

      let mut bq1 = Builder::new();
      bq1.add(q1, Occur::Must)?;
      bq1.add(TermQuery::new(all_terms[0].clone()), Occur::Filter)?;

      let mut bq2 = Builder::new();
      bq2.add(q2, Occur::Must)?;
      bq2.add(TermQuery::new(all_terms[0].clone()), Occur::Filter)?;

      assert_same_matches(&searcher, bq1.build(), bq2.build(), true)?;
    }
  }

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
  IRC: IndexReaderContext + std::marker::Sync,
  T1: Into<Query>,
  T2: Into<Query>,
  crate::core::search::index_searcher::IndexSearcher<IRC, SIM>: Sync,
{
  let max_doc = searcher.get_index_reader().max_doc()?;
  let sort = if scores {
    Arc::new(Sort::get_relevance()?)
  } else {
    Arc::new(Sort::get_index_order()?)
  };
  let td1 = searcher.search_with_sort(q1, max_doc.try_convert()?, sort.clone())?;
  let td2 = searcher.search_with_sort(q2, max_doc.try_convert()?, sort)?;
  assert_eq!(td1.total_hits().value(), td2.total_hits().value());
  for i in 0..td1.score_docs().len() {
    assert_eq!(td1.score_docs()[i].doc(), td2.score_docs()[i].doc());
    if scores {
      let score1 = td1.score_docs()[i].score();
      let score2 = td2.score_docs()[i].score();
      assert!(
        (score1.is_nan() && score2.is_nan()) || (score1 - score2).abs() <= 10e-7,
        "score for {i} was not the same: {score1} != {score2}"
      );
    }
  }
  Ok(())
}
