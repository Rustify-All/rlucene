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
use crate::core::search::query::{
  IntoBoxQuery, Query, QueryBase, QueryWeight, QueryWeightMatches, QueryWeightMatchesIterator,
  QueryWeightSs,
};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::weight::Weight;
use crate::core::util::accountable::Accountable;
use crate::core::util::core_helper::HasIdentity;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Utility type to help extract the set of sub queries that have matched from a larger query.
///
/// Individual subqueries may be wrapped using [`NamedMatches::wrap_query`], and the matching
/// queries for a particular document can then be pulled from the parent query's [`Matches`]
/// value by calling [`NamedMatches::find_named_matches`].
pub struct NamedMatches<'a> {
  in_: QueryWeightMatches<'a>,
  name: String,
}

impl<'a> NamedMatches<'a> {
  /// Wraps a [`Matches`] value and associates a name with it.
  pub fn new(name: String, in_: QueryWeightMatches<'a>) -> Self {
    Self { in_, name }
  }

  /// Returns the name of this [`Matches`] value.
  pub fn get_name(&self) -> &str {
    &self.name
  }
}

impl Matches for NamedMatches<'_> {
  fn get_matches(&self, field: &str) -> Result<Option<QueryWeightMatchesIterator<'_>>> {
    self.in_.get_matches(field)
  }

  fn get_sub_matches(&self) -> Vec<&QueryWeightMatches<'_>> {
    vec![&self.in_]
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
    NamedQuery::new(name.into(), in_.into_box_query()).into()
  }

  /// Finds all [`NamedMatches`] in a [`Matches`] tree.
  pub fn find_named_matches<'a, 'b>(
    matches: &'b QueryWeightMatches<'a>,
  ) -> Vec<&'b NamedMatches<'b>>
  where
    'a: 'b,
  {
    let mut named_matches = Vec::new();
    let mut to_process = VecDeque::new();
    to_process.push_back(matches);
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
  name: String,
  in_: Box<Query>,
}

impl NamedQuery {
  fn new(name: String, in_: Box<Query>) -> Self {
    Self {
      id: Identity::new(),
      name,
      in_,
    }
  }
}

impl QueryBase for NamedQuery {
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
    let in_ = self.in_.create_weight(searcher, score_mode, boost)?;
    Ok(Box::new(NamedWeight {
      in_,
      name: self.name,
    }))
  }

  fn rewrite<IRC>(mut self, searcher: &IndexSearcher<IRC>) -> Result<Query>
  where
    IRC: IndexReaderContext,
    Self: Sized,
  {
    let query_id = self.in_.identity().clone();
    let rewritten = self.in_.rewrite(searcher)?;
    if rewritten.identity() != &query_id {
      return Ok(NamedQuery::new(self.name, Box::new(rewritten)).into());
    }
    self.in_ = Box::new(rewritten);
    Ok(self.into())
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

struct NamedWeight<IRC> {
  in_: QueryWeight<IRC>,
  name: String,
}

impl<IRC> SegmentCacheable<IRC> for NamedWeight<IRC>
where
  IRC: IndexReaderContext,
{
  fn is_cacheable(&self, ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    self.in_.is_cacheable(ctx)
  }
}

impl<IRC> Weight<IRC> for NamedWeight<IRC>
where
  IRC: IndexReaderContext,
{
  fn matches<'a>(
    &'a self,
    context: &'a LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &'a IndexSearcher<IRC>,
  ) -> Result<Option<QueryWeightMatches<'a>>> {
    let Some(matches) = self.in_.matches(context, doc, searcher)? else {
      return Ok(None);
    };
    Ok(Some(QueryWeightMatches::NamedMatches(Box::new(
      NamedMatches::new(self.name.clone(), matches),
    ))))
  }

  fn explain(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Explanation> {
    self.in_.explain(context, doc, searcher)
  }

  fn get_query(&self) -> Arc<Query> {
    self.in_.get_query()
  }

  type ScorerSupplier = QueryWeightSs<IRC>;

  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::ScorerSupplier>> {
    self.in_.scorer_supplier(context, searcher)
  }
}
