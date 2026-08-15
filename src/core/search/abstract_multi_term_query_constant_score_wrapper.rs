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
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::term::Term;
use crate::core::index::term_states::{TermStateEnum, TermStates};
use crate::core::index::terms::{Terms, TermsPosting};
use crate::core::index::terms_enum::TermsEnum;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::boolean_query::Builder;
use crate::core::search::bulk_scorer::BulkScorerEnum4;
use crate::core::search::constant_score_query::ConstantScoreQuery;
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::constant_score_weight::ConstantScoreWeight;
use crate::core::search::disjunction_matches_iterator::from_terms_enum;
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, EmptyDISI};
use crate::core::search::dummy::dummy_disi::DummyDISI;
use crate::core::search::dummy::dummy_two_phase_iterator::DummyTwoPhaseIterator;
use crate::core::search::explanation::Explanation;
use crate::core::search::index_searcher::{IndexSearcher, get_max_clause_count};
use crate::core::search::matches_utils::for_field;
use crate::core::search::multi_term_query::MultiTermQuery;
use crate::core::search::multi_term_query_constant_score_blended_wrapper::BlendedRewritingWeight;
use crate::core::search::multi_term_query_constant_score_wrapper::StandardRewritingWeight;
use crate::core::search::query::{
  Query, QueryBase, QueryWeight, QueryWeightSs, QueryWeightSsBulkScorer, QueryWeightSsScorer,
};
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::ScorerEnum4;
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::term_query::TermQuery;
use crate::core::search::weight::{DefaultBulkScorer, Weight};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::impl_from_for_enum;
use std::rc::Rc;
use std::sync::Arc;

pub(crate) const BOOLEAN_REWRITE_TERM_COUNT_THRESHOLD: usize = 16;
/// Contains functionality shared by both
/// [`MultiTermQueryConstantScoreBlendedWrapper`](crate::core::search::multi_term_query_constant_score_blended_wrapper::MultiTermQueryConstantScoreBlendedWrapper) and
/// [`MultiTermQueryConstantScoreWrapper`](crate::core::search::multi_term_query_constant_score_wrapper::MultiTermQueryConstantScoreWrapper).
///
/// This is an internal implementation detail and is not intended to be used
/// or extended by users.
#[allow(dead_code)]
pub struct AbstractMultiTermQueryConstantScoreWrapper {}

pub struct RewritingWeight<Q> {
  score_mode: ScoreMode,
  q: Q,
  base: ConstantScoreWeight,
  sub: RewritingWeightBaseEnum,
  query: Arc<Query>,
}
impl<Q> RewritingWeight<Q>
where
  Q: MultiTermQuery,
{
  pub(crate) fn new(score: f32, score_mode: ScoreMode, q: Q, sub: RewritingWeightBaseEnum) -> Self {
    let query = Arc::new(q.to_query());
    let base = ConstantScoreWeight::new(score);
    Self {
      score_mode,
      q,
      base,
      sub,
      query,
    }
  }
  fn collect_terms<TE>(
    field_doc_count: i32,
    terms_enum: &mut TE,
    terms: &mut Vec<TermAndState>,
  ) -> Result<bool>
  where
    TE: TermsEnum,
  {
    let threshold = std::cmp::min(BOOLEAN_REWRITE_TERM_COUNT_THRESHOLD, get_max_clause_count());

    for _ in 0..threshold {
      let term = match terms_enum.next()? {
        Some(t) => t.into_owned(),
        None => return Ok(true),
      };

      let state = terms_enum.term_state()?;
      let doc_freq = terms_enum.doc_freq()?;
      let total_term_freq = terms_enum.total_term_freq()?;

      let term_and_state = TermAndState::new(term, state, doc_freq, total_term_freq);

      if field_doc_count == doc_freq {
        // If the term contains every document with a value for the field, we can ignore all
        // other terms:
        terms.clear();
        terms.push(term_and_state);
        return Ok(true);
      }

      terms.push(term_and_state);
    }

    Ok(terms_enum.next()?.is_none())
  }
}

impl<IRC, Q> SegmentCacheable<IRC> for RewritingWeight<Q>
where
  IRC: IndexReaderContext,
  Q: MultiTermQuery,
{
  fn is_cacheable(&self, _ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    Ok(true)
  }
}

impl<IRC, Q> Weight<IRC> for RewritingWeight<Q>
where
  IRC: IndexReaderContext,
  Q: MultiTermQuery,
  <Q as MultiTermQuery>::TermsEnum<<<IRC as IndexReaderContext>::LeafReader as LeafReader>::Terms>:
    'static,
  <Q as MultiTermQuery>::TermsEnum<
    Rc<<<IRC as IndexReaderContext>::LeafReader as LeafReader>::Terms>,
  >: 'static,
{
  fn matches<'a>(
    &'a self,
    context: &'a LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    _searcher: &'a IndexSearcher<IRC>,
  ) -> Result<Option<crate::core::search::query::QueryWeightMatches<'a>>> {
    let field = self.q.get_field().to_string();
    let Some(terms) = context.reader().terms(&field)? else {
      return Ok(None);
    };
    let terms = Rc::new(terms);
    for_field(field.clone(), move || {
      let terms_enum = self.q.get_terms_enum(terms.clone())?;
      from_terms_enum(context, doc, self.query.clone(), &field, terms_enum)
    })
  }

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
    let terms = match context.reader().terms(self.q.get_field())? {
      Some(t) => t,
      None => return Ok(None),
    };
    let terms = Rc::new(terms);
    let field_doc_count = terms.get_doc_count()?;
    let mut terms_enum = self.q.get_terms_enum(terms.clone())?;
    let mut collected_terms = Vec::new();

    let collect_result =
      Self::collect_terms(field_doc_count, &mut terms_enum, &mut collected_terms)?;

    let cost = if collect_result {
      if collected_terms.is_empty() {
        return Ok(None);
      }

      let mut sum_term_cost: i64 = 0;
      for collected_term in &collected_terms {
        sum_term_cost += collected_term.doc_freq as i64;
      }
      sum_term_cost
    } else {
      estimate_cost(&terms, self.q.get_terms_count())?
    };
    let v = ScorerSupplierImpl::new(
      cost,
      self.score_mode,
      terms,
      collected_terms,
      terms_enum,
      self.base.score(),
      collect_result,
      self.q.get_field().to_string(),
      self.sub.clone(),
    );
    Ok(Some(Box::new(v)))
  }
}
fn rewrite_as_boolean_query<IRC>(
  context: &LeafReaderContext<IRCLeafReader<IRC>>,
  collected_terms: &[TermAndState],
  searcher: &IndexSearcher<IRC>,
  score_mode: &ScoreMode,
  score: f32,
  field: &str,
) -> Result<WeightOrDocIdSetIterator<IRC, DummyDISI>>
where
  IRC: IndexReaderContext,
{
  let mut builder = Builder::new();

  for t in collected_terms.iter() {
    let mut term_states = TermStates::new(searcher.get_top_reader_context())?;
    let term = Term::new(field, t.term.clone());
    term_states.register_with_stats(t.state.clone(), context.ord, t.doc_freq, t.total_term_freq);
    let tq = TermQuery::with_term_state(term, Some(term_states));
    builder.add(tq, Occur::Should)?;
  }

  let bq = builder.build();
  let query = ConstantScoreQuery::new(bq);

  let rewritten = searcher.rewrite(query)?;
  let weight = rewritten.create_weight(searcher, score_mode, score)?;

  Ok(WeightOrDocIdSetIterator::from_weight(weight))
}
/// Estimate the cost. If the MTQ can provide its term count, we can do a better job
/// estimating.
/// Cost estimation reasoning is:
/// 1. If we don't know how many query terms there are, we assume that every term could be
///    in the MTQ and estimate the work as the total docs across all terms.
/// 2. If we know how many query terms there are...
///    2a. Assume every query term matches at least one document (queryTermsCount).
///    2b. Determine the total number of docs beyond the first one for each term.
///    That count provides a ceiling on the number of extra docs that could match beyond
///    that first one. (We omit the first since it's already been counted in 2a).
///    See: LUCENE-10207
pub(crate) fn estimate_cost<T>(terms: &T, query_terms_count: i64) -> Result<i64>
where
  T: Terms,
{
  let cost = if query_terms_count == -1 {
    terms.get_sum_doc_freq()?
  } else {
    let mut potential_extra_cost = terms.get_sum_doc_freq()?;
    let indexed_term_count = terms.size()?;
    if indexed_term_count != -1 {
      potential_extra_cost -= indexed_term_count;
    }
    query_terms_count + potential_extra_cost
  };
  Ok(cost)
}
pub trait RewritingWeightBase {
  type Iter<T>: DocIdSetIterator
  where
    T: Terms,
    TermsPosting<T>: 'static;
  /// Rewrite the query as either a [`Weight`] or a [`DocIdSetIterator`], wrapped in a
  /// [`WeightOrDocIdSetIterator`].
  ///
  /// Before this is called, the weight attempts to collect found terms up to a
  /// threshold. If fewer terms than the threshold are found, the query is rewritten
  /// into a [`BooleanQuery`](crate::core::search::boolean_query::BooleanQuery) and this method is not called. This is only called if it
  /// is determined that there are more found terms.
  ///
  /// When this method is invoked, `terms_enum` is positioned on the next
  /// "uncollected" term. Terms that were already collected are provided in
  /// `collected_terms`.
  #[allow(clippy::too_many_arguments)]
  fn rewrite_inner<T, TE, IRC>(
    &self,
    field_doc_count: i32,
    terms: &mut T,
    terms_enum: &mut TE,
    collected_terms: &[TermAndState],
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
    field: &str,
    score_mode: &ScoreMode,
    score: f32,
  ) -> Result<WeightOrDocIdSetIterator<IRC, Self::Iter<T>>>
  where
    T: Terms,
    TE: TermsEnum<PostingsEnum = <T::TermsEnum as TermsEnum>::PostingsEnum>,
    IRC: IndexReaderContext;
}

#[derive(Clone)]
pub(crate) enum RewritingWeightBaseEnum {
  Blended(BlendedRewritingWeight),
  Standard(StandardRewritingWeight),
}
impl_from_for_enum!(
    RewritingWeightBaseEnum,
    BlendedRewritingWeight => Blended,
    StandardRewritingWeight => Standard,
);
pub struct ScorerSupplierImpl<T, TE> {
  cost: i64,
  score_mode: ScoreMode,
  terms: T,
  collected_terms: Vec<TermAndState>,
  terms_enum: TE,
  score: f32,
  collect_result: bool,
  field: String,
  sub: RewritingWeightBaseEnum,
}
impl<T, TE> ScorerSupplierImpl<T, TE> {
  #[allow(clippy::too_many_arguments)]
  fn new(
    cost: i64,
    score_mode: ScoreMode,
    terms: T,
    collected_terms: Vec<TermAndState>,
    terms_enum: TE,
    score: f32,
    collect_result: bool,
    field: String,
    sub: RewritingWeightBaseEnum,
  ) -> Self {
    Self {
      cost,
      score_mode,
      terms,
      collected_terms,
      terms_enum,
      score,
      collect_result,
      field,
      sub,
    }
  }
}

impl<IRC, T, TE> ScorerSupplier<IRC> for ScorerSupplierImpl<T, TE>
where
  IRC: IndexReaderContext,
  T: Terms,
  TE: TermsEnum<PostingsEnum = TermsPosting<T>>,
  TermsPosting<T>: 'static,
{
  type Scorer = QueryWeightSsScorer;
  type BulkScorer = QueryWeightSsBulkScorer;

  fn get(
    &mut self,
    _lead_cost: i64,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Self::Scorer> {
    let s = match self.collect_result {
      true => {
        let v = rewrite_as_boolean_query(
          context,
          self.collected_terms.as_slice(),
          searcher,
          &self.score_mode,
          self.score,
          &self.field,
        )?;
        match v.weight {
          Some(weight) => match weight.scorer(context, searcher)? {
            Some(scorer) => ScorerEnum4::B(scorer),
            None => {
              let s = empty_scorer(self.score_mode, self.score);
              ScorerEnum4::A(s)
            },
          },
          None => return Err(LuceneError::illegal_state("weight is None")),
        }
      },
      false => match self.sub {
        RewritingWeightBaseEnum::Blended(ref b) => {
          let v = b.rewrite_inner(
            self.terms.get_doc_count()?,
            &mut self.terms,
            &mut self.terms_enum,
            self.collected_terms.as_slice(),
            context,
            searcher,
            &self.field,
            &self.score_mode,
            self.score,
          )?;
          match (v.weight, v.iterator) {
            (Some(weight), None) => match weight.scorer(context, searcher)? {
              Some(scorer) => ScorerEnum4::B(scorer),
              None => {
                let s = empty_scorer(self.score_mode, self.score);
                ScorerEnum4::A(s)
              },
            },
            (None, Some(iter)) => {
              ScorerEnum4::C(scorer_for_iterator(iter, self.score_mode, self.score))
            },
            _ => return Err(LuceneError::illegal_state("")),
          }
        },
        RewritingWeightBaseEnum::Standard(ref s) => {
          let v = s.rewrite_inner(
            self.terms.get_doc_count()?,
            &mut self.terms,
            &mut self.terms_enum,
            self.collected_terms.as_slice(),
            context,
            searcher,
            &self.field,
            &self.score_mode,
            self.score,
          )?;
          match (v.weight, v.iterator) {
            (Some(weight), None) => match weight.scorer(context, searcher)? {
              Some(scorer) => ScorerEnum4::B(scorer),
              None => {
                let s = empty_scorer(self.score_mode, self.score);
                ScorerEnum4::A(s)
              },
            },
            (None, Some(iter)) => {
              ScorerEnum4::D(scorer_for_iterator(iter, self.score_mode, self.score))
            },
            _ => return Err(LuceneError::illegal_state("")),
          }
        },
      },
    };
    Ok(Box::new(s))
  }

  fn bulk_scorer(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::BulkScorer>>
  where
    <<T as Terms>::TermsEnum as TermsEnum>::PostingsEnum: 'static,
  {
    let bs = match self.collect_result {
      true => {
        let v = rewrite_as_boolean_query(
          context,
          self.collected_terms.as_slice(),
          searcher,
          &self.score_mode,
          self.score,
          &self.field,
        )?;
        match v.weight {
          Some(weight) => match weight.bulk_scorer(context, searcher)? {
            Some(scorer) => BulkScorerEnum4::B(scorer),
            None => {
              let s = empty_scorer(self.score_mode, self.score);
              let v = DefaultBulkScorer::new(s);
              BulkScorerEnum4::A(v)
            },
          },
          None => return Err(LuceneError::illegal_state("weight is None")),
        }
      },
      false => match self.sub {
        RewritingWeightBaseEnum::Blended(ref b) => {
          let v = b.rewrite_inner(
            self.terms.get_doc_count()?,
            &mut self.terms,
            &mut self.terms_enum,
            self.collected_terms.as_slice(),
            context,
            searcher,
            &self.field,
            &self.score_mode,
            self.score,
          )?;
          match (v.weight, v.iterator) {
            (Some(weight), None) => match weight.bulk_scorer(context, searcher)? {
              Some(scorer) => BulkScorerEnum4::B(scorer),
              None => {
                let s = empty_scorer(self.score_mode, self.score);
                let v = DefaultBulkScorer::new(s);
                BulkScorerEnum4::A(v)
              },
            },
            (None, Some(iter)) => {
              let s = ConstantScoreScorer::from_disi(self.score, self.score_mode, iter);
              let v = DefaultBulkScorer::new(s);
              BulkScorerEnum4::C(v)
            },
            _ => return Err(LuceneError::illegal_state("")),
          }
        },
        RewritingWeightBaseEnum::Standard(ref s) => {
          let v = s.rewrite_inner(
            self.terms.get_doc_count()?,
            &mut self.terms,
            &mut self.terms_enum,
            self.collected_terms.as_slice(),
            context,
            searcher,
            &self.field,
            &self.score_mode,
            self.score,
          )?;
          match (v.weight, v.iterator) {
            (Some(weight), None) => match weight.bulk_scorer(context, searcher)? {
              Some(scorer) => BulkScorerEnum4::B(scorer),
              None => {
                let s = empty_scorer(self.score_mode, self.score);
                let v = DefaultBulkScorer::new(s);
                BulkScorerEnum4::A(v)
              },
            },
            (None, Some(iter)) => {
              let s = ConstantScoreScorer::from_disi(self.score, self.score_mode, iter);
              let v = DefaultBulkScorer::new(s);
              BulkScorerEnum4::D(v)
            },
            _ => return Err(LuceneError::illegal_state("")),
          }
        },
      },
    };
    Ok(Some(Box::new(bs)))
  }

  fn cost(
    &mut self,
    _context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC>,
  ) -> Result<i64> {
    Ok(self.cost)
  }
}
fn empty_scorer(
  score_mode: ScoreMode,
  score: f32,
) -> ConstantScoreScorer<EmptyDISI, DummyTwoPhaseIterator> {
  ConstantScoreScorer::from_disi(score, score_mode, EmptyDISI::default())
}
fn scorer_for_iterator<D>(
  iterator: D,
  score_mode: ScoreMode,
  score: f32,
) -> ConstantScoreScorer<D, DummyTwoPhaseIterator>
where
  D: DocIdSetIterator,
{
  ConstantScoreScorer::from_disi(score, score_mode, iterator)
}
pub(crate) struct TermAndState {
  pub(crate) term: BytesRef<Vec<u8>>,
  pub(crate) state: TermStateEnum,
  pub(crate) doc_freq: i32,
  pub(crate) total_term_freq: i64,
}

impl TermAndState {
  pub(crate) fn new(
    term: BytesRef<Vec<u8>>,
    state: TermStateEnum,
    doc_freq: i32,
    total_term_freq: i64,
  ) -> Self {
    Self {
      term,
      state,
      doc_freq,
      total_term_freq,
    }
  }
}
pub(crate) struct WeightOrDocIdSetIterator<IRC, D> {
  pub(crate) weight: Option<QueryWeight<IRC>>,
  pub(crate) iterator: Option<D>,
}

impl<IRC, D> WeightOrDocIdSetIterator<IRC, D>
where
  IRC: IndexReaderContext,
  D: DocIdSetIterator,
{
  pub(crate) fn from_weight(weight: QueryWeight<IRC>) -> Self {
    Self {
      weight: Some(weight),
      iterator: None,
    }
  }

  pub(crate) fn from_iterator(iterator: D) -> Self {
    Self {
      weight: None,
      iterator: Some(iterator),
    }
  }
}
