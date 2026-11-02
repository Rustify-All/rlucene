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
use crate::core::search::similarities_impl::similarities::Similarity;
use std::fmt::Write as _;

use crate::core::index::index_reader::Identity;
/// A [`Query`] wrapper that allows giving a boost to the wrapped query.
///
/// Boost values that are less than one will give less importance to this query
/// compared to other ones, while values that are greater than one will give
/// more importance to the scores returned by this query.
///
///
/// More complex boosts can be applied by using `FunctionScoreQuery` in the
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::constant_score_query::ConstantScoreQuery;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::query::{IntoBoxQuery, Query, QueryBase, QueryWeight};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::core_helper::HasIdentity;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::borrow::Cow;
use std::hash::{Hash, Hasher};

#[derive(Debug, Clone)]
pub struct BoostQuery {
  id: Identity,
  query: Box<Query>,
  boost: f32,
}
impl BoostQuery {
  pub fn new<T>(query: T, boost: f32) -> Result<Self>
  where
    T: IntoBoxQuery,
  {
    let query = query.into_box_query();
    Self::check_boost(boost)?;
    Ok(Self {
      id: Identity::new(),
      query,
      boost,
    })
  }

  pub(crate) fn check_boost(boost: f32) -> Result<()> {
    if !boost.is_finite() || boost < 0.0 || (boost == 0.0 && boost.is_sign_negative()) {
      return Err(LuceneError::illegal_argument(format!(
        "boost must be a positive float, got {:.1}",
        boost
      )));
    }
    Ok(())
  }
  pub fn get_query(&self) -> &Query {
    &self.query
  }
  pub fn get_boost(&self) -> f32 {
    self.boost
  }
  pub fn into_inner(self) -> Query {
    *self.query
  }
}

impl PartialEq for BoostQuery {
  fn eq(&self, other: &Self) -> bool {
    BitUtil::float_to_int_bits(self.boost) == BitUtil::float_to_int_bits(other.boost)
      && self.query == other.query
  }
}
impl Eq for BoostQuery {}
impl QueryBase for BoostQuery {
  type Weight<IRC, SIM>
    = QueryWeight<IRC, SIM>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static;

  fn to_string(&self, field: &str) -> Result<String> {
    let mut s = self.query.to_string(field)?;
    s.insert(0, '(');
    s.push_str(")^");
    write!(s, "{:.1}", self.boost)?;
    Ok(s)
  }

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
    self
      .query
      .create_weight(searcher, score_mode, self.boost * boost)
  }

  fn rewrite<IRC, SIM>(&self, searcher: &IndexSearcher<IRC, SIM>) -> Result<Option<Query>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync,
    Self: Sized,
  {
    let rewritten = match self.query.rewrite(searcher)? {
      Some(query) => Cow::Owned(query),
      None => Cow::Borrowed(self.query.as_ref()),
    };
    if self.boost == 1.0 {
      return Ok(Some(rewritten.into_owned()));
    }
    let rewritten = match rewritten {
      Cow::Owned(Query::Boost(inner)) => {
        let boost = self.boost * inner.boost;
        return Ok(Some(BoostQuery::new(inner.query, boost)?.into()));
      },
      Cow::Borrowed(Query::Boost(inner)) => {
        return Ok(Some(
          BoostQuery::new(inner.query.clone(), self.boost * inner.boost)?.into(),
        ));
      },
      other => other,
    };
    if matches!(rewritten.as_ref(), Query::MatchNoDocs(_)) {
      return Ok(Some(rewritten.into_owned()));
    }
    if self.boost == 0.0 && !matches!(rewritten.as_ref(), Query::ConstantScore(_)) {
      return Ok(Some(
        BoostQuery::new(ConstantScoreQuery::new(rewritten.into_owned()), 0.0)?.into(),
      ));
    }
    if rewritten.identity() != self.query.identity() {
      return Ok(Some(
        BoostQuery::new(rewritten.into_owned(), self.boost)?.into(),
      ));
    }
    Ok(None)
  }

  fn visit<QV>(&self, visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    let query = self.into();
    let mut visitor = visitor.get_sub_visitor(Occur::Must, query);
    self.query.visit(&mut visitor)
  }
}

impl Hash for BoostQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.query.hash(state);
    (BitUtil::float_to_int_bits(self.boost) as u32).hash(state);
  }
}

impl HasIdentity for BoostQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl crate::core::util::accountable::Accountable for BoostQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED)
  }
}
