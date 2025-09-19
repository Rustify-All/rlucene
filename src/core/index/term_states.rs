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
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::term::Term;
use crate::core::index::term_state::{Either2TermState, TermState};
use crate::core::index::terms::{Terms, terms_util};
use crate::core::index::terms_enum::{PreparedSeekExactResult, TermsEnum};
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::fmt::{Display, Formatter};
use std::rc::Rc;

/// Maintains an [`IndexReader`](crate::core::index::index_reader::IndexReader) [`TermState`] view over [`IndexReader`](crate::core::index::index_reader::IndexReader) instances
/// containing a single term. The [`TermStates`] doesn't track if the given [`TermState`]
/// objects are valid, neither if the [`TermState`] instances refer to the same terms in the
/// associated readers.
pub struct TermStates<TS>
where
    TS: TermState + Default,
{
    top_reader_context_identity: Rc<()>,
    states: Vec<Rc<TS>>,
    term: Option<Rc<Term>>,
    doc_freq: i32,
    total_term_freq: i64,
}
impl<TS> TermStates<TS>
where
    TS: TermState + Default,
{
    pub fn new<IRC, LR>(term: Option<Rc<Term>>, context: &IRC) -> Result<Self>
    where
        IRC: IndexReaderContext<LR>,
    {
        debug_assert!(context.base().is_top_level);
        let mut states = Vec::new();
        let num_leaves = context.leaves()?.len();
        for _ in 0..num_leaves {
            states.push(Rc::new(TS::default()))
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
        Rc::ptr_eq(&self.top_reader_context_identity, &context.base().identity)
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
            *slot = Rc::new(TS::default());
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
        self.states[ord] = Rc::new(state);
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
    TS: TermState + Default,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "TermStates")?;
        for state in &self.states {
            writeln!(f, "  state={}", state)?;
        }
        Ok(())
    }
}
pub type TermStateTerm<T> = Either2TermState<
    <<<T as LeafReader>::Terms as Terms>::TermsEnum as TermsEnum>::TermState,
    Either2TermState<TermStateImpl1, DummyTermState>,
>;
pub fn build<IRC, LR>(
    index_searcher: &IndexSearcher<IRC, LR>,
    term: Term,
    needs_stats: bool,
) -> Result<TermStates<TermStateTerm<LR>>>
where
    IRC: IndexReaderContext<LR>,
    LR: LeafReader,
{
    let context = index_searcher.get_top_reader_context();
    let term = Rc::new(term);
    let mut per_reader_term_state = TermStates::new(
        if needs_stats {
            None
        } else {
            Some(term.clone())
        },
        context,
    )?;

    if needs_stats {
        for ctx in context.leaves()? {
            let terms = terms_util::get_terms(ctx.reader(), term.field())?;
            let mut terms_enum = terms.iterator()?;

            let should_register = match terms_enum.prepare_seek_exact(term.bytes())? {
                Some(supplier) => supplier.get()?,
                None => false,
            };

            if should_register {
                per_reader_term_state.register_with_stats(
                    terms_enum.term_state()?,
                    ctx.ord,
                    terms_enum.doc_freq()?,
                    terms_enum.total_term_freq()?,
                );
            }
        }
    }

    Ok(per_reader_term_state)
}
