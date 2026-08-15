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
use crate::core::document::field::Store::No;
use crate::core::document::field::{FieldBase, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::long_point::LongPoint;
use crate::core::document::string_field::StringField;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, new_directory_shared, new_index_writer_config, new_index_writer_config_with_analyzer,
  new_log_merge_policy, new_searcher_with_reader, new_string_field, new_text_field, random,
  random_multiplier,
};

use crate::core::index::directory_reader;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::index_writer_config::IndexWriterConfig;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::multi_reader::MultiReader;
use crate::core::index::term::Term;
use crate::core::search::boolean_clause::{BooleanClause, Occur};
use crate::core::search::boolean_query::{BooleanQuery, Builder};
use crate::core::search::boost_query::BoostQuery;
use crate::core::search::collector::Collector;
use crate::core::search::collector_manager::CollectorManager;
use crate::core::search::constant_score_query::ConstantScoreQuery;
use crate::core::search::disjunction_max_query::DisjunctionMaxQuery;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::index_searcher::{
  self, IndexSearcher, IndexSearcherHook, get_max_clause_count,
};
use crate::core::search::leaf_collector::LeafCollector;
use crate::core::search::match_all_docs_query::MatchAllDocsQuery;
use crate::core::search::multi_term_query::SCORING_BOOLEAN_REWRITE;
use crate::core::search::phrase_query::PhraseQuery;
use crate::core::search::query::{Query, QueryBase, QueryRef};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::scorable::Scorable;
use crate::core::search::score_doc::ScoreDoc;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::ScorerKind;
use crate::core::search::similarities_impl::classic_similarity;
use crate::core::search::simple_collector::SimpleCollector;
use crate::core::search::term_query::TermQuery;
use crate::core::search::top_docs::TopDocsLike;
use crate::core::search::weight::Weight;
use crate::core::search::wildcard_query::WildcardQuery;
use crate::core::util::CoreHelper;
use crate::core::util::automation::operations::Operations;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::test_framework::core::analysis::mock_analyzer::MockAnalyzer;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::search::fixed_bit_set_collector::FixedBitSetCollector;
use crate::test_framework::core::search::query_utils::QueryUtils;
use crate::test_framework::core::search::test_boolean_query::CountingIndexSearcher;
use crate::test_framework::core::util::test_util::TestUtil;
use rand::RngExt;
use rand::prelude::SliceRandom;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[allow(dead_code)] // for quick search
struct TestBooleanQuery;

#[test]
fn test_equality() -> Result<()> {
  let mut bq1 = Builder::new();
  bq1.add(
    Query::Term(TermQuery::new(Term::from_text("field", "value1"))),
    Occur::Should,
  )?;
  bq1.add(
    Query::Term(TermQuery::new(Term::from_text("field", "value2"))),
    Occur::Should,
  )?;
  let mut nested1 = Builder::new();
  nested1.add(
    Query::Term(TermQuery::new(Term::from_text("field", "nestedvalue1"))),
    Occur::Should,
  )?;
  nested1.add(
    Query::Term(TermQuery::new(Term::from_text("field", "nestedvalue2"))),
    Occur::Should,
  )?;
  bq1.add(Query::Boolean(nested1.build()), Occur::Should)?;

  let mut bq2 = Builder::new();
  bq2.add(
    Query::Term(TermQuery::new(Term::from_text("field", "value1"))),
    Occur::Should,
  )?;
  bq2.add(
    Query::Term(TermQuery::new(Term::from_text("field", "value2"))),
    Occur::Should,
  )?;
  let mut nested2 = Builder::new();
  nested2.add(
    Query::Term(TermQuery::new(Term::from_text("field", "nestedvalue1"))),
    Occur::Should,
  )?;
  nested2.add(
    Query::Term(TermQuery::new(Term::from_text("field", "nestedvalue2"))),
    Occur::Should,
  )?;
  bq2.add(Query::Boolean(nested2.build()), Occur::Should)?;

  assert_eq!(bq1.build(), bq2.build());
  Ok(())
}
#[test]
fn test_equality_does_not_depend_on_order() -> Result<()> {
  let mut random = random();

  let queries = [
    TermQuery::new(Term::from_text("foo", "bar")),
    TermQuery::new(Term::from_text("foo", "baz")),
  ];

  for _ in 0..10 {
    let num_clauses = random.random_range(0..20) as usize;

    let mut clauses: Vec<BooleanClause> = Vec::with_capacity(num_clauses);
    for _ in 0..num_clauses {
      let mut query = if random.random_bool(0.5) {
        Query::Term(queries[0].clone())
      } else {
        Query::Term(queries[1].clone())
      };

      if random.random_bool(0.5) {
        let boost = random.random();
        query = Query::Boost(BoostQuery::new(query, boost)?);
      }

      let occur = match random.random_range(0..4) {
        0 => Occur::Must,
        1 => Occur::Filter,
        2 => Occur::Should,
        _ => Occur::MustNot,
      };

      clauses.push(BooleanClause { query, occur });
    }

    let min_should_match = random.random_range(0..5);

    let mut bq1_builder = Builder::new();
    bq1_builder.set_minimum_number_should_match(min_should_match);
    for clause in &clauses {
      bq1_builder.add_clause(clause.clone())?;
    }
    let bq1 = bq1_builder.build();

    clauses.shuffle(&mut random);

    let mut bq2_builder = Builder::new();
    bq2_builder.set_minimum_number_should_match(min_should_match);
    for clause in &clauses {
      bq2_builder.add_clause(clause.clone())?;
    }
    let bq2 = bq2_builder.build();

    QueryUtils::check_equal(&bq1, &bq2)
  }

  Ok(())
}
#[test]
fn test_equality_on_duplicate_should_clauses() -> Result<()> {
  let mut random = random();

  let min_should_match = random.random_range(0..2);

  let mut bq1_builder = Builder::new();
  bq1_builder.set_minimum_number_should_match(min_should_match);
  bq1_builder.add(TermQuery::new(Term::from_text("foo", "bar")), Occur::Should)?;
  let bq1 = bq1_builder.build();

  let mut bq2_builder = Builder::new();
  bq2_builder.set_minimum_number_should_match(bq1.get_minimum_number_should_match());
  bq2_builder.add(TermQuery::new(Term::from_text("foo", "bar")), Occur::Should)?;
  bq2_builder.add(TermQuery::new(Term::from_text("foo", "bar")), Occur::Should)?;
  let bq2 = bq2_builder.build();

  QueryUtils::check_unequal(&bq1, &bq2);
  Ok(())
}
#[test]
fn test_equality_on_duplicate_must_clauses() -> Result<()> {
  let mut random = random();

  let min_should_match = random.random_range(0..2);

  let mut bq1_builder = Builder::new();
  bq1_builder.set_minimum_number_should_match(min_should_match);
  bq1_builder.add(TermQuery::new(Term::from_text("foo", "bar")), Occur::Must)?;
  let bq1 = bq1_builder.build();

  let mut bq2_builder = Builder::new();
  bq2_builder.set_minimum_number_should_match(bq1.get_minimum_number_should_match());
  bq2_builder.add(TermQuery::new(Term::from_text("foo", "bar")), Occur::Must)?;
  bq2_builder.add(TermQuery::new(Term::from_text("foo", "bar")), Occur::Must)?;
  let bq2 = bq2_builder.build();

  QueryUtils::check_unequal(&bq1, &bq2);
  Ok(())
}
#[test]
fn test_equality_on_duplicate_filter_clauses() -> Result<()> {
  let mut random = random();

  let min_should_match = random.random_range(0..2);

  let mut bq1_builder = Builder::new();
  bq1_builder.set_minimum_number_should_match(min_should_match);
  bq1_builder.add(TermQuery::new(Term::from_text("foo", "bar")), Occur::Filter)?;
  let bq1 = bq1_builder.build();

  let mut bq2_builder = Builder::new();
  bq2_builder.set_minimum_number_should_match(bq1.get_minimum_number_should_match());
  bq2_builder.add(TermQuery::new(Term::from_text("foo", "bar")), Occur::Filter)?;
  bq2_builder.add(TermQuery::new(Term::from_text("foo", "bar")), Occur::Filter)?;
  let bq2 = bq2_builder.build();

  QueryUtils::check_equal(&bq1, &bq2);
  Ok(())
}

#[test]
fn test_equality_on_duplicate_must_not_clauses() -> Result<()> {
  let mut random = random();

  let min_should_match = random.random_range(0..2);

  let mut bq1_builder = Builder::new();
  bq1_builder.set_minimum_number_should_match(min_should_match);
  bq1_builder.add(Query::MatchAllDocs(MatchAllDocsQuery::new()), Occur::Must)?;
  bq1_builder.add(TermQuery::new(Term::from_text("foo", "bar")), Occur::Filter)?;
  let bq1 = bq1_builder.build();

  let mut bq2_builder = Builder::new();
  bq2_builder.set_minimum_number_should_match(bq1.get_minimum_number_should_match());
  bq2_builder.add(Query::MatchAllDocs(MatchAllDocsQuery::new()), Occur::Must)?;
  bq2_builder.add(TermQuery::new(Term::from_text("foo", "bar")), Occur::Filter)?;
  bq2_builder.add(TermQuery::new(Term::from_text("foo", "bar")), Occur::Filter)?;
  let bq2 = bq2_builder.build();

  QueryUtils::check_equal(&bq1, &bq2);
  Ok(())
}

#[test]
fn test_hash_code_is_stable() -> Result<()> {
  let mut random = random();

  let t1 = Term::from_text("foo", TestUtil::random_simple_string(&mut random));
  let t2 = Term::from_text("foo", TestUtil::random_simple_string(&mut random));

  let mut bq_builder = Builder::new();
  bq_builder.add(TermQuery::new(t1), Occur::Should)?;
  bq_builder.add(TermQuery::new(t2), Occur::Should)?;
  let bq = bq_builder.build();

  let hash1 = CoreHelper::calculate_hash(&bq);
  assert_eq!(hash1, CoreHelper::calculate_hash(&bq));

  Ok(())
}
#[test]
fn test_too_many_clauses() -> Result<()> {
  let mut bq = Builder::new();

  let max = get_max_clause_count();

  for i in 0..max {
    bq.add(
      TermQuery::new(Term::from_text("foo", format!("bar-{}", i))),
      Occur::Should,
    )?;
  }

  let res = bq.add(
    TermQuery::new(Term::from_text("foo", "bar-MAX")),
    Occur::Should,
  );

  assert!(matches!(res, Err(LuceneError::TooManyClauses(_))));
  Ok(())
}

#[test]
fn test_null_or_sub_scorer() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, dir.clone())?;

  let mut field_to_type = HashMap::new();

  let mut doc = Document::new();
  doc.add(new_text_field(
    &mut random,
    "field",
    "a b c d",
    Store::No,
    &mut field_to_type,
  )?);
  w.add_document(&mut random, doc)?;

  let reader = w.get_reader(&mut random)?;
  let mut s = new_searcher_with_reader(reader)?;
  s.set_similarity(classic_similarity::new());

  let mut q = Builder::new();
  q.add(TermQuery::new(Term::from_text("field", "a")), Occur::Should)?;

  let pq = PhraseQuery::from_terms_no_slop("field", Vec::<&str>::new().as_slice())?;
  q.add(pq.clone(), Occur::Should)?;
  assert_eq!(1, s.search(q.build(), 10)?.total_hits.value());

  let mut q = Builder::new();
  let pq = PhraseQuery::from_terms_no_slop("field", Vec::<&str>::new().as_slice())?;
  q.add(TermQuery::new(Term::from_text("field", "a")), Occur::Should)?;
  q.add(pq.clone(), Occur::Must)?;
  assert_eq!(0, s.search(q.build(), 10)?.total_hits.value());

  let dmq = DisjunctionMaxQuery::new(
    vec![
      TermQuery::new(Term::from_text("field", "a")).into(),
      pq.into(),
    ],
    1.0,
  )?;
  assert_eq!(1, s.search(dmq, 10)?.total_hits.value());
  w.close(&mut random)?;
  Ok(())
}
#[test]
fn test_de_morgan() -> Result<()> {
  let mut random = random();
  let dir1 = new_directory_shared(&mut random)?;
  let iw1 = RandomIndexWriter::new(&mut random, dir1)?;
  let mut field_to_type = HashMap::new();
  let mut doc1 = Document::new();
  doc1.add(new_text_field(
    &mut random,
    "field",
    "foo bar",
    Store::No,
    &mut field_to_type,
  )?);
  iw1.add_document(&mut random, doc1)?;
  let reader1 = iw1.get_reader(&mut random)?;
  iw1.close(&mut random)?;

  let dir2 = new_directory_shared(&mut random)?;
  let iw2 = RandomIndexWriter::new(&mut random, dir2)?;
  let mut doc2 = Document::new();
  doc2.add(new_text_field(
    &mut random,
    "field",
    "foo baz",
    Store::No,
    &mut field_to_type,
  )?);
  iw2.add_document(&mut random, doc2)?;
  let reader2 = iw2.get_reader(&mut random)?;
  iw2.close(&mut random)?;

  let mut query = Builder::new();
  query.add(TermQuery::new(Term::from_text("field", "foo")), Occur::Must)?;
  let wildcard_query = WildcardQuery::with_rewrite(
    Term::from_text("field", "ba*"),
    Operations::DEFAULT_DETERMINIZE_WORK_LIMIT as i32,
    SCORING_BOOLEAN_REWRITE,
  )?;
  query.add(wildcard_query, Occur::MustNot)?;
  let query = query.build();

  let multi_reader = Arc::new(MultiReader::new(vec![reader1, reader2])?);
  let searcher = new_searcher_with_reader(multi_reader.clone())?;
  assert_eq!(0, searcher.search(query.clone(), 10)?.total_hits.value());

  let searcher = index_searcher::from_reader_with_threads(multi_reader, 2)?;
  assert_eq!(0, searcher.search(query, 10)?.total_hits.value());
  Ok(())
}
#[test]
fn test_bs2_disjunction_next_vs_advance() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;
  let mut field_to_type = HashMap::new();

  let num_docs = at_least(&mut random, 300);
  for _doc_upto in 0..num_docs {
    let mut contents = String::from("a");
    if random.random_range(0..20) <= 16 {
      contents.push_str(" b");
    }
    if random.random_range(0..20) <= 8 {
      contents.push_str(" c");
    }
    if random.random_range(0..20) <= 4 {
      contents.push_str(" d");
    }
    if random.random_range(0..20) <= 2 {
      contents.push_str(" e");
    }
    if random.random_range(0..20) <= 1 {
      contents.push_str(" f");
    }

    let mut doc = Document::new();
    doc.add(new_text_field(
      &mut random,
      "field",
      &contents,
      Store::No,
      &mut field_to_type,
    )?);
    writer.add_document(&mut random, doc)?;
  }
  writer.force_merge(&mut random, 1)?;
  let reader = writer.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;
  writer.close(&mut random)?;

  for _ in 0..(10 * random_multiplier()) {
    let mut terms: Vec<&'static str> = vec!["a", "b", "c", "d", "e", "f"];
    let num_terms = random.random_range(1..(terms.len() + 1)) as usize;
    while terms.len() > num_terms {
      let idx = random.random_range(0..terms.len()) as usize;
      terms.remove(idx);
    }

    let mut bq = Builder::new();
    for term in &terms {
      bq.add(
        TermQuery::new(Term::from_text("field", *term)),
        Occur::Should,
      )?;
    }
    let q: Query = bq.build().into();

    let rewritten = searcher.rewrite(q.clone())?;
    let weight = rewritten.create_weight(&searcher, &ScoreMode::Complete, 1.0)?;

    let ctx = &searcher.get_leaf_contexts()?[0];
    let mut scorer = weight.scorer(ctx, &searcher)?.unwrap();

    // First pass: just use next_doc() to gather all hits
    let mut hits = Vec::new();
    loop {
      let doc_id = scorer.iterator_mut().next_doc()?;
      if doc_id == NO_MORE_DOCS {
        break;
      }
      hits.push(ScoreDoc::new(scorer.doc_id()?, scorer.score()?));
    }

    // Now, randomly next/advance through the list and verify exact match
    for _ in 0..10 {
      let rewritten = searcher.rewrite(q.clone())?;
      let weight = rewritten.create_weight(&searcher, &ScoreMode::Complete, 1.0)?;
      let mut scorer = weight.scorer(ctx, &searcher)?.unwrap();

      let mut upto: i32 = -1;
      while (upto as usize) < hits.len() {
        let next_upto: usize;
        let next_doc: i32;
        let left = hits.len() as i32 - upto;

        if left == 1 || random.random_bool(0.5) {
          next_upto = (upto + 1) as usize;
          next_doc = scorer.iterator_mut().next_doc()?;
        } else {
          let inc = random.random_range(1..left) - 1;
          let inc = inc.max(1);
          next_upto = (upto + inc) as usize;
          next_doc = scorer.iterator_mut().advance(hits[next_upto].doc)?;
        }

        if next_upto == hits.len() {
          assert_eq!(NO_MORE_DOCS, next_doc);
        } else {
          let hit = &hits[next_upto];
          assert_eq!(hit.doc, next_doc);
          let actual = scorer.score()?;
          assert_eq!(
            hit.score, actual,
            "doc {} has wrong score: expected={} actual={}",
            hit.doc, hit.score, actual
          );
        }

        upto = next_upto as i32;
      }
    }
  }

  Ok(())
}

#[test]
fn test_min_should_match_leniency() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mock = MockAnalyzer::new(&mut random);
  let iwc = new_index_writer_config_with_analyzer(&mut random, mock)?;
  let writer = IndexWriter::new(dir.clone(), iwc)?;
  let mut field_to_type: HashMap<String, FieldType> = HashMap::new();
  let mut doc = Document::new();
  doc.add(new_text_field(
    &mut random,
    "field",
    "a b c d",
    No,
    &mut field_to_type,
  )?);
  writer.add_document(doc)?;

  let reader = directory_reader::open_from_writer(&writer)?;
  let searcher = new_searcher_with_reader(reader)?;

  let mut bq = Builder::new();
  bq.add(TermQuery::new(Term::from_text("field", "a")), Occur::Should)?;
  bq.add(TermQuery::new(Term::from_text("field", "b")), Occur::Should)?;

  // No doc can match: only 2 SHOULD clauses, but min_should_match = 4
  bq.set_minimum_number_should_match(4);
  let query = bq.build();

  let top_docs = searcher.search(query, 1)?;
  assert_eq!(0, top_docs.total_hits.value());

  Ok(())
}
fn get_matches<IRC, T>(searcher: &IndexSearcher<IRC>, query: T) -> Result<FixedBitSet>
where
  IRC: IndexReaderContext + Sync + 'static,
  T: Into<Query>,
{
  let max_doc = searcher.get_index_reader().max_doc()?;
  let manager = FixedBitSetCollector::create_manager(max_doc);
  searcher.search_with_collector_manager(query, &manager)
}
#[test]
fn test_filter_clause_behaves_like_must() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, dir.clone())?;

  let mut field_to_type = HashMap::new();

  let mut doc = Document::new();
  let mut f = new_text_field(
    &mut random,
    "field",
    "a b c d",
    Store::No,
    &mut field_to_type,
  )?;
  doc.add(f.clone());
  w.add_document(&mut random, doc.clone())?;

  f.set_string_value("b d")?;
  let mut doc = Document::new();
  doc.add(f.clone());
  w.add_document(&mut random, doc.clone())?;

  f.set_string_value("d")?;
  let mut doc = Document::new();
  doc.add(f);
  w.add_document(&mut random, doc)?;

  w.commit(&mut random)?;

  let reader = w.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;

  let cases: Vec<Vec<&str>> = vec![
    vec!["a", "d"],
    vec!["a", "b", "d"],
    vec!["d"],
    vec!["e"],
    vec![],
  ];

  for required_terms in cases {
    let mut bq1 = Builder::new();
    let mut bq2 = Builder::new();

    for term in required_terms {
      let q = TermQuery::new(Term::from_text("field", term));
      bq1.add(q.clone(), Occur::Must)?;
      bq2.add(q, Occur::Filter)?;
    }

    let matches1 = get_matches(&searcher, bq1.build())?;
    let matches2 = get_matches(&searcher, bq2.build())?;

    assert_eq!(matches1, matches2);
  }
  w.close(&mut random)?;
  Ok(())
}

fn assert_same_scores_without_filters<IRC>(
  searcher: &IndexSearcher<IRC>,
  bq: BooleanQuery,
) -> Result<()>
where
  IRC: IndexReaderContext + Sync + 'static,
{
  let mut bq2_builder = Builder::new();
  let min_should_match = bq.get_minimum_number_should_match();
  for c in bq.clone().clauses.into_iter() {
    if *c.occur() != Occur::Filter {
      bq2_builder.add_clause(c)?;
    }
  }
  bq2_builder.set_minimum_number_should_match(min_should_match);
  let bq2 = bq2_builder.build();

  let matched = Arc::new(AtomicBool::new(false));
  let collector_manager = CollectorManagerImpl::new(searcher, matched.clone(), bq2);

  searcher.search_with_collector_manager(bq, &collector_manager)?;

  assert!(matched.load(Ordering::SeqCst));
  Ok(())
}

#[test]
fn test_filter_clause_does_not_impact_score() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let w = RandomIndexWriter::new(&mut random, dir.clone())?;

  let mut field_to_type = HashMap::new();

  let mut doc = Document::new();
  let mut f = new_text_field(
    &mut random,
    "field",
    "a b c d",
    Store::No,
    &mut field_to_type,
  )?;
  doc.add(f.clone());
  w.add_document(&mut random, doc.clone())?;

  f.set_string_value("b d")?;
  let mut doc = Document::new();
  doc.add(f.clone());
  w.add_document(&mut random, doc.clone())?;

  f.set_string_value("a d")?;
  let mut doc = Document::new();
  doc.add(f);
  w.add_document(&mut random, doc)?;

  w.commit(&mut random)?;

  let reader = w.get_reader(&mut random)?;
  let searcher = new_searcher_with_reader(reader)?;

  let mut q_builder = Builder::new();
  q_builder.add(TermQuery::new(Term::from_text("field", "a")), Occur::Filter)?;
  assert_same_scores_without_filters(&searcher, q_builder.clone().build())?;

  q_builder.add(TermQuery::new(Term::from_text("field", "b")), Occur::Filter)?;
  let mut q = q_builder.clone().build();
  assert_same_scores_without_filters(&searcher, q.clone())?;

  q_builder.add(TermQuery::new(Term::from_text("field", "c")), Occur::Should)?;
  q = q_builder.build();
  assert_same_scores_without_filters(&searcher, q.clone())?;

  let mut q_builder = Builder::new();
  q_builder.add(TermQuery::new(Term::from_text("field", "a")), Occur::Filter)?;
  q_builder.add(TermQuery::new(Term::from_text("field", "e")), Occur::Should)?;
  q = q_builder.build();
  assert_same_scores_without_filters(&searcher, q.clone())?;

  let mut q_builder = Builder::new();
  q_builder.add(TermQuery::new(Term::from_text("field", "a")), Occur::Filter)?;
  q_builder.add(TermQuery::new(Term::from_text("field", "d")), Occur::Must)?;
  q = q_builder.build();
  assert_same_scores_without_filters(&searcher, q.clone())?;

  let mut q_builder = Builder::new();
  q_builder.add(TermQuery::new(Term::from_text("field", "b")), Occur::Filter)?;
  q_builder.add(TermQuery::new(Term::from_text("field", "a")), Occur::Should)?;
  q_builder.add(TermQuery::new(Term::from_text("field", "d")), Occur::Should)?;
  q_builder.set_minimum_number_should_match(1);
  q = q_builder.build();
  assert_same_scores_without_filters(&searcher, q)?;

  w.close(&mut random)?;
  Ok(())
}

#[test]
fn test_conjunction_propagates_approximations() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;
  let mut field_to_type = HashMap::new();

  let mut doc = Document::new();
  doc.add(new_text_field(
    &mut random,
    "field",
    "a b c",
    Store::No,
    &mut field_to_type,
  )?);
  writer.add_document(&mut random, doc)?;
  writer.commit(&mut random)?;

  let reader = writer.get_reader(&mut random)?;
  // not new_searcher_with_reader to not have the asserting wrappers
  // and perform type checks.
  let mut searcher = index_searcher::from_reader(reader)?;
  searcher.set_query_cache(None); // to still have approximations

  let pq: Query = PhraseQuery::from_terms(0, "field", &["a", "b"])?.into();

  let mut b = Builder::new();
  b.add(pq, Occur::Must)?;
  b.add(TermQuery::new(Term::from_text("field", "c")), Occur::Filter)?;
  let q: Query = b.build().into();

  let rewritten = searcher.rewrite(q)?;
  let weight = rewritten.create_weight(&searcher, &ScoreMode::Complete, 1.0)?;

  let ctx = &searcher.get_leaf_contexts()?[0];
  let scorer = weight.scorer(ctx, &searcher)?.unwrap();

  assert_eq!(scorer.kind(), ScorerKind::Conjunction);
  assert!(scorer.two_phase_iterator().is_some());

  Ok(())
}

#[test]
fn test_disjunction_propagates_approximations() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;
  let mut field_to_type = HashMap::new();

  let mut doc = Document::new();
  doc.add(new_text_field(
    &mut random,
    "field",
    "a b c",
    Store::No,
    &mut field_to_type,
  )?);
  writer.add_document(&mut random, doc)?;
  writer.commit(&mut random)?;

  let reader = writer.get_reader(&mut random)?;
  let mut searcher = index_searcher::from_reader(reader)?;
  searcher.set_query_cache(None); // to still have approximations

  let pq: Query = PhraseQuery::from_terms(0, "field", &["a", "b"])?.into();

  let mut b = Builder::new();
  b.add(pq, Occur::Should)?;
  b.add(TermQuery::new(Term::from_text("field", "c")), Occur::Should)?;
  let q: Query = b.build().into();

  let rewritten = searcher.rewrite(q)?;
  let weight = rewritten.create_weight(&searcher, &ScoreMode::Complete, 1.0)?;

  let ctx = &searcher.get_leaf_contexts()?[0];
  let scorer = weight.scorer(ctx, &searcher)?.unwrap();

  assert_eq!(scorer.kind(), ScorerKind::Disjunction);
  assert!(scorer.two_phase_iterator().is_some());

  Ok(())
}

#[test]
fn test_boosted_scorer_propagates_approximations() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;
  let mut field_to_type = HashMap::new();

  let mut doc = Document::new();
  doc.add(new_text_field(
    &mut random,
    "field",
    "a b c",
    Store::No,
    &mut field_to_type,
  )?);
  writer.add_document(&mut random, doc)?;
  writer.commit(&mut random)?;

  let reader = writer.get_reader(&mut random)?;
  // not new_searcher_with_reader to not have the asserting wrappers
  // and perform type checks.
  let mut searcher = index_searcher::from_reader(reader)?;
  searcher.set_query_cache(None); // to still have approximations

  let pq: Query = PhraseQuery::from_terms(0, "field", &["a", "b"])?.into();

  let mut b = Builder::new();
  b.add(pq, Occur::Should)?;
  b.add(TermQuery::new(Term::from_text("field", "d")), Occur::Should)?;
  let q: Query = b.build().into();

  let rewritten = searcher.rewrite(q)?;
  let weight = rewritten.create_weight(&searcher, &ScoreMode::Complete, 1.0)?;

  let ctx = &searcher.get_leaf_contexts()?[0];
  let scorer = weight.scorer(ctx, &searcher)?.unwrap();

  assert_eq!(scorer.kind(), ScorerKind::Phrase);
  assert!(scorer.two_phase_iterator().is_some());

  Ok(())
}

#[test]
fn test_exclusion_propagates_approximations() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;
  let mut field_to_type = HashMap::new();

  let mut doc = Document::new();
  doc.add(new_text_field(
    &mut random,
    "field",
    "a b c",
    Store::No,
    &mut field_to_type,
  )?);
  writer.add_document(&mut random, doc)?;
  writer.commit(&mut random)?;

  let reader = writer.get_reader(&mut random)?;
  let mut searcher = index_searcher::from_reader(reader)?;
  searcher.set_query_cache(None); // to still have approximations

  let pq: Query = PhraseQuery::from_terms(0, "field", &["a", "b"])?.into();

  let mut b = Builder::new();
  b.add(pq, Occur::Should)?;
  b.add(
    TermQuery::new(Term::from_text("field", "c")),
    Occur::MustNot,
  )?;
  let q: Query = b.build().into();

  let rewritten = searcher.rewrite(q)?;
  let weight = rewritten.create_weight(&searcher, &ScoreMode::Complete, 1.0)?;

  let ctx = &searcher.get_leaf_contexts()?[0];
  let scorer = weight.scorer(ctx, &searcher)?.unwrap();

  assert_eq!(scorer.kind(), ScorerKind::ReqExcl);
  assert!(scorer.two_phase_iterator().is_some());

  Ok(())
}

#[test]
fn test_req_opt_propagates_approximations() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;
  let mut field_to_type = HashMap::new();

  let mut doc = Document::new();
  doc.add(new_text_field(
    &mut random,
    "field",
    "a b c",
    Store::No,
    &mut field_to_type,
  )?);
  writer.add_document(&mut random, doc)?;
  writer.commit(&mut random)?;

  let reader = writer.get_reader(&mut random)?;
  let mut searcher = index_searcher::from_reader(reader)?;
  searcher.set_query_cache(None); // to still have approximations

  let pq: Query = PhraseQuery::from_terms(0, "field", &["a", "b"])?.into();

  let mut b = Builder::new();
  b.add(pq, Occur::Must)?;
  b.add(TermQuery::new(Term::from_text("field", "c")), Occur::Should)?;
  let q: Query = b.build().into();

  let rewritten = searcher.rewrite(q)?;
  let weight = rewritten.create_weight(&searcher, &ScoreMode::Complete, 1.0)?;

  let ctx = &searcher.get_leaf_contexts()?[0];
  let scorer = weight.scorer(ctx, &searcher)?.unwrap();

  assert_eq!(scorer.kind(), ScorerKind::ReqOptSum);
  assert!(scorer.two_phase_iterator().is_some());

  Ok(())
}

#[test]
fn test_query_matches_count() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let writer = RandomIndexWriter::new(&mut random, dir.clone())?;
  let mut field_to_type = HashMap::new();

  let random_num_docs = TestUtil::next_int(&mut random, 10, 101) as usize;
  let mut num_matching_docs: i32 = 0;

  for _i in 0..random_num_docs {
    let mut doc = Document::new();

    if random.random_bool(0.5) {
      let text = format!("a b c {}", random.random::<i32>());
      doc.add(new_text_field(
        &mut random,
        "field",
        &text,
        Store::No,
        &mut field_to_type,
      )?);
      num_matching_docs += 1;
    } else {
      let text = random.random::<i32>().to_string();
      doc.add(new_text_field(
        &mut random,
        "field",
        &text,
        Store::No,
        &mut field_to_type,
      )?);
    }

    writer.add_document(&mut random, doc)?;
  }
  writer.commit(&mut random)?;

  let reader = writer.get_reader(&mut random)?;
  let searcher = index_searcher::from_reader(reader)?;

  let mut b = Builder::new();
  b.add(
    PhraseQuery::from_terms(0, "field", &["a", "b"])?,
    Occur::Should,
  )?;
  b.add(TermQuery::new(Term::from_text("field", "c")), Occur::Should)?;
  let built_query: Query = b.build().into();

  assert_eq!(num_matching_docs, searcher.count(built_query.clone())?);

  Ok(())
}

#[test]
fn test_conjunction_matches_count() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;

  let mut doc = Document::new();
  let mut long_point = LongPoint::new("long", [3i64])?;
  doc.add(long_point.clone());
  let mut string_field = StringField::from_string("string", "abc", No)?;
  doc.add(string_field.clone());
  writer.add_document(doc.clone())?;

  long_point.set_long_value(10)?;
  string_field.set_string_value("xyz")?;
  doc = Document::new();
  doc.add(string_field);
  doc.add(long_point);
  writer.add_document(doc)?;

  let reader = directory_reader::open_from_writer(&writer)?;

  let searcher = index_searcher::from_reader(reader)?;

  let mut builder = Builder::new();
  builder
    .add(
      TermQuery::new(Term::from_text("string", "abc")),
      Occur::Must,
    )?
    .add(LongPoint::new_exact_query("long", 3)?, Occur::Filter)?;
  let query = builder.build();

  let weight = searcher.create_weight(query, ScoreMode::Complete, 1.0)?;
  // Both queries match a single doc, BooleanWeight can't figure out the count of the conjunction
  assert_eq!(
    -1,
    weight.count(&searcher.get_leaf_contexts()?[0], &searcher)?
  );

  let mut builder = Builder::new();
  builder
    .add(
      TermQuery::new(Term::from_text("string", "missing")),
      Occur::Must,
    )?
    .add(LongPoint::new_exact_query("long", 3)?, Occur::Filter)?;
  let query = builder.build();

  let weight = searcher.create_weight(query, ScoreMode::Complete, 1.0)?;
  // One query has a count of 0, the conjunction has a count of 0 too
  assert_eq!(
    0,
    weight.count(&searcher.get_leaf_contexts()?[0], &searcher)?
  );

  let mut builder = Builder::new();
  builder
    .add(
      TermQuery::new(Term::from_text("string", "abc")),
      Occur::Must,
    )?
    .add(LongPoint::new_exact_query("long", 5)?, Occur::Filter)?;
  let query = builder.build();

  let weight = searcher.create_weight(query, ScoreMode::Complete, 1.0)?;
  // One query has a count of 0, the conjunction has a count of 0 too
  assert_eq!(
    0,
    weight.count(&searcher.get_leaf_contexts()?[0], &searcher)?
  );

  // FILTER matches all docs → conjunction count equals MUST count
  let mut builder = Builder::new();
  builder
    .add(
      TermQuery::new(Term::from_text("string", "abc")),
      Occur::Must,
    )?
    .add(LongPoint::new_range_query("long", 0, 10)?, Occur::Filter)?;
  let query = builder.build();

  let weight = searcher.create_weight(query, ScoreMode::Complete, 1.0)?;
  // One query matches all docs, the count of the conjunction is the count of the other query
  assert_eq!(
    1,
    weight.count(&searcher.get_leaf_contexts()?[0], &searcher)?
  );

  let mut builder = Builder::new();
  builder
    .add(Query::MatchAllDocs(MatchAllDocsQuery::new()), Occur::Must)?
    .add(LongPoint::new_range_query("long", 1, 5)?, Occur::Filter)?;
  let query = builder.build();

  let weight = searcher.create_weight(query, ScoreMode::Complete, 1.0)?;
  // One query matches all docs, the count of the conjunction is the count of the other query
  assert_eq!(
    1,
    weight.count(&searcher.get_leaf_contexts()?[0], &searcher)?
  );

  Ok(())
}
#[test]
fn test_disjunction_matches_count() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;

  let mut doc = Document::new();
  let mut long_point = LongPoint::new("long", [3i64])?;
  let mut long_point_3dim = LongPoint::new("long3dim", [3i64, 4i64, 5i64])?;
  doc.add(long_point.clone());
  doc.add(long_point_3dim.clone());

  let mut string_field = StringField::from_string("string", "abc", No)?;
  doc.add(string_field.clone());

  writer.add_document(doc.clone())?;

  long_point.set_long_value(10)?;
  long_point_3dim.set_long_values([10i64, 11i64, 12i64])?;
  string_field.set_string_value("xyz")?;

  doc = Document::new();
  doc.add(string_field);
  doc.add(long_point);
  doc.add(long_point_3dim);
  writer.add_document(doc)?;

  let reader = directory_reader::open_from_writer(&writer)?;
  let searcher = index_searcher::from_reader(reader)?;

  let leaf = &searcher.get_leaf_contexts()?[0];

  // Both queries match a single doc, BooleanWeight can't figure out the count of the disjunction
  let mut builder = Builder::new();
  builder
    .add(
      TermQuery::new(Term::from_text("string", "abc")),
      Occur::Should,
    )?
    .add(LongPoint::new_exact_query("long", 3)?, Occur::Should)?;
  let query = builder.build();
  // Both queries match a single doc, BooleanWeight can't figure out the count of the disjunction
  let weight = searcher.create_weight(query, ScoreMode::Complete, 1.0)?;
  assert_eq!(-1, weight.count(leaf, &searcher)?);

  // One query has a count of 0, the disjunction count is the other count
  let mut builder = Builder::new();
  builder
    .add(
      TermQuery::new(Term::from_text("string", "missing")),
      Occur::Should,
    )?
    .add(LongPoint::new_exact_query("long", 3)?, Occur::Should)?;
  let query = builder.build();
  // One query has a count of 0, the disjunction count is the other count
  let weight = searcher.create_weight(query, ScoreMode::Complete, 1.0)?;
  assert_eq!(1, weight.count(leaf, &searcher)?);

  let mut builder = Builder::new();
  builder
    .add(
      TermQuery::new(Term::from_text("string", "abc")),
      Occur::Should,
    )?
    .add(LongPoint::new_exact_query("long", 5)?, Occur::Should)?;
  let query = builder.build();
  // One query has a count of 0, the disjunction count is the other count
  let weight = searcher.create_weight(query, ScoreMode::Complete, 1.0)?;
  assert_eq!(1, weight.count(leaf, &searcher)?);

  // One query matches all docs, the count of the disjunction is the number of docs
  let mut builder = Builder::new();
  builder
    .add(
      TermQuery::new(Term::from_text("string", "abc")),
      Occur::Should,
    )?
    .add(LongPoint::new_range_query("long", 0, 10)?, Occur::Should)?;
  let query = builder.build();
  let weight = searcher.create_weight(query, ScoreMode::Complete, 1.0)?;
  // One query matches all docs, the count of the disjunction is the number of docs

  assert_eq!(2, weight.count(leaf, &searcher)?);

  let mut builder = Builder::new();
  builder
    .add(Query::MatchAllDocs(MatchAllDocsQuery::new()), Occur::Should)?
    .add(LongPoint::new_range_query("long", 1, 5)?, Occur::Should)?;
  let query = builder.build();
  let weight = searcher.create_weight(query, ScoreMode::Complete, 1.0)?;
  // One query matches all docs, the count of the disjunction is the number of docs
  assert_eq!(2, weight.count(leaf, &searcher)?);

  // Unknown count query on 3D long point range
  let lower = [4i64, 5i64, 6i64];
  let upper = [9i64, 10i64, 11i64];
  let unknown_count_query = LongPoint::new_range_query_n("long3dim", &lower, &upper)?;

  debug_assert_eq!(1, searcher.get_leaf_contexts()?.len());
  let w = searcher.create_weight(unknown_count_query.clone(), ScoreMode::Complete, 1.0)?;
  assert_eq!(-1, w.count(leaf, &searcher)?);

  // count of the first MUST_NOT clause is unknown, but the second MUST_NOT clause matches all docs
  let mut builder = Builder::new();
  builder
    .add(
      TermQuery::new(Term::from_text("string", "xyz")),
      Occur::Must,
    )?
    .add(unknown_count_query.clone(), Occur::MustNot)?
    .add(
      Query::MatchAllDocs(MatchAllDocsQuery::new()),
      Occur::MustNot,
    )?;
  let query = builder.build();
  let weight = searcher.create_weight(query, ScoreMode::Complete, 1.0)?;
  // count of the first MUST_NOT clause is unknown, but the second MUST_NOT clause matches all
  // docs
  assert_eq!(0, weight.count(leaf, &searcher)?);

  let mut builder = Builder::new();
  builder
    .add(
      TermQuery::new(Term::from_text("string", "xyz")),
      Occur::Must,
    )?
    .add(unknown_count_query.clone(), Occur::MustNot)?
    .add(
      TermQuery::new(Term::from_text("string", "abc")),
      Occur::MustNot,
    )?;
  let query = builder.build();
  let weight = searcher.create_weight(query, ScoreMode::Complete, 1.0)?;
  // count of the first MUST_NOT clause is unknown, though the second MUST_NOT clause matche one
  // doc, we can't figure out the number of
  // docs
  assert_eq!(-1, weight.count(leaf, &searcher)?);

  // test pure disjunction
  let mut builder = Builder::new();
  builder
    .add(unknown_count_query.clone(), Occur::Should)?
    .add(Query::MatchAllDocs(MatchAllDocsQuery::new()), Occur::Should)?;
  let query = builder.build();
  let weight = searcher.create_weight(query, ScoreMode::Complete, 1.0)?;
  // count of the first SHOULD clause is unknown, but the second SHOULD clause matches all docs
  assert_eq!(2, weight.count(leaf, &searcher)?);

  // count of the first SHOULD clause is unknown, though the second SHOULD clause matches one doc
  let mut builder = Builder::new();
  builder.add(unknown_count_query, Occur::Should)?.add(
    TermQuery::new(Term::from_text("string", "abc")),
    Occur::Should,
  )?;
  let query = builder.build();
  let weight = searcher.create_weight(query, ScoreMode::Complete, 1.0)?;
  // count of the first SHOULD clause is unknown, though the second SHOULD clause matche one doc,
  // we can't figure out the number of
  // docs
  assert_eq!(-1, weight.count(leaf, &searcher)?);

  Ok(())
}
#[test]
fn test_two_clause_term_disjunction_count_optimization() -> Result<()> {
  let mut random = random();
  let larger_term_count = random.random_range(11..=100);
  let smaller_term_count = random.random_range(1..=(larger_term_count - 1) / 10);

  let mut doc_content = Vec::with_capacity((larger_term_count + smaller_term_count) as usize);

  for _ in 0..larger_term_count {
    doc_content.push(vec!["large".to_string()]);
  }

  for _ in 0..smaller_term_count {
    doc_content.push(vec!["small".to_string(), "also small".to_string()]);
  }

  let dir = new_directory_shared(&mut random)?;
  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_merge_policy(new_log_merge_policy(&mut random)?);
  let writer = IndexWriter::new(dir.clone(), iwc)?;

  let mut field_types = HashMap::new();
  for values in doc_content {
    let mut doc = Document::new();
    for value in values {
      doc.add(new_string_field(
        &mut random,
        "foo",
        &value,
        Store::No,
        &mut field_types,
      )?);
    }
    writer.add_document(doc)?;
  }
  writer.force_merge(1)?;
  writer.close()?;

  let reader = directory_reader::open(dir.clone())?;
  let hook = CountingIndexSearcher::default();
  let counting_index_searcher =
    new_searcher_with_reader(reader)?.with_hook(IndexSearcherHook::Counting(hook.clone()));

  {
    hook.reset();

    let mut builder = Builder::new();
    builder.add(
      TermQuery::new(Term::from_text("foo", "no match")),
      Occur::Should,
    )?;
    builder.add(
      TermQuery::new(Term::from_text("foo", "also no match")),
      Occur::Should,
    )?;
    let query = builder.build();

    assert_eq!(0, counting_index_searcher.count(query)?);
    assert_eq!(3, hook.count_invocations());
  }

  {
    hook.reset();

    let mut builder = Builder::new();
    builder.add(
      TermQuery::new(Term::from_text("foo", "no match")),
      Occur::Should,
    )?;
    builder.add(
      TermQuery::new(Term::from_text("foo", "small")),
      Occur::Should,
    )?;
    let query = builder.build();

    assert_eq!(smaller_term_count, counting_index_searcher.count(query)?);
    assert_eq!(3, hook.count_invocations());
  }

  {
    hook.reset();

    let mut builder = Builder::new();
    builder.add(
      TermQuery::new(Term::from_text("foo", "small")),
      Occur::Should,
    )?;
    builder.add(
      TermQuery::new(Term::from_text("foo", "no match")),
      Occur::Should,
    )?;
    let query = builder.build();

    assert_eq!(smaller_term_count, counting_index_searcher.count(query)?);
    assert_eq!(3, hook.count_invocations());
  }

  {
    hook.reset();

    let mut builder = Builder::new();
    builder.add(
      TermQuery::new(Term::from_text("foo", "small")),
      Occur::Should,
    )?;
    builder.add(
      TermQuery::new(Term::from_text("foo", "large")),
      Occur::Should,
    )?;
    let query = builder.build();

    let count = counting_index_searcher.count(query.clone())?;

    assert_eq!(larger_term_count + smaller_term_count, count);
    assert_eq!(4, hook.count_invocations());

    assert!(query.is_two_clause_pure_disjunction_with_terms());
    let queries =
      query.rewrite_two_clause_disjunction_with_terms_for_count(&counting_index_searcher)?;
    assert_eq!(3, queries.len());
    assert_eq!(
      smaller_term_count,
      counting_index_searcher.count(queries[0].clone())?
    );
    assert_eq!(
      larger_term_count,
      counting_index_searcher.count(queries[1].clone())?
    );
  }

  {
    hook.reset();

    let mut builder = Builder::new();
    builder.add(
      TermQuery::new(Term::from_text("foo", "large")),
      Occur::Should,
    )?;
    builder.add(
      TermQuery::new(Term::from_text("foo", "small")),
      Occur::Should,
    )?;
    let query = builder.build();

    let count = counting_index_searcher.count(query.clone())?;

    assert_eq!(larger_term_count + smaller_term_count, count);
    assert_eq!(4, hook.count_invocations());

    assert!(query.is_two_clause_pure_disjunction_with_terms());
    let queries =
      query.rewrite_two_clause_disjunction_with_terms_for_count(&counting_index_searcher)?;
    assert_eq!(3, queries.len());
    assert_eq!(
      larger_term_count,
      counting_index_searcher.count(queries[0].clone())?
    );
    assert_eq!(
      smaller_term_count,
      counting_index_searcher.count(queries[1].clone())?
    );
  }

  {
    hook.reset();

    let mut builder = Builder::new();
    builder.add(
      TermQuery::new(Term::from_text("foo", "small")),
      Occur::Should,
    )?;
    builder.add(
      TermQuery::new(Term::from_text("foo", "also small")),
      Occur::Should,
    )?;
    let query = builder.build();

    let count = counting_index_searcher.count(query)?;

    assert_eq!(smaller_term_count, count);
    assert_eq!(3, hook.count_invocations());
  }
  Ok(())
}
#[test]
fn test_disjunction_two_clauses_matches_count_and_score() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let doc_content: Vec<Vec<&str>> = vec![
    vec!["A", "B"],      // 0
    vec!["A"],           // 1
    vec![],              // 2
    vec!["A", "B", "C"], // 3
    vec!["B"],           // 4
    vec!["B", "C"],      // 5
  ];

  // result sorted by score
  let match_doc_score: Vec<(i32, f32)> = vec![
    (0, (2 + 1) as f32),
    (3, (2 + 1) as f32),
    (1, 2f32),
    (4, 1f32),
    (5, 1f32),
  ];

  {
    let mut iwc = new_index_writer_config(&mut random)?;
    iwc.set_merge_policy(new_log_merge_policy(&mut random)?);
    let w = IndexWriter::new(dir.clone(), iwc)?;

    for values in doc_content {
      let mut doc = Document::new();
      for v in values {
        doc.add(StringField::from_string("foo", v, Store::No)?);
      }
      w.add_document(doc)?;
    }
    w.force_merge(1)?;
    w.close()?;
  }

  let reader = directory_reader::open(dir.clone())?;
  let searcher = new_searcher_with_reader(reader)?;

  let q_a: Query = TermQuery::new(Term::from_text("foo", "A")).into();
  let q_b: Query = TermQuery::new(Term::from_text("foo", "B")).into();

  let boosted_a = BoostQuery::new(ConstantScoreQuery::new(q_a), 2.0)?;
  let cs_b = ConstantScoreQuery::new(q_b);

  let mut builder = Builder::new();
  builder.add(boosted_a, Occur::Should)?;
  builder.add(cs_b, Occur::Should)?;
  let query: Query = builder.build().into();

  let top_docs = searcher.search(query, 10)?;
  let hits = top_docs.score_docs();

  for (i, (exp_doc, exp_score)) in match_doc_score.iter().enumerate() {
    let sd = &hits[i];
    assert_eq!(*exp_doc, sd.doc);
    assert!((sd.score - *exp_score).abs() < 0.0001);
  }

  Ok(())
}
#[test]
fn test_disjunction_random_clauses_matches_count() -> Result<()> {
  let mut random = random();

  let num_field_value: i32 = random.random_range(1..=10);
  let mut num_docs_per_field_value = Vec::with_capacity(num_field_value as usize);
  let mut all_docs_count: i32 = 0;

  for _ in 0..num_field_value {
    let num_docs: i32 = random.random_range(10..=50);
    num_docs_per_field_value.push(num_docs);
    all_docs_count += num_docs;
  }

  let dir = new_directory_shared(&mut random)?;
  {
    let mut iwc = new_index_writer_config(&mut random)?;
    iwc.set_merge_policy(new_log_merge_policy(&mut random)?);
    let w = IndexWriter::new(dir.clone(), iwc)?;

    for i in 0..num_field_value {
      for _ in 0..num_docs_per_field_value[i as usize] {
        let mut doc = Document::new();
        doc.add(StringField::from_string("field", i.to_string(), No)?);
        w.add_document(doc)?;
      }
    }

    w.force_merge(1)?;
    w.close()?;
  }

  let mut matched_docs_count: i32 = 0;
  let reader = directory_reader::open(dir.clone())?;
  let searcher = new_searcher_with_reader(reader)?;

  let mut builder = Builder::new();

  for i in 0..num_field_value {
    if random.random_bool(0.5) {
      matched_docs_count += num_docs_per_field_value[i as usize];
      let q = TermQuery::new(Term::from_text("field", i.to_string()));
      builder.add(q, Occur::Should)?;
    }
  }

  let query = builder.build();
  let top_docs = searcher.search(query, all_docs_count as usize)?;
  assert_eq!(matched_docs_count as usize, top_docs.score_docs().len());

  Ok(())
}
#[test]
fn test_prohibited_matches_count() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;

  let mut doc = Document::new();
  doc.add(LongPoint::new("long", vec![3])?);
  doc.add(StringField::from_string("string", "abc", No)?);
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(LongPoint::new("long", vec![10])?);
  doc.add(StringField::from_string("string", "xyz", No)?);
  writer.add_document(doc)?;

  let reader = directory_reader::open_from_writer(&writer)?;
  writer.close()?;
  let searcher = new_searcher_with_reader(reader)?;
  let leaves = searcher.get_top_reader_context().leaves()?;

  // MUST abc, MUST_NOT long==3 => BooleanWeight can't figure out count => -1
  {
    let mut b = Builder::new();
    b.add(
      TermQuery::new(Term::from_text("string", "abc")),
      Occur::Must,
    )?;
    b.add(LongPoint::new_exact_query("long", 3)?, Occur::MustNot)?;
    let query = b.build();
    let weight = searcher.create_weight(query, ScoreMode::Complete, 1.0)?;
    assert_eq!(-1, weight.count(&leaves[0], &searcher)?);
  }

  // MUST missing, MUST_NOT long==3 => 0
  {
    let mut b = Builder::new();
    b.add(
      TermQuery::new(Term::from_text("string", "missing")),
      Occur::Must,
    )?;
    b.add(LongPoint::new_exact_query("long", 3)?, Occur::MustNot)?;
    let query = b.build();
    let weight = searcher.create_weight(query, ScoreMode::Complete, 1.0)?;
    assert_eq!(0, weight.count(&leaves[0], &searcher)?);
  }

  // MUST abc, MUST_NOT long==5 => 1
  {
    let mut b = Builder::new();
    b.add(
      TermQuery::new(Term::from_text("string", "abc")),
      Occur::Must,
    )?;
    b.add(LongPoint::new_exact_query("long", 5)?, Occur::MustNot)?;
    let query = b.build();
    let weight = searcher.create_weight(query, ScoreMode::Complete, 1.0)?;
    assert_eq!(1, weight.count(&leaves[0], &searcher)?);
  }

  // MUST abc, MUST_NOT long in [0,10] => 0
  {
    let mut b = Builder::new();
    b.add(
      TermQuery::new(Term::from_text("string", "abc")),
      Occur::Must,
    )?;
    b.add(LongPoint::new_range_query("long", 0, 10)?, Occur::MustNot)?;
    let query = b.build();
    let weight = searcher.create_weight(query, ScoreMode::Complete, 1.0)?;
    assert_eq!(0, weight.count(&leaves[0], &searcher)?);
  }

  // MUST long in [0,10], MUST_NOT abc => 1
  {
    let mut b = Builder::new();
    b.add(LongPoint::new_range_query("long", 0, 10)?, Occur::Must)?;
    b.add(
      TermQuery::new(Term::from_text("string", "abc")),
      Occur::MustNot,
    )?;
    let query = b.build();
    let weight = searcher.create_weight(query, ScoreMode::Complete, 1.0)?;
    assert_eq!(1, weight.count(&leaves[0], &searcher)?);
  }

  Ok(())
}
#[test]
fn test_random_boolean_query_matches_count() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let writer = IndexWriter::new(dir.clone(), IndexWriterConfig::new()?)?;

  let mut doc = Document::new();
  doc.add(LongPoint::new("long", [3])?);
  doc.add(StringField::from_string("string", "abc", No)?);
  writer.add_document(doc)?;

  let mut doc = Document::new();
  doc.add(LongPoint::new("long", [10])?);
  doc.add(StringField::from_string("string", "xyz", No)?);
  writer.add_document(doc)?;

  let reader = directory_reader::open_from_writer(&writer)?;
  writer.close()?;
  let searcher = new_searcher_with_reader(reader)?;

  for _ in 0..1000 {
    let num_clauses = TestUtil::next_int(&mut random, 2, 5);
    let mut builder = Builder::new();
    let mut num_should_clauses: i32 = 0;
    for _ in 0..num_clauses {
      let query: Query = match random.random_range(0..6) {
        0 => TermQuery::new(Term::from_text("string", "abc")).into(),
        1 => LongPoint::new_exact_query("long", 3)?.into(),
        2 => TermQuery::new(Term::from_text("string", "missing")).into(),
        3 => LongPoint::new_exact_query("long", 5)?.into(),
        4 => MatchAllDocsQuery::new().into(),
        _ => LongPoint::new_range_query("long", 0, 10)?.into(),
      };

      let occur = match random.random_range(0..4) {
        0 => Occur::Must,
        1 => Occur::Filter,
        2 => Occur::Should,
        _ => Occur::MustNot,
      };

      if occur == Occur::Should {
        num_should_clauses += 1;
      }
      builder.add(query, occur)?;
    }

    builder.set_minimum_number_should_match(TestUtil::next_int(&mut random, 0, num_should_clauses));

    let boolean_query: Query = builder.build().into();

    let total_hits = searcher
      .search(boolean_query.clone(), 1)?
      .total_hits()
      .value;
    assert_eq!(total_hits, searcher.count(boolean_query)? as usize);
  }

  Ok(())
}
#[test]
fn test_to_string() -> Result<()> {
  let mut bq = Builder::new();
  bq.add(TermQuery::new(Term::from_text("field", "a")), Occur::Should)?;
  bq.add(TermQuery::new(Term::from_text("field", "b")), Occur::Must)?;
  bq.add(
    TermQuery::new(Term::from_text("field", "c")),
    Occur::MustNot,
  )?;
  bq.add(TermQuery::new(Term::from_text("field", "d")), Occur::Filter)?;

  let q = bq.build();
  assert_eq!("a +b -c #d", q.to_string("field")?);
  Ok(())
}
#[test]
fn test_query_visitor() -> Result<()> {
  struct Visitor {
    expected: Option<Term>,
    a: Term,
    b: Term,
    c: Term,
    d: Term,
  }

  impl QueryVisitor for Visitor {
    type SubVisitor<'a>
      = &'a mut Self
    where
      Self: 'a;

    fn get_sub_visitor<'a>(
      &'a mut self,
      occur: Occur,
      _parent: QueryRef<'_>,
    ) -> Self::SubVisitor<'a> {
      self.expected = Some(
        match occur {
          Occur::Should => &self.a,
          Occur::Must => &self.b,
          Occur::Filter => &self.c,
          Occur::MustNot => &self.d,
        }
        .clone(),
      );
      self
    }

    fn consume_terms(&mut self, _query: QueryRef<'_>, terms: &[Term]) -> Result<()> {
      assert_eq!(self.expected.as_ref(), terms.first());
      Ok(())
    }
  }

  let a = Term::from_text("f", "a");
  let b = Term::from_text("f", "b");
  let c = Term::from_text("f", "c");
  let d = Term::from_text("f", "d");
  let mut builder = Builder::new();
  builder.add(TermQuery::new(a.clone()), Occur::Should)?;
  builder.add(TermQuery::new(b.clone()), Occur::Must)?;
  builder.add(TermQuery::new(c.clone()), Occur::Filter)?;
  builder.add(TermQuery::new(d.clone()), Occur::MustNot)?;
  let query = builder.build();

  query.visit(&mut Visitor {
    expected: None,
    a,
    b,
    c,
    d,
  })?;
  Ok(())
}
#[test]
#[ignore = "Java-only: BooleanQuery clause accessors return immutable Rust slices by type"]
fn test_clause_sets_immutability() -> Result<()> {
  test_not_required_in_rust_lucene!();
}

struct CollectorManagerImpl<'a, IRC>
where
  IRC: 'static,
{
  searcher: &'a IndexSearcher<IRC>,
  matched: Arc<AtomicBool>,
  bq2: BooleanQuery,
}
impl<'a, IRC> CollectorManagerImpl<'a, IRC>
where
  IRC: IndexReaderContext,
{
  fn new(searcher: &'a IndexSearcher<IRC>, matched: Arc<AtomicBool>, bq2: BooleanQuery) -> Self {
    Self {
      searcher,
      matched,
      bq2,
    }
  }
}
impl<'a, IRC> CollectorManager for CollectorManagerImpl<'a, IRC>
where
  IRC: IndexReaderContext,
{
  type C = SimpleCollectorImpl<'a, IRC>;
  type T = ();

  fn new_collector(&self) -> Result<Self::C> {
    Ok(SimpleCollectorImpl::new(
      self.matched.clone(),
      self.bq2.clone(),
      self.searcher,
    ))
  }

  fn reduce(&self, _collectors: Vec<Self::C>) -> Result<Self::T> {
    Ok(())
  }
}

struct SimpleCollectorImpl<'a, IRC>
where
  IRC: 'static,
{
  doc_base: i32,
  matched: Arc<AtomicBool>,
  bq2: BooleanQuery,
  searcher: &'a IndexSearcher<IRC>,
}
impl<'a, IRC> SimpleCollectorImpl<'a, IRC>
where
  IRC: IndexReaderContext,
{
  fn new(matched: Arc<AtomicBool>, bq2: BooleanQuery, searcher: &'a IndexSearcher<IRC>) -> Self {
    Self {
      doc_base: 0,
      matched,
      bq2,
      searcher,
    }
  }
}

impl<IRC> Collector for SimpleCollectorImpl<'_, IRC>
where
  IRC: IndexReaderContext,
{
  type LeafCollector<'a, IRC1>
    = &'a mut Self
  where
    Self: 'a,
    IRC1: IndexReaderContext + 'a;

  fn get_leaf_collector<'a, W, IRC1>(
    &'a mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC1>>,
    _weight: Option<&W>,
    _searcher: &IndexSearcher<IRC1>,
  ) -> Result<Self::LeafCollector<'a, IRC>>
  where
    IRC1: IndexReaderContext,
    W: Weight<IRC1> + ?Sized,
  {
    SimpleCollector::do_set_next_reader(self, context)?;
    Ok(self)
  }

  fn score_mode(&self) -> ScoreMode {
    ScoreMode::Complete
  }
}

impl<IRC> LeafCollector for SimpleCollectorImpl<'_, IRC>
where
  IRC: IndexReaderContext,
{
  fn collect(&mut self, doc: i32, scorer: &mut dyn Scorable) -> Result<()> {
    let actual_score = scorer.score()?;
    let q = self.bq2.clone();
    let expected_score = self
      .searcher
      .explain(q, self.doc_base + doc)?
      .value
      .to_f32()
      .unwrap();
    assert_eq!(expected_score, actual_score);
    self.matched.store(true, Ordering::SeqCst);
    Ok(())
  }
}

impl<IRC> Display for SimpleCollectorImpl<'_, IRC>
where
  IRC: IndexReaderContext,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl<IRC> SimpleCollector for SimpleCollectorImpl<'_, IRC>
where
  IRC: IndexReaderContext,
{
  fn do_set_next_reader<LR>(&mut self, context: &LeafReaderContext<LR>) -> Result<()>
  where
    LR: LeafReader,
  {
    self.doc_base = context.doc_base as i32;
    Ok(())
  }
}
