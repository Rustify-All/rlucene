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
use crate::core::document::doc_values_long_hash_set::DocValuesLongHashSet;
use crate::core::index::doc_values::{DocValues, SortedNumeric};
use crate::core::index::index_reader_context::{IRCTermState, IndexReaderContext};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::query_timeout::QueryTimeout;
use crate::core::index::sorted_numeric_doc_values::SortedNumericDocValues;
use crate::core::index::term_states::TermStates;
use crate::core::search::QueryCache;
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::constant_score_weight::ConstantScoreWeight;
use crate::core::search::dummy::dummy_disi::DummyDISI;
use crate::core::search::dummy::dummy_query::DummyQuery;
use crate::core::search::explanation::Explanation;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::matches_utils::MatchWithNoTerms;
use crate::core::search::query::{Query, QueryBase};
use crate::core::search::query_caching_policy::QueryCachingPolicy;
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::two_phase_iterator::{TwoPhaseIterator, TwoPhaseIteratorEnum2};
use crate::core::search::weight::{DefaultScorerSupplier, Weight};
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::Result;
use std::hash::Hash;
use std::marker::PhantomData;
use std::sync::Arc;

/// Similar to SortedNumericDocValuesRangeQuery but for a set
#[derive(Debug, Hash, Eq, PartialEq, Clone)]
pub struct SortedNumericDocValuesSetQuery {
    field: String,
    numbers: Arc<DocValuesLongHashSet>,
}
impl SortedNumericDocValuesSetQuery {
    pub fn new(field: String, mut numbers: Vec<i64>) -> Result<Self> {
        numbers.sort_unstable();
        Ok(SortedNumericDocValuesSetQuery {
            field,
            numbers: Arc::new(DocValuesLongHashSet::new(numbers.as_slice())?),
        })
    }
}
impl QueryBase for SortedNumericDocValuesSetQuery {
    fn as_string(&self, _field: &str) -> String {
        format!("{}: {}", self.field, self.numbers)
    }

    type Weight<S, IRC, QCP, QC>
        = SortedNumericDocValuesSetQueryWeight<IRC::LeafReader>
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
        Ok(SortedNumericDocValuesSetQueryWeight::new(
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
impl Accountable for SortedNumericDocValuesSetQuery {
    fn ram_bytes_used(&self) -> Result<i64> {
        todo!()
    }
}

pub struct SortedNumericDocValuesSetQueryWeight<LR>
where
    LR: LeafReader,
{
    query: SortedNumericDocValuesSetQuery,
    base: ConstantScoreWeight,
    parent_query: Arc<Query>,
    score_mode: ScoreMode,
    _leaf_reader: PhantomData<LR>,
}
impl<LR> SortedNumericDocValuesSetQueryWeight<LR>
where
    LR: LeafReader,
{
    fn new(query: SortedNumericDocValuesSetQuery, score_mode: ScoreMode, boost: f32) -> Self {
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
}

impl<LR> SegmentCacheable<LR> for SortedNumericDocValuesSetQueryWeight<LR>
where
    LR: LeafReader,
{
    fn is_cacheable(&self, ctx: &LeafReaderContext<LR>) -> Result<bool> {
        let field = vec![self.query.field.clone()];
        DocValues::is_cacheable(ctx, field.as_ref())
    }
}

impl<LR> Weight<LR> for SortedNumericDocValuesSetQueryWeight<LR>
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

    type ScorerSupplier = DefaultScorerSupplierSs<LR>;

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
        let mut values = DocValues::get_sorted_numeric(context.reader(), &self.query.field)?;
        let iterator = if values.is_single_valued() {
            let singleton = DocValues::unwrap_singleton_numeric(&mut values)?;
            TwoPhaseIteratorEnum2::A(TwoPhaseIterator1::new(singleton, self.query.clone()))
        } else {
            TwoPhaseIteratorEnum2::B(TwoPhaseIterator2::new(values, self.query.clone()))
        };
        let scorer = ConstantScoreScorer::with_tpi(self.base.score(), self.score_mode, iterator);
        Ok(Some(DefaultScorerSupplier::new(scorer)))
    }
}
pub type DefaultScorerSupplierSs<LR> = DefaultScorerSupplier<
    ConstantScoreScorer<
        DummyDISI,
        TwoPhaseIteratorEnum2<
            TwoPhaseIterator1<<SortedNumeric<LR> as SortedNumericDocValues>::NumericDocValues>,
            TwoPhaseIterator2<SortedNumeric<LR>>,
        >,
    >,
>;
pub struct TwoPhaseIterator1<N>
where
    N: NumericDocValues,
{
    singleton: N,
    query: SortedNumericDocValuesSetQuery,
}
impl<N> TwoPhaseIterator1<N>
where
    N: NumericDocValues,
{
    pub fn new(singleton: N, query: SortedNumericDocValuesSetQuery) -> Self {
        TwoPhaseIterator1 { singleton, query }
    }
}
impl<N> TwoPhaseIterator for TwoPhaseIterator1<N>
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
        let numbers = &self.query.numbers;
        Ok(value >= numbers.min_value && value <= numbers.max_value && numbers.contains(value))
    }

    fn match_cost(&self) -> f32 {
        5f32
    }
}
pub struct TwoPhaseIterator2<S>
where
    S: SortedNumericDocValues,
{
    value: S,
    query: SortedNumericDocValuesSetQuery,
}

impl<S> TwoPhaseIterator2<S>
where
    S: SortedNumericDocValues,
{
    pub fn new(value: S, query: SortedNumericDocValuesSetQuery) -> Self {
        TwoPhaseIterator2 { value, query }
    }
}

impl<S> TwoPhaseIterator for TwoPhaseIterator2<S>
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
        let numbers = &self.query.numbers;
        let count = self.value.doc_value_count()?;

        for _ in 0..count {
            let value = self.value.next_value()?;

            if value < numbers.min_value {
                continue;
            } else if value > numbers.max_value {
                return Ok(false); // sorted, terminate
            } else if numbers.contains(value) {
                return Ok(true);
            }
        }

        Ok(false)
    }

    fn match_cost(&self) -> f32 {
        5f32
    }
}
