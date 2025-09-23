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
use crate::core::index::dummy::dummy_term_state_type::DummyTermState;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::{
    LeafReader, LeafReaderImpactsEnum, LeafReaderNumericDocValues, LeafReaderPosting,
    LeafReaderTermState, LeafReaderTermsEnum,
};
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::term::Term;
use crate::core::index::term_state::TermState;
use crate::core::index::term_states::{PrepareState, TermStateTerm, TermStates, build};
use crate::core::index::terms_enum::TermsEnum;
use crate::core::search::collection_statistics::CollectionStatistics;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::dummy::dummy_bulk_scorer::DummyBulkScorer;
use crate::core::search::dummy::dummy_matches::DummyMatches;
use crate::core::search::explanation::Explanation;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::query::Query;
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::Scorer;
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::similarities_impl::similarities::{
    Either2SimScorer, SimScorer, Similarity,
};
use crate::core::search::term_scorer::TermScorer;
use crate::core::search::term_statistics::TermStatistics;
use crate::core::search::weight::Weight;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::fmt::{Debug, Display, Formatter};
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;

pub struct TermQuery<TS>
where
    TS: TermState,
{
    term: Arc<Term>,
    per_reader_term_state: Option<TermStates<TS>>,
}
impl TermQuery<DummyTermState> {
    pub fn new<T>(term: T) -> Self
    where
        T: Into<Arc<Term>>,
    {
        Self {
            term: term.into(),
            per_reader_term_state: None,
        }
    }
}
impl<TS> TermQuery<TS>
where
    TS: TermState,
{
    pub fn new_with_states<T>(term: T, states: TermStates<TS>) -> Self
    where
        T: Into<Arc<Term>>,
    {
        Self {
            term: term.into(),
            per_reader_term_state: Option::from(states),
        }
    }
    pub fn get_term_state(&self) -> Option<&TermStates<TS>> {
        self.per_reader_term_state.as_ref()
    }
}

impl<TS> PartialEq<Self> for TermQuery<TS>
where
    TS: TermState,
{
    fn eq(&self, other: &Self) -> bool {
        self.term == other.term
    }
}

impl<TS> Hash for TermQuery<TS>
where
    TS: TermState,
{
    fn hash<H: Hasher>(&self, state: &mut H) {
        // TODO
        self.term.hash(state);
    }
}
impl<TS> Eq for TermQuery<TS> where TS: TermState {}

impl<TS> Debug for TermQuery<TS>
where
    TS: TermState,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        todo!()
    }
}

impl<TS> Query for TermQuery<TS>
where
    TS: TermState,
{
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

    type Weight<S, LR>
        = TermWeight<S, TS, LR>
    where
        S: Similarity,
        LR: LeafReader;

    fn crate_weight<IRC, S>(
        mut self,
        search: &IndexSearcher<IRC, S>,
        score_mod: &ScoreMode,
        boost: f32,
    ) -> Result<Self::Weight<S, IRC::LeafReader>>
    where
        IRC: IndexReaderContext,
        S: Similarity,
        Self: Sized,
    {
        let context = search.get_top_reader_context();
        let term_state = if self.per_reader_term_state.is_none()
            || !self
                .per_reader_term_state
                .as_ref()
                .unwrap()
                .was_built_for(context)
        {
            TermStatesENum::A(build(search, self.term.clone(), score_mod.needs_scores())?)
        } else {
            TermStatesENum::B(self.per_reader_term_state.take().unwrap())
        };
        TermWeight::new(search, *score_mod, boost, Some(term_state), Rc::new(self))
    }

    type Query = TermQuery<TS>;

    fn visit<QV>(&self, _visitor: &QV)
    where
        QV: QueryVisitor,
    {
        todo!()
    }
}

impl<TS> Display for TermQuery<TS>
where
    TS: TermState,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_string(""))
    }
}

pub struct TermWeight<S, TS, LR>
where
    S: Similarity,
    TS: TermState,
    LR: LeafReader,
{
    similarity: Rc<S>,
    sim_scorer: Option<Rc<TermQuerySimScorer<S::SimScorer>>>,
    term_states: Option<TermStatesENum<TS, LR>>,
    score_mode: ScoreMode,
    parent_query: Rc<TermQuery<TS>>,
}
impl<S, TS, LR> TermWeight<S, TS, LR>
where
    S: Similarity,
    TS: TermState,
    LR: LeafReader,
{
    pub fn new<IRC>(
        searcher: &IndexSearcher<IRC, S>,
        score_mode: ScoreMode,
        boost: f32,
        term_states: Option<TermStatesENum<TS, LR>>,
        query: Rc<TermQuery<TS>>,
    ) -> Result<Self>
    where
        IRC: IndexReaderContext,
        LR: LeafReader,
    {
        if score_mode.needs_scores() && term_states.is_none() {
            return Err(LuceneError::illegal_argument(
                "termStates are required when scores are needed",
            ));
        }

        let similarity = searcher.get_similarity();

        // collectionStats 和 termStats
        let ts = term_states.as_ref().unwrap();
        let (collection_stats, term_stats) = if score_mode.needs_scores() {
            let collection_stats = searcher.collection_statistics(query.term.field());
            let term_stats = if ts.doc_freq()? > 0 {
                Some(searcher.term_statistics(
                    query.term.clone(),
                    ts.doc_freq()?,
                    ts.total_term_freq()?,
                )?)
            } else {
                None
            };
            (collection_stats, term_stats)
        } else {
            // we do not need the actual stats, use fake stats with docFreq=maxDoc=ttf=1
            let collection_stats =
                CollectionStatistics::new(query.term.field().to_string(), 1, 1, 1, 1)?;
            let term_stats = Some(TermStatistics::new(query.term.clone(), 1, 1)?);
            (collection_stats, term_stats)
        };

        // Assigning a dummy simScorer in case score is not needed to avoid unnecessary float[]
        // allocations in case default BM25Scorer is used.
        // See: https://github.com/apache/lucene/issues/12297
        let sim_scorer = if let Some(term_stats) = term_stats {
            if score_mode.needs_scores() {
                Some(Rc::new(TermQuerySimScorer::A(similarity.scorer(
                    boost,
                    &collection_stats,
                    &[term_stats],
                ))))
            } else {
                Some(Rc::new(TermQuerySimScorer::B(SimScorerImpl)))
            }
        } else {
            None
        };

        Ok(Self {
            similarity,
            sim_scorer,
            term_states,
            score_mode,
            parent_query: query,
        })
    }
    fn get_terms_enum<LR1>(
        &self,
        _context: &LeafReaderContext<LR1>,
    ) -> Result<Option<LeafReaderTermsEnum<LR1>>>
    where
        LR1: LeafReader,
    {
        todo!()
    }
}

impl<S, TS, LR> SegmentCacheable for TermWeight<S, TS, LR>
where
    S: Similarity,
    TS: TermState,
    LR: LeafReader,
{
    type LeafReader = LR;

    fn is_cacheable(&self, _ctx: &LeafReaderContext<Self::LeafReader>) -> bool {
        true
    }
}

impl<S, TS, LR> Weight for TermWeight<S, TS, LR>
where
    S: Similarity,
    TS: TermState,
    LR: LeafReader,
{
    type Matches = DummyMatches;

    fn matches(
        &mut self,
        _context: &LeafReaderContext<LR>,
        _doc: i32,
    ) -> Result<Option<Self::Matches>> {
        todo!()
    }

    fn explain(&mut self, context: &LeafReaderContext<LR>, doc: i32) -> Result<Explanation> {
        let mut scorer_opt = self.scorer(context)?;
        if let Some(scorer) = scorer_opt.as_mut() {
            let new_doc = scorer.iterator().advance(doc)?;
            if new_doc == doc {
                let freq = scorer.freq()?;

                let mut norm: i64 = 1;
                if let Some(mut norms) = context
                    .reader()
                    .get_norm_values(&self.parent_query.term.field)?
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
                        "weight({} in {}) [{}], result of:",
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

    type Query = TermQuery<TS>;

    fn get_query(&self) -> &Self::Query {
        todo!()
    }

    type ScorerSupplier = ScorerSupplierImpl<LR, S>;

    fn scorer_supplier(
        &mut self,
        _context: &LeafReaderContext<LR>,
    ) -> Result<Option<Self::ScorerSupplier>> {
        todo!()
    }

    fn count(&self, context: &LeafReaderContext<LR>) -> Result<i32> {
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

pub struct ScorerSupplierImpl<LR, S>
where
    LR: LeafReader,
    S: Similarity,
{
    terms_enum: Option<LeafReaderTermsEnum<LR>>,
    top_level_scoring_clause: bool,
    term_states: TermStates<LeafReaderTermState<LR>>,
    prepare_state: Option<PrepareState<LR>>,
    context: Rc<LR>,
    term: Arc<Term>,
    sim_scorer: Rc<TermQuerySimScorer<S::SimScorer>>,
}
impl<LR, S> ScorerSupplierImpl<LR, S>
where
    LR: LeafReader,
    S: Similarity,
{
    pub(crate) fn get_terms_enum(&mut self) -> Result<Option<&mut LeafReaderTermsEnum<LR>>> {
        // if self.terms_enum.is_none() {
        //     let state_opt = self.term_states.resolve(self.prepare_state.take().unwrap())?;
        //     let state = match state_opt {
        //         None => return Ok(None),
        //         Some(s) => s,
        //     };
        //
        //     let mut te = self.context
        //         .terms(self.term.field())?
        //         .ok_or_else(|| LuceneError::IllegalState("missing terms".into()))?
        //         .iterator()?;
        //
        //     te.seek_exact_with_state(self.term.bytes(), state.as_ref())?;
        //
        //     self.terms_enum = Some(te);
        // }
        // Ok(self.terms_enum.as_mut())
        todo!()
    }
}
impl<LR, S> ScorerSupplier for ScorerSupplierImpl<LR, S>
where
    LR: LeafReader,
    S: Similarity,
{
    type Scorer = TermScorer<
        LeafReaderPosting<LR>,
        TermQuerySimScorer<S::SimScorer>,
        LeafReaderNumericDocValues<LR>,
        LeafReaderImpactsEnum<LR>,
    >;
    type BulkScorer = DummyBulkScorer;

    fn get(&self, lead_cost: i64) -> Result<Option<Self::Scorer>> {
        todo!()
    }

    fn cost(&mut self) -> Result<i64> {
        let result: Result<i32> = (|| match self.get_terms_enum()? {
            None => Ok(0),
            Some(te) => Ok(te.doc_freq()?),
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

pub enum TermStatesENum<TS, LR>
where
    TS: TermState,
    LR: LeafReader,
{
    A(TermStates<TermStateTerm<LR>>),
    B(TermStates<TS>),
}
impl<TS, LR> TermStatesENum<TS, LR>
where
    TS: TermState,
    LR: LeafReader,
{
    pub fn doc_freq(&self) -> Result<i32> {
        match self {
            TermStatesENum::A(v) => v.doc_freq(),
            TermStatesENum::B(v) => v.doc_freq(),
        }
    }
    pub fn total_term_freq(&self) -> Result<i64> {
        match self {
            TermStatesENum::A(v) => v.total_term_freq(),
            TermStatesENum::B(v) => v.total_term_freq(),
        }
    }
    pub fn was_built_for<IRC>(&self, ctx: &IRC) -> bool
    where
        IRC: IndexReaderContext,
        LR: LeafReader,
    {
        match self {
            TermStatesENum::A(v) => v.was_built_for(ctx),
            TermStatesENum::B(v) => v.was_built_for(ctx),
        }
    }
}
