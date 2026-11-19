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
use crate::core::index::term_states;
use crate::core::index::term_states::TermStates;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::boost_query::BoostQuery;
use crate::core::search::disjunction_max_query::DisjunctionMaxQuery;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::query::{Query, QueryBase, QueryWeight};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::term_query::TermQuery;
use crate::core::search::{boolean_query, index_searcher};
use crate::core::util::bit_util::BitUtil;
use crate::core::util::core_helper::CoreHelper;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::in_place_merge_sorter::InPlaceMergeSorter;
use crate::core::util::{HasIdentity, Sorter, ToInt};
use crate::impl_from_for_enum;
use smallvec::SmallVec;
use std::cmp::PartialEq;
use std::fmt::Write as _;
use std::fmt::{Debug, Formatter};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// A [`Query`] that blends index statistics across multiple terms. This is particularly useful
/// when several terms should produce identical scores, regardless of their index statistics.
///
/// For instance imagine that you are resolving synonyms at search time, all terms should produce
/// identical scores instead of the default behavior, which tends to give higher scores to rare
/// terms.
///
/// An other useful use-case is cross-field search: imagine that you would like to search for
/// `john` on two fields: `first_name` and `last_name`. You might not want to give a higher weight
/// to matches on the field where `john` is rarer, in which case [`BlendedTermQuery`] would help as
/// well.
#[derive(Clone)]
pub struct BlendedTermQuery {
  terms: Vec<Arc<Term>>,
  boosts: Vec<f32>,
  contexts: Vec<Option<Arc<TermStates>>>,
  rewrite_method: RewriteMethodEnum,
  id: Identity,
}
impl BlendedTermQuery {
  fn new(
    mut terms: Vec<Arc<Term>>,
    mut boosts: Vec<f32>,
    mut contexts: Vec<Option<Arc<TermStates>>>,
    rewrite_method: RewriteMethodEnum,
  ) -> Result<Self> {
    debug_assert!(terms.len() == boosts.len());
    debug_assert!(terms.len() == contexts.len());
    let len = terms.len();
    let sub = InPlaceMergeSorterImpl::new(terms.as_mut(), boosts.as_mut(), contexts.as_mut());
    let mut sorter = InPlaceMergeSorter::new(sub);
    sorter.sort(0, len)?;
    Ok(Self {
      terms,
      boosts,
      contexts,
      rewrite_method,
      id: Identity::new(),
    })
  }
}

impl PartialEq for BlendedTermQuery {
  fn eq(&self, other: &Self) -> bool {
    self.terms == other.terms
      && self.contexts == other.contexts
      && CoreHelper::array_equals_f32(&self.boosts, &other.boosts)
      && self.rewrite_method == other.rewrite_method
  }
}

impl Eq for BlendedTermQuery {}

impl Hash for BlendedTermQuery {
  fn hash<H: Hasher>(&self, state: &mut H) {
    std::any::TypeId::of::<Self>().hash(state);
    self.terms.hash(state);
    self.contexts.hash(state);
    for boost in &self.boosts {
      (BitUtil::float_to_int_bits(*boost) as u32).hash(state);
    }
    self.rewrite_method.hash(state);
  }
}

impl Debug for BlendedTermQuery {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl HasIdentity for BlendedTermQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl QueryBase for BlendedTermQuery {
  type Weight<IRC, SIM>
    = QueryWeight<IRC, SIM>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static;

  fn to_string(&self, field: &str) -> Result<String> {
    let mut builder = String::from("Blended(");

    for (i, term) in self.terms.iter().enumerate() {
      if i != 0 {
        builder.push(' ');
      }

      if self.boosts[i] != 1.0 {
        BoostQuery::check_boost(self.boosts[i])?;
        builder.push('(');
        builder.push_str(&TermQuery::format_term(term.field(), term.bytes(), field)?);
        builder.push_str(")^");
        write!(builder, "{:.1}", self.boosts[i])?;
      } else {
        builder.push_str(&TermQuery::format_term(term.field(), term.bytes(), field)?);
      }
    }

    builder.push(')');
    Ok(builder)
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

  fn rewrite<IRC, SIM>(&self, index_searcher: &IndexSearcher<IRC, SIM>) -> Result<Option<Query>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    Self: Sized,
    IndexSearcher<IRC, SIM>: Sync,
  {
    let term_len = self.terms.len();
    let top_reader_context = index_searcher.get_top_reader_context();
    let mut contexts: SmallVec<[Arc<TermStates>; 8]> = SmallVec::with_capacity(self.contexts.len());
    for (i, context) in self.contexts.iter().cloned().enumerate() {
      match context {
        Some(v) => contexts.push(v),
        None => contexts.push(Arc::new(term_states::build_with_stats(
          index_searcher,
          &self.terms[i],
        )?)),
      }
    }

    // Compute aggregated doc freq and total term freq
    // df will be the max of all doc freqs
    // ttf will be the sum of all total term freqs
    let mut df = 0;
    let mut ttf = 0;

    for ctx in &contexts {
      df = df.max(ctx.doc_freq()?);
      ttf += ctx.total_term_freq()?;
    }

    for ctx in &mut contexts {
      let adjusted = adjust_frequencies(top_reader_context, ctx, df, ttf)?;
      if let Some(unique) = Arc::get_mut(ctx) {
        *unique = adjusted;
      } else {
        *ctx = Arc::new(adjusted);
      }
    }

    let mut term_queries = Vec::with_capacity(term_len);
    for ((term, boost), context) in self
      .terms
      .iter()
      .cloned()
      .zip(self.boosts.iter().copied())
      .zip(contexts)
    {
      let mut term_query: Query = TermQuery::with_term_state(term, Some(context)).into();

      if boost != 1.0 {
        term_query = BoostQuery::new(term_query, boost)?.into();
      }

      term_queries.push(term_query);
    }

    self.rewrite_method.rewrite(term_queries).map(Some)
  }

  fn visit<QV>(&self, visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    let terms: SmallVec<[_; 8]> = self
      .terms
      .iter()
      .map(Arc::as_ref)
      .filter(|term| visitor.accept_field(term.field()))
      .collect();
    if !terms.is_empty() {
      let query = self.into();
      let mut visitor = visitor.get_sub_visitor(Occur::Should, query);
      visitor.consume_terms(query, &terms)?;
    }
    Ok(())
  }
}
struct InPlaceMergeSorterImpl<'a> {
  terms: &'a mut [Arc<Term>],
  boosts: &'a mut [f32],
  contexts: &'a mut [Option<Arc<TermStates>>],
}
impl<'a> InPlaceMergeSorterImpl<'a> {
  fn new(
    terms: &'a mut [Arc<Term>],
    boosts: &'a mut [f32],
    contexts: &'a mut [Option<Arc<TermStates>>],
  ) -> Self {
    Self {
      terms,
      boosts,
      contexts,
    }
  }
}
impl<'a> Sorter for InPlaceMergeSorterImpl<'a> {
  fn compare(&mut self, i: usize, j: usize) -> Result<i32> {
    Ok(self.terms[i].cmp(&self.terms[j]).to_int())
  }

  fn swap(&mut self, i: usize, j: usize) -> Result<()> {
    self.terms.swap(i, j);
    self.contexts.swap(i, j);
    self.boosts.swap(i, j);
    Ok(())
  }
}

pub struct Builder {
  num_terms: usize,
  terms: Vec<Arc<Term>>,
  boosts: Vec<f32>,
  contexts: Vec<Option<Arc<TermStates>>>,
  rewrite_method: RewriteMethodEnum,
}

impl Default for Builder {
  fn default() -> Self {
    Self::new()
  }
}

impl Builder {
  pub fn new() -> Self {
    Self {
      num_terms: 0,
      terms: Vec::new(),
      boosts: Vec::new(),
      contexts: Vec::new(),
      rewrite_method: DisjunctionMaxRewrite::default().into(),
    }
  }
}
impl Builder {
  /// Set the [`RewriteMethod`]. Default is to use
  /// [`DisjunctionMaxRewrite`].
  ///
  /// See also:
  /// - [`RewriteMethod`]
  pub fn set_rewrite_method<T>(&mut self, rewrite_method: T) -> &mut Self
  where
    T: Into<RewriteMethodEnum>,
  {
    self.rewrite_method = rewrite_method.into();
    self
  }

  /// Add a new [`Term`] to this builder, with a default boost of `1`.
  ///
  /// See also:
  /// - [`Self::add_with_boost`]
  pub fn add<TermInput>(&mut self, term: TermInput) -> Result<&mut Self>
  where
    TermInput: Into<Term>,
  {
    let term = term.into();
    self.add_with_boost(term, 1.0)
  }

  /// Add a [`Term`] with the provided boost. The higher the boost, the more this term will
  /// contribute to the overall score of the [`BlendedTermQuery`].
  pub fn add_with_boost<TermInput>(&mut self, term: TermInput, boost: f32) -> Result<&mut Self>
  where
    TermInput: Into<Term>,
  {
    let term = term.into();
    self.add_with_term_states(term, boost, None)
  }

  /// Expert: Add a [`Term`] with the provided boost and context. This method is useful if you
  /// already have a [`TermStates`] object constructed for the given term.
  pub fn add_with_term_states<TermInput>(
    &mut self,
    term: TermInput,
    boost: f32,
    context: Option<Arc<TermStates>>,
  ) -> Result<&mut Self>
  where
    TermInput: Into<Term>,
  {
    let term = term.into();
    if self.num_terms >= index_searcher::get_max_clause_count() {
      return Err(index_searcher::new_nested());
    }
    self.terms.push(Arc::new(term));
    self.boosts.push(boost);
    self.contexts.push(context);
    self.num_terms += 1;
    Ok(self)
  }

  /// Build the [`BlendedTermQuery`].
  pub fn build(self) -> Result<BlendedTermQuery> {
    BlendedTermQuery::new(self.terms, self.boosts, self.contexts, self.rewrite_method)
  }
}
/// A [`RewriteMethod`] that creates a [`DisjunctionMaxQuery`] out of the sub queries. This
/// [`RewriteMethod`] is useful when having a good match on a single field is considered better
/// than having average matches on several fields.
#[derive(Clone)]
pub struct DisjunctionMaxRewrite {
  tie_breaker_multiplier: f32,
}
impl Default for DisjunctionMaxRewrite {
  fn default() -> Self {
    DisjunctionMaxRewrite::new(0.01)
  }
}
impl DisjunctionMaxRewrite {
  /// This [`RewriteMethod`] will create [`DisjunctionMaxQuery`] instances that have the provided tie
  /// breaker.
  ///
  /// See also:
  /// - [`DisjunctionMaxQuery`]
  pub fn new(tie_breaker_multiplier: f32) -> Self {
    Self {
      tie_breaker_multiplier,
    }
  }
}
impl PartialEq for DisjunctionMaxRewrite {
  fn eq(&self, other: &Self) -> bool {
    // Compare distinct instances with primitive `==`; the identity case keeps `Eq`
    // reflexive when the tie breaker is NaN.
    std::ptr::eq(self, other) || self.tie_breaker_multiplier == other.tie_breaker_multiplier
  }
}

impl Eq for DisjunctionMaxRewrite {}

impl Hash for DisjunctionMaxRewrite {
  fn hash<H: Hasher>(&self, state: &mut H) {
    std::any::TypeId::of::<Self>().hash(state);
    CoreHelper::hash_bits_f32_for_primitive_eq(self.tie_breaker_multiplier).hash(state);
  }
}
impl RewriteMethod for DisjunctionMaxRewrite {
  fn rewrite(&self, sub_queries: Vec<Query>) -> Result<Query> {
    Ok(DisjunctionMaxQuery::new(sub_queries, self.tie_breaker_multiplier)?.into())
  }
}

/// A [`RewriteMethod`] defines how queries for individual terms should be merged.
///
/// @lucene.experimental
///
/// See also:
/// - [`BooleanRewrite`]
/// - [`DisjunctionMaxRewrite`]
pub trait RewriteMethod {
  /// Merge the provided sub queries into a single [`Query`] object.
  fn rewrite(&self, sub_queries: Vec<Query>) -> Result<Query>;
}
#[derive(Clone)]
pub enum RewriteMethodEnum {
  DisjunctionMax(DisjunctionMaxRewrite),
  Boolean(BooleanRewrite),
}
impl_from_for_enum!(
    RewriteMethodEnum,
    BooleanRewrite => Boolean,
    DisjunctionMaxRewrite => DisjunctionMax,
);
impl RewriteMethod for RewriteMethodEnum {
  fn rewrite(&self, sub_queries: Vec<Query>) -> Result<Query> {
    match self {
      RewriteMethodEnum::DisjunctionMax(r) => r.rewrite(sub_queries),
      RewriteMethodEnum::Boolean(r) => r.rewrite(sub_queries),
    }
  }
}
impl PartialEq for RewriteMethodEnum {
  fn eq(&self, other: &Self) -> bool {
    match (self, other) {
      (Self::DisjunctionMax(a), Self::DisjunctionMax(b)) => a == b,
      (Self::Boolean(_), Self::Boolean(_)) => true,
      _ => false,
    }
  }
}
impl Hash for RewriteMethodEnum {
  fn hash<H: Hasher>(&self, state: &mut H) {
    match self {
      RewriteMethodEnum::DisjunctionMax(r) => r.hash(state),
      RewriteMethodEnum::Boolean(r) => r.hash(state),
    }
  }
}

impl Eq for RewriteMethodEnum {}
/// A [`RewriteMethod`] that adds all sub queries to a
/// [`BooleanQuery`](crate::core::search::boolean_query::BooleanQuery). This [`RewriteMethod`] is
/// useful when matching on several fields is considered better than having a good match on a single
/// field.
#[derive(Default, Clone, Hash)]
pub struct BooleanRewrite;
impl RewriteMethod for BooleanRewrite {
  fn rewrite(&self, sub_queries: Vec<Query>) -> Result<Query> {
    let mut merged = boolean_query::Builder::new();
    for query in sub_queries.into_iter() {
      merged.add(query, Occur::Should)?;
    }
    Ok(merged.build().into())
  }
}
fn adjust_frequencies<IRC>(
  reader_context: &IRC,
  ctx: &TermStates,
  artificial_df: i32,
  artificial_ttf: i64,
) -> Result<TermStates>
where
  IRC: IndexReaderContext + 'static,
{
  let leaves = reader_context.leaves()?;
  let mut new_ctx = TermStates::new(reader_context)?;

  for (i, leaf) in leaves.iter().enumerate() {
    let Some(mut supplier) = ctx.get(leaf)? else {
      continue;
    };

    let Some(term_state) = ctx.resolve(&mut supplier)? else {
      continue;
    };

    new_ctx.register(term_state.clone(), i);
  }
  new_ctx.accumulate_statistics(artificial_df, artificial_ttf);
  Ok(new_ctx)
}

impl crate::core::util::accountable::Accountable for BlendedTermQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED)
  }
}
