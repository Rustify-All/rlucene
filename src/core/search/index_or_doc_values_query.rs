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
use crate::core::index::index_reader::Identity;
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::explanation::Explanation;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::match_all_docs_query::MatchAllDocsQuery;
use crate::core::search::query::{
  IntoBoxQuery, Query, QueryBase, QueryWeight, QueryWeightSs, QueryWeightSsBulkScorer,
  QueryWeightSsScorer,
};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::weight::Weight;
use crate::core::util::HasIdentity;
use crate::core::util::error::lucene_error::Result;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
/// A query that uses either an index structure (points or terms) or doc values in order to run a
/// query, depending which one is more efficient. This is typically useful for range queries, whose
/// [`Weight::scorer`] is costly to create since it usually needs to sort large lists of doc ids.
/// For instance, for a field that both indexed [`LongPoint`](crate::core::document::long_point::LongPoint)s and
/// [`SortedNumericDocValuesField`](crate::core::document::sorted_numeric_doc_values_field::SortedNumericDocValuesField)s with the same values, an efficient range query could be
/// created by doing:
///
/// ```text
/// let pointQuery = LongPoint::new_range_query(field, minValue, maxValue);
/// let dvQuery = SortedNumericDocValuesField.new_slow_range_query(field, minValue, maxValue);
/// let query = new IndexOrDocValuesQuery(pointQuery, dvQuery);
/// ```
///
/// The above query will be efficient as it will use points in the case that they perform better,
/// ie. when we need a good lead iterator that will be almost entirely consumed; and doc values
/// otherwise, ie. in the case that another part of the query is already leading iteration but we
/// still need the ability to verify that some documents match.
///
/// Some field types that work well with [`IndexOrDocValuesQuery`] are
/// [`IntField`](crate::core::document::int_field::IntField), [`LongField`](crate::core::document::long_field::LongField),
/// [`FloatField`](crate::core::document::float_field::FloatField), [`DoubleField`](crate::core::document::double_field::DoubleField), and
/// [`KeywordField`](crate::core::document::keyword_field::KeywordField). These fields provide both an indexed structure
/// and doc values.
///
/// **NOTE** This query currently only works well with point range/exact queries and their
/// equivalent doc values queries.
#[derive(Clone, Debug)]
pub struct IndexOrDocValuesQuery {
  index_query: Box<Query>,
  dv_query: Box<Query>,
  id: Identity,
}
impl IndexOrDocValuesQuery {
  /// Create an [`IndexOrDocValuesQuery`]. Both provided queries must match the same documents
  /// and give the same scores.
  ///
  /// # Parameters
  ///
  /// - `index_query`: a query that has a good iterator but whose scorer may be costly to create
  /// - `dv_query`: a query whose scorer is cheap to create that can quickly check whether a given
  ///   document matches
  pub fn new<T1, T2>(index_query: T1, dv_query: T2) -> Self
  where
    T1: IntoBoxQuery,
    T2: IntoBoxQuery,
  {
    Self {
      index_query: index_query.into_box_query(),
      dv_query: dv_query.into_box_query(),
      id: Identity::new(),
    }
  }
  /// Return the wrapped query that may be costly to initialize but has a good iterator.
  pub fn get_index_query(&self) -> &Query {
    &self.index_query
  }
  /// Return the wrapped query that may be slow at identifying all matching documents,
  /// but which is cheap to initialize and can efficiently verify that some documents match.
  pub fn get_random_access_query(&self) -> &Query {
    &self.dv_query
  }
}

impl HasIdentity for IndexOrDocValuesQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl QueryBase for IndexOrDocValuesQuery {
  fn to_string(&self, field: &str) -> Result<String> {
    Ok(format!(
      "IndexOrDocValuesQuery(indexQuery={:?}, dvQuery={:?})",
      self.index_query.to_string(field),
      self.dv_query.to_string(field)
    ))
  }

  fn create_weight<IRC>(
    self,
    searcher: &IndexSearcher<IRC>,
    score_mode: &ScoreMode,
    boost: f32,
  ) -> Result<QueryWeight<IRC>>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    let query = self.clone();
    let index_weight = self
      .index_query
      .create_weight(searcher, score_mode, boost)?;
    let dv_weight = self.dv_query.create_weight(searcher, score_mode, boost)?;
    Ok(Box::new(IndexOrDocValuesQueryWeight::new(
      dv_weight,
      index_weight,
      query,
    )))
  }

  fn rewrite<IRC>(mut self, index_searcher: &IndexSearcher<IRC>) -> Result<Query>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    let index_rewrite_id = self.index_query.identity().clone();
    let dv_rewrite_id = self.dv_query.identity().clone();
    let index_rewrite = index_searcher.rewrite(*(self.index_query))?;
    let dv_rewrite = index_searcher.rewrite(*(self.dv_query))?;

    if matches!(index_rewrite, Query::MatchAllDocs(_))
      || matches!(dv_rewrite, Query::MatchAllDocs(_))
    {
      return Ok(MatchAllDocsQuery::new().into());
    }
    if &index_rewrite_id != index_rewrite.identity() || &dv_rewrite_id != dv_rewrite.identity() {
      Ok(IndexOrDocValuesQuery::new(index_rewrite, dv_rewrite).into())
    } else {
      self.index_query = Box::new(index_rewrite);
      self.dv_query = Box::new(dv_rewrite);
      Ok(self.into())
    }
  }

  fn visit<QV>(&self, visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    let query = self.into();
    let mut visitor = visitor.get_sub_visitor(Occur::Must, query);
    self.index_query.visit(&mut visitor)?;
    self.dv_query.visit(&mut visitor)
  }
}
impl PartialEq for IndexOrDocValuesQuery {
  fn eq(&self, other: &Self) -> bool {
    self.index_query == other.index_query && self.dv_query == other.dv_query
  }
}
impl Eq for IndexOrDocValuesQuery {}
impl Hash for IndexOrDocValuesQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.index_query.hash(state);
    self.dv_query.hash(state);
  }
}

pub struct IndexOrDocValuesQueryWeight<IRC> {
  dv_weight: QueryWeight<IRC>,
  index_weight: QueryWeight<IRC>,
  query: Arc<Query>,
}
impl<IRC> IndexOrDocValuesQueryWeight<IRC>
where
  IRC: IndexReaderContext,
{
  pub fn new(
    dv_weight: QueryWeight<IRC>,
    index_weight: QueryWeight<IRC>,
    query: IndexOrDocValuesQuery,
  ) -> Self {
    Self {
      dv_weight,
      index_weight,
      query: Arc::new(query.into()),
    }
  }
}

impl<IRC> SegmentCacheable<IRC> for IndexOrDocValuesQueryWeight<IRC>
where
  IRC: IndexReaderContext,
{
  fn is_cacheable(&self, ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    // Both index and dv query should return the same values, so we can use
    // the index query's cachehelper here
    self.index_weight.is_cacheable(ctx)
  }
}

impl<IRC> Weight<IRC> for IndexOrDocValuesQueryWeight<IRC>
where
  IRC: IndexReaderContext,
{
  fn matches<'a>(
    &'a self,
    context: &'a LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &'a IndexSearcher<IRC>,
  ) -> Result<Option<crate::core::search::query::QueryWeightMatches<'a>>> {
    self.dv_weight.matches(context, doc, searcher)
  }

  fn explain(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Explanation> {
    self.dv_weight.explain(context, doc, searcher)
  }

  fn get_query(&self) -> Arc<Query> {
    self.query.clone()
  }

  type ScorerSupplier = QueryWeightSs<IRC>;

  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::ScorerSupplier>> {
    let index_scorer_supplier = self.index_weight.scorer_supplier(context, searcher)?;
    let dv_scorer_supplier = self.dv_weight.scorer_supplier(context, searcher)?;

    let (index_scorer_supplier, dv_scorer_supplier) =
      match (index_scorer_supplier, dv_scorer_supplier) {
        (Some(index_scorer_supplier), Some(dv_scorer_supplier)) => {
          (index_scorer_supplier, dv_scorer_supplier)
        },
        _ => return Ok(None),
      };

    Ok(Some(Box::new(IndexOrDocValuesQuerySs::new(
      index_scorer_supplier,
      dv_scorer_supplier,
    ))))
  }

  fn count(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<i32> {
    let count = self.index_weight.count(context, searcher)?;
    if count != -1 {
      return Ok(count);
    }
    let count = self.dv_weight.count(context, searcher)?;
    Ok(count)
  }
}
pub struct IndexOrDocValuesQuerySs<IRC> {
  index_scorer_supplier: QueryWeightSs<IRC>,
  dv_scorer_supplier: QueryWeightSs<IRC>,
}
impl<IRC> IndexOrDocValuesQuerySs<IRC>
where
  IRC: IndexReaderContext,
{
  pub fn new(
    index_scorer_supplier: QueryWeightSs<IRC>,
    dv_scorer_supplier: QueryWeightSs<IRC>,
  ) -> Self {
    Self {
      index_scorer_supplier,
      dv_scorer_supplier,
    }
  }
}
impl<IRC> ScorerSupplier<IRC> for IndexOrDocValuesQuerySs<IRC>
where
  IRC: IndexReaderContext,
{
  type Scorer = QueryWeightSsScorer;
  type BulkScorer = QueryWeightSsBulkScorer;

  fn get(
    &mut self,
    lead_cost: i64,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Self::Scorer> {
    // At equal costs, doc values tend to be worse than points since they
    // still need to perform one comparison per document while points can
    // do much better than that given how values are organized. So we give
    // an arbitrary 8x penalty to doc values.
    let threshold = self.cost(context, searcher)? >> 3;
    if threshold <= lead_cost {
      self.index_scorer_supplier.get(lead_cost, context, searcher)
    } else {
      self.dv_scorer_supplier.get(lead_cost, context, searcher)
    }
  }

  fn bulk_scorer(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::BulkScorer>> {
    // Bulk scorers need to consume the entire set of docs, so using an
    // index structure should perform better
    self.index_scorer_supplier.bulk_scorer(context, searcher)
  }

  fn cost(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<i64> {
    self.index_scorer_supplier.cost(context, searcher)
  }
}

impl crate::core::util::accountable::Accountable for IndexOrDocValuesQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED)
  }
}
