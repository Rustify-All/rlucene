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
use crate::core::document::string_field::StringField;
use crate::core::index::directory_reader;
use crate::core::index::index_reader::Identity;
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::term::Term;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, new_directory_shared, new_index_writer_config, new_log_merge_policy,
  new_searcher_with_reader, new_searcher_with_threads, random,
};

use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::boolean_query::{BooleanQuery, Builder};
use crate::core::search::boolean_weight::BooleanWeight;
use crate::core::search::boost_query::BoostQuery;
use crate::core::search::constant_score_query::ConstantScoreQuery;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::explanation::Explanation;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::matches_utils::MatchWithNoTerms;
use crate::core::search::query::{
  Query, QueryBase, QueryWeight, QueryWeightSs, QueryWeightSsBulkScorer, QueryWeightSsScorer,
};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::scorable::Scorable;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::{Scorer, ScorerEnum2, TwoPhaseState};
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::term_query::TermQuery;
use crate::core::search::two_phase_iterator::TwoPhaseIterator;
use crate::core::search::wand_scorer::{
  FLOAT_MANTISSA_BITS, WANDScorer, scale_max_score, scaling_factor,
};
use crate::core::search::weight::{DefaultScorerSupplier, Weight};
use crate::core::util::error::lucene_error::Result;
use crate::core::util::{HasIdentity, ToInt};
use crate::test_framework::core::search::asserting_query::AssertingQuery;
use crate::test_framework::core::search::block_score_query_wrapper::BlockScoreQueryWrapper;
use crate::test_framework::core::search::check_hits::CheckHits;
pub use crate::test_framework::core::search::query::{MaxScoreWrapperQuery, WANDScorerQuery};
use crate::test_framework::core::util::test_util::TestUtil;
use rand::{Rng, RngExt};
use std::fmt::Debug;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;

#[allow(dead_code)] // for quick search
struct TestWANDScorer;
#[test]
fn test_scaling_factor() -> Result<()> {
  use std::f32;

  do_test_scaling_factor(1.0)?;
  do_test_scaling_factor(2.0)?;
  do_test_scaling_factor(1.0f32.next_down())?;
  do_test_scaling_factor(1.0f32.next_up())?;
  do_test_scaling_factor(f32::MIN_POSITIVE)?;
  do_test_scaling_factor(f32::MIN_POSITIVE.next_up())?;
  do_test_scaling_factor(f32::MAX)?;
  do_test_scaling_factor(f32::MAX.next_down())?;

  assert_eq!(scaling_factor(f32::MIN_POSITIVE)? + 1, scaling_factor(0.0)?);

  assert_eq!(
    scaling_factor(f32::MAX)? - 1,
    scaling_factor(f32::INFINITY)?
  );

  assert!(scaling_factor(1.0)? > scaling_factor(10.0)?);
  assert!(scaling_factor(f32::MAX)? > scaling_factor(f32::INFINITY)?);
  assert!(scaling_factor(0.0)? > scaling_factor(f32::MIN_POSITIVE)?);
  Ok(())
}
fn do_test_scaling_factor(v: f32) -> Result<()> {
  let sf = scaling_factor(v)?;
  let scaled = (v as f64) * (2f64.powi(sf));
  assert!(
    scaled >= (1u64 << 23) as f64 && scaled < (1u64 << 24) as f64,
    "v={v}, sf={sf}, scaled={scaled}"
  );
  Ok(())
}
#[test]
fn test_scale_max_score() -> Result<()> {
  let expected = 1i64 << (FLOAT_MANTISSA_BITS - 1);
  let sf = scaling_factor(32.0)?;
  let scaled = scale_max_score(32.0, sf);
  assert_eq!(expected, scaled);

  let v = (1.0f32 as f64 * 2f64.powi(60)) as f32;
  let sf2 = scaling_factor(v)?;
  let scaled2 = scale_max_score(32.0, sf2);
  assert_eq!(1, scaled2);

  let sf3 = scaling_factor(f32::INFINITY)?;
  let scaled3 = scale_max_score(32.0, sf3);
  assert_eq!(1, scaled3);

  Ok(())
}
fn maybe_wrap<R>(random: &mut R, mut query: Query) -> Result<Query>
where
  R: Rng + ?Sized,
{
  if random.random_bool(0.5) {
    query = BlockScoreQueryWrapper::new(query, TestUtil::next_usize(random, 2, 8)).into();
    query = AssertingQuery::new(random, query).into()
  }
  Ok(query)
}
#[test]
fn test_basics() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut conf = new_index_writer_config(&mut random)?;
  conf.set_merge_policy(new_log_merge_policy(&mut random)?);
  let w = IndexWriter::new(dir.clone(), conf)?;

  let docs: &[&[&str]] = &[
    &["A", "B"],      // 0
    &["A"],           // 1
    &[],              // 2
    &["A", "B", "C"], // 3
    &["B"],           // 4
    &["B", "C"],      // 5
  ];

  for values in docs {
    let mut doc = Document::new();
    for value in *values {
      doc.add(StringField::from_string("foo", *value, Store::No)?);
    }
    w.add_document(doc)?;
  }

  w.force_merge(1)?;
  w.close()?;

  let reader = directory_reader::open(dir)?;
  let searcher = new_searcher_with_reader(reader)?;
  let mut builder = Builder::new();
  builder
    .add(
      BoostQuery::new(
        ConstantScoreQuery::new(TermQuery::new(Term::from_text("foo", "A"))),
        2.0,
      )?,
      Occur::Should,
    )?
    .add(
      ConstantScoreQuery::new(TermQuery::new(Term::from_text("foo", "B"))),
      Occur::Should,
    )?
    .add(
      BoostQuery::new(
        ConstantScoreQuery::new(TermQuery::new(Term::from_text("foo", "C"))),
        3.0,
      )?,
      Occur::Should,
    )?;
  let mut query: Query = WANDScorerQuery::new(builder.build(), random.random_bool(0.5)).into();

  let weight = searcher.create_weight(searcher.rewrite(query)?, ScoreMode::TopScores, 1.0)?;
  let context = &searcher.get_leaf_contexts()?[0];

  let mut ss = weight
    .scorer_supplier(context, &searcher)?
    .expect("expected scorer supplier");
  ss.set_top_level_scoring_clause()?;
  let mut scorer = ss.get(i64::MAX, context, &searcher)?;

  assert_eq!(0, scorer.iterator_mut().next_doc()?);
  assert_eq!(3.0, scorer.score()?);

  assert_eq!(1, scorer.iterator_mut().next_doc()?);
  assert_eq!(2.0, scorer.score()?);

  assert_eq!(3, scorer.iterator_mut().next_doc()?);
  assert_eq!(6.0, scorer.score()?);

  assert_eq!(4, scorer.iterator_mut().next_doc()?);
  assert_eq!(1.0, scorer.score()?);

  assert_eq!(5, scorer.iterator_mut().next_doc()?);
  assert_eq!(4.0, scorer.score()?);

  assert_eq!(NO_MORE_DOCS, scorer.iterator_mut().next_doc()?);

  let mut ss = weight
    .scorer_supplier(context, &searcher)?
    .expect("expected scorer supplier");
  ss.set_top_level_scoring_clause()?;
  let mut scorer = ss.get(i64::MAX, context, &searcher)?;
  scorer.set_min_competitive_score(4.0)?;

  assert_eq!(3, scorer.iterator_mut().next_doc()?);
  assert_eq!(6.0, scorer.score()?);

  assert_eq!(5, scorer.iterator_mut().next_doc()?);
  assert_eq!(4.0, scorer.score()?);

  assert_eq!(NO_MORE_DOCS, scorer.iterator_mut().next_doc()?);

  let mut ss = weight
    .scorer_supplier(context, &searcher)?
    .expect("expected scorer supplier");
  ss.set_top_level_scoring_clause()?;
  let mut scorer = ss.get(i64::MAX, context, &searcher)?;

  assert_eq!(0, scorer.iterator_mut().next_doc()?);
  assert_eq!(3.0, scorer.score()?);

  scorer.set_min_competitive_score(10.0)?;

  assert_eq!(NO_MORE_DOCS, scorer.iterator_mut().next_doc()?);
  //  test a filtered disjunction
  builder = Builder::new();
  builder
    .add(
      WANDScorerQuery::new(
        {
          let mut v = Builder::new();
          v.add(
            BoostQuery::new(
              ConstantScoreQuery::new(TermQuery::new(Term::from_text("foo", "A"))),
              2.0,
            )?,
            Occur::Should,
          )?
          .add(
            ConstantScoreQuery::new(TermQuery::new(Term::from_text("foo", "B"))),
            Occur::Should,
          )?;
          v.build()
        },
        random.random_bool(0.5),
      ),
      Occur::Must,
    )?
    .add(TermQuery::new(Term::from_text("foo", "C")), Occur::Filter)?;
  query = builder.build().into();

  let weight = searcher.create_weight(searcher.rewrite(query)?, ScoreMode::TopScores, 1.0)?;
  let mut ss = weight
    .scorer_supplier(context, &searcher)?
    .expect("expected scorer supplier");
  ss.set_top_level_scoring_clause()?;
  let mut scorer = ss.get(i64::MAX, context, &searcher)?;

  assert_eq!(3, scorer.iterator_mut().next_doc()?);
  assert_eq!(3.0, scorer.score()?);

  assert_eq!(5, scorer.iterator_mut().next_doc()?);
  assert_eq!(1.0, scorer.score()?);

  assert_eq!(NO_MORE_DOCS, scorer.iterator_mut().next_doc()?);

  let mut ss = weight
    .scorer_supplier(context, &searcher)?
    .expect("expected scorer supplier");
  ss.set_top_level_scoring_clause()?;
  let mut scorer = ss.get(i64::MAX, context, &searcher)?;
  scorer.set_min_competitive_score(2.0)?;

  assert_eq!(3, scorer.iterator_mut().next_doc()?);
  assert_eq!(3.0, scorer.score()?);

  assert_eq!(NO_MORE_DOCS, scorer.iterator_mut().next_doc()?);

  builder = Builder::new();
  builder
    .add(
      WANDScorerQuery::new(
        {
          let mut v = Builder::new();
          v.add(
            BoostQuery::new(
              ConstantScoreQuery::new(TermQuery::new(Term::from_text("foo", "A"))),
              2.0,
            )?,
            Occur::Should,
          )?
          .add(
            ConstantScoreQuery::new(TermQuery::new(Term::from_text("foo", "B"))),
            Occur::Should,
          )?;
          v.build()
        },
        random.random_bool(0.5),
      ),
      Occur::Must,
    )?
    .add(TermQuery::new(Term::from_text("foo", "C")), Occur::MustNot)?;
  query = builder.build().into();

  let weight = searcher.create_weight(searcher.rewrite(query)?, ScoreMode::TopScores, 1.0)?;
  let mut ss = weight
    .scorer_supplier(context, &searcher)?
    .expect("expected scorer supplier");
  ss.set_top_level_scoring_clause()?;
  let mut scorer = ss.get(i64::MAX, context, &searcher)?;

  assert_eq!(0, scorer.iterator_mut().next_doc()?);
  assert_eq!(3.0, scorer.score()?);

  assert_eq!(1, scorer.iterator_mut().next_doc()?);
  assert_eq!(2.0, scorer.score()?);

  assert_eq!(4, scorer.iterator_mut().next_doc()?);
  assert_eq!(1.0, scorer.score()?);

  assert_eq!(NO_MORE_DOCS, scorer.iterator_mut().next_doc()?);

  let mut ss = weight
    .scorer_supplier(context, &searcher)?
    .expect("expected scorer supplier");
  ss.set_top_level_scoring_clause()?;
  let mut scorer = ss.get(i64::MAX, context, &searcher)?;
  scorer.set_min_competitive_score(3.0)?;

  assert_eq!(0, scorer.iterator_mut().next_doc()?);
  assert_eq!(3.0, scorer.score()?);

  assert_eq!(NO_MORE_DOCS, scorer.iterator_mut().next_doc()?);

  Ok(())
}

#[test]
fn test_basics_with_disjunction_and_min_should_match() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut conf = new_index_writer_config(&mut random)?;
  conf.set_merge_policy(new_log_merge_policy(&mut random)?);
  let w = IndexWriter::new(dir.clone(), conf)?;

  let docs: &[&[&str]] = &[
    &["A", "B"],      // 0
    &["A"],           // 1
    &[],              // 2
    &["A", "B", "C"], // 3
    &["B"],           // 4
    &["B", "C"],      // 5
  ];

  for values in docs {
    let mut doc = Document::new();
    for value in *values {
      doc.add(StringField::from_string("foo", *value, Store::No)?);
    }
    w.add_document(doc)?;
  }

  w.force_merge(1)?;
  w.close()?;

  let reader = directory_reader::open(dir)?;
  let searcher = new_searcher_with_reader(reader)?;
  let context = &searcher.get_leaf_contexts()?[0];

  let mut builder = Builder::new();
  builder
    .add(
      BoostQuery::new(
        ConstantScoreQuery::new(TermQuery::new(Term::from_text("foo", "A"))),
        2.0,
      )?,
      Occur::Should,
    )?
    .add(
      ConstantScoreQuery::new(TermQuery::new(Term::from_text("foo", "B"))),
      Occur::Should,
    )?
    .add(
      BoostQuery::new(
        ConstantScoreQuery::new(TermQuery::new(Term::from_text("foo", "C"))),
        3.0,
      )?,
      Occur::Should,
    )?;
  builder.set_minimum_number_should_match(2);

  let query: Query = WANDScorerQuery::new(builder.build(), random.random_bool(0.5)).into();

  let weight = searcher.create_weight(searcher.rewrite(query)?, ScoreMode::TopScores, 1.0)?;
  let mut ss = weight
    .scorer_supplier(context, &searcher)?
    .expect("expected scorer supplier");
  ss.set_top_level_scoring_clause()?;
  let mut scorer = ss.get(i64::MAX, context, &searcher)?;

  assert_eq!(0, scorer.iterator_mut().next_doc()?);
  assert_eq!(3.0, scorer.score()?);

  assert_eq!(3, scorer.iterator_mut().next_doc()?);
  assert_eq!(6.0, scorer.score()?);

  assert_eq!(5, scorer.iterator_mut().next_doc()?);
  assert_eq!(4.0, scorer.score()?);

  assert_eq!(NO_MORE_DOCS, scorer.iterator_mut().next_doc()?);

  let mut ss = weight
    .scorer_supplier(context, &searcher)?
    .expect("expected scorer supplier");
  ss.set_top_level_scoring_clause()?;
  let mut scorer = ss.get(i64::MAX, context, &searcher)?;
  scorer.set_min_competitive_score(4.0)?;

  assert_eq!(3, scorer.iterator_mut().next_doc()?);
  assert_eq!(6.0, scorer.score()?);

  assert_eq!(5, scorer.iterator_mut().next_doc()?);
  assert_eq!(4.0, scorer.score()?);

  assert_eq!(NO_MORE_DOCS, scorer.iterator_mut().next_doc()?);

  let mut ss = weight
    .scorer_supplier(context, &searcher)?
    .expect("expected scorer supplier");
  ss.set_top_level_scoring_clause()?;
  let mut scorer = ss.get(i64::MAX, context, &searcher)?;

  assert_eq!(0, scorer.iterator_mut().next_doc()?);
  assert_eq!(3.0, scorer.score()?);

  scorer.set_min_competitive_score(10.0)?;

  assert_eq!(NO_MORE_DOCS, scorer.iterator_mut().next_doc()?);

  Ok(())
}

#[test]
fn test_basics_with_disjunction_and_min_should_match_and_tail_size_condition() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut conf = new_index_writer_config(&mut random)?;
  conf.set_merge_policy(new_log_merge_policy(&mut random)?);
  let w = IndexWriter::new(dir.clone(), conf)?;

  let docs: &[&[&str]] = &[
    &["A", "B"],      // 0
    &["A"],           // 1
    &[],              // 2
    &["A", "B", "C"], // 3
    // 2 "B"s here and the non constant score term query below forces the
    // tailMaxScore >= minCompetitiveScore && tailSize < minShouldMatch condition
    &["B", "B"], // 4
    &["B", "C"], // 5
  ];

  for values in docs {
    let mut doc = Document::new();
    for value in *values {
      doc.add(StringField::from_string("foo", *value, Store::No)?);
    }
    w.add_document(doc)?;
  }

  w.force_merge(1)?;
  w.close()?;

  let reader = directory_reader::open(dir)?;
  let searcher = new_searcher_with_reader(reader)?;
  let context = &searcher.get_leaf_contexts()?[0];

  let mut builder = Builder::new();
  builder
    .add(TermQuery::new(Term::from_text("foo", "A")), Occur::Should)?
    .add(TermQuery::new(Term::from_text("foo", "B")), Occur::Should)?
    .add(TermQuery::new(Term::from_text("foo", "C")), Occur::Should)?;
  builder.set_minimum_number_should_match(2);

  let query: Query = WANDScorerQuery::new(builder.build(), random.random_bool(0.5)).into();

  let weight = searcher.create_weight(searcher.rewrite(query)?, ScoreMode::TopScores, 1.0)?;
  let mut ss = weight
    .scorer_supplier(context, &searcher)?
    .expect("expected scorer supplier");
  ss.set_top_level_scoring_clause()?;
  let mut scorer = ss.get(i64::MAX, context, &searcher)?;

  assert_eq!(0, scorer.iterator_mut().next_doc()?);
  let score = scorer.score()?;
  scorer.set_min_competitive_score(score)?;

  assert_eq!(3, scorer.iterator_mut().next_doc()?);

  Ok(())
}

#[test]
fn test_basics_with_disjunction_and_min_should_match_and_non_scoring_mode() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut conf = new_index_writer_config(&mut random)?;
  conf.set_merge_policy(new_log_merge_policy(&mut random)?);
  let w = IndexWriter::new(dir.clone(), conf)?;

  let docs: &[&[&str]] = &[
    &["A", "B"],      // 0
    &["A"],           // 1
    &[],              // 2
    &["A", "B", "C"], // 3
    &["B"],           // 4
    &["B", "C"],      // 5
  ];

  for values in docs {
    let mut doc = Document::new();
    for value in *values {
      doc.add(StringField::from_string("foo", *value, Store::No)?);
    }
    w.add_document(doc)?;
  }

  w.force_merge(1)?;
  w.close()?;

  let reader = directory_reader::open(dir)?;
  let searcher = new_searcher_with_reader(reader)?;
  let context = &searcher.get_leaf_contexts()?[0];

  let mut builder = Builder::new();
  builder
    .add(
      BoostQuery::new(
        ConstantScoreQuery::new(TermQuery::new(Term::from_text("foo", "A"))),
        2.0,
      )?,
      Occur::Should,
    )?
    .add(
      ConstantScoreQuery::new(TermQuery::new(Term::from_text("foo", "B"))),
      Occur::Should,
    )?
    .add(
      BoostQuery::new(
        ConstantScoreQuery::new(TermQuery::new(Term::from_text("foo", "C"))),
        3.0,
      )?,
      Occur::Should,
    )?;
  builder.set_minimum_number_should_match(2);

  let query: Query = WANDScorerQuery::new(builder.build(), random.random_bool(0.5)).into();

  let weight =
    searcher.create_weight(searcher.rewrite(query)?, ScoreMode::CompleteNoScores, 1.0)?;
  let mut scorer = weight
    .scorer(context, &searcher)?
    .expect("expected scorer to be present");

  assert_eq!(0, scorer.iterator_mut().next_doc()?);
  assert_eq!(3, scorer.iterator_mut().next_doc()?);
  assert_eq!(5, scorer.iterator_mut().next_doc()?);
  assert_eq!(NO_MORE_DOCS, scorer.iterator_mut().next_doc()?);

  Ok(())
}
#[test]
fn test_basics_with_filtered_disjunction_and_min_should_match() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut conf = new_index_writer_config(&mut random)?;
  conf.set_merge_policy(new_log_merge_policy(&mut random)?);
  let w = IndexWriter::new(dir.clone(), conf)?;

  let docs: &[&[&str]] = &[
    &["A", "B"],           // 0
    &["A", "C", "D"],      // 1
    &[],                   // 2
    &["A", "B", "C", "D"], // 3
    &["B"],                // 4
    &["C", "D"],           // 5
  ];

  for values in docs {
    let mut doc = Document::new();
    for value in *values {
      doc.add(StringField::from_string("foo", *value, Store::No)?);
    }
    w.add_document(doc)?;
  }

  w.force_merge(1)?;
  w.close()?;

  let reader = directory_reader::open(dir)?;
  let searcher = new_searcher_with_reader(reader)?;
  let context = &searcher.get_leaf_contexts()?[0];

  let query: Query = {
    let mut inner = Builder::new();
    inner
      .add(
        BoostQuery::new(
          ConstantScoreQuery::new(TermQuery::new(Term::from_text("foo", "A"))),
          2.0,
        )?,
        Occur::Should,
      )?
      .add(
        ConstantScoreQuery::new(TermQuery::new(Term::from_text("foo", "B"))),
        Occur::Should,
      )?
      .add(
        BoostQuery::new(
          ConstantScoreQuery::new(TermQuery::new(Term::from_text("foo", "D"))),
          4.0,
        )?,
        Occur::Should,
      )?;
    inner.set_minimum_number_should_match(2);
    let v = inner.build();
    let inner_query: Query = WANDScorerQuery::new(v, random.random_bool(0.5)).into();

    let mut outer = Builder::new();
    outer
      .add(inner_query, Occur::Must)?
      .add(TermQuery::new(Term::from_text("foo", "C")), Occur::Filter)?;
    outer.build().into()
  };

  let weight = searcher.create_weight(searcher.rewrite(query)?, ScoreMode::TopScores, 1.0)?;
  let mut ss = weight
    .scorer_supplier(context, &searcher)?
    .expect("expected scorer supplier");
  ss.set_top_level_scoring_clause()?;
  let mut scorer = ss.get(i64::MAX, context, &searcher)?;

  assert_eq!(1, scorer.iterator_mut().next_doc()?);
  assert_eq!(6.0, scorer.score()?); // 2 + 4

  assert_eq!(3, scorer.iterator_mut().next_doc()?);
  assert_eq!(7.0, scorer.score()?); // 2 + 1 + 4

  assert_eq!(NO_MORE_DOCS, scorer.iterator_mut().next_doc()?);

  let mut ss = weight
    .scorer_supplier(context, &searcher)?
    .expect("expected scorer supplier");
  ss.set_top_level_scoring_clause()?;
  let mut scorer = ss.get(i64::MAX, context, &searcher)?;
  scorer.set_min_competitive_score(7.0)?; // 2 + 1 + 4

  assert_eq!(3, scorer.iterator_mut().next_doc()?);
  assert_eq!(7.0, scorer.score()?);

  assert_eq!(NO_MORE_DOCS, scorer.iterator_mut().next_doc()?);

  Ok(())
}

#[test]
fn test_basics_with_filtered_disjunction_and_min_should_match_and_non_scoring_mode() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut conf = new_index_writer_config(&mut random)?;
  conf.set_merge_policy(new_log_merge_policy(&mut random)?);
  let w = IndexWriter::new(dir.clone(), conf)?;

  let docs: &[&[&str]] = &[
    &["A", "B"],           // 0
    &["A", "C", "D"],      // 1
    &[],                   // 2
    &["A", "B", "C", "D"], // 3
    &["B"],                // 4
    &["C", "D"],           // 5
  ];

  for values in docs {
    let mut doc = Document::new();
    for value in *values {
      doc.add(StringField::from_string("foo", *value, Store::No)?);
    }
    w.add_document(doc)?;
  }

  w.force_merge(1)?;
  w.close()?;

  let reader = directory_reader::open(dir)?;
  let searcher = new_searcher_with_reader(reader)?;
  let context = &searcher.get_leaf_contexts()?[0];

  let query: Query = {
    let mut inner = Builder::new();
    inner
      .add(
        BoostQuery::new(
          ConstantScoreQuery::new(TermQuery::new(Term::from_text("foo", "A"))),
          2.0,
        )?,
        Occur::Should,
      )?
      .add(
        ConstantScoreQuery::new(TermQuery::new(Term::from_text("foo", "B"))),
        Occur::Should,
      )?
      .add(
        BoostQuery::new(
          ConstantScoreQuery::new(TermQuery::new(Term::from_text("foo", "D"))),
          4.0,
        )?,
        Occur::Should,
      )?;
    inner.set_minimum_number_should_match(2);

    let inner_query: Query = WANDScorerQuery::new(inner.build(), random.random_bool(0.5)).into();

    let mut outer = Builder::new();
    outer
      .add(inner_query, Occur::Must)?
      .add(TermQuery::new(Term::from_text("foo", "C")), Occur::Filter)?;
    outer.build().into()
  };

  let weight = searcher.create_weight(searcher.rewrite(query)?, ScoreMode::TopDocs, 1.0)?;
  let mut scorer = weight
    .scorer(context, &searcher)?
    .expect("expected scorer to be present");

  assert_eq!(1, scorer.iterator_mut().next_doc()?);
  assert_eq!(3, scorer.iterator_mut().next_doc()?);
  assert_eq!(NO_MORE_DOCS, scorer.iterator_mut().next_doc()?);

  Ok(())
}

#[test]
fn test_basics_with_filtered_disjunction_and_must_not_and_min_should_match() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let conf = new_index_writer_config(&mut random)?;
  let w = IndexWriter::new(dir.clone(), conf)?;

  let docs: &[&[&str]] = &[
    &["A", "B"],           // 0
    &["A", "C", "D"],      // 1
    &[],                   // 2
    &["A", "B", "C", "D"], // 3
    &["B", "D"],           // 4
    &["C", "D"],           // 5
  ];

  for values in docs {
    let mut doc = Document::new();
    for value in *values {
      doc.add(StringField::from_string("foo", *value, Store::No)?);
    }
    w.add_document(doc)?;
  }

  w.force_merge(1)?;
  w.close()?;

  let reader = directory_reader::open(dir)?;
  let searcher = new_searcher_with_reader(reader)?;
  let context = &searcher.get_leaf_contexts()?[0];

  let query: Query = {
    let mut inner = Builder::new();
    inner
      .add(
        BoostQuery::new(
          ConstantScoreQuery::new(TermQuery::new(Term::from_text("foo", "A"))),
          2.0,
        )?,
        Occur::Should,
      )?
      .add(
        ConstantScoreQuery::new(TermQuery::new(Term::from_text("foo", "B"))),
        Occur::Should,
      )?
      .add(
        BoostQuery::new(
          ConstantScoreQuery::new(TermQuery::new(Term::from_text("foo", "D"))),
          4.0,
        )?,
        Occur::Should,
      )?;
    inner.set_minimum_number_should_match(2);

    let inner_query: Query = WANDScorerQuery::new(inner.build(), random.random_bool(0.5)).into();

    let mut outer = Builder::new();
    outer
      .add(inner_query, Occur::Must)?
      .add(TermQuery::new(Term::from_text("foo", "C")), Occur::MustNot)?;
    outer.build().into()
  };

  let weight = searcher.create_weight(searcher.rewrite(query)?, ScoreMode::TopScores, 1.0)?;
  let mut scorer = weight
    .scorer(context, &searcher)?
    .expect("expected scorer to be present");

  assert_eq!(0, scorer.iterator_mut().next_doc()?);
  assert_eq!(3.0, scorer.score()?); // 2 + 1

  assert_eq!(4, scorer.iterator_mut().next_doc()?);
  assert_eq!(5.0, scorer.score()?); // 1 + 4

  assert_eq!(NO_MORE_DOCS, scorer.iterator_mut().next_doc()?);

  let mut ss = weight
    .scorer_supplier(context, &searcher)?
    .expect("expected scorer supplier");
  ss.set_top_level_scoring_clause()?;
  let mut scorer = ss.get(i64::MAX, context, &searcher)?;
  scorer.set_min_competitive_score(4.0)?;

  assert_eq!(4, scorer.iterator_mut().next_doc()?);
  assert_eq!(5.0, scorer.score()?);

  assert_eq!(NO_MORE_DOCS, scorer.iterator_mut().next_doc()?);

  Ok(())
}

#[test]
fn test_basics_with_filtered_disjunction_and_must_not_and_min_should_match_and_non_scoring_mode()
-> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;
  let mut conf = new_index_writer_config(&mut random)?;
  conf.set_merge_policy(new_log_merge_policy(&mut random)?);
  let w = IndexWriter::new(dir.clone(), conf)?;

  let docs: &[&[&str]] = &[
    &["A", "B"],           // 0
    &["A", "C", "D"],      // 1
    &[],                   // 2
    &["A", "B", "C", "D"], // 3
    &["B", "D"],           // 4
    &["C", "D"],           // 5
  ];

  for values in docs {
    let mut doc = Document::new();
    for value in *values {
      doc.add(StringField::from_string("foo", *value, Store::No)?);
    }
    w.add_document(doc)?;
  }

  w.force_merge(1)?;
  w.close()?;

  let reader = directory_reader::open(dir)?;
  let searcher = new_searcher_with_reader(reader)?;
  let context = &searcher.get_leaf_contexts()?[0];

  let query: Query = {
    let mut inner = Builder::new();
    inner
      .add(
        BoostQuery::new(
          ConstantScoreQuery::new(TermQuery::new(Term::from_text("foo", "A"))),
          2.0,
        )?,
        Occur::Should,
      )?
      .add(
        ConstantScoreQuery::new(TermQuery::new(Term::from_text("foo", "B"))),
        Occur::Should,
      )?
      .add(
        BoostQuery::new(
          ConstantScoreQuery::new(TermQuery::new(Term::from_text("foo", "D"))),
          4.0,
        )?,
        Occur::Should,
      )?;
    inner.set_minimum_number_should_match(2);

    let inner_query: Query = WANDScorerQuery::new(inner.build(), random.random_bool(0.5)).into();

    let mut outer = Builder::new();
    outer
      .add(inner_query, Occur::Must)?
      .add(TermQuery::new(Term::from_text("foo", "C")), Occur::MustNot)?;
    outer.build().into()
  };

  let weight =
    searcher.create_weight(searcher.rewrite(query)?, ScoreMode::CompleteNoScores, 1.0)?;
  let mut scorer = weight
    .scorer(context, &searcher)?
    .expect("expected scorer to be present");

  assert_eq!(0, scorer.iterator_mut().next_doc()?);
  assert_eq!(4, scorer.iterator_mut().next_doc()?);
  assert_eq!(NO_MORE_DOCS, scorer.iterator_mut().next_doc()?);

  Ok(())
}

#[test]
fn test_random() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let w = IndexWriter::new(dir.clone(), new_index_writer_config(&mut random)?)?;
  let num_docs = at_least(&mut random, 1000);
  for _ in 0..num_docs {
    let mut doc = Document::new();
    let v = random.random_range(0..5);
    let num_values = random.random_range(0..1 << v);
    let start = random.random_range(0..10);
    for j in 0..num_values {
      doc.add(StringField::from_string(
        "foo",
        (start + j).to_string(),
        Store::No,
      )?);
    }
    w.add_document(doc)?;
  }

  let reader = directory_reader::open_from_writer(&w)?;
  w.close()?;

  // turn off concurrent search to avoid Random object used across threads resulting into
  // Runtime error because `WANDScorerQuery::create_weight` references this searcher,
  // but will be called during searching
  let searcher = new_searcher_with_threads(&mut random, reader, true, true, false)?;

  for _ in 0..100 {
    let start = random.random_range(0..10);
    let v = random.random_range(0..5);
    let num_clauses = random.random_range(0..1 << v);

    let mut builder = Builder::new();
    for i in 0..num_clauses {
      let tq = TermQuery::new(Term::from_text("foo", (start + i).to_string()));
      let q = maybe_wrap(&mut random, tq.into())?;
      builder.add(q, Occur::Should)?;
    }

    let query = WANDScorerQuery::new(builder.build(), random.random_bool(0.5)).into();

    CheckHits::check_top_scores(&mut random, &query, &searcher)?;

    let filter_term = random.random_range(0..30);
    let filtered_query: Query = {
      let mut b = Builder::new();
      b.add(query, Occur::Must)?.add(
        TermQuery::new(Term::from_text("foo", filter_term.to_string())),
        Occur::Filter,
      )?;
      b.build().into()
    };

    CheckHits::check_top_scores(&mut random, &filtered_query, &searcher)?;
  }

  Ok(())
}

/// Degenerate case: all clauses produce a score of 0.
#[test]
fn test_random_with_zero_scores() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let w = IndexWriter::new(dir.clone(), new_index_writer_config(&mut random)?)?;
  let num_docs = at_least(&mut random, 1000);
  for _ in 0..num_docs {
    let mut doc = Document::new();
    let v = random.random_range(0..5);
    let num_values = random.random_range(0..1 << v);
    let start = random.random_range(0..10);
    for j in 0..num_values {
      doc.add(StringField::from_string(
        "foo",
        (start + j).to_string(),
        Store::No,
      )?);
    }
    w.add_document(doc)?;
  }

  let reader = directory_reader::open_from_writer(&w)?;
  w.close()?;

  // turn off concurrent search to avoid Random object used across threads resulting into
  // Runtime error because `WANDScorerQuery::create_weight` references this searcher,
  // but will be called during searching
  let searcher = new_searcher_with_threads(&mut random, reader, true, true, false)?;

  for _ in 0..100 {
    let start = random.random_range(0..10);
    let v = random.random_range(0..5);
    let num_clauses = random.random_range(0..1 << v);

    let mut builder = Builder::new();
    for i in 0..num_clauses {
      let tq = TermQuery::new(Term::from_text("foo", (start + i).to_string()));
      let q: Query = BoostQuery::new(ConstantScoreQuery::new(tq), 0.0)?.into();
      let q = maybe_wrap(&mut random, q)?;
      builder.add(q, Occur::Should)?;
    }

    let query = WANDScorerQuery::new(builder.build(), random.random_bool(0.5)).into();

    CheckHits::check_top_scores(&mut random, &query, &searcher)?;

    let filter_term = random.random_range(0..30);
    let filtered_query: Query = {
      let mut b = Builder::new();
      b.add(query, Occur::Must)?.add(
        TermQuery::new(Term::from_text("foo", filter_term.to_string())),
        Occur::Filter,
      )?;
      b.build().into()
    };

    CheckHits::check_top_scores(&mut random, &filtered_query, &searcher)?;
  }

  Ok(())
}
/// Test the case when some clauses produce infinite max scores.
#[test]
fn test_random_with_infinite_max_score() -> Result<()> {
  do_test_random_special_max_score(f32::INFINITY)
}

/// Test the case when some clauses produce finite max scores, but their sum overflows.
#[test]
fn test_random_with_max_score_overflow() -> Result<()> {
  do_test_random_special_max_score(f32::MAX)
}

fn do_test_random_special_max_score(max_score: f32) -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let w = IndexWriter::new(dir.clone(), new_index_writer_config(&mut random)?)?;
  let num_docs = at_least(&mut random, 1000);
  for _ in 0..num_docs {
    let mut doc = Document::new();
    let v = random.random_range(0..5);
    let num_values = random.random_range(0..1 << v);
    let start = random.random_range(0..10);
    for j in 0..num_values {
      doc.add(StringField::from_string(
        "foo",
        (start + j).to_string(),
        Store::No,
      )?);
    }
    w.add_document(doc)?;
  }

  let reader = directory_reader::open_from_writer(&w)?;
  w.close()?;

  // turn off concurrent search to avoid Random object used across threads resulting into
  // Runtime error because `WANDScorerQuery::create_weight` references this searcher,
  // but will be called during searching
  let searcher = new_searcher_with_threads(&mut random, reader, true, true, false)?;

  for _ in 0..100 {
    let start = random.random_range(0..10);
    let v = random.random_range(0..5);
    let num_clauses = random.random_range(0..1 << v);

    let mut builder = Builder::new();
    for i in 0..num_clauses {
      let mut q: Query = TermQuery::new(Term::from_text("foo", (start + i).to_string())).into();

      if random.random_bool(0.5) {
        let denom = random.random_range(1..=100);
        let max_range = num_docs / denom;
        q = MaxScoreWrapperQuery::new(q, max_range, max_score).into();
      }

      builder.add(q, Occur::Should)?;
    }

    let query = WANDScorerQuery::new(builder.build(), random.random_bool(0.5)).into();

    CheckHits::check_top_scores(&mut random, &query, &searcher)?;

    let filter_term = random.random_range(0..30);
    let filtered_query: Query = {
      let mut b = Builder::new();
      b.add(query, Occur::Must)?.add(
        TermQuery::new(Term::from_text("foo", filter_term.to_string())),
        Occur::Filter,
      )?;
      b.build().into()
    };

    CheckHits::check_top_scores(&mut random, &filtered_query, &searcher)?;
  }

  Ok(())
}
