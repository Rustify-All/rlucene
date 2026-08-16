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
use crate::core::analysis::token_attributes::term_frequency_attribute::TermFrequencyAttribute;
use crate::core::analysis::token_stream::TokenStream;
use crate::core::document::document::Document;
use crate::core::document::field::{Field, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::fields::FieldTokenStreamEnum;
use crate::core::document::float_point::FloatPoint;
use crate::core::document::string_field::StringField;
use crate::core::index::directory_reader;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::index_writer::IndexWriter;
use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
use crate::core::index::term::Term;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::boolean_query::Builder;
use crate::core::search::constant_score_query::ConstantScoreQuery;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::query::{Query, QueryWeightSsScorer};
use crate::core::search::req_opt_sum_scorer::ReqOptSumScorer;
use crate::core::search::scorable::Scorable;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::{Scorer, TwoPhaseState};
use crate::core::search::term_query::TermQuery;
use crate::core::search::top_score_doc_collector_manager::TopScoreDocCollectorManager;
use crate::core::search::two_phase_iterator::TwoPhaseIterator;
use crate::core::util::attribute_source::{AttributeSource, Attributes};
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::index::random_index_writer::RandomIndexWriter;
use crate::test_framework::core::search::check_hits::CheckHits;
use crate::test_framework::core::search::random_approximation_query::RandomApproximationQuery;
use crate::test_framework::core::search::similarity::new_simple_similarity;
use crate::test_framework::core::util::lucene_test_case::{
  at_least, new_directory_shared, new_index_writer_config, new_log_merge_policy,
  new_log_merge_policy_with_cfs, new_searcher_with_reader, random,
};
use rand::Rng;
use rand::RngExt;

#[allow(dead_code)]
struct TestReqOptSumScorer;
#[test]
fn test_basics_must() -> Result<()> {
  let mut random = random();
  do_test_basics(&mut random, Occur::Must)
}

#[test]
fn test_basics_filter() -> Result<()> {
  let mut random = random();
  do_test_basics(&mut random, Occur::Filter)
}

fn do_test_basics<R>(random: &mut R, req_occur: Occur) -> Result<()>
where
  R: Rng + ?Sized,
{
  let dir = new_directory_shared(random)?;

  let mut iwc = new_index_writer_config(random)?;
  let use_cfs = random.random_bool(0.5);
  iwc.set_merge_policy(new_log_merge_policy_with_cfs(random, use_cfs)?);
  let w = RandomIndexWriter::with_config(random, dir.clone(), iwc);

  {
    let mut doc = Document::new();
    doc.add(StringField::from_string("f", "foo".to_string(), Store::No)?);
    w.add_document(random, doc)?;
  }
  {
    let mut doc = Document::new();
    doc.add(StringField::from_string("f", "foo".to_string(), Store::No)?);
    doc.add(StringField::from_string("f", "bar".to_string(), Store::No)?);
    w.add_document(random, doc)?;
  }
  {
    let mut doc = Document::new();
    doc.add(StringField::from_string("f", "foo".to_string(), Store::No)?);
    w.add_document(random, doc)?;
  }
  {
    let mut doc = Document::new();
    doc.add(StringField::from_string("f", "bar".to_string(), Store::No)?);
    w.add_document(random, doc)?;
  }
  {
    let mut doc = Document::new();
    doc.add(StringField::from_string("f", "foo".to_string(), Store::No)?);
    doc.add(StringField::from_string("f", "bar".to_string(), Store::No)?);
    w.add_document(random, doc)?;
  }

  w.force_merge(random, 1)?;

  let reader = w.get_reader(random)?;
  w.close(random)?;

  let searcher = new_searcher_with_reader(reader)?;
  let query: Query = {
    let mut b = Builder::new();
    b.add(
      ConstantScoreQuery::new(TermQuery::new(Term::from_text("f", "foo"))),
      req_occur,
    )?
    .add(
      ConstantScoreQuery::new(TermQuery::new(Term::from_text("f", "bar"))),
      Occur::Should,
    )?;
    b.build().into()
  };

  let weight = searcher.create_weight(searcher.rewrite(query)?, ScoreMode::TopScores, 1.0)?;
  let context = &searcher.get_leaf_contexts()?[0];

  let mut scorer = weight.scorer(context, &searcher)?.expect("expected scorer");
  assert_eq!(0, scorer.iterator_mut().next_doc()?);
  assert_eq!(1, scorer.iterator_mut().next_doc()?);
  assert_eq!(2, scorer.iterator_mut().next_doc()?);
  assert_eq!(4, scorer.iterator_mut().next_doc()?);
  assert_eq!(NO_MORE_DOCS, scorer.iterator_mut().next_doc()?);

  let mut ss = weight
    .scorer_supplier(context, &searcher)?
    .expect("expected scorer supplier");
  ss.set_top_level_scoring_clause()?;
  let mut scorer = ss.get(i64::MAX, context, &searcher)?;
  scorer.set_min_competitive_score(FloatPoint::next_down(1.0))?;

  if req_occur == Occur::Must {
    assert_eq!(0, scorer.iterator_mut().next_doc()?);
  }
  assert_eq!(1, scorer.iterator_mut().next_doc()?);
  if req_occur == Occur::Must {
    assert_eq!(2, scorer.iterator_mut().next_doc()?);
  }
  assert_eq!(4, scorer.iterator_mut().next_doc()?);
  assert_eq!(NO_MORE_DOCS, scorer.iterator_mut().next_doc()?);

  let mut ss = weight
    .scorer_supplier(context, &searcher)?
    .expect("expected scorer supplier");
  ss.set_top_level_scoring_clause()?;
  let mut scorer = ss.get(i64::MAX, context, &searcher)?;
  scorer.set_min_competitive_score(FloatPoint::next_up(1.0))?;

  if req_occur == Occur::Must {
    assert_eq!(1, scorer.iterator_mut().next_doc()?);
    assert_eq!(4, scorer.iterator_mut().next_doc()?);
  }
  assert_eq!(NO_MORE_DOCS, scorer.iterator_mut().next_doc()?);

  let mut ss = weight
    .scorer_supplier(context, &searcher)?
    .expect("expected scorer supplier");
  ss.set_top_level_scoring_clause()?;
  let mut scorer = ss.get(i64::MAX, context, &searcher)?;

  assert_eq!(0, scorer.iterator_mut().next_doc()?);
  scorer.set_min_competitive_score(FloatPoint::next_up(1.0))?;
  if req_occur == Occur::Must {
    assert_eq!(1, scorer.iterator_mut().next_doc()?);
    assert_eq!(4, scorer.iterator_mut().next_doc()?);
  }
  assert_eq!(NO_MORE_DOCS, scorer.iterator_mut().next_doc()?);

  Ok(())
}
#[test]
fn test_max_block() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mut iwc = new_index_writer_config(&mut random)?;
  iwc.set_merge_policy(new_log_merge_policy(&mut random)?);
  let w = IndexWriter::new(dir.clone(), iwc)?;

  let mut ft = FieldType::new();
  ft.set_index_options(IndexOptions::DocsAndFreqs)?;
  ft.set_tokenized(true)?;
  ft.freeze();

  for i in 0..1024 {
    let mut doc = Document::new();
    doc.add(Field::from_token_stream(
      "foo",
      FieldTokenStreamEnum::custom(TermFreqTokenStream::new("a".to_string(), i + 1)),
      ft.clone(),
    )?);

    if random.random::<f32>() < 0.5 {
      doc.add(Field::from_token_stream(
        "foo",
        FieldTokenStreamEnum::custom(TermFreqTokenStream::new("b".to_string(), 1)),
        ft.clone(),
      )?);
    }

    w.add_document(doc)?;
  }

  w.force_merge(1)?;
  w.close()?;

  let reader = directory_reader::open(dir)?;
  let mut searcher = new_searcher_with_reader(reader)?;
  searcher.set_similarity(new_simple_similarity());

  let req_q = TermQuery::new(Term::from_text("foo", "a"));
  let opt_q = TermQuery::new(Term::from_text("foo", "b"));

  let mut builder = Builder::new();
  builder.add(req_q.clone(), Occur::Must)?;
  builder.add(opt_q.clone(), Occur::Should)?;
  let bool_q: Query = builder.build().into();

  let mut actual = req_opt_scorer(&searcher, req_q, opt_q, true)?;

  let leaves = searcher.get_leaf_contexts()?;
  let expected_weight = searcher.create_weight(bool_q, ScoreMode::Complete, 1.0)?;
  let mut expected = expected_weight.scorer(&leaves[0], &searcher)?.unwrap();

  actual.set_min_competitive_score(f32::next_up(1.0))?;

  for i in 0..1024 {
    assert_eq!(i, actual.iterator_mut().next_doc()?);
    assert_eq!(i, expected.iterator_mut().next_doc()?);
    assert_eq!(actual.score()?, expected.score()?);
  }

  Ok(())
}
#[test]
fn test_max_score_segment() -> Result<()> {
  let mut random = random();
  let dir = new_directory_shared(&mut random)?;

  let mut conf = new_index_writer_config(&mut random)?;
  conf.set_merge_policy(new_log_merge_policy(&mut random)?);
  let w = IndexWriter::new(dir.clone(), conf)?;

  let docs: &[&[&str]] = &[
    &["A"],      // 0
    &["A"],      // 1
    &[],         // 2
    &["A", "B"], // 3
    &["A"],      // 4
    &["B"],      // 5
    &["A", "B"], // 6
    &["B"],      // 7
  ];

  for values in docs {
    let mut doc = Document::new();
    for v in *values {
      doc.add(StringField::from_string(
        "foo",
        (*v).to_string(),
        Store::No,
      )?);
    }
    w.add_document(doc)?;
  }

  w.force_merge(1)?;
  w.close()?;

  let reader = directory_reader::open(dir)?;
  let searcher = new_searcher_with_reader(reader)?;
  let _ctx = &searcher.get_leaf_contexts()?[0];

  let req_q = ConstantScoreQuery::new(TermQuery::new(Term::from_text("foo", "A")));
  let opt_q = ConstantScoreQuery::new(TermQuery::new(Term::from_text("foo", "B")));

  let mut scorer = req_opt_scorer(&searcher, req_q.clone(), opt_q.clone(), false)?;

  assert_eq!(0, scorer.iterator_mut().next_doc()?);
  assert_eq!(1.0, scorer.score()?);
  assert_eq!(1, scorer.iterator_mut().next_doc()?);
  assert_eq!(1.0, scorer.score()?);
  assert_eq!(3, scorer.iterator_mut().next_doc()?);
  assert_eq!(2.0, scorer.score()?);
  assert_eq!(4, scorer.iterator_mut().next_doc()?);
  assert_eq!(1.0, scorer.score()?);
  assert_eq!(6, scorer.iterator_mut().next_doc()?);
  assert_eq!(2.0, scorer.score()?);
  assert_eq!(NO_MORE_DOCS, scorer.iterator_mut().next_doc()?);

  let mut scorer = req_opt_scorer(&searcher, req_q.clone(), opt_q.clone(), false)?;
  scorer.set_min_competitive_score(f32::from_bits(1.0f32.to_bits() - 1))?;
  assert_eq!(0, scorer.iterator_mut().next_doc()?);
  assert_eq!(1.0, scorer.score()?);
  assert_eq!(1, scorer.iterator_mut().next_doc()?);
  assert_eq!(1.0, scorer.score()?);
  assert_eq!(3, scorer.iterator_mut().next_doc()?);
  assert_eq!(2.0, scorer.score()?);
  assert_eq!(4, scorer.iterator_mut().next_doc()?);
  assert_eq!(1.0, scorer.score()?);
  assert_eq!(6, scorer.iterator_mut().next_doc()?);
  assert_eq!(2.0, scorer.score()?);
  assert_eq!(NO_MORE_DOCS, scorer.iterator_mut().next_doc()?);

  let mut scorer = req_opt_scorer(&searcher, req_q.clone(), opt_q.clone(), false)?;
  scorer.set_min_competitive_score(f32::from_bits(1.0f32.to_bits() + 1))?;
  assert_eq!(3, scorer.iterator_mut().next_doc()?);
  assert_eq!(2.0, scorer.score()?);
  assert_eq!(6, scorer.iterator_mut().next_doc()?);
  assert_eq!(2.0, scorer.score()?);
  assert_eq!(NO_MORE_DOCS, scorer.iterator_mut().next_doc()?);

  let mut scorer = req_opt_scorer(&searcher, req_q, opt_q, true)?;
  scorer.set_min_competitive_score(f32::from_bits(2.0f32.to_bits() + 1))?;
  assert_eq!(NO_MORE_DOCS, scorer.iterator_mut().next_doc()?);

  Ok(())
}
#[test]
fn test_must_random_frequent_opt() -> Result<()> {
  let mut random = random();
  do_test_random(&mut random, Occur::Must, 0.5)
}

#[test]
fn test_must_random_rare_opt() -> Result<()> {
  let mut random = random();
  do_test_random(&mut random, Occur::Must, 0.05)
}

#[test]
fn test_filter_random_frequent_opt() -> Result<()> {
  let mut random = random();
  do_test_random(&mut random, Occur::Filter, 0.5)
}

#[test]
fn test_filter_random_rare_opt() -> Result<()> {
  let mut random = random();
  do_test_random(&mut random, Occur::Filter, 0.05)
}

fn do_test_random<R>(random: &mut R, req_occur: Occur, opt_freq: f64) -> Result<()>
where
  R: Rng + ?Sized,
{
  let dir = new_directory_shared(random)?;
  let config = new_index_writer_config(random)?;
  let w = RandomIndexWriter::with_config(random, dir.clone(), config);
  let num_docs = at_least(random, 1000);

  for _ in 0..num_docs {
    let num_as = if random.random_bool(0.5) {
      0usize
    } else {
      1 + random.random_range(0..5)
    };
    let num_bs = if random.random::<f64>() < opt_freq {
      0usize
    } else {
      1 + random.random_range(0..5)
    };

    let mut doc = Document::new();
    for _ in 0..num_as {
      doc.add(StringField::from_string("f", "A".to_string(), Store::No)?);
    }
    for _ in 0..num_bs {
      doc.add(StringField::from_string("f", "B".to_string(), Store::No)?);
    }
    if random.random_bool(0.5) {
      doc.add(StringField::from_string("f", "C".to_string(), Store::No)?);
    }
    w.add_document(random, doc)?;
  }

  let reader = w.get_reader(random)?;
  w.close(random)?;
  let searcher = new_searcher_with_reader(reader)?;

  let must_term: Query = TermQuery::new(Term::from_text("f", "A")).into();
  let should_term: Query = TermQuery::new(Term::from_text("f", "B")).into();

  let mut query: Query = {
    let mut b = Builder::new();
    b.add(must_term.clone(), req_occur)?
      .add(should_term.clone(), Occur::Should)?;
    b.build().into()
  };

  let collector_manager = TopScoreDocCollectorManager::new(10, i32::MAX as usize)?;
  let top_docs = searcher.search_with_collector_manager(query.clone(), &collector_manager)?;
  let expected = top_docs.score_docs;
  // Also test a filtered query, since it does not compute the score on all
  // matches.
  query = {
    let mut b = Builder::new();
    b.add(query, Occur::Must)?
      .add(TermQuery::new(Term::from_text("f", "C")), Occur::Filter)?;
    b.build().into()
  };

  let collector_manager = TopScoreDocCollectorManager::new(10, i32::MAX as usize)?;
  let top_docs = searcher.search_with_collector_manager(query.clone(), &collector_manager)?;
  let expected_filtered = top_docs.score_docs;

  CheckHits::check_top_scores(random, &query, &searcher)?;

  {
    let mut q: Query = {
      let mut b = Builder::new();
      b.add(
        RandomApproximationQuery::new(must_term.clone(), random),
        req_occur,
      )?
      .add(should_term.clone(), Occur::Should)?;
      b.build().into()
    };

    let collector_manager = TopScoreDocCollectorManager::new(10, 1)?;
    let top_docs = searcher.search_with_collector_manager(q.clone(), &collector_manager)?;
    let actual = top_docs.score_docs;
    CheckHits::check_equal(&query, &expected, &actual)?;

    q = {
      let mut b = Builder::new();
      b.add(must_term.clone(), req_occur)?.add(
        RandomApproximationQuery::new(should_term.clone(), random),
        Occur::Should,
      )?;
      b.build().into()
    };

    let collector_manager = TopScoreDocCollectorManager::new(10, 1)?;
    let top_docs = searcher.search_with_collector_manager(q.clone(), &collector_manager)?;
    let actual = top_docs.score_docs;
    CheckHits::check_equal(&q, &expected, &actual)?;

    q = {
      let mut b = Builder::new();
      b.add(
        RandomApproximationQuery::new(must_term.clone(), random),
        req_occur,
      )?
      .add(
        RandomApproximationQuery::new(should_term.clone(), random),
        Occur::Should,
      )?;
      b.build().into()
    };

    let collector_manager = TopScoreDocCollectorManager::new(10, 1)?;
    let top_docs = searcher.search_with_collector_manager(q.clone(), &collector_manager)?;
    let actual = top_docs.score_docs;
    CheckHits::check_equal(&q, &expected, &actual)?;
  }

  {
    let nested_q: Query = {
      let mut b = Builder::new();
      b.add(query.clone(), Occur::Must)?
        .add(TermQuery::new(Term::from_text("f", "C")), Occur::Filter)?;
      b.build().into()
    };

    CheckHits::check_top_scores(random, &nested_q, &searcher)?;

    query = {
      let mut b = Builder::new();
      b.add(query.clone(), Occur::Must)?.add(
        RandomApproximationQuery::new(TermQuery::new(Term::from_text("f", "C")), random),
        Occur::Filter,
      )?;
      b.build().into()
    };

    let collector_manager = TopScoreDocCollectorManager::new(10, 1)?;
    let top_docs = searcher.search_with_collector_manager(nested_q.clone(), &collector_manager)?;
    let actual_filtered = top_docs.score_docs;
    CheckHits::check_equal(&nested_q, &expected_filtered, &actual_filtered)?;
  }

  {
    query = {
      let mut b = Builder::new();
      b.add(query, req_occur)?
        .add(TermQuery::new(Term::from_text("f", "C")), Occur::Should)?;
      b.build().into()
    };

    CheckHits::check_top_scores(random, &query, &searcher)?;

    query = {
      let mut b = Builder::new();
      b.add(TermQuery::new(Term::from_text("f", "C")), req_occur)?
        .add(query, Occur::Should)?;
      b.build().into()
    };

    CheckHits::check_top_scores(random, &query, &searcher)?;
  }
  Ok(())
}

fn req_opt_scorer<IRC, Q>(
  searcher: &IndexSearcher<IRC>,
  req_q: Q,
  opt_q: Q,
  with_block_score: bool,
) -> Result<ReqOptSumScorer<QueryWeightSsScorer, QueryWeightSsScorer>>
where
  Q: Into<Query>,
  IRC: IndexReaderContext,
{
  let req_q = req_q.into();
  let opt_q = opt_q.into();
  let ctx = &searcher.get_leaf_contexts()?[0];

  let req_scorer = searcher
    .create_weight(req_q, ScoreMode::TopScores, 1.0)?
    .scorer(ctx, searcher)?
    .expect("required scorer");

  let opt_scorer = searcher
    .create_weight(opt_q, ScoreMode::TopScores, 1.0)?
    .scorer(ctx, searcher)?
    .expect("optional scorer");
  let v = match with_block_score {
    true => ReqOptSumScorer::new(req_scorer, opt_scorer, ScoreMode::TopScores)?,
    false => ReqOptSumScorer::with_fixed_max_score(req_scorer, opt_scorer, ScoreMode::TopScores)?,
  };
  Ok(v)
}

struct ReqOptSumScorerWrapper<S1, S2> {
  base: ReqOptSumScorer<S1, S2>,
}
impl<S1, S2> ReqOptSumScorerWrapper<S1, S2>
where
  S1: Scorer,
  S2: Scorer,
{
  fn new(base: ReqOptSumScorer<S1, S2>) -> Self {
    Self { base }
  }
}

impl<S1, S2> Scorable for ReqOptSumScorerWrapper<S1, S2>
where
  S1: Scorer + 'static,
  S2: Scorer + 'static,
{
  fn score(&mut self) -> Result<f32> {
    self.base.score()
  }

  fn cost(&self) -> Result<i64> {
    self.iterator().cost()
  }
}

impl<S1, S2> crate::core::search::scorable::FixedScore for ReqOptSumScorerWrapper<S1, S2>
where
  S1: Scorer + 'static,
  S2: Scorer + 'static,
{
}

impl<S1, S2> Scorer for ReqOptSumScorerWrapper<S1, S2>
where
  S1: Scorer + 'static,
  S2: Scorer + 'static,
{
  fn doc_id(&mut self) -> Result<i32> {
    self.base.doc_id()
  }

  fn iterator(&self) -> Box<dyn DocIdSetIterator + '_> {
    self.base.iterator()
  }

  fn iterator_mut(&mut self) -> Box<dyn DocIdSetIterator + '_> {
    self.base.iterator_mut()
  }

  fn take_iterator(self: Box<Self>) -> Box<dyn DocIdSetIterator> {
    let ReqOptSumScorerWrapper { base } = *self;
    Box::new(base).take_iterator()
  }

  fn two_phase_iterator(&self) -> Option<Box<dyn TwoPhaseIterator + '_>> {
    self.base.two_phase_iterator()
  }

  fn two_phase_iterator_mut(&mut self) -> Option<Box<dyn TwoPhaseIterator + '_>> {
    self.base.two_phase_iterator_mut()
  }

  fn take_two_phase_iterator(self: Box<Self>) -> Option<Box<dyn TwoPhaseIterator>>
  where
    Self: Sized,
  {
    let ReqOptSumScorerWrapper { base } = *self;
    Box::new(base).take_two_phase_iterator()
  }

  fn advance_shallow(&mut self, target: i32) -> Result<i32> {
    self.base.advance_shallow(target)
  }

  fn get_max_score(&mut self, _up_to: i32) -> Result<f32> {
    Ok(f32::MAX)
  }

  fn default_cost(&mut self) -> Result<i64> {
    self.base.default_cost()
  }

  fn has_two_phase_iterator(&self) -> TwoPhaseState {
    self.base.has_two_phase_iterator()
  }

  fn approximation(&self) -> Box<dyn DocIdSetIterator + '_> {
    self.base.approximation()
  }

  fn approximation_mut(&mut self) -> Box<dyn DocIdSetIterator + '_> {
    self.base.approximation_mut()
  }
}
pub struct TermFreqTokenStream {
  attr: Attributes,
  term: String,
  term_freq: i32,
  finish: bool,
}

impl TermFreqTokenStream {
  pub fn new(term: String, term_freq: i32) -> Self {
    Self {
      attr: Attributes::default(),
      term,
      term_freq,
      finish: false,
    }
  }
}

impl crate::core::util::close::Closeable for TermFreqTokenStream {}

impl TokenStream for TermFreqTokenStream {
  fn increment_token(&mut self) -> Result<bool> {
    if self.finish {
      return Ok(false);
    }

    self.clear_attributes()?;

    match self.attr {
      Attributes::PackedToken(ref mut token_attr) => {
        token_attr.append_str(Some(&self.term))?;
        token_attr.sub.set_term_frequency(self.term_freq)?;
      },
      _ => unreachable!("PackedTokenAttribute not found in TermFreqTokenStream"),
    }

    self.finish = true;
    Ok(true)
  }

  fn end(&mut self) -> Result<()> {
    Ok(())
  }

  fn reset(&mut self) -> Result<()> {
    self.finish = false;
    Ok(())
  }

  fn get_attribute_source(&self) -> &Attributes {
    &self.attr
  }

  fn get_attribute_source_mut(&mut self) -> &mut Attributes {
    &mut self.attr
  }
}

impl AttributeSource for TermFreqTokenStream {
  fn clear_attributes(&mut self) -> Result<()> {
    self.attr.clear_attributes()
  }
}
