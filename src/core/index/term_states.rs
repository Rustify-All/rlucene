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
use crate::core::index::base_terms_enum::TermStateImpl1;
use crate::core::index::dummy::dummy_term_state_type::DummyTermState;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::{LeafReader, LeafReaderTermState};
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::term::Term;
use crate::core::index::term_state::{Either2TermState, TermState};
use crate::core::index::terms::{Terms, terms_util};
use crate::core::index::terms_enum::TermsEnum;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

/// Maintains an [`IndexReader`](crate::core::index::index_reader::IndexReader) [`TermState`] view over [`IndexReader`](crate::core::index::index_reader::IndexReader) instances
/// containing a single term. The [`TermStates`] doesn't track if the given [`TermState`]
/// objects are valid, neither if the [`TermState`] instances refer to the same terms in the
/// associated readers.
pub struct TermStates<TS>
where
    TS: TermState,
{
    top_reader_context_identity: Arc<()>,
    states: Vec<Option<Arc<EitherEmptyTermState<TS>>>>,
    term: Option<Arc<Term>>,
    doc_freq: i32,
    total_term_freq: i64,
}
impl<TS> TermStates<TS>
where
    TS: TermState,
{
    pub fn new<IRC, LR>(term: Option<Arc<Term>>, context: &IRC) -> Result<Self>
    where
        IRC: IndexReaderContext<LR>,
    {
        debug_assert!(context.base().is_top_level);
        let mut states = Vec::new();
        let num_leaves = context.leaves()?.len();
        for _ in 0..num_leaves {
            states.push(None)
        }
        Ok(TermStates {
            top_reader_context_identity: context.base().identity.clone(),
            doc_freq: 0,
            total_term_freq: 0,
            states,
            term,
        })
    }

    pub fn new_empty<IRC, LR>(context: &IRC) -> Result<Self>
    where
        IRC: IndexReaderContext<LR>,
    {
        Self::new(None, context)
    }
    pub fn was_built_for<IRC, LR>(&self, context: &IRC) -> bool
    where
        IRC: IndexReaderContext<LR>,
    {
        Arc::ptr_eq(&self.top_reader_context_identity, &context.base().identity)
    }
    pub fn with_state_and_stats<IRC, LR>(
        context: &IRC,
        state: TS,
        ord: usize,
        doc_freq: i32,
        total_term_freq: i64,
    ) -> Result<Self>
    where
        IRC: IndexReaderContext<LR>,
    {
        let mut ts = TermStates::new_empty(context)?;
        ts.register_with_stats(state, ord, doc_freq, total_term_freq);
        Ok(ts)
    }

    /// Clears the TermStates internal state and removes all registered TermStates
    pub fn clear(&mut self) {
        self.doc_freq = 0;
        self.total_term_freq = 0;

        for slot in &mut self.states {
            *slot = None;
        }
    }
    /// Registers and associates a TermState with an leaf ordinal.
    /// The leaf ordinal should be derived from a IndexReaderContext's leaf ord.
    pub fn register_with_stats(
        &mut self,
        state: TS,
        ord: usize,
        doc_freq: i32,
        total_term_freq: i64,
    ) {
        self.register(state, ord);
        self.accumulate_statistics(doc_freq, total_term_freq);
    }
    /// Expert: Registers and associates a [`TermState`] with a leaf ordinal.
    /// The leaf ordinal should be derived from an [`IndexReaderContext`]'s leaf ord.
    /// Unlike [`register`](Self::register_with_stats), this method does **not** update term statistics.
    pub fn register(&mut self, state: TS, ord: usize) {
        debug_assert!(ord < self.states.len(), "ord {} out of bounds", ord);
        // for clone
        self.states[ord] = Some(Arc::new(EitherEmptyTermState::A(state)));
    }
    /// Expert: Accumulate term statistics.
    pub fn accumulate_statistics(&mut self, doc_freq: i32, total_term_freq: i64) {
        debug_assert!(doc_freq >= 0);
        debug_assert!(total_term_freq >= 0);
        debug_assert!(
            (doc_freq as i64) <= total_term_freq,
            "doc_freq must not exceed total_term_freq"
        );
        self.doc_freq += doc_freq;
        self.total_term_freq += total_term_freq;
    }
    /// Returns a [`PrepareState`] for a [`TermState`] for the given [`LeafReaderContext`].
    /// This may return `None` if some cheap checks help figure out that this term
    /// doesn't exist in this leaf. The [`Supplier`] may then also return `None`
    /// if the term doesn't exist.
    ///
    /// Calling this method typically schedules some I/O in the background, so it is
    /// recommended to retrieve [`PrepareState`]s across all required terms first before
    /// calling [`resolve`] on all [`PrepareState`]s so that the I/O for these terms
    /// can be performed in parallel.
    ///
    /// # Arguments
    /// * `ctx` - the [`LeafReaderContext`] to get the [`TermState`] for.
    ///
    /// # Returns
    /// A [`PrepareState`] for a [`TermState`].
    pub fn get<LR>(
        &mut self,
        ctx_ord: usize,
        ctx: &LeafReaderContext<LR>,
    ) -> Result<Option<PrepareState<LR>>>
    where
        LR: LeafReader,
    {
        debug_assert!(ctx_ord < self.states.len());

        if self.term.is_none() {
            return Ok(if self.states[ctx_ord].is_none() {
                None
            } else {
                Some(PrepareState::Ready(ctx_ord))
            });
        }

        if self.states[ctx_ord].is_none() {
            let terms_opt = ctx.reader().terms(self.term.as_ref().unwrap().field())?;
            if terms_opt.is_none() {
                self.states[ctx_ord] = Some(Arc::new(Either2TermState::B(EmptyTermState)));
                return Ok(None);
            }

            let mut te = terms_opt.unwrap().iterator()?;
            let io_boolean_supplier = te.prepare_seek_exact(self.term.as_ref().unwrap().bytes())?;
            if io_boolean_supplier.is_none() {
                self.states[ctx_ord] = Some(Arc::new(Either2TermState::B(EmptyTermState)));
                return Ok(None);
            }
            return Ok(Some(PrepareState::Pending(
                self.term.as_ref().unwrap().clone(),
                ctx_ord,
                te,
            )));
        }
        let state = self.states[ctx_ord].as_ref().unwrap();
        if matches!(state.as_ref(), EitherEmptyTermState::B(_)) {
            Ok(None)
        } else {
            Ok(Some(PrepareState::Ready(ctx_ord)))
        }
    }
    pub fn resolve<LR>(
        &mut self,
        state: PrepareState<LR>,
    ) -> Result<Option<Arc<EitherEmptyTermState<TS>>>>
    where
        LR: LeafReader,
        <LR::Terms as Terms>::TermsEnum: TermsEnum<TermState = TS>,
    {
        match state {
            PrepareState::Ready(ord) => Ok(self.states[ord].clone()),

            PrepareState::Pending(term, ord, mut te) => {
                if self.states[ord - 1].as_ref().is_none() {
                    if te.get_prepare_seek_exact_status(term.bytes())? {
                        let state = te.term_state()?;
                        self.states[ord] = Some(Arc::new(Either2TermState::A(state)))
                    } else {
                        self.states[ord] = Some(Arc::new(Either2TermState::B(EmptyTermState)))
                    }
                }
                let state = self.states[ord].as_ref().unwrap();
                if matches!(state.as_ref(), EitherEmptyTermState::B(_)) {
                    Ok(None)
                } else {
                    Ok(Some(state.clone()))
                }
            },
        }
    }

    /// Returns the accumulated document frequency of all [`TermState`] instances
    /// passed to [`register(TermState, int, int, long)`].
    ///
    /// # Returns
    /// The accumulated document frequency of all [`TermState`] instances passed
    /// to [`register(TermState, int, int, long)`].
    pub fn doc_freq(&self) -> Result<i32> {
        if self.term.is_some() {
            return Err(LuceneError::illegal_state(
                "Cannot call doc_freq() when needsStats=false",
            ));
        }
        Ok(self.doc_freq)
    }
    /// Returns the accumulated term frequency of all [`TermState`] instances
    /// passed to [`register`](Self::register_with_stats).
    ///
    /// # Returns
    /// The accumulated term frequency of all [`TermState`] instances passed
    /// to [`register`](Self::register_with_stats).
    pub fn total_term_freq(&self) -> Result<i64> {
        if self.term.is_some() {
            return Err(LuceneError::illegal_state(
                "Cannot call total_term_freq() when needsStats=false",
            ));
        }
        Ok(self.total_term_freq)
    }
}
impl<TS> Display for TermStates<TS>
where
    TS: TermState,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "TermStates")?;
        for state in &self.states {
            match state.as_ref() {
                None => writeln!(f, "  null")?,
                Some(s) => writeln!(f, "  {}", s)?,
            }
        }
        Ok(())
    }
}

pub type EitherEmptyTermState<TS> = Either2TermState<TS, EmptyTermState>;

pub struct EmptyTermState;

impl Display for EmptyTermState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", std::any::type_name::<Self>())
    }
}

impl Clone for EmptyTermState {
    fn clone(&self) -> Self {
        todo!()
    }
}

impl TermState for EmptyTermState {
    fn copy_from(&mut self, _other: &Self) -> Result<()> {
        Ok(())
    }
}

pub enum PrepareState<LR>
where
    LR: LeafReader,
{
    Ready(usize),
    Pending(
        Arc<Term>,
        usize,
        <<LR as LeafReader>::Terms as Terms>::TermsEnum,
    ),
}

pub type TermStateTerm<T> =
    Either2TermState<LeafReaderTermState<T>, Either2TermState<TermStateImpl1, DummyTermState>>;
pub fn build<IRC, LR, S>(
    index_searcher: &IndexSearcher<IRC, LR, S>,
    term: Arc<Term>,
    needs_stats: bool,
) -> Result<TermStates<TermStateTerm<LR>>>
where
    IRC: IndexReaderContext<LR>,
    LR: LeafReader,
    S: Similarity,
{
    let context = index_searcher.get_top_reader_context();
    let mut per_reader_term_state = TermStates::new(
        if needs_stats {
            None
        } else {
            Some(term.clone())
        },
        context,
    )?;

    if needs_stats {
        let mut pending_term_lookups = Vec::new();

        for ctx in context.leaves()? {
            let terms = terms_util::get_terms(ctx.reader(), term.field())?;
            let mut terms_enum = terms.iterator()?;
            if terms_enum.prepare_seek_exact(term.bytes())?.is_some() {
                let ord = ctx.ord;
                if pending_term_lookups.len() <= ord {
                    ArrayUtil::grow_with_len(&mut pending_term_lookups, ord + 1);
                }
                pending_term_lookups[ord] = Some(terms_enum);
            }
        }

        for (ord, mut pending) in pending_term_lookups.into_iter().enumerate() {
            if let Some(ref mut terms_enum) = pending
                && terms_enum.get_prepare_seek_exact_status(term.bytes())?
            {
                per_reader_term_state.register_with_stats(
                    terms_enum.term_state()?,
                    ord,
                    terms_enum.doc_freq()?,
                    terms_enum.total_term_freq()?,
                );
            }
        }
    }

    Ok(per_reader_term_state)
}
