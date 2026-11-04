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
use crate::core::geo::component2d::Component2D;
use crate::core::geo::geometry::Geometry;
use crate::core::geo::xy_encoding_utils::XYEncodingUtils;
use crate::core::geo::xy_geometry;
use crate::core::geo::xy_geometry::{XYGeometryEnum, XYGeometryType};
use crate::core::index::doc_values::DocValues;
use crate::core::index::index_reader::Identity;
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::sorted_numeric_doc_values::SortedNumericDocValues;
use crate::core::search::approximation_disi::ApproximationBitSetAccess;
use crate::core::search::approximation_disi::ApproximationDISI;
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::constant_score_scorer::ConstantScoreTwoPhaseScorer;
use crate::core::search::constant_score_weight::ConstantScoreWeight;
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
use crate::core::util::core_helper::HasIdentity;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::fmt::{Debug, Formatter, Write};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// XYGeometry query for XY doc values.
///
/// The field must be indexed using XY doc values encoded as `x << 32 | y`.
#[derive(Clone)]
pub struct XYDocValuesPointInGeometryQuery {
  id: Identity,
  field: String,
  geometries: Arc<[XYGeometryEnum]>,
  component2d: Arc<XYGeometryType<<XYGeometryEnum as Geometry>::Component2D>>,
}

impl XYDocValuesPointInGeometryQuery {
  pub(crate) fn new(field: String, geometries: Vec<XYGeometryEnum>) -> Result<Self> {
    if geometries.is_empty() {
      return Err(LuceneError::illegal_argument(
        "geometries must not be empty",
      ));
    }
    let component2d = Arc::new(xy_geometry::create(geometries.as_slice())?);
    let geometries = Arc::from(geometries);
    Ok(Self {
      id: Identity::new(),
      field,
      geometries,
      component2d,
    })
  }
}

impl Debug for XYDocValuesPointInGeometryQuery {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("XYDocValuesPointInGeometryQuery")
      .field("field", &self.field)
      .finish()
  }
}

impl PartialEq for XYDocValuesPointInGeometryQuery {
  fn eq(&self, other: &Self) -> bool {
    self.field == other.field && self.geometries == other.geometries
  }
}

impl Eq for XYDocValuesPointInGeometryQuery {}

impl Hash for XYDocValuesPointInGeometryQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.field.hash(state);
    self.geometries.hash(state);
  }
}

impl HasIdentity for XYDocValuesPointInGeometryQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl QueryBase for XYDocValuesPointInGeometryQuery {
  type Weight<IRC, SIM>
    = XYDocValuesPointInGeometryQueryWeight
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static;

  fn to_string(&self, field: &str) -> Result<String> {
    let mut sb = String::new();
    if self.field != field {
      sb.push_str(&self.field);
      sb.push(':');
    }
    sb.push_str("geometries(");
    for (i, geometry) in self.geometries.iter().enumerate() {
      if i > 0 {
        sb.push_str(", ");
      }
      write!(sb, "{geometry}")?;
    }
    sb.push(')');
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
    Ok(XYDocValuesPointInGeometryQueryWeight::new(
      self,
      *score_mode,
      boost,
    ))
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

pub struct XYDocValuesPointInGeometryQueryWeight {
  base: ConstantScoreWeight,
  parent_query: XYDocValuesPointInGeometryQuery,
  score_mode: ScoreMode,
  boost: f32,
}

impl XYDocValuesPointInGeometryQueryWeight {
  fn new(query: XYDocValuesPointInGeometryQuery, score_mode: ScoreMode, boost: f32) -> Self {
    let parent_query = query;
    Self {
      base: ConstantScoreWeight::new(boost),
      parent_query,
      score_mode,
      boost,
    }
  }
}

impl<IRC> SegmentCacheable<IRC> for XYDocValuesPointInGeometryQueryWeight
where
  IRC: IndexReaderContext + 'static,
{
  fn is_cacheable(&self, ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    let query = &self.parent_query;
    let field = [query.field.as_str()];
    DocValues::is_cacheable(ctx, field)
  }
}

impl<IRC, SIM> Weight<IRC, SIM> for XYDocValuesPointInGeometryQueryWeight
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
    QueryRef::XYDocValuesPointInGeometry(&self.parent_query)
  }

  type ScorerSupplier = DefaultScorerSupplier<
    ConstantScoreTwoPhaseScorer<
      XYDocValuesPointInGeometryTPI<
        <IRCLeafReader<IRC> as LeafReader>::SortedNumericDocValues,
        Arc<XYGeometryType<<XYGeometryEnum as Geometry>::Component2D>>,
      >,
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
    match context
      .reader()
      .get_sorted_numeric_doc_values(&query.field)?
    {
      Some(values) => {
        let iterator = XYDocValuesPointInGeometryTPI::new(values, query.component2d.clone());
        let scorer = ConstantScoreScorer::from_tpi(self.boost, self.score_mode, iterator);
        Ok(Some(DefaultScorerSupplier::new(scorer)))
      },
      None => Ok(None),
    }
  }
}

pub struct XYDocValuesPointInGeometryTPI<S, C> {
  values: S,
  component2d: C,
}

impl<S, C> XYDocValuesPointInGeometryTPI<S, C> {
  fn new(values: S, component2d: C) -> Self {
    Self {
      values,
      component2d,
    }
  }
}

impl<S, C> ApproximationDISI for XYDocValuesPointInGeometryTPI<S, C>
where
  S: SortedNumericDocValues,
  C: Component2D,
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

impl<S, C> ApproximationBitSetAccess for XYDocValuesPointInGeometryTPI<S, C>
where
  S: SortedNumericDocValues,
  C: Component2D,
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

impl<S, C> TwoPhaseIterator for XYDocValuesPointInGeometryTPI<S, C>
where
  S: SortedNumericDocValues,
  C: Component2D,
{
  fn matches(&mut self) -> Result<bool> {
    let count = self.values.doc_value_count()?;
    for _ in 0..count {
      let value = self.values.next_value()? as u64;
      let x = XYEncodingUtils::decode((value >> 32) as i32) as f64;
      let y = XYEncodingUtils::decode(value as u32 as i32) as f64;
      if self.component2d.contains(x, y) {
        return Ok(true);
      }
    }
    Ok(false)
  }

  fn match_cost(&self) -> f32 {
    1000f32
  }
}

impl crate::core::util::accountable::Accountable for XYDocValuesPointInGeometryQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED)
  }
}
