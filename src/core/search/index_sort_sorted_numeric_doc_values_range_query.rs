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
use crate::core::document::int_point::IntPoint;
use crate::core::document::long_point::LongPoint;
use crate::core::document::sorted_numeric_doc_values_range_query::{
    SortedNumericDocValuesRangeQuery, SortedNumericDocValuesRangeQueryWeight,
};
use crate::core::document::sorted_numeric_doc_values_set_query::{
    SortedNumericDocValuesSetQuery, SortedNumericDocValuesSetQueryWeight,
};
use crate::core::document::sorted_set_doc_values_range_query::{
    SortedSetDocValuesRangeQuery, SortedSetDocValuesRangeQueryWeight,
};
use crate::core::index::doc_values::{DocValues, SortedNumeric};
use crate::core::index::index_reader_context::{IRCTermState, IndexReaderContext};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::point_values::{IntersectVisitor, PointTree, PointValues, Relation};
use crate::core::index::query_timeout::QueryTimeout;
use crate::core::index::sorted_numeric_doc_values::SortedNumericDocValues;
use crate::core::index::term_states::TermStates;
use crate::core::search::QueryCache;
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::constant_score_weight::ConstantScoreWeight;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::search::doc_id_set_iterator::{
    AllDISI, DocIdSetIterator, DocIdSetIteratorEnum4, EmptyDISI, RangeDISI,
};
use crate::core::search::dummy::dummy_query::DummyQuery;
use crate::core::search::dummy::dummy_scorer::DummyScorer;
use crate::core::search::dummy::dummy_two_phase_iterator::DummyTwoPhaseIterator;
use crate::core::search::explanation::Explanation;
use crate::core::search::field_comparator::{FieldComparator, FieldComparatorEnum};
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::leaf_field_comparator::{LeafFieldComparator, LeafFieldComparatorEnum};
use crate::core::search::matches_utils::MatchWithNoTerms;
use crate::core::search::point_range_query::{PointRangeQuery, PointRangeWeight};
use crate::core::search::pruning::Pruning;
use crate::core::search::query::{Query, QueryBase};
use crate::core::search::query_caching_policy::QueryCachingPolicy;
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::Scorer;
use crate::core::search::scorer_supplier::{ScorerSupplier, ScorerSupplierEnum2};
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::sort_field::{MissingValueEnum, SortFieldType, SortFiledBase};
use crate::core::search::sort_field_enum::SortFieldEnum;
use crate::core::search::weight::{DefaultBulkScorer, Weight, WeightEnum4};
use crate::core::util::TryIntoInt;
use crate::core::util::array_util::{ArrayUtil, ByteArrayComparator, ByteArrayComparatorEnum};
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct IndexSortSortedNumericDocValuesRangeQuery {
    field: String,
    lower_value: i64,
    upper_value: i64,
    fallback_query: FallbackQuery,
}
impl IndexSortSortedNumericDocValuesRangeQuery {
    pub fn new<S, T>(field: S, lower_value: i64, upper_value: i64, fallback_query: T) -> Self
    where
        S: Into<String>,
        T: Into<FallbackQuery>,
    {
        Self {
            field: field.into(),
            lower_value,
            upper_value,
            fallback_query: fallback_query.into(),
        }
    }
}
impl PartialEq for IndexSortSortedNumericDocValuesRangeQuery {
    fn eq(&self, other: &Self) -> bool {
        self.lower_value == other.lower_value
            && self.upper_value == other.upper_value
            && self.field == other.field
            && self.fallback_query == other.fallback_query
    }
}

impl Eq for IndexSortSortedNumericDocValuesRangeQuery {}

impl Hash for IndexSortSortedNumericDocValuesRangeQuery {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.field.hash(state);
        self.lower_value.hash(state);
        self.upper_value.hash(state);
        self.fallback_query.hash(state);
    }
}

impl QueryBase for IndexSortSortedNumericDocValuesRangeQuery {
    fn as_string(&self, field: &str) -> String {
        let mut s = String::new();

        if self.field != field {
            s.push_str(&self.field);
            s.push(':');
        }

        s.push('[');
        s.push_str(&self.lower_value.to_string());
        s.push_str(" TO ");
        s.push_str(&self.upper_value.to_string());
        s.push(']');
        s
    }

    type Weight<S, IRC, QCP, QC>
        = IndexSortSortedNumericDocValuesRangeQueryWeight<IRC::LeafReader>
    where
        S: Similarity,
        IRC: IndexReaderContext,
        QCP: QueryCachingPolicy,
        QC: QueryCache;

    fn create_weight<S, IRC, QT, QCP, QC>(
        self,
        searcher: &IndexSearcher<IRC, S, QT, QCP, QC>,
        score_mode: &ScoreMode,
        boost: f32,
        per_reader_term_state: Option<TermStates<IRCTermState<IRC>>>,
    ) -> Result<Self::Weight<S, IRC, QCP, QC>>
    where
        IRC: IndexReaderContext,
        S: Similarity,
        QT: QueryTimeout,
        QCP: QueryCachingPolicy,
        QC: QueryCache,
        Self: Sized,
    {
        let query = self.clone();
        let fallback_query_weight =
            match self.fallback_query {
                FallbackQuery::PointRange(p) => FallbackQueryWeight::A(p.create_weight(
                    searcher,
                    score_mode,
                    boost,
                    per_reader_term_state,
                )?),

                FallbackQuery::SortedNumericDocValuesSet(p) => FallbackQueryWeight::B(
                    p.create_weight(searcher, score_mode, boost, per_reader_term_state)?,
                ),
                FallbackQuery::SortedNumericDocValuesRange(p) => FallbackQueryWeight::C(
                    p.create_weight(searcher, score_mode, boost, per_reader_term_state)?,
                ),
                FallbackQuery::SortedSetDocValuesRange(p) => FallbackQueryWeight::D(
                    p.create_weight(searcher, score_mode, boost, per_reader_term_state)?,
                ),
            };
        Ok(IndexSortSortedNumericDocValuesRangeQueryWeight::new(
            query,
            ConstantScoreWeight::new(boost),
            *score_mode,
            fallback_query_weight,
        ))
    }

    type RewriteQuery = DummyQuery;

    fn visit<QV>(&self, _visitor: &QV)
    where
        QV: QueryVisitor,
    {
        todo!()
    }
}

pub struct IndexSortSortedNumericDocValuesRangeQueryWeight<LR>
where
    LR: LeafReader,
{
    query: IndexSortSortedNumericDocValuesRangeQuery,
    base: ConstantScoreWeight,
    score_mode: ScoreMode,
    fallback_query_weight: FallbackQueryWeight<LR>,
    parent_query: Arc<Query>,
}
impl<LR> IndexSortSortedNumericDocValuesRangeQueryWeight<LR>
where
    LR: LeafReader,
{
    pub fn new(
        query: IndexSortSortedNumericDocValuesRangeQuery,
        base: ConstantScoreWeight,
        score_mode: ScoreMode,
        fallback_query_weight: FallbackQueryWeight<LR>,
    ) -> Self {
        let query_clone = query.clone();
        Self {
            query: query_clone,
            base,
            score_mode,
            fallback_query_weight,
            parent_query: Arc::new(query.into()),
        }
    }
}
pub type Disi<LR> = <SortedNumeric<LR> as SortedNumericDocValues>::NumericDocValues;

impl<LR> SegmentCacheable<LR> for IndexSortSortedNumericDocValuesRangeQueryWeight<LR>
where
    LR: LeafReader,
{
    fn is_cacheable(&self, ctx: &LeafReaderContext<LR>) -> Result<bool> {
        // Both queries should always return the same values, so we can just check
        // if the fallback query is cacheable.
        self.fallback_query_weight.is_cacheable(ctx)
    }
}
pub type IndexSortSortedNumericDocValuesRangeSs<LR> = ScorerSupplierEnum2<
    ScorerSupplierImpl<Disi<LR>>,
    <FallbackQueryWeight<LR> as Weight<LR>>::ScorerSupplier,
>;
pub type ISSNDVRSsScorer<LR> =
    <IndexSortSortedNumericDocValuesRangeSs<LR> as ScorerSupplier<LR>>::Scorer;
pub type ISSNDVRSsScorerDisi<LR> = <ISSNDVRSsScorer<LR> as Scorer>::DocIdSetIterator;

impl<LR> Weight<LR> for IndexSortSortedNumericDocValuesRangeQueryWeight<LR>
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

    type ScorerSupplier = IndexSortSortedNumericDocValuesRangeSs<LR>;

    fn scorer_supplier(
        &self,
        context: &LeafReaderContext<LR>,
    ) -> Result<Option<Self::ScorerSupplier>> {
        match get_doc_id_set_iterator_or_null(
            context,
            self.query.lower_value,
            self.query.upper_value,
            &self.query.field,
        )? {
            Some(it_and_count) => {
                let disi = it_and_count.it;
                let scorer_supplier = ScorerSupplierImpl::new(
                    disi,
                    self.score_mode,
                    self.query.lower_value,
                    self.query.upper_value,
                    self.query.field.clone(),
                    self.base.score(),
                )?;
                Ok(Some(ScorerSupplierEnum2::A(scorer_supplier)))
            },
            None => match self.fallback_query_weight.scorer_supplier(context)? {
                Some(v) => Ok(Some(ScorerSupplierEnum2::B(v))),
                None => Ok(None),
            },
        }
    }

    fn count(&self, context: &LeafReaderContext<LR>) -> Result<i32>
    where
        LR: LeafReader,
    {
        let reader = context.reader();

        if !reader.has_deletions()? {
            if self.query.lower_value > self.query.upper_value {
                return Ok(0);
            }

            let mut sorted_numeric_values =
                DocValues::get_sorted_numeric(reader, &self.query.field)?;
            if !sorted_numeric_values.is_single_valued() {
                return self.fallback_query_weight.count(context);
            }
            let numeric_values = DocValues::unwrap_singleton_numeric(&mut sorted_numeric_values)?;

            let point_values = reader.get_point_values(&self.query.field)?;

            if let Some(ref points) = point_values
                && points.get_doc_count()? == reader.max_doc()?
            {
                let (opt_itc, _delegate_opt) = get_doc_id_set_iterator_or_null_from_bkd(
                    context,
                    numeric_values,
                    self.query.lower_value,
                    self.query.upper_value,
                    &self.query.field,
                )?;

                if let Some(itc) = opt_itc
                    && itc.count != -1
                {
                    return Ok(itc.count);
                }
            }
            // use index sort optimization if possible
            let meta = reader.get_metadata()?;
            if let Some(index_sort) = meta.get_sort() {
                let sort_fields = index_sort.get_sort();

                if !sort_fields.is_empty() && sort_fields[0].get_field() == Some(&self.query.field)
                {
                    let sort_field = &sort_fields[0];
                    let sort_field_type = get_sort_field_type(sort_field);
                    // The index sort optimization is only supported for Type.INT and Type.LONG
                    if sort_field_type == SortFieldType::Int
                        || sort_field_type == SortFieldType::Long
                    {
                        let missing_long_value = match sort_field.get_missing_value() {
                            None => 0i64,
                            Some(MissingValueEnum::Long(v)) => *v,
                            Some(MissingValueEnum::Int(v)) => *v as i64,
                            _ => {
                                return Err(LuceneError::illegal_argument(
                                    "Missing value for SortedNumericSortField must be Long/Int",
                                ));
                            },
                        };

                        let all_docs_have_values = match point_values {
                            Some(ref pv) => pv.get_doc_count()? == reader.max_doc()?,
                            None => false,
                        };

                        if all_docs_have_values
                            || (missing_long_value < self.query.lower_value
                                || missing_long_value > self.query.upper_value)
                        {
                            // TODO IMPORTANT numeric_values sometimes called twice we should optimism it
                            let mut sorted_numeric_values =
                                DocValues::get_sorted_numeric(reader, &self.query.field)?;
                            if !sorted_numeric_values.is_single_valued() {
                                return Err(LuceneError::illegal_argument(""));
                            }
                            let numeric_values =
                                DocValues::unwrap_singleton_numeric(&mut sorted_numeric_values)?;
                            let itc = get_doc_id_set_iterator(
                                sort_field,
                                context,
                                numeric_values,
                                self.query.lower_value,
                                self.query.upper_value,
                                &self.query.field,
                            )?;
                            if itc.count != -1 {
                                return Ok(itc.count);
                            }
                        }
                    }
                }
            }
        }
        self.fallback_query_weight.count(context)
    }
}

pub struct ScorerSupplierImpl<D>
where
    D: DocIdSetIterator,
{
    disi: Option<IteratorAndCountDisi<D>>,
    score_mode: ScoreMode,
    cost: i64,
    lower_value: i64,
    upper_value: i64,
    field: String,
    score: f32,
}
impl<D> ScorerSupplierImpl<D>
where
    D: DocIdSetIterator,
{
    pub fn new(
        disi: IteratorAndCountDisi<D>,
        score_mode: ScoreMode,
        lower_value: i64,
        upper_value: i64,
        field: String,
        score: f32,
    ) -> Result<Self> {
        let cost = disi.cost()?;
        Ok(Self {
            disi: Some(disi),
            score_mode,
            cost,
            lower_value,
            upper_value,
            field,
            score,
        })
    }
}
impl<LR> ScorerSupplier<LR> for ScorerSupplierImpl<Disi<LR>>
where
    LR: LeafReader,
{
    type Scorer = ConstantScoreScorer<IteratorAndCountDisi<Disi<LR>>, DummyTwoPhaseIterator>;
    type BulkScorer = DefaultBulkScorer<Self::Scorer>;

    fn get(
        &mut self,
        _lead_cost: i64,
        context: &LeafReaderContext<LR>,
    ) -> Result<Option<Self::Scorer>> {
        let disi = match self.disi.take() {
            Some(disi) => disi,
            None => {
                match get_doc_id_set_iterator_or_null(
                    context,
                    self.lower_value,
                    self.upper_value,
                    &self.field,
                )? {
                    Some(mut it_and_count) => std::mem::take(&mut it_and_count.it),
                    None => return Err(LuceneError::illegal_state("should not be here")),
                }
            },
        };
        let v = ConstantScoreScorer::with_disi(self.score, self.score_mode, disi);
        Ok(Some(v))
    }

    fn bulk_scorer(&mut self, context: &LeafReaderContext<LR>) -> Result<Option<Self::BulkScorer>> {
        Ok(Some(self.default_bulk_scorer(context)?))
    }

    fn cost(&mut self, _context: &LeafReaderContext<LR>) -> Result<i64> {
        Ok(self.cost)
    }
}
struct ValueAndDoc {
    value: Option<Vec<u8>>,
    doc_id: i32,
    done: bool,
}
impl ValueAndDoc {
    pub fn new() -> Self {
        Self {
            value: None,
            doc_id: 0,
            done: false,
        }
    }
}
fn find_next_value<P>(
    point_tree: &mut P,
    value: &[u8],
    allow_equal: bool,
    comparator: &ByteArrayComparatorEnum,
    last_doc: bool,
) -> Result<Option<ValueAndDoc>>
where
    P: PointTree,
{
    let cmp = comparator.compare(point_tree.get_max_packed_value()?.as_ref(), 0, value, 0);

    if cmp < 0 || (cmp == 0 && !allow_equal) {
        return Ok(None);
    }

    if !point_tree.move_to_child()? {
        let mut vd = ValueAndDoc::new();
        let mut visitor =
            IntersectVisitorImpl::new(&mut vd, comparator, value, last_doc, allow_equal);
        point_tree.visit_doc_values(&mut visitor)?;

        if vd.value.is_some() {
            return Ok(Some(vd));
        } else {
            return Ok(None);
        }
    }
    loop {
        if let Some(vd) = find_next_value(point_tree, value, allow_equal, comparator, last_doc)? {
            return Ok(Some(vd));
        }

        if !point_tree.move_to_sibling()? {
            break;
        }
    }

    let moved = point_tree.move_to_parent()?;
    debug_assert!(moved);
    Ok(None)
}
fn next_doc<P>(
    point_tree: &mut P,
    value: &[u8],
    allow_equal: bool,
    comparator: &ByteArrayComparatorEnum,
    last_doc_flag: bool,
) -> Result<i32>
where
    P: PointTree,
{
    let vd_opt = find_next_value(point_tree, value, allow_equal, comparator, last_doc_flag)?;

    let vd = match vd_opt {
        Some(v) => v,
        None => return Ok(-1),
    };

    if !last_doc_flag || vd.done {
        return Ok(vd.doc_id);
    }

    // We found the next value, now we need the last doc ID.
    let doc = last_doc(point_tree, vd.value.as_ref().unwrap(), comparator)?;

    if doc == -1 {
        // vd.docID was actually the last doc ID
        Ok(vd.doc_id)
    } else {
        Ok(doc)
    }
}
fn last_doc<P>(
    point_tree: &mut P,
    value: &[u8],
    comparator: &ByteArrayComparatorEnum,
) -> Result<i32>
where
    P: PointTree,
{
    // Create a stack of nodes that may contain value that we'll use to search for the last leaf
    // node that contains `value`.
    // While the logic looks a bit complicated due to the fact that the PointTree API doesn't allow
    // moving back to previous siblings, this effectively performs a binary search.
    let mut stack: Vec<P> = Vec::new();

    'outer: loop {
        // Move to the next node
        while !point_tree.move_to_sibling()? {
            if !point_tree.move_to_parent()? {
                // No next node
                break 'outer;
            }
        }

        let cmp = comparator.compare(point_tree.get_min_packed_value()?.as_ref(), 0, value, 0);
        if cmp > 0 {
            // This node doesn't have `value`, so next nodes can't either
            break;
        }

        stack.push(point_tree.try_clone()?);
    }

    // Now search stack nodes
    while let Some(mut next) = stack.pop() {
        if !next.move_to_child()? {
            let mut visitor = IntersectVisitorImpl1::new(value, comparator);
            next.visit_doc_values(&mut visitor)?;

            if visitor.last_doc != -1 {
                return Ok(visitor.last_doc);
            }
        } else {
            loop {
                let cmp = comparator.compare(next.get_min_packed_value()?.as_ref(), 0, value, 0);
                if cmp > 0 {
                    // This node doesn't have `value`, so next nodes can't either
                    break;
                }

                stack.push(next.try_clone()?);

                if !next.move_to_sibling()? {
                    break;
                }
            }
        }
    }

    Ok(-1)
}

struct IntersectVisitorImpl1<'a> {
    value: &'a [u8],
    comparator: &'a ByteArrayComparatorEnum,
    last_doc: i32,
}
impl<'a> IntersectVisitorImpl1<'a> {
    pub fn new(value: &'a [u8], comparator: &'a ByteArrayComparatorEnum) -> Self {
        Self {
            value,
            comparator,
            last_doc: -1,
        }
    }
}
impl<'a> IntersectVisitor for IntersectVisitorImpl1<'a> {
    fn visit(&mut self, _doc_id: i32) -> Result<()> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
        let cmp = self.comparator.compare(self.value, 0, packed_value, 0);
        if cmp == 0 {
            self.last_doc = doc_id;
        }
        Ok(())
    }

    fn compare(&self, _min_packed_value: &[u8], _max_packed_value: &[u8]) -> Result<Relation> {
        Ok(Relation::CellCrossesQuery)
    }
}

struct IntersectVisitorImpl<'a> {
    vd: &'a mut ValueAndDoc,
    comparator: &'a ByteArrayComparatorEnum,
    value: &'a [u8],
    last_doc: bool,
    allow_equal: bool,
}
impl<'a> IntersectVisitorImpl<'a> {
    pub fn new(
        vd: &'a mut ValueAndDoc,
        comparator: &'a ByteArrayComparatorEnum,
        value: &'a [u8],
        last_doc: bool,
        allow_equal: bool,
    ) -> Self {
        Self {
            vd,
            comparator,
            value,
            last_doc,
            allow_equal,
        }
    }
}
impl<'a> IntersectVisitor for IntersectVisitorImpl<'a> {
    fn visit(&mut self, _doc_id: i32) -> Result<()> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
        match self.vd.value {
            Some(ref value) if self.last_doc && !self.vd.done => {
                let cmp = self.comparator.compare(packed_value, 0, value, 0);
                debug_assert!(cmp >= 0);
                if cmp > 0 {
                    self.vd.done = true;
                } else {
                    self.vd.doc_id = doc_id;
                }
            },
            None => {
                let cmp = self.comparator.compare(packed_value, 0, self.value, 0);

                if cmp > 0 || (cmp == 0 && self.allow_equal) {
                    self.vd.value = Some(packed_value.to_vec());
                    self.vd.doc_id = doc_id;
                }
            },
            _ => {},
        }
        Ok(())
    }

    fn compare(&self, _min_packed_value: &[u8], _max_packed_value: &[u8]) -> Result<Relation> {
        Ok(Relation::CellCrossesQuery)
    }
}
pub struct BoundedDocIdSetIterator<D>
where
    D: DocIdSetIterator,
{
    first_doc: i32,
    last_doc: i32,
    delegate: D,
    doc_id: i32,
}

impl<D> BoundedDocIdSetIterator<D>
where
    D: DocIdSetIterator,
{
    fn new(first_doc: i32, last_doc: i32, delegate: D) -> Self {
        Self {
            first_doc,
            last_doc,
            delegate,
            doc_id: -1,
        }
    }
}

impl<D> DocIdSetIterator for BoundedDocIdSetIterator<D>
where
    D: DocIdSetIterator,
{
    fn doc_id(&self) -> i32 {
        self.doc_id
    }

    fn next_doc(&mut self) -> Result<i32> {
        self.advance(self.doc_id + 1)
    }

    fn advance(&mut self, mut target: i32) -> Result<i32> {
        if target < self.first_doc {
            target = self.first_doc;
        }

        let result = self.delegate.advance(target)?;

        if result < self.last_doc {
            self.doc_id = result;
        } else {
            self.doc_id = NO_MORE_DOCS;
        }

        Ok(self.doc_id)
    }

    fn cost(&self) -> Result<i64> {
        let delegate_cost = self.delegate.cost()?;
        let bound_cost = (self.last_doc - self.first_doc) as i64;
        Ok(delegate_cost.min(bound_cost))
    }
}

fn get_doc_id_set_iterator<LR>(
    sort_field: &SortFieldEnum,
    context: &LeafReaderContext<LR>,
    delegate: Disi<LR>,
    lower_value: i64,
    upper_value: i64,
    field: &str,
) -> Result<IteratorAndCount<Disi<LR>>>
where
    LR: LeafReader,
{
    let lower = if sort_field.get_reverse() {
        upper_value
    } else {
        lower_value
    };
    let upper = if sort_field.get_reverse() {
        lower_value
    } else {
        upper_value
    };

    let reader = context.reader();
    let max_doc = reader.max_doc()?;
    // Perform a binary search to find the first document with value >= lower.
    let mut comparator = load_comparator(sort_field, lower, context)?;
    let mut low: i32 = 0;
    let mut high: i32 = max_doc - 1;

    while low <= high {
        let mid = (low + high) >> 1;
        if comparator.compare(mid)? <= 0 {
            high = mid - 1;
            comparator = load_comparator(sort_field, lower, context)?;
        } else {
            low = mid + 1;
        }
    }

    let first_doc_id_inclusive = high + 1;
    // Perform a binary search to find the first document with value > upper.
    // Since we know that upper >= lower, we can initialize the lower bound
    // of the binary search to the result of the previous search.
    let mut comparator = load_comparator(sort_field, upper, context)?;
    low = first_doc_id_inclusive;
    high = max_doc - 1;

    while low <= high {
        let mid = (low + high) >> 1;

        if comparator.compare(mid)? < 0 {
            high = mid - 1;
            comparator = load_comparator(sort_field, upper, context)?;
        } else {
            low = mid + 1;
        }
    }

    let last_doc_id_exclusive = high + 1;

    if first_doc_id_inclusive == last_doc_id_exclusive {
        return Ok(IteratorAndCount::empty());
    }

    let missing_value = sort_field.get_missing_value();
    let missing_long_value = match missing_value {
        Some(MissingValueEnum::Long(mv)) => *mv,
        Some(MissingValueEnum::Int(mv)) => mv.to_owned() as i64,
        Some(_) => {
            return Err(LuceneError::illegal_argument(
                "Missing value for SortedNumericSortField must be Long/Int",
            ));
        },
        None => 0i64,
    };

    let point_values = reader.get_point_values(field)?;
    // all documents have docValues or missing value falls outside the range
    let all_docs_have_values = match point_values {
        Some(point_values) => point_values.get_doc_count()? == reader.max_doc()?,
        _ => false,
    };

    if all_docs_have_values
        || (missing_long_value < lower_value || missing_long_value > upper_value)
    {
        return IteratorAndCount::dense_range(first_doc_id_inclusive, last_doc_id_exclusive);
    }

    Ok(IteratorAndCount::sparse_range(
        first_doc_id_inclusive,
        last_doc_id_exclusive,
        delegate,
    ))
}
fn get_doc_id_set_iterator_or_null<LR>(
    context: &LeafReaderContext<LR>,
    lower_value: i64,
    upper_value: i64,
    field: &str,
) -> Result<Option<IteratorAndCount<Disi<LR>>>>
where
    LR: LeafReader,
{
    if lower_value > upper_value {
        return Ok(Some(IteratorAndCount::empty()));
    }

    let mut sorted_numeric_values = DocValues::get_sorted_numeric(context.reader(), field)?;

    if !sorted_numeric_values.is_single_valued() {
        return Ok(None);
    }
    let numeric_values = DocValues::unwrap_singleton_numeric(&mut sorted_numeric_values)?;
    let (it_and_count_opt, disi_opt) = get_doc_id_set_iterator_or_null_from_bkd(
        context,
        numeric_values,
        lower_value,
        upper_value,
        field,
    )?;
    match (it_and_count_opt, disi_opt) {
        (Some(itc), None) => Ok(Some(itc)),

        (None, Some(numeric_values)) => {
            let meta = context.reader().get_metadata()?;
            if let Some(index_sort) = meta.get_sort() {
                let sort_fields = index_sort.get_sort();

                if !sort_fields.is_empty() && sort_fields[0].get_field() == Some(field) {
                    let sort_field = &sort_fields[0];
                    let sort_field_type = get_sort_field_type(sort_field);

                    // Only INT and LONG supported
                    if sort_field_type == SortFieldType::Int
                        || sort_field_type == SortFieldType::Long
                    {
                        let it_and_count = get_doc_id_set_iterator(
                            sort_field,
                            context,
                            numeric_values,
                            lower_value,
                            upper_value,
                            field,
                        )?;
                        return Ok(Some(it_and_count));
                    }
                }
            }
            Ok(None)
        },
        _ => Err(LuceneError::illegal_state("should not be here")),
    }
}
fn match_none<P>(points: &P, query_lower_point: &[u8], query_upper_point: &[u8]) -> Result<bool>
where
    P: PointValues,
{
    debug_assert!(points.get_num_dimensions()? == 1);
    let comparator = ArrayUtil::get_unsigned_comparator(points.get_bytes_per_dimension()?);
    match points.get_min_packed_value()? {
        None => {
            return Err(LuceneError::illegal_state(
                "point values has no min packed value",
            ));
        },
        Some(v) => {
            let min_cmp = comparator.compare(v.as_ref(), 0, query_upper_point, 0);
            if min_cmp > 0 {
                return Ok(true);
            }
        },
    }
    match points.get_max_packed_value()? {
        None => {
            return Err(LuceneError::illegal_state(
                "point values has no max packed value",
            ));
        },
        Some(v) => {
            let max_cmp = comparator.compare(v.as_ref(), 0, query_lower_point, 0);
            if max_cmp < 0 {
                return Ok(true);
            }
        },
    }

    Ok(false)
}
fn match_all<P>(points: &P, query_lower_point: &[u8], query_upper_point: &[u8]) -> Result<bool>
where
    P: PointValues,
{
    debug_assert!(points.get_num_dimensions()? == 1);

    let comparator = ArrayUtil::get_unsigned_comparator(points.get_bytes_per_dimension()?);
    let min = points
        .get_min_packed_value()?
        .ok_or_else(|| LuceneError::illegal_state("point values has no min packed value"))?;
    let min_cmp = comparator.compare(min.as_ref(), 0, query_lower_point, 0);

    if min_cmp < 0 {
        return Ok(false);
    }
    let max = points
        .get_max_packed_value()?
        .ok_or_else(|| LuceneError::illegal_state("point values has no max packed value"))?;

    let max_cmp = comparator.compare(max.as_ref(), 0, query_upper_point, 0);
    Ok(max_cmp <= 0)
}
#[allow(clippy::type_complexity)]
fn get_doc_id_set_iterator_or_null_from_bkd<LR>(
    context: &LeafReaderContext<LR>,
    delegate: Disi<LR>,
    lower_value: i64,
    upper_value: i64,
    field: &str,
) -> Result<(Option<IteratorAndCount<Disi<LR>>>, Option<Disi<LR>>)>
where
    LR: LeafReader,
{
    let index_sort = context.reader().get_metadata()?.get_sort();
    if index_sort.is_none()
        || index_sort.as_ref().unwrap().get_sort().is_empty()
        || index_sort.as_ref().unwrap().get_sort()[0].get_field() != Some(field)
    {
        return Ok((None, Some(delegate)));
    }

    let points = context.reader().get_point_values(field)?;
    let points = match points {
        Some(p) => p,
        None => return Ok((None, Some(delegate))),
    };

    if points.get_num_dimensions()? != 1 {
        return Ok((None, Some(delegate)));
    }

    let bpd = points.get_bytes_per_dimension()?;
    if bpd != BitUtil::INT_BYTES && bpd != BitUtil::LONG_BYTES {
        return Ok((None, Some(delegate)));
    }

    if points.size()? != points.get_doc_count()?.try_convert()? {
        return Ok((None, Some(delegate)));
    }

    debug_assert!(lower_value <= upper_value);

    let (query_lower_point, query_upper_point) = if bpd == BitUtil::INT_BYTES {
        (
            IntPoint::pack([lower_value as i32])?,
            IntPoint::pack([upper_value as i32])?,
        )
    } else {
        (
            LongPoint::pack([lower_value])?,
            LongPoint::pack([upper_value])?,
        )
    };

    if match_none(&points, &query_lower_point.bytes, &query_upper_point.bytes)? {
        return Ok((Some(IteratorAndCount::empty()), None));
    }

    if match_all(&points, &query_lower_point.bytes, &query_upper_point.bytes)? {
        let max_doc = context.reader().max_doc()?;

        if points.get_doc_count()? == max_doc {
            return Ok((Some(IteratorAndCount::all(max_doc)), None));
        } else {
            return Ok((
                Some(IteratorAndCount::sparse_range(0, max_doc, delegate)),
                None,
            ));
        }
    }

    let reverse = index_sort.as_ref().unwrap().get_sort()[0].get_reverse();
    let comparator = ArrayUtil::get_unsigned_comparator(bpd);

    let min_doc_id;
    let mut max_doc_id;

    if reverse {
        min_doc_id = next_doc(
            &mut points.get_point_tree()?,
            &query_upper_point.bytes,
            false,
            &comparator,
            true,
        )? + 1;
    } else {
        min_doc_id = next_doc(
            &mut points.get_point_tree()?,
            &query_lower_point.bytes,
            true,
            &comparator,
            false,
        )?;
        if min_doc_id == -1 {
            return Ok((Some(IteratorAndCount::empty()), None));
        }
    }

    if reverse {
        max_doc_id = next_doc(
            &mut points.get_point_tree()?,
            &query_lower_point.bytes,
            true,
            &comparator,
            true,
        )? + 1;

        if max_doc_id == 0 {
            return Ok((Some(IteratorAndCount::empty()), None));
        }
    } else {
        max_doc_id = next_doc(
            &mut points.get_point_tree()?,
            &query_upper_point.bytes,
            false,
            &comparator,
            false,
        )?;

        if max_doc_id == -1 {
            max_doc_id = context.reader().max_doc()?;
        }
    }

    if min_doc_id == max_doc_id {
        return Ok((Some(IteratorAndCount::empty()), None));
    }

    if points.get_doc_count()? == context.reader().max_doc()? {
        Ok((
            Some(IteratorAndCount::dense_range(min_doc_id, max_doc_id)?),
            None,
        ))
    } else {
        Ok((
            Some(IteratorAndCount::sparse_range(
                min_doc_id, max_doc_id, delegate,
            )),
            None,
        ))
    }
}
trait ValueComparator {
    fn compare(&mut self, doc_id: i32) -> Result<i32>;
}
struct ValueComparatorImpl<LR>
where
    LR: LeafReader,
{
    field_comparator: FieldComparatorEnum,
    leaf_field_comparator: LeafFieldComparatorEnum<LR>,
    direction: i32,
}
impl<LR> ValueComparatorImpl<LR>
where
    LR: LeafReader,
{
    pub fn new(
        mut field_comparator: FieldComparatorEnum,
        direction: i32,
        context: &LeafReaderContext<LR>,
    ) -> Result<Self> {
        let leaf_field_comparator = field_comparator.get_leaf_comparator(context)?;
        Ok(Self {
            field_comparator,
            leaf_field_comparator,
            direction,
        })
    }
}
impl<LR> ValueComparator for ValueComparatorImpl<LR>
where
    LR: LeafReader,
{
    fn compare(&mut self, doc_id: i32) -> Result<i32> {
        let mut v = DummyScorer;
        let value =
            self.leaf_field_comparator
                .compare_top(doc_id, &mut v, &mut self.field_comparator)?;
        Ok(self.direction * value)
    }
}
fn load_comparator<LR>(
    sort_field: &SortFieldEnum,
    top_value: i64,
    context: &LeafReaderContext<LR>,
) -> Result<ValueComparatorImpl<LR>>
where
    LR: LeafReader,
{
    let mut field_comparator = sort_field.get_comparator(1, Pruning::None)?;
    match field_comparator {
        FieldComparatorEnum::Long(ref mut fc) => {
            fc.set_top_value(top_value);
        },
        FieldComparatorEnum::SortedNumericLong(ref mut fc) => {
            fc.set_top_value(top_value);
        },
        FieldComparatorEnum::Int(ref mut fc) => {
            fc.set_top_value(top_value as i32);
        },
        FieldComparatorEnum::SortedNumericInt(ref mut fc) => {
            fc.set_top_value(top_value as i32);
        },
        _ => {
            return Err(LuceneError::illegal_argument(
                "Expected Long or Int FieldComparator",
            ));
        },
    }

    let direction = if sort_field.get_reverse() { -1 } else { 1 };

    ValueComparatorImpl::new(field_comparator, direction, context)
}

fn get_sort_field_type(sort_field: &SortFieldEnum) -> SortFieldType {
    // We expect the sortField to be SortedNumericSortField
    match sort_field {
        SortFieldEnum::SortedNumeric(sf) => sf.get_numeric_type(),
        _ => sort_field.get_type(),
    }
}

struct IteratorAndCount<D>
where
    D: DocIdSetIterator,
{
    it: IteratorAndCountDisi<D>,
    count: i32,
}

impl<D> IteratorAndCount<D>
where
    D: DocIdSetIterator,
{
    fn new(it: IteratorAndCountDisi<D>, count: i32) -> Self {
        Self { it, count }
    }

    fn empty() -> Self {
        IteratorAndCount::new(DocIdSetIteratorEnum4::A(EmptyDISI::default()), 0)
    }

    fn all(max_doc: i32) -> Self {
        IteratorAndCount::new(DocIdSetIteratorEnum4::B(AllDISI::new(max_doc)), max_doc)
    }

    fn dense_range(min_doc: i32, max_doc: i32) -> Result<Self> {
        Ok(IteratorAndCount::new(
            DocIdSetIteratorEnum4::C(RangeDISI::new(min_doc, max_doc)?),
            max_doc - min_doc,
        ))
    }

    fn sparse_range(min_doc: i32, max_doc: i32, delegate: D) -> IteratorAndCount<D> {
        let v = BoundedDocIdSetIterator::new(min_doc, max_doc, delegate);
        IteratorAndCount::new(DocIdSetIteratorEnum4::D(v), -1)
    }
}

pub type IteratorAndCountDisi<D> =
    DocIdSetIteratorEnum4<EmptyDISI, AllDISI, RangeDISI, BoundedDocIdSetIterator<D>>;
// for std::mem::take
impl<D> Default for IteratorAndCountDisi<D>
where
    D: DocIdSetIterator,
{
    fn default() -> Self {
        DocIdSetIteratorEnum4::A(EmptyDISI::default())
    }
}
#[derive(Debug, Hash, Eq, PartialEq, Clone)]
pub enum FallbackQuery {
    PointRange(PointRangeQuery),
    SortedNumericDocValuesSet(SortedNumericDocValuesSetQuery),
    SortedNumericDocValuesRange(SortedNumericDocValuesRangeQuery),
    SortedSetDocValuesRange(SortedSetDocValuesRangeQuery),
}
impl From<PointRangeQuery> for FallbackQuery {
    fn from(value: PointRangeQuery) -> Self {
        FallbackQuery::PointRange(value)
    }
}
impl From<SortedNumericDocValuesSetQuery> for FallbackQuery {
    fn from(value: SortedNumericDocValuesSetQuery) -> Self {
        FallbackQuery::SortedNumericDocValuesSet(value)
    }
}
impl From<SortedNumericDocValuesRangeQuery> for FallbackQuery {
    fn from(value: SortedNumericDocValuesRangeQuery) -> Self {
        FallbackQuery::SortedNumericDocValuesRange(value)
    }
}
impl From<SortedSetDocValuesRangeQuery> for FallbackQuery {
    fn from(value: SortedSetDocValuesRangeQuery) -> Self {
        FallbackQuery::SortedSetDocValuesRange(value)
    }
}
pub type FallbackQueryWeight<LR> = WeightEnum4<
    PointRangeWeight<LR>,
    SortedNumericDocValuesSetQueryWeight<LR>,
    SortedNumericDocValuesRangeQueryWeight<LR>,
    SortedSetDocValuesRangeQueryWeight<LR>,
>;

#[cfg(test)]
mod tests {
    use crate::core::document::document::Document;
    use crate::core::document::field::Store;
    use crate::core::document::long_point::LongPoint;
    use crate::core::document::sorted_numeric_doc_values_field::{
        SortedNumericDocValuesField, sorted_numeric_doc_values_field_util,
    };
    use crate::core::document::string_field::StringField;
    use crate::core::index::index_reader::IndexReader;
    use crate::core::index::index_reader_context::IndexReaderContext;
    use crate::core::index::index_writer_config::IndexWriterConfig;
    use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
    use crate::core::index::query_timeout::QueryTimeout;
    use crate::core::search::QueryCache;
    use crate::core::search::index_searcher::IndexSearcher;
    use crate::core::search::index_sort_sorted_numeric_doc_values_range_query::IndexSortSortedNumericDocValuesRangeQuery;
    use crate::core::search::query::{Query, QueryBase};
    use crate::core::search::query_caching_policy::QueryCachingPolicy;
    use crate::core::search::score_doc::ScoreDocLike;
    use crate::core::search::score_mode::ScoreMode;
    use crate::core::search::similarities_impl::similarities::Similarity;
    use crate::core::search::sort::Sort;
    use crate::core::search::sort_field::{SortFieldType, SortFiledBase};
    use crate::core::search::sorted_numeric_sort_field::SortedNumericSortField;
    use crate::core::search::top_docs::TopDocsLike;
    use crate::core::store::directory::Directory;
    use crate::core::util::error::lucene_error::Result;
    use crate::test::index::random_index_writer::RandomIndexWriter;
    use crate::test::search::query_utils::QueryUtils;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{
        at_least, new_directory_shared, new_searcher_with_reader, random,
    };
    use crate::test::util::test_util::TestUtil;
    use rand::Rng;
    use std::sync::Arc;

    use crate::core::search::scorer::{Scorer, TwoPhaseState};
    use crate::core::search::weight::Weight;
    use crate::core::util::TryIntoInt;
    use crate::test::search::dummy_total_hit_count_collector::DummyTotalHitCountCollector;

    #[allow(dead_code)] // for quick search
    struct TestIndexSortSortedNumericDocValuesRangeQuery;
    #[test]
    fn test_same_hits_as_point_range_query() -> Result<()> {
        let mut random = random();
        let iters = at_least(&mut random, 10);

        for _iter in 0..iters {
            let dir = new_directory_shared(&mut random)?;
            // TODO: 未实现MockAnalyzer
            let mut iwc = IndexWriterConfig::new();

            let reverse = random.random_bool(0.5);
            let mut sort_field =
                SortedNumericSortField::with_reverse("dv", SortFieldType::Long, reverse)?;

            let enable_missing_value = random.random_bool(0.5);
            if enable_missing_value {
                let missing_value = if random.random_bool(0.5) {
                    TestUtil::next_long(&mut random, -100, 10000)
                } else if random.random_bool(0.5) {
                    i64::MIN
                } else {
                    i64::MAX
                };
                sort_field.set_missing_value(missing_value)?;
            }

            let sort = Sort::with_fields(vec![sort_field])?;
            iwc.set_index_sort(sort)?;

            let iw = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

            let num_docs = at_least(&mut random, 100);
            for _i in 0..num_docs {
                let mut doc = Document::new();
                let num_values = TestUtil::next_int(&mut random, 0, 1);

                for _ in 0..num_values {
                    let value = TestUtil::next_long(&mut random, -100, 10000);
                    doc.add(SortedNumericDocValuesField::new("dv", value));
                    doc.add(LongPoint::new("idx", vec![value])?);
                }

                iw.add_document(doc)?;
            }

            // TODO delete by query 未实现
            // Optional delete
            // if random.random_bool(0.5) {
            //     iw.delete_documents_query(LongPoint::new_range_query("idx", vec![0], vec![10])?)?;
            // }

            let reader = iw.get_reader()?;
            let searcher = new_searcher_with_reader(reader)?;
            iw.close()?;

            for _i in 0..100 {
                let min = if random.random_bool(0.5) {
                    i64::MIN
                } else {
                    TestUtil::next_long(&mut random, -100, 10000)
                };
                let max = if random.random_bool(0.5) {
                    i64::MAX
                } else {
                    TestUtil::next_long(&mut random, -100, 10000)
                };

                let q1 = LongPoint::new_range_query("idx", vec![min], vec![max])?;
                let q2 = create_query("dv", min, max);

                assert_same_hits(&searcher, q1, q2, false)?;
            }
        }

        Ok(())
    }

    fn assert_same_hits<S, IRC, QT, QCP, QC, T1, T2>(
        searcher: &IndexSearcher<IRC, S, QT, QCP, QC>,
        q1: T1,
        q2: T2,
        scores: bool,
    ) -> Result<()>
    where
        IRC: IndexReaderContext,
        S: Similarity,
        QT: QueryTimeout,
        QCP: QueryCachingPolicy,
        QC: QueryCache,
        T1: Into<Query>,
        T2: Into<Query>,
    {
        let irc = searcher.get_top_reader_context();
        let max_doc = irc.reader().max_doc()?;

        let sort = if scores {
            Arc::new(Sort::get_relevance()?)
        } else {
            Arc::new(Sort::get_index_order()?)
        };

        let td1 = searcher.search_with_sort(q1, max_doc.try_convert()?, sort.clone())?;
        let td2 = searcher.search_with_sort(q2, max_doc.try_convert()?, sort)?;
        assert_eq!(td1.total_hits().value(), td2.total_hits().value());

        for i in 0..td1.score_docs().len() {
            let sd1 = &td1.score_docs()[i];
            let sd2 = &td2.score_docs()[i];

            assert_eq!(sd1.doc(), sd2.doc());

            if scores {
                let diff = (sd1.score() - sd2.score()).abs();
                assert!(diff <= 1e-6, "score diff={} idx={}", diff, i);
            }
        }

        Ok(())
    }
    #[test]
    fn test_equals() -> Result<()> {
        let q1 = create_query("foo", 3, 5);

        QueryUtils::check_equal(&q1, &create_query("foo", 3, 5));
        QueryUtils::check_unequal(&q1, &create_query("foo", 3, 6));
        QueryUtils::check_unequal(&q1, &create_query("foo", 4, 5));
        QueryUtils::check_unequal(&q1, &create_query("bar", 3, 5));

        Ok(())
    }

    #[test]
    fn test_to_string() -> Result<()> {
        let q1 = create_query("foo", 3, 5);

        assert_eq!("foo:[3 TO 5]", q1.as_string(""));
        assert_eq!("[3 TO 5]", q1.as_string("foo"));
        assert_eq!("foo:[3 TO 5]", q1.as_string("bar"));

        Ok(())
    }
    #[test]
    fn test_index_sort_doc_values_with_even_length() -> Result<()> {
        use SortFieldType::*;

        for ty in [Int, Long] {
            test_index_sort_doc_values_with_even_length_inner(true, ty)?;
            test_index_sort_doc_values_with_even_length_inner(false, ty)?;
        }
        Ok(())
    }
    fn test_index_sort_doc_values_with_even_length_inner(
        reverse: bool,
        field_type: SortFieldType,
    ) -> Result<()> {
        let mut random = random();
        // TODO 未实现MockAnalyzer
        let dir = new_directory_shared(&mut random)?;
        let mut iwc = IndexWriterConfig::new();
        let sort_field = SortedNumericSortField::with_reverse("field", field_type, reverse)?;
        iwc.set_index_sort(Sort::with_fields(vec![sort_field])?)?;

        let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

        // even-length doc list = 6 docs
        writer.add_document(create_document("field", -80))?;
        writer.add_document(create_document("field", -5))?;
        writer.add_document(create_document("field", 0))?;
        writer.add_document(create_document("field", 0))?;
        writer.add_document(create_document("field", 30))?;
        writer.add_document(create_document("field", 35))?;

        let reader = writer.get_reader()?;
        let searcher = new_searcher_with_reader(reader)?;

        // Test ranges consisting of one value.

        assert_number_of_hits(&searcher, create_query("field", -80, -80), 1)?;
        assert_number_of_hits(&searcher, create_query("field", -5, -5), 1)?;
        assert_number_of_hits(&searcher, create_query("field", 0, 0), 2)?;
        assert_number_of_hits(&searcher, create_query("field", 30, 30), 1)?;
        assert_number_of_hits(&searcher, create_query("field", 35, 35), 1)?;

        assert_number_of_hits(&searcher, create_query("field", -90, -90), 0)?;
        assert_number_of_hits(&searcher, create_query("field", 5, 5), 0)?;
        assert_number_of_hits(&searcher, create_query("field", 40, 40), 0)?;

        // Test the lower end of the document value range.
        assert_number_of_hits(&searcher, create_query("field", -90, -4), 2)?;
        assert_number_of_hits(&searcher, create_query("field", -80, -4), 2)?;
        assert_number_of_hits(&searcher, create_query("field", -70, -4), 1)?;
        assert_number_of_hits(&searcher, create_query("field", -80, -5), 2)?;

        // Test the upper end of the document value range.
        assert_number_of_hits(&searcher, create_query("field", 25, 34), 1)?;
        assert_number_of_hits(&searcher, create_query("field", 25, 35), 2)?;
        assert_number_of_hits(&searcher, create_query("field", 25, 36), 2)?;
        assert_number_of_hits(&searcher, create_query("field", 30, 35), 2)?;

        // Test multiple occurrences of the same value.
        assert_number_of_hits(&searcher, create_query("field", -4, 4), 2)?;
        assert_number_of_hits(&searcher, create_query("field", -4, 0), 2)?;
        assert_number_of_hits(&searcher, create_query("field", 0, 4), 2)?;
        assert_number_of_hits(&searcher, create_query("field", 0, 30), 3)?;

        // Test ranges that span all documents.
        assert_number_of_hits(&searcher, create_query("field", -80, 35), 6)?;
        assert_number_of_hits(&searcher, create_query("field", -90, 40), 6)?;

        writer.close()?;
        Ok(())
    }

    fn assert_number_of_hits<IRC, S, QT, QCP, QC>(
        searcher: &IndexSearcher<IRC, S, QT, QCP, QC>,
        query: impl Into<Query>,
        number_of_hits: i32,
    ) -> Result<()>
    where
        IRC: IndexReaderContext,
        S: Similarity,
        QT: QueryTimeout,
        QCP: QueryCachingPolicy,
        QC: QueryCache,
    {
        let query = query.into();

        let manager = DummyTotalHitCountCollector::create_manager();
        let total_hits = searcher.search_with_collector_manager(query.clone(), &manager)?;
        assert_eq!(number_of_hits, total_hits);

        let count = searcher.count(query)?;
        assert_eq!(number_of_hits, count);

        Ok(())
    }
    #[test]
    fn test_index_sort_doc_values_with_odd_length() -> Result<()> {
        test_index_sort_doc_values_with_odd_length_inner(false)?;
        test_index_sort_doc_values_with_odd_length_inner(true)?;
        Ok(())
    }
    fn test_index_sort_doc_values_with_odd_length_inner(reverse: bool) -> Result<()> {
        let mut random = random();
        // TODO 未实现MockAnalyzer
        let dir = new_directory_shared(&mut random)?;

        let mut iwc = IndexWriterConfig::new();
        let sort_field =
            SortedNumericSortField::with_reverse("field", SortFieldType::Long, reverse)?;
        iwc.set_index_sort(Sort::with_fields(vec![sort_field])?)?;

        let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

        writer.add_document(create_document("field", -80))?;
        writer.add_document(create_document("field", -5))?;
        writer.add_document(create_document("field", 0))?;
        writer.add_document(create_document("field", 0))?;
        writer.add_document(create_document("field", 5))?;
        writer.add_document(create_document("field", 30))?;
        writer.add_document(create_document("field", 35))?;

        let reader = writer.get_reader()?;
        let searcher = new_searcher_with_reader(reader)?;

        // Test ranges consisting of one value.
        assert_number_of_hits(&searcher, create_query("field", -80, -80), 1)?;
        assert_number_of_hits(&searcher, create_query("field", -5, -5), 1)?;
        assert_number_of_hits(&searcher, create_query("field", 0, 0), 2)?;
        assert_number_of_hits(&searcher, create_query("field", 5, 5), 1)?;
        assert_number_of_hits(&searcher, create_query("field", 30, 30), 1)?;
        assert_number_of_hits(&searcher, create_query("field", 35, 35), 1)?;

        assert_number_of_hits(&searcher, create_query("field", -90, -90), 0)?;
        assert_number_of_hits(&searcher, create_query("field", 6, 6), 0)?;
        assert_number_of_hits(&searcher, create_query("field", 40, 40), 0)?;

        // Test the lower end of the document value range.
        assert_number_of_hits(&searcher, create_query("field", -90, -4), 2)?;
        assert_number_of_hits(&searcher, create_query("field", -80, -4), 2)?;
        assert_number_of_hits(&searcher, create_query("field", -70, -4), 1)?;
        assert_number_of_hits(&searcher, create_query("field", -80, -5), 2)?;

        // Test the upper end of the document value range.
        assert_number_of_hits(&searcher, create_query("field", 25, 34), 1)?;
        assert_number_of_hits(&searcher, create_query("field", 25, 35), 2)?;
        assert_number_of_hits(&searcher, create_query("field", 25, 36), 2)?;
        assert_number_of_hits(&searcher, create_query("field", 30, 35), 2)?;

        // Test multiple occurrences of the same value.
        assert_number_of_hits(&searcher, create_query("field", -4, 4), 2)?;
        assert_number_of_hits(&searcher, create_query("field", -4, 0), 2)?;
        assert_number_of_hits(&searcher, create_query("field", 0, 4), 2)?;
        assert_number_of_hits(&searcher, create_query("field", 0, 30), 4)?;

        // Test ranges that span all documents.
        assert_number_of_hits(&searcher, create_query("field", -80, 35), 7)?;
        assert_number_of_hits(&searcher, create_query("field", -90, 40), 7)?;

        writer.close()?;
        Ok(())
    }

    #[test]
    fn test_index_sort_doc_values_with_single_value() -> Result<()> {
        test_index_sort_doc_values_with_single_value_inner(false)?;
        test_index_sort_doc_values_with_single_value_inner(true)?;
        Ok(())
    }

    fn test_index_sort_doc_values_with_single_value_inner(reverse: bool) -> Result<()> {
        let mut random = random();
        // TODO 未实现MockAnalyzer
        let dir = new_directory_shared(&mut random)?;

        let mut iwc = IndexWriterConfig::new();
        let sort_field =
            SortedNumericSortField::with_reverse("field", SortFieldType::Long, reverse)?;
        iwc.set_index_sort(Sort::with_fields(vec![sort_field])?)?;

        let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

        writer.add_document(create_document("field", 42))?;

        let reader = writer.get_reader()?;
        let searcher = new_searcher_with_reader(reader)?;

        assert_number_of_hits(&searcher, create_query("field", 42, 43), 1)?;
        assert_number_of_hits(&searcher, create_query("field", 42, 42), 1)?;
        assert_number_of_hits(&searcher, create_query("field", 41, 41), 0)?;
        assert_number_of_hits(&searcher, create_query("field", 43, 43), 0)?;

        writer.close()?;
        Ok(())
    }

    #[test]
    fn test_index_sort_missing_values() -> Result<()> {
        let mut random = random();
        // TODO 未实现MockAnalyzer
        let dir = new_directory_shared(&mut random)?;
        let mut iwc = IndexWriterConfig::new();
        let mut sort_field = SortedNumericSortField::new("field", SortFieldType::Long)?;
        let missing_value: i64 = random.random();
        sort_field.set_missing_value(missing_value)?;
        let sort = Sort::with_fields(vec![sort_field])?;
        iwc.set_index_sort(sort)?;

        let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

        writer.add_document(create_document("field", -80))?;
        writer.add_document(create_document("field", -5))?;
        writer.add_document(create_document("field", 0))?;
        writer.add_document(create_document("field", 35))?;

        writer.add_document(create_document("other-field", 0))?;
        writer.add_document(create_document("other-field", 10))?;
        writer.add_document(create_document("other-field", 20))?;

        let reader = writer.get_reader()?;
        let searcher = new_searcher_with_reader(reader)?;

        assert_number_of_hits(&searcher, create_query("field", -70, 0), 2)?;
        assert_number_of_hits(&searcher, create_query("field", -2, 35), 2)?;

        assert_number_of_hits(&searcher, create_query("field", -80, 35), 4)?;
        assert_number_of_hits(&searcher, create_query("field", i64::MIN, i64::MAX), 4)?;

        writer.close()?;
        Ok(())
    }

    #[test]
    fn test_no_documents() -> Result<()> {
        let mut random = random();

        let dir = new_directory_shared(&mut random)?;

        let writer = RandomIndexWriter::new(&mut random, dir.clone());
        writer.add_document(Document::new())?;

        let reader = writer.get_reader()?;
        let searcher = new_searcher_with_reader(reader)?;

        let query = create_query("foo", 2, 4);

        // TODO query rewrite 未实现
        // let rewritten = searcher.rewrite(&query)?;
        let weight = searcher.create_weight(query, ScoreMode::Complete, 1.0, None)?;

        let leaves = searcher.get_leaf_contexts()?;
        let ctx0 = &leaves[0];

        let scorer_opt = weight.scorer(ctx0)?;
        assert!(scorer_opt.is_none());

        writer.close()?;
        Ok(())
    }

    #[test]
    fn test_rewrite_exhaustive_range() -> Result<()> {
        // TODO query rewrite 未实现
        Ok(())
    }
    #[test]
    fn test_rewrite_fallback_query() -> Result<()> {
        // TODO query rewrite 未实现
        Ok(())
    }
    /// Test that the index sort optimization not activated if there is no index sort.
    #[test]
    fn test_no_index_sort() -> Result<()> {
        let mut random = random();
        // TODO 未实现MockAnalyzer
        let dir = new_directory_shared(&mut random)?;
        let writer = RandomIndexWriter::new(&mut random, dir.clone());
        writer.add_document(create_document("field", 0))?;
        test_index_sort_optimization_deactivated(&writer)?;
        writer.close()?;
        Ok(())
    }

    /// Test that the index sort optimization is not activated when the sort is on the wrong field.
    #[test]
    fn test_index_sort_on_wrong_field() -> Result<()> {
        let mut random = random();
        // TODO 未实现MockAnalyzer
        let dir = new_directory_shared(&mut random)?;
        let mut iwc = IndexWriterConfig::new();
        let sort_field = SortedNumericSortField::new("other-field", SortFieldType::Long)?;
        let sort = Sort::with_fields(vec![sort_field])?;
        iwc.set_index_sort(sort)?;
        let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);
        writer.add_document(create_document("field", 0))?;
        test_index_sort_optimization_deactivated(&writer)?;
        writer.close()?;
        Ok(())
    }

    #[test]
    fn test_other_sort_types() -> Result<()> {
        use SortFieldType::{Double, Float};
        let mut random = random();
        for sort_type in [Float, Double] {
            // TODO 未实现MockAnalyzer
            let dir = new_directory_shared(&mut random)?;
            let mut iwc = IndexWriterConfig::new();
            let sort_field = SortedNumericSortField::new("field", sort_type)?;
            let sort = Sort::with_fields(vec![sort_field])?;
            iwc.set_index_sort(sort)?;
            let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);
            writer.add_document(create_document("field", 0))?;
            test_index_sort_optimization_deactivated(&writer)?;
            writer.close()?;
        }

        Ok(())
    }

    /// Test that the index sort optimization is not activated when some documents have multiple values.
    #[test]
    fn test_multi_doc_values() -> Result<()> {
        let mut random = random();
        // TODO 未实现MockAnalyzer
        let dir = new_directory_shared(&mut random)?;
        let mut iwc = IndexWriterConfig::new();
        let sort_field = SortedNumericSortField::new("field", SortFieldType::Long)?;
        let sort = Sort::with_fields(vec![sort_field])?;
        iwc.set_index_sort(sort)?;
        let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);
        let mut doc = Document::new();
        doc.add(SortedNumericDocValuesField::new("field", 0));
        doc.add(SortedNumericDocValuesField::new("field", 10));
        writer.add_document(doc)?;

        test_index_sort_optimization_deactivated(&writer)?;

        writer.close()?;
        Ok(())
    }

    fn test_index_sort_optimization_deactivated<D>(writer: &RandomIndexWriter<D>) -> Result<()>
    where
        D: Directory,
    {
        let reader = writer.get_reader()?;
        let searcher = new_searcher_with_reader(reader)?;
        let query = create_query("field", 0, 0);
        let weight = query.create_weight(&searcher, &ScoreMode::TopScores, 1.0, None)?;
        for ctx in searcher.get_leaf_contexts()? {
            let mut scorer = weight.scorer(ctx)?;
            assert!(
                scorer.as_mut().unwrap().has_two_phase_iterator() == TwoPhaseState::Yes
                    || scorer.as_mut().unwrap().two_phase_iterator()?.is_some()
            );
        }
        Ok(())
    }

    #[test]
    fn test_fallback_count() -> Result<()> {
        // this test is not required in Rust Lucene
        Ok(())
    }

    #[test]
    fn test_compare_count() -> Result<()> {
        let mut random = random();
        let iters = at_least(&mut random, 10);

        for _iter in 0..iters {
            let dir = new_directory_shared(&mut random)?;
            // TODO 未实现MockAnalyzer
            let mut iwc = IndexWriterConfig::new();
            let mut sort_field =
                SortedNumericSortField::with_reverse("field", SortFieldType::Long, false)?;
            let enable_missing_value = random.random_bool(0.5);
            if enable_missing_value {
                let missing_value = if random.random_bool(0.5) {
                    TestUtil::next_long(&mut random, -100, 10000)
                } else if random.random_bool(0.5) {
                    i64::MIN
                } else {
                    i64::MAX
                };
                sort_field.set_missing_value(missing_value)?;
            }

            let sort = Sort::with_fields(vec![sort_field])?;
            iwc.set_index_sort(sort)?;

            let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

            let num_docs = at_least(&mut random, 100);
            for _i in 0..num_docs {
                let mut doc = Document::new();
                let num_values = TestUtil::next_int(&mut random, 0, 1);

                for _ in 0..num_values {
                    let value = TestUtil::next_long(&mut random, -100, 10000);
                    doc = create_sndv_and_point_document("field", value)?;
                }

                writer.add_document(doc)?;
            }

            // TODO delete by query 未实现
            // Optional delete
            // if random.random_bool(0.5) {
            //     writer.delete_documents_query(
            //         LongPoint::new_range_query("field", vec![0], vec![10])?
            //     )?;
            // }

            // Reader + Searcher
            let reader = writer.get_reader()?;
            let searcher = new_searcher_with_reader(reader)?;
            writer.close()?;

            for _i in 0..100 {
                let min = if random.random_bool(0.5) {
                    i64::MIN
                } else {
                    TestUtil::next_long(&mut random, -100, 10000)
                };

                let max = if random.random_bool(0.5) {
                    i64::MAX
                } else {
                    TestUtil::next_long(&mut random, -100, 10000)
                };

                let q1 = LongPoint::new_range_query("field", vec![min], vec![max])?;

                let fallback = LongPoint::new_range_query("field", vec![min], vec![max])?;
                let q2 =
                    IndexSortSortedNumericDocValuesRangeQuery::new("field", min, max, fallback);

                let w1 = q1.create_weight(&searcher, &ScoreMode::Complete, 1.0, None)?;
                let w2 = q2.create_weight(&searcher, &ScoreMode::Complete, 1.0, None)?;

                assert_same_count(&w1, &w2, &searcher)?;
            }
        }

        Ok(())
    }

    fn assert_same_count<IRC, S, QT, QCP, QC>(
        weight1: &impl Weight<IRC::LeafReader>,
        weight2: &impl Weight<IRC::LeafReader>,
        searcher: &IndexSearcher<IRC, S, QT, QCP, QC>,
    ) -> Result<()>
    where
        IRC: IndexReaderContext,
        S: Similarity,
        QT: QueryTimeout,
        QCP: QueryCachingPolicy,
        QC: QueryCache,
    {
        for ctx in searcher.get_leaf_contexts()? {
            let c1 = weight1.count(ctx)?;
            let c2 = weight2.count(ctx)?;
            assert_eq!(c1, c2);
        }
        Ok(())
    }

    #[test]
    fn test_count_boundary() -> Result<()> {
        let mut random = random();
        // TODO 未实现MockAnalyzer
        let dir = new_directory_shared(&mut random)?;
        let mut iwc = IndexWriterConfig::new();

        let mut sort_field = SortedNumericSortField::new("field", SortFieldType::Long)?;

        let use_lower = random.random_bool(0.5);
        let lower_value = 1_i64;
        let upper_value = 100_i64;

        if use_lower {
            sort_field.set_missing_value(lower_value)?;
        } else {
            sort_field.set_missing_value(upper_value)?;
        }

        let sort = Sort::with_fields(vec![sort_field])?;
        iwc.set_index_sort(sort)?;

        let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

        writer.add_document(create_sndv_and_point_document(
            "field",
            random.random_range(lower_value..upper_value),
        )?)?;
        writer.add_document(create_sndv_and_point_document(
            "field",
            random.random_range(lower_value..upper_value),
        )?)?;
        writer.add_document(create_missing_value_document()?)?;

        let reader = writer.get_reader()?;
        let searcher = new_searcher_with_reader(reader)?;

        let fallback_query =
            LongPoint::new_range_query("field", vec![lower_value], vec![upper_value])?;

        let query = IndexSortSortedNumericDocValuesRangeQuery::new(
            "field",
            lower_value,
            upper_value,
            fallback_query,
        );

        let weight = query.create_weight(&searcher, &ScoreMode::Complete, 1.0, None)?;

        let mut count = 0;
        for ctx in searcher.get_leaf_contexts()? {
            count += weight.count(ctx)?;
        }

        assert_eq!(2, count);

        writer.close()?;
        Ok(())
    }

    fn create_missing_value_document() -> Result<Document> {
        let mut doc = Document::new();
        doc.add(StringField::with_string("foo", "fox", Store::Yes)?);
        Ok(doc)
    }

    fn create_sndv_and_point_document<S: Into<String>>(field: S, value: i64) -> Result<Document> {
        let field = field.into();
        let mut doc = Document::new();
        doc.add(SortedNumericDocValuesField::new(&field, value));
        doc.add(LongPoint::new(&field, vec![value])?);
        Ok(doc)
    }

    fn create_document<S: Into<String>>(field: S, value: i64) -> Document {
        let field = field.into();
        let mut doc = Document::new();
        doc.add(SortedNumericDocValuesField::new(&field, value));
        doc
    }

    fn create_query<S: Into<String>>(
        field: S,
        lower_value: i64,
        upper_value: i64,
    ) -> IndexSortSortedNumericDocValuesRangeQuery {
        let field_str = field.into();

        let fallback_query = sorted_numeric_doc_values_field_util::new_slow_range_query(
            field_str.clone(),
            lower_value,
            upper_value,
        );

        IndexSortSortedNumericDocValuesRangeQuery::new(
            field_str,
            lower_value,
            upper_value,
            fallback_query,
        )
    }
    #[test]
    fn test_count_with_bkd_asc() -> Result<()> {
        do_test_count_with_bkd(false)
    }

    #[test]
    fn test_count_with_bkd_desc() -> Result<()> {
        do_test_count_with_bkd(true)
    }

    fn do_test_count_with_bkd(reverse: bool) -> Result<()> {
        let mut random = random();
        let field_name = "field";

        // TODO 未实现MockAnalyzer
        let dir = new_directory_shared(&mut random)?;
        let mut iwc = IndexWriterConfig::new();

        let sort_field =
            SortedNumericSortField::with_reverse(field_name, SortFieldType::Long, reverse)?;
        let sort = Sort::with_fields(vec![sort_field])?;
        iwc.set_index_sort(sort)?;

        let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

        add_doc_with_bkd(&writer, field_name, 7, 500)?;
        add_doc_with_bkd(&writer, field_name, 5, 600)?;
        add_doc_with_bkd(&writer, field_name, 11, 700)?;
        add_doc_with_bkd(&writer, field_name, 13, 800)?;
        add_doc_with_bkd(&writer, field_name, 9, 900)?;

        writer.flush()?;
        // writer.force_merge(1)?; // TODO force_merge未实现

        let reader = writer.get_reader()?;
        let searcher = new_searcher_with_reader(reader)?;
        // Both bounds exist in the dataset
        {
            let fallback = LongPoint::new_range_query(field_name, vec![7], vec![9])?;
            let query = IndexSortSortedNumericDocValuesRangeQuery::new(field_name, 7, 9, fallback);
            let weight = query.create_weight(&searcher, &ScoreMode::Complete, 1.0, None)?;
            for ctx in searcher.get_leaf_contexts()? {
                assert_eq!(1400, weight.count(ctx)?);
            }
        }
        // Both bounds do not exist in the dataset
        {
            let fallback = LongPoint::new_range_query(field_name, vec![6], vec![10])?;
            let query = IndexSortSortedNumericDocValuesRangeQuery::new(field_name, 6, 10, fallback);
            let weight = query.create_weight(&searcher, &ScoreMode::Complete, 1.0, None)?;
            for ctx in searcher.get_leaf_contexts()? {
                assert_eq!(1400, weight.count(ctx)?);
            }
        }
        // Min bound exists in the dataset, not the max
        {
            let fallback = LongPoint::new_range_query(field_name, vec![7], vec![10])?;
            let query = IndexSortSortedNumericDocValuesRangeQuery::new(field_name, 7, 10, fallback);
            let weight = query.create_weight(&searcher, &ScoreMode::Complete, 1.0, None)?;
            for ctx in searcher.get_leaf_contexts()? {
                assert_eq!(1400, weight.count(ctx)?);
            }
        }
        // Min bound doesn't exist in the dataset, max does
        {
            let fallback = LongPoint::new_range_query(field_name, vec![6], vec![9])?;
            let query = IndexSortSortedNumericDocValuesRangeQuery::new(field_name, 6, 9, fallback);
            let weight = query.create_weight(&searcher, &ScoreMode::Complete, 1.0, None)?;
            for ctx in searcher.get_leaf_contexts()? {
                assert_eq!(1400, weight.count(ctx)?);
            }
        }
        // Min bound is the min value of the dataset
        {
            let fallback = LongPoint::new_range_query(field_name, vec![5], vec![8])?;
            let query = IndexSortSortedNumericDocValuesRangeQuery::new(field_name, 5, 8, fallback);
            let weight = query.create_weight(&searcher, &ScoreMode::Complete, 1.0, None)?;
            for ctx in searcher.get_leaf_contexts()? {
                assert_eq!(1100, weight.count(ctx)?);
            }
        }
        // Min bound is less than min value of the dataset
        {
            let fallback = LongPoint::new_range_query(field_name, vec![4], vec![8])?;
            let query = IndexSortSortedNumericDocValuesRangeQuery::new(field_name, 4, 8, fallback);
            let weight = query.create_weight(&searcher, &ScoreMode::Complete, 1.0, None)?;
            for ctx in searcher.get_leaf_contexts()? {
                assert_eq!(1100, weight.count(ctx)?);
            }
        }
        // Max bound is the max value of the dataset
        {
            let fallback = LongPoint::new_range_query(field_name, vec![10], vec![13])?;
            let query =
                IndexSortSortedNumericDocValuesRangeQuery::new(field_name, 10, 13, fallback);
            let weight = query.create_weight(&searcher, &ScoreMode::Complete, 1.0, None)?;
            for ctx in searcher.get_leaf_contexts()? {
                assert_eq!(1500, weight.count(ctx)?);
            }
        }
        // Max bound is greater than max value of the dataset
        {
            let fallback = LongPoint::new_range_query(field_name, vec![10], vec![14])?;
            let query =
                IndexSortSortedNumericDocValuesRangeQuery::new(field_name, 10, 14, fallback);
            let weight = query.create_weight(&searcher, &ScoreMode::Complete, 1.0, None)?;
            for ctx in searcher.get_leaf_contexts()? {
                assert_eq!(1500, weight.count(ctx)?);
            }
        }
        // Everything matches
        {
            let fallback = LongPoint::new_range_query(field_name, vec![2], vec![14])?;
            let query = IndexSortSortedNumericDocValuesRangeQuery::new(field_name, 2, 14, fallback);
            let weight = query.create_weight(&searcher, &ScoreMode::Complete, 1.0, None)?;
            for ctx in searcher.get_leaf_contexts()? {
                assert_eq!(3500, weight.count(ctx)?);
            }
        }
        // Bounds equal to min/max values of the dataset, everything matches
        {
            let fallback = LongPoint::new_range_query(field_name, vec![2], vec![3])?;
            let query = IndexSortSortedNumericDocValuesRangeQuery::new(field_name, 2, 3, fallback);
            let weight = query.create_weight(&searcher, &ScoreMode::Complete, 1.0, None)?;
            for ctx in searcher.get_leaf_contexts()? {
                assert_eq!(0, weight.count(ctx)?);
            }
        }
        // Bounds are greater than the max value of the dataset
        {
            let fallback = LongPoint::new_range_query(field_name, vec![14], vec![15])?;
            let query =
                IndexSortSortedNumericDocValuesRangeQuery::new(field_name, 14, 15, fallback);
            let weight = query.create_weight(&searcher, &ScoreMode::Complete, 1.0, None)?;
            for ctx in searcher.get_leaf_contexts()? {
                assert_eq!(0, weight.count(ctx)?);
            }
        }

        writer.close()?;
        Ok(())
    }

    #[test]
    fn test_random_count_with_bkd_asc() -> Result<()> {
        do_test_random_count_with_bkd(false)
    }

    #[test]
    fn test_random_count_with_bkd_desc() -> Result<()> {
        do_test_random_count_with_bkd(true)
    }

    fn do_test_random_count_with_bkd(reverse: bool) -> Result<()> {
        let mut random = random();
        let field_name = "field";
        // TODO 未实现MockAnalyzer
        let dir = new_directory_shared(&mut random)?;
        let mut iwc = IndexWriterConfig::new();
        let sort_field =
            SortedNumericSortField::with_reverse(field_name, SortFieldType::Long, reverse)?;
        let sort = Sort::with_fields(vec![sort_field])?;
        iwc.set_index_sort(sort)?;
        let writer = RandomIndexWriter::with_config(&mut random, dir.clone(), iwc);

        for _i in 0..100 {
            let value = random.random_range(0..1000) as i64;
            let repeat = random.random_range(0..1000);

            add_doc_with_bkd(&writer, field_name, value, repeat)?;
        }

        writer.flush()?;
        // TODO force_merge未实现
        // writer.force_merge(1)?;
        let reader = writer.get_reader()?;
        let searcher = new_searcher_with_reader(reader)?;

        for _k in 0..100 {
            let random1 = random.random_range(0..1100) as i64;
            let random2 = random.random_range(0..1100) as i64;

            let low = random1.min(random2);
            let upper = random1.max(random2);

            let range_query = LongPoint::new_range_query(field_name, vec![low], vec![upper])?;
            let index_sort_range_query = IndexSortSortedNumericDocValuesRangeQuery::new(
                field_name,
                low,
                upper,
                range_query.clone(),
            );

            let index_sort_range_query_weight =
                index_sort_range_query.create_weight(&searcher, &ScoreMode::Complete, 1.0, None)?;

            let range_query_weight =
                range_query.create_weight(&searcher, &ScoreMode::Complete, 1.0, None)?;

            for ctx in searcher.get_leaf_contexts()? {
                let expected = range_query_weight.count(ctx)?;
                let actual = index_sort_range_query_weight.count(ctx)?;
                assert_eq!(expected, actual);
            }
        }
        writer.close()?;
        Ok(())
    }

    fn add_doc_with_bkd<D>(
        index_writer: &RandomIndexWriter<D>,
        field: &str,
        value: i64,
        repeat: i32,
    ) -> Result<()>
    where
        D: Directory,
    {
        for _ in 0..repeat {
            let mut doc = Document::new();
            doc.add(SortedNumericDocValuesField::new(field, value));
            doc.add(LongPoint::new(field, vec![value])?);
            index_writer.add_document(doc)?;
        }
        Ok(())
    }
}
