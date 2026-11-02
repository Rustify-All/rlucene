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
use crate::core::index::doc_values::DocValues;
use crate::core::index::doc_values_skipper::DocValuesSkipper;
use crate::core::index::index_reader::{Identity, IndexReader};
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::sorted_numeric_doc_values::SortedNumericDocValues;
use crate::core::search::approximation_disi::ApproximationBitSetAccess;
use crate::core::search::approximation_disi::ApproximationDISI;
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::constant_score_weight::ConstantScoreWeight;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::doc_id_set_iterator::{
  AllDISI, DocIdSetIterator, DocIdSetIteratorEnum2, EmptyDISI, RangeDISI,
};
use crate::core::search::doc_values_range_iterator::DocValuesRangeIterator;
use crate::core::search::explanation::Explanation;
use crate::core::search::field_exists_query::FieldExistsQuery;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::match_no_docs_query::MatchNoDocsQuery;
use crate::core::search::query::QueryRef;
use crate::core::search::query::{Query, QueryBase, QueryWeightSsScorer};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::two_phase_iterator::{TwoPhaseIterator, TwoPhaseIteratorEnum2};
use crate::core::search::weight::{DefaultScorerSupplier, Weight};
use crate::core::util::core_helper::HasIdentity;
use crate::core::util::error::lucene_error::Result;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};

#[derive(Debug, Clone)]
pub struct SortedNumericDocValuesRangeQuery {
  id: Identity,
  field: String,
  lower_value: i64,
  upper_value: i64,
}
impl SortedNumericDocValuesRangeQuery {
  pub fn new<FName>(field: FName, lower_value: i64, upper_value: i64) -> Self
  where
    FName: Into<String>,
  {
    let field = field.into();
    Self {
      id: Identity::new(),
      field,
      lower_value,
      upper_value,
    }
  }
}
impl PartialEq for SortedNumericDocValuesRangeQuery {
  fn eq(&self, other: &Self) -> bool {
    self.field == other.field
      && self.lower_value == other.lower_value
      && self.upper_value == other.upper_value
  }
}
impl Eq for SortedNumericDocValuesRangeQuery {}

impl Hash for SortedNumericDocValuesRangeQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.field.hash(state);
    self.lower_value.hash(state);
    self.upper_value.hash(state);
  }
}

impl HasIdentity for SortedNumericDocValuesRangeQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}
impl QueryBase for SortedNumericDocValuesRangeQuery {
  type Weight<IRC, SIM>
    = SortedNumericDocValuesRangeQueryWeight
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static;

  fn to_string(&self, field: &str) -> Result<String> {
    let mut out = String::new();

    if self.field != field {
      out.push_str(&self.field);
      out.push(':');
    }
    out.push('[');
    write!(out, "{} TO {}", self.lower_value, self.upper_value)?;
    out.push(']');
    Ok(out)
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
    Ok(SortedNumericDocValuesRangeQueryWeight::new(
      self,
      *score_mode,
      boost,
    ))
  }

  fn rewrite<IRC, SIM>(&self, _searcher: &IndexSearcher<IRC, SIM>) -> Result<Option<Query>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    Self: Sized,
    IndexSearcher<IRC, SIM>: Sync,
  {
    if self.lower_value == i64::MIN && self.upper_value == i64::MAX {
      return Ok(Some(FieldExistsQuery::new(&self.field).into()));
    }
    if self.lower_value > self.upper_value {
      return Ok(Some(MatchNoDocsQuery::new().into()));
    }
    Ok(None)
  }

  fn visit<QV>(&self, visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    let query = self.into();
    if visitor.accept_field(&self.field) {
      visitor.visit_leaf(query)?;
    }
    Ok(())
  }
}
pub struct SortedNumericDocValuesRangeQueryWeight {
  query: SortedNumericDocValuesRangeQuery,
  base: ConstantScoreWeight,
  score_mode: ScoreMode,
}
impl SortedNumericDocValuesRangeQueryWeight {
  pub(crate) fn new(
    query: SortedNumericDocValuesRangeQuery,
    score_mode: ScoreMode,
    boost: f32,
  ) -> Self {
    Self {
      query,
      base: ConstantScoreWeight::new(boost),
      score_mode,
    }
  }

  fn get_doc_id_set_iterator_or_null_for_primary_sort<NDV, SK, LR>(
    &self,
    reader: &LR,
    numeric_doc_values: &mut NDV,
    skipper: &mut SK,
  ) -> Result<Option<DISI>>
  where
    NDV: NumericDocValues,
    SK: DocValuesSkipper,
    LR: LeafReader,
  {
    if skipper.doc_count() != reader.max_doc()? {
      return Ok(None);
    }

    let index_sort = reader.get_metadata()?.get_sort();
    let index_sort = match index_sort {
      Some(index_sort) => index_sort,
      None => return Ok(None),
    };
    let sort_fields = index_sort.get_sort();
    let Some(first) = sort_fields.first() else {
      return Ok(None);
    };
    if first.get_field() != Some(&self.query.field) {
      return Ok(None);
    }
    let min_doc_id: i32;
    let max_doc_id: i32;

    if first.get_reverse() {
      if skipper.max_value() <= self.query.upper_value {
        min_doc_id = 0;
      } else {
        skipper.advance_with_range(i64::MIN, self.query.upper_value)?;
        min_doc_id = Self::next_doc(skipper.min_doc_id_with_level(0), numeric_doc_values, |l| {
          l <= self.query.upper_value
        })?;
      }

      if skipper.min_value() >= self.query.lower_value {
        max_doc_id = skipper.doc_count();
      } else {
        skipper.advance_with_range(i64::MIN, self.query.lower_value)?;
        max_doc_id = Self::next_doc(skipper.min_doc_id_with_level(0), numeric_doc_values, |l| {
          l < self.query.lower_value
        })?;
      }
    } else {
      if skipper.min_value() >= self.query.lower_value {
        min_doc_id = 0;
      } else {
        skipper.advance_with_range(self.query.lower_value, i64::MAX)?;
        min_doc_id = Self::next_doc(skipper.min_doc_id_with_level(0), numeric_doc_values, |l| {
          l >= self.query.lower_value
        })?;
      }

      if skipper.max_value() <= self.query.upper_value {
        max_doc_id = skipper.doc_count();
      } else {
        skipper.advance_with_range(self.query.upper_value, i64::MAX)?;
        max_doc_id = Self::next_doc(skipper.min_doc_id_with_level(0), numeric_doc_values, |l| {
          l > self.query.upper_value
        })?;
      }
    }

    if min_doc_id == max_doc_id {
      return Ok(Some(DISI::A(EmptyDISI::default())));
    }

    Ok(Some(DISI::B(RangeDISI::new(min_doc_id, max_doc_id)?)))
  }
  fn next_doc<NDV, P>(start_doc: i32, doc_values: &mut NDV, predicate: P) -> Result<i32>
  where
    NDV: NumericDocValues,
    P: Fn(i64) -> bool,
  {
    let mut doc = doc_values.doc_id();
    if start_doc > doc {
      doc = doc_values.advance(start_doc)?;
    }

    while doc < NO_MORE_DOCS {
      if predicate(doc_values.long_value()?) {
        break;
      }
      doc = doc_values.next_doc()?;
    }
    Ok(doc)
  }
}

impl<IRC> SegmentCacheable<IRC> for SortedNumericDocValuesRangeQueryWeight
where
  IRC: IndexReaderContext + 'static,
{
  fn is_cacheable(&self, ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    let field = [self.query.field.as_str()];
    DocValues::is_cacheable(ctx, field)
  }
}

impl<IRC, SIM> Weight<IRC, SIM> for SortedNumericDocValuesRangeQueryWeight
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
{
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
    self.base.explain(scorer, doc, self.query.to_string("")?)
  }

  fn get_query(&self) -> QueryRef<'_> {
    (&self.query).into()
  }

  type ScorerSupplier = DefaultScorerSupplier<QueryWeightSsScorer<IRC, SIM>>;

  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Option<Self::ScorerSupplier>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    if context
      .reader()
      .get_field_infos()?
      .field_info_by_name(&self.query.field)?
      .is_none()
    {
      return Ok(None);
    }
    let mut skipper_opt = context.reader().get_doc_values_skipper(&self.query.field)?;
    if let Some(ref skipper) = skipper_opt {
      if skipper.min_value() > self.query.upper_value
        || skipper.max_value() < self.query.lower_value
      {
        return Ok(None);
      }

      if skipper.doc_count() == context.reader().max_doc()?
        && skipper.min_value() >= self.query.lower_value
        && skipper.max_value() <= self.query.upper_value
      {
        let iter = AllDISI::new(skipper.doc_count());
        let scorer = ConstantScoreScorer::from_disi(self.base.score(), self.score_mode, iter);
        return Ok(Some(DefaultScorerSupplier::new(QueryWeightSsScorer::<
          IRC,
          SIM,
        >::new(scorer))));
      }
    }
    let mut values = DocValues::get_sorted_numeric(context.reader(), &self.query.field)?;
    let iterator = if values.is_single_valued() {
      let mut singleton = DocValues::unwrap_singleton_numeric(&mut values)?;
      match skipper_opt {
        Some(ref mut skipper) => {
          let ps_iterator_opt = self.get_doc_id_set_iterator_or_null_for_primary_sort(
            context.reader(),
            &mut singleton,
            skipper,
          )?;
          if let Some(ps_iterator) = ps_iterator_opt {
            let v = DefaultScorerSupplier::new(QueryWeightSsScorer::<IRC, SIM>::new(
              ConstantScoreScorer::from_disi(self.base.score(), self.score_mode, ps_iterator),
            ));
            return Ok(Some(v));
          } else {
            TwoPhaseIteratorEnum2::A(TwoPhaseIterator3::new(singleton, &self.query))
          }
        },
        None => TwoPhaseIteratorEnum2::A(TwoPhaseIterator3::new(singleton, &self.query)),
      }
    } else {
      TwoPhaseIteratorEnum2::B(TwoPhaseIterator4::new(values, &self.query))
    };
    match skipper_opt {
      Some(skipper) => {
        let v = DocValuesRangeIterator::new(
          iterator,
          skipper,
          self.query.lower_value,
          self.query.upper_value,
          false,
        );
        let scorer = ConstantScoreScorer::from_tpi(self.base.score(), self.score_mode, v);
        let v = DefaultScorerSupplier::new(QueryWeightSsScorer::<IRC, SIM>::new(scorer));
        Ok(Some(v))
      },
      None => {
        let scorer = ConstantScoreScorer::from_tpi(self.base.score(), self.score_mode, iterator);
        let v = DefaultScorerSupplier::new(QueryWeightSsScorer::<IRC, SIM>::new(scorer));
        Ok(Some(v))
      },
    }
  }
}
pub type DISI = DocIdSetIteratorEnum2<EmptyDISI, RangeDISI>;
pub struct TwoPhaseIterator3<N> {
  singleton: N,
  lower_value: i64,
  upper_value: i64,
}
impl<N> TwoPhaseIterator3<N> {
  pub fn new(singleton: N, query: &SortedNumericDocValuesRangeQuery) -> Self {
    TwoPhaseIterator3 {
      singleton,
      lower_value: query.lower_value,
      upper_value: query.upper_value,
    }
  }
}
impl<N> ApproximationDISI for TwoPhaseIterator3<N>
where
  N: NumericDocValues,
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
impl<N> ApproximationBitSetAccess for TwoPhaseIterator3<N>
where
  N: NumericDocValues,
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

impl<N> TwoPhaseIterator for TwoPhaseIterator3<N>
where
  N: NumericDocValues,
{
  fn matches(&mut self) -> Result<bool> {
    let value = self.singleton.long_value()?;
    Ok(value >= self.lower_value && value <= self.upper_value)
  }

  fn match_cost(&self) -> f32 {
    2f32
  }
}

pub struct TwoPhaseIterator4<S> {
  value: S,
  lower_value: i64,
  upper_value: i64,
}

impl<S> TwoPhaseIterator4<S> {
  pub fn new(value: S, query: &SortedNumericDocValuesRangeQuery) -> Self {
    TwoPhaseIterator4 {
      value,
      lower_value: query.lower_value,
      upper_value: query.upper_value,
    }
  }
}

impl<S> ApproximationDISI for TwoPhaseIterator4<S>
where
  S: SortedNumericDocValues,
{
  fn approx_doc_id(&self) -> i32 {
    self.value.doc_id()
  }

  fn approx_next_doc(&mut self) -> Result<i32> {
    self.value.next_doc()
  }

  fn approx_advance(&mut self, target: i32) -> Result<i32> {
    self.value.advance(target)
  }

  fn approx_slow_advance(&mut self, target: i32) -> Result<i32> {
    self.value.slow_advance(target)
  }

  fn approx_cost(&self) -> Result<i64> {
    self.value.cost()
  }
}

impl<S> ApproximationBitSetAccess for TwoPhaseIterator4<S>
where
  S: SortedNumericDocValues,
{
  fn approx_is_bit_iter(&self) -> bool {
    self.value.is_bit_iter()
  }

  fn approx_get(&self, index: usize) -> Result<bool> {
    self.value.get(index)
  }

  fn approx_set_doc_id(&mut self, doc: i32) -> Result<()> {
    self.value.set_doc_id(doc)
  }

  fn approx_bit_set_length(&self) -> Result<usize> {
    self.value.bit_set_length()
  }
}

impl<S> TwoPhaseIterator for TwoPhaseIterator4<S>
where
  S: SortedNumericDocValues,
{
  fn matches(&mut self) -> Result<bool> {
    let lower = self.lower_value;
    let upper = self.upper_value;
    let count = self.value.doc_value_count()?;
    for _ in 0..count {
      let value = self.value.next_value()?;

      if value < lower {
        continue;
      }
      return Ok(value <= upper);
    }

    Ok(false)
  }

  fn match_cost(&self) -> f32 {
    2f32
  }
}

impl crate::core::util::accountable::Accountable for SortedNumericDocValuesRangeQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED)
  }
}
