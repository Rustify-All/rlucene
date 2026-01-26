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
use crate::core::index::query_timeout::QueryTimeout;
use crate::core::index::term_states::TermStates;
use crate::core::search::QueryCache;
use crate::core::search::boolean_clause::{BooleanClause, Occur};
use crate::core::search::dummy::dummy_weight::DummyWeight;
use crate::core::search::index_searcher::{IndexSearcher, get_max_clause_count};
use crate::core::search::query::{Query, QueryBase};
use crate::core::search::query_caching_policy::QueryCachingPolicy;
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::{Hash, Hasher};

/// A query that matches documents matching boolean combinations of other queries, e.g.
/// [`TermQuery`](crate::core::search::term_query::TermQuery)s, [`PhraseQuery`](crate::core::search::phrase_query::PhraseQuery)s or other [`BooleanQuery`]s.
#[derive(Eq, PartialEq, Debug)]
pub struct BooleanQuery {
    minimum_number_should_match: i32,
    clauses: Vec<BooleanClause>,
    clause_sets: HashMap<Occur, Vec<Query>>,
}
#[cfg(test)]
impl Clone for BooleanQuery {
    fn clone(&self) -> Self {
        Self {
            minimum_number_should_match: self.minimum_number_should_match,
            clauses: self.clauses.clone(),
            clause_sets: self.clause_sets.clone(),
        }
    }
}

impl BooleanQuery {
    pub fn new(minimum_number_should_match: i32, clauses: Vec<BooleanClause>) -> BooleanQuery {
        BooleanQuery {
            minimum_number_should_match,
            clauses,
            clause_sets: HashMap::new(),
        }
    }

    /// Gets the minimum number of the optional [`BooleanClause`]s which must be satisfied.
    pub fn get_minimum_number_should_match(&self) -> i32 {
        self.minimum_number_should_match
    }

    /// Return a slice of the clauses of this [`BooleanQuery`].
    pub fn clauses(&self) -> &[BooleanClause] {
        &self.clauses
    }

    /// Return the collection of queries for the given [`Occur`].
    pub fn get_clauses(&self, occur: Occur) -> &[Query] {
        self.clause_sets
            .get(&occur)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
    /// Whether this query is a pure disjunction, ie. it only has SHOULD clauses and it is enough for
    /// a single clause to match for this boolean query to match.
    pub(crate) fn is_pure_disjunction(&self) -> bool {
        self.clauses.len() == self.get_clauses(Occur::Should).len()
            && self.minimum_number_should_match <= 1
    }

    /// Whether this query is a two clause disjunction with two term query clauses.
    pub(crate) fn is_two_clause_pure_disjunction_with_terms(&self) -> bool {
        self.clauses.len() == 2
            && self.is_pure_disjunction()
            && matches!(self.clauses[0].query, Query::Term(_))
            && matches!(self.clauses[1].query, Query::Term(_))
    }
}
impl Hash for BooleanQuery {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.minimum_number_should_match.hash(state);
        for clause in &self.clause_sets {
            clause.hash(state);
        }
    }
}
impl QueryBase for BooleanQuery {
    fn as_string(&self, field: &str) -> String {
        let mut buffer = String::new();
        let need_parens = self.minimum_number_should_match > 0;

        if need_parens {
            buffer.push('(');
        }

        for (i, clause) in self.clauses.iter().enumerate() {
            buffer.push_str(&clause.occur.to_string());

            match clause.query {
                Query::Boolean(ref v) => {
                    buffer.push_str(&v.as_string(field));
                },
                _ => {
                    buffer.push_str(&clause.query.as_string(field));
                },
            }

            if i != self.clauses.len() - 1 {
                buffer.push(' ');
            }
        }

        if need_parens {
            buffer.push(')');
        }

        if self.minimum_number_should_match > 0 {
            buffer.push('~');
            buffer.push_str(&self.minimum_number_should_match.to_string());
        }

        buffer
    }

    type Weight<S, IRC, QCP, QC>
        = DummyWeight<IRC::LeafReader>
    where
        S: Similarity,
        IRC: IndexReaderContext,
        QCP: QueryCachingPolicy,
        QC: QueryCache;
    fn create_weight<S, IRC, QT, QCP, QC>(
        self,
        _searcher: &IndexSearcher<IRC, S, QT, QCP, QC>,
        _score_mode: &ScoreMode,
        _boost: f32,
        _per_reader_term_state: Option<TermStates<IRCTermState<IRC>>>,
    ) -> Result<Self::Weight<S, IRC, QCP, QC>>
    where
        IRC: IndexReaderContext,
        S: Similarity,
        QT: QueryTimeout,
        QCP: QueryCachingPolicy,
        QC: QueryCache,
        Self: Sized,
    {
        todo!()
    }

    type RewriteQuery = Query;

    fn rewrite<IRC, S, QT, QCP, QC>(
        &self,
        _searcher: &IndexSearcher<IRC, S, QT, QCP, QC>,
    ) -> Result<Option<Self::RewriteQuery>>
    where
        IRC: IndexReaderContext,
        S: Similarity,
        QT: QueryTimeout,
        QCP: QueryCachingPolicy,
        QC: QueryCache,
    {
        todo!()
    }

    fn visit<QV>(&self, _visitor: &QV)
    where
        QV: QueryVisitor,
    {
        todo!()
    }
}

/// A builder for boolean queries
pub struct Builder {
    minimum_number_should_match: i32,
    clauses: Vec<BooleanClause>,
}
impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

impl Builder {
    pub fn new() -> Builder {
        Builder {
            minimum_number_should_match: 0,
            clauses: Vec::new(),
        }
    }
    /// Specifies a minimum number of the optional [`BooleanClause`]s which must be satisfied.
    ///
    /// By default, no optional clauses are necessary for a match (unless there are no required
    /// clauses). If this method is used, then the specified number of clauses is required.
    ///
    /// Use of this method is totally independent of specifying that any specific clauses are
    /// required (or prohibited). This number will only be compared against the number of matching
    /// optional clauses.
    /// # Parameters
    ///
    /// * `min` – the number of optional clauses that must match
    pub fn set_minimum_number_should_match(&mut self, min: i32) -> &mut Self {
        self.minimum_number_should_match = min;
        self
    }

    /// Add a new clause to this [`Builder`]. Note that the order in which clauses are added does
    /// not have any impact on matching documents or query performance.
    ///
    /// # Errors
    ///
    /// Returns [`IndexSearcherError::TooManyClauses`] if the new number of clauses exceeds
    /// the maximum clause count.
    pub fn add_clause(&mut self, clause: BooleanClause) -> Result<&mut Self> {
        // We do the final deep check for max clauses count limit during
        // `IndexSearcher::rewrite` but do this check to short circuit in case
        // a single query holds more than numClauses.
        //
        // NOTE: this is not just an early check for optimization -- it's
        // necessary to prevent run-away rewriting of bad queries from
        // creating BooleanQuery objects that might eat up all the heap.
        if self.clauses.len() >= get_max_clause_count() {
            return Err(LuceneError::too_many_clauses(""));
        }
        self.clauses.push(clause);
        Ok(self)
    }
    /// Add a collection of [`BooleanClause`]s to this [`Builder`]. Note that the order in which
    /// clauses are added does not have any impact on matching documents or query performance.
    ///
    /// # Errors
    ///
    /// Returns [`LuceneError::TooManyClauses`] if the new number of clauses exceeds
    /// the maximum clause count.
    pub fn add_all(&mut self, collection: Vec<BooleanClause>) -> Result<&mut Self> {
        let len = collection.len();

        if self.clauses.len() + len > get_max_clause_count() {
            return Err(LuceneError::too_many_clauses(""));
        }
        self.clauses.extend(collection);
        Ok(self)
    }
    /// Add a new clause to this [`Builder`]. Note that the order in which clauses are added does
    /// not have any impact on matching documents or query performance.
    ///
    /// # Errors
    ///
    /// Returns [`LuceneError::TooManyClauses`] if the new number of clauses exceeds
    /// the maximum clause count.
    pub fn add_query(&mut self, query: Query, occur: Occur) -> Result<&mut Self> {
        self.add_clause(BooleanClause::new(query, occur))
    }

    /// Create a new [`BooleanQuery`] based on the parameters that have been set on this builder.
    pub fn build(self) -> BooleanQuery {
        BooleanQuery::new(self.minimum_number_should_match, self.clauses)
    }
}
