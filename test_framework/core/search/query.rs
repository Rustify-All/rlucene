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
use crate::core::index::BytesRef;
use crate::core::index::doc_values::DocValues;
use crate::core::index::index_reader::Identity;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::term::Term;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::boolean_query::BooleanQuery;
use crate::core::search::bulk_scorer::BulkScorer;
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::constant_score_weight::ConstantScoreWeight;
use crate::core::search::doc_id_set_iterator::{AllDISI, DocIdSetIterator, NO_MORE_DOCS};
use crate::core::search::explanation::Explanation;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::leaf_collector::LeafCollector;
use crate::core::search::query::{
  IntoBoxQuery, Query, QueryBase, QueryWeight, QueryWeightSs, QueryWeightSsBulkScorer,
  QueryWeightSsScorer,
};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::scorable::Scorable;
use crate::core::search::score::Score;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::{Scorer, ScorerEnum2, TwoPhaseState};
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::term_query::TermQuery;
use crate::core::search::two_phase_iterator::TwoPhaseIterator;
use crate::core::search::wand_scorer::WANDScorer;
use crate::core::search::weight::{DefaultBulkScorer, DefaultScorerSupplier, Weight};
use crate::core::util::HasIdentity;
use crate::core::util::ToInt;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bit_set_iterator::BitSetIterator;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED;
use parking_lot::Mutex;
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use std::collections::HashMap;
use std::fmt::{Debug, Formatter};
use std::hash::{Hash, Hasher};
use std::mem;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};

#[derive(Clone)]
pub struct BrokenExplainTermQuery {
  id: Identity,
  term_query: TermQuery,
  pub(crate) toggle_explain_match: bool,
  pub(crate) break_explain_scores: bool,
}

impl BrokenExplainTermQuery {
  pub fn new<T>(term: T, toggle_explain_match: bool, break_explain_scores: bool) -> Self
  where
    T: Into<Arc<Term>>,
  {
    Self {
      id: Identity::new(),
      term_query: TermQuery::new(term),
      toggle_explain_match,
      break_explain_scores,
    }
  }
}

impl std::fmt::Debug for BrokenExplainTermQuery {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self.to_string("") {
      Ok(s) => write!(f, "{}", s),
      Err(_) => Err(std::fmt::Error),
    }
  }
}

impl PartialEq for BrokenExplainTermQuery {
  fn eq(&self, other: &Self) -> bool {
    self.term_query == other.term_query
      && self.toggle_explain_match == other.toggle_explain_match
      && self.break_explain_scores == other.break_explain_scores
  }
}

impl Eq for BrokenExplainTermQuery {}

impl Hash for BrokenExplainTermQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.term_query.hash(state);
    self.toggle_explain_match.hash(state);
    self.break_explain_scores.hash(state);
  }
}

impl HasIdentity for BrokenExplainTermQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl QueryBase for BrokenExplainTermQuery {
  fn to_string(&self, field: &str) -> Result<String> {
    self.term_query.to_string(field)
  }

  fn create_weight<IRC>(
    self,
    searcher: &IndexSearcher<IRC>,
    score_mode: &ScoreMode,
    boost: f32,
  ) -> Result<QueryWeight<IRC>>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    let inner_weight = self
      .term_query
      .clone()
      .create_weight(searcher, score_mode, boost)?;
    Ok(Box::new(BrokenExplainWeight::new(self, inner_weight)))
  }

  fn rewrite<IRC>(self, _searcher: &IndexSearcher<IRC>) -> Result<Query>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    Ok(self.into())
  }

  fn visit<QV>(&self, visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    self.term_query.visit(visitor)
  }
}

impl IntoBoxQuery for BrokenExplainTermQuery {
  fn into_box_query(self) -> Box<Query> {
    Box::new(self.into())
  }
}

pub(crate) struct BrokenExplainWeight<IRC> {
  query: Arc<Query>,
  in_: QueryWeight<IRC>,
}

impl<IRC> BrokenExplainWeight<IRC>
where
  IRC: IndexReaderContext,
{
  fn new(query: BrokenExplainTermQuery, inner_weight: QueryWeight<IRC>) -> Self {
    Self {
      query: Arc::new(query.into()),
      in_: inner_weight,
    }
  }
}

impl<IRC> SegmentCacheable<IRC> for BrokenExplainWeight<IRC>
where
  IRC: IndexReaderContext,
{
  fn is_cacheable(&self, ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    self.in_.is_cacheable(ctx)
  }
}

impl<IRC> Weight<IRC> for BrokenExplainWeight<IRC>
where
  IRC: IndexReaderContext,
{
  type ScorerSupplier = QueryWeightSs<IRC>;

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
    let query = match self.query.as_ref() {
      Query::BrokenExplainTerm(query) => query,
      _ => {
        return Err(LuceneError::illegal_state(
          "expected BrokenExplainTermQuery",
        ));
      },
    };
    let mut result = self.in_.explain(context, doc, searcher)?;
    if result.is_match() {
      if query.break_explain_scores {
        let value = result.get_value().to_f64().ok_or_else(|| {
          LuceneError::illegal_state(format!(
            "Explanation value is not a number: {:?}",
            result.get_value()
          ))
        })?;
        result = Explanation::match_(-value, "Broken Explanation Score", vec![result]);
      }
      if query.toggle_explain_match {
        result = Explanation::no_match("Broken Explanation Matching", vec![result]);
      }
    } else if query.toggle_explain_match {
      result = Explanation::match_(-42.0f32, "Broken Explanation Matching", vec![result]);
    }
    Ok(result)
  }

  fn get_query(&self) -> Arc<Query> {
    self.query.clone()
  }

  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::ScorerSupplier>> {
    match self.in_.scorer(context, searcher)? {
      Some(scorer) => Ok(Some(Box::new(DefaultScorerSupplier::new(scorer)))),
      None => Ok(None),
    }
  }
}

impl crate::core::util::accountable::Accountable for BrokenExplainTermQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(QUERY_DEFAULT_RAM_BYTES_USED)
  }
}

#[derive(Clone)]
pub struct TestRewriteQuery {
  num_rewrites: Arc<AtomicUsize>,
  id: Identity,
}

impl TestRewriteQuery {
  pub fn new(num_rewrites: Arc<AtomicUsize>) -> Self {
    Self {
      num_rewrites,
      id: Identity::new(),
    }
  }
}

impl Debug for TestRewriteQuery {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "TestRewriteQuery({:?})", self.num_rewrites)
  }
}

impl HasIdentity for TestRewriteQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl QueryBase for TestRewriteQuery {
  fn to_string(&self, _field: &str) -> Result<String> {
    Ok(format!("TestRewriteQuery({:?})", self.num_rewrites))
  }

  fn create_weight<IRC>(
    self,
    _searcher: &IndexSearcher<IRC>,
    _score_mode: &ScoreMode,
    _boost: f32,
  ) -> Result<QueryWeight<IRC>>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    Err(LuceneError::unsupported_operation(""))
  }

  fn rewrite<IRC>(self, _searcher: &IndexSearcher<IRC>) -> Result<Query>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    self.num_rewrites.fetch_add(1, Ordering::Relaxed);
    Ok(self.into())
  }

  fn visit<QV>(&self, _visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    Ok(())
  }
}

impl PartialEq for TestRewriteQuery {
  fn eq(&self, _other: &Self) -> bool {
    true
  }
}

impl Eq for TestRewriteQuery {}

impl Hash for TestRewriteQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    1.hash(state)
  }
}

impl crate::core::util::accountable::Accountable for TestRewriteQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(QUERY_DEFAULT_RAM_BYTES_USED)
  }
}

#[derive(Clone, Debug)]
pub struct CrazyMustUseBulkScorerQuery {
  id: Identity,
}

impl CrazyMustUseBulkScorerQuery {
  pub(crate) fn new() -> Self {
    Self {
      id: Identity::new(),
    }
  }
}

impl Default for CrazyMustUseBulkScorerQuery {
  fn default() -> Self {
    Self::new()
  }
}

impl PartialEq for CrazyMustUseBulkScorerQuery {
  fn eq(&self, other: &Self) -> bool {
    self.identity() == other.identity()
  }
}

impl Eq for CrazyMustUseBulkScorerQuery {}

impl Hash for CrazyMustUseBulkScorerQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.identity().hash(state);
  }
}

impl HasIdentity for CrazyMustUseBulkScorerQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl QueryBase for CrazyMustUseBulkScorerQuery {
  fn to_string(&self, _field: &str) -> Result<String> {
    Ok("MustUseBulkScorerQuery".to_string())
  }

  fn create_weight<IRC>(
    self,
    _searcher: &IndexSearcher<IRC>,
    _score_mode: &ScoreMode,
    _boost: f32,
  ) -> Result<QueryWeight<IRC>>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    Ok(Box::new(CrazyMustUseBulkScorerWeight::new(self)))
  }

  fn rewrite<IRC>(self, _searcher: &IndexSearcher<IRC>) -> Result<Query>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    Ok(self.into())
  }

  fn visit<QV>(&self, _visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    Ok(())
  }
}

struct CrazyMustUseBulkScorerWeight {
  query: Arc<Query>,
}

impl CrazyMustUseBulkScorerWeight {
  fn new(query: CrazyMustUseBulkScorerQuery) -> Self {
    Self {
      query: Arc::new(query.into()),
    }
  }
}

impl<IRC> SegmentCacheable<IRC> for CrazyMustUseBulkScorerWeight
where
  IRC: IndexReaderContext,
{
  fn is_cacheable(&self, _ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    Ok(false)
  }
}

impl<IRC> Weight<IRC> for CrazyMustUseBulkScorerWeight
where
  IRC: IndexReaderContext,
{
  fn matches<'a>(
    &'a self,
    _context: &'a LeafReaderContext<IRCLeafReader<IRC>>,
    _doc: i32,
    _searcher: &'a IndexSearcher<IRC>,
  ) -> Result<Option<crate::core::search::query::QueryWeightMatches<'a>>> {
    Ok(None)
  }

  fn explain(
    &self,
    _context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _doc: i32,
    _searcher: &IndexSearcher<IRC>,
  ) -> Result<Explanation> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_query(&self) -> Arc<Query> {
    self.query.clone()
  }

  type ScorerSupplier = QueryWeightSs<IRC>;

  fn scorer_supplier(
    &self,
    _context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::ScorerSupplier>> {
    Ok(Some(Box::new(CrazyMustUseBulkScorerSupplier)))
  }
}

struct CrazyMustUseBulkScorerSupplier;

impl<IRC> ScorerSupplier<IRC> for CrazyMustUseBulkScorerSupplier
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
    Err(LuceneError::unsupported_operation(""))
  }

  fn bulk_scorer(
    &mut self,
    _context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::BulkScorer>> {
    Ok(Some(Box::new(CrazyMustUseBulkScorer)))
  }

  fn cost(
    &mut self,
    _context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC>,
  ) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }
}

struct CrazyMustUseBulkScorer;

impl BulkScorer for CrazyMustUseBulkScorer {
  fn score(
    &mut self,
    collector: &mut dyn LeafCollector,
    accept_docs: Option<&dyn Bits>,
    min: i32,
    max: i32,
  ) -> Result<i32> {
    if min <= 0 && max > 0 && accept_docs.map_or(Ok(true), |bits| bits.get(0))? {
      let mut score = Score::default();
      collector.set_scorer(&mut score)?;
      collector.collect(0, &mut score)?;
    }
    Ok(NO_MORE_DOCS)
  }

  fn cost(&mut self) -> Result<i64> {
    Ok(1)
  }
}

impl crate::core::util::accountable::Accountable for CrazyMustUseBulkScorerQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(QUERY_DEFAULT_RAM_BYTES_USED)
  }
}

/// Wraps a query, checking that the score mode passed to `Weight` is the expected value.
#[derive(Clone, Debug)]
pub struct AssertNeedsScores {
  query: Box<Query>,
  value: ScoreMode,
  id: Identity,
}

impl AssertNeedsScores {
  pub fn new<T>(query: T, value: ScoreMode) -> Self
  where
    T: IntoBoxQuery,
  {
    Self {
      query: query.into_box_query(),
      value,
      id: Identity::new(),
    }
  }
}

impl HasIdentity for AssertNeedsScores {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl PartialEq for AssertNeedsScores {
  fn eq(&self, other: &Self) -> bool {
    self.query == other.query && self.value == other.value
  }
}

impl Eq for AssertNeedsScores {}

impl Hash for AssertNeedsScores {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.query.hash(state);
    mem::discriminant(&self.value).hash(state);
  }
}

impl QueryBase for AssertNeedsScores {
  fn to_string(&self, field: &str) -> Result<String> {
    Ok(format!("asserting({})", self.query.to_string(field)?))
  }

  fn create_weight<IRC>(
    self,
    searcher: &IndexSearcher<IRC>,
    score_mode: &ScoreMode,
    boost: f32,
  ) -> Result<QueryWeight<IRC>>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    assert_eq!(
      self.value,
      *score_mode,
      "query={}",
      self.query.to_string("")?
    );
    let inner_weight = (*self.query)
      .clone()
      .create_weight(searcher, score_mode, boost)?;
    assert_eq!(self.value, *score_mode);
    Ok(Box::new(AssertNeedsScoresWeight::new(self, inner_weight)))
  }

  fn rewrite<IRC>(mut self, searcher: &IndexSearcher<IRC>) -> Result<Query>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    let query_id = self.query.identity().clone();
    let rewritten = self.query.rewrite(searcher)?;
    if rewritten.identity() != &query_id {
      Ok(AssertNeedsScores::new(rewritten, self.value).into())
    } else {
      self.query = Box::new(rewritten);
      Ok(self.into())
    }
  }

  fn visit<QV>(&self, visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    self.query.visit(visitor)
  }
}

impl IntoBoxQuery for AssertNeedsScores {
  fn into_box_query(self) -> Box<Query> {
    Box::new(self.into())
  }
}

struct AssertNeedsScoresWeight<IRC> {
  query: Arc<Query>,
  inner_weight: QueryWeight<IRC>,
}

impl<IRC> AssertNeedsScoresWeight<IRC>
where
  IRC: IndexReaderContext,
{
  fn new(query: AssertNeedsScores, inner_weight: QueryWeight<IRC>) -> Self {
    Self {
      query: Arc::new(query.into()),
      inner_weight,
    }
  }
}

impl<IRC> SegmentCacheable<IRC> for AssertNeedsScoresWeight<IRC>
where
  IRC: IndexReaderContext,
{
  fn is_cacheable(&self, ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    self.inner_weight.is_cacheable(ctx)
  }
}

impl<IRC> Weight<IRC> for AssertNeedsScoresWeight<IRC>
where
  IRC: IndexReaderContext,
{
  type ScorerSupplier = QueryWeightSs<IRC>;

  fn matches<'a>(
    &'a self,
    context: &'a LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &'a IndexSearcher<IRC>,
  ) -> Result<Option<crate::core::search::query::QueryWeightMatches<'a>>> {
    self.inner_weight.matches(context, doc, searcher)
  }

  fn explain(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Explanation> {
    self.inner_weight.explain(context, doc, searcher)
  }

  fn get_query(&self) -> Arc<Query> {
    self.query.clone()
  }

  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::ScorerSupplier>> {
    self.inner_weight.scorer_supplier(context, searcher)
  }

  fn count(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<i32> {
    self.inner_weight.count(context, searcher)
  }
}

impl crate::core::util::accountable::Accountable for AssertNeedsScores {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(QUERY_DEFAULT_RAM_BYTES_USED)
  }
}

#[derive(Clone, Debug)]
pub struct BitSetQuery {
  docs: Arc<FixedBitSet>,
  id: Identity,
}

impl BitSetQuery {
  pub fn new(docs: Arc<FixedBitSet>) -> Self {
    Self {
      docs,
      id: Identity::new(),
    }
  }
}

impl HasIdentity for BitSetQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl QueryBase for BitSetQuery {
  fn to_string(&self, _field: &str) -> Result<String> {
    Ok("randomBitSetFilter".to_string())
  }

  fn create_weight<IRC>(
    self,
    _searcher: &IndexSearcher<IRC>,
    score_mode: &ScoreMode,
    boost: f32,
  ) -> Result<QueryWeight<IRC>>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    Ok(Box::new(BitSetQueryWeight::new(boost, self, *score_mode)))
  }

  fn rewrite<IRC>(self, _searcher: &IndexSearcher<IRC>) -> Result<Query>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    Ok(self.into())
  }

  fn visit<QV>(&self, _visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    Ok(())
  }
}

impl PartialEq for BitSetQuery {
  fn eq(&self, other: &Self) -> bool {
    self.docs == other.docs
  }
}

impl Hash for BitSetQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: std::hash::Hasher,
  {
    self.docs.hash(state);
  }
}

impl Eq for BitSetQuery {}

pub struct BitSetQueryWeight {
  docs: Arc<FixedBitSet>,
  score_mode: ScoreMode,
  query: Arc<Query>,
  base: ConstantScoreWeight,
}

impl BitSetQueryWeight {
  pub fn new(score: f32, query: BitSetQuery, score_mode: ScoreMode) -> Self {
    let docs = query.docs.clone();
    Self {
      docs,
      score_mode,
      query: Arc::new(query.into()),
      base: ConstantScoreWeight::new(score),
    }
  }
}

impl<IRC> SegmentCacheable<IRC> for BitSetQueryWeight
where
  IRC: IndexReaderContext,
{
  fn is_cacheable(&self, _ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    Ok(false)
  }
}

impl<IRC> Weight<IRC> for BitSetQueryWeight
where
  IRC: IndexReaderContext,
{
  fn explain(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Explanation> {
    let scorer = self.scorer(context, searcher)?;
    self.base.explain(scorer, doc, self.query.to_string("")?)
  }

  fn get_query(&self) -> Arc<Query> {
    self.query.clone()
  }

  type ScorerSupplier = QueryWeightSs<IRC>;

  fn scorer_supplier(
    &self,
    _context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::ScorerSupplier>> {
    let iter = BitSetIterator::new(
      self.docs.clone(),
      self.docs.approximate_cardinality() as i64,
    )?;
    let s = ConstantScoreScorer::from_disi(self.base.score(), self.score_mode, iter);
    let ss = DefaultScorerSupplier::new(s);
    Ok(Some(Box::new(ss)))
  }
}

impl crate::core::util::accountable::Accountable for BitSetQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(QUERY_DEFAULT_RAM_BYTES_USED)
  }
}

#[derive(Clone, Debug)]
pub struct RandomQuery {
  id: Identity,
  seed: u64,
  density: f32,
  doc_values: Arc<Vec<Option<BytesRef<Vec<u8>>>>>,
  #[allow(clippy::type_complexity)]
  pub match_values: Arc<Mutex<Vec<Option<BytesRef<Vec<u8>>>>>>,
  bitsets: Arc<Mutex<HashMap<Identity, Arc<FixedBitSet>>>>,
}

impl RandomQuery {
  pub fn new(seed: u64, density: f32, doc_values: Arc<Vec<Option<BytesRef<Vec<u8>>>>>) -> Self {
    Self {
      id: Identity::new(),
      seed,
      density,
      doc_values,
      match_values: Arc::new(Mutex::new(Vec::new())),
      bitsets: Arc::new(Mutex::new(HashMap::new())),
    }
  }

  fn bitset_for_context<LR>(&self, context: &LeafReaderContext<LR>) -> Result<Arc<FixedBitSet>>
  where
    LR: LeafReader,
  {
    let key = context.base().id().clone();
    if let Some(bits) = self.bitsets.lock().get(&key).cloned() {
      return Ok(bits);
    }

    let mut random = StdRng::seed_from_u64((context.doc_base as u64) ^ self.seed);
    let max_doc = context.reader().max_doc()?;
    let mut id_source = context.reader().get_numeric_doc_values("id")?.unwrap();
    let mut bitset = FixedBitSet::new(max_doc as usize);

    for doc_id in 0..max_doc {
      let actual_doc_id = id_source.next_doc()?;
      if actual_doc_id != doc_id {
        return Err(LuceneError::illegal_state(format!(
          "expected doc values doc id {doc_id}, got {actual_doc_id}"
        )));
      }

      if random.random::<f32>() <= self.density {
        bitset.set(doc_id as usize);
        let value_ord = id_source.long_value()? as usize;
        let value = self.doc_values.get(value_ord).unwrap();
        self
          .match_values
          .lock()
          .push(value.as_ref().map(BytesRef::deep_copy_of));
      }
    }

    let bitset = Arc::new(bitset);
    self.bitsets.lock().insert(key, bitset.clone());
    Ok(bitset)
  }
}

impl PartialEq for RandomQuery {
  fn eq(&self, other: &Self) -> bool {
    self.seed == other.seed
      && self.density.to_bits() == other.density.to_bits()
      && Arc::ptr_eq(&self.doc_values, &other.doc_values)
  }
}

impl Eq for RandomQuery {}

impl Hash for RandomQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.seed.hash(state);
    self.density.to_bits().hash(state);
    Arc::as_ptr(&self.doc_values).hash(state);
  }
}

impl HasIdentity for RandomQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl QueryBase for RandomQuery {
  fn to_string(&self, _field: &str) -> Result<String> {
    Ok(format!("RandomFilter(density={})", self.density))
  }

  fn create_weight<IRC>(
    self,
    _searcher: &IndexSearcher<IRC>,
    score_mode: &ScoreMode,
    boost: f32,
  ) -> Result<QueryWeight<IRC>>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    Ok(Box::new(RandomQueryWeight::new(self, *score_mode, boost)))
  }

  fn rewrite<IRC>(self, _searcher: &IndexSearcher<IRC>) -> Result<Query>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    Ok(self.into())
  }

  fn visit<QV>(&self, _visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    Ok(())
  }
}

pub(crate) struct RandomQueryWeight {
  query: Arc<Query>,
  random_query: RandomQuery,
  score_mode: ScoreMode,
  base: ConstantScoreWeight,
}

impl RandomQueryWeight {
  fn new(query: RandomQuery, score_mode: ScoreMode, boost: f32) -> Self {
    Self {
      query: Arc::new(query.clone().into()),
      random_query: query,
      score_mode,
      base: ConstantScoreWeight::new(boost),
    }
  }
}

impl<IRC> SegmentCacheable<IRC> for RandomQueryWeight
where
  IRC: IndexReaderContext,
{
  fn is_cacheable(&self, _ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    Ok(false)
  }
}

impl<IRC> Weight<IRC> for RandomQueryWeight
where
  IRC: IndexReaderContext,
{
  fn explain(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Explanation> {
    let scorer = self.scorer(context, searcher)?;
    self.base.explain(scorer, doc, self.query.to_string("")?)
  }

  fn get_query(&self) -> Arc<Query> {
    self.query.clone()
  }

  type ScorerSupplier = QueryWeightSs<IRC>;

  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::ScorerSupplier>> {
    let bits = self.random_query.bitset_for_context(context)?;
    let scorer = ConstantScoreScorer::from_disi(
      self.base.score(),
      self.score_mode,
      BitSetIterator::new(bits.clone(), bits.approximate_cardinality() as i64)?,
    );
    Ok(Some(Box::new(DefaultScorerSupplier::new(scorer))))
  }
}

impl crate::core::util::accountable::Accountable for RandomQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(QUERY_DEFAULT_RAM_BYTES_USED)
  }
}

#[derive(Clone)]
pub struct DummyQuery1 {
  id: i32,
  identity: Identity,
}

impl DummyQuery1 {
  pub fn new(id: i32) -> Self {
    Self {
      id,
      identity: Identity::new(),
    }
  }
}

impl PartialEq for DummyQuery1 {
  fn eq(&self, other: &Self) -> bool {
    self.id == other.id
  }
}

impl Eq for DummyQuery1 {}

impl Hash for DummyQuery1 {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.id.hash(state);
  }
}

impl Debug for DummyQuery1 {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "dummy")
  }
}

impl HasIdentity for DummyQuery1 {
  fn identity(&self) -> &Identity {
    &self.identity
  }
}

impl QueryBase for DummyQuery1 {
  fn to_string(&self, _field: &str) -> Result<String> {
    Ok("dummy".to_string())
  }

  fn create_weight<IRC>(
    self,
    _searcher: &IndexSearcher<IRC>,
    score_mode: &ScoreMode,
    boost: f32,
  ) -> Result<QueryWeight<IRC>>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    let base = ConstantScoreWeight::new(boost);
    Ok(Box::new(DummyQueryWeight1::new(*score_mode, base, self)))
  }

  fn rewrite<IRC>(self, _searcher: &IndexSearcher<IRC>) -> Result<Query>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    Ok(self.into())
  }

  fn visit<QV>(&self, _visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    Ok(())
  }
}

struct DummyQueryWeight1 {
  score_mode: ScoreMode,
  base: ConstantScoreWeight,
  query: Arc<Query>,
}

impl DummyQueryWeight1 {
  fn new(score_mode: ScoreMode, base: ConstantScoreWeight, query: DummyQuery1) -> Self {
    let query = Arc::new(query.into());
    Self {
      score_mode,
      base,
      query,
    }
  }
}

impl<IRC> SegmentCacheable<IRC> for DummyQueryWeight1
where
  IRC: IndexReaderContext,
{
  fn is_cacheable(&self, _ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    Ok(true)
  }
}

impl<IRC> Weight<IRC> for DummyQueryWeight1
where
  IRC: IndexReaderContext,
{
  fn explain(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Explanation> {
    let scorer = self.scorer(context, searcher)?;
    self.base.explain(scorer, doc, self.query.to_string("")?)
  }

  fn get_query(&self) -> Arc<Query> {
    self.query.clone()
  }

  type ScorerSupplier = QueryWeightSs<IRC>;

  fn scorer_supplier(
    &self,
    _context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::ScorerSupplier>> {
    let scorer =
      ConstantScoreScorer::from_disi(self.base.score(), self.score_mode, AllDISI::new(1));
    Ok(Some(Box::new(DefaultScorerSupplier::new(scorer))))
  }
}

impl crate::core::util::accountable::Accountable for DummyQuery1 {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(QUERY_DEFAULT_RAM_BYTES_USED)
  }
}

/// A query that doesn't match anything
#[derive(Clone)]
pub enum TestLRUQuery {
  Dummy {
    id: i32,
    identity: Identity,
  },
  AccountableDummy {
    id: i32,
    identity: Identity,
  },
  NoCache {
    identity: Identity,
  },
  Dummy2 {
    scorer_created: Arc<AtomicBool>,
    identity: Identity,
  },
}

static DUMMY_QUERY_COUNTER: AtomicI32 = AtomicI32::new(0);

impl TestLRUQuery {
  pub fn dummy() -> Self {
    Self::Dummy {
      id: DUMMY_QUERY_COUNTER.fetch_add(1, Ordering::Relaxed),
      identity: Identity::new(),
    }
  }

  pub(crate) fn accountable_dummy() -> Self {
    Self::AccountableDummy {
      id: DUMMY_QUERY_COUNTER.fetch_add(1, Ordering::Relaxed),
      identity: Identity::new(),
    }
  }

  pub(crate) fn no_cache() -> Self {
    Self::NoCache {
      identity: Identity::new(),
    }
  }

  pub(crate) fn dummy2(scorer_created: Arc<AtomicBool>) -> Self {
    Self::Dummy2 {
      scorer_created,
      identity: Identity::new(),
    }
  }
}

impl PartialEq for TestLRUQuery {
  fn eq(&self, other: &Self) -> bool {
    match (self, other) {
      (Self::Dummy { id, .. }, Self::Dummy { id: other_id, .. }) => id == other_id,
      (Self::AccountableDummy { id, .. }, Self::AccountableDummy { id: other_id, .. }) => {
        id == other_id
      },
      (Self::NoCache { .. }, Self::NoCache { .. }) => true,
      (Self::Dummy2 { .. }, Self::Dummy2 { .. }) => true,
      _ => false,
    }
  }
}

impl Eq for TestLRUQuery {}

impl Hash for TestLRUQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    std::mem::discriminant(self).hash(state);
    match self {
      Self::Dummy { id, .. } | Self::AccountableDummy { id, .. } => id.hash(state),
      Self::NoCache { .. } | Self::Dummy2 { .. } => {},
    }
  }
}

impl Debug for TestLRUQuery {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Dummy { .. } => write!(f, "DummyQuery"),
      Self::AccountableDummy { .. } => write!(f, "AccountableDummyQuery"),
      Self::NoCache { .. } => write!(f, "NoCacheQuery"),
      Self::Dummy2 { .. } => write!(f, "DummyQuery2"),
    }
  }
}

impl HasIdentity for TestLRUQuery {
  fn identity(&self) -> &Identity {
    match self {
      Self::Dummy { identity, .. }
      | Self::AccountableDummy { identity, .. }
      | Self::NoCache { identity }
      | Self::Dummy2 { identity, .. } => identity,
    }
  }
}

impl QueryBase for TestLRUQuery {
  fn to_string(&self, _field: &str) -> Result<String> {
    Ok(format!("{:?}", self))
  }

  fn create_weight<IRC>(
    self,
    _searcher: &IndexSearcher<IRC>,
    score_mode: &ScoreMode,
    boost: f32,
  ) -> Result<QueryWeight<IRC>>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    let query = Arc::new(self.clone().into());
    let cacheable = !matches!(&self, Self::NoCache { .. });
    let kind = match &self {
      Self::Dummy { .. } | Self::AccountableDummy { .. } => TestLRUWeightKind::NoScorer,
      Self::NoCache { .. } => TestLRUWeightKind::NoScorer,
      Self::Dummy2 { scorer_created, .. } => TestLRUWeightKind::AllDocs {
        max_doc: 1,
        scorer_created: Some(scorer_created.clone()),
      },
    };
    Ok(Box::new(TestLRUWeight {
      query,
      base: ConstantScoreWeight::new(boost),
      score_mode: *score_mode,
      cacheable,
      kind,
    }))
  }

  fn rewrite<IRC>(self, _searcher: &IndexSearcher<IRC>) -> Result<Query>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    Ok(self.into())
  }

  fn visit<QV>(&self, _visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    Ok(())
  }
}

impl crate::core::util::accountable::Accountable for TestLRUQuery {
  fn ram_bytes_used(&self) -> Result<i64> {
    let bytes = match self {
      Self::AccountableDummy { .. } => 10 * QUERY_DEFAULT_RAM_BYTES_USED,
      _ => QUERY_DEFAULT_RAM_BYTES_USED,
    };
    Ok(bytes)
  }
}

enum TestLRUWeightKind {
  NoScorer,
  AllDocs {
    max_doc: i32,
    scorer_created: Option<Arc<AtomicBool>>,
  },
}

struct TestLRUWeight {
  query: Arc<Query>,
  base: ConstantScoreWeight,
  score_mode: ScoreMode,
  cacheable: bool,
  kind: TestLRUWeightKind,
}

impl<IRC> SegmentCacheable<IRC> for TestLRUWeight
where
  IRC: IndexReaderContext,
{
  fn is_cacheable(&self, _ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    Ok(self.cacheable)
  }
}

impl<IRC> Weight<IRC> for TestLRUWeight
where
  IRC: IndexReaderContext,
{
  fn explain(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Explanation> {
    let scorer = self.scorer(context, searcher)?;
    self.base.explain(scorer, doc, self.query.to_string("")?)
  }

  fn get_query(&self) -> Arc<Query> {
    self.query.clone()
  }

  type ScorerSupplier = QueryWeightSs<IRC>;

  fn scorer_supplier(
    &self,
    _context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::ScorerSupplier>> {
    match &self.kind {
      TestLRUWeightKind::NoScorer => Ok(None),
      TestLRUWeightKind::AllDocs {
        max_doc,
        scorer_created,
      } => Ok(Some(Box::new(TestLRUScorerSupplier {
        score: self.base.score(),
        score_mode: self.score_mode,
        max_doc: *max_doc,
        scorer_created: scorer_created.clone(),
      }))),
    }
  }
}

struct TestLRUScorerSupplier {
  score: f32,
  score_mode: ScoreMode,
  max_doc: i32,
  scorer_created: Option<Arc<AtomicBool>>,
}

impl<IRC> ScorerSupplier<IRC> for TestLRUScorerSupplier
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
    if let Some(scorer_created) = &self.scorer_created {
      scorer_created.store(true, Ordering::SeqCst);
    }
    Ok(Box::new(ConstantScoreScorer::from_disi(
      self.score,
      self.score_mode,
      AllDISI::new(self.max_doc),
    )))
  }

  fn bulk_scorer(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::BulkScorer>> {
    let scorer = self.get(i64::MAX, context, searcher)?;
    Ok(Some(Box::new(DefaultBulkScorer::new(scorer))))
  }

  fn cost(
    &mut self,
    _context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC>,
  ) -> Result<i64> {
    Ok(i64::from(self.max_doc))
  }
}

#[derive(Clone)]
pub struct DVCacheQuery {
  field: String,
  scorer_created_count: Arc<AtomicI32>,
  identity: Identity,
}

impl DVCacheQuery {
  pub(crate) fn new(field: &str) -> Self {
    Self {
      field: field.to_string(),
      scorer_created_count: Arc::new(AtomicI32::new(0)),
      identity: Identity::new(),
    }
  }

  pub(crate) fn scorer_created_count(&self) -> i32 {
    self.scorer_created_count.load(Ordering::SeqCst)
  }
}

impl Debug for DVCacheQuery {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "DVCacheQuery")
  }
}

impl PartialEq for DVCacheQuery {
  fn eq(&self, _other: &Self) -> bool {
    true
  }
}

impl Eq for DVCacheQuery {}

impl Hash for DVCacheQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    0usize.hash(state);
  }
}

impl HasIdentity for DVCacheQuery {
  fn identity(&self) -> &Identity {
    &self.identity
  }
}

impl QueryBase for DVCacheQuery {
  fn to_string(&self, _field: &str) -> Result<String> {
    Ok("DVCacheQuery".to_string())
  }

  fn create_weight<IRC>(
    self,
    _searcher: &IndexSearcher<IRC>,
    score_mode: &ScoreMode,
    _boost: f32,
  ) -> Result<QueryWeight<IRC>>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    Ok(Box::new(DVCacheWeight {
      query: Arc::new(self.clone().into()),
      field: self.field,
      scorer_created_count: self.scorer_created_count,
      score_mode: *score_mode,
    }))
  }

  fn rewrite<IRC>(self, _searcher: &IndexSearcher<IRC>) -> Result<Query>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    Ok(self.into())
  }

  fn visit<QV>(&self, _visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    Ok(())
  }
}

impl crate::core::util::accountable::Accountable for DVCacheQuery {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(QUERY_DEFAULT_RAM_BYTES_USED)
  }
}

struct DVCacheWeight {
  query: Arc<Query>,
  field: String,
  scorer_created_count: Arc<AtomicI32>,
  score_mode: ScoreMode,
}

impl<IRC> SegmentCacheable<IRC> for DVCacheWeight
where
  IRC: IndexReaderContext,
{
  fn is_cacheable(&self, ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    DocValues::is_cacheable(ctx, std::slice::from_ref(&self.field))
  }
}

impl<IRC> Weight<IRC> for DVCacheWeight
where
  IRC: IndexReaderContext,
{
  fn explain(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Explanation> {
    let scorer = self.scorer(context, searcher)?;
    ConstantScoreWeight::new(1.0).explain(scorer, doc, self.query.to_string("")?)
  }

  fn get_query(&self) -> Arc<Query> {
    self.query.clone()
  }

  type ScorerSupplier = QueryWeightSs<IRC>;

  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::ScorerSupplier>> {
    Ok(Some(Box::new(DVCacheScorerSupplier {
      max_doc: context.reader().max_doc()?,
      scorer_created_count: self.scorer_created_count.clone(),
      score_mode: self.score_mode,
    })))
  }
}

struct DVCacheScorerSupplier {
  max_doc: i32,
  scorer_created_count: Arc<AtomicI32>,
  score_mode: ScoreMode,
}

impl<IRC> ScorerSupplier<IRC> for DVCacheScorerSupplier
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
    self.scorer_created_count.fetch_add(1, Ordering::SeqCst);
    Ok(Box::new(ConstantScoreScorer::from_disi(
      1.0,
      self.score_mode,
      AllDISI::new(self.max_doc),
    )))
  }

  fn bulk_scorer(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::BulkScorer>> {
    let scorer = self.get(i64::MAX, context, searcher)?;
    Ok(Some(Box::new(DefaultBulkScorer::new(scorer))))
  }

  fn cost(
    &mut self,
    _context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC>,
  ) -> Result<i64> {
    Ok(i64::from(self.max_doc))
  }
}

struct MaxScoreWrapperScorer<S> {
  max_range: i32,
  max_score: f32,
  last_shallow_target: i32,
  scorer: S,
}

impl<S> MaxScoreWrapperScorer<S>
where
  S: Scorer,
{
  fn new(scorer: S, max_range: i32, max_score: f32) -> Self {
    Self {
      max_range,
      max_score,
      last_shallow_target: -1,
      scorer,
    }
  }
}

impl<S> Scorable for MaxScoreWrapperScorer<S>
where
  S: Scorer,
{
  fn score(&mut self) -> Result<f32> {
    self.scorer.score()
  }

  fn cost(&self) -> Result<i64> {
    self.iterator().cost()
  }
}

impl<S> crate::core::search::scorable::FixedScore for MaxScoreWrapperScorer<S> where S: Scorer {}

impl<S> Scorer for MaxScoreWrapperScorer<S>
where
  S: Scorer,
{
  fn doc_id(&mut self) -> Result<i32> {
    self.scorer.doc_id()
  }

  fn iterator(&self) -> Box<dyn DocIdSetIterator + '_> {
    self.scorer.iterator()
  }

  fn iterator_mut(&mut self) -> Box<dyn DocIdSetIterator + '_> {
    self.scorer.iterator_mut()
  }

  fn take_iterator(self: Box<Self>) -> Box<dyn DocIdSetIterator> {
    unreachable!()
  }

  fn two_phase_iterator(&self) -> Option<Box<dyn TwoPhaseIterator + '_>> {
    self.scorer.two_phase_iterator()
  }

  fn two_phase_iterator_mut(&mut self) -> Option<Box<dyn TwoPhaseIterator + '_>> {
    self.scorer.two_phase_iterator_mut()
  }

  fn take_two_phase_iterator(self: Box<Self>) -> Option<Box<dyn TwoPhaseIterator>>
  where
    Self: Sized,
  {
    unreachable!()
  }

  fn advance_shallow(&mut self, target: i32) -> Result<i32> {
    self.last_shallow_target = target;
    self.scorer.advance_shallow(target)
  }

  fn get_max_score(&mut self, upto: i32) -> Result<f32> {
    let v = self.doc_id()?.max(self.last_shallow_target);
    if upto - v >= self.max_range {
      return Ok(self.max_score);
    }
    self.scorer.get_max_score(upto)
  }

  fn has_two_phase_iterator(&self) -> TwoPhaseState {
    self.scorer.has_two_phase_iterator()
  }

  fn approximation(&self) -> Box<dyn DocIdSetIterator + '_> {
    self.scorer.approximation()
  }

  fn approximation_mut(&mut self) -> Box<dyn DocIdSetIterator + '_> {
    self.scorer.approximation_mut()
  }
}

#[derive(Clone, Debug)]
pub struct MaxScoreWrapperQuery {
  query: Box<Query>,
  max_range: i32,
  max_score: f32,
  id: Identity,
}

impl MaxScoreWrapperQuery {
  pub fn new<T>(query: T, max_range: i32, max_score: f32) -> Self
  where
    T: Into<Box<Query>>,
  {
    let query = query.into();
    Self {
      query,
      max_range,
      max_score,
      id: Identity::new(),
    }
  }
}

impl HasIdentity for MaxScoreWrapperQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl Hash for MaxScoreWrapperQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.query.hash(state);
    self.max_range.hash(state);
    self.max_score.to_bits().hash(state);
  }
}

impl Eq for MaxScoreWrapperQuery {}

impl PartialEq for MaxScoreWrapperQuery {
  fn eq(&self, other: &Self) -> bool {
    self.query == other.query
      && self.max_range == other.max_range
      && self.max_score.total_cmp(&other.max_score).to_int() == 0
  }
}

impl QueryBase for MaxScoreWrapperQuery {
  fn to_string(&self, _field: &str) -> Result<String> {
    Ok("MaxScoreWrapperQuery".to_string())
  }

  fn create_weight<IRC>(
    self,
    searcher: &IndexSearcher<IRC>,
    score_mode: &ScoreMode,
    boost: f32,
  ) -> Result<QueryWeight<IRC>>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    let weight = self.query.create_weight(searcher, score_mode, boost)?;
    Ok(Box::new(MaxScoreWrapperQueryWeight::new(
      self.max_range,
      self.max_score,
      weight,
    )))
  }

  fn rewrite<IRC>(self, searcher: &IndexSearcher<IRC>) -> Result<Query>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    let rewritten = self.query.rewrite(searcher)?;
    Ok(MaxScoreWrapperQuery::new(rewritten, self.max_range, self.max_score).into())
  }

  fn visit<QV>(&self, _visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    Ok(())
  }
}

struct MaxScoreWrapperQueryWeight<IRC> {
  max_range: i32,
  max_score: f32,
  weight: QueryWeight<IRC>,
}

impl<IRC> MaxScoreWrapperQueryWeight<IRC>
where
  IRC: IndexReaderContext,
{
  fn new(max_range: i32, max_score: f32, weight: QueryWeight<IRC>) -> Self {
    Self {
      max_range,
      max_score,
      weight,
    }
  }
}

impl<IRC> SegmentCacheable<IRC> for MaxScoreWrapperQueryWeight<IRC>
where
  IRC: IndexReaderContext,
{
  fn is_cacheable(&self, ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    self.weight.is_cacheable(ctx)
  }
}

impl<IRC> Weight<IRC> for MaxScoreWrapperQueryWeight<IRC>
where
  IRC: IndexReaderContext,
{
  fn matches<'a>(
    &'a self,
    context: &'a LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &'a IndexSearcher<IRC>,
  ) -> Result<Option<crate::core::search::query::QueryWeightMatches<'a>>> {
    self.weight.matches(context, doc, searcher)
  }

  fn explain(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Explanation> {
    self.weight.explain(context, doc, searcher)
  }

  fn get_query(&self) -> Arc<Query> {
    self.weight.get_query()
  }

  type ScorerSupplier = QueryWeightSs<IRC>;

  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::ScorerSupplier>> {
    match self.weight.scorer_supplier(context, searcher)? {
      Some(s) => Ok(Some(Box::new(ScorerSupplierImpl::new(
        s,
        self.max_range,
        self.max_score,
      )))),
      None => Ok(None),
    }
  }
}

struct ScorerSupplierImpl<IRC> {
  supplier: QueryWeightSs<IRC>,
  max_range: i32,
  max_score: f32,
}

impl<IRC> ScorerSupplierImpl<IRC>
where
  IRC: IndexReaderContext,
{
  fn new(supplier: QueryWeightSs<IRC>, max_range: i32, max_score: f32) -> Self {
    Self {
      supplier,
      max_range,
      max_score,
    }
  }
}

impl<IRC> ScorerSupplier<IRC> for ScorerSupplierImpl<IRC>
where
  IRC: IndexReaderContext,
  IRCLeafReader<IRC>: LeafReader,
{
  type Scorer = QueryWeightSsScorer;
  type BulkScorer = QueryWeightSsBulkScorer;

  fn get(
    &mut self,
    lead_cost: i64,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Self::Scorer> {
    let v = self.supplier.get(lead_cost, context, searcher)?;
    let s = MaxScoreWrapperScorer::new(v, self.max_range, self.max_score);
    Ok(Box::new(s))
  }

  fn bulk_scorer(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::BulkScorer>> {
    Ok(Some(Box::new(self.default_bulk_scorer(context, searcher)?)))
  }

  fn cost(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<i64> {
    self.supplier.cost(context, searcher)
  }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WANDScorerQuery {
  query: BooleanQuery,
  do_blocks: bool,
  id: Identity,
}

impl WANDScorerQuery {
  pub fn new(query: BooleanQuery, do_blocks: bool) -> Self {
    let id = Identity::new();
    assert_eq!(
      query.clauses().len(),
      query.get_clauses_idx(Occur::Should).len()
    );
    Self {
      query,
      do_blocks,
      id,
    }
  }
}

impl HasIdentity for WANDScorerQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl QueryBase for WANDScorerQuery {
  fn to_string(&self, _field: &str) -> Result<String> {
    Ok("WANDScorerQuery".to_string())
  }

  fn create_weight<IRC>(
    self,
    _searcher: &IndexSearcher<IRC>,
    score_mode: &ScoreMode,
    boost: f32,
  ) -> Result<QueryWeight<IRC>>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    Ok(Box::new(WANDScorerQueryWeight::new(
      self.query,
      self.do_blocks,
      *score_mode,
      boost,
    )))
  }

  fn rewrite<IRC>(self, _searcher: &IndexSearcher<IRC>) -> Result<Query>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    Ok(self.into())
  }

  fn visit<QV>(&self, _visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    Ok(())
  }
}

struct WANDScorerQueryWeight {
  minimum_number_should_match: i32,
  query: Arc<Query>,
  do_blocks: bool,
  score_mode: ScoreMode,
  boost: f32,
}

impl WANDScorerQueryWeight {
  fn new(query: BooleanQuery, do_blocks: bool, score_mode: ScoreMode, boost: f32) -> Self {
    let minimum_number_should_match = query.get_minimum_number_should_match();
    let query = Arc::new(query.into());
    Self {
      minimum_number_should_match,
      query,
      do_blocks,
      score_mode,
      boost,
    }
  }
}

impl<IRC> SegmentCacheable<IRC> for WANDScorerQueryWeight
where
  IRC: IndexReaderContext,
{
  fn is_cacheable(&self, _ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    Ok(false)
  }
}

impl<IRC> Weight<IRC> for WANDScorerQueryWeight
where
  IRC: IndexReaderContext,
{
  fn matches<'a>(
    &'a self,
    _context: &'a LeafReaderContext<IRCLeafReader<IRC>>,
    _doc: i32,
    _searcher: &'a IndexSearcher<IRC>,
  ) -> Result<Option<crate::core::search::query::QueryWeightMatches<'a>>> {
    unreachable!("")
  }

  fn explain(
    &self,
    _context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _doc: i32,
    _searcher: &IndexSearcher<IRC>,
  ) -> Result<Explanation> {
    unreachable!("")
  }

  fn get_query(&self) -> Arc<Query> {
    self.query.clone()
  }

  type ScorerSupplier = QueryWeightSs<IRC>;

  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::ScorerSupplier>> {
    let weight = match self.query.as_ref() {
      Query::Boolean(query) => query
        .clone()
        .raw_weight(searcher, &self.score_mode, self.boost)?,
      _ => unreachable!("WANDScorerQueryWeight must wrap a BooleanQuery"),
    };
    let mut optional_scorers = Vec::new();
    for wc in weight.weighted_clauses.iter() {
      let w = &wc.weight;
      let ss = w.scorer_supplier(context, searcher)?;
      if let Some(mut ss) = ss {
        let scorer = ss.get(i64::MAX, context, searcher)?;
        optional_scorers.push(scorer);
      }
    }

    let scorer = if !optional_scorers.is_empty() {
      ScorerEnum2::A(WANDScorer::new(
        optional_scorers,
        self.minimum_number_should_match,
        self.score_mode,
        if self.do_blocks { i64::MAX } else { 0 },
      )?)
    } else {
      match weight.scorer(context, searcher)? {
        Some(ss) => ScorerEnum2::B(ss),
        None => return Ok(None),
      }
    };
    let v = DefaultScorerSupplier::new(scorer);
    Ok(Some(Box::new(v)))
  }
}

impl crate::core::util::accountable::Accountable for MaxScoreWrapperQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(QUERY_DEFAULT_RAM_BYTES_USED)
  }
}

impl crate::core::util::accountable::Accountable for WANDScorerQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(QUERY_DEFAULT_RAM_BYTES_USED)
  }
}
