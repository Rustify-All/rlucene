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

use crate::core::index::field_info::FieldInfo;
use crate::core::index::index_reader::{Identity, IndexReader};
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::query_timeout::QueryTimeout;
use crate::core::search::approximation_disi::ApproximationBitSetAccess;
use crate::core::search::approximation_disi::ApproximationDISI;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::boolean_query::Builder;
use crate::core::search::conjunction_disi::ConjunctionDISI;
use crate::core::search::doc_id_set_iterator::BitSetIteratorAccess;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::DocIdSetIteratorEnum2;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::explanation::Explanation;
use crate::core::search::field_exists_query::FieldExistsQuery;
use crate::core::search::filtered_doc_id_set_iterator::{
  FilteredDocIdSetIterator, FilteredDocIdSetIteratorBase,
};
use crate::core::search::hit_queue;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::knn::knn_collector_manager::KnnCollectorManager;
use crate::core::search::knn::top_knn_collector_manager::TopKnnCollectorManager;
use crate::core::search::match_no_docs_query::MatchNoDocsQuery;
use crate::core::search::query::{Query, QueryBase};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::scorable::{FixedScore, Scorable};
use crate::core::search::score_doc::{ScoreDoc, ScoreDocLike};
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::{Scorer, TwoPhaseState};
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::time_limiting_knn_collector_manager::TimeLimitingKnnCollectorManager;
use crate::core::search::top_docs::TopDocs;
use crate::core::search::top_docs::merge_top_docs;
use crate::core::search::top_docs_collector::EMPTY_TOP_DOCS;
use crate::core::search::total_hits::Relation::{EqualTo, GreaterThanOrEqualTo};
use crate::core::search::total_hits::TotalHits;
use crate::core::search::two_phase_iterator::TwoPhaseIterator;
use crate::core::search::vector_scorer::VectorScorer;
use crate::core::search::weight::{DefaultScorerSupplier, Weight};
use crate::core::util::bit_set::{BitSet, SparseFixedBitSetBitSet, of};
use crate::core::util::bit_set_iterator::BitSetIterator;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{CoreHelper, HasIdentity};
use std::borrow::Borrow;
use std::fmt::Debug;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::LazyLock;

pub static NO_RESULTS: LazyLock<TopDocs<ScoreDoc>> = LazyLock::new(|| EMPTY_TOP_DOCS.clone());

pub struct AbstractKnnVectorQueryDefaults;

impl AbstractKnnVectorQueryDefaults {
  pub fn exact_search<Q, LR, T, QT>(
    query: &Q,
    context: &LeafReaderContext<LR>,
    accept_iterator: BitSetIterator<T>,
    query_timeout: Option<&QT>,
  ) -> Result<TopDocs<ScoreDoc>>
  where
    Q: AbstractKnnVectorQuery + ?Sized,
    LR: LeafReader,
    T: BitSet,
    QT: QueryTimeout,
  {
    let field_infos = context.reader().get_field_infos()?;
    let fi = match field_infos.field_info_by_name(&query.base().field)? {
      Some(fi) => fi,
      None => {
        // The field does not exist or does not index vectors
        return Ok(NO_RESULTS.clone());
      },
    };
    if fi.get_vector_dimension() == 0 {
      return Ok(NO_RESULTS.clone());
    }

    let vector_scorer = match query.create_vector_scorer(context, fi.as_ref())? {
      Some(vector_scorer) => vector_scorer,
      None => {
        return Ok(NO_RESULTS.clone());
      },
    };

    let cost = accept_iterator.cost()? as usize;
    let queue_size = query.base().k.min(cost);
    let mut queue = hit_queue::new(queue_size, true)?;
    let mut relation = EqualTo;
    let mut top_doc = queue
      .top_mut()
      .ok_or_else(|| LuceneError::illegal_state("top is None"))?;

    let mut conjunction = ConjunctionDISI::create_conjunction(vec![
      DocIdSetIteratorEnum2::A(vector_scorer),
      DocIdSetIteratorEnum2::B(accept_iterator),
    ])?;

    loop {
      let doc = conjunction.next_doc()?;
      if doc == NO_MORE_DOCS {
        break;
      }

      if query_timeout.is_some_and(|qt| qt.should_exit()) {
        relation = GreaterThanOrEqualTo;
        break;
      }
      let vector_scorer = conjunction.non_bit_set_iterator()?;
      debug_assert_eq!(vector_scorer.doc_id(), doc);
      let score = vector_scorer.score()?;
      if score > top_doc.score {
        top_doc.score = score;
        top_doc.doc = doc;
        top_doc = queue.update_top()?;
      }
    }

    while queue.size() > 0
      && queue
        .top()
        .ok_or_else(|| LuceneError::illegal_state("top is None"))?
        .score
        < 0.0
    {
      queue.pop()?;
    }

    let mut top_score_docs = vec![ScoreDoc::default(); queue.size()];
    for i in (0..top_score_docs.len()).rev() {
      top_score_docs[i] = queue
        .pop()?
        .ok_or_else(|| LuceneError::illegal_state("top is None"))?;
    }

    let total_hits = TotalHits::new(cost, relation);
    Ok(TopDocs::new(total_hits, top_score_docs))
  }
}
/// Uses [`KnnVectorsReader::search_f32`](crate::core::codecs::knn_vectors_reader::KnnVectorsReader::search_f32)
/// or [`KnnVectorsReader::search_u8`](crate::core::codecs::knn_vectors_reader::KnnVectorsReader::search_u8)
/// to perform nearest neighbour search, depending on the vector encoding.
///
/// This query also allows for performing a kNN search subject to a filter. In this case, it first
/// executes the filter for each leaf, then chooses a strategy dynamically:
///
/// - If the filter cost is less than `k`, just execute an exact search
/// - Otherwise run a kNN search subject to the filter
/// - If the kNN search visits too many vectors without completing, stop and run an exact search
pub trait AbstractKnnVectorQuery: QueryBase + Sync {
  fn base(&self) -> &AbstractKnnVectorQueryBase;
  fn rewrite<IRC, SIM>(&self, index_searcher: &IndexSearcher<IRC, SIM>) -> Result<Query>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync,
    Self: Sized,
  {
    let filter_weight = if let Some(filter) = self.base().filter.as_ref() {
      let filter = (**filter).clone();
      let mut builder = Builder::new();
      builder.add(filter, Occur::Filter)?;
      builder.add(FieldExistsQuery::new(&self.base().field), Occur::Filter)?;
      let rewritten = index_searcher.rewrite(builder.build())?;
      Some(index_searcher.create_weight(rewritten, ScoreMode::CompleteNoScores, 1.0)?)
    } else {
      None
    };

    let kcm = self.get_knn_collector_manager(self.base().k, index_searcher)?;
    let knn_collector_manager =
      TimeLimitingKnnCollectorManager::new(kcm, index_searcher.get_timeout());

    let leaf_reader_contexts = index_searcher.get_leaf_contexts()?;
    let query = self;
    let tasks = leaf_reader_contexts.iter().map(|ctx| {
      let ctx_ord = ctx.ord;
      let filter_weight = filter_weight.as_ref();
      let knn_collector_manager = &knn_collector_manager;
      move || {
        query.search_leaf(
          &index_searcher.get_leaf_contexts()?[ctx_ord],
          filter_weight,
          knn_collector_manager,
          index_searcher,
        )
      }
    });
    let per_leaf_results = index_searcher.get_task_executor().invoke_all(tasks)?;

    let top_k = merge_leaf_results(self.base().k, per_leaf_results)?;

    if top_k.score_docs.is_empty() {
      return Ok(MatchNoDocsQuery::new().into());
    }
    let id = index_searcher.get_top_reader_context().base().id().clone();
    create_rewritten_query(leaf_reader_contexts, top_k, id)
  }
  fn search_leaf<IRC, K, Q, W, SIM>(
    &self,
    ctx: &LeafReaderContext<IRCLeafReader<IRC>>,
    filter_weight: Option<&W>,
    time_limiting_knn_collector_manager: &TimeLimitingKnnCollectorManager<K, Q>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<TopDocs<ScoreDoc>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    K: KnnCollectorManager,
    Q: QueryTimeout,
    W: Weight<IRC, SIM>,
    IndexSearcher<IRC, SIM>: Sync,
  {
    let mut results = self.get_leaf_results(
      ctx,
      filter_weight,
      time_limiting_knn_collector_manager,
      searcher,
    )?;

    if ctx.doc_base > 0 {
      for score_doc in &mut results.score_docs {
        score_doc.doc += ctx.doc_base as i32;
      }
    }

    Ok(results)
  }
  fn get_leaf_results<IRC, K, Q, W, SIM>(
    &self,
    ctx: &LeafReaderContext<IRCLeafReader<IRC>>,
    filter_weight: Option<&W>,
    time_limiting_knn_collector_manager: &TimeLimitingKnnCollectorManager<K, Q>,
    search: &IndexSearcher<IRC, SIM>,
  ) -> Result<TopDocs<ScoreDoc>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    K: KnnCollectorManager,
    Q: QueryTimeout,
    W: Weight<IRC, SIM>,
    IndexSearcher<IRC, SIM>: Sync,
  {
    let reader = ctx.reader();
    let live_docs = reader.get_live_docs()?;

    let filter_weight = match filter_weight {
      None => {
        return self.approximate_search(
          ctx,
          live_docs.as_ref(),
          i32::MAX as usize,
          time_limiting_knn_collector_manager,
        );
      },
      Some(filter_weight) => filter_weight,
    };

    let mut scorer = match filter_weight.scorer(ctx, search)? {
      Some(scorer) => scorer,
      None => return Ok(NO_RESULTS.clone()),
    };

    let accept_docs = create_bit_set(&mut scorer, live_docs.as_ref(), reader.max_doc()?)?;
    let cost = accept_docs.cardinality();
    let query_timeout = time_limiting_knn_collector_manager.get_query_timeout();

    if cost <= self.base().k {
      // If there are <= k possible matches, short-circuit and perform exact search, since HNSW
      // must always visit at least k documents
      return self.exact_search(
        ctx,
        BitSetIterator::new(accept_docs, cost as i64)?,
        query_timeout,
      );
    }
    // Perform the approximate kNN search
    // We pass cost + 1 here to account for the edge case when we explore exactly cost vectors
    let results = self.approximate_search(
      ctx,
      Some(&accept_docs),
      cost + 1,
      time_limiting_knn_collector_manager,
    )?;

    if results.total_hits.relation() == EqualTo || query_timeout.is_some_and(|qt| qt.should_exit())
    {
      Ok(results)
    } else {
      self.exact_search(
        ctx,
        BitSetIterator::new(accept_docs, cost as i64)?,
        query_timeout,
      )
    }
  }

  type KnnCollectorManager: KnnCollectorManager + Sync;

  fn get_knn_collector_manager<IRC, SIM>(
    &self,
    k: usize,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Self::KnnCollectorManager>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync;
  fn default_get_knn_collector_manager<IRC, SIM>(
    &self,
    k: usize,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<TopKnnCollectorManager>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync,
  {
    TopKnnCollectorManager::new(k, searcher)
  }

  fn approximate_search<LR, B, K>(
    &self,
    context: &LeafReaderContext<LR>,
    accept_docs: Option<B>,
    visited_limit: usize,
    knn_collector_manager: &K,
  ) -> Result<TopDocs<ScoreDoc>>
  where
    LR: LeafReader,
    B: Bits,
    K: KnnCollectorManager;

  type VectorScorer<LR>: VectorScorer
  where
    LR: LeafReader;
  fn create_vector_scorer<LR>(
    &self,
    context: &LeafReaderContext<LR>,
    fi: &FieldInfo,
  ) -> Result<Option<Self::VectorScorer<LR>>>
  where
    LR: LeafReader;
  fn exact_search<LR, T, Q>(
    &self,
    context: &LeafReaderContext<LR>,
    accept_iterator: BitSetIterator<T>,
    query_timeout: Option<&Q>,
  ) -> Result<TopDocs<ScoreDoc>>
  where
    LR: LeafReader,
    T: BitSet,
    Q: QueryTimeout,
  {
    AbstractKnnVectorQueryDefaults::exact_search(self, context, accept_iterator, query_timeout)
  }
}
#[derive(Debug, Clone)]
pub struct AbstractKnnVectorQueryBase {
  pub(crate) field: String,
  pub(crate) k: usize,
  pub(crate) filter: Option<Box<Query>>,
}
impl AbstractKnnVectorQueryBase {
  pub fn new<FName>(field: FName, k: usize, filter: Option<Query>) -> Result<Self>
  where
    FName: Into<String>,
  {
    let field = field.into();
    if k < 1 {
      return Err(LuceneError::illegal_argument(format!(
        "k must be at least 1, got: {}",
        k
      )));
    }
    let filter = filter.map(Box::new);
    Ok(Self { field, k, filter })
  }
}
impl Hash for AbstractKnnVectorQueryBase {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.field.hash(state);
    self.k.hash(state);
    self.filter.hash(state);
  }
}
impl Eq for AbstractKnnVectorQueryBase {}
impl PartialEq for AbstractKnnVectorQueryBase {
  fn eq(&self, other: &Self) -> bool {
    self.k == other.k && self.field == other.field && self.filter == other.filter
  }
}

fn create_bit_set<D, B>(
  iterator: D,
  live_docs: Option<&B>,
  max_doc: i32,
) -> Result<SparseFixedBitSetBitSet>
where
  D: DocIdSetIterator,
  B: Bits,
{
  // TODO IMPORTANT 复用 Bitset 未实现
  let mut filter_iterator = FilteredDocIdSetIteratorImpl::new(live_docs, iterator);
  of(&mut filter_iterator, max_doc as usize)
}
/// Merges all segment-level kNN results to get the index-level kNN results.
///
/// The default implementation delegates to [`TopDocs::merge`] to find the
/// overall top `k`, which requires input results to be sorted.
///
/// This method is useful for reading and / or modifying the final results as needed.
///
/// # Arguments
///
/// * `per_leaf_results` - array of segment-level kNN results.
///
/// # Returns
///
/// index-level kNN results (no constraint on their ordering).
fn merge_leaf_results<S>(k: usize, per_leaf_results: Vec<TopDocs<S>>) -> Result<TopDocs<S>>
where
  S: ScoreDocLike,
{
  merge_top_docs(k, per_leaf_results)
}
fn create_rewritten_query<LR, S>(
  reader: &[LeafReaderContext<LR>],
  mut top_k: TopDocs<S>,
  id: Identity,
) -> Result<Query>
where
  LR: LeafReader,
  S: ScoreDocLike,
{
  let len = top_k.score_docs.len();
  debug_assert!(len > 0);

  let max_score = top_k.score_docs[0].score();

  top_k.score_docs.sort_unstable_by_key(|a| a.doc());

  let mut docs = Vec::with_capacity(len);
  let mut scores = Vec::with_capacity(len);

  for sd in top_k.score_docs.iter() {
    docs.push(sd.doc());
    scores.push(sd.score());
  }

  let segment_starts = find_segment_starts(reader, &docs)?;

  Ok(DocAndScoreQuery::new(docs, scores, max_score, segment_starts, id).into())
}
pub(crate) fn find_segment_starts<LR>(
  leaves: &[LeafReaderContext<LR>],
  docs: &[i32],
) -> Result<Vec<usize>>
where
  LR: LeafReader,
{
  let mut starts = Vec::with_capacity(leaves.len() + 1);
  if !leaves.is_empty() {
    starts.push(0);
  }

  let mut result_index: usize = 0;

  for leaf in leaves.iter().skip(1) {
    let upper = leaf.doc_base as i32;

    match docs[result_index..].binary_search(&upper) {
      Ok(pos) => {
        result_index += pos;
      },
      Err(pos) => {
        result_index += pos;
      },
    }

    starts.push(result_index);
  }
  starts.push(docs.len());

  Ok(starts)
}

pub struct FilteredDocIdSetIteratorImpl<'a, B, D> {
  live_docs: Option<&'a B>,
  base: FilteredDocIdSetIteratorBase<D>,
}
impl<'a, B, D> FilteredDocIdSetIteratorImpl<'a, B, D> {
  pub(crate) fn new(
    live_docs: Option<&'a B>,
    iterator: D,
  ) -> FilteredDocIdSetIteratorImpl<'a, B, D> {
    let base = FilteredDocIdSetIteratorBase::new(iterator);
    Self { live_docs, base }
  }
}

impl<B, D> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for FilteredDocIdSetIteratorImpl<'_, B, D>
where
  B: Bits,
  D: DocIdSetIterator,
{
}
impl<B, D> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for FilteredDocIdSetIteratorImpl<'_, B, D>
where
  B: Bits,
  D: DocIdSetIterator,
{
}

impl<B, D> DocIdSetIterator for FilteredDocIdSetIteratorImpl<'_, B, D>
where
  B: Bits,
  D: DocIdSetIterator,
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

impl<B, D> FilteredDocIdSetIterator for FilteredDocIdSetIteratorImpl<'_, B, D>
where
  B: Bits,
  D: DocIdSetIterator,
{
  type DocIdSetIterator = D;

  fn base(&self) -> &FilteredDocIdSetIteratorBase<Self::DocIdSetIterator> {
    &self.base
  }

  fn base_mut(&mut self) -> &mut FilteredDocIdSetIteratorBase<Self::DocIdSetIterator> {
    &mut self.base
  }

  fn match_(&mut self, doc: i32) -> Result<bool> {
    match self.live_docs {
      Some(ref v) => v.get(doc as usize),
      None => Ok(true),
    }
  }
}
#[derive(Debug)]
struct DocAndScoreArrays {
  docs: Vec<i32>,
  scores: Vec<f32>,
  segment_starts: Vec<usize>,
}

#[derive(Clone, Debug)]
struct DocAndScoreData(Arc<DocAndScoreArrays>);

impl Borrow<Vec<i32>> for DocAndScoreData {
  fn borrow(&self) -> &Vec<i32> {
    &self.0.docs
  }
}

/// Caches the results of a KnnVector search: a list of docs and their scores
#[derive(Clone, Debug)]
pub struct DocAndScoreQuery {
  data: DocAndScoreData,
  max_score: f32,
  context_identity: Identity,
  id: Identity,
}
impl DocAndScoreQuery {
  /// Creation method
  ///
  /// # Arguments
  ///
  /// * `docs` - the global docids of documents that match, in ascending order
  /// * `scores` - the scores of the matching documents
  /// * `max_score` - the maximum score
  /// * `segment_starts` - the indexes in docs and scores corresponding to the first matching
  ///   document in each segment. If a segment has no matching documents, it should be assigned
  ///   the index of the next segment that does. There should be a final entry that is always
  ///   docs.length-1.
  /// * `context_identity` - an object identifying the reader context that was used to build this
  ///   query
  pub fn new(
    docs: Vec<i32>,
    scores: Vec<f32>,
    max_score: f32,
    segment_starts: Vec<usize>,
    context_identity: Identity,
  ) -> Self {
    Self {
      data: DocAndScoreData(Arc::new(DocAndScoreArrays {
        docs,
        scores,
        segment_starts,
      })),
      max_score,
      context_identity,
      id: Identity::new(),
    }
  }
}

impl HasIdentity for DocAndScoreQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl QueryBase for DocAndScoreQuery {
  type Weight<IRC, SIM>
    = DocAndScoreQueryWeight
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static;

  fn to_string(&self, _field: &str) -> Result<String> {
    Ok(format!(
      "DocAndScoreQuery[{},...][{},...],{}",
      self.data.0.docs[0], self.data.0.scores[0], self.max_score
    ))
  }

  fn create_weight<IRC, SIM>(
    self,
    searcher: &IndexSearcher<IRC, SIM>,
    _score_mode: &ScoreMode,
    _boost: f32,
  ) -> Result<Self::Weight<IRC, SIM>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    Self: Sized,
    IndexSearcher<IRC, SIM>: Sync,
  {
    if searcher.get_top_reader_context().base().id() != &self.context_identity {
      return Err(LuceneError::illegal_state(
        "This DocAndScore query was created by a different reader",
      ));
    }
    Ok(DocAndScoreQueryWeight::new(self, _boost))
  }

  fn visit<QV>(&self, visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    visitor.visit_leaf(self.into())
  }
}
impl Eq for DocAndScoreQuery {}
impl PartialEq for DocAndScoreQuery {
  fn eq(&self, other: &Self) -> bool {
    self.context_identity == other.context_identity
      && (Arc::ptr_eq(&self.data.0, &other.data.0) || self.data.0.docs == other.data.0.docs)
      && CoreHelper::array_equals_f32(&self.data.0.scores, &other.data.0.scores)
  }
}
impl Hash for DocAndScoreQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.context_identity.hash(state);
    self.data.0.docs.hash(state);
    for f in self.data.0.scores.iter() {
      state.write_u32(BitUtil::float_to_int_bits(*f) as u32);
    }
  }
}

pub struct DocAndScoreQueryWeight {
  parent_query: DocAndScoreQuery,
  boost: f32,
}
impl DocAndScoreQueryWeight {
  pub fn new(query: DocAndScoreQuery, boost: f32) -> Self {
    Self {
      parent_query: query,
      boost,
    }
  }

  fn query(&self) -> Result<&DocAndScoreQuery> {
    Ok(&self.parent_query)
  }
}

impl<IRC> SegmentCacheable<IRC> for DocAndScoreQueryWeight
where
  IRC: IndexReaderContext + 'static,
{
  fn is_cacheable(&self, _ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    Ok(true)
  }
}

impl<IRC, SIM> Weight<IRC, SIM> for DocAndScoreQueryWeight
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
{
  fn explain(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Explanation>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let target = doc + context.doc_base as i32;

    let query = self.query()?;
    match query.data.0.docs.binary_search(&target) {
      Ok(found) => Ok(Explanation::match_(
        query.data.0.scores[found] * self.boost,
        format!("within top {} docs", query.data.0.docs.len()),
        vec![],
      )),
      Err(_) => Ok(Explanation::no_match_no_details(format!(
        "not in top {} docs",
        query.data.0.docs.len()
      ))),
    }
  }

  fn get_query(&self) -> QueryRef<'_> {
    QueryRef::DocAndScore(&self.parent_query)
  }

  type ScorerSupplier = DefaultScorerSupplier<ScorerImpl>;

  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Option<Self::ScorerSupplier>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let query = self.query()?;
    if query.data.0.segment_starts[context.ord] == query.data.0.segment_starts[context.ord + 1] {
      return Ok(None);
    }
    let disi = DocIdSetIteratorImpl::new(
      query.data.0.segment_starts[context.ord],
      query.data.0.segment_starts[context.ord + 1],
      query.data.clone(),
      context.doc_base,
    );
    let scorer = ScorerImpl::new(disi, query.max_score, self.boost);
    Ok(Some(DefaultScorerSupplier::new(scorer)))
  }

  fn count(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<i32>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let query = self.query()?;
    Ok(
      (query.data.0.segment_starts[context.ord + 1] - query.data.0.segment_starts[context.ord])
        as i32,
    )
  }
}

pub struct DocIdSetIteratorImpl<D = Arc<Vec<i32>>> {
  lower: usize,
  upper: usize,
  upto: Option<usize>,
  docs: D,
  doc_base: usize,
}
impl<D> DocIdSetIteratorImpl<D> {
  pub fn new(lower: usize, upper: usize, docs: D, doc_base: usize) -> Self {
    Self {
      lower,
      upper,
      upto: None,
      docs,
      doc_base,
    }
  }
}
impl<D: Borrow<Vec<i32>>> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for DocIdSetIteratorImpl<D>
{
}
impl<D: Borrow<Vec<i32>>> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for DocIdSetIteratorImpl<D>
{
}

impl<D: Borrow<Vec<i32>>> DocIdSetIterator for DocIdSetIteratorImpl<D> {
  fn doc_id(&self) -> i32 {
    doc_id_no_shadow(self.upto, self.upper, self.docs.borrow(), self.doc_base)
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self.upto {
      Some(ref mut v) => {
        *v += 1;
      },
      None => self.upto = Some(self.lower),
    }
    Ok(doc_id_no_shadow(
      self.upto,
      self.upper,
      self.docs.borrow(),
      self.doc_base,
    ))
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.slow_advance(target)
  }

  fn cost(&self) -> Result<i64> {
    Ok((self.upper - self.lower) as i64)
  }
}
fn doc_id_no_shadow(upto: Option<usize>, upper: usize, docs: &[i32], doc_base: usize) -> i32 {
  match upto {
    Some(v) => {
      if v >= upper {
        return NO_MORE_DOCS;
      }
      docs[v] - doc_base as i32
    },
    None => -1,
  }
}

pub struct ScorerImpl {
  disi: DocIdSetIteratorImpl<DocAndScoreData>,
  max_score: f32,
  boost: f32,
}

impl ScorerImpl {
  fn new(disi: DocIdSetIteratorImpl<DocAndScoreData>, max_score: f32, boost: f32) -> Self {
    Self {
      disi,
      max_score,
      boost,
    }
  }
}

impl Scorable for ScorerImpl {
  type Child<'a>
    = crate::core::search::dummy::dummy_scorable::DummyScorable
  where
    Self: 'a;

  fn score(&mut self) -> Result<f32> {
    let scores = &self.disi.docs.0.scores;
    let upto = self
      .disi
      .upto
      .filter(|&upto| upto < scores.len())
      .ok_or_else(|| LuceneError::array_index_out_of_bounds("upto is out of bounds"))?;
    Ok(scores[upto] * self.boost)
  }
}

impl FixedScore for ScorerImpl {}

impl DocIdSetIterator for ScorerImpl {
  fn doc_id(&self) -> i32 {
    DocIdSetIterator::doc_id(&self.disi)
  }
  fn next_doc(&mut self) -> Result<i32> {
    DocIdSetIterator::next_doc(&mut self.disi)
  }
  fn advance(&mut self, target: i32) -> Result<i32> {
    DocIdSetIterator::advance(&mut self.disi, target)
  }
  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    DocIdSetIterator::slow_advance(&mut self.disi, target)
  }
  fn cost(&self) -> Result<i64> {
    DocIdSetIterator::cost(&self.disi)
  }
}
impl crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions for ScorerImpl {}
impl crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for ScorerImpl {}
impl ApproximationDISI for ScorerImpl {
  fn approx_doc_id(&self) -> i32 {
    DocIdSetIterator::doc_id(&{ &self.disi })
  }
  fn approx_next_doc(&mut self) -> Result<i32> {
    DocIdSetIterator::next_doc(&mut { &mut self.disi })
  }
  fn approx_advance(&mut self, target: i32) -> Result<i32> {
    DocIdSetIterator::advance(&mut { &mut self.disi }, target)
  }
  fn approx_slow_advance(&mut self, target: i32) -> Result<i32> {
    DocIdSetIterator::slow_advance(&mut { &mut self.disi }, target)
  }
  fn approx_cost(&self) -> Result<i64> {
    DocIdSetIterator::cost(&{ &self.disi })
  }
}
impl ApproximationBitSetAccess for ScorerImpl {
  fn approx_is_bit_iter(&self) -> bool {
    BitSetIteratorAccess::is_bit_iter(&{ &self.disi })
  }
  fn approx_get(&self, index: usize) -> Result<bool> {
    BitSetIteratorAccess::get(&{ &self.disi }, index)
  }
  fn approx_set_doc_id(&mut self, doc: i32) -> Result<()> {
    BitSetIteratorAccess::set_doc_id(&mut { &mut self.disi }, doc)
  }
  fn approx_bit_set_length(&self) -> Result<usize> {
    BitSetIteratorAccess::bit_set_length(&{ &self.disi })
  }
}

impl TwoPhaseIterator for ScorerImpl {
  fn matches(&mut self) -> Result<bool> {
    Ok(true)
  }
  fn match_cost(&self) -> f32 {
    0.0
  }
}

impl Scorer for ScorerImpl {
  fn scoring_doc_id(&mut self) -> Result<i32> {
    Ok(DocIdSetIterator::doc_id(&self.disi))
  }

  fn get_max_score(&mut self, _upto: i32) -> Result<f32> {
    Ok(self.max_score * self.boost)
  }

  fn has_two_phase_iterator(&self) -> TwoPhaseState {
    TwoPhaseState::No
  }
}

impl crate::core::util::accountable::Accountable for DocAndScoreQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED)
  }
}
