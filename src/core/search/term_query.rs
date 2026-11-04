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
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::index_reader::{Identity, IndexReader};
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader::{
  LRImpactsEnum, LRNormNumericDocValues, LRPosting, LRTermsEnum, LeafReader,
};
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::postings_enum::{FREQS, NONE, OFFSETS};
use crate::core::index::reader_util::ReaderUtil;
use crate::core::index::term::Term;
use crate::core::index::term_states::{PrepareState, TermStates, build, build_with_stats};
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::search::constant_score_scorer::ConstantScoreDisiScorer;
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, EmptyDISI};
use crate::core::search::explanation::Explanation;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::matches_utils::for_field;
use crate::core::search::query::QueryBase;
use crate::core::search::query::QueryRef;
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::{ScorerEnum2, ScorerEnum4};
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::similarities_impl::similarities::{
  SimScorer, SimScorerEnum2, Similarity, SimilaritySimScorer,
};
use crate::core::search::term_matches_iterator::TermMatchesIterator;
use crate::core::search::term_scorer::{
  ImpactsTermState, PostingsTermState, TermScorer, TermScorerState,
};
use crate::core::search::weight::Weight;
use crate::core::util::core_helper::HasIdentity;
use crate::core::util::error::UncheckedIOError;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::fmt::{Debug, Formatter};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// A Query that matches documents containing a term. This may be combined with other terms with a [`BooleanQuery`](crate::core::search::boolean_query::BooleanQuery).
#[derive(Clone)]
pub struct TermQuery {
  id: Identity,
  pub(crate) term: Arc<Term>,
  per_reader_term_state: Option<Arc<TermStates>>,
}
impl TermQuery {
  pub fn new<T>(term: T) -> Self
  where
    T: Into<Arc<Term>>,
  {
    Self::with_term_state(term, None)
  }
  pub fn get_term(&self) -> Arc<Term> {
    self.term.clone()
  }
  pub fn with_term_state<T>(term: T, ts: Option<Arc<TermStates>>) -> Self
  where
    T: Into<Arc<Term>>,
  {
    Self {
      id: Identity::new(),
      term: term.into(),
      per_reader_term_state: ts,
    }
  }
  pub fn get_term_states(&self) -> Option<&TermStates> {
    self.per_reader_term_state.as_deref()
  }

  pub(crate) fn format_term(
    term_field: &str,
    term_bytes: &BytesRef<Vec<u8>>,
    field: &str,
  ) -> Result<String> {
    let mut buffer = String::new();
    if term_field != field {
      buffer.push_str(term_field);
      buffer.push(':');
    }
    if buffer.is_empty() {
      return Ok(Term::get_string(term_bytes)?.into_owned());
    }
    match Term::get_string(term_bytes) {
      Ok(text) => buffer.push_str(&text),
      Err(_) => {
        buffer.push_str("<?>");
      },
    }
    Ok(buffer)
  }
}

impl PartialEq for TermQuery {
  fn eq(&self, other: &Self) -> bool {
    self.term == other.term
  }
}

impl Hash for TermQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.term.hash(state);
  }
}
impl Eq for TermQuery {}

impl Debug for TermQuery {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self.to_string("") {
      Ok(s) => write!(f, "{}", s),
      Err(_) => Err(std::fmt::Error),
    }
  }
}

impl HasIdentity for TermQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl QueryBase for TermQuery {
  type Weight<IRC, SIM>
    = TermWeight<SIM>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static;

  fn to_string(&self, field: &str) -> Result<String> {
    Self::format_term(self.term.field(), self.term.bytes(), field)
  }

  fn create_weight<IRC, SIM>(
    mut self,
    searcher: &IndexSearcher<IRC, SIM>,
    score_mode: &ScoreMode,
    boost: f32,
  ) -> Result<Self::Weight<IRC, SIM>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    Self: Sized,
    IndexSearcher<IRC, SIM>: Sync,
  {
    let context = searcher.get_top_reader_context();
    let ts = match self.per_reader_term_state.take() {
      Some(v) if { v.was_built_for_id(&context.base().identity) } => v,
      _ => Arc::new(if score_mode.needs_scores() {
        build_with_stats(searcher, &self.term)?
      } else {
        build(searcher, self.term.clone(), false)?
      }),
    };
    TermWeight::new(searcher, *score_mode, boost, ts, self)
  }

  fn visit<QV>(&self, visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    if visitor.accept_field(self.term.field()) {
      visitor.consume_terms(self.into(), std::slice::from_ref(self.term.as_ref()))?;
    }
    Ok(())
  }
}
pub struct TermWeight<SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity>
where
  SIM: Similarity,
{
  similarity: Arc<SIM>,
  sim_scorer: Option<Arc<TermQuerySimScorer<SIM>>>,
  term_states: Arc<TermStates>,
  score_mode: ScoreMode,
  parent_query: TermQuery,
}
impl<SIM> TermWeight<SIM>
where
  SIM: Similarity,
{
  pub fn new<IRC>(
    searcher: &IndexSearcher<IRC, SIM>,
    score_mode: ScoreMode,
    boost: f32,
    term_states: Arc<TermStates>,
    query: TermQuery,
  ) -> Result<Self>
  where
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync,
  {
    debug_assert!(query.per_reader_term_state.is_none());
    let similarity = searcher.get_similarity();

    let sim_scorer = if score_mode.needs_scores() {
      let collection_stats = searcher.collection_statistics(query.term.field())?;
      let term_stats = if term_states.doc_freq()? > 0 {
        Some(searcher.term_statistics(
          query.term.clone(),
          term_states.doc_freq()?,
          term_states.total_term_freq()?,
        )?)
      } else {
        None
      };
      if let Some(term_stats) = term_stats {
        debug_assert!(collection_stats.is_some());
        let collection_stats = collection_stats.as_ref().ok_or_else(|| {
          LuceneError::illegal_state("term query collection statistics are missing")
        })?;
        Some(Arc::new(TermQuerySimScorer::<SIM>::A(similarity.scorer(
          boost,
          collection_stats,
          &[term_stats],
        )?)))
      } else {
        None
      }
    } else {
      // The dummy scorer does not consume statistics, so no fake term or collection
      // statistics need to retain the term or copy the field name.
      Some(Arc::new(TermQuerySimScorer::<SIM>::B(SimScorerImpl)))
    };

    Ok(Self {
      similarity,
      sim_scorer,
      term_states,
      score_mode,
      parent_query: query,
    })
  }
  /// Returns a TermsEnum positioned at this weights Term or None if the term does not exist in the given context
  fn get_terms_enum<LR>(&self, context: &LeafReaderContext<LR>) -> Result<Option<LRTermsEnum<LR>>>
  where
    LR: LeafReader,
  {
    debug_assert!(
      {
        let v = ReaderUtil::get_top_level_context(context);
        self.term_states.was_built_for(v)
      },
      "The top-reader used to create Weight is not the same as the current reader's top-reader"
    );
    let term_states = &self.term_states;
    let mut supplier = term_states.get(context)?;

    let state = match supplier {
      Some(ref mut s) => term_states.resolve(s)?,
      None => None,
    };
    let parent_query = &self.parent_query;

    let Some(state) = state else {
      debug_assert!(
        self.term_not_in_reader(context.reader(), parent_query.term.as_ref())?,
        "no termstate found but term exists in reader"
      );
      return Ok(None);
    };
    let terms = context
      .reader()
      .terms(parent_query.term.field())?
      .ok_or_else(|| LuceneError::illegal_state("term state exists but field terms are missing"))?;
    let mut terms_enum = terms.iterator()?;
    terms_enum.seek_exact_with_state(parent_query.term.bytes(), state.as_ref())?;
    Ok(Some(terms_enum))
  }
  fn term_not_in_reader<LR>(&self, reader: &LR, term: &Term) -> Result<bool>
  where
    LR: LeafReader,
  {
    Ok(LeafReader::doc_freq(reader, term)? == 0)
  }
}

impl<IRC, SIM> SegmentCacheable<IRC> for TermWeight<SIM>
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
{
  fn is_cacheable(&self, _ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    Ok(true)
  }
}

impl<IRC, SIM> Weight<IRC, SIM> for TermWeight<SIM>
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
    let parent_query = &self.parent_query;
    for_field(&parent_query.term.field, (self, context, doc).into())
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
    let mut terms_enum = self.get_terms_enum(context)?;
    let mut scorer = Self::build_term_scorer(
      context,
      terms_enum.as_mut(),
      parent_query.term.as_ref(),
      self.sim_scorer.as_deref(),
      self.score_mode,
      false,
    )?;
    let new_doc = scorer.advance(doc)?;
    if new_doc == doc {
      let freq = match &mut scorer {
        ScorerEnum4::A(ts) => ts.state.freq()?,
        ScorerEnum4::B(ts) => ts.state.freq()?,
        ScorerEnum4::C(ts) => ts.state.freq()?,
        ScorerEnum4::D(_) => {
          return Err(LuceneError::illegal_state("should TermScorer here"));
        },
      };

      let mut norm: i64 = 1;
      if let Some(mut norms) = context.reader().get_norm_values(&parent_query.term.field)?
        && norms.advance_exact(doc)?
      {
        norm = norms.long_value()?;
      }

      let freq_explanation =
        Explanation::match_no_details(freq, "freq, occurrences of term within document");

      let sim_scorer = self
        .sim_scorer
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("term scorer is missing"))?;
      let score_explanation = sim_scorer.explain(freq_explanation, norm)?;

      return Ok(Explanation::match_(
        score_explanation.value.clone(),
        format!(
          "weight({:?} in {}) [{}], result of:",
          QueryRef::Term(&self.parent_query),
          doc,
          self.similarity,
        ),
        vec![score_explanation],
      ));
    }

    Ok(Explanation::no_match_no_details("no matching term"))
  }

  fn get_query(&self) -> QueryRef<'_> {
    QueryRef::Term(&self.parent_query)
  }

  type ScorerSupplier = TermScorerSupplier<IRCLeafReader<IRC>, SIM>;

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
        self.term_states.was_built_for(v)
      },
      "The top-reader used to create Weight is not the same as the current reader's top-reader"
    );
    let state_supplier = self.term_states.get(context)?;
    let parent_query = &self.parent_query;

    match state_supplier {
      None => Ok(None),
      Some(v) => {
        let sim_scorer = self
          .sim_scorer
          .as_ref()
          .ok_or_else(|| LuceneError::illegal_state("term scorer is missing"))?;
        let v = TermScorerSupplier::new(
          false,
          self.term_states.clone(),
          v,
          parent_query.term.clone(),
          sim_scorer.clone(),
          self.score_mode,
        );
        Ok(Some(v))
      },
    }
  }

  fn count(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<i32>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    if !context.reader().has_deletions()? {
      if let Some(mut terms_enum) = self.get_terms_enum(context)? {
        terms_enum.doc_freq()
      } else {
        Ok(0)
      }
    } else {
      <Self as Weight<IRC, SIM>>::default_count(self, context)
    }
  }
}

impl<SIM> TermWeight<SIM>
where
  SIM: Similarity,
{
  fn build_term_scorer<LR, SS>(
    context: &LeafReaderContext<LR>,
    terms_enum: Option<&mut LRTermsEnum<LR>>,
    term: &Term,
    sim_scorer: Option<SS>,
    score_mode: ScoreMode,
    top_level_scoring_clause: bool,
  ) -> Result<TermScorerWithSimilarity<LR, SS>>
  where
    LR: LeafReader,
    SS: SimScorer,
  {
    match terms_enum {
      Some(terms_enum) => {
        let norms = if score_mode.needs_scores() {
          context.reader().get_norm_values(term.field())?
        } else {
          None
        };
        let sim_scorer = sim_scorer
          .ok_or_else(|| LuceneError::illegal_state("TermQuery similarity scorer is missing"))?;

        if score_mode == ScoreMode::TopScores {
          let v = TermScorer::from_impacts(
            terms_enum.impacts(FREQS as i32)?,
            sim_scorer,
            norms,
            top_level_scoring_clause,
          );
          Ok(match v {
            ScorerEnum2::A(scorer) => ScorerEnum4::B(scorer),
            ScorerEnum2::B(scorer) => ScorerEnum4::C(scorer),
          })
        } else {
          let flags = if score_mode.needs_scores() {
            FREQS
          } else {
            NONE
          };
          let v = ScorerEnum4::A(TermScorer::from_postings(
            terms_enum.postings_with_flags(None, flags as i32)?,
            sim_scorer,
            norms,
          ));
          Ok(v)
        }
      },
      None => {
        let v = ScorerEnum4::D(ConstantScoreScorer::from_disi(
          0.0,
          score_mode,
          EmptyDISI::default(),
        ));
        Ok(v)
      },
    }
  }
}
pub struct TermScorerSupplier<
  LR,
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> where
  SIM: Similarity,
  LR: LeafReader,
{
  top_level_scoring_clause: bool,
  term_states: Arc<TermStates>,
  prepare_state: PrepareState<LRTermsEnum<LR>>,
  term: Arc<Term>,
  sim_scorer: Arc<TermQuerySimScorer<SIM>>,
  pub(crate) score_mode: ScoreMode,
  terms_enum: Option<LRTermsEnum<LR>>,
}
impl<LR, SIM> TermScorerSupplier<LR, SIM>
where
  SIM: Similarity,
  LR: LeafReader,
{
  #[allow(clippy::too_many_arguments)]
  pub fn new(
    top_level_scoring_clause: bool,
    term_states: Arc<TermStates>,
    prepare_state: PrepareState<LRTermsEnum<LR>>,
    term: Arc<Term>,
    sim_scorer: Arc<TermQuerySimScorer<SIM>>,
    score_mode: ScoreMode,
  ) -> Self {
    Self {
      top_level_scoring_clause,
      term_states,
      prepare_state,
      term,
      sim_scorer,
      score_mode,
      terms_enum: None,
    }
  }

  pub(crate) fn get_terms_enum(&mut self, context: &LeafReaderContext<LR>) -> Result<Option<()>> {
    if self.terms_enum.is_none() {
      let state_opt = self.term_states.resolve(&mut self.prepare_state)?;
      match state_opt {
        None => return Ok(None),
        Some(s) => {
          let mut terms_enum = match context.reader().terms(self.term.field())? {
            Some(term) => term.iterator()?,
            None => {
              return Err(LuceneError::illegal_argument(format!(
                "term should exist here {}",
                self.term
              )));
            },
          };
          terms_enum.seek_exact_with_state(self.term.bytes(), s.as_ref())?;
          self.terms_enum = Some(terms_enum);
        },
      };
    }
    Ok(Some(()))
  }
}
impl<IRC, SIM> ScorerSupplier<IRC, SIM> for TermScorerSupplier<IRCLeafReader<IRC>, SIM>
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
{
  type Scorer = TermScorerEnum<IRCLeafReader<IRC>, SIM>;
  type BulkScorer =
    crate::core::search::weight::DefaultBulkScorer<TermScorerEnum<IRCLeafReader<IRC>, SIM>>;

  fn get(
    &mut self,
    _lead_cost: i64,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Self::Scorer>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    self.get_terms_enum(context)?;
    let scorer = TermWeight::<SIM>::build_term_scorer(
      context,
      self.terms_enum.as_mut(),
      self.term.as_ref(),
      Some(self.sim_scorer.clone()),
      self.score_mode,
      self.top_level_scoring_clause,
    )?;
    Ok(scorer)
  }

  fn bulk_scorer(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Option<Self::BulkScorer>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    self.get_terms_enum(context)?;
    let scorer = TermWeight::<SIM>::build_term_scorer(
      context,
      self.terms_enum.as_mut(),
      self.term.as_ref(),
      Some(self.sim_scorer.clone()),
      self.score_mode,
      self.top_level_scoring_clause,
    )?;
    Ok(Some(crate::core::search::weight::DefaultBulkScorer::new(
      scorer,
    )))
  }

  fn cost(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<i64>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let result: Result<i32> = (|| match self.get_terms_enum(context)? {
      None => Ok(0),
      Some(_) => Ok(
        self
          .terms_enum
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("term enum is missing"))?
          .doc_freq()?,
      ),
    })();
    match result {
      Ok(v) => Ok(v as i64),
      Err(e) => {
        let mut err = UncheckedIOError::new("");
        err.add_suppressed(e);
        Err(err.into())
      },
    }
  }

  fn set_top_level_scoring_clause(&mut self) -> Result<()> {
    self.top_level_scoring_clause = true;
    Ok(())
  }
}

pub struct SimScorerImpl;
impl SimScorer for SimScorerImpl {
  fn score(&self, _freq: f32, _norm: i64) -> f32 {
    0f32
  }
}
pub(crate) type TermQuerySimScorer<
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> = SimScorerEnum2<SimilaritySimScorer<SIM>, SimScorerImpl>;

pub type TermScorerEnum<
  LR,
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> = TermScorerWithSimilarity<LR, Arc<TermQuerySimScorer<SIM>>>;

type TermScorerWithSimilarity<LR, SS> = ScorerEnum4<
  TermScorer<PostingsTermState<LRPosting<LR>, SS>, LRNormNumericDocValues<LR>>,
  TermScorer<ImpactsTermState<LRImpactsEnum<LR>, SS>, LRNormNumericDocValues<LR>>,
  TermScorer<
    crate::core::search::impacts_disi::SourceImpactsDISI<LRImpactsEnum<LR>, SS>,
    LRNormNumericDocValues<LR>,
  >,
  ConstantScoreDisiScorer<EmptyDISI>,
>;

impl crate::core::util::accountable::Accountable for TermQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED)
  }
}

impl<SIM> TermWeight<SIM>
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
    let Some(mut terms_enum) = self.get_terms_enum(context)? else {
      return Ok(None);
    };
    let mut postings = terms_enum.postings_with_flags(None, OFFSETS as i32)?;
    if postings.advance(doc)? != doc {
      return Ok(None);
    }
    Ok(Some(
      Box::new(TermMatchesIterator::new(
        postings,
        Arc::new(self.parent_query.clone().into()),
      )?)
      .into(),
    ))
  }
}
