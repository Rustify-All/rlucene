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
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::term::Term;
use crate::core::index::term_state::TermState;
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
    states: Vec<TS>,
    term: Option<Rc<Term>>,
    doc_freq: i32,
    total_term_freq: i64,
}
impl<TS> TermStates<TS>
where
    TS: TermState + Default,
{
    pub fn new(term: Option<Rc<Term>>, context: &impl IndexReaderContext) -> Result<Self> {
        debug_assert!(context.base().is_top_level);

        let num_leaves = context.leaves()?.len();

        Ok(TermStates {
            top_reader_context_identity: context.base().identity.clone(),
            doc_freq: 0,
            total_term_freq: 0,
            states: vec![TS::default(); num_leaves],
            term,
        })
    }

    pub fn new_empty(context: &impl IndexReaderContext) -> Result<Self> {
        Self::new(None, context)
    }
    pub fn was_built_for(&self, context: &impl IndexReaderContext) -> bool {
        Rc::ptr_eq(&self.top_reader_context_identity, &context.base().identity)
    }
    pub fn with_state_and_stats(
        context: &impl IndexReaderContext,
        state: TS,
        ord: usize,
        doc_freq: i32,
        total_term_freq: i64,
    ) -> Result<Self> {
        let mut ts = TermStates::new_empty(context)?;
        ts.register_with_stats(state, ord, doc_freq, total_term_freq);
        Ok(ts)
    }

    /// Clears the TermStates internal state and removes all registered TermStates
    pub fn clear(&mut self) {
        self.doc_freq = 0;
        self.total_term_freq = 0;

        for slot in &mut self.states {
            *slot = TS::default();
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
        self.states[ord] = state;
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
