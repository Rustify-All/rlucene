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
use crate::core::document::sorted_numeric_doc_values_range_query::DISI;
use crate::core::index::BytesRef;
use crate::core::index::doc_values::{DocValues, SortedSet};
use crate::core::index::doc_values_skipper::DocValuesSkipper;
use crate::core::index::index_reader_context::{IRCTermState, IndexReaderContext};
use crate::core::index::leaf_reader::{LRDocValuesSkipper, LeafReader};
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::query_timeout::QueryTimeout;
use crate::core::index::sorted_doc_values::SortedDocValues;
use crate::core::index::sorted_set_doc_values::SortedSetDocValues;
use crate::core::index::term_states::TermStates;
use crate::core::search::QueryCache;
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::constant_score_weight::ConstantScoreWeight;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::search::doc_id_set_iterator::{AllDISI, DocIdSetIterator, EmptyDISI, RangeDISI};
use crate::core::search::doc_values_range_iterator::DocValuesRangeIterator;
use crate::core::search::dummy::dummy_disi::DummyDISI;
use crate::core::search::dummy::dummy_query::DummyQuery;
use crate::core::search::dummy::dummy_two_phase_iterator::DummyTwoPhaseIterator;
use crate::core::search::explanation::Explanation;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::matches_utils::MatchWithNoTerms;
use crate::core::search::query::{Query, QueryBase};
use crate::core::search::query_caching_policy::QueryCachingPolicy;
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::{Scorer, ScorerEnum5};
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::two_phase_iterator::{TwoPhaseIterator, TwoPhaseIteratorEnum2};
use crate::core::search::weight::{DefaultBulkScorer, Weight};
use crate::core::util::error::lucene_error::Result;
use std::marker::PhantomData;
use std::sync::Arc;

#[derive(Debug, Hash, Eq, PartialEq, Clone)]
pub struct SortedSetDocValuesRangeQuery {
    pub field: String,
    pub lower_value: Option<BytesRef<Vec<u8>>>,
    pub upper_value: Option<BytesRef<Vec<u8>>>,
    pub lower_inclusive: bool,
    pub upper_inclusive: bool,
}
impl SortedSetDocValuesRangeQuery {
    pub fn new(
        field: String,
        lower_value: Option<BytesRef<Vec<u8>>>,
        upper_value: Option<BytesRef<Vec<u8>>>,
        lower_inclusive: bool,
        upper_inclusive: bool,
    ) -> Self {
        let lower_inclusive = lower_inclusive && lower_value.is_some();
        let upper_inclusive = upper_inclusive && upper_value.is_some();

        SortedSetDocValuesRangeQuery {
            field,
            lower_value,
            upper_value,
            lower_inclusive,
            upper_inclusive,
        }
    }
}
impl QueryBase for SortedSetDocValuesRangeQuery {
    fn as_string(&self, field: &str) -> String {
        let mut b = String::new();
        if self.field.as_str() != field {
            b.push_str(self.field.as_str());
            b.push(':');
        }
        b.push(if self.lower_inclusive { '[' } else { '{' });
        match &self.lower_value {
            None => b.push('*'),
            Some(v) => b.push_str(&format!("{}", v)),
        }
        b.push_str(" TO ");
        match &self.upper_value {
            None => b.push('*'),
            Some(v) => b.push_str(&format!("{}", v)),
        }
        b.push(if self.upper_inclusive { ']' } else { '}' });
        b
    }

    type Weight<S, IRC, QCP, QC>
        = SortedSetDocValuesRangeQueryWeight<IRC::LeafReader>
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
        Ok(SortedSetDocValuesRangeQueryWeight::new(
            self,
            boost,
            *score_mode,
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

pub struct SortedSetDocValuesRangeQueryWeight<LR>
where
    LR: LeafReader,
{
    query: SortedSetDocValuesRangeQuery,
    base: ConstantScoreWeight,
    parent_query: Arc<Query>,
    score_mode: ScoreMode,
    _leaf_reader: PhantomData<LR>,
}
impl<LR> SortedSetDocValuesRangeQueryWeight<LR>
where
    LR: LeafReader,
{
    fn new(query: SortedSetDocValuesRangeQuery, score: f32, score_mode: ScoreMode) -> Self {
        let query_clone = query.clone();
        let parent_query = Arc::new(query.into());
        SortedSetDocValuesRangeQueryWeight {
            query: query_clone,
            base: ConstantScoreWeight::new(score),
            parent_query,
            score_mode,
            _leaf_reader: PhantomData,
        }
    }
}

impl<LR> SegmentCacheable<LR> for SortedSetDocValuesRangeQueryWeight<LR>
where
    LR: LeafReader,
{
    fn is_cacheable(&self, ctx: &LeafReaderContext<LR>) -> Result<bool> {
        let field = vec![self.query.field.clone()];
        DocValues::is_cacheable(ctx, field.as_ref())
    }
}

impl<LR> Weight<LR> for SortedSetDocValuesRangeQueryWeight<LR>
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

    type ScorerSupplier = SortedSetDocValuesRangeSs<LR>;

    fn scorer_supplier(
        &self,
        context: &LeafReaderContext<LR>,
    ) -> Result<Option<Self::ScorerSupplier>> {
        if context
            .reader()
            .get_field_infos()?
            .field_info_by_name(&self.query.field)
            .is_none()
        {
            return Ok(None);
        }

        let values = DocValues::get_sorted_set(context.reader(), &self.query.field)?;
        Ok(Some(ScorerSupplierImpl3::new(
            self.query.clone(),
            values,
            self.base.score(),
            self.score_mode,
        )?))
    }
}
fn get_doc_id_set_iterator_or_null_for_primary_sort<LR, SDV, SK>(
    reader: &LR,
    sorted_doc_values: &mut SDV,
    skipper: &mut SK,
    min_ord: i64,
    max_ord: i64,
    field: &str,
) -> Result<Option<DISI>>
where
    SDV: SortedDocValues,
    SK: DocValuesSkipper,
    LR: LeafReader,
{
    if skipper.doc_count() != reader.max_doc()? {
        return Ok(None);
    }

    let index_sort = reader.get_metadata()?.get_sort();
    if index_sort.is_none() {
        return Ok(None);
    }
    let index_sort = index_sort.as_ref().unwrap();

    let sort_fields = index_sort.get_sort();
    let Some(first) = sort_fields.first() else {
        return Ok(None);
    };

    if first.get_field() != Some(field) {
        return Ok(None);
    }

    let min_doc_id: i32;
    let max_doc_id: i32;

    if first.get_reverse() {
        if skipper.max_value() <= max_ord {
            min_doc_id = 0;
        } else {
            skipper.advance_with_range(i64::MIN, max_ord)?;
            min_doc_id = next_doc_ord(skipper.min_doc_id_with_level(0), sorted_doc_values, |l| {
                l <= max_ord
            })?;
        }

        if skipper.min_value() >= min_ord {
            max_doc_id = skipper.doc_count();
        } else {
            skipper.advance_with_range(i64::MIN, min_ord)?;
            max_doc_id = next_doc_ord(skipper.min_doc_id_with_level(0), sorted_doc_values, |l| {
                l < min_ord
            })?;
        }
    } else {
        if skipper.min_value() >= min_ord {
            min_doc_id = 0;
        } else {
            skipper.advance_with_range(min_ord, i64::MAX)?;
            min_doc_id = next_doc_ord(skipper.min_doc_id_with_level(0), sorted_doc_values, |l| {
                l >= min_ord
            })?;
        }

        if skipper.max_value() <= max_ord {
            max_doc_id = skipper.doc_count();
        } else {
            skipper.advance_with_range(max_ord, i64::MAX)?;
            max_doc_id = next_doc_ord(skipper.min_doc_id_with_level(0), sorted_doc_values, |l| {
                l > max_ord
            })?;
        }
    }

    if min_doc_id == max_doc_id {
        return Ok(Some(DISI::A(EmptyDISI::default())));
    }

    Ok(Some(DISI::B(RangeDISI::new(min_doc_id, max_doc_id)?)))
}

fn next_doc_ord<SDV, P>(start_doc: i32, doc_values: &mut SDV, predicate: P) -> Result<i32>
where
    SDV: SortedDocValues,
    P: Fn(i64) -> bool,
{
    let mut doc = doc_values.doc_id();
    if start_doc > doc {
        doc = doc_values.advance(start_doc)?;
    }

    while doc < NO_MORE_DOCS {
        if predicate(doc_values.ord_value()? as i64) {
            break;
        }
        doc = doc_values.next_doc()?;
    }

    Ok(doc)
}
pub type SortedSetDocValuesRangeSs<LR> = ScorerSupplierImpl3<LR>;
pub type SortedSetDocValuesRangeSsScorer<LR> =
    <ScorerSupplierImpl3<LR> as ScorerSupplier<LR>>::Scorer;
pub type SortedSetDocValuesRangeSsScorerDisi<LR> =
    <<ScorerSupplierImpl3<LR> as ScorerSupplier<LR>>::Scorer as Scorer>::DocIdSetIterator;
pub struct ScorerSupplierImpl3<LR>
where
    LR: LeafReader,
{
    query: SortedSetDocValuesRangeQuery,
    values: Option<SortedSet<LR>>,
    cost: i64,
    score: f32,
    score_mode: ScoreMode,
}
impl<LR> ScorerSupplierImpl3<LR>
where
    LR: LeafReader,
{
    pub fn new(
        query: SortedSetDocValuesRangeQuery,
        values: SortedSet<LR>,
        score: f32,
        score_mode: ScoreMode,
    ) -> Result<Self> {
        let cost = values.cost()?;
        Ok(ScorerSupplierImpl3 {
            query,
            values: Some(values),
            cost,
            score,
            score_mode,
        })
    }
}
impl<LR> ScorerSupplier<LR> for ScorerSupplierImpl3<LR>
where
    LR: LeafReader,
{
    type Scorer = ScorerType<LR>;
    type BulkScorer = DefaultBulkScorer<Self::Scorer>;

    fn get(
        &mut self,
        _lead_cost: i64,
        context: &LeafReaderContext<LR>,
    ) -> Result<Option<Self::Scorer>> {
        let mut skipper_opt = context.reader().get_doc_values_skipper(&self.query.field)?;
        let mut values = match self.values.take() {
            Some(v) => v,
            None => DocValues::get_sorted_set(context.reader(), &self.query.field)?,
        };
        let min_ord: i64 = if self.query.lower_value.is_none() {
            0
        } else {
            let lv = self.query.lower_value.as_ref().unwrap();
            let ord = values.lookup_term(lv)?;

            if ord < 0 {
                -1 - ord
            } else if self.query.lower_inclusive {
                ord
            } else {
                ord + 1
            }
        };

        let max_ord: i64 = if self.query.upper_value.is_none() {
            values.get_value_count()? - 1
        } else {
            let uv = self.query.upper_value.as_ref().unwrap();
            let ord = values.lookup_term(uv)?;

            if ord < 0 {
                -2 - ord
            } else if self.query.upper_inclusive {
                ord
            } else {
                ord - 1
            }
        };

        // no terms matched
        if min_ord > max_ord {
            let v =
                ConstantScoreScorer::with_disi(self.score, self.score_mode, EmptyDISI::default());
            return Ok(Some(ScorerType::<LR>::A(v)));
        }

        if let Some(ref skipper) = skipper_opt
            && (min_ord > skipper.max_value() || max_ord < skipper.min_value())
        {
            let v =
                ConstantScoreScorer::with_disi(self.score, self.score_mode, EmptyDISI::default());
            return Ok(Some(ScorerType::<LR>::A(v)));
        }

        if let Some(ref skipper) = skipper_opt
            && skipper.doc_count() == context.reader().max_doc()?
            && skipper.min_value() >= min_ord
            && skipper.max_value() <= max_ord
        {
            let v = ConstantScoreScorer::with_disi(
                self.score,
                self.score_mode,
                AllDISI::new(skipper.doc_count()),
            );
            return Ok(Some(ScorerType::<LR>::B(v)));
        }
        let iterator = if values.is_single_valued() {
            let mut singleton = DocValues::unwrap_singleton_sorted(&mut values)?;
            match skipper_opt {
                Some(ref mut skipper) => {
                    let ps_iterator_opt = get_doc_id_set_iterator_or_null_for_primary_sort(
                        context.reader(),
                        &mut singleton,
                        skipper,
                        min_ord,
                        max_ord,
                        &self.query.field,
                    )?;
                    match ps_iterator_opt {
                        Some(ps_iterator) => {
                            let v = ConstantScoreScorer::with_disi(
                                self.score,
                                self.score_mode,
                                ps_iterator,
                            );
                            return Ok(Some(ScorerType::<LR>::C(v)));
                        },
                        None => TwoPhaseIteratorEnum2::A(TwoPhaseIterator5::new(
                            singleton, min_ord, max_ord,
                        )),
                    }
                },
                None => {
                    TwoPhaseIteratorEnum2::A(TwoPhaseIterator5::new(singleton, min_ord, max_ord))
                },
            }
        } else {
            TwoPhaseIteratorEnum2::B(TwoPhaseIterator6::new(values, min_ord, max_ord))
        };
        match skipper_opt {
            Some(skipper) => {
                let v = DocValuesRangeIterator::new(iterator, skipper, min_ord, max_ord, false);
                let scorer = ConstantScoreScorer::with_tpi(self.score, self.score_mode, v);
                Ok(Some(ScorerType::<LR>::E(scorer)))
            },
            None => {
                let scorer = ConstantScoreScorer::with_tpi(self.score, self.score_mode, iterator);
                Ok(Some(ScorerType::<LR>::D(scorer)))
            },
        }
    }

    fn bulk_scorer(&mut self, context: &LeafReaderContext<LR>) -> Result<Option<Self::BulkScorer>> {
        Ok(Some(self.default_bulk_scorer(context)?))
    }

    fn cost(&mut self, _context: &LeafReaderContext<LR>) -> Result<i64> {
        Ok(self.cost)
    }
}
pub type TPI1<LR> = TwoPhaseIteratorEnum2<
    TwoPhaseIterator5<<SortedSet<LR> as SortedSetDocValues>::SortedDocValues>,
    TwoPhaseIterator6<SortedSet<LR>>,
>;
pub type ScorerType<LR> = ScorerEnum5<
    ConstantScoreScorer<EmptyDISI, DummyTwoPhaseIterator>,
    ConstantScoreScorer<AllDISI, DummyTwoPhaseIterator>,
    ConstantScoreScorer<DISI, DummyTwoPhaseIterator>,
    ConstantScoreScorer<DummyDISI, TPI1<LR>>,
    ConstantScoreScorer<DummyDISI, DocValuesRangeIterator<TPI1<LR>, LRDocValuesSkipper<LR>>>,
>;
pub struct TwoPhaseIterator5<S>
where
    S: SortedDocValues,
{
    singleton: S,
    min_ord: i64,
    max_ord: i64,
}

impl<S> TwoPhaseIterator5<S>
where
    S: SortedDocValues,
{
    pub fn new(singleton: S, min_ord: i64, max_ord: i64) -> Self {
        TwoPhaseIterator5 {
            singleton,
            min_ord,
            max_ord,
        }
    }
}

impl<S> TwoPhaseIterator for TwoPhaseIterator5<S>
where
    S: SortedDocValues,
{
    type DocIdSetIterator = S;

    type DocIdSetIteratorRef<'a>
        = &'a S
    where
        Self: 'a;

    type DocIdSetIteratorMut<'a>
        = &'a mut S
    where
        Self: 'a;

    fn approximation_mut(&mut self) -> Result<Self::DocIdSetIteratorMut<'_>> {
        Ok(&mut self.singleton)
    }

    fn approximation(&self) -> Result<Self::DocIdSetIteratorRef<'_>> {
        Ok(&self.singleton)
    }

    fn matches(&mut self) -> Result<bool> {
        let ord = self.singleton.ord_value()? as i64;
        Ok(ord >= self.min_ord && ord <= self.max_ord)
    }

    fn match_cost(&self) -> f32 {
        2f32
    }
}
pub struct TwoPhaseIterator6<S>
where
    S: SortedSetDocValues,
{
    values: S,
    min_ord: i64,
    max_ord: i64,
}

impl<S> TwoPhaseIterator6<S>
where
    S: SortedSetDocValues,
{
    pub fn new(values: S, min_ord: i64, max_ord: i64) -> Self {
        TwoPhaseIterator6 {
            values,
            min_ord,
            max_ord,
        }
    }
}

impl<S> TwoPhaseIterator for TwoPhaseIterator6<S>
where
    S: SortedSetDocValues,
{
    type DocIdSetIterator = S;

    type DocIdSetIteratorRef<'a>
        = &'a S
    where
        Self: 'a;

    type DocIdSetIteratorMut<'a>
        = &'a mut S
    where
        Self: 'a;

    fn approximation_mut(&mut self) -> Result<Self::DocIdSetIteratorMut<'_>> {
        Ok(&mut self.values)
    }

    fn approximation(&self) -> Result<Self::DocIdSetIteratorRef<'_>> {
        Ok(&self.values)
    }

    fn matches(&mut self) -> Result<bool> {
        let count = self.values.doc_value_count()?;
        for _ in 0..count {
            let ord = self.values.next_ord()?;

            if ord < self.min_ord {
                continue;
            }
            // Values are sorted, so the first ord that is >= minOrd is our best
            // candidate
            return Ok(ord <= self.max_ord);
        }

        Ok(false) // all ords were < minOrd
    }

    fn match_cost(&self) -> f32 {
        2f32
    }
}
