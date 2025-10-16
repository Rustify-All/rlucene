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
use crate::core::index::index_reader_context::{IRCTermState, IndexReaderContext};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::query_timeout::QueryTimeout;
use crate::core::index::term_states::TermStates;
use crate::core::search::QueryCache;
use crate::core::search::bulk_scorer::BulkScorer;
use crate::core::search::constant_score_weight::ConstantScoreWeight;
use crate::core::search::doc_id_stream::DocIdStream;
use crate::core::search::dummy::dummy_query::DummyQuery;
use crate::core::search::dummy::dummy_weight::DummyWeight;
use crate::core::search::filter_leaf_collector::{FilterLeafCollectorRef, FilterSource};
use crate::core::search::filter_scorable::FilterScorable;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::leaf_collector::LeafCollector;
use crate::core::search::query::{Query, QueryBase};
use crate::core::search::query_caching_policy::QueryCachingPolicy;
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::scorable::{ChildScorable, Scorable};
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::weight::Weight;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::Result;
use std::fmt::{Debug, Display, Formatter};
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::sync::Arc;

pub struct ConstantScoreQuery {
    query: Box<Query>,
}
impl ConstantScoreQuery {
    pub fn new(query: Query) -> Self {
        Self {
            query: Box::new(query),
        }
    }
}

impl Eq for ConstantScoreQuery {}

impl PartialEq<Self> for ConstantScoreQuery {
    fn eq(&self, other: &Self) -> bool {
        self.query == other.query
    }
}

impl Hash for ConstantScoreQuery {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::any::type_name::<Self>().to_string().hash(state);
        self.query.hash(state);
    }
}

impl Debug for ConstantScoreQuery {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        todo!()
    }
}

impl QueryBase for ConstantScoreQuery {
    fn as_string(&self, field: &str) -> String {
        let inner = self.query.as_string(field);
        format!("ConstantScore({})", inner)
    }

    type Weight<S, IRC>
        = DummyWeight<IRC::LeafReader>
    where
        S: Similarity,
        IRC: IndexReaderContext;

    fn create_weight<S, IRC, QT, QCP, QC>(
        self,
        searcher: &IndexSearcher<IRC, S, QT, QCP, QC>,
        score_mode: &ScoreMode,
        boost: f32,
        _per_reader_term_state: Option<TermStates<IRCTermState<IRC>>>,
    ) -> Result<Self::Weight<S, IRC>>
    where
        IRC: IndexReaderContext,
        S: Similarity,
        QT: QueryTimeout,
        QCP: QueryCachingPolicy,
        QC: QueryCache,
        Self: Sized,
    {
        let inner_score_mode = if score_mode.is_exhaustive() {
            ScoreMode::CompleteNoScores
        } else {
            ScoreMode::TopDocs
        };

        let inner_weight = searcher.create_weight(self, inner_score_mode, 1.0)?;
        // if score_mode.needs_scores() {
        //     Ok(ConstantScoreWeight::new(boost, inner_weight))
        // } else {
        //     Ok(inner_weight)
        // }
        todo!()
    }

    type RewriteQuery = DummyQuery;

    fn visit<QV>(&self, visitor: &QV)
    where
        QV: QueryVisitor,
    {
        todo!()
    }
}

pub struct ConstantScoreQueryWeight {
    base: ConstantScoreWeight,
    parent_query: Arc<Query>,
}

pub struct ConstantBulkScorer<BS, W, LR>
where
    BS: BulkScorer,
    W: Weight<LR>,
    LR: LeafReader,
{
    bulk_scorer: BS,
    weight: W,
    the_score: f32,
    _marker: PhantomData<LR>,
}
impl<BS, W, LR> ConstantBulkScorer<BS, W, LR>
where
    BS: BulkScorer,
    W: Weight<LR>,
    LR: LeafReader,
{
    pub fn new(bulk_scorer: BS, weight: W, the_score: f32) -> Self {
        Self {
            bulk_scorer,
            weight,
            the_score,
            _marker: PhantomData,
        }
    }
    fn wrap_collector<LC>(collector: &mut LC, the_score: f32) -> FilterLeafCollectorImpl<LC>
    where
        LC: LeafCollector,
    {
        FilterLeafCollectorImpl::new(collector, the_score)
    }
}
impl<BS, W, LR> BulkScorer for ConstantBulkScorer<BS, W, LR>
where
    BS: BulkScorer,
    W: Weight<LR>,
    LR: LeafReader,
{
    fn score<LC, B>(
        &mut self,
        collector: &mut LC,
        accept_docs: Option<&B>,
        min: i32,
        max: i32,
    ) -> Result<i32>
    where
        LC: LeafCollector,
        B: Bits,
    {
        self.bulk_scorer.score(
            &mut Self::wrap_collector(collector, self.the_score),
            accept_docs,
            min,
            max,
        )
    }

    fn cost(&mut self) -> Result<i64> {
        self.bulk_scorer.cost()
    }
}

pub struct FilterLeafCollectorImpl<'a, LC>
where
    LC: LeafCollector,
{
    base: FilterLeafCollectorRef<'a, LC>,
    the_score: f32,
}

impl<'a, LC> FilterLeafCollectorImpl<'a, LC>
where
    LC: LeafCollector,
{
    pub fn new(in_: &'a mut LC, the_score: f32) -> Self {
        Self {
            base: in_.into(),
            the_score,
        }
    }
}

impl<'a, LC> Display for FilterLeafCollectorImpl<'a, LC>
where
    LC: LeafCollector + Display,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", std::any::type_name::<Self>(), self.base)
    }
}

impl<'a, LC> LeafCollector for FilterLeafCollectorImpl<'a, LC>
where
    LC: LeafCollector,
{
    fn set_scorer<S>(&mut self, scorer: &mut S) -> Result<()>
    where
        S: Scorable,
    {
        let mut v = FilterScorableImpl::new(self.the_score, scorer);
        self.base.inner.as_mut().set_scorer(&mut v)
    }

    fn collect<S>(&mut self, doc: i32, scorer: &mut S) -> Result<()>
    where
        S: Scorable,
    {
        self.base.collect(doc, scorer)
    }

    fn collect_stream<DS, S>(&mut self, stream: &mut DS, scorer: &mut S) -> Result<()>
    where
        DS: DocIdStream,
        S: Scorable,
    {
        self.base.collect_stream(stream, scorer)
    }

    type DocIdSetIterator = <FilterLeafCollectorRef<'a, LC> as LeafCollector>::DocIdSetIterator;

    fn competitive_iterator(&mut self) -> Result<Option<Self::DocIdSetIterator>> {
        self.base.competitive_iterator()
    }

    fn finish(&mut self) -> Result<()> {
        self.base.finish()
    }
}

struct FilterScorableImpl<'a, S>
where
    S: Scorable,
{
    the_score: f32,
    base: FilterScorable<'a, S>,
}
impl<'a, S> FilterScorableImpl<'a, S>
where
    S: Scorable,
{
    pub fn new(the_score: f32, s: &'a mut S) -> Self {
        let base = FilterScorable::new(s);
        Self { the_score, base }
    }
}
impl<'a, S> Scorable for FilterScorableImpl<'a, S>
where
    S: Scorable,
{
    fn score(&mut self) -> Result<f32> {
        Ok(self.the_score)
    }

    fn smoothing_score(&mut self, doc_id: i32) -> Result<f32> {
        self.base.smoothing_score(doc_id)
    }

    fn set_min_competitive_score(&mut self, min_score: f32) -> Result<()> {
        self.base.set_min_competitive_score(min_score)
    }

    type Scorable = <FilterScorable<'a, S> as Scorable>::Scorable;

    fn get_children(&self) -> Result<Vec<ChildScorable<Self::Scorable>>> {
        self.base.get_children()
    }

    fn cost(&mut self) -> Result<i64> {
        self.base.cost()
    }
}
