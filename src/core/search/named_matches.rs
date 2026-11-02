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
use crate::core::search::matches::Matches;
use crate::core::search::query::QueryRef;
use crate::core::search::query::{
  IntoBoxQuery, Query, QueryBase, QueryWeight, QueryWeightMatches, QueryWeightMatchesIterator,
  QueryWeightSs,
};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::weight::Weight;
use crate::core::util::accountable::Accountable;
use crate::core::util::core_helper::HasIdentity;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED;
use std::borrow::Cow;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Utility type to help extract the set of sub queries that have matched from a larger query.
///
/// Individual subqueries may be wrapped using [`NamedMatches::wrap_query`], and the matching
/// queries for a particular document can then be pulled from the parent query's [`Matches`]
/// value by calling [`NamedMatches::find_named_matches`].
pub struct NamedMatches<
  'a,
  IRC: IndexReaderContext + 'static = LeafReaderContext<
    crate::core::index::dummy::dummy_leaf_reader::DummyLeafReader,
  >,
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> where
  SIM: Similarity,
{
  in_: QueryWeightMatches<'a, IRC, SIM>,
  name: Cow<'a, str>,
}

impl<'a, IRC: IndexReaderContext + 'static, SIM> NamedMatches<'a, IRC, SIM>
where
  SIM: Similarity,
{
  /// Wraps a [`Matches`] value and associates a name with it.
  pub fn new<FName>(name: FName, in_: QueryWeightMatches<'a, IRC, SIM>) -> Self
  where
    FName: Into<Cow<'a, str>>,
  {
    let name = name.into();
    Self { in_, name }
  }

  /// Returns the name of this [`Matches`] value.
  pub fn get_name(&self) -> &str {
    &self.name
  }
}

impl<'a, IRC: IndexReaderContext + 'static, SIM> Matches for NamedMatches<'a, IRC, SIM>
where
  SIM: Similarity,
{
  type Iterator<'b>
    = QueryWeightMatchesIterator<'b, IRCLeafReader<IRC>, SIM>
  where
    Self: 'b;
  type SubMatches = QueryWeightMatches<'a, IRC, SIM>;
  fn get_matches(
    &self,
    field: &str,
  ) -> Result<Option<QueryWeightMatchesIterator<'_, IRCLeafReader<IRC>, SIM>>> {
    self.in_.get_matches(field)
  }

  fn get_sub_matches(&self) -> &[QueryWeightMatches<'a, IRC, SIM>] {
    std::slice::from_ref(&self.in_)
  }

  fn field(&self) -> &[String] {
    self.in_.field()
  }
}

impl NamedMatches<'_> {
  /// Wrap a query so that it associates a name with its [`Matches`] value.
  pub fn wrap_query<N, Q>(name: N, in_: Q) -> Query
  where
    N: Into<String>,
    Q: IntoBoxQuery,
  {
    NamedQuery::new(Arc::new(name.into()), in_.into_box_query()).into()
  }
}
impl<IRC: IndexReaderContext + 'static, SIM> NamedMatches<'_, IRC, SIM>
where
  SIM: Similarity,
{
  /// Finds all [`NamedMatches`] in a [`Matches`] tree.
  pub fn find_named_matches<'a, 'b>(
    matches: &'b QueryWeightMatches<'a, IRC, SIM>,
  ) -> Vec<&'b NamedMatches<'a, IRC, SIM>>
  where
    'a: 'b,
  {
    let mut named_matches = Vec::new();
    if let QueryWeightMatches::NamedMatches(matches) = matches {
      named_matches.push(matches.as_ref());
    }
    let mut to_process = VecDeque::new();
    to_process.extend(matches.get_sub_matches());
    while let Some(matches) = to_process.pop_front() {
      if let QueryWeightMatches::NamedMatches(matches) = matches {
        named_matches.push(matches.as_ref());
      }
      to_process.extend(matches.get_sub_matches());
    }
    named_matches
  }
}

#[derive(Clone, Debug)]
pub struct NamedQuery {
  id: Identity,
  name: Arc<String>,
  in_: Box<Query>,
}

impl NamedQuery {
  fn new(name: Arc<String>, in_: Box<Query>) -> Self {
    Self {
      id: Identity::new(),
      name,
      in_,
    }
  }
}

impl QueryBase for NamedQuery {
  type Weight<IRC, SIM>
    = NamedWeight<IRC, SIM>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static;

  fn create_weight<IRC, SIM>(
    self,
    searcher: &IndexSearcher<IRC, SIM>,
    score_mode: &ScoreMode,
    boost: f32,
  ) -> Result<Self::Weight<IRC, SIM>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync,
    Self: Sized,
  {
    let in_ = self.in_.create_weight(searcher, score_mode, boost)?;
    Ok(NamedWeight::<_, SIM> {
      in_,
      name: self.name,
    })
  }

  fn rewrite<IRC, SIM>(&self, searcher: &IndexSearcher<IRC, SIM>) -> Result<Option<Query>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync,
    Self: Sized,
  {
    match self.in_.rewrite(searcher)? {
      Some(rewritten) => Ok(Some(
        NamedQuery::new(self.name.clone(), Box::new(rewritten)).into(),
      )),
      None => Ok(None),
    }
  }

  fn to_string(&self, field: &str) -> Result<String> {
    Ok(format!(
      "NamedQuery({},{})",
      self.name,
      self.in_.to_string(field)?
    ))
  }

  fn visit<QV>(&self, visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    let query = self.into();
    let mut visitor = visitor.get_sub_visitor(Occur::Must, query);
    self.in_.visit(&mut visitor)
  }
}

impl PartialEq for NamedQuery {
  fn eq(&self, other: &Self) -> bool {
    self.name == other.name && self.in_ == other.in_
  }
}

impl Eq for NamedQuery {}

impl Hash for NamedQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.name.hash(state);
    self.in_.hash(state);
  }
}

impl HasIdentity for NamedQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl Accountable for NamedQuery {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(QUERY_DEFAULT_RAM_BYTES_USED)
  }
}

pub struct NamedWeight<
  IRC: IndexReaderContext + 'static,
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> where
  SIM: Similarity,
{
  in_: QueryWeight<IRC, SIM>,
  name: Arc<String>,
}

impl<IRC, SIM> SegmentCacheable<IRC> for NamedWeight<IRC, SIM>
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
{
  fn is_cacheable(&self, ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    self.in_.is_cacheable(ctx)
  }
}

impl<IRC, SIM> Weight<IRC, SIM> for NamedWeight<IRC, SIM>
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
{
  fn matches<'a>(
    &'a self,
    context: &'a LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &'a IndexSearcher<IRC, SIM>,
  ) -> Result<Option<QueryWeightMatches<'a, IRC, SIM>>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let Some(matches) = self.in_.matches(context, doc, searcher)? else {
      return Ok(None);
    };
    Ok(Some(QueryWeightMatches::NamedMatches(Box::new(
      NamedMatches::new(self.name.as_str(), matches),
    ))))
  }

  fn explain(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Explanation>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    self.in_.explain(context, doc, searcher)
  }

  fn get_query(&self) -> QueryRef<'_> {
    self.in_.get_query()
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
    self.in_.scorer_supplier(context, searcher)
  }
}
