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
use crate::core::search::similarities_impl::similarities::Similarity;
use std::fmt::Write as _;

use crate::core::codecs::block_term_state::TermStateEnum;
use crate::core::index::filtered_terms_enum::FilteredTermsEnum;
use crate::core::index::impacts_enum::ImpactsEnumEnum2;
use crate::core::index::index_reader::Identity;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::single_terms_enum::SingleTermsEnum;
use crate::core::index::term::Term;
use crate::core::index::terms::{Terms, TermsIntersect, TermsPosting, TermsTE};
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::index::{BytesRef, BytesRefValue, BytesRefValueEnum};
use crate::core::search::fuzzy_automaton_builder::FuzzyAutomatonBuilder;
use crate::core::search::fuzzy_terms_enum::FuzzyTermsEnum;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::multi_term_query::{
  MultiTermQuery, MultiTermQuerySet, RewriteMethod, RewriteMethodEnum,
  TopTermsBlendedFreqScoringRewrite,
};
use crate::core::search::query::{Query, QueryBase, QueryWeight};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::util::access::ByteSource;
use crate::core::util::attribute_source::AttributeSourceEnum2;
use crate::core::util::automation::compiled_automaton::CompiledAutomaton;
use crate::core::util::automation::levenshtein_automata::LevenshteinAutomata;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{CoreHelper, HasIdentity};
use std::fmt::{Debug, Formatter};
use std::hash::{Hash, Hasher};

/// Implements the fuzzy search query. The similarity measurement is based on the
/// Damerau-Levenshtein (optimal string alignment) algorithm, though you can explicitly choose
/// classic Levenshtein by passing `false` to the `transpositions` parameter.
///
/// This query uses [`TopTermsBlendedFreqScoringRewrite`] as default. So terms will be collected and
/// scored according to their edit distance. Only the top terms are used for building the
/// [`BooleanQuery`](crate::core::search::boolean_query::BooleanQuery). It is not recommended to change the rewrite mode for fuzzy queries.
///
/// At most, this query will match terms up to
/// [`LevenshteinAutomata::MAXIMUM_SUPPORTED_DISTANCE`](crate::core::util::automation::levenshtein_automata::LevenshteinAutomata::MAXIMUM_SUPPORTED_DISTANCE) edits. Higher distances (especially with
/// transpositions enabled), are generally not useful and will match a significant amount of the term
/// dictionary. If you really want this, consider using an n-gram indexing technique instead.
///
/// NOTE: terms of length 1 or 2 will sometimes not match because of how the scaled distance between
/// two terms is computed. For a term to match, the edit distance between the terms must be less than
/// the minimum length term (either the input term, or the candidate term). For example, [`FuzzyQuery`]
/// on term `"abcd"` with `maxEdits=2` will not match an indexed term `"ab"`, and [`FuzzyQuery`] on
/// term `"a"` with `maxEdits=2` will not match an indexed term `"abc"`.
#[derive(Clone)]
pub struct FuzzyQuery {
  max_edits: i32,
  max_expansions: usize,
  transpositions: bool,
  prefix_length: usize,
  term: Term,
  rewrite_method: RewriteMethodEnum,
  id: Identity,
}

impl FuzzyQuery {
  pub const DEFAULT_MAX_EDITS: i32 = LevenshteinAutomata::MAXIMUM_SUPPORTED_DISTANCE;
  pub const DEFAULT_PREFIX_LENGTH: usize = 0;
  pub const DEFAULT_MAX_EXPANSIONS: usize = 50;
  pub const DEFAULT_TRANSPOSITIONS: bool = true;
  /// Creates a default top-terms blended frequency scoring rewrite with the given max expansions
  pub fn default_rewrite_method(max_expansions: usize) -> TopTermsBlendedFreqScoringRewrite {
    TopTermsBlendedFreqScoringRewrite::new(max_expansions)
  }

  pub fn new<TermInput>(term: TermInput) -> Result<Self>
  where
    TermInput: Into<Term>,
  {
    let term = term.into();
    Self::with_max_edits(term, Self::DEFAULT_MAX_EDITS)
  }

  pub fn with_max_edits<TermInput>(term: TermInput, max_edits: i32) -> Result<Self>
  where
    TermInput: Into<Term>,
  {
    let term = term.into();
    Self::with_max_edits_and_prefix(term, max_edits, Self::DEFAULT_PREFIX_LENGTH)
  }

  pub fn with_max_edits_and_prefix<TermInput>(
    term: TermInput,
    max_edits: i32,
    prefix_length: usize,
  ) -> Result<Self>
  where
    TermInput: Into<Term>,
  {
    let term = term.into();
    Self::with_options(
      term,
      max_edits,
      prefix_length,
      Self::DEFAULT_MAX_EXPANSIONS,
      Self::DEFAULT_TRANSPOSITIONS,
    )
  }

  pub fn with_options<TermInput>(
    term: TermInput,
    max_edits: i32,
    prefix_length: usize,
    max_expansions: usize,
    transpositions: bool,
  ) -> Result<Self>
  where
    TermInput: Into<Term>,
  {
    let term = term.into();
    Self::with_rewrite(
      term,
      max_edits,
      prefix_length,
      max_expansions,
      transpositions,
      Self::default_rewrite_method(max_expansions),
    )
  }
  /// Create a new [`FuzzyQuery`] that will match terms with an edit distance of at most `max_edits` to
  /// `term`. If a `prefix_length` > 0 is specified, a common prefix of that length is also required.
  ///
  /// # Parameters
  ///
  /// - `term`: the term to search for.
  /// - `max_edits`: must be `>= 0` and `<=` [`LevenshteinAutomata::MAXIMUM_SUPPORTED_DISTANCE`](crate::core::util::automation::levenshtein_automata::LevenshteinAutomata::MAXIMUM_SUPPORTED_DISTANCE).
  /// - `prefix_length`: length of common (non-fuzzy) prefix.
  /// - `max_expansions`: the maximum number of terms to match. If this number is greater than
  ///   [`index_searcher::get_max_clause_count`](crate::core::search::index_searcher::get_max_clause_count) when the query is rewritten, then the
  ///   `max_clause_count` will be used instead.
  /// - `transpositions`: `true` if transpositions should be treated as a primitive edit operation.
  ///   If this is `false`, comparisons will implement the classic Levenshtein algorithm.
  /// - `rewrite_method`: the rewrite method to use to build the final query.
  pub fn with_rewrite<R, TermInput>(
    term: TermInput,
    max_edits: i32,
    prefix_length: usize,
    max_expansions: usize,
    transpositions: bool,
    rewrite_method: R,
  ) -> Result<Self>
  where
    R: Into<RewriteMethodEnum>,
    TermInput: Into<Term>,
  {
    let term = term.into();
    if !(0..=LevenshteinAutomata::MAXIMUM_SUPPORTED_DISTANCE).contains(&max_edits) {
      return Err(LuceneError::illegal_argument(format!(
        "maxEdits must be between 0 and {}",
        LevenshteinAutomata::MAXIMUM_SUPPORTED_DISTANCE
      )));
    }
    if max_expansions == 0 {
      return Err(LuceneError::illegal_argument(
        "maxExpansions must be positive.",
      ));
    }

    Ok(Self {
      term,
      max_edits,
      prefix_length,
      transpositions,
      max_expansions,
      rewrite_method: rewrite_method.into(),
      id: Identity::default(),
    })
  }
  /// Returns:
  // the maximum number of edit distances allowed for this query to match.
  pub fn get_max_edits(&self) -> i32 {
    self.max_edits
  }
  /// Returns the non-fuzzy prefix length.
  /// This is the number of characters at the start of a term that must be identical (not fuzzy) to the query term if the query is to match that term.
  pub fn get_prefix_length(&self) -> usize {
    self.prefix_length
  }
  /// Returns true if transpositions should be treated as a primitive edit operation.
  /// If this is false, comparisons will implement the classic Levenshtein algorithm.
  pub fn get_transpositions(&self) -> bool {
    self.transpositions
  }
  /// Returns the compiled automata used to match terms
  pub fn get_automata(&self) -> Result<CompiledAutomaton> {
    let text = self.term.text()?;
    FuzzyAutomatonBuilder::from_term(
      || text,
      self.max_edits,
      self.prefix_length,
      self.transpositions,
    )?
    .build_max_edit_automaton()
  }
  /// Returns the [`CompiledAutomaton`] internally used by [`FuzzyQuery`] to match terms. This is a
  /// very low-level method and may no longer exist in case the implementation of fuzzy-matching
  /// changes in the future.
  ///
  /// # Parameters
  ///
  /// - `term`: the term to search for.
  /// - `max_edits`: must be `>= 0` and `<=`
  ///   [`LevenshteinAutomata::MAXIMUM_SUPPORTED_DISTANCE`](crate::core::util::automation::levenshtein_automata::LevenshteinAutomata::MAXIMUM_SUPPORTED_DISTANCE).
  /// - `prefix_length`: length of common (non-fuzzy) prefix.
  /// - `transpositions`: `true` if transpositions should be treated as a primitive edit operation.
  ///   If this is `false`, comparisons will implement the classic Levenshtein algorithm.
  ///
  /// Returns a [`CompiledAutomaton`] that matches terms that satisfy input parameters.
  pub fn get_fuzzy_automaton<T>(
    term: T,
    max_edits: i32,
    prefix_length: usize,
    transpositions: bool,
  ) -> Result<CompiledAutomaton>
  where
    T: Into<String>,
  {
    let builder = FuzzyAutomatonBuilder::new(term, max_edits, prefix_length, transpositions)?;
    builder.build_max_edit_automaton()
  }

  pub fn get_term(&self) -> &Term {
    &self.term
  }

  pub fn float_to_edits(minimum_similarity: f32, term_len: usize) -> i32 {
    if minimum_similarity >= 1.0 {
      CoreHelper::min_f32(
        minimum_similarity,
        LevenshteinAutomata::MAXIMUM_SUPPORTED_DISTANCE as f32,
      ) as i32
    } else if minimum_similarity == 0.0 {
      0
    } else {
      std::cmp::min(
        ((1.0_f64 - minimum_similarity as f64) * term_len as f64) as i32,
        LevenshteinAutomata::MAXIMUM_SUPPORTED_DISTANCE,
      )
    }
  }
}

impl QueryBase for FuzzyQuery {
  type Weight<IRC, SIM>
    = QueryWeight<IRC, SIM>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static;

  fn to_string(&self, field: &str) -> Result<String> {
    let mut buffer = String::new();
    if self.term.field() != field {
      buffer.push_str(self.term.field());
      buffer.push(':');
    }
    {
      let text = self.term.text()?;
      if buffer.is_empty() {
        buffer = text.into_owned();
      } else {
        buffer.push_str(&text);
      }
    }
    buffer.push('~');
    write!(buffer, "{}", self.max_edits)?;
    Ok(buffer)
  }

  fn create_weight<IRC, SIM>(
    self,
    _searcher: &IndexSearcher<IRC, SIM>,
    _score_mode: &ScoreMode,
    _boost: f32,
  ) -> Result<Self::Weight<IRC, SIM>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    Self: Sized,
    IndexSearcher<IRC, SIM>: Sync,
  {
    Err(LuceneError::unsupported_operation(""))
  }

  fn rewrite<IRC, SIM>(&self, searcher: &IndexSearcher<IRC, SIM>) -> Result<Option<Query>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    Self: Sized,
    IndexSearcher<IRC, SIM>: Sync,
  {
    self.rewrite_method.rewrite(searcher, self).map(Some)
  }

  fn visit<QV>(&self, visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    if visitor.accept_field(self.term.field()) {
      visitor.consume_terms_matching(self.into(), self.term.field(), || {
        Ok(self.get_automata()?.run_automaton)
      })?;
    }
    Ok(())
  }
}

impl Debug for FuzzyQuery {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self.to_string("") {
      Ok(s) => write!(f, "{}", s),
      Err(_) => Err(std::fmt::Error),
    }
  }
}

impl HasIdentity for FuzzyQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl Hash for FuzzyQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.term.hash(state);
    self.max_edits.hash(state);
    self.prefix_length.hash(state);
    self.max_expansions.hash(state);
    self.transpositions.hash(state);
    self.rewrite_method.hash(state);
  }
}

impl Eq for FuzzyQuery {}

impl PartialEq for FuzzyQuery {
  fn eq(&self, other: &Self) -> bool {
    self.term == other.term
      && self.max_edits == other.max_edits
      && self.prefix_length == other.prefix_length
      && self.max_expansions == other.max_expansions
      && self.transpositions == other.transpositions
      && self.rewrite_method == other.rewrite_method
  }
}

pub enum FuzzyQueryTermsEnum<T>
where
  T: Terms,
{
  Single(FilteredTermsEnum<TermsTE<T>, SingleTermsEnum>),
  Fuzzy(FuzzyTermsEnum<T>),
}

impl<T> BytesRefIterator for FuzzyQueryTermsEnum<T>
where
  T: Terms,
  TermsIntersect<T>: TermsEnum<PostingsEnum = TermsPosting<T>>,
{
  type Value<'a>
    = BytesRefValueEnum<'a>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    match self {
      Self::Single(t) => t.next().map(|value| value.map(BytesRefValue::into_value)),
      Self::Fuzzy(t) => t.next().map(|value| value.map(BytesRefValue::into_value)),
    }
  }

  fn set_next(&mut self) -> Result<bool> {
    match self {
      Self::Single(t) => t.set_next(),
      Self::Fuzzy(t) => t.set_next(),
    }
  }
}

impl<T> TermsEnum for FuzzyQueryTermsEnum<T>
where
  T: Terms,
  TermsIntersect<T>: TermsEnum<PostingsEnum = TermsPosting<T>>,
{
  type AttributeSource<'a>
    = AttributeSourceEnum2<
    <FilteredTermsEnum<TermsTE<T>, SingleTermsEnum> as TermsEnum>::AttributeSource<'a>,
    <FuzzyTermsEnum<T> as TermsEnum>::AttributeSource<'a>,
  >
  where
    Self: 'a;
  type AttributeSourceMut<'a>
    = AttributeSourceEnum2<
    <FilteredTermsEnum<TermsTE<T>, SingleTermsEnum> as TermsEnum>::AttributeSourceMut<'a>,
    <FuzzyTermsEnum<T> as TermsEnum>::AttributeSourceMut<'a>,
  >
  where
    Self: 'a;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    match self {
      Self::Single(t) => Ok(AttributeSourceEnum2::A(t.attributes()?)),
      Self::Fuzzy(t) => Ok(AttributeSourceEnum2::B(t.attributes()?)),
    }
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    match self {
      Self::Single(t) => Ok(AttributeSourceEnum2::A(t.attributes_mut()?)),
      Self::Fuzzy(t) => Ok(AttributeSourceEnum2::B(t.attributes_mut()?)),
    }
  }

  fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool> {
    match self {
      Self::Single(t) => t.seek_exact(term),
      Self::Fuzzy(t) => t.seek_exact(term),
    }
  }

  fn prepare_seek_exact<BS: ByteSource>(&mut self, text: &BytesRef<BS>) -> Result<Option<()>> {
    match self {
      Self::Single(t) => t.prepare_seek_exact(text),
      Self::Fuzzy(t) => t.prepare_seek_exact(text),
    }
  }

  fn get_prepare_seek_exact_status<BS: ByteSource>(
    &mut self,
    target: &BytesRef<BS>,
  ) -> Result<bool> {
    match self {
      Self::Single(t) => t.get_prepare_seek_exact_status(target),
      Self::Fuzzy(t) => t.get_prepare_seek_exact_status(target),
    }
  }

  fn seek_ceil<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<SeekStatus> {
    match self {
      Self::Single(t) => t.seek_ceil(term),
      Self::Fuzzy(t) => t.seek_ceil(term),
    }
  }

  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    match self {
      Self::Single(t) => t.seek_exact_with_ord(ord),
      Self::Fuzzy(t) => t.seek_exact_with_ord(ord),
    }
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    state: &TermStateEnum,
  ) -> Result<()> {
    match self {
      Self::Single(t) => t.seek_exact_with_state(term, state),
      Self::Fuzzy(t) => t.seek_exact_with_state(term, state),
    }
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    match self {
      Self::Single(t) => t.term().map(BytesRefValue::into_value),
      Self::Fuzzy(t) => t.term().map(BytesRefValue::into_value),
    }
  }

  fn ord(&self) -> Result<i64> {
    match self {
      Self::Single(t) => t.ord(),
      Self::Fuzzy(t) => t.ord(),
    }
  }

  fn doc_freq(&mut self) -> Result<i32> {
    match self {
      Self::Single(t) => t.doc_freq(),
      Self::Fuzzy(t) => t.doc_freq(),
    }
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    match self {
      Self::Single(t) => t.total_term_freq(),
      Self::Fuzzy(t) => t.total_term_freq(),
    }
  }

  type PostingsEnum = TermsPosting<T>;

  fn postings(&mut self, reuse: Option<Self::PostingsEnum>) -> Result<Self::PostingsEnum> {
    match self {
      Self::Single(t) => t.postings(reuse),
      Self::Fuzzy(t) => t.postings(reuse),
    }
  }

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    match self {
      Self::Single(t) => t.postings_with_flags(reuse, flags),
      Self::Fuzzy(t) => t.postings_with_flags(reuse, flags),
    }
  }

  type ImpactsEnum = ImpactsEnumEnum2<
    <FilteredTermsEnum<TermsTE<T>, SingleTermsEnum> as TermsEnum>::ImpactsEnum,
    <FuzzyTermsEnum<T> as TermsEnum>::ImpactsEnum,
  >;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    match self {
      Self::Single(t) => Ok(ImpactsEnumEnum2::A(t.impacts(flags)?)),
      Self::Fuzzy(t) => Ok(ImpactsEnumEnum2::B(t.impacts(flags)?)),
    }
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    match self {
      Self::Single(t) => t.term_state(),
      Self::Fuzzy(t) => t.term_state(),
    }
  }
}

impl MultiTermQuery for FuzzyQuery {
  fn get_field(&self) -> &str {
    self.term.field()
  }

  type TermsEnum<T>
    = FuzzyQueryTermsEnum<T>
  where
    T: Terms,
    TermsIntersect<T>: TermsEnum<PostingsEnum = TermsPosting<T>>;

  fn get_terms_enum<T>(&self, terms: T) -> Result<Self::TermsEnum<T>>
  where
    T: Terms + Clone,
    TermsIntersect<T>: TermsEnum<PostingsEnum = TermsPosting<T>>,
  {
    if self.max_edits == 0 {
      return Ok(FuzzyQueryTermsEnum::Single(SingleTermsEnum::new(
        terms.iterator()?,
        self.term.bytes().clone(),
      )));
    }
    Ok(FuzzyQueryTermsEnum::Fuzzy(FuzzyTermsEnum::new_with_attrs(
      terms,
      &self.term,
      self.max_edits,
      self.prefix_length,
      self.transpositions,
    )?))
  }

  fn to_query(&self) -> Query {
    MultiTermQuerySet::from(self.clone()).into()
  }
}

impl crate::core::util::accountable::Accountable for FuzzyQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED)
  }
}
