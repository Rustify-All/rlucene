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
use crate::core::index::index_reader::Identity;
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::term::Term;
use crate::core::index::terms::Terms;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::multi_term_query::{
  ConstantScoreBlendedRewrite, MultiTermQuery, MultiTermQuerySet, RewriteMethod, RewriteMethodEnum,
};
use crate::core::search::query::{Query, QueryBase, QueryRef, QueryWeight};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::util::HasIdentity;
use crate::core::util::accountable::Accountable;
use crate::core::util::automation::automaton::Automaton;
use crate::core::util::automation::compiled_automaton::{CompiledAutomaton, CompiledAutomatonTE};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::fmt::{Debug, Formatter};
use std::hash::{Hash, Hasher};
/// A [`Query`] that will match terms against a finite-state machine.
///
/// This query will match documents that contain terms accepted by a given finite-state machine.
/// The automaton can be constructed with the automaton API.
/// Alternatively, it can be created from a regular expression with [`RegexpQuery`](crate::core::search::regexp_query::RegexpQuery) or from the
/// standard Lucene wildcard syntax with [`WildcardQuery`](crate::core::search::regexp_query::RegexpQuery).
///
/// When the query is executed, it will will enumerate the term dictionary in an intelligent way
/// to reduce the number of comparisons. For example: the regular expression of `[dl]og?`
/// will make approximately four comparisons: do, dog, lo, and log.
///

#[derive(Clone)]
pub struct AutomatonQuery {
  pub(crate) compiled: CompiledAutomaton,
  pub(crate) term: Term,
  #[allow(dead_code)]
  automaton_is_binary: bool,
  ram_bytes_used: i64,
  id: Identity,
  pub(crate) rewrite_method: RewriteMethodEnum,
}
impl AutomatonQuery {
  /// Create a new [`AutomatonQuery`] from an [`Automaton`].
  ///
  /// - `term`: [`Term`] containing field and possibly some pattern structure. The term text is
  ///   ignored.
  /// - `automaton`: [`Automaton`] to run, terms that are accepted are considered a match.
  pub fn from_automaton<TermInput>(term: TermInput, automaton: Automaton) -> Result<Self>
  where
    TermInput: Into<Term>,
  {
    let term = term.into();
    Self::from_automaton_with_binary(term, automaton, false)
  }

  /// Create a new [`AutomatonQuery`] from an [`Automaton`].
  ///
  /// - `term`: [`Term`] containing field and possibly some pattern structure. The term text is
  ///   ignored.
  /// - `automaton`: [`Automaton`] to run, terms that are accepted are considered a match.
  /// - `is_binary`: if `true`, this automaton is already binary and will not go through the
  ///   UTF32ToUTF8 conversion.
  pub fn from_automaton_with_binary<TermInput>(
    term: TermInput,
    automaton: Automaton,
    is_binary: bool,
  ) -> Result<Self>
  where
    TermInput: Into<Term>,
  {
    let term = term.into();
    Self::new(term, automaton, is_binary, ConstantScoreBlendedRewrite)
  }
  /// Create a new [`AutomatonQuery`] from an [`Automaton`].
  ///
  /// - `term`: [`Term`] containing field and possibly some pattern structure. The term text is
  ///   ignored.
  /// - `automaton`: [`Automaton`] to run, terms that are accepted are considered a match.
  /// - `is_binary`: unused.
  /// - `rewrite_method`: the rewrite method to use to build the final query from the automaton.
  pub fn new<T, TermInput>(
    term: TermInput,
    automaton: Automaton,
    is_binary: bool,
    rewrite_method: T,
  ) -> Result<Self>
  where
    T: Into<RewriteMethodEnum>,
    TermInput: Into<Term>,
  {
    let term = term.into();
    let rewrite_method = rewrite_method.into();
    let compiled = CompiledAutomaton::with_binary(automaton, false, true, is_binary)?;
    let ram_bytes_used = term
      .ram_bytes_used()?
      .saturating_add(compiled.ram_bytes_used()?);

    Ok(Self {
      compiled,
      term,
      #[allow(dead_code)]
      automaton_is_binary: is_binary,
      ram_bytes_used,
      id: Identity::new(),
      rewrite_method,
    })
  }
  pub fn get_compiled(&self) -> &CompiledAutomaton {
    &self.compiled
  }

  pub(crate) fn visit_with_query<QV>(&self, visitor: &mut QV, query: QueryRef<'_>) -> Result<()>
  where
    QV: QueryVisitor,
  {
    if visitor.accept_field(self.term.field()) {
      self.compiled.visit(visitor, query, self.term.field())?;
    }
    Ok(())
  }
}

impl QueryBase for AutomatonQuery {
  type Weight<IRC, SIM>
    = QueryWeight<IRC, SIM>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static;

  fn to_string(&self, field: &str) -> Result<String> {
    let text = "AutomatonQuery {\n}";
    if self.term.field() == field {
      return Ok(text.to_owned());
    }
    let mut buffer = String::with_capacity(self.term.field().len() + 1 + text.len());
    buffer.push_str(self.term.field());
    buffer.push(':');
    buffer.push_str(text);
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
    self.visit_with_query(visitor, self.into())
  }
}

impl Debug for AutomatonQuery {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self.to_string("") {
      Ok(s) => write!(f, "{}", s),
      Err(_) => Err(std::fmt::Error),
    }
  }
}

impl HasIdentity for AutomatonQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl Accountable for AutomatonQuery {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(self.ram_bytes_used)
  }
}
impl PartialEq for AutomatonQuery {
  fn eq(&self, other: &Self) -> bool {
    if std::ptr::eq(self, other) {
      return true;
    }
    self.rewrite_method == other.rewrite_method
      && self.compiled == other.compiled
      && self.term == other.term
  }
}
impl Eq for AutomatonQuery {}
impl Hash for AutomatonQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.rewrite_method.hash(state);
    self.compiled.hash(state);
    self.term.hash(state);
  }
}
impl MultiTermQuery for AutomatonQuery {
  fn get_field(&self) -> &str {
    self.term.field()
  }

  type TermsEnum<T>
    = CompiledAutomatonTE<T>
  where
    T: Terms;

  fn get_terms_enum<T>(&self, terms: T) -> Result<Self::TermsEnum<T>>
  where
    T: Terms + Clone,
  {
    self.compiled.get_terms_enum(terms)
  }

  fn to_query(&self) -> Query {
    MultiTermQuerySet::from(self.clone()).into()
  }
}
