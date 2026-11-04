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
use crate::core::document::xy_point_field::XYPointField;
use crate::core::geo::component2d::Component2D;
use crate::core::geo::geometry::Geometry;
use crate::core::geo::xy_encoding_utils::XYEncodingUtils;
use crate::core::geo::xy_geometry;
use crate::core::geo::xy_geometry::{XYGeometryEnum, XYGeometryType};
use crate::core::index::index_reader::{Identity, IndexReader};
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::point_values::{IntersectVisitor, PointValues, Relation};
use crate::core::search::constant_score_scorer::ConstantScoreDisiScorer;
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::constant_score_weight::ConstantScoreWeight;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::explanation::Explanation;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::query::QueryBase;
use crate::core::search::query::QueryRef;
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::weight::Weight;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::core_helper::HasIdentity;
use crate::core::util::doc_id_set_builder::DocIdSetBuilder;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::fmt::{Debug, Formatter, Write};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Finds all previously indexed points that fall within the specified XY geometries.
///
/// The field must be indexed with XY point values added per document.
#[derive(Clone)]
pub struct XYPointInGeometryQuery {
  id: Identity,
  field: String,
  xy_geometries: Arc<[XYGeometryEnum]>,
  component2d: Arc<XYGeometryType<<XYGeometryEnum as Geometry>::Component2D>>,
}

impl XYPointInGeometryQuery {
  pub(crate) fn new(field: String, xy_geometries: Vec<XYGeometryEnum>) -> Result<Self> {
    if xy_geometries.is_empty() {
      return Err(LuceneError::illegal_argument(
        "geometries must not be empty",
      ));
    }
    let component2d = Arc::new(xy_geometry::create(xy_geometries.as_slice())?);
    Ok(Self {
      id: Identity::new(),
      field,
      xy_geometries: Arc::from(xy_geometries),
      component2d,
    })
  }

  /// Returns the query field.
  pub fn get_field(&self) -> &str {
    &self.field
  }

  /// Returns a copy of the internal geometries.
  pub fn get_geometries(&self) -> &[XYGeometryEnum] {
    self.xy_geometries.as_ref()
  }
}

impl Debug for XYPointInGeometryQuery {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("XYPointInGeometryQuery")
      .field("field", &self.field)
      .finish()
  }
}

impl PartialEq for XYPointInGeometryQuery {
  fn eq(&self, other: &Self) -> bool {
    self.field == other.field && self.xy_geometries == other.xy_geometries
  }
}

impl Eq for XYPointInGeometryQuery {}

impl Hash for XYPointInGeometryQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.field.hash(state);
    self.xy_geometries.hash(state);
  }
}

impl HasIdentity for XYPointInGeometryQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl QueryBase for XYPointInGeometryQuery {
  type Weight<IRC, SIM>
    = XYPointInGeometryWeight
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static;

  fn to_string(&self, field: &str) -> Result<String> {
    let mut sb = String::from("XYPointInGeometryQuery:");
    if self.field != field {
      sb.push_str(" field=");
      sb.push_str(&self.field);
      sb.push(':');
    }
    sb.push('[');
    for (i, geometry) in self.xy_geometries.iter().enumerate() {
      if i > 0 {
        sb.push_str(", ");
      }
      write!(sb, "{geometry}")?;
    }
    sb.push(']');
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
    Ok(XYPointInGeometryWeight::new(self, *score_mode, boost))
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

pub struct XYPointInGeometryWeight {
  base: ConstantScoreWeight,
  parent_query: XYPointInGeometryQuery,
  score_mode: ScoreMode,
}

impl XYPointInGeometryWeight {
  fn new(query: XYPointInGeometryQuery, score_mode: ScoreMode, boost: f32) -> Self {
    let parent_query = query;
    Self {
      base: ConstantScoreWeight::new(boost),
      parent_query,
      score_mode,
    }
  }

  fn get_intersect_visitor(
    &self,
    result: DocIdSetBuilder,
    component2d: Arc<XYGeometryType<<XYGeometryEnum as Geometry>::Component2D>>,
  ) -> XYPointInGeometryIntersectVisitor {
    XYPointInGeometryIntersectVisitor::new(result, component2d)
  }
}

impl<IRC> SegmentCacheable<IRC> for XYPointInGeometryWeight
where
  IRC: IndexReaderContext + 'static,
{
  fn is_cacheable(&self, _ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    Ok(true)
  }
}

impl<IRC, SIM> Weight<IRC, SIM> for XYPointInGeometryWeight
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
    QueryRef::XYPointInGeometry(&self.parent_query)
  }

  type ScorerSupplier =
    XYPointInGeometryScorerSupplier<<IRCLeafReader<IRC> as LeafReader>::PointValues>;

  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Option<Self::ScorerSupplier>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let query = &self.parent_query;
    let reader = context.reader();
    let values = match reader.get_point_values(&query.field)? {
      Some(values) => values,
      None => return Ok(None),
    };
    let field_infos = reader.get_field_infos()?;
    let Some(field_info) = field_infos.field_info_by_name(&query.field)? else {
      // No docs in this segment indexed this field at all
      return Ok(None);
    };
    XYPointField::check_compatible(field_info)?;

    let result = DocIdSetBuilder::from_point_values(reader.max_doc()?, &values, &query.field)?;
    let visitor = self.get_intersect_visitor(result, query.component2d.clone());
    Ok(Some(XYPointInGeometryScorerSupplier::new(
      self.base.score(),
      self.score_mode,
      values,
      visitor,
    )))
  }
}

pub struct XYPointInGeometryScorerSupplier<PV> {
  score: f32,
  score_mode: ScoreMode,
  values: PV,
  visitor: XYPointInGeometryIntersectVisitor,
  cost: i64,
}

impl<PV> XYPointInGeometryScorerSupplier<PV> {
  fn new(
    score: f32,
    score_mode: ScoreMode,
    values: PV,
    visitor: XYPointInGeometryIntersectVisitor,
  ) -> Self {
    Self {
      score,
      score_mode,
      values,
      visitor,
      cost: -1,
    }
  }
}

impl<IRC, SIM> ScorerSupplier<IRC, SIM>
  for XYPointInGeometryScorerSupplier<<IRCLeafReader<IRC> as LeafReader>::PointValues>
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
  IRCLeafReader<IRC>: LeafReader,
{
  type Scorer =
    ConstantScoreDisiScorer<crate::core::util::doc_id_set_builder::OwnedDocIdSetBuilderIterator>;
  type BulkScorer = crate::core::search::weight::DefaultBulkScorer<Self::Scorer>;

  fn get(
    &mut self,
    _lead_cost: i64,
    _context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Self::Scorer>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    self.values.intersect(&mut self.visitor)?;
    let iterator = self.visitor.adder.build_iterator()?;
    Ok(ConstantScoreScorer::from_disi(
      self.score,
      self.score_mode,
      iterator,
    ))
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
    if self.cost == -1 {
      self.cost = self.values.estimate_doc_count(&self.visitor)?;
      debug_assert!(self.cost >= 0);
    }
    Ok(self.cost)
  }
}

struct XYPointInGeometryIntersectVisitor {
  adder: DocIdSetBuilder,
  component2d: Arc<XYGeometryType<<XYGeometryEnum as Geometry>::Component2D>>,
}

impl XYPointInGeometryIntersectVisitor {
  fn new(
    result: DocIdSetBuilder,
    component2d: Arc<XYGeometryType<<XYGeometryEnum as Geometry>::Component2D>>,
  ) -> Self {
    Self {
      adder: result,
      component2d,
    }
  }
}

impl IntersectVisitor for XYPointInGeometryIntersectVisitor {
  fn grow(&mut self, count: usize) -> Result<()> {
    self.adder.grow(count)?;
    Ok(())
  }

  fn visit(&mut self, doc_id: i32) -> Result<()> {
    self.adder.add_doc(doc_id)?;
    Ok(())
  }

  fn visit_with_iterator<I>(&mut self, iterator: &mut I) -> Result<()>
  where
    I: DocIdSetIterator,
  {
    self.adder.add_disi(iterator)
  }

  fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
    let x = XYEncodingUtils::decode_bytes(packed_value, 0) as f64;
    let y = XYEncodingUtils::decode_bytes(packed_value, BitUtil::INT_BYTES) as f64;
    if self.component2d.contains(x, y) {
      self.visit(doc_id)?;
    }
    Ok(())
  }

  fn visit_iterator_with_packed_value<I>(
    &mut self,
    iterator: &mut I,
    packed_value: &[u8],
  ) -> Result<()>
  where
    I: DocIdSetIterator,
  {
    let x = XYEncodingUtils::decode_bytes(packed_value, 0) as f64;
    let y = XYEncodingUtils::decode_bytes(packed_value, BitUtil::INT_BYTES) as f64;
    if self.component2d.contains(x, y) {
      self.adder.add_disi(iterator)?;
    }
    Ok(())
  }

  fn compare(&self, min_packed_value: &[u8], max_packed_value: &[u8]) -> Result<Relation> {
    let cell_min_x = XYEncodingUtils::decode_bytes(min_packed_value, 0) as f64;
    let cell_min_y = XYEncodingUtils::decode_bytes(min_packed_value, BitUtil::INT_BYTES) as f64;
    let cell_max_x = XYEncodingUtils::decode_bytes(max_packed_value, 0) as f64;
    let cell_max_y = XYEncodingUtils::decode_bytes(max_packed_value, BitUtil::INT_BYTES) as f64;
    self
      .component2d
      .relate(cell_min_x, cell_max_x, cell_min_y, cell_max_y)
  }
}

impl crate::core::util::accountable::Accountable for XYPointInGeometryQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED)
  }
}
