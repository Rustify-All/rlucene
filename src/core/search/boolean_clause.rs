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
use crate::core::search::constant_score_query::ConstantScoreQuery;
use crate::core::search::query::{BaseQuery, QueryBase};

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub enum BooleanClauseQuery {
    Base(BaseQuery),
    ConstantScore(ConstantScoreQuery),
}

impl BooleanClauseQuery {
    pub fn as_string(&self, field: &str) -> String {
        match self {
            BooleanClauseQuery::Base(query) => query.as_string(field),
            BooleanClauseQuery::ConstantScore(query) => query.as_string(field),
        }
    }
}

impl From<BaseQuery> for BooleanClauseQuery {
    fn from(value: BaseQuery) -> Self {
        BooleanClauseQuery::Base(value)
    }
}

impl From<ConstantScoreQuery> for BooleanClauseQuery {
    fn from(value: ConstantScoreQuery) -> Self {
        BooleanClauseQuery::ConstantScore(value)
    }
}

/// A clause in a BooleanQuery.
#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub struct BooleanClause {
    pub query: BooleanClauseQuery,
    pub occur: Occur,
}

impl BooleanClause {
    /// Constructs a BooleanClause.
    ///
    /// In Java this validated non-null arguments. In Rust, `Q` is a value type,
    /// so we just take ownership.
    pub fn new(query: BooleanClauseQuery, occur: Occur) -> Self {
        Self { query, occur }
    }

    pub fn is_prohibited(&self) -> bool {
        self.occur == Occur::MustNot
    }

    pub fn is_required(&self) -> bool {
        matches!(self.occur, Occur::Must | Occur::Filter)
    }

    pub fn is_scoring(&self) -> bool {
        matches!(self.occur, Occur::Must | Occur::Should)
    }
    pub fn occur(&self) -> &Occur {
        &self.occur
    }
}

/// Specifies how clauses are to occur in matching documents.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Occur {
    /// Use this operator for clauses that *must* appear in the matching documents.
    Must,

    /// Like [`Occur::Must`] except that these clauses do not participate in scoring.
    Filter,

    /// Use this operator for clauses that *should* appear in the matching documents.
    ///
    /// For a BooleanQuery with no `MUST` clauses one or more `SHOULD` clauses must match
    /// a document for the BooleanQuery to match.
    ///
    /// See also: `BooleanQuery::Builder::set_minimum_number_should_match`.
    Should,

    /// Use this operator for clauses that *must not* appear in the matching documents.
    ///
    /// Note that it is not possible to search for queries that only consist of a `MUST_NOT`
    /// clause. These clauses do not contribute to the score of documents.
    MustNot,
}

impl core::fmt::Display for Occur {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Occur::Must => write!(f, "+"),
            Occur::Filter => write!(f, "#"),
            Occur::Should => write!(f, ""),
            Occur::MustNot => write!(f, "-"),
        }
    }
}

impl Occur {
    /// Convenience mirror of Java helpers if you ever want them on `Occur` directly.
    pub fn is_required(self) -> bool {
        matches!(self, Occur::Must | Occur::Filter)
    }
    pub fn is_scoring(self) -> bool {
        matches!(self, Occur::Must | Occur::Should)
    }
    pub fn is_prohibited(self) -> bool {
        matches!(self, Occur::MustNot)
    }
    #[cfg(test)]
    pub const fn values() -> &'static [Occur] {
        &[Occur::Must, Occur::Filter, Occur::Should, Occur::MustNot]
    }
}
