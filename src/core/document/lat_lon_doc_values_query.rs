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
use crate::core::document::shape_field::QueryRelation;
use crate::core::geo::component2d::{Component2D, WithinRelation};
use crate::core::geo::geo_encoding_utils::{Component2DPredicate, GeoEncodingUtils};
use crate::core::geo::geometry::Geometry;
use crate::core::geo::lat_lon_geometry;
use crate::core::geo::lat_lon_geometry::{
  LatLonGeometryEnum, LatLonGeometryEnumComponent2D, LatLonGeometryType,
};
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

/// Finds all previously indexed geo points that comply the given [`QueryRelation`] with the
/// specified array of [`LatLonGeometryEnum`].
///
/// The field must be indexed using LatLonDocValuesField added per document.
#[derive(Clone)]
pub struct LatLonDocValuesQuery {
  id: Identity,
  field: String,
  data: Arc<LatLonDocValuesQueryData>,
  query_relation: QueryRelation,
}

struct LatLonDocValuesQueryData {
  geometries: Box<[LatLonGeometryEnum]>,
  component2d: Arc<LatLonGeometryType<<LatLonGeometryEnum as Geometry>::Component2D>>,
}

impl LatLonDocValuesQuery {
  pub(crate) fn new(
    field: String,
    query_relation: QueryRelation,
    geometries: Vec<LatLonGeometryEnum>,
  ) -> Result<Self> {
    if query_relation == QueryRelation::Within {
      for geometry in &geometries {
        if matches!(geometry, LatLonGeometryEnum::Line(_)) {
          return Err(LuceneError::illegal_argument(format!(
            "LatLonDocValuesPointQuery does not support {:?} queries with line geometries",
            QueryRelation::Within
          )));
        }
      }
    }
    if query_relation == QueryRelation::Contains {
      for geometry in &geometries {
        if !matches!(geometry, LatLonGeometryEnum::Point(_)) {
          return Err(LuceneError::illegal_argument(format!(
            "LatLonDocValuesPointQuery does not support {:?} queries with non-points geometries",
            QueryRelation::Contains
          )));
        }
      }
    }
    let component2d = Arc::new(lat_lon_geometry::create(geometries.as_slice())?);
    let data = Arc::new(LatLonDocValuesQueryData {
      geometries: geometries.into_boxed_slice(),
      component2d,
    });
    Ok(Self {
      id: Identity::new(),
      field,
      data,
      query_relation,
    })
  }
}

impl Debug for LatLonDocValuesQuery {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("LatLonDocValuesQuery")
      .field("field", &self.field)
      .field("query_relation", &self.query_relation)
      .finish()
  }
}

impl PartialEq for LatLonDocValuesQuery {
  fn eq(&self, other: &Self) -> bool {
    // Preserve the original shared-geometry equality fast path.
    self.field == other.field
      && self.query_relation == other.query_relation
      && (Arc::ptr_eq(&self.data, &other.data) || self.data.geometries == other.data.geometries)
  }
}

impl Eq for LatLonDocValuesQuery {}

impl Hash for LatLonDocValuesQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.field.hash(state);
    self.query_relation.hash(state);
    self.data.geometries.hash(state);
  }
}

impl HasIdentity for LatLonDocValuesQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl QueryBase for LatLonDocValuesQuery {
  type Weight<IRC, SIM>
    = LatLonDocValuesQueryWeight
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static;

  fn to_string(&self, field: &str) -> Result<String> {
    let mut sb = String::new();
    if self.field != field {
      sb.push_str(&self.field);
      sb.push(':');
    }
    write!(sb, "{:?}", self.query_relation)?;
    sb.push(':');
    sb.push_str("geometries(");
    for (i, geometry) in self.data.geometries.iter().enumerate() {
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
    LatLonDocValuesQueryWeight::new(self, *score_mode, boost)
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

#[allow(clippy::type_complexity)]
pub struct LatLonDocValuesQueryWeight {
  query: LatLonDocValuesQuery,
  base: ConstantScoreWeight,
  score_mode: ScoreMode,
  boost: f32,
  component2d_predicate:
    Option<Arc<Component2DPredicate<Arc<LatLonGeometryType<LatLonGeometryEnumComponent2D>>>>>,
}

impl LatLonDocValuesQueryWeight {
  fn new(query: LatLonDocValuesQuery, score_mode: ScoreMode, boost: f32) -> Result<Self> {
    let component2d_predicate = if query.query_relation == QueryRelation::Contains {
      None
    } else {
      Some(Arc::new(GeoEncodingUtils::create_component_predicate(
        query.data.component2d.clone(),
      )?))
    };
    Ok(Self {
      query,
      base: ConstantScoreWeight::new(boost),
      score_mode,
      boost,
      component2d_predicate,
    })
  }
}

impl<IRC> SegmentCacheable<IRC> for LatLonDocValuesQueryWeight
where
  IRC: IndexReaderContext + 'static,
{
  fn is_cacheable(&self, ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    let field = [self.query.field.as_str()];
    DocValues::is_cacheable(ctx, field)
  }
}

impl<IRC, SIM> Weight<IRC, SIM> for LatLonDocValuesQueryWeight
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

  type ScorerSupplier = DefaultScorerSupplier<
    ConstantScoreTwoPhaseScorer<
      LatLonDocValuesTwoPhaseIterator<
        <IRCLeafReader<IRC> as LeafReader>::SortedNumericDocValues,
        Arc<LatLonGeometryType<LatLonGeometryEnumComponent2D>>,
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
    let component2d_predicate = self.component2d_predicate.as_ref();
    match context
      .reader()
      .get_sorted_numeric_doc_values(&self.query.field)?
    {
      Some(values) => {
        let iterator = match self.query.query_relation {
          QueryRelation::Intersects => {
            LatLonDocValuesTwoPhaseIterator::Intersects(IntersectsTPI::new(
              values,
              component2d_predicate.cloned().ok_or_else(|| {
                LuceneError::illegal_state("intersects query has no component predicate")
              })?,
            ))
          },
          QueryRelation::Within => LatLonDocValuesTwoPhaseIterator::Within(WithinTPI::new(
            values,
            component2d_predicate.cloned().ok_or_else(|| {
              LuceneError::illegal_state("within query has no component predicate")
            })?,
          )),
          QueryRelation::Disjoint => LatLonDocValuesTwoPhaseIterator::Disjoint(DisjointTPI::new(
            values,
            component2d_predicate.cloned().ok_or_else(|| {
              LuceneError::illegal_state("disjoint query has no component predicate")
            })?,
          )),
          QueryRelation::Contains => LatLonDocValuesTwoPhaseIterator::Contains(ContainsTPI::new(
            values,
            self.query.data.geometries.as_ref(),
          )?),
        };
        let scorer = ConstantScoreScorer::from_tpi(self.boost, self.score_mode, iterator);
        Ok(Some(DefaultScorerSupplier::new(scorer)))
      },
      None => Ok(None),
    }
  }
}

pub struct IntersectsTPI<S, C> {
  values: S,
  component2d_predicate: Arc<Component2DPredicate<C>>,
}
impl<S, C> IntersectsTPI<S, C> {
  fn new(values: S, component2d_predicate: Arc<Component2DPredicate<C>>) -> Self {
    Self {
      values,
      component2d_predicate,
    }
  }
}
impl<S, C> ApproximationDISI for IntersectsTPI<S, C>
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
impl<S, C> ApproximationBitSetAccess for IntersectsTPI<S, C>
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

impl<S, C> TwoPhaseIterator for IntersectsTPI<S, C>
where
  S: SortedNumericDocValues,
  C: Component2D,
{
  fn matches(&mut self) -> Result<bool> {
    let count = self.values.doc_value_count()?;
    for _ in 0..count {
      let value = self.values.next_value()? as u64;
      let lat = (value >> 32) as i32;
      let lon = value as u32 as i32;
      if self.component2d_predicate.test(lat, lon) {
        return Ok(true);
      }
    }
    Ok(false)
  }

  fn match_cost(&self) -> f32 {
    1000f32
  }
}

pub struct WithinTPI<S, C> {
  values: S,
  component2d_predicate: Arc<Component2DPredicate<C>>,
}
impl<S, C> WithinTPI<S, C> {
  fn new(values: S, component2d_predicate: Arc<Component2DPredicate<C>>) -> Self {
    Self {
      values,
      component2d_predicate,
    }
  }
}
impl<S, C> ApproximationDISI for WithinTPI<S, C>
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
impl<S, C> ApproximationBitSetAccess for WithinTPI<S, C>
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

impl<S, C> TwoPhaseIterator for WithinTPI<S, C>
where
  S: SortedNumericDocValues,
  C: Component2D,
{
  fn matches(&mut self) -> Result<bool> {
    let count = self.values.doc_value_count()?;
    for _ in 0..count {
      let value = self.values.next_value()? as u64;
      let lat = (value >> 32) as i32;
      let lon = value as u32 as i32;
      if !self.component2d_predicate.test(lat, lon) {
        return Ok(false);
      }
    }
    Ok(true)
  }

  fn match_cost(&self) -> f32 {
    1000f32
  }
}

pub struct DisjointTPI<S, C> {
  values: S,
  component2d_predicate: Arc<Component2DPredicate<C>>,
}
impl<S, C> DisjointTPI<S, C> {
  fn new(values: S, component2d_predicate: Arc<Component2DPredicate<C>>) -> Self {
    Self {
      values,
      component2d_predicate,
    }
  }
}
impl<S, C> ApproximationDISI for DisjointTPI<S, C>
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
impl<S, C> ApproximationBitSetAccess for DisjointTPI<S, C>
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

impl<S, C> TwoPhaseIterator for DisjointTPI<S, C>
where
  S: SortedNumericDocValues,
  C: Component2D,
{
  fn matches(&mut self) -> Result<bool> {
    let count = self.values.doc_value_count()?;
    for _ in 0..count {
      let value = self.values.next_value()? as u64;
      let lat = (value >> 32) as i32;
      let lon = value as u32 as i32;
      if self.component2d_predicate.test(lat, lon) {
        return Ok(false);
      }
    }
    Ok(true)
  }

  fn match_cost(&self) -> f32 {
    1000f32
  }
}

pub struct ContainsTPI<S> {
  values: S,
  component2ds: Vec<LatLonGeometryType<<LatLonGeometryEnum as Geometry>::Component2D>>,
}

pub enum LatLonDocValuesTwoPhaseIterator<S, C> {
  Intersects(IntersectsTPI<S, C>),
  Within(WithinTPI<S, C>),
  Disjoint(DisjointTPI<S, C>),
  Contains(ContainsTPI<S>),
}

impl<S, C> ApproximationDISI for LatLonDocValuesTwoPhaseIterator<S, C>
where
  S: SortedNumericDocValues,
  C: Component2D,
{
  fn approx_doc_id(&self) -> i32 {
    match self {
      Self::Intersects(iterator) => iterator.approx_doc_id(),
      Self::Within(iterator) => iterator.approx_doc_id(),
      Self::Disjoint(iterator) => iterator.approx_doc_id(),
      Self::Contains(iterator) => iterator.approx_doc_id(),
    }
  }

  fn approx_next_doc(&mut self) -> Result<i32> {
    match self {
      Self::Intersects(iterator) => iterator.approx_next_doc(),
      Self::Within(iterator) => iterator.approx_next_doc(),
      Self::Disjoint(iterator) => iterator.approx_next_doc(),
      Self::Contains(iterator) => iterator.approx_next_doc(),
    }
  }

  fn approx_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Intersects(iterator) => iterator.approx_advance(target),
      Self::Within(iterator) => iterator.approx_advance(target),
      Self::Disjoint(iterator) => iterator.approx_advance(target),
      Self::Contains(iterator) => iterator.approx_advance(target),
    }
  }

  fn approx_slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Intersects(iterator) => iterator.approx_slow_advance(target),
      Self::Within(iterator) => iterator.approx_slow_advance(target),
      Self::Disjoint(iterator) => iterator.approx_slow_advance(target),
      Self::Contains(iterator) => iterator.approx_slow_advance(target),
    }
  }

  fn approx_cost(&self) -> Result<i64> {
    match self {
      Self::Intersects(iterator) => iterator.approx_cost(),
      Self::Within(iterator) => iterator.approx_cost(),
      Self::Disjoint(iterator) => iterator.approx_cost(),
      Self::Contains(iterator) => iterator.approx_cost(),
    }
  }
}

impl<S, C> ApproximationBitSetAccess for LatLonDocValuesTwoPhaseIterator<S, C>
where
  S: SortedNumericDocValues,
  C: Component2D,
{
  fn approx_is_bit_iter(&self) -> bool {
    match self {
      Self::Intersects(iterator) => iterator.approx_is_bit_iter(),
      Self::Within(iterator) => iterator.approx_is_bit_iter(),
      Self::Disjoint(iterator) => iterator.approx_is_bit_iter(),
      Self::Contains(iterator) => iterator.approx_is_bit_iter(),
    }
  }

  fn approx_get(&self, index: usize) -> Result<bool> {
    match self {
      Self::Intersects(iterator) => iterator.approx_get(index),
      Self::Within(iterator) => iterator.approx_get(index),
      Self::Disjoint(iterator) => iterator.approx_get(index),
      Self::Contains(iterator) => iterator.approx_get(index),
    }
  }

  fn approx_set_doc_id(&mut self, doc: i32) -> Result<()> {
    match self {
      Self::Intersects(iterator) => iterator.approx_set_doc_id(doc),
      Self::Within(iterator) => iterator.approx_set_doc_id(doc),
      Self::Disjoint(iterator) => iterator.approx_set_doc_id(doc),
      Self::Contains(iterator) => iterator.approx_set_doc_id(doc),
    }
  }

  fn approx_bit_set_length(&self) -> Result<usize> {
    match self {
      Self::Intersects(iterator) => iterator.approx_bit_set_length(),
      Self::Within(iterator) => iterator.approx_bit_set_length(),
      Self::Disjoint(iterator) => iterator.approx_bit_set_length(),
      Self::Contains(iterator) => iterator.approx_bit_set_length(),
    }
  }
}

impl<S, C> TwoPhaseIterator for LatLonDocValuesTwoPhaseIterator<S, C>
where
  S: SortedNumericDocValues,
  C: Component2D,
{
  fn matches(&mut self) -> Result<bool> {
    match self {
      Self::Intersects(iterator) => iterator.matches(),
      Self::Within(iterator) => iterator.matches(),
      Self::Disjoint(iterator) => iterator.matches(),
      Self::Contains(iterator) => iterator.matches(),
    }
  }

  fn match_cost(&self) -> f32 {
    match self {
      Self::Intersects(iterator) => iterator.match_cost(),
      Self::Within(iterator) => iterator.match_cost(),
      Self::Disjoint(iterator) => iterator.match_cost(),
      Self::Contains(iterator) => iterator.match_cost(),
    }
  }
}

impl<S> ContainsTPI<S> {
  fn new(values: S, geometries: &[LatLonGeometryEnum]) -> Result<Self> {
    let mut component2ds = Vec::with_capacity(geometries.len());
    for geometry in geometries {
      component2ds.push(lat_lon_geometry::create(std::slice::from_ref(geometry))?);
    }
    Ok(Self {
      values,
      component2ds,
    })
  }
}
impl<S> ApproximationDISI for ContainsTPI<S>
where
  S: SortedNumericDocValues,
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
impl<S> ApproximationBitSetAccess for ContainsTPI<S>
where
  S: SortedNumericDocValues,
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

impl<S> TwoPhaseIterator for ContainsTPI<S>
where
  S: SortedNumericDocValues,
{
  fn matches(&mut self) -> Result<bool> {
    let mut answer = WithinRelation::Disjoint;
    let count = self.values.doc_value_count()?;
    for _ in 0..count {
      let value = self.values.next_value()? as u64;
      let lat = (value >> 32) as i32;
      let lon = value as u32 as i32;
      let lat = GeoEncodingUtils::decode_latitude(lat);
      let lon = GeoEncodingUtils::decode_longitude(lon);
      for component2d in self.component2ds.as_slice() {
        let relation = component2d.within_point(lon, lat)?;
        if relation == WithinRelation::NotWithin {
          return Ok(false);
        } else if relation != WithinRelation::Disjoint {
          answer = relation;
        }
      }
    }
    Ok(answer == WithinRelation::Candidate)
  }

  fn match_cost(&self) -> f32 {
    1000f32
  }
}

impl crate::core::util::accountable::Accountable for LatLonDocValuesQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED)
  }
}
