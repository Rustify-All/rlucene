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
use crate::core::util::automation::compiled_automaton::CompiledAutomaton;
use crate::core::util::automation::levenshtein_automata::LevenshteinAutomata;
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use std::borrow::Cow;

/// Builds a set of [`CompiledAutomaton`] for fuzzy matching on a given term,
/// with specified maximum edit distance, fixed prefix and whether or not to
/// allow transpositions.
pub(crate) struct FuzzyAutomatonBuilder<'a> {
  term: Cow<'a, str>,
  max_edits: usize,
  lev_builder: LevenshteinAutomata,
  prefix_end: usize,
  term_length: usize,
}

impl FuzzyAutomatonBuilder<'static> {
  pub(crate) fn new<T>(
    term: T,
    max_edits: i32,
    prefix_length: usize,
    transpositions: bool,
  ) -> Result<Self>
  where
    T: Into<String>,
  {
    Self::from_term(
      || Cow::Owned(term.into()),
      max_edits,
      prefix_length,
      transpositions,
    )
  }
}

impl<'a> FuzzyAutomatonBuilder<'a> {
  pub(crate) fn from_term(
    term: impl FnOnce() -> Cow<'a, str>,
    max_edits: i32,
    prefix_length: usize,
    transpositions: bool,
  ) -> Result<Self> {
    if !(0..=LevenshteinAutomata::MAXIMUM_SUPPORTED_DISTANCE).contains(&max_edits) {
      return Err(LuceneError::illegal_argument(format!(
        "max edits must be 0..{}, inclusive; got: {}",
        LevenshteinAutomata::MAXIMUM_SUPPORTED_DISTANCE,
        max_edits
      )));
    }

    let term = term();
    let mut chars = term.chars();
    let prefix_length = chars.by_ref().take(prefix_length).count();
    let prefix_end = term.len() - chars.as_str().len();
    let suffix: Vec<i32> = chars.map(|ch| ch as i32).collect();
    let term_length = prefix_length + suffix.len();
    let lev_builder = LevenshteinAutomata::from_word(suffix, char::MAX as i32, transpositions)?;

    Ok(Self {
      term,
      max_edits: max_edits as usize,
      lev_builder,
      prefix_end,
      term_length,
    })
  }

  pub(crate) fn build_automaton_set(&self) -> Result<Vec<CompiledAutomaton>> {
    let mut compiled = Vec::with_capacity(self.max_edits + 1);
    for edits in 0..=self.max_edits {
      compiled.push(self.compile(edits)?);
    }
    Ok(compiled)
  }

  pub(crate) fn build_max_edit_automaton(&self) -> Result<CompiledAutomaton> {
    self.compile(self.max_edits)
  }

  pub(crate) fn get_term_length(&self) -> usize {
    self.term_length
  }

  fn compile(&self, edits: usize) -> Result<CompiledAutomaton> {
    let automaton = self
      .lev_builder
      .to_automaton_with_prefix(edits, &self.term[..self.prefix_end])?
      .ok_or_else(|| {
        LuceneError::illegal_argument(format!("unsupported edit distance: {}", edits))
      })?;

    match CompiledAutomaton::new(automaton, true, false) {
      Ok(compiled) => Ok(compiled),
      Err(err) if matches!(err.kind(), LuceneErrorKind::TooComplexToDeterminize(_)) => {
        let mut fuzzy_error = LuceneError::fuzzy_terms(self.term.as_ref().to_owned());
        fuzzy_error.add_suppressed(err);
        Err(fuzzy_error)
      },
      Err(err) => Err(err),
    }
  }
}
