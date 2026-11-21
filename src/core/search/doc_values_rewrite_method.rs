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
use crate::core::index::automaton_terms_enum::AutomatonTermsEnum;
use crate::core::index::doc_values::DocValues;
use crate::core::index::doc_values_skipper::DocValuesSkipper;
use crate::core::index::dummy::dummy_impacts_enum::DummyImpactsEnum;
use crate::core::index::dummy::dummy_postings_enum::DummyPostingsEnum;
use crate::core::index::filtered_terms_enum::FilteredTermsEnum;
use crate::core::index::index_reader::Identity;
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::sorted_doc_values::SortedDocValues;
use crate::core::index::sorted_set_doc_values::SortedSetDocValues;
use crate::core::index::sorted_set_doc_values_terms_enum::SortedSetDocValuesTermsEnum;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::index::{BytesRef, BytesRefValue, BytesRefValueEnum};
use crate::core::search::approximation_disi::ApproximationBitSetAccess;
use crate::core::search::approximation_disi::ApproximationDISI;
use crate::core::search::automaton_query::AutomatonQuery;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::constant_score_query::ConstantScoreQuery;
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::constant_score_weight::ConstantScoreWeight;
use crate::core::search::disjunction_matches_iterator::from_terms_enum;
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, EmptyDISI};
use crate::core::search::doc_values_range_iterator::DocValuesRangeIterator;
use crate::core::search::explanation::Explanation;
use crate::core::search::fuzzy_query::FuzzyQuery;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::matches_utils::for_field;
use crate::core::search::multi_term_query::{
  MultiTermQuery, MultiTermQuerySet, RewriteMethod, dispatch_multi_term_query,
};
use crate::core::search::query::QueryRef;
use crate::core::search::query::{Query, QueryBase, QueryWeightSsScorer};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::term_in_set_query::TermInSetQuery;
use crate::core::search::two_phase_iterator::{TwoPhaseIterator, TwoPhaseIteratorEnum2};
use crate::core::search::weight::Weight;
use crate::core::util::access::ByteSource;
use crate::core::util::automation::compiled_automaton::CompiledAutomaton;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::core_helper::HasIdentity;
use crate::core::util::dummy::dummy_attribute_source::DummyAttributeSource;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::long_bit_set::LongBitSet;
use std::borrow::Borrow;
use std::cell::RefCell;
use std::fmt::{Debug, Formatter};
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;

/// Rewrites [`MultiTermQuery`] implementations into a filter, using doc values for term
/// enumeration.
///
/// This can be used to perform these queries against an unindexed docvalues field.
#[derive(Default, Clone, Debug, PartialEq, Eq, Hash)]
pub struct DocValuesRewriteMethod;
impl RewriteMethod for DocValuesRewriteMethod {
  fn rewrite<IRC, Q, SIM>(
    &self,
    _index_searcher: &IndexSearcher<IRC, SIM>,
    query: &Q,
  ) -> Result<Query>
  where
    SIM: Similarity,
    Q: MultiTermQuery + Into<MultiTermQuerySet>,
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync,
  {
    Ok(ConstantScoreQuery::new(MultiTermQueryDocValuesWrapper::new(query.clone())).into())
  }
}

#[derive(Clone)]
pub struct MultiTermQueryDocValuesWrapper {
  query: MultiTermQuerySet,
  id: Identity,
}

impl MultiTermQueryDocValuesWrapper {
  pub fn new<T>(query: T) -> Self
  where
    T: Into<MultiTermQuerySet>,
  {
    Self {
      query: query.into(),
      id: Identity::new(),
    }
  }

  pub fn get_field(&self) -> &str {
    dispatch_multi_term_query!(&self.query, |q| q.get_field())
  }
}

impl Debug for MultiTermQueryDocValuesWrapper {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    match self.to_string("") {
      Ok(s) => write!(f, "{}", s),
      Err(_) => Err(std::fmt::Error),
    }
  }
}

impl HasIdentity for MultiTermQueryDocValuesWrapper {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl QueryBase for MultiTermQueryDocValuesWrapper {
  type Weight<IRC, SIM>
    = MultiTermQueryDocValuesWeight
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static;

  fn to_string(&self, field: &str) -> Result<String> {
    self.query.to_string(field)
  }

  fn create_weight<IRC, SIM>(
    self,
    _searcher: &IndexSearcher<IRC, SIM>,
    score_mode: &ScoreMode,
    boost: f32,
  ) -> Result<Self::Weight<IRC, SIM>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    Self: Sized,
    IndexSearcher<IRC, SIM>: Sync,
  {
    Ok(MultiTermQueryDocValuesWeight::new(self, boost, *score_mode))
  }

  fn visit<QV>(&self, visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    if visitor.accept_field(self.get_field()) {
      dispatch_multi_term_query!(&self.query, |query| {
        let _ = visitor.get_sub_visitor(Occur::Filter, query.into());
      });
    }
    Ok(())
  }
}

impl Hash for MultiTermQueryDocValuesWrapper {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    std::any::type_name::<Self>().hash(state);
    self.query.hash(state);
  }
}

impl PartialEq for MultiTermQueryDocValuesWrapper {
  fn eq(&self, other: &Self) -> bool {
    self.query == other.query
  }
}

impl Eq for MultiTermQueryDocValuesWrapper {}

pub struct MultiTermQueryDocValuesWeight {
  parent_query: Arc<MultiTermQueryDocValuesWrapper>,
  base: ConstantScoreWeight,
  score_mode: ScoreMode,
}

impl MultiTermQueryDocValuesWeight {
  fn new(query: MultiTermQueryDocValuesWrapper, boost: f32, score_mode: ScoreMode) -> Self {
    Self {
      parent_query: Arc::new(query),
      base: ConstantScoreWeight::new(boost),
      score_mode,
    }
  }
}

impl<IRC> SegmentCacheable<IRC> for MultiTermQueryDocValuesWeight
where
  IRC: IndexReaderContext + 'static,
{
  fn is_cacheable(&self, ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    let wrapper = self.parent_query.as_ref();
    let field = dispatch_multi_term_query!(&wrapper.query, |q| q.get_field());
    DocValues::is_cacheable(ctx, [field])
  }
}

impl<IRC, SIM> Weight<IRC, SIM> for MultiTermQueryDocValuesWeight
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
    let wrapper = self.parent_query.as_ref();
    let query = &wrapper.query;
    let field = dispatch_multi_term_query!(query, |query| query.get_field());
    for_field(field.to_owned(), (self, context, doc).into())
  }

  fn explain(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Explanation>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let scorer = self.scorer(context, searcher)?;
    self
      .base
      .explain(scorer, doc, self.parent_query.to_string("")?)
  }

  fn get_query(&self) -> QueryRef<'_> {
    QueryRef::MultiTermQueryDocValuesWrapper(self.parent_query.as_ref())
  }

  type ScorerSupplier = DocValuesScorerSupplier;

  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Option<Self::ScorerSupplier>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let wrapper = self.parent_query.as_ref();
    let field = dispatch_multi_term_query!(&wrapper.query, |q| q.get_field());
    let values = DocValues::get_sorted_set(context.reader(), field)?;
    if values.get_value_count()? == 0 {
      return Ok(None);
    }
    Ok(Some(DocValuesScorerSupplier::new(
      self.parent_query.clone(),
      values.cost()?,
      self.base.score(),
      self.score_mode,
    )))
  }
}

pub struct DocValuesScorerSupplier {
  query: Arc<MultiTermQueryDocValuesWrapper>,
  cost: i64,
  score: f32,
  score_mode: ScoreMode,
}

impl DocValuesScorerSupplier {
  fn new(
    query: Arc<MultiTermQueryDocValuesWrapper>,
    cost: i64,
    score: f32,
    score_mode: ScoreMode,
  ) -> Self {
    Self {
      query,
      cost,
      score,
      score_mode,
    }
  }
}

impl<IRC, SIM> ScorerSupplier<IRC, SIM> for DocValuesScorerSupplier
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
{
  type Scorer = QueryWeightSsScorer<IRC, SIM>;
  type BulkScorer = crate::core::search::weight::DefaultBulkScorer<Self::Scorer>;

  fn get(
    &mut self,
    _lead_cost: i64,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Self::Scorer>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let wrapper = self.query.as_ref();
    let query = &wrapper.query;
    let field = dispatch_multi_term_query!(query, |q| q.get_field());
    let values = DocValues::get_sorted_set(context.reader(), field)?;
    let term_values = RefCell::new(values);
    let terms = DocValuesTerms {
      values: &term_values,
      source_type: std::marker::PhantomData,
    };
    let mut terms_enum = get_terms_enum(query, terms)?;

    if terms_enum.next()?.is_none() {
      let v = ConstantScoreScorer::from_disi(self.score, self.score_mode, EmptyDISI::default());
      return Ok(QueryWeightSsScorer::<IRC, SIM>::new(v));
    }

    let mut skipper_opt = context.reader().get_doc_values_skipper(field)?;
    // Create a bit set for the "term set" ordinals (these are the terms provided by the
    // query that are actually present in the doc values field). Cannot use FixedBitSet
    // because we require long index (ord):
    let mut term_set = LongBitSet::new(term_values.borrow().get_value_count()? as usize)?;
    let min_ord = terms_enum.ord()?;
    debug_assert!(min_ord >= 0);
    let mut max_ord = -1;

    loop {
      let ord = terms_enum.ord()?;
      debug_assert!(ord >= 0 && ord > max_ord);
      max_ord = ord;
      term_set.set(ord as usize);
      if terms_enum.next()?.is_none() {
        break;
      }
    }
    // Dictionary lookups leave the document cursor untouched. Reclaim its owner
    // once the terms enum no longer borrows it.
    drop(terms_enum);
    let mut values = term_values.into_inner();

    if let Some(ref skipper) = skipper_opt
      && (min_ord > skipper.max_value() || max_ord < skipper.min_value())
    {
      let v = ConstantScoreScorer::from_disi(self.score, self.score_mode, EmptyDISI::default());
      return Ok(QueryWeightSsScorer::<IRC, SIM>::new(v));
    }

    let iterator = if values.is_single_valued() {
      let singleton = DocValues::unwrap_singleton_sorted(&mut values)?;
      TwoPhaseIteratorEnum2::A(SingletonTermSetTwoPhaseIterator::new(singleton, term_set))
    } else {
      TwoPhaseIteratorEnum2::B(SortedSetTermSetTwoPhaseIterator::new(
        values, term_set, max_ord,
      ))
    };

    match skipper_opt.take() {
      Some(skipper) => {
        let v = DocValuesRangeIterator::new(iterator, skipper, min_ord, max_ord, true);
        Ok(QueryWeightSsScorer::<IRC, SIM>::new(
          ConstantScoreScorer::from_tpi(self.score, self.score_mode, v),
        ))
      },
      None => Ok(QueryWeightSsScorer::<IRC, SIM>::new(
        ConstantScoreScorer::from_tpi(self.score, self.score_mode, iterator),
      )),
    }
  }

  fn bulk_scorer(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Option<Self::BulkScorer>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    Ok(Some(self.default_bulk_scorer(context, searcher)?))
  }

  fn cost(
    &mut self,
    _context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<i64>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    Ok(self.cost)
  }
}
/// Create a [`TermsEnum`] that provides the intersection of the query terms with the terms
/// present in the doc values.
pub(crate) fn get_terms_enum<S, V>(
  query: &MultiTermQuerySet,
  terms: DocValuesTerms<S, V>,
) -> Result<MultiTermQueryDocValuesTermsEnum<S, V>>
where
  S: SortedSetDocValues,
  V: Borrow<RefCell<S>> + Clone,
{
  match query {
    MultiTermQuerySet::Automaton(q) => Ok(MultiTermQueryDocValuesTermsEnum::Automaton(
      q.get_terms_enum(terms)?,
    )),
    MultiTermQuerySet::Fuzzy(q) => Ok(MultiTermQueryDocValuesTermsEnum::Fuzzy(
      q.get_terms_enum(terms)?,
    )),
    MultiTermQuerySet::Prefix(q) => Ok(MultiTermQueryDocValuesTermsEnum::Automaton(
      q.get_terms_enum(terms)?,
    )),
    MultiTermQuerySet::Regexp(q) => Ok(MultiTermQueryDocValuesTermsEnum::Automaton(
      q.get_terms_enum(terms)?,
    )),
    MultiTermQuerySet::TermInSet(q) => Ok(MultiTermQueryDocValuesTermsEnum::TermInSet(
      q.get_terms_enum(terms)?,
    )),
    MultiTermQuerySet::TermRange(q) => Ok(MultiTermQueryDocValuesTermsEnum::Automaton(
      q.get_terms_enum(terms)?,
    )),
    MultiTermQuerySet::Wildcard(q) => Ok(MultiTermQueryDocValuesTermsEnum::Automaton(
      q.get_terms_enum(terms)?,
    )),
    #[cfg(test)]
    MultiTermQuerySet::BoostChecking(q) => Ok(MultiTermQueryDocValuesTermsEnum::BoostChecking(
      q.get_terms_enum(terms)?,
    )),
    #[cfg(test)]
    MultiTermQuerySet::DumbPrefix(q) => Ok(MultiTermQueryDocValuesTermsEnum::DumbPrefix(
      q.get_terms_enum(terms)?,
    )),
    #[cfg(test)]
    MultiTermQuerySet::DumbRegexp(q) => Ok(MultiTermQueryDocValuesTermsEnum::DumbRegexp(
      q.get_terms_enum(terms)?,
    )),
  }
}

pub struct SingletonTermSetTwoPhaseIterator<S> {
  singleton: S,
  term_set: LongBitSet,
}

impl<S> SingletonTermSetTwoPhaseIterator<S> {
  fn new(singleton: S, term_set: LongBitSet) -> Self {
    Self {
      singleton,
      term_set,
    }
  }
}

impl<S> ApproximationDISI for SingletonTermSetTwoPhaseIterator<S>
where
  S: SortedDocValues,
{
  fn approx_doc_id(&self) -> i32 {
    self.singleton.doc_id()
  }

  fn approx_next_doc(&mut self) -> Result<i32> {
    self.singleton.next_doc()
  }

  fn approx_advance(&mut self, target: i32) -> Result<i32> {
    self.singleton.advance(target)
  }

  fn approx_slow_advance(&mut self, target: i32) -> Result<i32> {
    self.singleton.slow_advance(target)
  }

  fn approx_cost(&self) -> Result<i64> {
    self.singleton.cost()
  }
}

impl<S> ApproximationBitSetAccess for SingletonTermSetTwoPhaseIterator<S>
where
  S: SortedDocValues,
{
  fn approx_is_bit_iter(&self) -> bool {
    self.singleton.is_bit_iter()
  }

  fn approx_get(&self, index: usize) -> Result<bool> {
    self.singleton.get(index)
  }

  fn approx_set_doc_id(&mut self, doc: i32) -> Result<()> {
    self.singleton.set_doc_id(doc)
  }

  fn approx_bit_set_length(&self) -> Result<usize> {
    self.singleton.bit_set_length()
  }
}

impl<S> TwoPhaseIterator for SingletonTermSetTwoPhaseIterator<S>
where
  S: SortedDocValues,
{
  fn matches(&mut self) -> Result<bool> {
    Ok(self.term_set.get(self.singleton.ord_value()? as usize))
  }

  fn match_cost(&self) -> f32 {
    3.0
  }
}

pub struct SortedSetTermSetTwoPhaseIterator<S> {
  values: S,
  term_set: LongBitSet,
  max_ord: i64,
}

impl<S> SortedSetTermSetTwoPhaseIterator<S> {
  fn new(values: S, term_set: LongBitSet, max_ord: i64) -> Self {
    Self {
      values,
      term_set,
      max_ord,
    }
  }
}

impl<S> ApproximationDISI for SortedSetTermSetTwoPhaseIterator<S>
where
  S: SortedSetDocValues,
{
  fn approx_doc_id(&self) -> i32 {
    self.values.doc_id()
  }

  fn approx_next_doc(&mut self) -> Result<i32> {
    self.values.next_doc()
  }

  fn approx_advance(&mut self, target: i32) -> Result<i32> {
    self.values.advance(target)
  }

  fn approx_slow_advance(&mut self, target: i32) -> Result<i32> {
    self.values.slow_advance(target)
  }

  fn approx_cost(&self) -> Result<i64> {
    self.values.cost()
  }
}

impl<S> ApproximationBitSetAccess for SortedSetTermSetTwoPhaseIterator<S>
where
  S: SortedSetDocValues,
{
  fn approx_is_bit_iter(&self) -> bool {
    self.values.is_bit_iter()
  }

  fn approx_get(&self, index: usize) -> Result<bool> {
    self.values.get(index)
  }

  fn approx_set_doc_id(&mut self, doc: i32) -> Result<()> {
    self.values.set_doc_id(doc)
  }

  fn approx_bit_set_length(&self) -> Result<usize> {
    self.values.bit_set_length()
  }
}

impl<S> TwoPhaseIterator for SortedSetTermSetTwoPhaseIterator<S>
where
  S: SortedSetDocValues,
{
  fn matches(&mut self) -> Result<bool> {
    for _ in 0..self.values.doc_value_count()? {
      let value = self.values.next_ord()?;
      if value > self.max_ord {
        return Ok(false);
      } else if self.term_set.get(value as usize) {
        return Ok(true);
      }
    }
    Ok(false)
  }

  fn match_cost(&self) -> f32 {
    3.0
  }
}

pub struct DocValuesTerms<S, V = Rc<RefCell<S>>> {
  values: V,
  source_type: std::marker::PhantomData<fn() -> S>,
}

impl<S> DocValuesTerms<S> {
  fn new(values: S) -> Self {
    Self {
      values: Rc::new(RefCell::new(values)),
      source_type: std::marker::PhantomData,
    }
  }
}

impl<S, V: Clone> Clone for DocValuesTerms<S, V> {
  fn clone(&self) -> Self {
    Self {
      values: <V as Clone>::clone(&self.values),
      source_type: std::marker::PhantomData,
    }
  }
}

impl<S, V> Terms for DocValuesTerms<S, V>
where
  S: SortedSetDocValues,
  V: Borrow<RefCell<S>> + Clone,
{
  type TermsEnum = SortedSetDocValuesTermsEnum<Self>;

  fn iterator(&self) -> Result<<Self as Terms>::TermsEnum> {
    Ok(SortedSetDocValuesTermsEnum::new(<Self as Clone>::clone(
      self,
    )))
  }

  type IntersectIter = FilteredTermsEnum<<Self as Terms>::TermsEnum, AutomatonTermsEnum>;

  fn intersect(
    &self,
    compiled: &CompiledAutomaton,
    start_term: Option<&BytesRef<Vec<u8>>>,
  ) -> Result<Self::IntersectIter> {
    self.default_intersect(compiled, start_term)
  }

  fn size(&self) -> Result<i64> {
    Ok(-1)
  }

  fn get_sum_total_term_freq(&self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_sum_doc_freq(&self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_doc_count(&self) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn has_freqs(&self) -> bool {
    false
  }

  fn has_offsets(&self) -> bool {
    false
  }

  fn has_positions(&self) -> bool {
    false
  }

  fn has_payloads(&self) -> bool {
    false
  }
}

impl<S, V> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for DocValuesTerms<S, V>
where
  S: SortedSetDocValues,
  V: Borrow<RefCell<S>> + Clone,
{
}
impl<S, V> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for DocValuesTerms<S, V>
where
  S: SortedSetDocValues,
  V: Borrow<RefCell<S>> + Clone,
{
}
impl<S, V> DocIdSetIterator for DocValuesTerms<S, V>
where
  S: SortedSetDocValues,
  V: Borrow<RefCell<S>> + Clone,
{
  fn doc_id(&self) -> i32 {
    let values: &RefCell<S> = self.values.borrow();
    values.borrow().doc_id()
  }
  fn next_doc(&mut self) -> Result<i32> {
    let values: &RefCell<S> = self.values.borrow();
    values.borrow_mut().next_doc()
  }
  fn advance(&mut self, target: i32) -> Result<i32> {
    let values: &RefCell<S> = self.values.borrow();
    values.borrow_mut().advance(target)
  }
  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    let values: &RefCell<S> = self.values.borrow();
    values.borrow_mut().slow_advance(target)
  }
  fn cost(&self) -> Result<i64> {
    let values: &RefCell<S> = self.values.borrow();
    values.borrow().cost()
  }
}
impl<S, V> crate::core::index::doc_values_iterator::DocValuesIterator for DocValuesTerms<S, V>
where
  S: SortedSetDocValues,
  V: Borrow<RefCell<S>> + Clone,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    let values: &RefCell<S> = self.values.borrow();
    values.borrow_mut().advance_exact(target)
  }
}
impl<S, V> SortedSetDocValues for DocValuesTerms<S, V>
where
  S: SortedSetDocValues,
  V: Borrow<RefCell<S>> + Clone,
{
  type OrdValue<'a>
    = BytesRef<Vec<u8>>
  where
    Self: 'a;
  fn next_ord(&mut self) -> Result<i64> {
    let values: &RefCell<S> = self.values.borrow();
    values.borrow_mut().next_ord()
  }
  fn doc_value_count(&mut self) -> Result<i32> {
    let values: &RefCell<S> = self.values.borrow();
    values.borrow_mut().doc_value_count()
  }
  fn lookup_ord(&mut self, ord: i64) -> Result<Self::OrdValue<'_>> {
    let values: &RefCell<S> = self.values.borrow();
    Ok(values.borrow_mut().lookup_ord(ord)?.into_owned())
  }
  fn get_value_count(&self) -> Result<i64> {
    let values: &RefCell<S> = self.values.borrow();
    values.borrow().get_value_count()
  }
  fn lookup_term<BS: ByteSource>(&mut self, key: &BytesRef<BS>) -> Result<i64> {
    let values: &RefCell<S> = self.values.borrow();
    values.borrow_mut().lookup_term(key)
  }
  type TermsEnum<'a>
    = SortedSetDocValuesTermsEnum<&'a mut Self>
  where
    Self: 'a;
  fn terms_enum(&mut self) -> Result<<Self as SortedSetDocValues>::TermsEnum<'_>> {
    self.default_terms_enum()
  }
  fn is_single_valued(&self) -> bool {
    let values: &RefCell<S> = self.values.borrow();
    values.borrow().is_single_valued()
  }
  type SortedDocValues = crate::core::index::doc_values::EmptySorted;
}

#[allow(clippy::large_enum_variant)] // Keep doc-values terms iteration allocation-free.
pub enum MultiTermQueryDocValuesTermsEnum<S, V = Rc<RefCell<S>>>
where
  S: SortedSetDocValues,
  V: Borrow<RefCell<S>> + Clone,
{
  Automaton(<AutomatonQuery as MultiTermQuery>::TermsEnum<DocValuesTerms<S, V>>),
  Fuzzy(<FuzzyQuery as MultiTermQuery>::TermsEnum<DocValuesTerms<S, V>>),
  TermInSet(<TermInSetQuery as MultiTermQuery>::TermsEnum<DocValuesTerms<S, V>>),
  #[cfg(test)]
  BoostChecking(
    <crate::test_framework::core::search::multi_term::BoostCheckingQuery as MultiTermQuery>::TermsEnum<
      DocValuesTerms<S, V>,
    >,
  ),
  #[cfg(test)]
  DumbPrefix(
    <crate::test_framework::core::search::multi_term::DumbPrefixQuery as MultiTermQuery>::TermsEnum<
      DocValuesTerms<S, V>,
    >,
  ),
  #[cfg(test)]
  DumbRegexp(
    <crate::test_framework::core::search::multi_term::DumbRegexpQuery as MultiTermQuery>::TermsEnum<
      DocValuesTerms<S, V>,
    >,
  ),
}

macro_rules! dispatch_doc_values_terms_enum {
  ($self:expr, |$inner:ident| $body:expr) => {{
    match $self {
      MultiTermQueryDocValuesTermsEnum::Automaton($inner) => $body,
      MultiTermQueryDocValuesTermsEnum::Fuzzy($inner) => $body,
      MultiTermQueryDocValuesTermsEnum::TermInSet($inner) => $body,
      #[cfg(test)]
      MultiTermQueryDocValuesTermsEnum::BoostChecking($inner) => $body,
      #[cfg(test)]
      MultiTermQueryDocValuesTermsEnum::DumbPrefix($inner) => $body,
      #[cfg(test)]
      MultiTermQueryDocValuesTermsEnum::DumbRegexp($inner) => $body,
    }
  }};
}

impl<S, V> BytesRefIterator for MultiTermQueryDocValuesTermsEnum<S, V>
where
  S: SortedSetDocValues,
  V: Borrow<RefCell<S>> + Clone,
{
  type Value<'a>
    = BytesRefValueEnum<'a>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    dispatch_doc_values_terms_enum!(self, |terms_enum| terms_enum
      .next()
      .map(|value| value.map(BytesRefValue::into_value)))
  }
}

impl<S, V> TermsEnum for MultiTermQueryDocValuesTermsEnum<S, V>
where
  S: SortedSetDocValues,
  V: Borrow<RefCell<S>> + Clone,
{
  type AttributeSource<'a>
    = &'a DummyAttributeSource
  where
    Self: 'a;
  type AttributeSourceMut<'a>
    = &'a mut DummyAttributeSource
  where
    Self: 'a;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn seek_exact<BS: ByteSource>(&mut self, text: &BytesRef<BS>) -> Result<bool> {
    dispatch_doc_values_terms_enum!(self, |terms_enum| terms_enum.seek_exact(text))
  }

  #[cfg(not(test))]
  crate::core::index::terms_enum::prepare_seek_exact_enum!(
    Automaton(<AutomatonQuery as MultiTermQuery>::TermsEnum<DocValuesTerms<S, V>>) => A,
    Fuzzy(<FuzzyQuery as MultiTermQuery>::TermsEnum<DocValuesTerms<S, V>>) => B,
    TermInSet(<TermInSetQuery as MultiTermQuery>::TermsEnum<DocValuesTerms<S, V>>) => C,
  );

  #[cfg(test)]
  crate::core::index::terms_enum::prepare_seek_exact_enum!(
    Automaton(<AutomatonQuery as MultiTermQuery>::TermsEnum<DocValuesTerms<S, V>>) => A,
    Fuzzy(<FuzzyQuery as MultiTermQuery>::TermsEnum<DocValuesTerms<S, V>>) => B,
    TermInSet(<TermInSetQuery as MultiTermQuery>::TermsEnum<DocValuesTerms<S, V>>) => C,
    BoostChecking(<crate::test_framework::core::search::multi_term::BoostCheckingQuery as MultiTermQuery>::TermsEnum<DocValuesTerms<S, V>>) => D,
    DumbPrefix(<crate::test_framework::core::search::multi_term::DumbPrefixQuery as MultiTermQuery>::TermsEnum<DocValuesTerms<S, V>>) => E,
    DumbRegexp(<crate::test_framework::core::search::multi_term::DumbRegexpQuery as MultiTermQuery>::TermsEnum<DocValuesTerms<S, V>>) => F,
  );

  fn seek_ceil<BS: ByteSource>(&mut self, text: &BytesRef<BS>) -> Result<SeekStatus> {
    dispatch_doc_values_terms_enum!(self, |terms_enum| terms_enum.seek_ceil(text))
  }

  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    dispatch_doc_values_terms_enum!(self, |terms_enum| terms_enum.seek_exact_with_ord(ord))
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    state: &crate::core::codecs::block_term_state::TermStateEnum,
  ) -> Result<()> {
    dispatch_doc_values_terms_enum!(self, |terms_enum| terms_enum
      .seek_exact_with_state(term, state))
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    dispatch_doc_values_terms_enum!(self, |terms_enum| terms_enum
      .term()
      .map(BytesRefValue::into_value))
  }

  fn ord(&self) -> Result<i64> {
    dispatch_doc_values_terms_enum!(self, |terms_enum| terms_enum.ord())
  }

  fn doc_freq(&mut self) -> Result<i32> {
    dispatch_doc_values_terms_enum!(self, |terms_enum| terms_enum.doc_freq())
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    dispatch_doc_values_terms_enum!(self, |terms_enum| terms_enum.total_term_freq())
  }

  type PostingsEnum = DummyPostingsEnum;

  fn postings(&mut self, reuse: Option<Self::PostingsEnum>) -> Result<Self::PostingsEnum> {
    dispatch_doc_values_terms_enum!(self, |terms_enum| terms_enum.postings(reuse))
  }

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    dispatch_doc_values_terms_enum!(self, |terms_enum| terms_enum
      .postings_with_flags(reuse, flags))
  }

  type ImpactsEnum = DummyImpactsEnum;

  fn impacts(&mut self, _flags: i32) -> Result<Self::ImpactsEnum> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn term_state(&mut self) -> Result<crate::core::codecs::block_term_state::TermStateEnum> {
    dispatch_doc_values_terms_enum!(self, |terms_enum| terms_enum.term_state())
  }
}

impl crate::core::util::accountable::Accountable for MultiTermQueryDocValuesWrapper {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED)
  }
}

impl MultiTermQueryDocValuesWeight {
  pub(crate) fn matches_iterator<'a, IRC: IndexReaderContext + 'static, SIM>(
    &'a self,
    context: &'a LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
  ) -> Result<
    Option<crate::core::search::query::QueryWeightMatchesIterator<'a, IRCLeafReader<IRC>, SIM>>,
  >
  where
    SIM: Similarity,
  {
    let wrapper = self.parent_query.as_ref();
    let query = &wrapper.query;
    let field = dispatch_multi_term_query!(query, |q| q.get_field());
    let values = DocValues::get_sorted_set(context.reader(), field)?;
    let terms_enum = get_terms_enum(query, DocValuesTerms::new(values))?;
    from_terms_enum(
      context,
      doc,
      Arc::new(wrapper.query.clone().into()),
      field,
      terms_enum,
    )
    .map(|value| value.map(|iterator| Box::new(iterator).into()))
  }
}
