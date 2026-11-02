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
use crate::core::search::query::QueryRef;

use crate::core::search::approximation_disi::ApproximationBitSetAccess;
use crate::core::search::approximation_disi::ApproximationDISI;
use crate::core::search::constant_score_scorer::ConstantScoreDisiScorer;
use crate::core::search::query::QueryWeightMatches;
use crate::core::search::query::QueryWeightSs;
use crate::core::search::query::QueryWeightSsBulkScorer;
use crate::core::search::two_phase_iterator::TwoPhaseIterator;
use std::fmt::Write as _;

use crate::core::index::BytesRef;
use crate::core::index::bytes_ref::{BytesRefValue, BytesRefValueEnum};
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::impact::Impact;
use crate::core::index::impacts::Impacts;
use crate::core::index::impacts_enum::{ImpactsEnum, ImpactsEnumEnum2};
use crate::core::index::impacts_source::ImpactsSource;
use crate::core::index::index_reader::Identity;
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader::{LRTermsEnum, LeafReader};
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::postings_enum::{FREQS, PostingsEnum, PostingsEnumEnum2};
use crate::core::index::reader_util::ReaderUtil;
use crate::core::index::slow_impacts_enum::SlowImpactsEnum;
use crate::core::index::term::Term;
use crate::core::index::term_states;
use crate::core::index::term_states::{PrepareState, TermStates};
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::boolean_query;
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::disi_priority_queue::DisiPriorityQueue;
use crate::core::search::disi_wrapper::DisiWrapper;
use crate::core::search::disjunction_disi_approximation::DisjunctionDISIApproximation;
use crate::core::search::disjunction_matches_iterator::from_terms_enum;
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, EmptyDISI};
use crate::core::search::explanation::Explanation;
use crate::core::search::impacts_disi::SeparateImpactsDISI;
use crate::core::search::index_searcher;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::matches_utils::for_field;
use crate::core::search::max_score_cache::MaxScoreCache;
use crate::core::search::query::{Query, QueryBase, QueryWeight, QueryWeightSsScorer};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::scorable::{FixedScore, Scorable};
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::{Scorer, ScorerEnum4, TwoPhaseState};
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::scorer_types::{LeafScorerTypes, ScorerTypes};
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::similarities_impl::similarities::{SimScorer, Similarity};
use crate::core::search::term_query::TermQuery;
use crate::core::search::term_scorer::{TermScorer, TermScorerState};
use crate::core::search::term_statistics::TermStatistics;
use crate::core::search::weight::Weight;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::core_helper::{CoreHelper, HasIdentity};
use crate::core::util::error::UncheckedIOError;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::priority_queue::{Compare, PriorityQueue};
use smallvec::SmallVec;
use std::borrow::Cow;
use std::cell::RefCell;
use std::fmt::{Debug, Formatter};
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;

/// A query that treats multiple terms as synonyms.
///
/// For scoring purposes, this query tries to score the terms as if you had indexed them as one
/// term: it will match any of the terms but only invoke the similarity a single time, scoring the
/// sum of all term frequencies for the document.
#[derive(Clone)]
pub struct SynonymQuery {
  id: Identity,
  data: Arc<SynonymQueryData>,
}

#[derive(Clone)]
struct SynonymQueryData {
  terms: Box<[TermAndBoost]>,
  field: String,
}

impl SynonymQuery {
  fn new(terms: Vec<TermAndBoost>, field: String) -> Self {
    Self {
      id: Identity::new(),
      data: Arc::new(SynonymQueryData {
        terms: terms.into_boxed_slice(),
        field,
      }),
    }
  }

  /// Returns the terms of this [`SynonymQuery`].
  pub fn get_terms(&self) -> Vec<Term> {
    self
      .data
      .terms
      .iter()
      .map(|term| Term::new(self.data.field.clone(), term.term.clone()))
      .collect()
  }

  /// Returns the field name of this [`SynonymQuery`].
  pub fn get_field(&self) -> &str {
    &self.data.field
  }

  /// Merge impacts for multiple synonyms.
  pub(crate) fn merge_impacts<IE>(
    impacts_enums: Vec<IE>,
    boosts: Vec<f32>,
  ) -> SynonymImpactsSource<IE>
  where
    IE: ImpactsEnum,
  {
    SynonymImpactsSource::new(impacts_enums, boosts)
  }
}

impl PartialEq for SynonymQuery {
  fn eq(&self, other: &Self) -> bool {
    self.data.field == other.data.field && self.data.terms == other.data.terms
  }
}

impl Eq for SynonymQuery {}

impl Hash for SynonymQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    std::any::TypeId::of::<Self>().hash(state);
    self.data.field.hash(state);
    self.data.terms.hash(state);
  }
}

impl Debug for SynonymQuery {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self.to_string("") {
      Ok(s) => write!(f, "{}", s),
      Err(_) => Err(std::fmt::Error),
    }
  }
}

impl HasIdentity for SynonymQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl QueryBase for SynonymQuery {
  type Weight<IRC, SIM>
    = SynonymQueryWeight<IRC, SIM>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static;

  fn to_string(&self, field: &str) -> Result<String> {
    let mut builder = String::from("Synonym(");
    for (i, term_and_boost) in self.data.terms.iter().enumerate() {
      if i != 0 {
        builder.push(' ');
      }
      builder.push_str(&TermQuery::format_term(
        &self.data.field,
        &term_and_boost.term,
        field,
      )?);
      if term_and_boost.boost != 1.0 {
        builder.push('^');
        write!(builder, "{:.1}", term_and_boost.boost)?;
      }
    }
    builder.push(')');
    Ok(builder)
  }

  fn create_weight<IRC, SIM>(
    self,
    searcher: &IndexSearcher<IRC, SIM>,
    score_mode: &ScoreMode,
    boost: f32,
  ) -> Result<Self::Weight<IRC, SIM>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync,
    Self: Sized,
  {
    if score_mode.needs_scores() {
      SynonymWeight::new(self, searcher, *score_mode, boost).map(SynonymQueryWeight::Scoring)
    } else {
      // If scores are not needed, let BooleanWeight deal with optimizing that case.
      let mut builder = boolean_query::Builder::new();
      let data = Arc::unwrap_or_clone(self.data);
      for term in data.terms {
        builder.add(
          TermQuery::new(Term::new(data.field.clone(), term.term)),
          Occur::Should,
        )?;
      }
      searcher
        .rewrite(builder.build())?
        .create_weight(searcher, &ScoreMode::CompleteNoScores, boost)
        .map(SynonymQueryWeight::Rewritten)
    }
  }

  fn rewrite<IRC, SIM>(&self, _searcher: &IndexSearcher<IRC, SIM>) -> Result<Option<Query>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    Self: Sized,
    IndexSearcher<IRC, SIM>: Sync,
  {
    if self.data.terms.is_empty() {
      return Ok(Some(boolean_query::Builder::new().build().into()));
    }
    if self.data.terms.len() == 1 && self.data.terms[0].boost == 1.0 {
      return Ok(Some(
        TermQuery::new(Term::new(
          self.data.field.clone(),
          self.data.terms[0].term.clone(),
        ))
        .into(),
      ));
    }
    Ok(None)
  }

  fn visit<QV>(&self, visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    if !visitor.accept_field(&self.data.field) {
      return Ok(());
    }
    let query = self.into();
    let mut visitor = visitor.get_sub_visitor(Occur::Should, query);
    visitor.consume_terms(query, &self.get_terms())
  }
}

#[derive(Clone)]
struct TermAndBoost {
  term: BytesRef<Vec<u8>>,
  boost: f32,
}

pub(crate) struct SynonymTermBytesRefIterator<'a> {
  terms: &'a [TermAndBoost],
  index: usize,
}

impl BytesRefIterator for SynonymTermBytesRefIterator<'_> {
  type Value<'a>
    = &'a BytesRef<Vec<u8>>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    let Some(term) = self.terms.get(self.index) else {
      return Ok(None);
    };
    self.index += 1;
    Ok(Some(&term.term))
  }
}

impl PartialEq for TermAndBoost {
  fn eq(&self, other: &Self) -> bool {
    self.term == other.term && CoreHelper::compare_f32(self.boost, other.boost).is_eq()
  }
}

impl Eq for TermAndBoost {}

impl Hash for TermAndBoost {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.term.hash(state);
    (BitUtil::float_to_int_bits(self.boost) as u32).hash(state);
  }
}

pub struct SynonymWeight<
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> where
  SIM: Similarity,
{
  term_states: Arc<Vec<TermStates>>,
  similarity: Arc<SIM>,
  sim_weight: Option<Arc<SynonymSimScorer<SIM::SimScorer>>>,
  score_mode: ScoreMode,
  parent_query: SynonymQuery,
}

impl<SIM> SynonymWeight<SIM>
where
  SIM: Similarity,
{
  fn new<IRC>(
    query: SynonymQuery,
    searcher: &IndexSearcher<IRC, SIM>,
    score_mode: ScoreMode,
    boost: f32,
  ) -> Result<Self>
  where
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync,
  {
    debug_assert!(score_mode.needs_scores());

    let collection_stats = searcher.collection_statistics(&query.data.field)?;
    let mut doc_freq = 0;
    let mut total_term_freq = 0;
    let mut term_states = Vec::with_capacity(query.data.terms.len());

    for term_and_boost in query.data.terms.iter() {
      let term = Term::new(query.data.field.clone(), term_and_boost.term.clone());
      let ts = term_states::build_with_stats(searcher, &term)?;

      let ts_doc_freq = ts.doc_freq()?;
      if ts_doc_freq > 0 {
        let term_stats = searcher.term_statistics(term, ts_doc_freq, ts.total_term_freq()?)?;
        doc_freq = doc_freq.max(term_stats.get_doc_freq());
        total_term_freq += term_stats.get_total_term_freq();
      }
      term_states.push(ts);
    }

    let similarity = searcher.get_similarity();
    let sim_weight = if doc_freq > 0 {
      let collection_stats = collection_stats.as_ref().ok_or_else(|| {
        LuceneError::illegal_state("collection statistics are missing for matching synonym terms")
      })?;
      let pseudo_stats = TermStatistics::new(
        Term::from_text(&query.data.field, "synonym pseudo-term"),
        doc_freq,
        total_term_freq,
      )?;
      Some(Arc::new(SynonymSimScorer(similarity.scorer(
        boost,
        collection_stats,
        &[pseudo_stats],
      )?)))
    } else {
      None
    };

    Ok(Self {
      term_states: Arc::new(term_states),
      similarity,
      sim_weight,
      score_mode,
      parent_query: query,
    })
  }
}

impl<IRC, SIM> SegmentCacheable<IRC> for SynonymWeight<SIM>
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
{
  fn is_cacheable(&self, _ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    Ok(true)
  }
}

impl<IRC, SIM> Weight<IRC, SIM> for SynonymWeight<SIM>
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
{
  fn matches<'a>(
    &'a self,
    context: &'a LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    _searcher: &'a IndexSearcher<IRC, SIM>,
  ) -> Result<Option<crate::core::search::query::QueryWeightMatches<'a, IRC, SIM>>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let query = &self.parent_query;
    if context.reader().terms(query.get_field())?.is_none() {
      return Ok(None);
    }
    for_field(&query.data.field, (self, context, doc).into())
  }

  fn explain(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Explanation>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let parent_query = &self.parent_query;

    let mut prepare_states = Vec::with_capacity(parent_query.data.terms.len());
    for term_states in self.term_states.iter() {
      prepare_states.push(term_states.get(context)?);
    }
    let mut supplier = SynonymScorerSupplier::<_, SIM>::new(
      -1,
      self.term_states.clone(),
      prepare_states,
      self.parent_query.data.clone(),
      self.sim_weight.clone(),
      self.score_mode,
    );
    let mut scorer = supplier.get_scorer(context)?;
    if scorer.advance(doc)? != doc {
      return Ok(Explanation::no_match_no_details("no matching term"));
    }

    let freq = match &mut scorer {
      SynonymScorerEnum::A(scorer) => scorer.freq()?,
      SynonymScorerEnum::B(scorer) => scorer.freq()?,
      SynonymScorerEnum::C(scorer) => scorer.state.freq()? as f32,
      SynonymScorerEnum::D(_) => {
        return Ok(Explanation::no_match_no_details("no matching term"));
      },
    };
    let freq_explanation = Explanation::match_no_details(freq, format!("termFreq={freq}"));

    let mut norm = 1;
    if let Some(mut norms) = context.reader().get_norm_values(&parent_query.data.field)?
      && norms.advance_exact(doc)?
    {
      norm = norms.long_value()?;
    }
    let sim_weight = self.sim_weight.as_ref().ok_or_else(|| {
      LuceneError::illegal_state("simWeight is missing for matching synonym terms")
    })?;
    let score_explanation = sim_weight.explain(freq_explanation, norm)?;
    Ok(Explanation::match_(
      score_explanation.value.clone(),
      format!(
        "weight({:?} in {}) [{}], result of:",
        QueryRef::Synonym(&self.parent_query),
        doc,
        self.similarity,
      ),
      vec![score_explanation],
    ))
  }

  fn get_query(&self) -> QueryRef<'_> {
    QueryRef::Synonym(&self.parent_query)
  }

  type ScorerSupplier = SynonymScorerSupplier<IRCLeafReader<IRC>, SIM>;

  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Option<Self::ScorerSupplier>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    debug_assert!(
      {
        let v = ReaderUtil::get_top_level_context(context);
        self
          .term_states
          .iter()
          .all(|term_states| term_states.was_built_for(v))
      },
      "The top-reader used to create Weight is not the same as the current reader's top-reader"
    );

    let parent_query = &self.parent_query;

    let mut prepare_states = Vec::with_capacity(parent_query.data.terms.len());
    for term_states in self.term_states.iter() {
      prepare_states.push(term_states.get(context)?);
    }

    Ok(Some(SynonymScorerSupplier::new(
      -1,
      self.term_states.clone(),
      prepare_states,
      self.parent_query.data.clone(),
      self.sim_weight.clone(),
      self.score_mode,
    )))
  }

  fn count(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<i32>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    <Self as Weight<IRC, SIM>>::default_count(self, context)
  }
}

#[doc(hidden)]
pub struct SharedPostingsEnum<P> {
  inner: Rc<RefCell<P>>,
}

impl<P> Clone for SharedPostingsEnum<P> {
  fn clone(&self) -> Self {
    Self {
      inner: self.inner.clone(),
    }
  }
}

impl<P> SharedPostingsEnum<P> {
  fn new(inner: P) -> Self {
    Self {
      inner: Rc::new(RefCell::new(inner)),
    }
  }
}

impl<P> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SharedPostingsEnum<P>
where
  P: PostingsEnum,
{
}
impl<P> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for SharedPostingsEnum<P> where
  P: PostingsEnum
{
}

impl<P> DocIdSetIterator for SharedPostingsEnum<P>
where
  P: PostingsEnum,
{
  fn doc_id(&self) -> i32 {
    self.inner.borrow().doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.inner.borrow_mut().next_doc()
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.inner.borrow_mut().advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self.inner.borrow().cost()
  }
}

impl<P> PostingsEnum for SharedPostingsEnum<P>
where
  P: PostingsEnum,
{
  fn freq(&mut self) -> Result<i32> {
    self.inner.borrow_mut().freq()
  }

  fn next_position(&mut self) -> Result<i32> {
    self.inner.borrow_mut().next_position()
  }

  fn start_offset(&self) -> Result<i32> {
    self.inner.borrow().start_offset()
  }

  fn end_offset(&self) -> Result<i32> {
    self.inner.borrow().end_offset()
  }

  fn get_payload(&self) -> Result<Option<BytesRefValueEnum<'_>>> {
    Ok(
      self
        .inner
        .borrow()
        .get_payload()?
        .map(|payload| BytesRefValueEnum::Buffer(Cow::Owned(payload.into_owned()))),
    )
  }
}

#[doc(hidden)]
pub struct SharedImpactsEnum<I> {
  inner: Rc<RefCell<I>>,
}

impl<I> Clone for SharedImpactsEnum<I> {
  fn clone(&self) -> Self {
    Self {
      inner: self.inner.clone(),
    }
  }
}

impl<I> SharedImpactsEnum<I> {
  fn new(inner: I) -> Self {
    Self {
      inner: Rc::new(RefCell::new(inner)),
    }
  }
}

impl<I> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SharedImpactsEnum<I>
where
  I: ImpactsEnum,
{
}
impl<I> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for SharedImpactsEnum<I> where
  I: ImpactsEnum
{
}

impl<I> DocIdSetIterator for SharedImpactsEnum<I>
where
  I: ImpactsEnum,
{
  fn doc_id(&self) -> i32 {
    self.inner.borrow().doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.inner.borrow_mut().next_doc()
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.inner.borrow_mut().advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self.inner.borrow().cost()
  }
}

impl<I> PostingsEnum for SharedImpactsEnum<I>
where
  I: ImpactsEnum,
{
  fn freq(&mut self) -> Result<i32> {
    self.inner.borrow_mut().freq()
  }

  fn next_position(&mut self) -> Result<i32> {
    self.inner.borrow_mut().next_position()
  }

  fn start_offset(&self) -> Result<i32> {
    self.inner.borrow().start_offset()
  }

  fn end_offset(&self) -> Result<i32> {
    self.inner.borrow().end_offset()
  }

  fn get_payload(&self) -> Result<Option<BytesRefValueEnum<'_>>> {
    Ok(
      self
        .inner
        .borrow()
        .get_payload()?
        .map(|payload| BytesRefValueEnum::Buffer(Cow::Owned(payload.into_owned()))),
    )
  }
}

impl<I> ImpactsSource for SharedImpactsEnum<I>
where
  I: ImpactsEnum,
{
  fn advance_shallow(&mut self, target: i32) -> Result<()> {
    self.inner.borrow_mut().advance_shallow(target)
  }

  type Impacts<'a>
    = OwnedImpacts
  where
    Self: 'a;

  fn get_impacts(&self) -> Result<Self::Impacts<'_>> {
    let inner = self.inner.borrow();
    let impacts = inner.get_impacts()?;
    OwnedImpacts::from_impacts(&impacts)
  }
}

impl<I> ImpactsEnum for SharedImpactsEnum<I> where I: ImpactsEnum {}

#[derive(Clone)]
#[doc(hidden)]
pub struct OwnedImpacts {
  doc_id_uptos: Vec<i32>,
  impacts: Vec<Vec<Impact>>,
}

impl OwnedImpacts {
  fn from_impacts<I>(impacts: &I) -> Result<Self>
  where
    I: Impacts,
  {
    let mut doc_id_uptos = Vec::with_capacity(impacts.num_levels());
    let mut impact_lists = Vec::with_capacity(impacts.num_levels());
    for level in 0..impacts.num_levels() {
      doc_id_uptos.push(impacts.get_doc_id_upto(level));
      impact_lists.push(impacts.get_impacts(level)?);
    }
    Ok(Self {
      doc_id_uptos,
      impacts: impact_lists,
    })
  }
}

impl Impacts for OwnedImpacts {
  fn num_levels(&self) -> usize {
    self.doc_id_uptos.len()
  }

  fn get_doc_id_upto(&self, level: usize) -> i32 {
    self.doc_id_uptos[level]
  }

  fn get_impacts(&self, level: usize) -> Result<Vec<Impact>> {
    Ok(self.impacts[level].clone())
  }
}

#[doc(hidden)]
pub struct SynonymImpactsSource<IE> {
  impacts_enums: Vec<IE>,
  boosts: Vec<f32>,
}

impl<IE> SynonymImpactsSource<IE> {
  fn new(impacts_enums: Vec<IE>, boosts: Vec<f32>) -> Self {
    debug_assert_eq!(impacts_enums.len(), boosts.len());
    Self {
      impacts_enums,
      boosts,
    }
  }
}

impl<IE> ImpactsSource for SynonymImpactsSource<IE>
where
  IE: ImpactsEnum,
{
  fn advance_shallow(&mut self, target: i32) -> Result<()> {
    for impacts_enum in &mut self.impacts_enums {
      if impacts_enum.doc_id() < target {
        impacts_enum.advance_shallow(target)?;
      }
    }
    Ok(())
  }

  type Impacts<'a>
    = SynonymImpacts<'a, IE::Impacts<'a>, IE>
  where
    Self: 'a;

  fn get_impacts(&self) -> Result<Self::Impacts<'_>> {
    let mut impacts = Vec::with_capacity(self.impacts_enums.len());
    let mut lead = 0;
    let mut lead_doc_id_up_to = None;

    for (i, impacts_enum) in self.impacts_enums.iter().enumerate() {
      let impact_values = impacts_enum.get_impacts()?;
      let doc_id_up_to = impact_values.get_doc_id_upto(0);
      if lead_doc_id_up_to.is_none_or(|lead_doc_id| doc_id_up_to < lead_doc_id) {
        lead = i;
        lead_doc_id_up_to = Some(doc_id_up_to);
      }
      impacts.push(impact_values);
    }

    Ok(SynonymImpacts {
      impacts,
      impacts_enums: &self.impacts_enums,
      boosts: &self.boosts,
      lead,
    })
  }
}

#[doc(hidden)]
pub struct SynonymImpacts<'a, I, IE> {
  impacts: Vec<I>,
  impacts_enums: &'a [IE],
  boosts: &'a [f32],
  lead: usize,
}

impl<I: Impacts, IE> SynonymImpacts<'_, I, IE> {
  fn get_level(impacts: &I, doc_id_up_to: i32) -> Option<usize> {
    (0..impacts.num_levels()).find(|&level| impacts.get_doc_id_upto(level) >= doc_id_up_to)
  }

  fn merge_impacts(to_merge: SmallVec<[Vec<Impact>; 8]>) -> Result<Vec<Impact>> {
    let mut pq = PriorityQueue::new(to_merge.len(), SynonymSubIteratorCmp)?;
    let mut subs = Vec::with_capacity(to_merge.len());
    subs.extend(to_merge.into_iter().map(SynonymSubIterator::new));
    pq.add_all(subs)?;

    let mut merged_impacts = Vec::new();

    // Idea: merge impacts by norm. The tricky thing is that we need to
    // consider norm values that are not in the impacts too. For
    // instance if the list of impacts is [{freq=2,norm=10}, {freq=4,norm=12}],
    // there might well be a document that has a freq of 2 and a length of 11,
    // which was just not added to the list of impacts because {freq=2,norm=10}
    // is more competitive. So the way it works is that we track the sum of
    // the term freqs that we have seen so far in order to account for these
    // implicit impacts.
    let mut sum_tf = 0i64;
    let mut top = pq
      .top_mut()
      .ok_or_else(|| LuceneError::illegal_state("top is None"))?;
    loop {
      let norm = top.current()?.norm;
      loop {
        let current_freq = top.current()?.freq;
        sum_tf += (current_freq - top.previous_freq) as i64;
        top.next();
        top = pq.update_top()?;
        if top.current_opt().is_none() || top.current()?.norm != norm {
          break;
        }
      }

      let freq_upper_bound = std::cmp::min(i32::MAX as i64, sum_tf) as i32;
      match merged_impacts.last() {
        None => merged_impacts.push(Impact::new(freq_upper_bound, norm)),
        Some(prev_impact) => {
          debug_assert!((prev_impact.norm as u64) < (norm as u64));
          if freq_upper_bound > prev_impact.freq {
            merged_impacts.push(Impact::new(freq_upper_bound, norm));
          } // otherwise the previous impact is already more competitive
        },
      }

      if top.current_opt().is_none() {
        break;
      }
    }

    Ok(merged_impacts)
  }
}

impl<I: Impacts, IE: ImpactsEnum> Impacts for SynonymImpacts<'_, I, IE> {
  fn num_levels(&self) -> usize {
    self.impacts[self.lead].num_levels()
  }

  fn get_doc_id_upto(&self, level: usize) -> i32 {
    self.impacts[self.lead].get_doc_id_upto(level)
  }

  fn get_impacts(&self, level: usize) -> Result<Vec<Impact>> {
    let doc_id_up_to = self.get_doc_id_upto(level);
    let mut to_merge: SmallVec<[_; 8]> = SmallVec::with_capacity(self.impacts.len());

    for i in 0..self.impacts.len() {
      if self.impacts_enums[i].doc_id() <= doc_id_up_to {
        let Some(impacts_level) = Self::get_level(&self.impacts[i], doc_id_up_to) else {
          // One instance doesn't have impacts that cover up to docIdUpTo.
          // Return impacts that trigger the maximum score.
          return Ok(vec![Impact::new(i32::MAX, 1)]);
        };

        let mut impact_list = self.impacts[i].get_impacts(impacts_level)?;
        if self.boosts[i] != 1.0 {
          let boost = self.boosts[i];
          for impact in &mut impact_list {
            impact.freq = ((impact.freq as f32) * boost).ceil() as i32;
          }
        }
        to_merge.push(impact_list);
      }
    }

    debug_assert!(!to_merge.is_empty());
    if to_merge.len() == 1 {
      return Ok(to_merge.remove(0));
    }

    Self::merge_impacts(to_merge)
  }
}

struct SynonymSubIterator {
  iterator: Vec<Impact>,
  current: Option<usize>,
  previous_freq: i32,
}

impl SynonymSubIterator {
  fn new(impacts: Vec<Impact>) -> Self {
    let current = if impacts.is_empty() { None } else { Some(0) };
    Self {
      iterator: impacts,
      current,
      previous_freq: 0,
    }
  }

  fn next(&mut self) {
    let Some(idx) = self.current else {
      return;
    };
    self.previous_freq = self.iterator[idx].freq;
    let next_idx = idx + 1;
    self.current = if next_idx >= self.iterator.len() {
      None
    } else {
      Some(next_idx)
    };
  }

  fn current(&self) -> Result<&Impact> {
    self
      .current_opt()
      .ok_or_else(|| LuceneError::illegal_state("current is None"))
  }

  fn current_opt(&self) -> Option<&Impact> {
    self.current.map(|idx| &self.iterator[idx])
  }
}

struct SynonymSubIteratorCmp;

impl Compare<SynonymSubIterator> for SynonymSubIteratorCmp {
  fn less_than(&self, a: &SynonymSubIterator, b: &SynonymSubIterator) -> Result<bool> {
    match (a.current_opt(), b.current_opt()) {
      (None, _) => Ok(false),
      (_, None) => Ok(true),
      (Some(a), Some(b)) => Ok((a.norm as u64) < (b.norm as u64)),
    }
  }
}

type SynonymPostingsEnum<ST> = PostingsEnumEnum2<
  SharedImpactsEnum<<ST as ScorerTypes>::Impacts>,
  SharedPostingsEnum<<ST as ScorerTypes>::Posting>,
>;

type SynonymImpactsEnum<ST> = ImpactsEnumEnum2<
  SharedImpactsEnum<<ST as ScorerTypes>::Impacts>,
  SlowImpactsEnum<SharedPostingsEnum<<ST as ScorerTypes>::Posting>>,
>;

/// Keeps synonym scorers distinct from ordinary term scorers at the shared
/// scorer boundary even when a custom similarity returns an enum scorer.
pub struct SynonymSimScorer<SS>(SS);

impl<SS: SimScorer> SimScorer for SynonymSimScorer<SS> {
  fn score(&self, freq: f32, norm: i64) -> f32 {
    self.0.score(freq, norm)
  }

  fn explain(&self, freq: Explanation, norm: i64) -> Result<Explanation> {
    self.0.explain(freq, norm)
  }
}

pub(crate) type SynonymTermScorer<
  ST,
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> = TermScorer<
  crate::core::search::term_scorer::PostingsTermState<
    SynonymPostingsEnum<ST>,
    Arc<SynonymSimScorer<<SIM as Similarity>::SimScorer>>,
  >,
  <ST as ScorerTypes>::Norm,
>;

type SynonymDisi<
  ST,
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> = DisjunctionDISIApproximation<SynonymSubScorer<ST, SIM>>;

type SynonymImpactsDISI<
  ST,
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> = SeparateImpactsDISI<
  SynonymDisi<ST, SIM>,
  SynonymImpactsSource<SynonymImpactsEnum<ST>>,
  Arc<SynonymSimScorer<<SIM as Similarity>::SimScorer>>,
>;

type EmptySynonymScorer = ConstantScoreDisiScorer<EmptyDISI>;

#[doc(hidden)]
pub enum SynonymSubScorer<
  ST,
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> where
  ST: ScorerTypes,
  SIM: Similarity,
{
  Term {
    scorer: SynonymTermScorer<ST, SIM>,
    boost: f32,
  },
}

impl<ST, SIM> SynonymSubScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes,
{
  fn freq(&mut self) -> Result<f32> {
    match self {
      SynonymSubScorer::Term { scorer, boost } => Ok(*boost * scorer.state.freq()? as f32),
    }
  }
}

impl<ST, SIM> FixedScore for SynonymSubScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes,
{
}

impl<ST, SIM> Scorable for SynonymSubScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
  type Child<'a>
    = crate::core::search::dummy::dummy_scorable::DummyScorable
  where
    Self: 'a;

  fn score(&mut self) -> Result<f32> {
    match self {
      SynonymSubScorer::Term { scorer, .. } => scorer.score(),
    }
  }

  fn smoothing_score(&mut self, doc_id: i32) -> Result<f32> {
    match self {
      SynonymSubScorer::Term { scorer, .. } => scorer.smoothing_score(doc_id),
    }
  }

  fn set_min_competitive_score(&mut self, min_score: f32) -> Result<()> {
    match self {
      SynonymSubScorer::Term { scorer, .. } => scorer.set_min_competitive_score(min_score),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      SynonymSubScorer::Term { scorer, .. } => Scorable::cost(scorer),
    }
  }
}

impl<ST, SIM> DocIdSetIterator for SynonymSubScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
  fn doc_id(&self) -> i32 {
    match self {
      SynonymSubScorer::Term { scorer, .. } => DocIdSetIterator::doc_id(scorer),
    }
  }
  fn next_doc(&mut self) -> Result<i32> {
    match self {
      SynonymSubScorer::Term { scorer, .. } => DocIdSetIterator::next_doc(scorer),
    }
  }
  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      SynonymSubScorer::Term { scorer, .. } => DocIdSetIterator::advance(scorer, target),
    }
  }
  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      SynonymSubScorer::Term { scorer, .. } => DocIdSetIterator::slow_advance(scorer, target),
    }
  }
  fn cost(&self) -> Result<i64> {
    match self {
      SynonymSubScorer::Term { scorer, .. } => DocIdSetIterator::cost(scorer),
    }
  }
}
impl<ST, SIM> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SynonymSubScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
  fn get_fixed_bit_set(&self) -> Option<&crate::core::util::fixed_bit_set::FixedBitSet> {
    let iterator = {
      match self {
        SynonymSubScorer::Term { scorer, .. } => scorer,
      }
    };
    crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions::get_fixed_bit_set(
      iterator,
    )
  }
  fn get_sparse_fixed_bit_set(
    &self,
  ) -> Option<&crate::core::util::sparse_fixed_bit_set::SparseFixedBitSet> {
    let iterator = {
      match self {
        SynonymSubScorer::Term { scorer, .. } => scorer,
      }
    };
    crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions::get_sparse_fixed_bit_set(
      iterator,
    )
  }
  fn get_doc_base_fixed_bit_set(
    &self,
  ) -> Option<(usize, &crate::core::util::fixed_bit_set::FixedBitSet)> {
    let iterator = {
      match self {
        SynonymSubScorer::Term { scorer, .. } => scorer,
      }
    };
    crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions::get_doc_base_fixed_bit_set(
      iterator,
    )
  }
}
impl<ST, SIM> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for SynonymSubScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
  fn is_bit_iter(&self) -> bool {
    match self {
      SynonymSubScorer::Term { scorer, .. } => {
        crate::core::search::doc_id_set_iterator::BitSetIteratorAccess::is_bit_iter(scorer)
      },
    }
  }
  fn get(&self, index: usize) -> Result<bool> {
    match self {
      SynonymSubScorer::Term { scorer, .. } => {
        crate::core::search::doc_id_set_iterator::BitSetIteratorAccess::get(scorer, index)
      },
    }
  }
  fn set_doc_id(&mut self, doc: i32) -> Result<()> {
    match self {
      SynonymSubScorer::Term { scorer, .. } => {
        crate::core::search::doc_id_set_iterator::BitSetIteratorAccess::set_doc_id(scorer, doc)
      },
    }
  }
  fn bit_set_length(&self) -> Result<usize> {
    match self {
      SynonymSubScorer::Term { scorer, .. } => {
        crate::core::search::doc_id_set_iterator::BitSetIteratorAccess::bit_set_length(scorer)
      },
    }
  }
}
impl<ST, SIM> ApproximationDISI for SynonymSubScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
  fn approx_doc_id(&self) -> i32 {
    DocIdSetIterator::doc_id(self)
  }
  fn approx_next_doc(&mut self) -> Result<i32> {
    DocIdSetIterator::next_doc(self)
  }
  fn approx_advance(&mut self, target: i32) -> Result<i32> {
    DocIdSetIterator::advance(self, target)
  }
  fn approx_slow_advance(&mut self, target: i32) -> Result<i32> {
    DocIdSetIterator::slow_advance(self, target)
  }
  fn approx_cost(&self) -> Result<i64> {
    DocIdSetIterator::cost(self)
  }
}
impl<ST, SIM> ApproximationBitSetAccess for SynonymSubScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
  fn approx_is_bit_iter(&self) -> bool {
    crate::core::search::doc_id_set_iterator::BitSetIteratorAccess::is_bit_iter(self)
  }
  fn approx_get(&self, index: usize) -> Result<bool> {
    crate::core::search::doc_id_set_iterator::BitSetIteratorAccess::get(self, index)
  }
  fn approx_set_doc_id(&mut self, doc: i32) -> Result<()> {
    crate::core::search::doc_id_set_iterator::BitSetIteratorAccess::set_doc_id(self, doc)
  }
  fn approx_bit_set_length(&self) -> Result<usize> {
    crate::core::search::doc_id_set_iterator::BitSetIteratorAccess::bit_set_length(self)
  }
}

impl<ST, SIM> TwoPhaseIterator for SynonymSubScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
  fn matches(&mut self) -> Result<bool> {
    Ok(true)
  }
  fn match_cost(&self) -> f32 {
    0.0
  }
}

impl<ST, SIM> Scorer for SynonymSubScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
  fn scoring_doc_id(&mut self) -> Result<i32> {
    match self {
      SynonymSubScorer::Term { scorer, .. } => (scorer).scoring_doc_id(),
    }
  }

  fn get_max_score(&mut self, upto: i32) -> Result<f32> {
    match self {
      SynonymSubScorer::Term { scorer, .. } => scorer.get_max_score(upto),
    }
  }

  fn has_two_phase_iterator(&self) -> TwoPhaseState {
    TwoPhaseState::No
  }
}

#[doc(hidden)]
pub struct SynonymScorer<
  ST,
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
  impacts_disi: SynonymImpactsDISI<ST, SIM>,
  norms: Option<<ST as ScorerTypes>::Norm>,
  score_mode: ScoreMode,
}

impl<ST, SIM> SynonymScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
  fn new(
    impacts_disi: SynonymImpactsDISI<ST, SIM>,
    norms: Option<<ST as ScorerTypes>::Norm>,
    score_mode: ScoreMode,
  ) -> Self {
    Self {
      impacts_disi,
      norms,
      score_mode,
    }
  }

  fn freq(&mut self) -> Result<f32> {
    let list_index = self.impacts_disi.iterator_mut().top_list_root();
    let all_scores = self.impacts_disi.iterator_mut().all_scores_mut();
    let mut freq = all_scores[list_index].scorer.freq()?;
    let mut next = all_scores[list_index].next;
    while let Some(next_index) = next {
      freq += all_scores[next_index].scorer.freq()?;
      next = all_scores[next_index].next;
    }
    Ok(freq)
  }

  fn use_impacts_disi(&self) -> bool {
    self.score_mode == ScoreMode::TopScores
  }
}

impl<ST, SIM> FixedScore for SynonymScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
}

impl<ST, SIM> Scorable for SynonymScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
  type Child<'a>
    = crate::core::search::dummy::dummy_scorable::DummyScorable
  where
    Self: 'a;

  fn score(&mut self) -> Result<f32> {
    let mut norm = 1;
    let doc = if self.use_impacts_disi() {
      self.impacts_disi.doc_id()
    } else {
      self.impacts_disi.iterator().doc_id()
    };
    if let Some(ref mut norms) = self.norms
      && norms.advance_exact(doc)?
    {
      norm = norms.long_value()?;
    }
    let freq = self.freq()?;
    Ok(self.impacts_disi.max_score_cache().scorer.score(freq, norm))
  }

  fn set_min_competitive_score(&mut self, _min_score: f32) -> Result<()> {
    self.impacts_disi.set_min_competitive_score(_min_score);
    Ok(())
  }

  fn cost(&self) -> Result<i64> {
    self.impacts_disi.cost()
  }
}

impl<ST, SIM> DocIdSetIterator for SynonymScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
  fn doc_id(&self) -> i32 {
    if self.use_impacts_disi() {
      DocIdSetIterator::doc_id(&self.impacts_disi)
    } else {
      DocIdSetIterator::doc_id(self.impacts_disi.iterator())
    }
  }
  fn next_doc(&mut self) -> Result<i32> {
    if self.use_impacts_disi() {
      DocIdSetIterator::next_doc(&mut self.impacts_disi)
    } else {
      DocIdSetIterator::next_doc(self.impacts_disi.iterator_mut())
    }
  }
  fn advance(&mut self, target: i32) -> Result<i32> {
    if self.use_impacts_disi() {
      DocIdSetIterator::advance(&mut self.impacts_disi, target)
    } else {
      DocIdSetIterator::advance(self.impacts_disi.iterator_mut(), target)
    }
  }
  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    if self.use_impacts_disi() {
      DocIdSetIterator::slow_advance(&mut self.impacts_disi, target)
    } else {
      DocIdSetIterator::slow_advance(self.impacts_disi.iterator_mut(), target)
    }
  }
  fn cost(&self) -> Result<i64> {
    if self.use_impacts_disi() {
      DocIdSetIterator::cost(&self.impacts_disi)
    } else {
      DocIdSetIterator::cost(self.impacts_disi.iterator())
    }
  }
}
impl<ST, SIM> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SynonymScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
}
impl<ST, SIM> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for SynonymScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
}
impl<ST, SIM> ApproximationDISI for SynonymScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
  fn approx_doc_id(&self) -> i32 {
    DocIdSetIterator::doc_id(self)
  }
  fn approx_next_doc(&mut self) -> Result<i32> {
    DocIdSetIterator::next_doc(self)
  }
  fn approx_advance(&mut self, target: i32) -> Result<i32> {
    DocIdSetIterator::advance(self, target)
  }
  fn approx_slow_advance(&mut self, target: i32) -> Result<i32> {
    DocIdSetIterator::slow_advance(self, target)
  }
  fn approx_cost(&self) -> Result<i64> {
    DocIdSetIterator::cost(self)
  }
}
impl<ST, SIM> ApproximationBitSetAccess for SynonymScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
  fn approx_is_bit_iter(&self) -> bool {
    crate::core::search::doc_id_set_iterator::BitSetIteratorAccess::is_bit_iter(self)
  }
  fn approx_get(&self, index: usize) -> Result<bool> {
    crate::core::search::doc_id_set_iterator::BitSetIteratorAccess::get(self, index)
  }
  fn approx_set_doc_id(&mut self, doc: i32) -> Result<()> {
    crate::core::search::doc_id_set_iterator::BitSetIteratorAccess::set_doc_id(self, doc)
  }
  fn approx_bit_set_length(&self) -> Result<usize> {
    crate::core::search::doc_id_set_iterator::BitSetIteratorAccess::bit_set_length(self)
  }
}

impl<ST, SIM> TwoPhaseIterator for SynonymScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
  fn matches(&mut self) -> Result<bool> {
    Ok(true)
  }
  fn match_cost(&self) -> f32 {
    0.0
  }
}

impl<ST, SIM> Scorer for SynonymScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
  fn scoring_doc_id(&mut self) -> Result<i32> {
    Ok(if self.use_impacts_disi() {
      self.impacts_disi.doc_id()
    } else {
      self.impacts_disi.iterator().doc_id()
    })
  }

  fn advance_shallow(&mut self, target: i32) -> Result<i32> {
    self
      .impacts_disi
      .max_score_cache_mut()
      .advance_shallow(target)
  }

  fn get_max_score(&mut self, _upto: i32) -> Result<f32> {
    self.impacts_disi.max_score_cache_mut().get_max_score(_upto)
  }

  fn has_two_phase_iterator(&self) -> TwoPhaseState {
    TwoPhaseState::No
  }
}

#[doc(hidden)]
pub struct FreqBoostTermScorer<
  ST,
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
  boost: f32,
  in_: SynonymTermScorer<ST, SIM>,
  norms: Option<<ST as ScorerTypes>::Norm>,
}

impl<ST, SIM> FreqBoostTermScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
  fn new(
    boost: f32,
    in_: SynonymTermScorer<ST, SIM>,
    norms: Option<<ST as ScorerTypes>::Norm>,
  ) -> Result<Self> {
    if boost.is_nan()
      || CoreHelper::compare_f32(boost, 0.0).is_lt()
      || CoreHelper::compare_f32(boost, 1.0).is_gt()
    {
      return Err(LuceneError::illegal_argument(
        "boost must be a positive float between 0 (exclusive) and 1 (inclusive)",
      ));
    }
    Ok(Self { boost, in_, norms })
  }

  fn freq(&mut self) -> Result<f32> {
    Ok(self.boost * self.in_.state.freq()? as f32)
  }
}

impl<ST, SIM> FixedScore for FreqBoostTermScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
}

impl<ST, SIM> Scorable for FreqBoostTermScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
  type Child<'a>
    = crate::core::search::dummy::dummy_scorable::DummyScorable
  where
    Self: 'a;

  fn score(&mut self) -> Result<f32> {
    let mut norm = 1;
    let doc = (self.in_).scoring_doc_id()?;
    if let Some(ref mut norms) = self.norms
      && norms.advance_exact(doc)?
    {
      norm = norms.long_value()?;
    }
    let freq = self.freq()?;
    Ok(self.in_.state.sim_scorer().score(freq, norm))
  }

  fn set_min_competitive_score(&mut self, min_score: f32) -> Result<()> {
    self.in_.set_min_competitive_score(min_score)
  }
}

impl<ST, SIM> DocIdSetIterator for FreqBoostTermScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
  fn doc_id(&self) -> i32 {
    DocIdSetIterator::doc_id(&self.in_)
  }
  fn next_doc(&mut self) -> Result<i32> {
    DocIdSetIterator::next_doc(&mut self.in_)
  }
  fn advance(&mut self, target: i32) -> Result<i32> {
    DocIdSetIterator::advance(&mut self.in_, target)
  }
  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    DocIdSetIterator::slow_advance(&mut self.in_, target)
  }
  fn cost(&self) -> Result<i64> {
    DocIdSetIterator::cost(&self.in_)
  }
}
impl<ST, SIM> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for FreqBoostTermScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
  fn get_fixed_bit_set(&self) -> Option<&crate::core::util::fixed_bit_set::FixedBitSet> {
    let iterator = { &self.in_ };
    crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions::get_fixed_bit_set(
      iterator,
    )
  }
  fn get_sparse_fixed_bit_set(
    &self,
  ) -> Option<&crate::core::util::sparse_fixed_bit_set::SparseFixedBitSet> {
    let iterator = { &self.in_ };
    crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions::get_sparse_fixed_bit_set(
      iterator,
    )
  }
  fn get_doc_base_fixed_bit_set(
    &self,
  ) -> Option<(usize, &crate::core::util::fixed_bit_set::FixedBitSet)> {
    let iterator = { &self.in_ };
    crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions::get_doc_base_fixed_bit_set(
      iterator,
    )
  }
}
impl<ST, SIM> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for FreqBoostTermScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
  fn is_bit_iter(&self) -> bool {
    crate::core::search::doc_id_set_iterator::BitSetIteratorAccess::is_bit_iter(&self.in_)
  }
  fn get(&self, index: usize) -> Result<bool> {
    crate::core::search::doc_id_set_iterator::BitSetIteratorAccess::get(&self.in_, index)
  }
  fn set_doc_id(&mut self, doc: i32) -> Result<()> {
    crate::core::search::doc_id_set_iterator::BitSetIteratorAccess::set_doc_id(&mut self.in_, doc)
  }
  fn bit_set_length(&self) -> Result<usize> {
    crate::core::search::doc_id_set_iterator::BitSetIteratorAccess::bit_set_length(&self.in_)
  }
}
impl<ST, SIM> ApproximationDISI for FreqBoostTermScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
  fn approx_doc_id(&self) -> i32 {
    DocIdSetIterator::doc_id(self)
  }
  fn approx_next_doc(&mut self) -> Result<i32> {
    DocIdSetIterator::next_doc(self)
  }
  fn approx_advance(&mut self, target: i32) -> Result<i32> {
    DocIdSetIterator::advance(self, target)
  }
  fn approx_slow_advance(&mut self, target: i32) -> Result<i32> {
    DocIdSetIterator::slow_advance(self, target)
  }
  fn approx_cost(&self) -> Result<i64> {
    DocIdSetIterator::cost(self)
  }
}
impl<ST, SIM> ApproximationBitSetAccess for FreqBoostTermScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
  fn approx_is_bit_iter(&self) -> bool {
    crate::core::search::doc_id_set_iterator::BitSetIteratorAccess::is_bit_iter(self)
  }
  fn approx_get(&self, index: usize) -> Result<bool> {
    crate::core::search::doc_id_set_iterator::BitSetIteratorAccess::get(self, index)
  }
  fn approx_set_doc_id(&mut self, doc: i32) -> Result<()> {
    crate::core::search::doc_id_set_iterator::BitSetIteratorAccess::set_doc_id(self, doc)
  }
  fn approx_bit_set_length(&self) -> Result<usize> {
    crate::core::search::doc_id_set_iterator::BitSetIteratorAccess::bit_set_length(self)
  }
}

impl<ST, SIM> TwoPhaseIterator for FreqBoostTermScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
  fn matches(&mut self) -> Result<bool> {
    Ok(true)
  }
  fn match_cost(&self) -> f32 {
    0.0
  }
}

impl<ST, SIM> Scorer for FreqBoostTermScorer<ST, SIM>
where
  SIM: Similarity,
  ST: ScorerTypes + 'static,
{
  fn scoring_doc_id(&mut self) -> Result<i32> {
    (self.in_).scoring_doc_id()
  }

  fn advance_shallow(&mut self, target: i32) -> Result<i32> {
    self.in_.advance_shallow(target)
  }

  fn get_max_score(&mut self, upto: i32) -> Result<f32> {
    self.in_.get_max_score(upto)
  }

  fn has_two_phase_iterator(&self) -> TwoPhaseState {
    TwoPhaseState::No
  }
}

type SynonymScorerEnum<
  ST,
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> = ScorerEnum4<
  SynonymScorer<ST, SIM>,
  FreqBoostTermScorer<ST, SIM>,
  SynonymTermScorer<ST, SIM>,
  EmptySynonymScorer,
>;

pub struct SynonymScorerSupplier<
  LR,
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> where
  SIM: Similarity,
  LR: LeafReader + 'static,
{
  cost: i64,
  term_states: Arc<Vec<TermStates>>,
  prepare_states: Box<[Option<PrepareState<LRTermsEnum<LR>>>]>,
  query: Arc<SynonymQueryData>,
  sim_weight: Option<Arc<SynonymSimScorer<SIM::SimScorer>>>,
  score_mode: ScoreMode,
  iterators: Option<Vec<SynonymPostingsEnum<LeafScorerTypes<LR, SIM>>>>,
  impacts: Option<Vec<SynonymImpactsEnum<LeafScorerTypes<LR, SIM>>>>,
  term_boosts: Vec<f32>,
  initialized: bool,
}

impl<LR, SIM> SynonymScorerSupplier<LR, SIM>
where
  SIM: Similarity,
  LR: LeafReader + 'static,
{
  fn new(
    cost: i64,
    term_states: Arc<Vec<TermStates>>,
    prepare_states: Vec<Option<PrepareState<LRTermsEnum<LR>>>>,
    query: Arc<SynonymQueryData>,
    sim_weight: Option<Arc<SynonymSimScorer<SIM::SimScorer>>>,
    score_mode: ScoreMode,
  ) -> Self {
    Self {
      cost,
      term_states,
      prepare_states: prepare_states.into_boxed_slice(),
      query,
      sim_weight,
      score_mode,
      iterators: None,
      impacts: None,
      term_boosts: Vec::new(),
      initialized: false,
    }
  }

  fn init(&mut self, context: &LeafReaderContext<LR>) -> Result<()> {
    if self.initialized {
      return Ok(());
    }

    let query = self.query.as_ref();
    let mut iterators = Vec::with_capacity(query.terms.len());
    let mut impacts = Vec::with_capacity(query.terms.len());
    let mut term_boosts = Vec::with_capacity(query.terms.len());
    let mut cost = 0;

    for i in 0..query.terms.len() {
      let Some(mut prepare_state) = self.prepare_states[i].take() else {
        continue;
      };
      let state = self.term_states[i].resolve(&mut prepare_state)?;
      if let Some(state) = state {
        let mut terms_enum = context
          .reader()
          .terms(&query.field)?
          .ok_or_else(|| LuceneError::illegal_state("term should exist here"))?
          .iterator()?;
        terms_enum.seek_exact_with_state(&query.terms[i].term, state.as_ref())?;
        if self.score_mode == ScoreMode::TopScores {
          let impacts_enum = SharedImpactsEnum::new(terms_enum.impacts(FREQS as i32)?);
          iterators.push(PostingsEnumEnum2::A(impacts_enum.clone()));
          impacts.push(ImpactsEnumEnum2::A(impacts_enum));
        } else {
          let postings_enum =
            SharedPostingsEnum::new(terms_enum.postings_with_flags(None, FREQS as i32)?);
          iterators.push(PostingsEnumEnum2::B(postings_enum.clone()));
          impacts.push(ImpactsEnumEnum2::B(SlowImpactsEnum::new(postings_enum)));
        }
        term_boosts.push(query.terms[i].boost);
      }
    }

    for iterator in &iterators {
      cost += iterator.cost()?;
    }

    self.iterators = Some(iterators);
    self.impacts = Some(impacts);
    self.term_boosts = term_boosts;
    self.cost = cost;
    self.initialized = true;
    Ok(())
  }

  fn get_scorer(
    &mut self,
    context: &LeafReaderContext<LR>,
  ) -> Result<SynonymScorerEnum<LeafScorerTypes<LR, SIM>, SIM>>
  where
    LR: 'static,
  {
    self.init(context)?;

    let Some(mut iterators) = self.iterators.take() else {
      return Err(LuceneError::illegal_state(
        "ScorerSupplier.get must be called at most once",
      ));
    };
    let Some(impacts) = self.impacts.take() else {
      return Err(LuceneError::illegal_state(
        "ScorerSupplier.get must be called at most once",
      ));
    };
    let mut term_boosts = std::mem::take(&mut self.term_boosts);

    if iterators.is_empty() {
      return Ok(SynonymScorerEnum::D(ConstantScoreScorer::from_disi(
        0.0,
        self.score_mode,
        EmptyDISI::new(),
      )));
    }

    let sim_weight = self.sim_weight.take().ok_or_else(|| {
      LuceneError::illegal_state("simWeight is missing for matching synonym terms")
    })?;
    let query = self.query.as_ref();
    let norms = context.reader().get_norm_values(&query.field)?;

    if iterators.len() == 1 {
      let iterator = iterators.remove(0);
      let boost = term_boosts.remove(0);
      return if self.score_mode == ScoreMode::CompleteNoScores || boost == 1.0 {
        let scorer = TermScorer::from_postings(iterator, sim_weight, norms);
        Ok(SynonymScorerEnum::C(scorer))
      } else {
        let scorer = TermScorer::from_postings(iterator, sim_weight, None);
        Ok(SynonymScorerEnum::B(FreqBoostTermScorer::new(
          boost, scorer, norms,
        )?))
      };
    }

    let mut wrappers = Vec::with_capacity(iterators.len());
    let mut queue = DisiPriorityQueue::new(iterators.len());
    for (idx, (iterator, boost)) in iterators
      .into_iter()
      .zip(term_boosts.iter().copied())
      .enumerate()
    {
      let term_scorer = TermScorer::from_postings(iterator, sim_weight.clone(), None);
      let mut wrapper = DisiWrapper::new(SynonymSubScorer::Term {
        scorer: term_scorer,
        boost,
      })?;
      wrapper.doc = -1;
      wrappers.push(wrapper);
      queue.add(idx, &wrappers);
    }

    let iterator = DisjunctionDISIApproximation::new(queue, wrappers)?;
    let impacts_source = SynonymQuery::merge_impacts(impacts, term_boosts);
    let max_score_cache = MaxScoreCache::new(impacts_source, sim_weight);
    let impacts_disi = SeparateImpactsDISI::new(iterator, max_score_cache);

    Ok(SynonymScorerEnum::A(SynonymScorer::new(
      impacts_disi,
      norms,
      self.score_mode,
    )))
  }
}

impl<IRC, SIM> ScorerSupplier<IRC, SIM> for SynonymScorerSupplier<IRCLeafReader<IRC>, SIM>
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
{
  type Scorer = SynonymScorerEnum<LeafScorerTypes<IRCLeafReader<IRC>, SIM>, SIM>;
  type BulkScorer = crate::core::search::weight::DefaultBulkScorer<Self::Scorer>;

  fn get(
    &mut self,
    _lead_cost: i64,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Self::Scorer>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    self.get_scorer(context)
  }

  fn bulk_scorer(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Option<Self::BulkScorer>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    Ok(Some(self.default_bulk_scorer(context, searcher)?))
  }

  fn cost(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<i64>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let result: Result<i64> = (|| {
      self.init(context)?;
      Ok(self.cost)
    })();

    match result {
      Ok(v) => Ok(v),
      Err(e) => {
        let mut err = UncheckedIOError::new("");
        err.add_suppressed(e);
        Err(err.into())
      },
    }
  }
}

/// A builder for [`SynonymQuery`].
pub struct Builder {
  field: String,
  terms: Vec<TermAndBoost>,
}

impl Builder {
  /// Creates a new instance.
  pub fn new<T>(field: T) -> Self
  where
    T: Into<String>,
  {
    Self {
      field: field.into(),
      terms: Vec::new(),
    }
  }

  /// Adds the provided [`Term`] as a synonym.
  pub fn add_term<TermInput>(&mut self, term: TermInput) -> Result<&mut Self>
  where
    TermInput: Into<Term>,
  {
    let term = term.into();
    self.add_term_with_boost(term, 1.0)
  }

  /// Adds the provided [`Term`] as a synonym with a document-frequency boost.
  pub fn add_term_with_boost<TermInput>(&mut self, term: TermInput, boost: f32) -> Result<&mut Self>
  where
    TermInput: Into<Term>,
  {
    let term = term.into();
    if self.field != term.field {
      return Err(LuceneError::illegal_argument(
        "Synonyms must be across the same field",
      ));
    }
    self.add_bytes_with_boost(term.bytes, boost)
  }

  /// Adds the provided term bytes as a synonym with a document-frequency boost.
  /// Accepts text, owned or borrowed bytes, and `BytesRef` values.
  pub fn add_bytes_with_boost<B>(&mut self, term: B, boost: f32) -> Result<&mut Self>
  where
    B: Into<BytesRef<Vec<u8>>>,
  {
    if boost.is_nan() || boost <= 0.0 || boost > 1.0 {
      return Err(LuceneError::illegal_argument(
        "boost must be a positive float between 0 (exclusive) and 1 (inclusive)",
      ));
    }
    if self.terms.len() >= index_searcher::get_max_clause_count() {
      return Err(index_searcher::new_nested());
    }
    self.terms.push(TermAndBoost {
      term: term.into(),
      boost,
    });
    Ok(self)
  }

  /// Builds the [`SynonymQuery`].
  pub fn build(mut self) -> SynonymQuery {
    self.terms.sort_by(|a, b| a.term.cmp(&b.term));
    SynonymQuery::new(self.terms, self.field)
  }
}

impl crate::core::util::accountable::Accountable for SynonymQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED)
  }
}

impl<SIM> SynonymWeight<SIM>
where
  SIM: Similarity,
{
  pub(crate) fn matches_iterator<'a, IRC: IndexReaderContext + 'static>(
    &'a self,
    context: &'a LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
  ) -> Result<
    Option<crate::core::search::query::QueryWeightMatchesIterator<'a, IRCLeafReader<IRC>, SIM>>,
  > {
    let query = &self.parent_query;
    let field = query.get_field();
    from_terms_enum(
      context,
      doc,
      Arc::new(self.parent_query.clone().into()),
      field,
      SynonymTermBytesRefIterator {
        terms: &query.data.terms,
        index: 0,
      },
    )
    .map(|value| value.map(|iterator| Box::new(iterator).into()))
  }
}

/// Weight returned by the actual branches of this query factory.
pub enum SynonymQueryWeight<
  IRC: IndexReaderContext + 'static,
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> where
  SIM: Similarity,
{
  Scoring(SynonymWeight<SIM>),
  Rewritten(QueryWeight<IRC, SIM>),
}
impl<IRC: IndexReaderContext + 'static, SIM> SegmentCacheable<IRC> for SynonymQueryWeight<IRC, SIM>
where
  SIM: Similarity,
{
  fn is_cacheable(&self, context: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    match self {
      Self::Scoring(inner) => SegmentCacheable::<IRC>::is_cacheable(inner, context),
      Self::Rewritten(inner) => SegmentCacheable::<IRC>::is_cacheable(inner, context),
    }
  }
}
impl<IRC: IndexReaderContext + 'static, SIM> Weight<IRC, SIM> for SynonymQueryWeight<IRC, SIM>
where
  SIM: Similarity,
{
  type ScorerSupplier = QueryWeightSs<IRC, SIM>;
  fn matches<'a>(
    &'a self,
    context: &'a LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &'a IndexSearcher<IRC, SIM>,
  ) -> Result<Option<QueryWeightMatches<'a, IRC, SIM>>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    match self {
      Self::Scoring(inner) => Weight::<IRC, SIM>::matches(inner, context, doc, searcher),
      Self::Rewritten(inner) => Weight::<IRC, SIM>::matches(inner, context, doc, searcher),
    }
  }
  fn explain(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Explanation>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    match self {
      Self::Scoring(inner) => Weight::<IRC, SIM>::explain(inner, context, doc, searcher),
      Self::Rewritten(inner) => Weight::<IRC, SIM>::explain(inner, context, doc, searcher),
    }
  }
  fn get_query(&self) -> QueryRef<'_> {
    match self {
      Self::Scoring(inner) => Weight::<IRC, SIM>::get_query(inner),
      Self::Rewritten(inner) => Weight::<IRC, SIM>::get_query(inner),
    }
  }
  fn scorer(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Option<QueryWeightSsScorer<IRC, SIM>>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    match self {
      Self::Scoring(inner) => Weight::<IRC, SIM>::scorer(inner, context, searcher)
        .map(|scorer| scorer.map(QueryWeightSsScorer::<IRC, SIM>::new)),
      Self::Rewritten(inner) => Weight::<IRC, SIM>::scorer(inner, context, searcher),
    }
  }
  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Option<Self::ScorerSupplier>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    match self {
      Self::Scoring(inner) => Weight::<IRC, SIM>::scorer_supplier(inner, context, searcher)
        .map(|supplier| supplier.map(Into::into)),
      Self::Rewritten(inner) => Weight::<IRC, SIM>::scorer_supplier(inner, context, searcher),
    }
  }
  fn bulk_scorer(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Option<QueryWeightSsBulkScorer<IRC, SIM>>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    match self {
      Self::Scoring(inner) => Weight::<IRC, SIM>::bulk_scorer(inner, context, searcher)
        .map(|scorer| scorer.map(QueryWeightSsBulkScorer::<IRC, SIM>::new)),
      Self::Rewritten(inner) => Weight::<IRC, SIM>::bulk_scorer(inner, context, searcher),
    }
  }
  fn count(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<i32>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    match self {
      Self::Scoring(inner) => Weight::<IRC, SIM>::count(inner, context, searcher),
      Self::Rewritten(inner) => Weight::<IRC, SIM>::count(inner, context, searcher),
    }
  }
  fn default_count(&self, context: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<i32> {
    match self {
      Self::Scoring(inner) => Weight::<IRC, SIM>::default_count(inner, context),
      Self::Rewritten(inner) => Weight::<IRC, SIM>::default_count(inner, context),
    }
  }
  fn is_cache_wrapper(&self) -> bool {
    match self {
      Self::Scoring(inner) => Weight::<IRC, SIM>::is_cache_wrapper(inner),
      Self::Rewritten(inner) => Weight::<IRC, SIM>::is_cache_wrapper(inner),
    }
  }
  fn into_inner_weight(self: Box<Self>) -> Option<QueryWeight<IRC, SIM>> {
    match *self {
      Self::Scoring(_) => None,
      Self::Rewritten(inner) => inner.into_inner_weight(),
    }
  }
  #[cfg(test)]
  fn as_any(&mut self) -> &mut dyn std::any::Any {
    match self {
      Self::Scoring(inner) => Weight::<IRC, SIM>::as_any(inner),
      Self::Rewritten(inner) => Weight::<IRC, SIM>::as_any(inner),
    }
  }
}
impl<IRC: IndexReaderContext + 'static, SIM> From<SynonymQueryWeight<IRC, SIM>>
  for QueryWeight<IRC, SIM>
where
  SIM: Similarity,
{
  fn from(value: SynonymQueryWeight<IRC, SIM>) -> Self {
    match value {
      SynonymQueryWeight::Scoring(inner) => inner.into(),
      SynonymQueryWeight::Rewritten(inner) => inner,
    }
  }
}
