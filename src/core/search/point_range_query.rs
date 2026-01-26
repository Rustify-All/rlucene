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
use crate::core::document::int_point::IntPointRangeQuery;
use crate::core::document::long_point::LongPointRangeQuery;
use crate::core::index::index_reader_context::{IRCTermState, IndexReaderContext};
use crate::core::index::leaf_reader::{LRPointValues, LeafReader};
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::point_values::{IntersectVisitor, PointTree, PointValues, Relation};
use crate::core::index::query_timeout::QueryTimeout;
use crate::core::index::term_states::TermStates;
use crate::core::search::QueryCache;
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::constant_score_weight::ConstantScoreWeight;
use crate::core::search::doc_id_set::DocIdSet;
use crate::core::search::doc_id_set_iterator::{AllDISI, DocIdSetIterator};
use crate::core::search::dummy::dummy_two_phase_iterator::DummyTwoPhaseIterator;
use crate::core::search::explanation::Explanation;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::matches_utils::MatchWithNoTerms;
use crate::core::search::query::{Query, QueryBase};
use crate::core::search::query_caching_policy::QueryCachingPolicy;
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::ScorerEnum2;
use crate::core::search::scorer_supplier::{ScorerSupplier, ScorerSupplierEnum2};
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::weight::{DefaultBulkScorer, Weight};
use crate::core::util::TryIntoInt;
use crate::core::util::array_util::{ArrayUtil, ByteArrayComparator, ByteArrayComparatorEnum};
use crate::core::util::bit_set::BitSet;
use crate::core::util::bit_set_iterator::BitSetIterator;
use crate::core::util::doc_id_set_builder::{DocIdSetBuilder, DocIdSetBuilderIterator};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::ints_ref::IntsRef;
#[cfg(test)]
use crate::test::search::test_point_queries::PointRangeQueryBaseImpl;
use std::fmt::Debug;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::sync::Arc;

/// Struct for range queries over single- or multi-dimensional point fields,
/// such as [`IntPoint`](crate::core::document::int_point::IntPoint).
///
/// This type is intended for subclasses and works directly on the underlying
/// binary encoding. For Lucene's standard point types, use the factory methods
/// on those types instead—for example, [`IntPoint::new_range_query`](crate::core::document::int_point::IntPoint::new_range_query) for fields
/// indexed with [`IntPoint`](crate::core::document::int_point::IntPoint).
///
/// For single-dimensional fields, this represents a simple range query; for
/// multi-dimensional fields, it represents a box-shaped query.
///
/// See also: [`PointValues`]
#[derive(Debug, Clone)]
pub struct PointRangeQuery {
    field: String,
    num_dims: usize,
    bytes_per_dim: usize,
    lower_point: Vec<u8>,
    upper_point: Vec<u8>,
    sub: PointRangeBaseEnum,
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
    pub fn new<S>(
        field: String,
        lower_point: Vec<u8>,
        upper_point: Vec<u8>,
        num_dims: usize,
        sub: S,
    ) -> Result<Self>
    where
        S: Into<PointRangeBaseEnum>,
    {
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
            field,
            num_dims,
            bytes_per_dim,
            lower_point,
            upper_point,
            sub: sub.into(),
        })
    }

    fn equals_to(&self, other: &PointRangeQuery) -> bool {
        self.field == other.field
            && self.num_dims == other.num_dims
            && self.bytes_per_dim == other.bytes_per_dim
            && self.lower_point == other.lower_point
            && self.upper_point == other.upper_point
    }
    pub fn to_string(&self, field: &str) -> String {
        let mut sb = String::new();

        if self.field != field {
            sb.push_str(&self.field);
            sb.push(':');
        }

        for i in 0..self.num_dims {
            if i > 0 {
                sb.push(',');
            }
            let start = self.bytes_per_dim * i;
            let end = start + self.bytes_per_dim;

            let lower = &self.lower_point[start..end];
            let upper = &self.upper_point[start..end];

            sb.push('[');
            sb.push_str(&self.sub.to_string(i, lower));
            sb.push_str(" TO ");
            sb.push_str(&self.sub.to_string(i, upper));
            sb.push(']');
        }

        sb
    }
    #[cfg(test)]
    pub(crate) fn get_lower_point(&self) -> &[u8] {
        &self.lower_point
    }
    #[cfg(test)]
    pub(crate) fn get_upper_point(&self) -> &[u8] {
        &self.upper_point
    }
}

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
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.field.hash(state);
        self.num_dims.hash(state);
        self.bytes_per_dim.hash(state);
        self.lower_point.hash(state);
        self.upper_point.hash(state);
    }
}
impl QueryBase for PointRangeQuery {
    fn as_string(&self, _field: &str) -> String {
        debug_assert!(false, "should never be called");
        "".to_string()
    }

    type Weight<S, IRC, QCP, QC>
        = PointRangeWeight<IRC::LeafReader>
    where
        S: Similarity,
        IRC: IndexReaderContext,
        QCP: QueryCachingPolicy,
        QC: QueryCache;
    fn create_weight<S, IRC, QT, QCP, QC>(
        self,
        _searcher: &IndexSearcher<IRC, S, QT, QCP, QC>,
        score_mode: &ScoreMode,
        boost: f32,
        _per_reader_term_state: Option<TermStates<IRCTermState<IRC>>>,
    ) -> Result<Self::Weight<S, IRC, QCP, QC>>
    where
        IRC: IndexReaderContext,
        S: Similarity,
        QT: QueryTimeout,
        QCP: QueryCachingPolicy,
        QC: QueryCache,
        Self: Sized,
    {
        Ok(PointRangeWeight::new(boost, self, *score_mode))
    }

    type RewriteQuery = PointRangeQuery;

    fn visit<QV>(&self, _visitor: &QV)
    where
        QV: QueryVisitor,
    {
        todo!()
    }
}

pub struct PointRangeWeight<LR>
where
    LR: LeafReader,
{
    base: ConstantScoreWeight,
    parent_query: Arc<Query>,
    comparator: ByteArrayComparatorEnum,
    _leaf_reader: PhantomData<LR>,
    query: Arc<PointRangeQuery>,
    score_mode: ScoreMode,
}
impl<LR> PointRangeWeight<LR>
where
    LR: LeafReader,
{
    pub fn new(score: f32, query: PointRangeQuery, score_mode: ScoreMode) -> Self {
        let comparator = ArrayUtil::get_unsigned_comparator(query.bytes_per_dim);
        let point_range_query = Arc::new(query.clone());
        let parent_query = Arc::new(query.into());
        Self {
            base: ConstantScoreWeight::new(score),
            parent_query,
            comparator,
            _leaf_reader: PhantomData,
            query: point_range_query,
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
        let field = &q.field;

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
    fn get_intersect_visitor(
        result: DocIdSetBuilder,
        weight: &'_ PointRangeWeight<LR>,
    ) -> IntersectVisitorImpl1 {
        IntersectVisitorImpl1::new(result, weight.query.clone(), weight.comparator.clone())
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
    fn point_count(&self, point_tree: &mut impl PointTree) -> Result<i64> {
        let mut visitor = IntersectVisitorImpl2::new(self.query.clone(), self.comparator.clone());
        self.point_count_with_visitor(&mut visitor, point_tree)?;
        Ok(visitor.matching_node_count)
    }

    fn point_count_with_visitor(
        &self,
        visitor: &mut IntersectVisitorImpl2,
        point_tree: &mut impl PointTree,
    ) -> Result<()> {
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
        if let Query::PointRange(v) = self.parent_query.as_ref() {
            Ok(v)
        } else {
            Err(LuceneError::illegal_state(""))
        }
    }
}

impl<LR> SegmentCacheable<LR> for PointRangeWeight<LR>
where
    LR: LeafReader,
{
    fn is_cacheable(&self, _ctx: &LeafReaderContext<LR>) -> Result<bool> {
        Ok(true)
    }
}

impl<LR> Weight<LR> for PointRangeWeight<LR>
where
    LR: LeafReader,
{
    type Matches = MatchWithNoTerms;

    fn matches(&self, context: &LeafReaderContext<LR>, doc: i32) -> Result<Option<Self::Matches>> {
        self.default_matches(context, doc)
    }

    fn explain(&self, context: &LeafReaderContext<LR>, doc: i32) -> Result<Explanation> {
        let scorer = self.scorer(context)?;
        self.base
            .explain(scorer, doc, self.parent_query.as_string(""))
    }

    fn get_query(&self) -> Arc<Query> {
        self.parent_query.clone()
    }

    type ScorerSupplier = PointRangeSs<LR>;

    fn scorer_supplier(
        &self,
        context: &LeafReaderContext<LR>,
    ) -> Result<Option<Self::ScorerSupplier>> {
        let reader = context.reader();

        let values_opt = reader.get_point_values(&self.query.field)?;
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

            let q = self.query.as_ref();
            let num_dims = q.num_dims;
            let bytes_per_dim = q.bytes_per_dim;

            for i in 0..num_dims {
                let offset = i * bytes_per_dim;

                if self.comparator.compare(
                    &q.lower_point,
                    offset,
                    field_packed_upper.as_ref(),
                    offset,
                ) > 0
                    || self.comparator.compare(
                        &q.upper_point,
                        offset,
                        field_packed_lower.as_ref(),
                        offset,
                    ) < 0
                {
                    // If this query is a required clause of a boolean query, then returning null here
                    // will help make sure that we don't call ScorerSupplier#get on other required clauses
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

            let q = self.query.as_ref();
            let num_dims = q.num_dims;
            let bytes_per_dim = q.bytes_per_dim;

            all_docs_match = true;
            for i in 0..num_dims {
                let offset = i * bytes_per_dim;

                if self.comparator.compare(
                    &q.lower_point,
                    offset,
                    field_packed_lower.as_ref(),
                    offset,
                ) > 0
                    || self.comparator.compare(
                        &q.upper_point,
                        offset,
                        field_packed_upper.as_ref(),
                        offset,
                    ) < 0
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
            Ok(Some(PointRangeWeightScorerSupplier::A(
                ScorerSupplierImpl::new(self.base.score(), self.score_mode, max_doc),
            )))
        } else {
            let result =
                DocIdSetBuilder::with_point_values(max_doc, &values, self.query.field.as_ref())?;
            Ok(Some(PointRangeWeightScorerSupplier::B(
                ScorerSupplierImpl1::new(
                    self.base.score(),
                    self.score_mode,
                    values,
                    Self::get_intersect_visitor(result, self),
                ),
            )))
        }
    }

    fn count(&self, context: &LeafReaderContext<LR>) -> Result<i32> {
        let query = self.point_range_query()?;
        let reader = context.reader();

        let values = reader.get_point_values(query.field.as_str())?;

        if !self.check_valid_point_values(values.as_ref())? {
            return Ok(0);
        }

        if !reader.has_deletions()? {
            let values = values.unwrap();

            let relation = relate(
                query,
                &self.comparator,
                values.get_min_packed_value()?.as_ref().unwrap().as_ref(),
                values.get_max_packed_value()?.as_ref().unwrap().as_ref(),
            )?;

            if relation == Relation::CellInsideQuery {
                return values.get_doc_count();
            }

            // only 1D: we have the guarantee that it will actually run fast since there are at most 2
            // crossing leaves.
            // docCount == size : counting according number of points in leaf node, so must be
            // single-valued.
            if query.num_dims == 1 && values.get_doc_count()? == values.size()? as i32 {
                let mut tree = values.get_point_tree()?;
                return Ok(self.point_count(&mut tree)? as i32);
            }
        }
        self.default_count(context)
    }
}
pub type PointRangeWeightScorerSupplier<PV> =
    ScorerSupplierEnum2<ScorerSupplierImpl, ScorerSupplierImpl1<PV>>;
pub(crate) fn matches(
    query: &PointRangeQuery,
    comparator: &ByteArrayComparatorEnum,
    packed_value: &[u8],
) -> Result<bool> {
    let num_dims = query.num_dims;
    let bytes_per_dim = query.bytes_per_dim;
    let mut offset = 0usize;
    for _ in 0..num_dims {
        if comparator.compare(packed_value, offset, query.lower_point.as_ref(), offset) < 0 {
            // Doc's value is too low, in this dimension
            return Ok(false);
        }
        if comparator.compare(packed_value, offset, query.upper_point.as_ref(), offset) > 0 {
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
        if comparator.compare(min_packed_value, offset, &query.upper_point, offset) > 0
            || comparator.compare(max_packed_value, offset, &query.lower_point, offset) < 0
        {
            return Ok(Relation::CellOutsideQuery);
        }

        if comparator.compare(min_packed_value, offset, &query.lower_point, offset) < 0
            || comparator.compare(max_packed_value, offset, &query.upper_point, offset) > 0
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
pub struct ScorerSupplierImpl1<PV>
where
    PV: PointValues,
{
    score: f32,
    score_mode: ScoreMode,
    values: PV,
    visitor: IntersectVisitorImpl1,
    cost: i64,
}
impl<PV> ScorerSupplierImpl1<PV>
where
    PV: PointValues,
{
    pub fn new(
        score: f32,
        score_mode: ScoreMode,
        values: PV,
        visitor: IntersectVisitorImpl1,
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
pub type PointRangeWeightScorer = ScorerEnum2<
    ConstantScoreScorer<BitSetIterator<FixedBitSet>, DummyTwoPhaseIterator>,
    ConstantScoreScorer<DocIdSetBuilderIterator, DummyTwoPhaseIterator>,
>;
impl<LR> ScorerSupplier<LR> for ScorerSupplierImpl1<LR::PointValues>
where
    LR: LeafReader,
{
    type Scorer = PointRangeWeightScorer;
    type BulkScorer = DefaultBulkScorer<Self::Scorer>;

    fn get(
        &mut self,
        _lead_cost: i64,
        context: &LeafReaderContext<LR>,
    ) -> Result<Option<Self::Scorer>> {
        let reader = context.reader();
        let v: i32 = self.values.size()?.try_convert()?;
        if self.values.get_doc_count()? == reader.max_doc()?
            && self.values.get_doc_count()? == v
            && self.cost(context)? > (reader.max_doc()? as i64 / 2)
        {
            let max_doc = reader.max_doc()?;
            // If all docs have exactly one value and the cost is greater
            // than half the leaf size then maybe we can make things faster
            // by computing the set of documents that do NOT match the range
            let mut result = FixedBitSet::new(max_doc as usize);
            result.set_with_range(0, max_doc as usize);
            let mut visitor = get_inverse_intersect_visitor(
                &mut result,
                max_doc as i64,
                &self.visitor.comparator,
                self.visitor.query.as_ref(),
            );
            self.values.intersect(&mut visitor)?;
            let cost = visitor.cost;
            let iterator = BitSetIterator::new(result, cost)?;
            return Ok(Some(PointRangeWeightScorer::A(
                ConstantScoreScorer::with_disi(self.score, self.score_mode, iterator),
            )));
        }
        self.values.intersect(&mut self.visitor)?;
        let iterator = self.visitor.result.build()?.iterator()?;
        debug_assert!(iterator.is_some());
        Ok(Some(PointRangeWeightScorer::B(
            ConstantScoreScorer::with_disi(self.score, self.score_mode, iterator.unwrap()),
        )))
    }

    fn bulk_scorer(&mut self, context: &LeafReaderContext<LR>) -> Result<Option<Self::BulkScorer>> {
        Ok(Some(self.default_bulk_scorer(context)?))
    }

    fn cost(&mut self, _context: &LeafReaderContext<LR>) -> Result<i64> {
        if self.cost == -1 {
            // Computing the cost may be expensive, so only do it if necessary
            self.cost = self.values.estimate_doc_count(&self.visitor)?;
            debug_assert!(self.cost >= 0);
        }
        Ok(self.cost)
    }
}
pub type PointRangeSs<LR> = PointRangeWeightScorerSupplier<LRPointValues<LR>>;
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
impl<LR> ScorerSupplier<LR> for ScorerSupplierImpl
where
    LR: LeafReader,
{
    type Scorer = ConstantScoreScorer<AllDISI, DummyTwoPhaseIterator>;
    type BulkScorer = DefaultBulkScorer<Self::Scorer>;

    fn get(
        &mut self,
        _lead_cost: i64,
        context: &LeafReaderContext<LR>,
    ) -> Result<Option<Self::Scorer>> {
        debug_assert!(context.reader().max_doc()? == self.max_doc);
        Ok(Some(ConstantScoreScorer::with_disi(
            self.score,
            self.score_mode,
            AllDISI::new(self.max_doc),
        )))
    }

    fn bulk_scorer(&mut self, context: &LeafReaderContext<LR>) -> Result<Option<Self::BulkScorer>> {
        Ok(Some(self.default_bulk_scorer(context)?))
    }

    fn cost(&mut self, context: &LeafReaderContext<LR>) -> Result<i64> {
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
        self.result.clear_with_index(doc_id as usize);
        self.cost -= 1;
        Ok(())
    }

    fn visit_with_iterator(&mut self, iterator: &mut impl DocIdSetIterator) -> Result<()> {
        self.result.and_not_iter(iterator)?;
        self.cost = (self.cost - iterator.cost()?).max(0);
        Ok(())
    }

    fn visit_with_ints_ref(&mut self, ints_ref: &IntsRef<Vec<i32>>) -> Result<()> {
        for i in ints_ref.offset..(ints_ref.offset + ints_ref.length) {
            self.result.clear_with_index(ints_ref.ints[i] as usize)
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

    fn visit_iterator_with_packed_value(
        &mut self,
        iterator: &mut impl DocIdSetIterator,
        packed_value: &[u8],
    ) -> Result<()> {
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
pub struct IntersectVisitorImpl1 {
    result: DocIdSetBuilder,
    query: Arc<PointRangeQuery>,
    comparator: ByteArrayComparatorEnum,
}

impl IntersectVisitorImpl1 {
    pub fn new(
        result: DocIdSetBuilder,
        query: Arc<PointRangeQuery>,
        comparator: ByteArrayComparatorEnum,
    ) -> Self {
        Self {
            result,
            query,
            comparator,
        }
    }
}

impl IntersectVisitor for IntersectVisitorImpl1 {
    fn grow(&mut self, count: usize) -> Result<()> {
        self.result.grow(count.try_convert()?);
        Ok(())
    }

    fn visit(&mut self, doc_id: i32) -> Result<()> {
        self.result.add_doc(doc_id);
        Ok(())
    }

    fn visit_with_iterator(&mut self, iterator: &mut impl DocIdSetIterator) -> Result<()> {
        self.result.add_disi(iterator)?;
        Ok(())
    }

    fn visit_with_ints_ref(&mut self, ints_ref: &IntsRef<Vec<i32>>) -> Result<()> {
        for i in ints_ref.offset..(ints_ref.offset + ints_ref.length) {
            self.result.add_doc(ints_ref.ints[i]);
        }
        Ok(())
    }

    fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
        if matches(&self.query, &self.comparator, packed_value)? {
            self.visit(doc_id)?;
        }
        Ok(())
    }

    fn visit_iterator_with_packed_value(
        &mut self,
        iterator: &mut impl DocIdSetIterator,
        packed_value: &[u8],
    ) -> Result<()> {
        if matches(&self.query, &self.comparator, packed_value)? {
            self.result.add_disi(iterator)?;
        }
        Ok(())
    }

    fn compare(&self, min_packed_value: &[u8], max_packed_value: &[u8]) -> Result<Relation> {
        relate(
            &self.query,
            &self.comparator,
            min_packed_value,
            max_packed_value,
        )
    }
}

struct IntersectVisitorImpl2 {
    query: Arc<PointRangeQuery>,
    comparator: ByteArrayComparatorEnum,
    matching_node_count: i64,
}
impl IntersectVisitorImpl2 {
    pub fn new(query: Arc<PointRangeQuery>, comparator: ByteArrayComparatorEnum) -> Self {
        Self {
            query,
            comparator,
            matching_node_count: 0,
        }
    }
}
impl IntersectVisitor for IntersectVisitorImpl2 {
    fn visit(&mut self, doc_id: i32) -> Result<()> {
        Err(LuceneError::unsupported_operation(format!(
            "This IntersectVisitor does not perform any actions on a docID={} node being visited",
            doc_id
        )))
    }

    fn visit_with_packed_value(&mut self, _doc_id: i32, packed_value: &[u8]) -> Result<()> {
        if matches(&self.query, &self.comparator, packed_value)? {
            self.matching_node_count += 1;
        }
        Ok(())
    }

    fn compare(&self, min_packed_value: &[u8], max_packed_value: &[u8]) -> Result<Relation> {
        relate(
            &self.query,
            &self.comparator,
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
    fn to_string(&self, dimension: usize, value: &[u8]) -> String;
}
#[derive(Debug, Clone)]
pub enum PointRangeBaseEnum {
    Int(IntPointRangeQuery),
    Long(LongPointRangeQuery),
    Float(FloatPointRangeQuery),
    Double(DoublePointRangeQuery),
    Binary(BinaryPointRangeQuery),
    #[cfg(test)]
    Test(PointRangeQueryBaseImpl),
}
impl From<IntPointRangeQuery> for PointRangeBaseEnum {
    fn from(v: IntPointRangeQuery) -> Self {
        PointRangeBaseEnum::Int(v)
    }
}
impl From<LongPointRangeQuery> for PointRangeBaseEnum {
    fn from(v: LongPointRangeQuery) -> Self {
        PointRangeBaseEnum::Long(v)
    }
}
impl From<FloatPointRangeQuery> for PointRangeBaseEnum {
    fn from(v: FloatPointRangeQuery) -> Self {
        PointRangeBaseEnum::Float(v)
    }
}
impl From<DoublePointRangeQuery> for PointRangeBaseEnum {
    fn from(v: DoublePointRangeQuery) -> Self {
        PointRangeBaseEnum::Double(v)
    }
}
impl From<BinaryPointRangeQuery> for PointRangeBaseEnum {
    fn from(v: BinaryPointRangeQuery) -> Self {
        PointRangeBaseEnum::Binary(v)
    }
}
#[cfg(test)]
impl From<PointRangeQueryBaseImpl> for PointRangeBaseEnum {
    fn from(v: PointRangeQueryBaseImpl) -> Self {
        PointRangeBaseEnum::Test(v)
    }
}
impl PointRangeBase for PointRangeBaseEnum {
    fn to_string(&self, dimension: usize, value: &[u8]) -> String {
        match self {
            PointRangeBaseEnum::Int(q) => q.to_string(dimension, value),
            PointRangeBaseEnum::Long(q) => q.to_string(dimension, value),
            PointRangeBaseEnum::Float(q) => q.to_string(dimension, value),
            PointRangeBaseEnum::Double(q) => q.to_string(dimension, value),
            PointRangeBaseEnum::Binary(q) => q.to_string(dimension, value),
            #[cfg(test)]
            PointRangeBaseEnum::Test(q) => q.to_string(dimension, value),
        }
    }
}
