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
use crate::core::index::index_reader::IndexReader;
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::query_timeout::QueryTimeout;
use crate::core::search::abstract_knn_vector_query::{
  ConjunctionDISIEnum, FilteredDocIdSetIteratorImpl,
};
use crate::core::search::conjunction_disi::{ConjunctionDISI, VectorScorerDisi};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::explanation::Explanation;
use crate::core::search::filtered_doc_id_set_iterator::{
  FilteredDocIdSetIterator, FilteredDocIdSetIteratorBase,
};
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::knn::knn_collector_manager::KnnCollectorManager;
use crate::core::search::query::{
  Query, QueryBase, QueryWeight, QueryWeightSs, QueryWeightSsBulkScorer, QueryWeightSsScorer,
};
use crate::core::search::scorable::{FixedScore, Scorable};
use crate::core::search::score_doc::ScoreDoc;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::{Scorer, TwoPhaseState};
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::time_limiting_knn_collector_manager::TimeLimitingKnnCollectorManager;
use crate::core::search::top_docs::TopDocs;
use crate::core::search::total_hits::Relation::EqualTo;
use crate::core::search::vector_scorer::VectorScorer;
use crate::core::search::vector_similarity_collector::VectorSimilarityCollector;
use crate::core::search::weight::{DefaultBulkScorer, Weight};
use crate::core::util::bit_set::{BitSet, of};
use crate::core::util::bit_set_iterator::BitSetIterator;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::cell::Cell;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Search for all (approximate) vectors above a similarity threshold.
pub trait AbstractVectorSimilarityQuery: QueryBase {
  fn base(&self) -> &AbstractVectorSimilarityQueryBase;

  fn create_weight<IRC>(self, searcher: &IndexSearcher<IRC>, boost: f32) -> Result<QueryWeight<IRC>>
  where
    IRC: IndexReaderContext,
    Self: Clone + Into<Query> + Send + Sync + Sized + 'static,
  {
    let filter_weight = match self.base().filter.as_ref() {
      Some(filter) => {
        let rewritten = searcher.rewrite((**filter).clone())?;
        Some(searcher.create_weight(rewritten, ScoreMode::CompleteNoScores, 1.0)?)
      },
      None => None,
    };

    Ok(Box::new(AbstractVectorSimilarityQueryWeight::new(
      self,
      filter_weight,
      searcher.get_timeout::<()>(),
      boost,
    )))
  }

  fn get_knn_collector_manager(&self) -> VectorSimilarityCollectorManager {
    VectorSimilarityCollectorManager::new(
      self.base().traversal_similarity,
      self.base().result_similarity,
    )
  }

  type VectorScorer<LR>: VectorScorer
  where
    LR: LeafReader;

  fn create_vector_scorer<LR>(
    &self,
    context: &LeafReaderContext<LR>,
  ) -> Result<Option<Self::VectorScorer<LR>>>
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
    self.traversal_similarity.to_bits().hash(state);
    self.result_similarity.to_bits().hash(state);
    self.filter.hash(state);
  }
}

impl PartialEq for AbstractVectorSimilarityQueryBase {
  fn eq(&self, other: &Self) -> bool {
    self.field == other.field
      && self.traversal_similarity.to_bits() == other.traversal_similarity.to_bits()
      && self.result_similarity.to_bits() == other.result_similarity.to_bits()
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

struct AbstractVectorSimilarityQueryWeight<Q, IRC, QT> {
  parent_query: Arc<Query>,
  query: Q,
  filter_weight: Option<QueryWeight<IRC>>,
  time_limiting_knn_collector_manager:
    TimeLimitingKnnCollectorManager<VectorSimilarityCollectorManager, QT>,
  boost: f32,
}

impl<Q, IRC, QT> AbstractVectorSimilarityQueryWeight<Q, IRC, QT>
where
  Q: AbstractVectorSimilarityQuery + Clone + Into<Query>,
  IRC: IndexReaderContext,
  QT: QueryTimeout,
  Q::VectorScorer<<IRC as IndexReaderContext>::LeafReader>: 'static,
{
  fn new(
    query: Q,
    filter_weight: Option<QueryWeight<IRC>>,
    query_timeout: Option<QT>,
    boost: f32,
  ) -> Self {
    let manager = query.get_knn_collector_manager();
    let parent_query = Arc::new(query.clone().into());
    Self {
      parent_query,
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

impl<Q, IRC, QT> SegmentCacheable<IRC> for AbstractVectorSimilarityQueryWeight<Q, IRC, QT>
where
  Q: AbstractVectorSimilarityQuery + Clone + Into<Query>,
  IRC: IndexReaderContext,
  QT: QueryTimeout,
{
  fn is_cacheable(&self, _ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    Ok(true)
  }
}

impl<Q, IRC, QT> Weight<IRC> for AbstractVectorSimilarityQueryWeight<Q, IRC, QT>
where
  Q: AbstractVectorSimilarityQuery + Clone + Into<Query>,
  IRC: IndexReaderContext,
  QT: QueryTimeout,
  Q::VectorScorer<<IRC as IndexReaderContext>::LeafReader>: 'static,
{
  fn explain(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Explanation> {
    if let Some(filter_weight) = &self.filter_weight {
      let mut filter_scorer = match filter_weight.scorer(context, searcher)? {
        Some(scorer) => scorer,
        None => {
          return Ok(Explanation::no_match_no_details(
            "Doc does not match the filter",
          ));
        },
      };
      if filter_scorer.iterator_mut().advance(doc)? > doc {
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
    let doc_id = {
      let mut iterator = scorer.iterator_mut();
      iterator.advance(doc)?
    };
    if doc_id == doc {
      let score = scorer.score()?;
      if score >= self.query.base().result_similarity {
        Ok(Explanation::match_(
          self.boost * score,
          "Score above threshold".to_string(),
          vec![],
        ))
      } else {
        Ok(Explanation::no_match_no_details("Score below threshold"))
      }
    } else {
      Ok(Explanation::no_match_no_details("No vector found for doc"))
    }
  }

  fn get_query(&self) -> Arc<Query> {
    self.parent_query.clone()
  }

  type ScorerSupplier = QueryWeightSs<IRC>;

  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::ScorerSupplier>> {
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
            .map(|supplier| Box::new(supplier) as QueryWeightSs<IRC>),
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
        let mut filtered =
          FilteredDocIdSetIteratorImpl::new(live_docs.as_ref(), scorer.take_iterator());
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
              .map(|supplier| Box::new(supplier) as QueryWeightSs<IRC>),
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
            .map(|supplier| Box::new(supplier) as QueryWeightSs<IRC>),
          )
        }
      },
    }
  }
}

struct VectorSimilarityScorerSupplier<I> {
  iterator: Option<I>,
}

impl<IRC, I> ScorerSupplier<IRC> for VectorSimilarityScorerSupplier<I>
where
  IRC: IndexReaderContext,
  I: DocIdSetIterator + CachedScoreHelper + 'static,
{
  type Scorer = QueryWeightSsScorer;
  type BulkScorer = QueryWeightSsBulkScorer;

  fn get(
    &mut self,
    _lead_cost: i64,
    _context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC>,
  ) -> Result<Self::Scorer> {
    let iterator = self
      .iterator
      .take()
      .ok_or_else(|| LuceneError::illegal_state("ScorerSupplier::get called more than once"))?;
    Ok(Box::new(ScorerImpl::new(iterator)))
  }

  fn bulk_scorer(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::BulkScorer>> {
    let scorer = self.get(i64::MAX, context, searcher)?;
    Ok(Some(Box::new(DefaultBulkScorer::new(scorer))))
  }

  fn cost(
    &mut self,
    _context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC>,
  ) -> Result<i64> {
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
    score_docs.sort_by_key(|score_doc| score_doc.doc);
    Some(Self {
      iterator: Some(DocsIteratorImpl::new(score_docs, boost)),
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
  fn score(&mut self) -> Result<f32> {
    Ok(self.iterator.cached_score())
  }
}

impl<I> FixedScore for ScorerImpl<I> {}

impl<I> Scorer for ScorerImpl<I>
where
  I: DocIdSetIterator + CachedScoreHelper + 'static,
{
  fn doc_id(&mut self) -> Result<i32> {
    Ok(self.iterator.doc_id())
  }

  fn iterator(&self) -> Box<dyn DocIdSetIterator + '_> {
    Box::new(&self.iterator)
  }

  fn iterator_mut(&mut self) -> Box<dyn DocIdSetIterator + '_> {
    Box::new(&mut self.iterator)
  }

  fn take_iterator(self: Box<Self>) -> Box<dyn DocIdSetIterator> {
    Box::new(self.iterator)
  }

  fn get_max_score(&mut self, _upto: i32) -> Result<f32> {
    Ok(f32::INFINITY)
  }

  fn has_two_phase_iterator(&self) -> TwoPhaseState {
    TwoPhaseState::No
  }

  fn approximation(&self) -> Box<dyn DocIdSetIterator + '_> {
    Box::new(&self.iterator)
  }

  fn approximation_mut(&mut self) -> Box<dyn DocIdSetIterator + '_> {
    Box::new(&mut self.iterator)
  }
}

pub struct DocsIteratorImpl {
  score_docs: Vec<ScoreDoc>,
  index: i32,
  boost: f32,
  cached_score: Cell<f32>,
}

impl DocsIteratorImpl {
  fn new(score_docs: Vec<ScoreDoc>, boost: f32) -> Self {
    Self {
      score_docs,
      index: -1,
      boost,
      cached_score: Cell::new(0f32),
    }
  }
}

impl DocIdSetIterator for DocsIteratorImpl {
  fn doc_id(&self) -> i32 {
    if self.index < 0 {
      -1
    } else if self.index as usize >= self.score_docs.len() {
      NO_MORE_DOCS
    } else {
      let index = self.index as usize;
      self
        .cached_score
        .set(self.boost * self.score_docs[index].score);
      self.score_docs[index].doc
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.index += 1;
    Ok(self.doc_id())
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    let pos = self
      .score_docs
      .binary_search_by_key(&target, |score_doc| score_doc.doc)
      .unwrap_or_else(|pos| pos);
    self.index = pos as i32;
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

struct FilteredDocIdSetIteratorImpl1<V, B> {
  boost: f32,
  threshold: f32,
  cached_score: f32,
  base: FilteredDocIdSetIteratorBase<ConjunctionDISI<ConjunctionDISIEnum<B, V>>>,
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
    let vector_iterator = VectorScorerDisi::new(vector_scorer);
    let conjunction = ConjunctionDISI::from_disi(vec![
      ConjunctionDISIEnum::VectorScorer(vector_iterator),
      ConjunctionDISIEnum::Bit(accept_docs),
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
  type DocIdSetIterator = ConjunctionDISI<ConjunctionDISIEnum<B, V>>;

  fn base(&self) -> &FilteredDocIdSetIteratorBase<Self::DocIdSetIterator> {
    &self.base
  }

  fn base_mut(&mut self) -> &mut FilteredDocIdSetIteratorBase<Self::DocIdSetIterator> {
    &mut self.base
  }

  fn match_(&mut self, doc: i32) -> Result<bool> {
    let score = match &self.base.inner_iter.all_disi[0] {
      ConjunctionDISIEnum::VectorScorer(vector_scorer) => {
        debug_assert_eq!(vector_scorer.doc_id(), doc);
        vector_scorer.score()?
      },
      _ => {
        return Err(LuceneError::illegal_state(
          "expected vector scorer to be first in conjunction",
        ));
      },
    };
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
