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
/// A [`Query`] wrapper that allows giving a boost to the wrapped query.
///
/// Boost values that are less than one will give less importance to this query
/// compared to other ones, while values that are greater than one will give
/// more importance to the scores returned by this query.
///
///
/// More complex boosts can be applied by using `FunctionScoreQuery` in the
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader::{LRTermState, LeafReader};
use crate::core::index::term_states::TermStates;
use crate::core::search::QueryCache;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::query::{BaseQuery, Query, QueryBase};
use crate::core::search::constant_score_query::BaseQueryWeight;
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::util::core_helper::HasIdentity;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::hash::{Hash, Hasher};

#[derive(Debug, Clone)]
pub struct BoostQuery {
    id: Identity,
    pub(crate) query: Box<BaseQuery>,
    boost: f32,
}
impl BoostQuery {
    pub fn new<T>(query: T, boost: f32) -> Result<Self>
    where
        T: Into<Box<BaseQuery>>,
    {
        let query = query.into();
        if !boost.is_finite() || boost < 0.0 {
            return Err(LuceneError::illegal_argument(format!(
                "boost must be a positive float, got {}",
                boost
            )));
        }
        Ok(Self {
            id: Identity::new(),
            query,
            boost,
        })
    }
    pub fn get_query(&self) -> &BaseQuery {
        &self.query
    }
    pub fn get_boost(&self) -> f32 {
        self.boost
    }
}

impl PartialEq for BoostQuery {
    fn eq(&self, other: &Self) -> bool {
        self.boost.to_bits() == other.boost.to_bits() && self.query == other.query
    }
}
impl Eq for BoostQuery {}
impl QueryBase for BoostQuery {
    fn as_string(&self, field: &str) -> String {
        let inner = self.query.as_string(field);
        let mut s = String::new();
        s.push('(');
        s.push_str(&inner);
        s.push(')');
        s.push('^');
        s.push_str(&self.boost.to_string());
        s
    }

    type Weight<LR, QC>
        = BaseQueryWeight<LR>
    where
        LR: LeafReader,
        QC: QueryCache;

    fn create_weight<IRC, QC>(
        self,
        searcher: &IndexSearcher<IRC, QC>,
        score_mode: &ScoreMode,
        boost: f32,
        per_reader_term_state: Option<TermStates<LRTermState<IRCLeafReader<IRC>>>>,
    ) -> Result<Self::Weight<IRCLeafReader<IRC>, QC>>
    where
        IRC: IndexReaderContext,
        QC: QueryCache,
        Self: Sized,
    {
        self.query.create_weight(
            searcher,
            score_mode,
            self.boost * boost,
            per_reader_term_state,
        )
    }

    fn rewrite<IRC, QC>(mut self, searcher: &IndexSearcher<IRC, QC>) -> Result<Query>
    where
        IRC: IndexReaderContext,
        QC: QueryCache,
        Self: Sized,
    {
        let query_id = self.query.identity().clone();

        let rewritten = self.query.rewrite(searcher)?;

        if self.boost == 1.0 {
            return Ok(rewritten);
        }

        let rewritten_base = match rewritten {
            Query::Boost(in_boost) => {
                return Ok(BoostQuery::new(in_boost.query, self.boost * in_boost.boost)?.into());
            },
            Query::MatchNoDoc(_) => {
                return Ok(rewritten);
            },
            Query::ConstantScore(cs) => cs.into_inner(),
            other => BaseQuery::try_from(other)?,
        };

        if self.boost == 0.0 {
            return Ok(BoostQuery::new(Box::new(rewritten_base), 0.0)?.into());
        }

        if &query_id != rewritten_base.identity() {
            return Ok(BoostQuery::new(Box::new(rewritten_base), self.boost)?.into());
        }
        self.query = Box::new(rewritten_base);
        Ok(self.into())
    }

    fn visit<QV>(&self, _visitor: &QV)
    where
        QV: QueryVisitor,
    {
        todo!()
    }
}

impl Hash for BoostQuery {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.query.hash(state);
        self.boost.to_bits().hash(state);
    }
}

impl HasIdentity for BoostQuery {
    fn identity(&self) -> &Identity {
        &self.id
    }
}
