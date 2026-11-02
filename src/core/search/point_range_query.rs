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
use crate::core::document::binary_point::BinaryPointRangeQuery;
use crate::core::document::double_point::DoublePointRangeQuery;
use crate::core::document::float_point::FloatPointRangeQuery;
use crate::core::document::inet_address_point::InetAddressPointRangeQuery;
use crate::core::document::int_point::IntPointRangeQuery;
use crate::core::document::lat_lon_point::LatLonPointRangeQuery;
use crate::core::document::long_point::LongPointRangeQuery;
use crate::core::index::index_reader::{Identity, IndexReader};
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::point_values::{IntersectVisitor, PointTree, PointValues, Relation};
use crate::core::search::constant_score_scorer::ConstantScoreDisiScorer;
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::constant_score_weight::ConstantScoreWeight;
use crate::core::search::doc_id_set_iterator::{AllDISI, DocIdSetIterator};
use crate::core::search::explanation::Explanation;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::query::QueryRef;
use crate::core::search::query::{QueryBase, QueryWeightSs};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::ScorerEnum2;
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::weight::Weight;
use crate::core::util::TryIntoInt;
use crate::core::util::array_util::{ArrayUtil, ByteArrayComparator, ByteArrayComparatorEnum};
use crate::core::util::bit_set::BitSet;
use crate::core::util::bit_set_iterator::BitSetIterator;
use crate::core::util::core_helper::HasIdentity;
use crate::core::util::doc_id_set_builder::{DocIdSetBuilder, OwnedDocIdSetBuilderIterator};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::ints_ref::IntsRef;
use crate::impl_from_for_enum;
use crate::sandbox::document::big_integer_point::BigIntegerPointRangeQuery;
use crate::sandbox::document::half_float_point::HalfFloatPointRangeQuery;
#[cfg(test)]
use crate::test_framework::core::search::point::PointRangeQueryBaseImpl;
use std::fmt::Debug;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Struct for range queries over single- or multi-dimensional point fields,
/// such as [`IntPoint`](crate::core::document::int_point::IntPoint).
///
/// This type is intended for query implementations and works directly on the underlying
/// binary encoding. For Lucene's standard point types, use the factory methods
/// on those types instead—for example, [`IntPoint::new_range_query`](crate::core::document::int_point::IntPoint::new_range_query) for fields
/// indexed with [`IntPoint`](crate::core::document::int_point::IntPoint).
///
/// For single-dimensional fields, this represents a simple range query; for
/// multi-dimensional fields, it represents a box-shaped query.
///
/// See also: [`PointValues`]
#[derive(Clone)]
pub struct PointRangeQuery {
  id: Identity,
  num_dims: usize,
  bytes_per_dim: usize,
  points: Arc<PointRangeQueryData>,
  sub: PointRangeBaseEnum,
}

struct PointRangeQueryData {
  field: String,
  lower: Box<[u8]>,
  upper: Box<[u8]>,
}
impl Debug for PointRangeQuery {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("PointRangeQuery")
      .field("id", &self.id)
      .field("field", &self.points.field)
      .field("num_dims", &self.num_dims)
      .field("bytes_per_dim", &self.bytes_per_dim)
      .field("lower_point", &self.points.lower)
      .field("upper_point", &self.points.upper)
      .field("sub", &self.sub)
      .finish()
  }
}

impl PointRangeQuery {
  /// Expert: create a multi-dimensional range query over point values.
  ///
  /// # Parameters
  /// - `field`: the field name.
  /// - `lower_point`: the inclusive lower bound of the range.
  /// - `upper_point`: the inclusive upper bound of the range.
  /// - `num_dims`: number of dimensions.
  ///
  /// # Errors
  /// Returns an error if `field` is empty or if the lengths of `lower_point` and
  /// `upper_point` do not match.
  pub fn new<S, FName, L, U>(
    field: FName,
    lower_point: L,
    upper_point: U,
    num_dims: usize,
    sub: S,
  ) -> Result<Self>
  where
    S: Into<PointRangeBaseEnum>,
    FName: Into<String>,
    L: Into<Vec<u8>>,
    U: Into<Vec<u8>>,
  {
    let upper_point = upper_point.into();
    let lower_point = lower_point.into();
    let field = field.into();
    #[cfg(debug_assertions)]
    check_args(&field, lower_point.as_ref(), upper_point.as_ref())?;
    if lower_point.is_empty() {
      return Err(LuceneError::illegal_argument(
        "lower_point has length of zero".to_string(),
      ));
    }
    if !lower_point.len().is_multiple_of(num_dims) {
      return Err(LuceneError::illegal_argument(
        "lower_point is not a fixed multiple of num_dims".to_string(),
      ));
    }
    if lower_point.len() != upper_point.len() {
      return Err(LuceneError::illegal_argument(format!(
        "lower_point has length={} but upper_point has different length={}",
        lower_point.len(),
        upper_point.len()
      )));
    }

    let bytes_per_dim = lower_point.len() / num_dims;

    Ok(Self {
      id: Identity::new(),
      num_dims,
      bytes_per_dim,
      points: Arc::new(PointRangeQueryData {
        field,
        lower: lower_point.into_boxed_slice(),
        upper: upper_point.into_boxed_slice(),
      }),
      sub: sub.into(),
    })
  }

  fn equals_to(&self, other: &PointRangeQuery) -> bool {
    std::mem::discriminant(&self.sub) == std::mem::discriminant(&other.sub)
      && self.points.field == other.points.field
      && self.num_dims == other.num_dims
      && self.bytes_per_dim == other.bytes_per_dim
      && self.points.lower == other.points.lower
      && self.points.upper == other.points.upper
  }
  pub fn to_string(&self, field: &str) -> Result<String> {
    let mut sb = String::new();

    if self.points.field != field {
      sb.push_str(&self.points.field);
      sb.push(':');
    }

    for i in 0..self.num_dims {
      if i > 0 {
        sb.push(',');
      }
      let start = self.bytes_per_dim * i;
      let end = start + self.bytes_per_dim;

      let lower = &self.points.lower[start..end];
      let upper = &self.points.upper[start..end];

      sb.push('[');
      sb.push_str(&self.sub.to_string(i, lower)?);
      sb.push_str(" TO ");
      sb.push_str(&self.sub.to_string(i, upper)?);
      sb.push(']');
    }

    Ok(sb)
  }
  #[cfg(test)]
  pub fn get_lower_point(&self) -> &[u8] {
    &self.points.lower
  }
  #[cfg(test)]
  pub fn get_upper_point(&self) -> &[u8] {
    &self.points.upper
  }
}

#[cfg(debug_assertions)]
pub fn check_args(_field: &String, _lower_point: &[u8], _upper_point: &[u8]) -> Result<()> {
  // not required in Rust Lucene, return Ok directly
  Ok(())
}

impl Eq for PointRangeQuery {}

impl PartialEq<Self> for PointRangeQuery {
  fn eq(&self, other: &Self) -> bool {
    self.equals_to(other)
  }
}

impl Hash for PointRangeQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    std::mem::discriminant(&self.sub).hash(state);
    self.points.field.hash(state);
    self.num_dims.hash(state);
    self.bytes_per_dim.hash(state);
    self.points.lower.hash(state);
    self.points.upper.hash(state);
  }
}

impl HasIdentity for PointRangeQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}
impl QueryBase for PointRangeQuery {
  type Weight<IRC, SIM>
    = PointRangeWeight
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static;

  fn to_string(&self, field: &str) -> Result<String> {
    self.to_string(field)
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
    Ok(PointRangeWeight::new(boost, self, *score_mode))
  }

  fn visit<QV>(&self, visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    let query = self.into();
    if visitor.accept_field(&self.points.field) {
      visitor.visit_leaf(query)?;
    }
    Ok(())
  }
}

pub struct PointRangeWeight {
  base: ConstantScoreWeight,
  parent_query: Arc<PointRangeQuery>,
  comparator: ByteArrayComparatorEnum,
  score_mode: ScoreMode,
}
impl PointRangeWeight {
  pub fn new(score: f32, query: PointRangeQuery, score_mode: ScoreMode) -> Self {
    let comparator = ArrayUtil::get_unsigned_comparator(query.bytes_per_dim);
    let parent_query = Arc::new(query);
    Self {
      base: ConstantScoreWeight::new(score),
      parent_query,
      comparator,
      score_mode,
    }
  }

  pub fn check_valid_point_values<PV>(&self, values: Option<&PV>) -> Result<bool>
  where
    PV: PointValues,
  {
    let values = match values {
      Some(v) => v,
      None => return Ok(false),
    };

    let q = self.point_range_query()?;
    let num_dims = q.num_dims;
    let bytes_per_dim = q.bytes_per_dim;
    let field = &q.points.field;

    if values.get_num_index_dimensions()? != num_dims {
      return Err(LuceneError::illegal_argument(format!(
        "field=\"{}\" was indexed with numIndexDimensions={} but this query has numDims={}",
        field,
        values.get_num_index_dimensions()?,
        num_dims
      )));
    }

    if values.get_bytes_per_dimension()? != bytes_per_dim {
      return Err(LuceneError::illegal_argument(format!(
        "field=\"{}\" was indexed with bytesPerDim={} but this query has bytesPerDim={}",
        field,
        values.get_bytes_per_dimension()?,
        bytes_per_dim
      )));
    }

    Ok(true)
  }
  /// Count the number of points that satisfy the given range constraints.
  ///
  /// This is faster than calling [`PointValues::intersect`] to collect and count
  /// matching points. It does **not** enforce live documents, so it must only be
  /// used when there are no deleted documents.
  ///
  /// # Parameters
  /// - `point_tree`: the starting node of the count operation.
  ///
  /// # Returns
  /// The number of points that match the queried range.
  fn point_count<T>(&self, point_tree: &mut T) -> Result<i64>
  where
    T: PointTree,
  {
    let mut visitor = IntersectVisitorImpl2::new(self.point_range_query()?, &self.comparator);
    self.point_count_with_visitor(&mut visitor, point_tree)?;
    Ok(visitor.matching_node_count)
  }

  fn point_count_with_visitor<T>(
    &self,
    visitor: &mut IntersectVisitorImpl2<'_>,
    point_tree: &mut T,
  ) -> Result<()>
  where
    T: PointTree,
  {
    let relation = visitor.compare(
      point_tree.get_min_packed_value()?.as_ref(),
      point_tree.get_max_packed_value()?.as_ref(),
    )?;

    match relation {
      Relation::CellOutsideQuery => {
        // This cell is fully outside the query shape: return 0 as the count of its nodes
        Ok(())
      },

      Relation::CellInsideQuery => {
        // This cell is fully inside the query shape: return the size of the entire node as the
        // count
        let v: i64 = point_tree.size()?.try_convert()?;
        visitor.matching_node_count += v;
        Ok(())
      },

      Relation::CellCrossesQuery => {
        // The cell crosses the shape boundary, or the cell fully contains the query, so we fall
        // through and do full counting.
        if point_tree.move_to_child()? {
          loop {
            self.point_count_with_visitor(visitor, point_tree)?;
            if !point_tree.move_to_sibling()? {
              break;
            }
          }
          point_tree.move_to_parent()?;
        } else {
          // we have reached a leaf node here.
          point_tree.visit_doc_values(visitor)?;
          // leaf node count is saved in the matchingNodeCount array by the visitor
        }
        Ok(())
      },
    }
  }
  fn point_range_query(&self) -> Result<&PointRangeQuery> {
    Ok(&self.parent_query)
  }
}

impl<IRC> SegmentCacheable<IRC> for PointRangeWeight
where
  IRC: IndexReaderContext + 'static,
{
  fn is_cacheable(&self, _ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    Ok(true)
  }
}

impl<IRC, SIM> Weight<IRC, SIM> for PointRangeWeight
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
    QueryRef::PointRange(self.parent_query.as_ref())
  }

  type ScorerSupplier = QueryWeightSs<IRC, SIM>;

  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Option<Self::ScorerSupplier>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let reader = context.reader();

    let values_opt = reader.get_point_values(&self.point_range_query()?.points.field)?;
    if !self.check_valid_point_values(values_opt.as_ref())? {
      return Ok(None);
    }

    let values = match values_opt {
      Some(v) => v,
      None => return Ok(None),
    };

    if values.get_doc_count()? == 0 {
      return Ok(None);
    } else {
      let field_packed_lower = values
        .get_min_packed_value()?
        .ok_or_else(|| LuceneError::illegal_state("min_packed_value is None"))?;
      let field_packed_upper = values
        .get_max_packed_value()?
        .ok_or_else(|| LuceneError::illegal_state("max_packed_value is None"))?;

      let q = self.point_range_query()?;
      let num_dims = q.num_dims;
      let bytes_per_dim = q.bytes_per_dim;

      for i in 0..num_dims {
        let offset = i * bytes_per_dim;

        if self
          .comparator
          .compare(&q.points.lower, offset, field_packed_upper.as_ref(), offset)
          > 0
          || self
            .comparator
            .compare(&q.points.upper, offset, field_packed_lower.as_ref(), offset)
            < 0
        {
          // If this query is a required clause of a boolean query, then returning None here
          // This ensures that we do not call `ScorerSupplier::get` on other required clauses.
          // of the same boolean query, which is an expensive operation for some queries (e.g.
          // multi-term queries).
          return Ok(None);
        }
      }
    }

    let mut all_docs_match;

    if values.get_doc_count()? == reader.max_doc()? {
      let field_packed_lower = values
        .get_min_packed_value()?
        .ok_or_else(|| LuceneError::illegal_state("min_packed_value is None"))?;
      let field_packed_upper = values
        .get_max_packed_value()?
        .ok_or_else(|| LuceneError::illegal_state("max_packed_value is None"))?;

      let q = self.point_range_query()?;
      let num_dims = q.num_dims;
      let bytes_per_dim = q.bytes_per_dim;

      all_docs_match = true;
      for i in 0..num_dims {
        let offset = i * bytes_per_dim;

        if self
          .comparator
          .compare(&q.points.lower, offset, field_packed_lower.as_ref(), offset)
          > 0
          || self
            .comparator
            .compare(&q.points.upper, offset, field_packed_upper.as_ref(), offset)
            < 0
        {
          all_docs_match = false;
          break;
        }
      }
    } else {
      all_docs_match = false;
    }
    let max_doc = reader.max_doc()?;
    if all_docs_match {
      Ok(Some(
        ScorerSupplierImpl::new(self.base.score(), self.score_mode, max_doc).into(),
      ))
    } else {
      let result = DocIdSetBuilder::from_point_values(
        max_doc,
        &values,
        self.point_range_query()?.points.field.as_ref(),
      )?;
      Ok(Some(
        Box::new(ScorerSupplierImpl1::new(
          self.base.score(),
          self.score_mode,
          values,
          result,
          self.parent_query.clone(),
          self.comparator.clone(),
        ))
        .into(),
      ))
    }
  }

  fn count(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<i32>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let query = self.point_range_query()?;
    let reader = context.reader();

    let values = reader.get_point_values(query.points.field.as_str())?;

    if !self.check_valid_point_values(values.as_ref())? {
      return Ok(0);
    }

    if !reader.has_deletions()? {
      let values =
        values.ok_or_else(|| LuceneError::illegal_state("validated point values are missing"))?;
      let min_packed_value = values.get_min_packed_value()?.ok_or_else(|| {
        LuceneError::illegal_state("non-empty point values have no minimum packed value")
      })?;
      let max_packed_value = values.get_max_packed_value()?.ok_or_else(|| {
        LuceneError::illegal_state("non-empty point values have no maximum packed value")
      })?;

      let relation = relate(
        query,
        &self.comparator,
        min_packed_value.as_ref(),
        max_packed_value.as_ref(),
      )?;

      if relation == Relation::CellInsideQuery {
        return values.get_doc_count();
      }

      // only 1D: we have the guarantee that it will actually run fast since there are at most 2
      // crossing leaves.
      // docCount == size : counting according number of points in leaf node, so must be
      // single-valued.
      if query.num_dims == 1 && values.get_doc_count()? as usize == values.size()? {
        let mut tree = values.get_point_tree()?;
        return Ok(self.point_count(&mut tree)? as i32);
      }
    }
    <Self as Weight<IRC, SIM>>::default_count(self, context)
  }
}
pub(crate) fn matches(
  query: &PointRangeQuery,
  comparator: &ByteArrayComparatorEnum,
  packed_value: &[u8],
) -> Result<bool> {
  let num_dims = query.num_dims;
  let bytes_per_dim = query.bytes_per_dim;
  let mut offset = 0usize;
  for _ in 0..num_dims {
    if comparator.compare(packed_value, offset, query.points.lower.as_ref(), offset) < 0 {
      // Doc's value is too low, in this dimension
      return Ok(false);
    }
    if comparator.compare(packed_value, offset, query.points.upper.as_ref(), offset) > 0 {
      // Doc's value is too high, in this dimension
      return Ok(false);
    }
    offset += bytes_per_dim;
  }
  Ok(true)
}
fn get_inverse_intersect_visitor<'a>(
  result: &'a mut FixedBitSet,
  cost: i64,
  comparator: &'a ByteArrayComparatorEnum,
  query: &'a PointRangeQuery,
) -> IntersectVisitorImpl<'a> {
  IntersectVisitorImpl::new(result, cost, query, comparator)
}
pub(crate) fn relate(
  query: &PointRangeQuery,
  comparator: &ByteArrayComparatorEnum,
  min_packed_value: &[u8],
  max_packed_value: &[u8],
) -> Result<Relation> {
  let num_dims = query.num_dims;
  let bytes_per_dim = query.bytes_per_dim;

  let mut crosses = false;
  let mut offset = 0usize;

  for _ in 0..num_dims {
    if comparator.compare(min_packed_value, offset, &query.points.upper, offset) > 0
      || comparator.compare(max_packed_value, offset, &query.points.lower, offset) < 0
    {
      return Ok(Relation::CellOutsideQuery);
    }

    if comparator.compare(min_packed_value, offset, &query.points.lower, offset) < 0
      || comparator.compare(max_packed_value, offset, &query.points.upper, offset) > 0
    {
      crosses = true;
    }

    offset += bytes_per_dim;
  }

  if crosses {
    Ok(Relation::CellCrossesQuery)
  } else {
    Ok(Relation::CellInsideQuery)
  }
}
pub struct ScorerSupplierImpl1<PV> {
  score: f32,
  score_mode: ScoreMode,
  values: PV,
  result: DocIdSetBuilder,
  parent_query: Arc<PointRangeQuery>,
  comparator: ByteArrayComparatorEnum,
  cost: i64,
}
impl<PV> ScorerSupplierImpl1<PV> {
  pub fn new(
    score: f32,
    score_mode: ScoreMode,
    values: PV,
    result: DocIdSetBuilder,
    parent_query: Arc<PointRangeQuery>,
    comparator: ByteArrayComparatorEnum,
  ) -> Self {
    Self {
      score,
      score_mode,
      values,
      result,
      parent_query,
      comparator,
      cost: -1,
    }
  }
}
impl<IRC, SIM> ScorerSupplier<IRC, SIM>
  for ScorerSupplierImpl1<<IRCLeafReader<IRC> as LeafReader>::PointValues>
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
  IRCLeafReader<IRC>: LeafReader,
{
  type Scorer = ScorerEnum2<
    ConstantScoreDisiScorer<BitSetIterator<FixedBitSet>>,
    ConstantScoreDisiScorer<OwnedDocIdSetBuilderIterator>,
  >;
  type BulkScorer = crate::core::search::weight::DefaultBulkScorer<Self::Scorer>;

  fn get(
    &mut self,
    _lead_cost: i64,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Self::Scorer>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let reader = context.reader();
    let size = self.values.size()?;
    let inverse = self.values.get_doc_count()? == reader.max_doc()?
      && self.values.get_doc_count()? as usize == size
      && self.cost(context, searcher)? > (reader.max_doc()? as i64 / 2);
    let mut visitor =
      IntersectVisitorImpl1::new(&mut self.result, &self.parent_query, &self.comparator)?;
    if inverse {
      let max_doc = reader.max_doc()?;
      let max_doc_usize = max_doc as usize;
      // If all docs have exactly one value and the cost is greater
      // than half the leaf size then maybe we can make things faster
      // by computing the set of documents that do NOT match the range
      let mut result = FixedBitSet::new(max_doc_usize);
      result.set_with_range(0, max_doc_usize);
      let mut visitor = get_inverse_intersect_visitor(
        &mut result,
        max_doc as i64,
        visitor.comparator,
        visitor.query,
      );
      self.values.intersect(&mut visitor)?;
      let cost = visitor.cost;
      let iterator = BitSetIterator::new(result, cost)?;
      return Ok(ScorerEnum2::A(ConstantScoreScorer::from_disi(
        self.score,
        self.score_mode,
        iterator,
      )));
    }
    self.values.intersect(&mut visitor)?;
    let iterator = visitor.result.build_iterator()?;
    Ok(ScorerEnum2::B(ConstantScoreScorer::from_disi(
      self.score,
      self.score_mode,
      iterator,
    )))
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
      // Computing the cost may be expensive, so only do it if necessary
      let visitor =
        IntersectVisitorImpl1::new(&mut self.result, &self.parent_query, &self.comparator)?;
      self.cost = self.values.estimate_doc_count(&visitor)?;
      debug_assert!(self.cost >= 0);
    }
    Ok(self.cost)
  }
}
pub struct ScorerSupplierImpl {
  score_mode: ScoreMode,
  max_doc: i32,
  score: f32,
}
impl ScorerSupplierImpl {
  pub fn new(score: f32, score_mode: ScoreMode, max_doc: i32) -> Self {
    Self {
      score,
      score_mode,
      max_doc,
    }
  }
}
impl<IRC, SIM> ScorerSupplier<IRC, SIM> for ScorerSupplierImpl
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
  IRCLeafReader<IRC>: LeafReader,
{
  type Scorer = ConstantScoreDisiScorer<AllDISI>;
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
    debug_assert!(context.reader().max_doc()? == self.max_doc);
    Ok(ConstantScoreScorer::from_disi(
      self.score,
      self.score_mode,
      AllDISI::new(self.max_doc),
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
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<i64>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    debug_assert!(context.reader().max_doc()? == self.max_doc);
    Ok(self.max_doc as i64)
  }
}

struct IntersectVisitorImpl<'a> {
  result: &'a mut FixedBitSet,
  cost: i64,
  query: &'a PointRangeQuery,
  comparator: &'a ByteArrayComparatorEnum,
}
impl<'a> IntersectVisitorImpl<'a> {
  fn new(
    result: &'a mut FixedBitSet,
    cost: i64,
    query: &'a PointRangeQuery,
    comparator: &'a ByteArrayComparatorEnum,
  ) -> Self {
    Self {
      result,
      cost,
      query,
      comparator,
    }
  }
}
impl IntersectVisitor for IntersectVisitorImpl<'_> {
  fn visit(&mut self, doc_id: i32) -> Result<()> {
    self.result.clear_with_index(doc_id as usize)?;
    self.cost -= 1;
    Ok(())
  }

  fn visit_with_iterator<I>(&mut self, iterator: &mut I) -> Result<()>
  where
    I: DocIdSetIterator,
  {
    self.result.and_not_iter(iterator)?;
    self.cost = (self.cost - iterator.cost()?).max(0);
    Ok(())
  }

  fn visit_with_ints_ref(&mut self, ints_ref: &IntsRef<Vec<i32>>) -> Result<()> {
    for i in ints_ref.offset..(ints_ref.offset + ints_ref.length) {
      self.result.clear_with_index(ints_ref.ints[i] as usize)?;
    }
    self.cost -= ints_ref.length as i64;
    Ok(())
  }

  fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
    if !matches(self.query, self.comparator, packed_value)? {
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
    if !matches(self.query, self.comparator, packed_value)? {
      self.visit_with_iterator(iterator)?;
    }
    Ok(())
  }

  fn compare(&self, min_packed_value: &[u8], max_packed_value: &[u8]) -> Result<Relation> {
    let relation = relate(
      self.query,
      self.comparator,
      min_packed_value,
      max_packed_value,
    )?;

    Ok(match relation {
      // all points match, skip this subtree
      Relation::CellInsideQuery => Relation::CellOutsideQuery,
      // none of the points match, clear all documents
      Relation::CellOutsideQuery => Relation::CellInsideQuery,
      Relation::CellCrossesQuery => Relation::CellCrossesQuery,
    })
  }
}
pub struct IntersectVisitorImpl1<'a> {
  result: &'a mut DocIdSetBuilder,
  query: &'a PointRangeQuery,
  comparator: &'a ByteArrayComparatorEnum,
}

impl<'a> IntersectVisitorImpl1<'a> {
  pub fn new(
    result: &'a mut DocIdSetBuilder,
    parent_query: &'a PointRangeQuery,
    comparator: &'a ByteArrayComparatorEnum,
  ) -> Result<Self> {
    let query = parent_query;
    Ok(Self {
      result,
      query,
      comparator,
    })
  }
}

impl IntersectVisitor for IntersectVisitorImpl1<'_> {
  fn grow(&mut self, count: usize) -> Result<()> {
    self.result.grow(count)?;
    Ok(())
  }

  fn visit(&mut self, doc_id: i32) -> Result<()> {
    self.result.add_doc(doc_id)?;
    Ok(())
  }

  fn visit_with_iterator<I>(&mut self, iterator: &mut I) -> Result<()>
  where
    I: DocIdSetIterator,
  {
    self.result.add_disi(iterator)?;
    Ok(())
  }

  fn visit_with_ints_ref(&mut self, ints_ref: &IntsRef<Vec<i32>>) -> Result<()> {
    for i in ints_ref.offset..(ints_ref.offset + ints_ref.length) {
      self.result.add_doc(ints_ref.ints[i])?;
    }
    Ok(())
  }

  fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
    if matches(self.query, self.comparator, packed_value)? {
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
    if matches(self.query, self.comparator, packed_value)? {
      self.result.add_disi(iterator)?;
    }
    Ok(())
  }

  fn compare(&self, min_packed_value: &[u8], max_packed_value: &[u8]) -> Result<Relation> {
    relate(
      self.query,
      self.comparator,
      min_packed_value,
      max_packed_value,
    )
  }
}

struct IntersectVisitorImpl2<'a> {
  query: &'a PointRangeQuery,
  comparator: &'a ByteArrayComparatorEnum,
  matching_node_count: i64,
}
impl<'a> IntersectVisitorImpl2<'a> {
  pub fn new(query: &'a PointRangeQuery, comparator: &'a ByteArrayComparatorEnum) -> Self {
    Self {
      query,
      comparator,
      matching_node_count: 0,
    }
  }
}
impl IntersectVisitor for IntersectVisitorImpl2<'_> {
  fn visit(&mut self, doc_id: i32) -> Result<()> {
    Err(LuceneError::unsupported_operation(format!(
      "This IntersectVisitor does not perform any actions on a docID={} node being visited",
      doc_id
    )))
  }

  fn visit_with_packed_value(&mut self, _doc_id: i32, packed_value: &[u8]) -> Result<()> {
    if matches(self.query, self.comparator, packed_value)? {
      self.matching_node_count += 1;
    }
    Ok(())
  }

  fn compare(&self, min_packed_value: &[u8], max_packed_value: &[u8]) -> Result<Relation> {
    relate(
      self.query,
      self.comparator,
      min_packed_value,
      max_packed_value,
    )
  }
}
pub trait PointRangeBase {
  /// Format a single point value as a human-readable string for debugging.
  ///
  /// # Parameters
  /// - `dimension`: the dimension index of this value.
  /// - `value`: the encoded point value .
  ///
  /// # Returns
  /// A human-readable representation of the value for debugging.
  fn to_string(&self, dimension: usize, value: &[u8]) -> Result<String>;
}
#[derive(Debug, Clone)]
pub enum PointRangeBaseEnum {
  Int(IntPointRangeQuery),
  BigInteger(BigIntegerPointRangeQuery),
  Long(LongPointRangeQuery),
  Float(FloatPointRangeQuery),
  HalfFloat(HalfFloatPointRangeQuery),
  Double(DoublePointRangeQuery),
  Binary(BinaryPointRangeQuery),
  InetAddress(InetAddressPointRangeQuery),
  LatLon(LatLonPointRangeQuery),
  #[cfg(test)]
  Test(PointRangeQueryBaseImpl),
}
impl_from_for_enum!(
    PointRangeBaseEnum,
    IntPointRangeQuery => Int,
    BigIntegerPointRangeQuery => BigInteger,
    LongPointRangeQuery => Long,
    FloatPointRangeQuery => Float,
    HalfFloatPointRangeQuery => HalfFloat,
    DoublePointRangeQuery => Double,
    BinaryPointRangeQuery => Binary,
    InetAddressPointRangeQuery => InetAddress,
    LatLonPointRangeQuery => LatLon,
);
#[cfg(test)]
impl_from_for_enum!(PointRangeBaseEnum, PointRangeQueryBaseImpl => Test);
impl PointRangeBase for PointRangeBaseEnum {
  fn to_string(&self, dimension: usize, value: &[u8]) -> Result<String> {
    match self {
      PointRangeBaseEnum::Int(q) => q.to_string(dimension, value),
      PointRangeBaseEnum::BigInteger(q) => q.to_string(dimension, value),
      PointRangeBaseEnum::Long(q) => q.to_string(dimension, value),
      PointRangeBaseEnum::Float(q) => q.to_string(dimension, value),
      PointRangeBaseEnum::HalfFloat(q) => q.to_string(dimension, value),
      PointRangeBaseEnum::Double(q) => q.to_string(dimension, value),
      PointRangeBaseEnum::Binary(q) => q.to_string(dimension, value),
      PointRangeBaseEnum::InetAddress(q) => q.to_string(dimension, value),
      PointRangeBaseEnum::LatLon(q) => q.to_string(dimension, value),
      #[cfg(test)]
      PointRangeBaseEnum::Test(q) => q.to_string(dimension, value),
    }
  }
}

impl crate::core::util::accountable::Accountable for PointRangeQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED)
  }
}
