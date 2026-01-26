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
use crate::core::index::doc_values::{DocValues, SortedNumeric};
use crate::core::index::doc_values_skipper::DocValuesSkipper;
use crate::core::index::index_reader_context::{IRCTermState, IndexReaderContext};
use crate::core::index::leaf_reader::{LRDocValuesSkipper, LeafReader};
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::query_timeout::QueryTimeout;
use crate::core::index::sorted_numeric_doc_values::SortedNumericDocValues;
use crate::core::index::term_states::TermStates;
use crate::core::search::QueryCache;
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::constant_score_weight::ConstantScoreWeight;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::search::doc_id_set_iterator::{
    AllDISI, DocIdSetIterator, DocIdSetIteratorEnum2, EmptyDISI, RangeDISI,
};
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
use crate::core::search::scorer_supplier::ScorerSupplierEnum4;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::two_phase_iterator::{TwoPhaseIterator, TwoPhaseIteratorEnum2};
use crate::core::search::weight::{DefaultScorerSupplier, Weight};
use crate::core::util::error::lucene_error::Result;
use std::marker::PhantomData;
use std::sync::Arc;

#[derive(Hash, Eq, PartialEq, Debug, Clone)]
pub struct SortedNumericDocValuesRangeQuery {
    field: String,
    lower_value: i64,
    upper_value: i64,
}
impl SortedNumericDocValuesRangeQuery {
    pub fn new(field: String, lower_value: i64, upper_value: i64) -> Self {
        Self {
            field,
            lower_value,
            upper_value,
        }
    }
}
impl QueryBase for SortedNumericDocValuesRangeQuery {
    fn as_string(&self, field: &str) -> String {
        let mut out = String::new();

        if self.field != field {
            out.push_str(&self.field);
            out.push(':');
        }
        out.push('[');
        out.push_str(&self.lower_value.to_string());
        out.push_str(" TO ");
        out.push_str(&self.upper_value.to_string());
        out.push(']');
        out
    }

    type Weight<S, IRC, QCP, QC>
        = SortedNumericDocValuesRangeQueryWeight<IRC::LeafReader>
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
        Ok(SortedNumericDocValuesRangeQueryWeight::new(
            self,
            *score_mode,
            boost,
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
pub struct SortedNumericDocValuesRangeQueryWeight<LR>
where
    LR: LeafReader,
{
    query: SortedNumericDocValuesRangeQuery,
    base: ConstantScoreWeight,
    parent_query: Arc<Query>,
    score_mode: ScoreMode,
    _leaf_reader: PhantomData<LR>,
}
impl<LR> SortedNumericDocValuesRangeQueryWeight<LR>
where
    LR: LeafReader,
{
    fn new(query: SortedNumericDocValuesRangeQuery, score_mode: ScoreMode, boost: f32) -> Self {
        let query_clone = query.clone();
        let parent_query = Arc::new(query.into());
        Self {
            query: query_clone,
            base: ConstantScoreWeight::new(boost),
            parent_query,
            score_mode,
            _leaf_reader: PhantomData,
        }
    }

    fn get_doc_id_set_iterator_or_null_for_primary_sort<NDV, SK>(
        &self,
        reader: &LR,
        numeric_doc_values: &mut NDV,
        skipper: &mut SK,
    ) -> Result<Option<DISI>>
    where
        NDV: NumericDocValues,
        SK: DocValuesSkipper,
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
                min_doc_id =
                    Self::next_doc(skipper.min_doc_id_with_level(0), numeric_doc_values, |l| {
                        l <= self.query.upper_value
                    })?;
            }

            if skipper.min_value() >= self.query.lower_value {
                max_doc_id = skipper.doc_count();
            } else {
                skipper.advance_with_range(i64::MIN, self.query.lower_value)?;
                max_doc_id =
                    Self::next_doc(skipper.min_doc_id_with_level(0), numeric_doc_values, |l| {
                        l < self.query.lower_value
                    })?;
            }
        } else {
            if skipper.min_value() >= self.query.lower_value {
                min_doc_id = 0;
            } else {
                skipper.advance_with_range(self.query.lower_value, i64::MAX)?;
                min_doc_id =
                    Self::next_doc(skipper.min_doc_id_with_level(0), numeric_doc_values, |l| {
                        l >= self.query.lower_value
                    })?;
            }

            if skipper.max_value() <= self.query.upper_value {
                max_doc_id = skipper.doc_count();
            } else {
                skipper.advance_with_range(self.query.upper_value, i64::MAX)?;
                max_doc_id =
                    Self::next_doc(skipper.min_doc_id_with_level(0), numeric_doc_values, |l| {
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

impl<LR> SegmentCacheable<LR> for SortedNumericDocValuesRangeQueryWeight<LR>
where
    LR: LeafReader,
{
    fn is_cacheable(&self, ctx: &LeafReaderContext<LR>) -> Result<bool> {
        let field = vec![self.query.field.clone()];
        DocValues::is_cacheable(ctx, field.as_ref())
    }
}

impl<LR> Weight<LR> for SortedNumericDocValuesRangeQueryWeight<LR>
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

    type ScorerSupplier = SortedNumericDocValuesRangeSs<LR>;

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
                let scorer =
                    ConstantScoreScorer::with_disi(self.base.score(), self.score_mode, iter);
                return Ok(Some(ScorerSupplierEnum4::A(DefaultScorerSupplier::new(
                    scorer,
                ))));
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
                        let v = DefaultScorerSupplier::new(ConstantScoreScorer::with_disi(
                            self.base.score(),
                            self.score_mode,
                            ps_iterator,
                        ));
                        return Ok(Some(ScorerSupplierEnum4::B(v)));
                    } else {
                        TwoPhaseIteratorEnum2::A(TwoPhaseIterator3::new(
                            singleton,
                            self.query.clone(),
                        ))
                    }
                },
                None => {
                    TwoPhaseIteratorEnum2::A(TwoPhaseIterator3::new(singleton, self.query.clone()))
                },
            }
        } else {
            TwoPhaseIteratorEnum2::B(TwoPhaseIterator4::new(values, self.query.clone()))
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
                let scorer = ConstantScoreScorer::with_tpi(self.base.score(), self.score_mode, v);
                let v = DefaultScorerSupplier::new(scorer);
                Ok(Some(ScorerSupplierEnum4::C(v)))
            },
            None => {
                let scorer =
                    ConstantScoreScorer::with_tpi(self.base.score(), self.score_mode, iterator);
                let v = DefaultScorerSupplier::new(scorer);
                Ok(Some(ScorerSupplierEnum4::D(v)))
            },
        }
    }
}
pub type DISI = DocIdSetIteratorEnum2<EmptyDISI, RangeDISI>;
pub type TPI<LR> = TwoPhaseIteratorEnum2<
    TwoPhaseIterator3<<SortedNumeric<LR> as SortedNumericDocValues>::NumericDocValues>,
    TwoPhaseIterator4<SortedNumeric<LR>>,
>;

pub type SortedNumericDocValuesRangeSs<LR> = ScorerSupplierEnum4<
    DefaultScorerSupplier<ConstantScoreScorer<AllDISI, DummyTwoPhaseIterator>>,
    DefaultScorerSupplier<ConstantScoreScorer<DISI, DummyTwoPhaseIterator>>,
    DefaultScorerSupplier<
        ConstantScoreScorer<DummyDISI, DocValuesRangeIterator<TPI<LR>, LRDocValuesSkipper<LR>>>,
    >,
    DefaultScorerSupplier<ConstantScoreScorer<DummyDISI, TPI<LR>>>,
>;
pub struct TwoPhaseIterator3<N>
where
    N: NumericDocValues,
{
    singleton: N,
    query: SortedNumericDocValuesRangeQuery,
}
impl<N> TwoPhaseIterator3<N>
where
    N: NumericDocValues,
{
    pub fn new(singleton: N, query: SortedNumericDocValuesRangeQuery) -> Self {
        TwoPhaseIterator3 { singleton, query }
    }
}
impl<N> TwoPhaseIterator for TwoPhaseIterator3<N>
where
    N: NumericDocValues,
{
    type DocIdSetIterator = N;
    type DocIdSetIteratorRef<'a>
        = &'a N
    where
        Self: 'a;
    type DocIdSetIteratorMut<'a>
        = &'a mut N
    where
        Self: 'a;

    fn approximation_mut(&mut self) -> Result<Self::DocIdSetIteratorMut<'_>> {
        Ok(&mut self.singleton)
    }

    fn approximation(&self) -> Result<Self::DocIdSetIteratorRef<'_>> {
        Ok(&self.singleton)
    }

    fn matches(&mut self) -> Result<bool> {
        let value = self.singleton.long_value()?;
        Ok(value >= self.query.lower_value && value <= self.query.upper_value)
    }

    fn match_cost(&self) -> f32 {
        2f32
    }
}
pub struct TwoPhaseIterator4<S>
where
    S: SortedNumericDocValues,
{
    value: S,
    query: SortedNumericDocValuesRangeQuery,
}

impl<S> TwoPhaseIterator4<S>
where
    S: SortedNumericDocValues,
{
    pub fn new(value: S, query: SortedNumericDocValuesRangeQuery) -> Self {
        TwoPhaseIterator4 { value, query }
    }
}

impl<S> TwoPhaseIterator for TwoPhaseIterator4<S>
where
    S: SortedNumericDocValues,
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
        Ok(&mut self.value)
    }

    fn approximation(&self) -> Result<Self::DocIdSetIteratorRef<'_>> {
        Ok(&self.value)
    }

    fn matches(&mut self) -> Result<bool> {
        let lower = self.query.lower_value;
        let upper = self.query.upper_value;
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
