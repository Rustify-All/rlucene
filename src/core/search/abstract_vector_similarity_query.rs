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
use crate::core::search::query::QueryRef;

use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::query_timeout::QueryTimeout;
use crate::core::search::abstract_knn_vector_query::FilteredDocIdSetIteratorImpl;
use crate::core::search::approximation_disi::ApproximationBitSetAccess;
use crate::core::search::approximation_disi::ApproximationDISI;
use crate::core::search::conjunction_disi::{ConjunctionDISI, ConjunctionDISIWithBitSet};
use crate::core::search::doc_id_set_iterator::BitSetIteratorAccess;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::DocIdSetIteratorEnum2;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::explanation::Explanation;
use crate::core::search::filtered_doc_id_set_iterator::{
  FilteredDocIdSetIterator, FilteredDocIdSetIteratorBase,
};
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::knn::knn_collector_manager::KnnCollectorManager;
use crate::core::search::query::{Query, QueryBase, QueryWeight, QueryWeightSs};
use crate::core::search::scorable::{FixedScore, Scorable};
use crate::core::search::score_doc::ScoreDoc;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::{Scorer, TwoPhaseState};
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::time_limiting_knn_collector_manager::TimeLimitingKnnCollectorManager;
use crate::core::search::top_docs::TopDocs;
use crate::core::search::total_hits::Relation::EqualTo;
use crate::core::search::two_phase_iterator::TwoPhaseIterator;
use crate::core::search::vector_scorer::VectorScorer;
use crate::core::search::vector_similarity_collector::VectorSimilarityCollector;
use crate::core::search::weight::{DefaultBulkScorer, Weight};
use crate::core::util::bit_set::{BitSet, of};
use crate::core::util::bit_set_iterator::BitSetIterator;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bits::Bits;
use crate::core::util::core_helper::CoreHelper;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::cell::Cell;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Search for all (approximate) vectors above a similarity threshold.
pub trait AbstractVectorSimilarityQuery: QueryBase {
  fn base(&self) -> &AbstractVectorSimilarityQueryBase;

  fn create_weight<IRC, SIM>(
    self,
    searcher: &IndexSearcher<IRC, SIM>,
    boost: f32,
  ) -> Result<
    AbstractVectorSimilarityQueryWeight<
      Self,
      IRC,
      Arc<crate::core::index::query_timeout::QueryTimeoutEnum>,
      SIM,
    >,
  >
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync,
    Self: Clone + Into<Query> + Send + Sync + Sized + 'static,
    for<'a> QueryRef<'a>: From<&'a Self>,
  {
    let filter_weight = match self.base().filter.as_ref() {
      Some(filter) => {
        let rewritten = searcher.rewrite_borrowed(filter)?.into_owned();
        Some(searcher.create_weight(rewritten, ScoreMode::CompleteNoScores, 1.0)?)
      },
      None => None,
    };

    Ok(AbstractVectorSimilarityQueryWeight::new(
      self,
      filter_weight,
      searcher.get_timeout(),
      boost,
    ))
  }

  fn get_knn_collector_manager(&self) -> VectorSimilarityCollectorManager {
    VectorSimilarityCollectorManager::new(
      self.base().traversal_similarity,
      self.base().result_similarity,
    )
  }

  fn create_vector_scorer<LR>(
    &self,
    context: &LeafReaderContext<LR>,
  ) -> Result<Option<crate::core::search::vector_scorer::LeafVectorScorer<LR>>>
  where
    LR: LeafReader;

  fn approximate_search<LR, B, K>(
    &self,
    context: &LeafReaderContext<LR>,
    accept_docs: Option<B>,
    visit_limit: usize,
    knn_collector_manager: &K,
  ) -> Result<TopDocs<ScoreDoc>>
  where
    LR: LeafReader,
    B: Bits,
    K: KnnCollectorManager;
}

#[derive(Clone, Debug)]
pub struct AbstractVectorSimilarityQueryBase {
  pub(crate) field: String,
  pub(crate) traversal_similarity: f32,
  pub(crate) result_similarity: f32,
  pub(crate) filter: Option<Box<Query>>,
}

impl AbstractVectorSimilarityQueryBase {
  /// Search for all (approximate) vectors above a similarity threshold using
  /// [`VectorSimilarityCollector`]. If a filter is applied, it traverses as many
  /// nodes as the cost of the filter, and then falls back to exact search if
  /// results are incomplete.
  ///
  /// # Parameters
  ///
  /// - `field`: A field that has been indexed as a vector field.
  /// - `traversal_similarity`: Lower similarity score for graph traversal.
  /// - `result_similarity`: Higher similarity score for result collection.
  /// - `filter`: A filter applied before the vector search.
  pub(crate) fn new(
    field: String,
    traversal_similarity: f32,
    result_similarity: f32,
    filter: Option<Query>,
  ) -> Result<Self> {
    if traversal_similarity > result_similarity {
      return Err(LuceneError::illegal_argument(
        "traversalSimilarity should be <= resultSimilarity",
      ));
    }
    Ok(Self {
      field,
      traversal_similarity,
      result_similarity,
      filter: filter.map(Box::new),
    })
  }
}

impl Hash for AbstractVectorSimilarityQueryBase {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.field.hash(state);
    (BitUtil::float_to_int_bits(self.traversal_similarity) as u32).hash(state);
    (BitUtil::float_to_int_bits(self.result_similarity) as u32).hash(state);
    self.filter.hash(state);
  }
}

impl PartialEq for AbstractVectorSimilarityQueryBase {
  fn eq(&self, other: &Self) -> bool {
    self.field == other.field
      && CoreHelper::compare_f32(self.traversal_similarity, other.traversal_similarity).is_eq()
      && CoreHelper::compare_f32(self.result_similarity, other.result_similarity).is_eq()
      && self.filter == other.filter
  }
}

impl Eq for AbstractVectorSimilarityQueryBase {}

pub struct VectorSimilarityCollectorManager {
  traversal_similarity: f32,
  result_similarity: f32,
}

impl VectorSimilarityCollectorManager {
  pub fn new(traversal_similarity: f32, result_similarity: f32) -> Self {
    Self {
      traversal_similarity,
      result_similarity,
    }
  }
}

impl KnnCollectorManager for VectorSimilarityCollectorManager {
  type KnnCollector<'a>
    = VectorSimilarityCollector
  where
    Self: 'a;

  fn new_collector<LR>(
    &self,
    visited_limit: usize,
    _context: &LeafReaderContext<LR>,
  ) -> Result<Self::KnnCollector<'_>>
  where
    LR: LeafReader,
  {
    VectorSimilarityCollector::new(
      self.traversal_similarity,
      self.result_similarity,
      visited_limit,
    )
  }
}

pub struct AbstractVectorSimilarityQueryWeight<
  Q,
  IRC: IndexReaderContext + 'static,
  QT,
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> where
  SIM: Similarity,
{
  query: Q,
  filter_weight: Option<QueryWeight<IRC, SIM>>,
  time_limiting_knn_collector_manager:
    TimeLimitingKnnCollectorManager<VectorSimilarityCollectorManager, QT>,
  boost: f32,
}

impl<Q, IRC, QT, SIM> AbstractVectorSimilarityQueryWeight<Q, IRC, QT, SIM>
where
  SIM: Similarity,
  Q: AbstractVectorSimilarityQuery + Clone + Into<Query>,
  for<'a> QueryRef<'a>: From<&'a Q>,
  IRC: IndexReaderContext + 'static,
  QT: QueryTimeout,
{
  fn new(
    query: Q,
    filter_weight: Option<QueryWeight<IRC, SIM>>,
    query_timeout: Option<QT>,
    boost: f32,
  ) -> Self {
    let manager = query.get_knn_collector_manager();
    Self {
      query,
      filter_weight,
      time_limiting_knn_collector_manager: TimeLimitingKnnCollectorManager::new(
        manager,
        query_timeout,
      ),
      boost,
    }
  }
}

impl<Q, IRC, QT, SIM> SegmentCacheable<IRC> for AbstractVectorSimilarityQueryWeight<Q, IRC, QT, SIM>
where
  SIM: Similarity,
  Q: AbstractVectorSimilarityQuery + Clone + Into<Query>,
  for<'a> QueryRef<'a>: From<&'a Q>,
  IRC: IndexReaderContext + 'static,
  QT: QueryTimeout,
{
  fn is_cacheable(&self, _ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    Ok(true)
  }
}

impl<Q, IRC, QT, SIM> Weight<IRC, SIM> for AbstractVectorSimilarityQueryWeight<Q, IRC, QT, SIM>
where
  SIM: Similarity,
  Q: AbstractVectorSimilarityQuery + Clone + Into<Query>,
  for<'a> QueryRef<'a>: From<&'a Q>,
  IRC: IndexReaderContext + 'static,
  QT: QueryTimeout,
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
    if let Some(filter_weight) = &self.filter_weight {
      let mut filter_scorer = match filter_weight.scorer(context, searcher)? {
        Some(scorer) => scorer,
        None => {
          return Ok(Explanation::no_match_no_details(
            "Doc does not match the filter",
          ));
        },
      };
      if filter_scorer.advance(doc)? > doc {
        return Ok(Explanation::no_match_no_details(
          "Doc does not match the filter",
        ));
      }
    }

    let mut scorer = match self.query.create_vector_scorer(context)? {
      Some(scorer) => scorer,
      None => {
        return Ok(Explanation::no_match_no_details(
          "Not indexed as the correct vector field",
        ));
      },
    };
    let doc_id = scorer.advance(doc)?;
    if doc_id == doc {
      let score = scorer.score()?;
      if score >= self.query.base().result_similarity {
        Ok(Explanation::match_(
          self.boost * score,
          "Score above threshold",
          vec![],
        ))
      } else {
        Ok(Explanation::no_match_no_details("Score below threshold"))
      }
    } else {
      Ok(Explanation::no_match_no_details("No vector found for doc"))
    }
  }

  fn get_query(&self) -> QueryRef<'_> {
    (&self.query).into()
  }

  type ScorerSupplier = QueryWeightSs<IRC, SIM>;

  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Option<Self::ScorerSupplier>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let reader = context.reader();
    let live_docs = reader.get_live_docs()?;
    match &self.filter_weight {
      None => {
        let results = self.query.approximate_search(
          context,
          live_docs.as_ref(),
          i32::MAX as usize,
          &self.time_limiting_knn_collector_manager,
        )?;
        Ok(
          VectorSimilarityScorerSupplier::from_score_docs(self.boost, results.score_docs)
            .map(|supplier| Box::new(supplier).into()),
        )
      },
      Some(filter_weight) => {
        let scorer = match filter_weight.scorer(context, searcher)? {
          Some(scorer) => scorer,
          // If the filter does not match any documents
          None => return Ok(None),
        };
        let live_docs = reader.get_live_docs()?;

        // TODO IMPORTANT 复用 Bitset 未实现 是不是可以让 DISI 加一个 super 获取 bitset?
        let mut filtered = FilteredDocIdSetIteratorImpl::new(live_docs.as_ref(), scorer);
        let accept_docs = of(&mut filtered, reader.max_doc()? as usize)?;
        let cardinality = accept_docs.cardinality();
        if cardinality == 0 {
          return Ok(None);
        }

        let results = self.query.approximate_search(
          context,
          Some(&accept_docs),
          cardinality,
          &self.time_limiting_knn_collector_manager,
        )?;

        let query_timeout = self.time_limiting_knn_collector_manager.get_query_timeout();
        if results.total_hits.relation() == EqualTo
          || query_timeout.is_some_and(|timeout| timeout.should_exit())
        {
          Ok(
            VectorSimilarityScorerSupplier::from_score_docs(self.boost, results.score_docs)
              .map(|supplier| Box::new(supplier).into()),
          )
        } else {
          let vector_scorer = self.query.create_vector_scorer(context)?;
          Ok(
            VectorSimilarityScorerSupplier::from_accept_docs(
              self.boost,
              vector_scorer,
              BitSetIterator::new(accept_docs, cardinality as i64)?,
              self.query.base().result_similarity,
            )?
            .map(|supplier| Box::new(supplier).into()),
          )
        }
      },
    }
  }
}

pub(crate) struct VectorSimilarityScorerSupplier<I> {
  iterator: Option<I>,
  fixed_cost: Option<i64>,
}

impl<IRC, I, SIM> ScorerSupplier<IRC, SIM> for VectorSimilarityScorerSupplier<I>
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
  I: DocIdSetIterator + CachedScoreHelper + 'static,
{
  type Scorer = ScorerImpl<I>;
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
    let iterator = self
      .iterator
      .take()
      .ok_or_else(|| LuceneError::illegal_state("ScorerSupplier::get called more than once"))?;
    Ok(ScorerImpl::new(iterator))
  }

  fn bulk_scorer(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Option<Self::BulkScorer>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let scorer = self.get(i64::MAX, context, searcher)?;
    Ok(Some(DefaultBulkScorer::new(scorer)))
  }

  fn cost(
    &mut self,
    _context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<i64>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    if let Some(cost) = self.fixed_cost {
      return Ok(cost);
    }
    self
      .iterator
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("ScorerSupplier::get called before cost"))?
      .cost()
  }
}

impl VectorSimilarityScorerSupplier<DocsIteratorImpl> {
  fn from_score_docs(boost: f32, mut score_docs: Vec<ScoreDoc>) -> Option<Self> {
    if score_docs.is_empty() {
      return None;
    }
    score_docs.sort_unstable_by_key(|score_doc| score_doc.doc);
    let fixed_cost = Some(score_docs.len() as i64);
    Some(Self {
      iterator: Some(DocsIteratorImpl::new(score_docs, boost)),
      fixed_cost,
    })
  }
}
impl<V, B> VectorSimilarityScorerSupplier<FilteredDocIdSetIteratorImpl1<V, B>>
where
  V: VectorScorer,
  B: BitSet,
{
  fn from_accept_docs(
    boost: f32,
    scorer: Option<V>,
    accept_docs: BitSetIterator<B>,
    threshold: f32,
  ) -> Result<Option<Self>> {
    let scorer = match scorer {
      Some(scorer) => scorer,
      None => return Ok(None),
    };
    Ok(Some(Self {
      iterator: Some(FilteredDocIdSetIteratorImpl1::new(
        scorer,
        accept_docs,
        boost,
        threshold,
      )?),
      fixed_cost: None,
    }))
  }
}

pub struct ScorerImpl<I> {
  iterator: I,
}

impl<I> ScorerImpl<I> {
  fn new(iterator: I) -> Self {
    Self { iterator }
  }
}

impl<I> Scorable for ScorerImpl<I>
where
  I: DocIdSetIterator + CachedScoreHelper,
{
  type Child<'a>
    = crate::core::search::dummy::dummy_scorable::DummyScorable
  where
    Self: 'a;

  fn score(&mut self) -> Result<f32> {
    Ok(self.iterator.cached_score())
  }
}

impl<I> FixedScore for ScorerImpl<I> {}

impl<I> DocIdSetIterator for ScorerImpl<I>
where
  I: DocIdSetIterator + CachedScoreHelper + 'static,
{
  fn doc_id(&self) -> i32 {
    DocIdSetIterator::doc_id(&self.iterator)
  }
  fn next_doc(&mut self) -> Result<i32> {
    DocIdSetIterator::next_doc(&mut self.iterator)
  }
  fn advance(&mut self, target: i32) -> Result<i32> {
    DocIdSetIterator::advance(&mut self.iterator, target)
  }
  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    DocIdSetIterator::slow_advance(&mut self.iterator, target)
  }
  fn cost(&self) -> Result<i64> {
    DocIdSetIterator::cost(&self.iterator)
  }
}
impl<I> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions for ScorerImpl<I>
where
  I: DocIdSetIterator + CachedScoreHelper + 'static,
{
  fn get_fixed_bit_set(&self) -> Option<&crate::core::util::fixed_bit_set::FixedBitSet> {
    crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions::get_fixed_bit_set(
      &self.iterator,
    )
  }
  fn get_sparse_fixed_bit_set(
    &self,
  ) -> Option<&crate::core::util::sparse_fixed_bit_set::SparseFixedBitSet> {
    crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions::get_sparse_fixed_bit_set(
      &self.iterator,
    )
  }
  fn get_doc_base_fixed_bit_set(
    &self,
  ) -> Option<(usize, &crate::core::util::fixed_bit_set::FixedBitSet)> {
    crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions::get_doc_base_fixed_bit_set(
      &self.iterator,
    )
  }
}
impl<I> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for ScorerImpl<I>
where
  I: DocIdSetIterator + CachedScoreHelper + 'static,
{
  fn is_bit_iter(&self) -> bool {
    crate::core::search::doc_id_set_iterator::BitSetIteratorAccess::is_bit_iter(&self.iterator)
  }
  fn get(&self, index: usize) -> Result<bool> {
    crate::core::search::doc_id_set_iterator::BitSetIteratorAccess::get(&self.iterator, index)
  }
  fn set_doc_id(&mut self, doc: i32) -> Result<()> {
    crate::core::search::doc_id_set_iterator::BitSetIteratorAccess::set_doc_id(
      &mut self.iterator,
      doc,
    )
  }
  fn bit_set_length(&self) -> Result<usize> {
    crate::core::search::doc_id_set_iterator::BitSetIteratorAccess::bit_set_length(&self.iterator)
  }
}
impl<I> ApproximationDISI for ScorerImpl<I>
where
  I: DocIdSetIterator + CachedScoreHelper + 'static,
{
  fn approx_doc_id(&self) -> i32 {
    DocIdSetIterator::doc_id(&{ &self.iterator })
  }
  fn approx_next_doc(&mut self) -> Result<i32> {
    DocIdSetIterator::next_doc(&mut { &mut self.iterator })
  }
  fn approx_advance(&mut self, target: i32) -> Result<i32> {
    DocIdSetIterator::advance(&mut { &mut self.iterator }, target)
  }
  fn approx_slow_advance(&mut self, target: i32) -> Result<i32> {
    DocIdSetIterator::slow_advance(&mut { &mut self.iterator }, target)
  }
  fn approx_cost(&self) -> Result<i64> {
    DocIdSetIterator::cost(&{ &self.iterator })
  }
}
impl<I> ApproximationBitSetAccess for ScorerImpl<I>
where
  I: DocIdSetIterator + CachedScoreHelper + 'static,
{
  fn approx_is_bit_iter(&self) -> bool {
    BitSetIteratorAccess::is_bit_iter(&{ &self.iterator })
  }
  fn approx_get(&self, index: usize) -> Result<bool> {
    BitSetIteratorAccess::get(&{ &self.iterator }, index)
  }
  fn approx_set_doc_id(&mut self, doc: i32) -> Result<()> {
    BitSetIteratorAccess::set_doc_id(&mut { &mut self.iterator }, doc)
  }
  fn approx_bit_set_length(&self) -> Result<usize> {
    BitSetIteratorAccess::bit_set_length(&{ &self.iterator })
  }
}

impl<I> TwoPhaseIterator for ScorerImpl<I>
where
  I: DocIdSetIterator + CachedScoreHelper + 'static,
{
  fn matches(&mut self) -> Result<bool> {
    Ok(true)
  }
  fn match_cost(&self) -> f32 {
    0.0
  }
}

impl<I> Scorer for ScorerImpl<I>
where
  I: DocIdSetIterator + CachedScoreHelper + 'static,
{
  fn scoring_doc_id(&mut self) -> Result<i32> {
    Ok(self.iterator.doc_id())
  }

  fn get_max_score(&mut self, _upto: i32) -> Result<f32> {
    Ok(f32::INFINITY)
  }

  fn has_two_phase_iterator(&self) -> TwoPhaseState {
    TwoPhaseState::No
  }
}

pub struct DocsIteratorImpl {
  score_docs: Vec<ScoreDoc>,
  index: Option<usize>,
  boost: f32,
  cached_score: Cell<f32>,
}

impl DocsIteratorImpl {
  fn new(score_docs: Vec<ScoreDoc>, boost: f32) -> Self {
    Self {
      score_docs,
      index: None,
      boost,
      cached_score: Cell::new(0f32),
    }
  }
}

impl crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions for DocsIteratorImpl {}
impl crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for DocsIteratorImpl {}

impl DocIdSetIterator for DocsIteratorImpl {
  fn doc_id(&self) -> i32 {
    let Some(index) = self.index else {
      return -1;
    };
    if index >= self.score_docs.len() {
      NO_MORE_DOCS
    } else {
      self
        .cached_score
        .set(self.boost * self.score_docs[index].score);
      self.score_docs[index].doc
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.index = Some(self.index.map_or(0, |index| index + 1));
    Ok(self.doc_id())
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    let pos = self
      .score_docs
      .binary_search_by_key(&target, |score_doc| score_doc.doc)
      .unwrap_or_else(|pos| pos);
    self.index = Some(pos);
    Ok(self.doc_id())
  }

  fn cost(&self) -> Result<i64> {
    Ok(self.score_docs.len() as i64)
  }
}
impl CachedScoreHelper for DocsIteratorImpl {
  fn cached_score(&self) -> f32 {
    self.cached_score.get()
  }
}
pub trait CachedScoreHelper {
  fn cached_score(&self) -> f32;
}

pub(crate) struct FilteredDocIdSetIteratorImpl1<V, B> {
  boost: f32,
  threshold: f32,
  cached_score: f32,
  base: FilteredDocIdSetIteratorBase<ConjunctionDISIWithBitSet<V, B>>,
}

impl<V, B> FilteredDocIdSetIteratorImpl1<V, B>
where
  V: VectorScorer,
  B: BitSet,
{
  fn new(
    vector_scorer: V,
    accept_docs: BitSetIterator<B>,
    boost: f32,
    threshold: f32,
  ) -> Result<Self> {
    let conjunction = ConjunctionDISI::create_conjunction(vec![
      DocIdSetIteratorEnum2::A(vector_scorer),
      DocIdSetIteratorEnum2::B(accept_docs),
    ])?;
    let base = FilteredDocIdSetIteratorBase::new(conjunction);
    Ok(Self {
      boost,
      threshold,
      cached_score: 0f32,
      base,
    })
  }
}

impl<V, B> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for FilteredDocIdSetIteratorImpl1<V, B>
where
  V: VectorScorer,
  B: BitSet,
{
}
impl<V, B> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for FilteredDocIdSetIteratorImpl1<V, B>
where
  V: VectorScorer,
  B: BitSet,
{
}

impl<V, B> DocIdSetIterator for FilteredDocIdSetIteratorImpl1<V, B>
where
  V: VectorScorer,
  B: BitSet,
{
  fn doc_id(&self) -> i32 {
    FilteredDocIdSetIterator::doc_id(self)
  }

  fn next_doc(&mut self) -> Result<i32> {
    FilteredDocIdSetIterator::next_doc(self)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    FilteredDocIdSetIterator::advance(self, target)
  }

  fn cost(&self) -> Result<i64> {
    FilteredDocIdSetIterator::cost(self)
  }
}
impl<V, B> FilteredDocIdSetIterator for FilteredDocIdSetIteratorImpl1<V, B>
where
  V: VectorScorer,
  B: BitSet,
{
  type DocIdSetIterator = ConjunctionDISIWithBitSet<V, B>;

  fn base(&self) -> &FilteredDocIdSetIteratorBase<Self::DocIdSetIterator> {
    &self.base
  }

  fn base_mut(&mut self) -> &mut FilteredDocIdSetIteratorBase<Self::DocIdSetIterator> {
    &mut self.base
  }

  fn match_(&mut self, doc: i32) -> Result<bool> {
    let vector_scorer = self.base.inner_iter.non_bit_set_iterator()?;
    debug_assert_eq!(vector_scorer.doc_id(), doc);
    let score = vector_scorer.score()?;
    self.cached_score = score * self.boost;
    Ok(score >= self.threshold)
  }
}
impl<V, B> CachedScoreHelper for FilteredDocIdSetIteratorImpl1<V, B>
where
  V: VectorScorer,
  B: BitSet,
{
  fn cached_score(&self) -> f32 {
    self.cached_score
  }
}
