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
use crate::core::document::binary_range_doc_values::BinaryRangeDocValues;
use crate::core::document::double_range_slow_range_query::DoubleRangeSlowRangeQuery;
use crate::core::document::float_range_slow_range_query::FloatRangeSlowRangeQuery;
use crate::core::document::int_range_slow_range_query::IntRangeSlowRangeQuery;
use crate::core::document::long_range_slow_range_query::LongRangeSlowRangeQuery;
use crate::core::document::range_field_query::QueryType;
use crate::core::index::binary_doc_values::BinaryDocValues;
use crate::core::index::doc_values::DocValues;
use crate::core::index::index_reader::Identity;
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::search::approximation_disi::ApproximationBitSetAccess;
use crate::core::search::approximation_disi::ApproximationDISI;
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::constant_score_scorer::ConstantScoreTwoPhaseScorer;
use crate::core::search::constant_score_weight::ConstantScoreWeight;
use crate::core::search::doc_id_set_iterator::{BitSetIteratorAccess, DocIdSetIterator};
use crate::core::search::explanation::Explanation;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::query::QueryBase;
use crate::core::search::query::QueryRef;
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::two_phase_iterator::TwoPhaseIterator;
use crate::core::search::weight::{DefaultScorerSupplier, Weight};
use crate::core::util::array_util::{ArrayUtil, ByteArrayComparatorEnum};
use crate::core::util::core_helper::HasIdentity;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::impl_from_for_enum;
use std::fmt::Debug;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

#[derive(Clone)]
pub struct BinaryRangeFieldRangeQuery {
  id: Identity,
  data: Arc<BinaryRangeFieldRangeQueryData>,
  num_dims: usize,
  num_bytes_per_dimension: usize,
  query_type: QueryType,
  comparator: ByteArrayComparatorEnum,
}

struct BinaryRangeFieldRangeQueryData {
  query_packed_value: Arc<[u8]>,
  sub: BinaryRangeFieldRangeQueryEnum,
}

impl BinaryRangeFieldRangeQuery {
  pub fn new<T, V>(
    query_packed_value: V,
    num_bytes_per_dimension: usize,
    num_dims: usize,
    query_type: QueryType,
    sub: T,
  ) -> Result<Self>
  where
    T: Into<BinaryRangeFieldRangeQueryEnum>,
    V: Into<Vec<u8>>,
  {
    if query_type != QueryType::Intersects {
      return Err(LuceneError::unsupported_operation(
        "INTERSECTS is the only query type supported for this field type right now",
      ));
    }
    let comparator = ArrayUtil::get_unsigned_comparator(num_bytes_per_dimension);
    let sub = sub.into();
    Ok(Self {
      id: Identity::new(),
      data: Arc::new(BinaryRangeFieldRangeQueryData {
        query_packed_value: {
          let value: Vec<u8> = query_packed_value.into();
          value.into()
        },
        sub,
      }),
      num_dims,
      num_bytes_per_dimension,
      query_type,
      comparator,
    })
  }
}

impl Debug for BinaryRangeFieldRangeQuery {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("BinaryRangeFieldRangeQuery")
      .field("field", &self.data.sub.field())
      .finish()
  }
}

impl PartialEq for BinaryRangeFieldRangeQuery {
  fn eq(&self, other: &Self) -> bool {
    // Preserve the original Arc equality fast path; packed bytes are not part of equality.
    Arc::ptr_eq(&self.data, &other.data) || self.data.sub == other.data.sub
  }
}

impl Eq for BinaryRangeFieldRangeQuery {}

impl Hash for BinaryRangeFieldRangeQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.data.sub.hash(state);
  }
}

impl HasIdentity for BinaryRangeFieldRangeQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl QueryBase for BinaryRangeFieldRangeQuery {
  type Weight<IRC, SIM>
    = BinaryRangeFieldRangeWeight
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static;

  fn to_string(&self, field: &str) -> Result<String> {
    if self.data.sub.field() == field {
      return Ok(self.data.sub.range_string());
    }
    let mut sb = String::new();
    sb.push_str(self.data.sub.field());
    sb.push(':');
    sb.push_str(&self.data.sub.range_string());
    Ok(sb)
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
    Ok(BinaryRangeFieldRangeWeight::new(self, *score_mode, boost))
  }

  fn visit<QV>(&self, visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    let query = self.into();
    if visitor.accept_field(self.data.sub.field()) {
      visitor.visit_leaf(query)?;
    }
    Ok(())
  }
}

pub struct BinaryRangeFieldRangeWeight {
  base: ConstantScoreWeight,
  parent_query: BinaryRangeFieldRangeQuery,
  score_mode: ScoreMode,
  boost: f32,
}

impl BinaryRangeFieldRangeWeight {
  fn new(query: BinaryRangeFieldRangeQuery, score_mode: ScoreMode, boost: f32) -> Self {
    let parent_query = query;
    Self {
      base: ConstantScoreWeight::new(boost),
      parent_query,
      score_mode,
      boost,
    }
  }
}

impl<IRC> SegmentCacheable<IRC> for BinaryRangeFieldRangeWeight
where
  IRC: IndexReaderContext + 'static,
{
  fn is_cacheable(&self, ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    let query = &self.parent_query;
    let field = [query.data.sub.field()];
    DocValues::is_cacheable(ctx, field)
  }
}

impl<IRC, SIM> Weight<IRC, SIM> for BinaryRangeFieldRangeWeight
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
    self
      .base
      .explain(scorer, doc, self.parent_query.to_string("")?)
  }

  fn get_query(&self) -> QueryRef<'_> {
    QueryRef::BinaryRangeFieldRange(&self.parent_query)
  }

  type ScorerSupplier = DefaultScorerSupplier<
    ConstantScoreTwoPhaseScorer<
      BinaryRangeFieldRangeTPI<crate::core::index::doc_values::Binary<IRCLeafReader<IRC>>>,
    >,
  >;

  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Option<Self::ScorerSupplier>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let query = &self.parent_query;
    let field_infos = context.reader().get_field_infos()?;
    let field_info = field_infos.field_info_by_name(query.data.sub.field())?;
    if field_info.is_none() {
      return Ok(None);
    }
    let binary_doc_values = DocValues::get_binary(context.reader(), query.data.sub.field())?;
    let brdv = BinaryRangeDocValues::new(
      binary_doc_values,
      query.num_dims,
      query.num_bytes_per_dimension,
    );
    let tpi = BinaryRangeFieldRangeTPI::new(
      brdv,
      query.data.query_packed_value.clone(),
      query.num_dims,
      query.num_bytes_per_dimension,
      query.query_type,
      query.comparator.clone(),
    );
    let scorer = ConstantScoreScorer::from_tpi(self.boost, self.score_mode, tpi);
    Ok(Some(DefaultScorerSupplier::new(scorer)))
  }
}

/// TwoPhaseIterator for BinaryRangeFieldRangeQuery.
///
/// Wraps a BinaryRangeDocValues (which itself wraps BinaryDocValues) and
/// uses the QueryType's matches method to check if each document's packed
/// value matches the query's packed value.
pub struct BinaryRangeFieldRangeTPI<T> {
  values: BinaryRangeDocValues<T>,
  query_packed_value: Arc<[u8]>,
  num_dims: usize,
  num_bytes_per_dimension: usize,
  query_type: QueryType,
  comparator: ByteArrayComparatorEnum,
}

impl<T> BinaryRangeFieldRangeTPI<T> {
  fn new(
    values: BinaryRangeDocValues<T>,
    query_packed_value: Arc<[u8]>,
    num_dims: usize,
    num_bytes_per_dimension: usize,
    query_type: QueryType,
    comparator: ByteArrayComparatorEnum,
  ) -> Self {
    Self {
      values,
      query_packed_value,
      num_dims,
      num_bytes_per_dimension,
      query_type,
      comparator,
    }
  }
}

impl<T> ApproximationDISI for BinaryRangeFieldRangeTPI<T>
where
  T: BinaryDocValues,
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

impl<T> ApproximationBitSetAccess for BinaryRangeFieldRangeTPI<T>
where
  T: BinaryDocValues,
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

impl<T> TwoPhaseIterator for BinaryRangeFieldRangeTPI<T>
where
  T: BinaryDocValues,
{
  fn matches(&mut self) -> Result<bool> {
    let packed_value = self.values.get_packed_value();
    self.query_type.matches(
      &self.query_packed_value,
      packed_value,
      self.num_dims,
      self.num_bytes_per_dimension,
      &self.comparator,
    )
  }

  fn match_cost(&self) -> f32 {
    self.query_packed_value.len() as f32
  }
}

#[derive(Clone)]
pub enum BinaryRangeFieldRangeQueryEnum {
  Double(DoubleRangeSlowRangeQuery),
  Int(IntRangeSlowRangeQuery),
  Float(FloatRangeSlowRangeQuery),
  Long(LongRangeSlowRangeQuery),
}
impl BinaryRangeFieldRangeQueryEnum {
  fn field(&self) -> &str {
    match self {
      BinaryRangeFieldRangeQueryEnum::Double(v) => v.field(),
      BinaryRangeFieldRangeQueryEnum::Int(v) => v.field(),
      BinaryRangeFieldRangeQueryEnum::Float(v) => v.field(),
      BinaryRangeFieldRangeQueryEnum::Long(v) => v.field(),
    }
  }

  fn range_string(&self) -> String {
    match self {
      BinaryRangeFieldRangeQueryEnum::Double(v) => {
        format!("[{:?} TO {:?}]", v.min(), v.max())
      },
      BinaryRangeFieldRangeQueryEnum::Int(v) => {
        format!("[{:?} TO {:?}]", v.min(), v.max())
      },
      BinaryRangeFieldRangeQueryEnum::Float(v) => {
        format!("[{:?} TO {:?}]", v.min(), v.max())
      },
      BinaryRangeFieldRangeQueryEnum::Long(v) => {
        format!("[{:?} TO {:?}]", v.min(), v.max())
      },
    }
  }
}
impl Hash for BinaryRangeFieldRangeQueryEnum {
  fn hash<H: Hasher>(&self, state: &mut H) {
    match self {
      BinaryRangeFieldRangeQueryEnum::Double(v) => v.hash(state),
      BinaryRangeFieldRangeQueryEnum::Int(v) => v.hash(state),
      BinaryRangeFieldRangeQueryEnum::Float(v) => v.hash(state),
      BinaryRangeFieldRangeQueryEnum::Long(v) => v.hash(state),
    }
  }
}
impl PartialEq for BinaryRangeFieldRangeQueryEnum {
  fn eq(&self, other: &Self) -> bool {
    match (self, other) {
      (BinaryRangeFieldRangeQueryEnum::Double(v1), BinaryRangeFieldRangeQueryEnum::Double(v2)) => {
        v1 == v2
      },
      (BinaryRangeFieldRangeQueryEnum::Int(v1), BinaryRangeFieldRangeQueryEnum::Int(v2)) => {
        v1 == v2
      },
      (BinaryRangeFieldRangeQueryEnum::Float(v1), BinaryRangeFieldRangeQueryEnum::Float(v2)) => {
        v1 == v2
      },
      (BinaryRangeFieldRangeQueryEnum::Long(v1), BinaryRangeFieldRangeQueryEnum::Long(v2)) => {
        v1 == v2
      },
      _ => false,
    }
  }
}
impl Eq for BinaryRangeFieldRangeQueryEnum {}
impl_from_for_enum!(
    BinaryRangeFieldRangeQueryEnum,
    DoubleRangeSlowRangeQuery=> Double,
    IntRangeSlowRangeQuery=> Int,
    FloatRangeSlowRangeQuery=> Float,
  LongRangeSlowRangeQuery => Long,
);

impl crate::core::util::accountable::Accountable for BinaryRangeFieldRangeQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED)
  }
}
