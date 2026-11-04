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
use crate::core::document::binary_point::BinaryPointInSetQuery;
use crate::core::document::double_point::DoublePointInSetQuery;
use crate::core::document::float_point::FloatPointInSetQuery;
use crate::core::document::inet_address_point::InetAddressPointInSetQuery;
use crate::core::document::int_point::IntPointInSetQuery;
use crate::core::document::long_point::LongPointInSetQuery;
use crate::core::index::BytesRefValue;
use crate::core::index::bytes_ref_builder::BytesRefBuilder;
use crate::core::index::index_reader::{Identity, IndexReader};
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::point_values::{
  IntersectVisitor, MAX_INDEX_DIMENSIONS, MAX_NUM_BYTES, PointValues, Relation,
};
use crate::core::index::prefix_coded_terms::{
  PrefixCodedTerms, PrefixCodedTermsBuilder, TermIteratorRef,
};
use crate::core::search::constant_score_scorer::ConstantScoreDisiScorer;
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::constant_score_weight::ConstantScoreWeight;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::explanation::Explanation;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::query::QueryRef;
use crate::core::search::query::{QueryBase, QueryWeightSs};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::weight::Weight;
#[cfg(test)]
use crate::core::util::TryIntoInt;
use crate::core::util::accountable::Accountable;
use crate::core::util::array_util::{ArrayUtil, ByteArrayComparator, ByteArrayComparatorEnum};
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::core_helper::HasIdentity;
use crate::core::util::doc_id_set_builder::DocIdSetBuilder;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::sandbox::document::big_integer_point::BigIntegerPointInSetQuery;
use crate::sandbox::document::half_float_point::HalfFloatPointInSetQuery;
#[cfg(test)]
use crate::test_framework::core::search::point::MultiDimIntPointInSetQuery;
use std::cell::RefCell;
use std::fmt::Debug;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Base query trait for finding documents whose indexed point values are
/// contained in the specified set.
///
/// This works on the underlying binary encoding. Point field types should add
/// factory methods that pack typed values and pass a matching
/// [`PointInSetBase`] implementation for display.
#[derive(Debug, Clone)]
pub struct PointInSetQuery {
  id: Identity,
  sorted_packed_points: Arc<PrefixCodedTerms>,
  sorted_packed_points_hash_code: u64,
  field: Arc<str>,
  num_dims: usize,
  bytes_per_dim: usize,
  ram_bytes_used: i64,
  sub: PointInSetBaseEnum,
}

impl PointInSetQuery {
  /// The `packed_points` iterator must be in sorted order.
  pub fn new<S, I, FName>(
    field: FName,
    num_dims: usize,
    bytes_per_dim: usize,
    mut packed_points: I,
    sub: S,
  ) -> Result<Self>
  where
    S: Into<PointInSetBaseEnum>,
    I: BytesRefIterator,
    FName: Into<String>,
  {
    let field = field.into();
    if !(1..=MAX_NUM_BYTES).contains(&bytes_per_dim) {
      return Err(LuceneError::illegal_argument(format!(
        "bytesPerDim must be > 0 and <= {}; got {}",
        MAX_NUM_BYTES, bytes_per_dim
      )));
    }
    if !(1..=MAX_INDEX_DIMENSIONS).contains(&num_dims) {
      return Err(LuceneError::illegal_argument(format!(
        "numDims must be > 0 and <= {}; got {}",
        MAX_INDEX_DIMENSIONS, num_dims
      )));
    }

    let mut builder = PrefixCodedTermsBuilder::new();
    let mut previous: Option<BytesRefBuilder<Vec<u8>>> = None;

    while let Some(current) = packed_points.next()? {
      let current = current.as_bytes_ref();
      let packed_length = num_dims * bytes_per_dim;
      if current.length != packed_length {
        return Err(LuceneError::illegal_argument(format!(
          "packed point length should be {} but got {}; field=\"{}\" numDims={} bytesPerDim={}",
          packed_length, current.length, field, num_dims, bytes_per_dim
        )));
      }

      if let Some(prev) = previous.as_ref() {
        let prev = prev.bytes();
        match prev.as_bytes().cmp(current.as_bytes()) {
          std::cmp::Ordering::Equal => continue,
          std::cmp::Ordering::Greater => {
            return Err(LuceneError::illegal_argument(format!(
              "values are out of order: saw {:?} before {:?}",
              prev, current
            )));
          },
          std::cmp::Ordering::Less => {},
        }
      }

      builder.add(field.as_str(), &current)?;
      let previous_buffer = previous.get_or_insert_with(BytesRefBuilder::new);
      previous_buffer.copy_bytes_from_ref(&current)?;
      previous_buffer.bytes_mut().bytes.truncate(packed_length);
    }

    let sorted_packed_points = builder.finish();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    sorted_packed_points.hash(&mut hasher);
    let sorted_packed_points_hash_code = hasher.finish();
    let ram_bytes_used = field.len() as i64 + sorted_packed_points.ram_bytes_used()?;

    Ok(Self {
      id: Identity::new(),
      sorted_packed_points: Arc::new(sorted_packed_points),
      sorted_packed_points_hash_code,
      field: Arc::from(field),
      num_dims,
      bytes_per_dim,
      ram_bytes_used,
      sub: sub.into(),
    })
  }

  #[cfg(test)]
  pub fn get_packed_points(&self) -> Result<Vec<Vec<u8>>> {
    packed_points_as_vec(&self.sorted_packed_points)
  }

  pub fn field(&self) -> &str {
    &self.field
  }

  pub fn num_dims(&self) -> usize {
    self.num_dims
  }

  pub fn bytes_per_dim(&self) -> usize {
    self.bytes_per_dim
  }

  fn equals_to(&self, other: &PointInSetQuery) -> bool {
    std::mem::discriminant(&self.sub) == std::mem::discriminant(&other.sub)
      && self.field == other.field
      && self.num_dims == other.num_dims
      && self.bytes_per_dim == other.bytes_per_dim
      && self.sorted_packed_points_hash_code == other.sorted_packed_points_hash_code
      && self.sorted_packed_points == other.sorted_packed_points
  }

  pub fn to_string(&self, field: &str) -> Result<String> {
    let mut sb = String::new();
    if self.field.as_ref() != field {
      sb.push_str(&self.field);
      sb.push(':');
    }

    sb.push('{');
    let mut first = true;
    let mut iterator = self.sorted_packed_points.iterator()?;
    while let Some(point) = iterator.next()? {
      if !first {
        sb.push(' ');
      }
      first = false;
      let value = point;
      sb.push_str(
        &self
          .sub
          .to_string(&value.bytes[value.offset..value.offset + value.length])?,
      );
    }
    sb.push('}');
    Ok(sb)
  }
}

impl Accountable for PointInSetQuery {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(self.ram_bytes_used)
  }
}

impl Eq for PointInSetQuery {}

impl PartialEq<Self> for PointInSetQuery {
  fn eq(&self, other: &Self) -> bool {
    self.equals_to(other)
  }
}

impl Hash for PointInSetQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    std::mem::discriminant(&self.sub).hash(state);
    self.field.hash(state);
    self.sorted_packed_points_hash_code.hash(state);
    self.num_dims.hash(state);
    self.bytes_per_dim.hash(state);
  }
}

impl HasIdentity for PointInSetQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl QueryBase for PointInSetQuery {
  type Weight<IRC, SIM>
    = PointInSetWeight
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
    Ok(PointInSetWeight::new(boost, self, *score_mode))
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

pub struct PointInSetWeight {
  base: ConstantScoreWeight,
  parent_query: PointInSetQuery,
  score_mode: ScoreMode,
}

impl PointInSetWeight {
  pub fn new(score: f32, query: PointInSetQuery, score_mode: ScoreMode) -> Self {
    let parent_query = query;
    Self {
      base: ConstantScoreWeight::new(score),
      parent_query,
      score_mode,
    }
  }
}

impl<IRC> SegmentCacheable<IRC> for PointInSetWeight
where
  IRC: IndexReaderContext + 'static,
{
  fn is_cacheable(&self, _ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    Ok(true)
  }
}

impl<IRC, SIM> Weight<IRC, SIM> for PointInSetWeight
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
    QueryRef::PointInSet(&self.parent_query)
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
    let query = &self.parent_query;
    let reader = context.reader();
    let values = match reader.get_point_values(&query.field)? {
      Some(v) => v,
      None => return Ok(None),
    };

    if values.get_num_index_dimensions()? != query.num_dims {
      return Err(LuceneError::illegal_argument(format!(
        "field=\"{}\" was indexed with numIndexDims={} but this query has numIndexDims={}",
        query.field,
        values.get_num_index_dimensions()?,
        query.num_dims
      )));
    }
    if values.get_bytes_per_dimension()? != query.bytes_per_dim {
      return Err(LuceneError::illegal_argument(format!(
        "field=\"{}\" was indexed with bytesPerDim={} but this query has bytesPerDim={}",
        query.field,
        values.get_bytes_per_dimension()?,
        query.bytes_per_dim
      )));
    }

    let max_doc = reader.max_doc()?;
    if query.num_dims == 1 {
      Ok(Some(
        Box::new(MergePointScorerSupplier::new(
          self.base.score(),
          self.score_mode,
          values,
          max_doc,
          query.field.clone(),
          query.sorted_packed_points.clone(),
          query.bytes_per_dim,
        ))
        .into(),
      ))
    } else {
      Ok(Some(
        Box::new(SinglePointScorerSupplier::new(
          self.base.score(),
          self.score_mode,
          values,
          max_doc,
          query.field.clone(),
          query.sorted_packed_points.clone(),
          query.num_dims,
          query.bytes_per_dim,
        ))
        .into(),
      ))
    }
  }
}

pub struct MergePointScorerSupplier<PV> {
  score: f32,
  score_mode: ScoreMode,
  values: PV,
  max_doc: i32,
  field: Arc<str>,
  sorted_packed_points: Arc<PrefixCodedTerms>,
  bytes_per_dim: usize,
  cost: i64,
}

impl<PV> MergePointScorerSupplier<PV> {
  pub fn new<FName>(
    score: f32,
    score_mode: ScoreMode,
    values: PV,
    max_doc: i32,
    field: FName,
    sorted_packed_points: Arc<PrefixCodedTerms>,
    bytes_per_dim: usize,
  ) -> Self
  where
    FName: Into<Arc<str>>,
  {
    let field = field.into();
    Self {
      score,
      score_mode,
      values,
      max_doc,
      field,
      sorted_packed_points,
      bytes_per_dim,
      cost: -1,
    }
  }
}

impl<IRC, SIM> ScorerSupplier<IRC, SIM>
  for MergePointScorerSupplier<<IRCLeafReader<IRC> as LeafReader>::PointValues>
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
    let mut visitor = MergePointVisitor::new(
      &self.sorted_packed_points,
      DocIdSetBuilder::from_point_values(self.max_doc, &self.values, &self.field)?,
      self.bytes_per_dim,
    )?;
    self.values.intersect(&mut visitor)?;
    let iterator = visitor.result.build_iterator()?;
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
      let visitor = MergePointVisitor::new(
        &self.sorted_packed_points,
        DocIdSetBuilder::from_point_values(self.max_doc, &self.values, &self.field)?,
        self.bytes_per_dim,
      )?;
      self.cost = self.values.estimate_doc_count(&visitor)?;
      debug_assert!(self.cost >= 0);
    }
    Ok(self.cost)
  }
}

pub struct SinglePointScorerSupplier<PV> {
  score: f32,
  score_mode: ScoreMode,
  values: PV,
  max_doc: i32,
  field: Arc<str>,
  sorted_packed_points: Arc<PrefixCodedTerms>,
  num_dims: usize,
  bytes_per_dim: usize,
  cost: i64,
}

impl<PV> SinglePointScorerSupplier<PV> {
  #[allow(clippy::too_many_arguments)]
  pub fn new<FName>(
    score: f32,
    score_mode: ScoreMode,
    values: PV,
    max_doc: i32,
    field: FName,
    sorted_packed_points: Arc<PrefixCodedTerms>,
    num_dims: usize,
    bytes_per_dim: usize,
  ) -> Self
  where
    FName: Into<Arc<str>>,
  {
    let field = field.into();
    Self {
      score,
      score_mode,
      values,
      max_doc,
      field,
      sorted_packed_points,
      num_dims,
      bytes_per_dim,
      cost: -1,
    }
  }
}

impl<IRC, SIM> ScorerSupplier<IRC, SIM>
  for SinglePointScorerSupplier<<IRCLeafReader<IRC> as LeafReader>::PointValues>
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
    let result = DocIdSetBuilder::from_point_values(self.max_doc, &self.values, &self.field)?;
    let mut visitor = SinglePointVisitor::new(result, self.num_dims, self.bytes_per_dim);
    let mut iterator = self.sorted_packed_points.iterator()?;
    while let Some(point) = iterator.next()? {
      visitor.set_point(&point.bytes[point.offset..point.offset + point.length]);
      self.values.intersect(&mut visitor)?;
    }
    let iterator = visitor.result.build_iterator()?;
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
      let result = DocIdSetBuilder::from_point_values(self.max_doc, &self.values, &self.field)?;
      let mut visitor = SinglePointVisitor::new(result, self.num_dims, self.bytes_per_dim);
      let mut cost = 0;
      let mut iterator = self.sorted_packed_points.iterator()?;
      while let Some(point) = iterator.next()? {
        visitor.set_point(&point.bytes[point.offset..point.offset + point.length]);
        cost += self.values.estimate_doc_count(&visitor)?;
      }
      self.cost = cost;
      debug_assert!(self.cost >= 0);
    }
    Ok(self.cost)
  }
}

/// Essentially does a merge sort, collecting hits when the indexed point and
/// query point are the same. This is an optimization for the 1D case.
pub struct MergePointVisitor<'a> {
  result: DocIdSetBuilder,
  iterator: RefCell<TermIteratorRef<'a>>,
  next_query_point: RefCell<Option<smallvec::SmallVec<[u8; 16]>>>,
  comparator: ByteArrayComparatorEnum,
}

impl<'a> MergePointVisitor<'a> {
  pub fn new(
    sorted_packed_points: &'a PrefixCodedTerms,
    result: DocIdSetBuilder,
    bytes_per_dim: usize,
  ) -> Result<Self> {
    let mut iterator = sorted_packed_points.iterator()?;
    let next_query_point = iterator
      .next()?
      .map(|point| smallvec::SmallVec::from_slice(point.as_bytes()));
    Ok(Self {
      result,
      iterator: RefCell::new(iterator),
      next_query_point: RefCell::new(next_query_point),
      comparator: ArrayUtil::get_unsigned_comparator(bytes_per_dim),
    })
  }

  fn next_query_point(&self) -> Result<()> {
    let mut iterator = self.iterator.borrow_mut();
    let point = iterator.next()?;
    let mut next_query_point = self.next_query_point.borrow_mut();
    match point {
      Some(point) => {
        let bytes = next_query_point.get_or_insert_with(smallvec::SmallVec::new);
        bytes.clear();
        bytes.extend_from_slice(point.as_bytes());
      },
      None => *next_query_point = None,
    }
    Ok(())
  }

  fn matches(&mut self, packed_value: &[u8]) -> Result<bool> {
    loop {
      let cmp = match self.next_query_point.get_mut().as_ref() {
        Some(next_query_point) => self
          .comparator
          .compare(next_query_point, 0, packed_value, 0),
        None => return Ok(false),
      };
      if cmp == 0 {
        return Ok(true);
      } else if cmp < 0 {
        self.next_query_point()?;
      } else {
        break;
      }
    }
    Ok(false)
  }
}

impl IntersectVisitor for MergePointVisitor<'_> {
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

  fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
    if self.matches(packed_value)? {
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
    if self.matches(packed_value)? {
      self.result.add_disi(iterator)?;
    }
    Ok(())
  }

  fn compare(&self, min_packed_value: &[u8], max_packed_value: &[u8]) -> Result<Relation> {
    loop {
      let next_query_point_ref = self.next_query_point.borrow();
      let Some(next_query_point) = next_query_point_ref.as_ref() else {
        return Ok(Relation::CellOutsideQuery);
      };
      let cmp_min = self
        .comparator
        .compare(next_query_point, 0, min_packed_value, 0);
      if cmp_min < 0 {
        drop(next_query_point_ref);
        self.next_query_point()?;
        continue;
      }
      let cmp_max = self
        .comparator
        .compare(next_query_point, 0, max_packed_value, 0);
      if cmp_max > 0 {
        return Ok(Relation::CellOutsideQuery);
      }

      if cmp_min == 0 && cmp_max == 0 {
        return Ok(Relation::CellInsideQuery);
      } else {
        return Ok(Relation::CellCrossesQuery);
      }
    }
  }
}

/// IntersectVisitor that queries against a single point, used in the > 1D case.
pub struct SinglePointVisitor {
  result: DocIdSetBuilder,
  comparator: ByteArrayComparatorEnum,
  num_dims: usize,
  bytes_per_dim: usize,
  point_bytes: Vec<u8>,
}

impl SinglePointVisitor {
  pub fn new(result: DocIdSetBuilder, num_dims: usize, bytes_per_dim: usize) -> Self {
    Self {
      result,
      comparator: ArrayUtil::get_unsigned_comparator(bytes_per_dim),
      num_dims,
      bytes_per_dim,
      point_bytes: vec![0u8; bytes_per_dim * num_dims],
    }
  }

  pub fn set_point(&mut self, point: &[u8]) {
    debug_assert!(point.len() == self.point_bytes.len());
    self.point_bytes.copy_from_slice(point);
  }
}

impl IntersectVisitor for SinglePointVisitor {
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

  fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
    debug_assert!(packed_value.len() == self.point_bytes.len());
    if packed_value == self.point_bytes {
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
    debug_assert!(packed_value.len() == self.point_bytes.len());
    if packed_value == self.point_bytes {
      self.result.add_disi(iterator)?;
    }
    Ok(())
  }

  fn compare(&self, min_packed_value: &[u8], max_packed_value: &[u8]) -> Result<Relation> {
    let mut crosses = false;

    for dim in 0..self.num_dims {
      let offset = dim * self.bytes_per_dim;

      let cmp_min = self
        .comparator
        .compare(min_packed_value, offset, &self.point_bytes, offset);
      if cmp_min > 0 {
        return Ok(Relation::CellOutsideQuery);
      }

      let cmp_max = self
        .comparator
        .compare(max_packed_value, offset, &self.point_bytes, offset);
      if cmp_max < 0 {
        return Ok(Relation::CellOutsideQuery);
      }

      if cmp_min != 0 || cmp_max != 0 {
        crosses = true;
      }
    }

    if crosses {
      Ok(Relation::CellCrossesQuery)
    } else {
      Ok(Relation::CellInsideQuery)
    }
  }
}

#[cfg(test)]
fn packed_points_as_vec(sorted_packed_points: &PrefixCodedTerms) -> Result<Vec<Vec<u8>>> {
  let mut iterator = sorted_packed_points.iterator()?;
  let mut points = Vec::with_capacity(sorted_packed_points.size().try_convert()?);
  while let Some(point) = iterator.next()? {
    points.push(point.bytes[point.offset..point.offset + point.length].to_vec());
  }
  Ok(points)
}

pub trait PointInSetBase {
  /// Format a single packed point value as a human-readable string for debugging.
  fn to_string(&self, value: &[u8]) -> Result<String>;
}

/// Default raw-byte formatter for callers that do not have a typed formatter.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DefaultPointInSetQuery;

impl PointInSetBase for DefaultPointInSetQuery {
  fn to_string(&self, value: &[u8]) -> Result<String> {
    Ok(format!("{:?}", value))
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PointInSetBaseEnum {
  Default(DefaultPointInSetQuery),
  BigInteger(BigIntegerPointInSetQuery),
  Binary(BinaryPointInSetQuery),
  Double(DoublePointInSetQuery),
  Float(FloatPointInSetQuery),
  HalfFloat(HalfFloatPointInSetQuery),
  InetAddress(InetAddressPointInSetQuery),
  Int(IntPointInSetQuery),
  Long(LongPointInSetQuery),
  #[cfg(test)]
  MultiDimInt(MultiDimIntPointInSetQuery),
}

impl From<DefaultPointInSetQuery> for PointInSetBaseEnum {
  fn from(value: DefaultPointInSetQuery) -> Self {
    Self::Default(value)
  }
}

impl From<BigIntegerPointInSetQuery> for PointInSetBaseEnum {
  fn from(value: BigIntegerPointInSetQuery) -> Self {
    Self::BigInteger(value)
  }
}

impl From<BinaryPointInSetQuery> for PointInSetBaseEnum {
  fn from(value: BinaryPointInSetQuery) -> Self {
    Self::Binary(value)
  }
}

impl From<DoublePointInSetQuery> for PointInSetBaseEnum {
  fn from(value: DoublePointInSetQuery) -> Self {
    Self::Double(value)
  }
}

impl From<FloatPointInSetQuery> for PointInSetBaseEnum {
  fn from(value: FloatPointInSetQuery) -> Self {
    Self::Float(value)
  }
}

impl From<HalfFloatPointInSetQuery> for PointInSetBaseEnum {
  fn from(value: HalfFloatPointInSetQuery) -> Self {
    Self::HalfFloat(value)
  }
}

impl From<InetAddressPointInSetQuery> for PointInSetBaseEnum {
  fn from(value: InetAddressPointInSetQuery) -> Self {
    Self::InetAddress(value)
  }
}

impl From<IntPointInSetQuery> for PointInSetBaseEnum {
  fn from(value: IntPointInSetQuery) -> Self {
    Self::Int(value)
  }
}

impl From<LongPointInSetQuery> for PointInSetBaseEnum {
  fn from(value: LongPointInSetQuery) -> Self {
    Self::Long(value)
  }
}

impl PointInSetBase for PointInSetBaseEnum {
  fn to_string(&self, value: &[u8]) -> Result<String> {
    match self {
      PointInSetBaseEnum::Default(q) => q.to_string(value),
      PointInSetBaseEnum::BigInteger(q) => q.to_string(value),
      PointInSetBaseEnum::Binary(q) => q.to_string(value),
      PointInSetBaseEnum::Double(q) => q.to_string(value),
      PointInSetBaseEnum::Float(q) => q.to_string(value),
      PointInSetBaseEnum::HalfFloat(q) => q.to_string(value),
      PointInSetBaseEnum::InetAddress(q) => q.to_string(value),
      PointInSetBaseEnum::Int(q) => q.to_string(value),
      PointInSetBaseEnum::Long(q) => q.to_string(value),
      #[cfg(test)]
      PointInSetBaseEnum::MultiDimInt(q) => q.to_string(value),
    }
  }
}
