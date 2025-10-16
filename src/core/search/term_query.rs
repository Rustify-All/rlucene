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
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::{IRCTermState, IndexReaderContext};
use crate::core::index::leaf_reader::{
    LRImpactsEnum, LRNormNumericDocValues, LRPosting, LRTermsEnum, LeafReader,
};
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::postings_enum::{FREQS, NONE};
use crate::core::index::query_timeout::QueryTimeout;
use crate::core::index::term::Term;
use crate::core::index::term_states::{EitherEmptyTermState, PrepareState, TermStates, build};
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::search::QueryCache;
use crate::core::search::collection_statistics::CollectionStatistics;
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, EmptyDISI};
use crate::core::search::dummy::dummy_matches::DummyMatches;
use crate::core::search::dummy::dummy_two_phase_iterator::DummyTwoPhaseIterator;
use crate::core::search::explanation::Explanation;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::query::{Query, QueryBase};
use crate::core::search::query_caching_policy::QueryCachingPolicy;
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::{Either2Scorer, Scorer};
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::similarities_impl::similarities::{
    Either2SimScorer, SimScorer, Similarity,
};
use crate::core::search::term_scorer::TermScorer;
use crate::core::search::term_statistics::TermStatistics;
use crate::core::search::weight::{DefaultBulkScorer, Weight};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use parking_lot::Mutex;
use std::fmt::{Debug, Formatter};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// A Query that matches documents containing a term. This may be combined with other terms with a [`BooleanQuery`](crate::core::search::boolean_query::BooleanQuery).
pub struct TermQuery {
    term: Arc<Term>,
}
impl TermQuery {
    pub fn new<T>(term: T) -> Self
    where
        T: Into<Arc<Term>>,
    {
        Self { term: term.into() }
    }
    /// Expert: constructs a [`TermQuery`] and returns it along with the provided [`TermStates`].
    pub fn new_with_states<T, IRC>(
        term: T,
        states: TermStates<IRCTermState<IRC>>,
    ) -> (Self, TermStates<IRCTermState<IRC>>)
    where
        IRC: IndexReaderContext,
        T: Into<Arc<Term>>,
    {
        (Self { term: term.into() }, states)
    }
    pub fn get_term(&self) -> Arc<Term> {
        self.term.clone()
    }
}

impl PartialEq for TermQuery {
    fn eq(&self, other: &Self) -> bool {
        self.term == other.term
    }
}

impl Hash for TermQuery {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.term.hash(state);
    }
}
impl Eq for TermQuery {}

impl Debug for TermQuery {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_string(""))
    }
}

impl QueryBase for TermQuery {
    fn as_string(&self, field: &str) -> String {
        let mut buffer = String::new();
        if self.term.field != field {
            buffer.push_str(&self.term.field);
            buffer.push(':');
        }
        match self.term.text() {
            Ok(text) => {
                buffer.push_str(&text);
            },
            Err(_) => {
                buffer.push_str("<?>");
            },
        }
        buffer
    }

    type Weight<S, IRC, QCP, QC>
        = TermWeight<S, IRC>
    where
        S: Similarity,
        IRC: IndexReaderContext,
        QCP: QueryCachingPolicy,
        QC: QueryCache;

    fn create_weight<S, IRC, QT, QCP, QC>(
        self,
        searcher: &IndexSearcher<IRC, S, QT, QCP, QC>,
        score_mode: &ScoreMode,
        boost: f32,
        per_reader_term_state: Option<TermStates<IRCTermState<IRC>>>,
    ) -> Result<Self::Weight<S, IRC, QCP, QC>>
    where
        IRC: IndexReaderContext,
        S: Similarity,
        QT: QueryTimeout,
        QCP: QueryCachingPolicy,
        QC: QueryCache,
        Self: Sized,
    {
        let context = searcher.get_top_reader_context();
        let term_state = match per_reader_term_state {
            Some(states) if states.was_built_for(context) => states,
            _ => build(searcher, self.term.clone(), score_mode.needs_scores())?,
        };
        TermWeight::new(searcher, *score_mode, boost, term_state, self)
    }

    type RewriteQuery = TermQuery;

    fn visit<QV>(&self, _visitor: &QV)
    where
        QV: QueryVisitor,
    {
        todo!()
    }
}
pub struct TermWeight<S, IRC>
where
    S: Similarity,
    IRC: IndexReaderContext,
{
    similarity: Arc<S>,
    sim_scorer: Option<Arc<TermQuerySimScorer<S::SimScorer>>>,
    term_states: Arc<Mutex<TermStates<IRCTermState<IRC>>>>,
    score_mode: ScoreMode,
    parent_query: Arc<Query>,
}
impl<S, IRC> TermWeight<S, IRC>
where
    S: Similarity,
    IRC: IndexReaderContext,
{
    pub fn new<I, QT, QCP, QC>(
        searcher: &IndexSearcher<I, S, QT, QCP, QC>,
        score_mode: ScoreMode,
        boost: f32,
        term_states: TermStates<IRCTermState<IRC>>,
        query: TermQuery,
    ) -> Result<Self>
    where
        I: IndexReaderContext,
        QT: QueryTimeout,
        QCP: QueryCachingPolicy,
        QC: QueryCache,
    {
        let similarity = searcher.get_similarity();

        let (collection_stats, term_stats) = if score_mode.needs_scores() {
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
            (collection_stats, term_stats)
        } else {
            // we do not need the actual stats, use fake stats with docFreq=maxDoc=ttf=1
            let collection_stats = Some(CollectionStatistics::new(
                query.term.field().to_string(),
                1,
                1,
                1,
                1,
            )?);
            let term_stats = Some(TermStatistics::new(query.term.clone(), 1, 1)?);
            (collection_stats, term_stats)
        };

        // Assigning a dummy simScorer in case score is not needed to avoid unnecessary float[]
        // allocations in case default BM25Scorer is used.
        // See: https://github.com/apache/lucene/issues/12297
        let sim_scorer = if let Some(term_stats) = term_stats {
            debug_assert!(collection_stats.is_some());
            if score_mode.needs_scores() {
                Some(Arc::new(TermQuerySimScorer::A(similarity.scorer(
                    boost,
                    collection_stats.as_ref().unwrap(),
                    &[term_stats],
                ))))
            } else {
                Some(Arc::new(TermQuerySimScorer::B(SimScorerImpl)))
            }
        } else {
            None
        };

        Ok(Self {
            similarity,
            sim_scorer,
            term_states: Arc::new(Mutex::new(term_states)),
            score_mode,
            parent_query: Arc::new(query.into()),
        })
    }
    /// Returns a TermsEnum positioned at this weights Term or None if the term does not exist in the given context
    fn get_terms_enum(
        &self,
        context: &LeafReaderContext<IRC::LeafReader>,
    ) -> Result<Option<LRTermsEnum<IRC::LeafReader>>> {
        // TODO:
        // debug_assert!(
        //     term_states.was_built_for(&context.get_top_level_context()),
        //     "The top-reader used to create Weight is not the same as the current reader's top-reader"
        // );
        let mut term_states = self.term_states.lock();
        let supplier = term_states.get(context)?;

        let state = match supplier {
            Some(s) => term_states.resolve(s)?,
            None => None,
        };
        let Query::Term(parent_query) = self.parent_query.as_ref() else {
            unreachable!("should never happen");
        };

        if state.is_none() {
            debug_assert!(
                !self.term_not_in_reader(context.reader(), parent_query.term.as_ref())?,
                "no termstate found but term exists in reader"
            );
            return Ok(None);
        }

        let state = state.unwrap();
        let mut terms_enum = context
            .reader()
            .terms(parent_query.term.field())?
            .as_ref()
            .unwrap()
            .iterator()?;
        match state.as_ref() {
            EitherEmptyTermState::A(s) => {
                terms_enum.seek_exact_with_state(parent_query.term.bytes(), s)?;
                Ok(Some(terms_enum))
            },
            EitherEmptyTermState::B(_) => Err(LuceneError::illegal_argument(
                "should never get empty term state here",
            )),
        }
    }
    fn term_not_in_reader<LR>(&self, reader: &LR, term: &Term) -> Result<bool>
    where
        LR: LeafReader,
    {
        Ok(LeafReader::doc_freq(reader, term)? == 0)
    }
}

impl<S, IRC> SegmentCacheable<IRC::LeafReader> for TermWeight<S, IRC>
where
    S: Similarity,
    IRC: IndexReaderContext,
{
    fn is_cacheable(&self, _ctx: &LeafReaderContext<IRC::LeafReader>) -> bool {
        true
    }
}

impl<S, IRC> Weight<IRC::LeafReader> for TermWeight<S, IRC>
where
    S: Similarity,
    IRC: IndexReaderContext,
{
    type Matches = DummyMatches;

    fn matches(
        &self,
        _context: &LeafReaderContext<IRC::LeafReader>,
        _doc: i32,
    ) -> Result<Option<Self::Matches>> {
        todo!()
    }

    fn explain(
        &self,
        context: &LeafReaderContext<IRC::LeafReader>,
        doc: i32,
    ) -> Result<Explanation> {
        let mut scorer_opt = self.scorer(context)?;
        if let Some(scorer) = scorer_opt.as_mut() {
            let new_doc = scorer.iterator().advance(doc)?;
            if new_doc == doc {
                let freq = scorer.freq()?;

                let mut norm: i64 = 1;
                let Query::Term(parent_query) = self.parent_query.as_ref() else {
                    unreachable!("should never happen");
                };
                if let Some(mut norms) =
                    context.reader().get_norm_values(&parent_query.term.field)?
                    && norms.advance_exact(doc)?
                {
                    norm = norms.long_value()?;
                }

                let freq_explanation = Explanation::match_(
                    freq,
                    "freq, occurrences of term within document".to_string(),
                    vec![],
                );

                let score_explanation = self
                    .sim_scorer
                    .as_ref()
                    .unwrap()
                    .explain(freq_explanation, norm);

                return Ok(Explanation::match_(
                    score_explanation.value,
                    format!(
                        "weight({:?} in {}) [{}], result of:",
                        self.get_query(),
                        doc,
                        self.similarity,
                    ),
                    vec![score_explanation],
                ));
            }
        }

        Ok(Explanation::no_match(
            "no matching term".to_string(),
            vec![],
        ))
    }

    fn get_query(&self) -> Arc<Query> {
        self.parent_query.clone()
    }

    type ScorerSupplier = TermScorerSupplier<IRC, S>;

    fn scorer_supplier(
        &self,
        context: &LeafReaderContext<IRC::LeafReader>,
    ) -> Result<Option<Self::ScorerSupplier>> {
        // TODO
        // debug_assert!(self.term_states.is_some() || self.term_states.as_ref().unwrap().was_built_for(&_context.get_top_level_context()),);
        //     "The top-reader used to create Weight is not the same as the current reader's top-reader"
        // );
        let state_supplier = self.term_states.lock().get(context)?;
        let Query::Term(parent_query) = self.parent_query.as_ref() else {
            unreachable!("should never happen");
        };
        let term_enum = context
            .reader()
            .terms(parent_query.term.field())?
            .as_mut()
            .unwrap()
            .iterator()?;
        debug_assert!(self.sim_scorer.is_some());
        match state_supplier {
            None => Ok(None),
            Some(v) => Ok(Some(TermScorerSupplier::new(
                false,
                self.term_states.clone(),
                v,
                parent_query.term.clone(),
                self.sim_scorer.as_ref().unwrap().clone(),
                self.score_mode,
                term_enum,
            ))),
        }
    }

    fn count(&self, context: &LeafReaderContext<IRC::LeafReader>) -> Result<i32> {
        if !context.reader().has_deletions()? {
            if let Some(mut terms_enum) = self.get_terms_enum(context)? {
                terms_enum.doc_freq()
            } else {
                Ok(0)
            }
        } else {
            self.default_count(context)
        }
    }
}

pub struct TermScorerSupplier<IRC, S>
where
    IRC: IndexReaderContext,
    S: Similarity,
{
    top_level_scoring_clause: bool,
    term_states: Arc<Mutex<TermStates<IRCTermState<IRC>>>>,
    // wrap with Option to easily take it when needed
    prepare_state: Option<PrepareState<IRC::LeafReader>>,
    term: Arc<Term>,
    sim_scorer: Arc<TermQuerySimScorer<S::SimScorer>>,
    score_mode: ScoreMode,
    terms_enum: LRTermsEnum<IRC::LeafReader>,
    init_terms_enum: bool,
}
impl<IRC, S> TermScorerSupplier<IRC, S>
where
    IRC: IndexReaderContext,
    S: Similarity,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        top_level_scoring_clause: bool,
        term_states: Arc<Mutex<TermStates<IRCTermState<IRC>>>>,
        prepare_state: PrepareState<IRC::LeafReader>,
        term: Arc<Term>,
        sim_scorer: Arc<TermQuerySimScorer<S::SimScorer>>,
        score_mode: ScoreMode,
        terms_enum: LRTermsEnum<IRC::LeafReader>,
    ) -> Self {
        Self {
            top_level_scoring_clause,
            term_states,
            prepare_state: Some(prepare_state),
            term,
            sim_scorer,
            score_mode,
            terms_enum,
            init_terms_enum: false,
        }
    }

    pub(crate) fn get_terms_enum(&mut self) -> Result<Option<()>> {
        if !self.init_terms_enum {
            self.init_terms_enum = true;
            let state_opt = self
                .term_states
                .lock()
                .resolve(self.prepare_state.take().unwrap())?;
            match state_opt {
                None => return Ok(None),
                Some(s) => match s.as_ref() {
                    EitherEmptyTermState::A(s) => {
                        self.terms_enum
                            .seek_exact_with_state(self.term.bytes(), s)?;
                    },
                    EitherEmptyTermState::B(_) => {
                        return Err(LuceneError::illegal_argument(
                            "should never get empty term state here",
                        ));
                    },
                },
            };
        }
        Ok(Some(()))
    }
}
impl<IRC, S> ScorerSupplier<IRC::LeafReader> for TermScorerSupplier<IRC, S>
where
    IRC: IndexReaderContext,
    S: Similarity,
{
    type Scorer = TermScorerEnum<IRC::LeafReader, S::SimScorer, EmptyDISI, DummyTwoPhaseIterator>;
    type BulkScorer = DefaultBulkScorer<Self::Scorer>;

    fn get(
        &mut self,
        _lead_cost: i64,
        context: &LeafReaderContext<IRC::LeafReader>,
    ) -> Result<Option<Self::Scorer>> {
        match self.get_terms_enum()? {
            Some(_) => {
                let norms = if self.score_mode.needs_scores() {
                    context.reader().get_norm_values(&self.term.field)?
                } else {
                    None
                };

                if self.score_mode == ScoreMode::TopScores {
                    Ok(Some(TermScorerEnum::<
                        IRC::LeafReader,
                        S::SimScorer,
                        EmptyDISI,
                        DummyTwoPhaseIterator,
                    >::A(TermScorer::new(
                        self.terms_enum.impacts(FREQS as i32)?,
                        self.sim_scorer.clone(),
                        norms,
                        self.top_level_scoring_clause,
                    ))))
                } else {
                    let flags = if self.score_mode.needs_scores() {
                        FREQS
                    } else {
                        NONE
                    };

                    Ok(Some(TermScorerEnum::<
                        IRC::LeafReader,
                        S::SimScorer,
                        EmptyDISI,
                        DummyTwoPhaseIterator,
                    >::A(TermScorer::with_postings(
                        self.terms_enum.postings_with_flags(None, flags as i32)?,
                        self.sim_scorer.clone(),
                        norms,
                    ))))
                }
            },
            None => Ok(Some(TermScorerEnum::<
                IRC::LeafReader,
                S::SimScorer,
                EmptyDISI,
                DummyTwoPhaseIterator,
            >::B(ConstantScoreScorer::with_disi(
                0.0,
                self.score_mode,
                EmptyDISI::default(),
            )))),
        }
    }

    fn bulk_scorer(
        &mut self,
        context: &LeafReaderContext<IRC::LeafReader>,
    ) -> Result<Self::BulkScorer> {
        self.default_bulk_scorer(context)
    }

    fn cost(&mut self) -> Result<i64> {
        let result: Result<i32> = (|| match self.get_terms_enum()? {
            None => Ok(0),
            Some(_) => Ok(self.terms_enum.doc_freq()?),
        })();
        match result {
            Ok(v) => Ok(v as i64),
            Err(e) => Err(LuceneError::unchecked_io_error(e)),
        }
    }
}

pub struct SimScorerImpl;
impl SimScorer for SimScorerImpl {
    fn score(&self, _freq: f32, _norm: i64) -> f32 {
        0f32
    }
}
pub(crate) type TermQuerySimScorer<S> = Either2SimScorer<S, SimScorerImpl>;

pub type TermScorerEnum<LR, SS, DISI, TPI> = Either2Scorer<
    TermScorer<
        LRPosting<LR>,
        TermQuerySimScorer<SS>,
        LRNormNumericDocValues<LR>,
        LRImpactsEnum<LR>,
    >,
    ConstantScoreScorer<DISI, TPI>,
>;
