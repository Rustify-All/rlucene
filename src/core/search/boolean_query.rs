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
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::term_states::build;
use crate::core::search::boolean_clause::{BooleanClause, Occur};
use crate::core::search::boolean_weight::{BooleanWeight, WeightedBooleanClause};
use crate::core::search::boost_query::BoostQuery;
use crate::core::search::constant_score_query::ConstantScoreQuery;
use crate::core::search::index_searcher::{IndexSearcher, get_max_clause_count};
use crate::core::search::match_no_docs_query::MatchNoDocsQuery;
use crate::core::search::query::{IntoQuery, Query, QueryBase};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::term_query::TermQuery;
use crate::core::util::core_helper::HasIdentity;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use smallvec::SmallVec;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

/// A query that matches documents matching boolean combinations of other queries, e.g.
/// [`TermQuery`]s, [`PhraseQuery`](crate::core::search::phrase_query::PhraseQuery)s or other [`BooleanQuery`]s.
#[derive(Debug, Clone)]
pub struct BooleanQuery {
  id: Identity,
  minimum_number_should_match: i32,
  pub(crate) clauses: Vec<BooleanClause>,
  clause_sets: [Vec<usize>; 4],
}

impl BooleanQuery {
  fn new(minimum_number_should_match: i32, clauses: Vec<BooleanClause>) -> BooleanQuery {
    let mut clause_sets: [Vec<usize>; 4] = [const { Vec::new() }; 4];
    for occur in [Occur::Should, Occur::Must] {
      let count = clauses
        .iter()
        .filter(|clause| clause.occur == occur)
        .count();
      clause_sets[occur as usize].reserve_exact(count);
    }

    for (idx, clause) in clauses.iter().enumerate() {
      let occur = clause.occur;

      match occur {
        Occur::Should | Occur::Must => {
          clause_sets[occur as usize].push(idx);
        },

        Occur::Filter | Occur::MustNot => {
          let indices = &mut clause_sets[occur as usize];

          let exists = indices.iter().any(|&i| clauses[i].query == clause.query);
          if !exists {
            indices.push(idx);
          }
        },
      }
    }

    BooleanQuery {
      id: Identity::new(),
      minimum_number_should_match,
      clauses,
      clause_sets,
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
  pub fn get_clauses_idx(&self, occur: Occur) -> &[usize] {
    &self.clause_sets[occur as usize]
  }
  /// Whether this query is a pure disjunction, ie. it only has SHOULD clauses and it is enough for
  /// a single clause to match for this boolean query to match.
  pub(crate) fn is_pure_disjunction(&self) -> bool {
    self.clauses.len() == self.get_clauses_idx(Occur::Should).len()
      && self.minimum_number_should_match <= 1
  }

  /// Whether this query is a two clause disjunction with two term query clauses.
  pub(crate) fn is_two_clause_pure_disjunction_with_terms(&self) -> bool {
    self.clauses.len() == 2
      && self.is_pure_disjunction()
      && matches!(self.clauses[0].query, Query::Term(_))
      && matches!(self.clauses[1].query, Query::Term(_))
  }
  pub fn rewrite_no_scoring(&self) -> Result<Option<Query>> {
    let mut actually_rewritten = false;
    // Retain builder validation while borrowing clauses until a replacement is needed.
    let mut new_query = Vec::with_capacity(self.clauses.len());
    let mut add_clause = |query, occur| -> Result<()> {
      if new_query.len() >= get_max_clause_count() {
        return Err(LuceneError::too_many_clauses(""));
      }
      new_query.push((query, occur));
      Ok(())
    };

    let keep_should = self.get_minimum_number_should_match() > 0
      || (self.get_clauses_idx(Occur::Must).len() + self.get_clauses_idx(Occur::Filter).len() == 0);

    for clause in &self.clauses {
      let query = &clause.query;
      // NOTE: rewrite_no_scoring() should not call rewrite(), otherwise this
      // method could run in exponential time with the depth of the query as
      // every new level would rewrite 2x more than its parent level.
      let mut rewritten = query;
      if let Query::Boost(boost_query) = rewritten {
        rewritten = boost_query.get_query();
      }
      if let Query::ConstantScore(constant_score_query) = rewritten {
        rewritten = constant_score_query.get_query();
      }
      let rewritten = if let Query::Boolean(boolean_query) = rewritten {
        match boolean_query.rewrite_no_scoring()? {
          Some(query) => Cow::Owned(query),
          None => Cow::Borrowed(rewritten),
        }
      } else {
        Cow::Borrowed(rewritten)
      };
      let occur = clause.occur;
      if occur == Occur::Should && !keep_should {
        // ignore clause
        actually_rewritten = true;
      } else if occur == Occur::Must {
        // replace MUST clauses with FILTER clauses
        add_clause(rewritten, Occur::Filter)?;
        actually_rewritten = true;
      } else if query.identity() != rewritten.identity() {
        add_clause(rewritten, occur)?;
        actually_rewritten = true;
      } else {
        add_clause(Cow::Borrowed(query), occur)?;
      }
    }

    if !actually_rewritten {
      return Ok(None);
    }

    let clauses = new_query
      .into_iter()
      .map(|(query, occur)| BooleanClause::new(query.into_owned(), occur))
      .collect();
    Ok(Some(
      BooleanQuery::new(self.get_minimum_number_should_match(), clauses).into(),
    ))
  }
  #[cfg_attr(test, allow(clippy::mutable_key_type))]
  fn as_counts_map(&self) -> HashMap<(Occur, &Query), usize> {
    let mut m = HashMap::new();
    for occur in [Occur::Must, Occur::Filter, Occur::Should, Occur::MustNot] {
      let indices = self.get_clauses_idx(occur);
      for &idx in indices {
        let q = &self.clauses[idx].query;
        *m.entry((occur, q)).or_insert(0) += 1;
      }
    }
    m
  }
  pub(crate) fn raw_weight<IRC, SIM>(
    self,
    searcher: &IndexSearcher<IRC, SIM>,
    score_mode: &ScoreMode,
    boost: f32,
  ) -> Result<BooleanWeight<IRC, SIM>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync,
    Self: Sized,
  {
    let mut weighted_clauses = Vec::with_capacity(self.clauses().len());
    for (clause_index, c) in self.clauses.iter().enumerate() {
      let clause_score_mode = if c.is_scoring() {
        score_mode
      } else {
        &ScoreMode::CompleteNoScores
      };
      let weight = searcher.create_weight(c.query.clone(), *clause_score_mode, boost)?;

      weighted_clauses.push(WeightedBooleanClause::new(clause_index, weight));
    }
    Ok(BooleanWeight::<_, SIM> {
      weighted_clauses: weighted_clauses.into_boxed_slice(),
      query: self,
      score_mode: *score_mode,
    })
  }
  /// Rewrite a single two clause disjunction query with terms to two term queries and a conjunction query using the inclusion–exclusion principle.
  pub(crate) fn rewrite_two_clause_disjunction_with_terms_for_count<IRC, SIM>(
    &mut self,
    index_searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<[Query; 3]>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
  {
    if self.clauses.len() != 2 {
      return Err(LuceneError::illegal_state(
        "two-clause count rewrite requires exactly two clauses",
      ));
    }

    let mut new_query = Builder::new();
    let mut queries: [Option<Query>; 2] = [None, None];

    for (clause, result) in self.clauses.iter_mut().zip(&mut queries) {
      let term_query = match &mut clause.query {
        Query::Term(q) => q,
        _ => {
          return Err(LuceneError::unsupported_operation("expected TermQuery"));
        },
      };

      if term_query.get_term_states().is_none() {
        let term_states = build(index_searcher, term_query.get_term(), false)?;
        // Keep the resolved states available if count falls back to this disjunction.
        *term_query =
          TermQuery::with_term_state(term_query.get_term(), Some(Arc::new(term_states)));
      }

      new_query.add(term_query.clone(), Occur::Must)?;
      *result = Some(term_query.clone().into());
    }

    let conjunction = new_query.build().into();
    match queries {
      [Some(first), Some(second)] => Ok([first, second, conjunction]),
      _ => Err(LuceneError::illegal_state(
        "two-clause count rewrite did not create three queries",
      )),
    }
  }
}

impl Hash for BooleanQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    self.minimum_number_should_match.hash(state);
    // FILTER and MUST_NOT clauses may have been deduplicated in clause_sets.
    let mut hs = SmallVec::<[u64; 8]>::with_capacity(self.clause_sets.iter().map(Vec::len).sum());
    for occur in [Occur::Must, Occur::Filter, Occur::Should, Occur::MustNot] {
      let indices = self.get_clauses_idx(occur);
      for &idx in indices {
        let mut h = DefaultHasher::new();
        occur.hash(&mut h);
        self.clauses[idx].query.hash(&mut h);
        hs.push(h.finish());
      }
    }
    hs.sort_unstable();
    hs.hash(state);
  }
}

impl PartialEq for BooleanQuery {
  fn eq(&self, other: &Self) -> bool {
    self.minimum_number_should_match == other.minimum_number_should_match
      && self.as_counts_map() == other.as_counts_map()
  }
}

impl Eq for BooleanQuery {}

impl HasIdentity for BooleanQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}
impl QueryBase for BooleanQuery {
  type Weight<IRC, SIM>
    = BooleanWeight<IRC, SIM>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static;

  fn to_string(&self, field: &str) -> Result<String> {
    let mut buffer = String::new();
    let need_parens = self.minimum_number_should_match > 0;

    if need_parens {
      buffer.push('(');
    }

    for (i, clause) in self.clauses.iter().enumerate() {
      write!(buffer, "{}", clause.occur)?;
      let is_boolean = matches!(&clause.query, Query::Boolean(_));
      if is_boolean {
        buffer.push('(');
        buffer.push_str(&clause.query.to_string(field)?);
        buffer.push(')');
      } else {
        buffer.push_str(&clause.query.to_string(field)?);
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
      write!(buffer, "{}", self.minimum_number_should_match)?;
    }

    Ok(buffer)
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
    let weight = self.raw_weight(searcher, score_mode, boost)?;
    Ok(weight)
  }
  fn rewrite<IRC, SIM>(&self, searcher: &IndexSearcher<IRC, SIM>) -> Result<Option<Query>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync,
    Self: Sized,
  {
    if self.clauses.is_empty() {
      return Ok(Some(
        MatchNoDocsQuery::with_reason("empty BooleanQuery").into(),
      ));
    }
    let must_not_len = self.get_clauses_idx(Occur::MustNot).len();
    if self.clauses.len() == must_not_len {
      return Ok(Some(Query::MatchNoDocs(MatchNoDocsQuery::with_reason(
        "pure negative BooleanQuery",
      ))));
    }

    // optimize 1-clause queries
    if self.clauses.len() == 1 {
      let clause = &self.clauses[0];

      if self.minimum_number_should_match == 1 && clause.occur == Occur::Should {
        let v = self.clauses[0].clone();
        return Ok(Some(v.query));
      } else if self.minimum_number_should_match == 0 {
        match clause.occur {
          Occur::Should | Occur::Must => {
            let v = self.clauses[0].clone();
            return Ok(Some(v.query));
          },
          Occur::Filter => {
            // no scoring clauses, so return a score of 0
            {
              let v = self.clauses[0].clone();
              return Ok(Some(Query::Boost(BoostQuery::new(
                Query::ConstantScore(ConstantScoreQuery::new(v.query)),
                0.0,
              )?)));
            };
          },
          Occur::MustNot => return Err(LuceneError::illegal_state("should not be here")),
        }
      }
    }
    // recursively rewrite
    {
      let mut clauses = Vec::with_capacity(self.clauses.len());
      let mut actually_rewritten = false;
      for clause in &self.clauses {
        let query = &clause.query;
        let occur = clause.occur;
        let rewritten = match occur {
          Occur::Filter | Occur::MustNot if !matches!(query, Query::Term(_)) => {
            // Clauses that are not involved in scoring can get some extra simplifications.
            let rewritten = ConstantScoreQuery::rewrite_inner(query, searcher)?;
            match rewritten {
              Some(Query::ConstantScore(constant_score_query)) => {
                Some(constant_score_query.into_inner())
              },
              rewritten => rewritten,
            }
          },
          _ => query.rewrite(searcher)?,
        };
        let rewritten = match rewritten {
          Some(query) => Cow::Owned(query),
          None => Cow::Borrowed(query),
        };
        if rewritten.identity() != query.identity() || matches!(query, Query::MatchNoDocs(_)) {
          actually_rewritten = true;
          if matches!(rewritten.as_ref(), Query::MatchNoDocs(_)) {
            match occur {
              Occur::Should | Occur::MustNot => continue,
              Occur::Must | Occur::Filter => return Ok(Some(rewritten.into_owned())),
            }
          }
        }
        // This is the same check as Builder::add_clause, at the same point in traversal.
        if clauses.len() >= get_max_clause_count() {
          return Err(LuceneError::too_many_clauses(""));
        }
        clauses.push((rewritten, occur));
      }
      if actually_rewritten {
        let clauses = clauses
          .into_iter()
          .map(|(query, occur)| BooleanClause::new(query.into_owned(), occur))
          .collect();
        return Ok(Some(
          BooleanQuery::new(self.minimum_number_should_match, clauses).into(),
        ));
      }
    }
    // Keep the later rules in a separate call frame: their owned Query temporaries must not
    // occupy the stack at every level while recursively rewriting children in debug builds.
    let rewrite_non_recursive = || -> Result<Option<Query>> {
      // remove duplicate FILTER and MUST_NOT clauses
      {
        let mut clause_count = 0;
        for queries in &self.clause_sets {
          clause_count += queries.len();
        }

        if clause_count != self.clauses.len() {
          // since clause_sets implicitly deduplicates FILTER and MUST_NOT clauses,
          // this means there were duplicates
          let mut rewritten = Builder::new();
          rewritten.set_minimum_number_should_match(self.minimum_number_should_match);

          for occur in [Occur::Must, Occur::Filter, Occur::Should, Occur::MustNot] {
            let indices = self.get_clauses_idx(occur);
            for &idx in indices {
              let clause = self.clauses[idx].clone();
              rewritten.add(clause.query, occur)?;
            }
          }

          return Ok(Some(rewritten.build().into()));
        }
      }

      // Check whether some clauses are both required and excluded
      let must_not = self.get_clauses_idx(Occur::MustNot);

      if !must_not.is_empty() {
        let must = self.get_clauses_idx(Occur::Must);
        let filter = self.get_clauses_idx(Occur::Filter);

        if must_not.iter().any(|&mn_idx| {
          let q = &self.clauses[mn_idx].query;
          must.iter().any(|&m_idx| self.clauses[m_idx].query == *q)
            || filter.iter().any(|&f_idx| self.clauses[f_idx].query == *q)
        }) {
          return Ok(Some(
            MatchNoDocsQuery::with_reason("FILTER or MUST clause also in MUST_NOT").into(),
          ));
        }

        if must_not
          .iter()
          .any(|&idx| matches!(self.clauses[idx].query, Query::MatchAllDocs(_)))
        {
          return Ok(Some(
            MatchNoDocsQuery::with_reason("MUST_NOT clause is MatchAllDocsQuery").into(),
          ));
        }
      }

      // remove FILTER clauses that are also MUST clauses or that match all documents
      let filter_idx = self.get_clauses_idx(Occur::Filter);

      if !filter_idx.is_empty() {
        let must_indices = self.get_clauses_idx(Occur::Must);

        let mut new_filter_ixd = SmallVec::<[usize; 8]>::from_slice(filter_idx);
        let mut modified = false;

        // remove MatchAllDocsQuery if needed
        if new_filter_ixd.len() > 1 || !must_indices.is_empty() {
          let before = new_filter_ixd.len();
          new_filter_ixd
            .retain(|&mut idx| !matches!(self.clauses[idx].query, Query::MatchAllDocs(_)));
          modified |= new_filter_ixd.len() != before;
        }

        let before = new_filter_ixd.len();
        new_filter_ixd.retain(|&mut f_idx| {
          let fq = &self.clauses[f_idx].query;
          !must_indices
            .iter()
            .any(|&m_idx| self.clauses[m_idx].query == *fq)
        });

        modified |= new_filter_ixd.len() != before;

        if modified {
          let mut builder = Builder::new();
          builder.set_minimum_number_should_match(self.get_minimum_number_should_match());

          for clause in &self.clauses {
            if clause.occur != Occur::Filter {
              builder.add_clause(clause.clone())?;
            }
          }

          for idx in new_filter_ixd {
            let v = self.clauses[idx].clone();
            debug_assert!(!matches!(v.query, Query::Dummy(_)));
            builder.add(v.query, Occur::Filter)?;
          }

          return Ok(Some(builder.build().into()));
        }
      }

      // convert FILTER clauses that are also SHOULD clauses to MUST clauses
      let should_indices = self.get_clauses_idx(Occur::Should);
      let filter_indices = self.get_clauses_idx(Occur::Filter);

      if !should_indices.is_empty() && !filter_indices.is_empty() {
        // compute intersection by Query equality (not identity)
        let intersection: SmallVec<[usize; 8]> = filter_indices
          .iter()
          .cloned()
          .filter(|&f_idx| {
            let fq = &self.clauses[f_idx].query;
            should_indices
              .iter()
              .any(|&s_idx| self.clauses[s_idx].query == *fq)
          })
          .collect();

        if !intersection.is_empty() {
          let mut builder = Builder::new();
          let mut min_should_match = self.get_minimum_number_should_match();
          for clause in &self.clauses {
            let in_intersection = intersection
              .iter()
              .any(|&idx| self.clauses[idx].query == clause.query);

            // TODO IMPORTANT avoid query copy here
            if in_intersection {
              if clause.occur == Occur::Should {
                builder.add(clause.query.clone(), Occur::Must)?;
                min_should_match = min_should_match.wrapping_sub(1);
              }
            } else {
              builder.add_clause(clause.clone())?;
            }
          }

          builder.set_minimum_number_should_match(min_should_match.max(0));
          return Ok(Some(builder.build().into()));
        }
      }

      // Deduplicate SHOULD clauses by summing up their boosts
      let should_indices = self.get_clauses_idx(Occur::Should);

      if !should_indices.is_empty() && self.minimum_number_should_match <= 1 {
        #[cfg_attr(test, allow(clippy::mutable_key_type))]
        let mut should_clauses = HashMap::new();

        for &idx in should_indices {
          let mut query = &self.clauses[idx].query;
          let mut boost = 1.0;

          while let Query::Boost(bq) = query {
            boost *= bq.get_boost() as f64;
            query = bq.get_query();
          }

          *should_clauses.entry(query).or_insert(0.0) += boost;
        }

        if should_clauses.len() != should_indices.len() {
          let mut builder = Builder::new();
          builder.set_minimum_number_should_match(self.minimum_number_should_match);

          for (query, boost) in should_clauses {
            let mut query = query.clone();
            let boost = boost as f32;
            if boost != 1.0 {
              query = Query::Boost(BoostQuery::new(query, boost)?);
            }
            builder.add(query, Occur::Should)?;
          }

          for clause in &self.clauses {
            if clause.occur != Occur::Should {
              let v = clause.clone();
              builder.add_clause(v)?;
            }
          }

          return Ok(Some(builder.build().into()));
        }
      }

      // Deduplicate MUST clauses by summing up their boosts
      let must_indices = self.get_clauses_idx(Occur::Must);

      if !must_indices.is_empty() {
        #[cfg_attr(test, allow(clippy::mutable_key_type))]
        let mut must_clauses = HashMap::new();

        for &idx in must_indices {
          let mut query = &self.clauses[idx].query;
          let mut boost: f64 = 1.0;

          while let Query::Boost(bq) = query {
            boost *= bq.get_boost() as f64;
            query = bq.get_query();
          }

          *must_clauses.entry(query).or_insert(0.0) += boost;
        }

        if must_clauses.len() != must_indices.len() {
          let mut builder = Builder::new();
          builder.set_minimum_number_should_match(self.minimum_number_should_match);

          for (query, boost) in must_clauses {
            let mut query = query.clone();
            let boost = boost as f32;
            if boost != 1.0 {
              query = Query::Boost(BoostQuery::new(query, boost)?);
            }
            builder.add(query, Occur::Must)?;
          }

          for clause in &self.clauses {
            if clause.occur != Occur::Must {
              let v = clause.clone();
              builder.add_clause(v)?;
            }
          }

          return Ok(Some(builder.build().into()));
        }
      }

      // Rewrite queries whose single scoring clause is a MUST clause on a
      // MatchAllDocsQuery to a ConstantScoreQuery
      {
        let must_indices = self.get_clauses_idx(Occur::Must);
        let filter_indices = self.get_clauses_idx(Occur::Filter);

        if must_indices.len() == 1 && !filter_indices.is_empty() {
          let mut must = &self.clauses[must_indices[0]].query;
          let mut boost = 1.0f32;

          if let Query::Boost(bq) = must {
            boost = bq.get_boost();
            must = bq.get_query();
          }

          if matches!(must, Query::MatchAllDocs(_)) {
            // our single scoring clause matches everything: rewrite to a CSQ on the filter
            // ignore SHOULD clause for now
            let mut builder = Builder::new();
            for clause in &self.clauses {
              match clause.occur {
                Occur::Filter | Occur::MustNot => {
                  let v = clause.clone();
                  builder.add_clause(v)?;
                },
                Occur::Must | Occur::Should => {
                  // ignore
                },
              }
            }

            let mut rewritten = builder.build().into();
            rewritten = Query::ConstantScore(ConstantScoreQuery::new(rewritten));

            if boost != 1.0 {
              rewritten = Query::Boost(BoostQuery::new(rewritten, boost)?);
            }

            // now add back the SHOULD clauses
            let mut builder = Builder::new();
            builder.set_minimum_number_should_match(self.get_minimum_number_should_match());
            builder.add(rewritten, Occur::Must)?;

            let should_indices = self.get_clauses_idx(Occur::Should);
            for &idx in should_indices {
              let v = self.clauses[idx].clone();
              debug_assert!(!matches!(v.query, Query::Dummy(_)));
              builder.add(v.query, Occur::Should)?;
            }

            return Ok(Some(builder.build().into()));
          }
        }
      }

      // Flatten nested disjunctions, this is important for block-max WAND to perform well
      if self.minimum_number_should_match <= 1 {
        let mut clauses: SmallVec<[(&Query, Occur); 4]> =
          SmallVec::with_capacity(self.clauses.len());
        let mut add_clause = |query, occur| {
          if clauses.len() >= get_max_clause_count() {
            return Err(LuceneError::too_many_clauses(""));
          }
          clauses.push((query, occur));
          Ok(())
        };
        let mut actually_rewritten = false;

        for clause in &self.clauses {
          if clause.occur == Occur::Should {
            if let Query::Boolean(inner) = &clause.query {
              if inner.is_pure_disjunction() {
                actually_rewritten = true;
                for inner_clause in inner.clauses.iter() {
                  add_clause(&inner_clause.query, inner_clause.occur)?;
                }
              } else {
                add_clause(&clause.query, clause.occur)?;
              }
            } else {
              add_clause(&clause.query, clause.occur)?;
            }
          } else {
            add_clause(&clause.query, clause.occur)?;
          }
        }

        if actually_rewritten {
          let clauses = clauses
            .into_iter()
            .map(|(query, occur)| BooleanClause::new(query.clone(), occur))
            .collect();
          return Ok(Some(
            BooleanQuery::new(self.minimum_number_should_match, clauses).into(),
          ));
        }
      }

      // Inline required / prohibited clauses. This helps run filtered conjunctive queries more
      // efficiently by providing all clauses to the block-max AND scorer.
      {
        let mut clauses: SmallVec<[(&Query, Occur); 4]> =
          SmallVec::with_capacity(self.clauses.len());
        let mut add_clause = |query, occur| {
          if clauses.len() >= get_max_clause_count() {
            return Err(LuceneError::too_many_clauses(""));
          }
          clauses.push((query, occur));
          Ok(())
        };
        let mut actually_rewritten = false;

        for outer_clause in &self.clauses {
          if outer_clause.is_required() {
            if let Query::Boolean(inner_query) = &outer_clause.query {
              // Inlining prohibited clauses is not legal if the query is a pure negation, since pure
              // negations have no matches. It works because the inner BooleanQuery would have first
              // rewritten to a MatchNoDocsQuery if it only had prohibited clauses.
              debug_assert!(
                inner_query.get_clauses_idx(Occur::MustNot).len() != inner_query.clauses.len()
              );

              let inner_should_len = inner_query.get_clauses_idx(Occur::Should).len();

              if inner_query.get_minimum_number_should_match() == 0 && inner_should_len == 0 {
                actually_rewritten = true;

                for inner_clause in &inner_query.clauses {
                  let inner_occur = inner_clause.occur;

                  if inner_occur == Occur::Filter
                    || inner_occur == Occur::MustNot
                    || outer_clause.occur == Occur::Must
                  {
                    add_clause(&inner_clause.query, inner_clause.occur)?;
                  } else {
                    debug_assert!(outer_clause.occur == Occur::Filter);
                    debug_assert!(inner_occur == Occur::Must);
                    // In this case we need to change the occur of the inner query from MUST to FILTER.
                    add_clause(&inner_clause.query, Occur::Filter)?;
                  }
                }
              } else {
                add_clause(&outer_clause.query, outer_clause.occur)?;
              }
            } else {
              add_clause(&outer_clause.query, outer_clause.occur)?;
            }
          } else {
            add_clause(&outer_clause.query, outer_clause.occur)?;
          }
        }

        if actually_rewritten {
          let clauses = clauses
            .into_iter()
            .map(|(query, occur)| BooleanClause::new(query.clone(), occur))
            .collect();
          return Ok(Some(
            BooleanQuery::new(self.minimum_number_should_match, clauses).into(),
          ));
        }
      }
      // SHOULD clause count less than or equal to minimum_number_should_match
      // Important(this can only be processed after nested clauses have been flattened)
      {
        let should_indices = self.get_clauses_idx(Occur::Should);
        let should_len = should_indices.len() as i64;

        if should_len < i64::from(self.minimum_number_should_match) {
          return Ok(Some(
            MatchNoDocsQuery::with_reason("SHOULD clause count less than minimumNumberShouldMatch")
              .into(),
          ));
        }

        if should_len > 0 && should_len == i64::from(self.minimum_number_should_match) {
          let mut builder = Builder::new();

          for clause in &self.clauses {
            let v = clause.clone();
            if v.occur == Occur::Should {
              builder.add(v.query, Occur::Must)?;
            } else {
              builder.add_clause(v)?;
            }
          }

          return Ok(Some(builder.build().into()));
        }
      }
      // Inline SHOULD clauses from the only MUST clause
      {
        let should_indices = self.get_clauses_idx(Occur::Should);
        let must_indices = self.get_clauses_idx(Occur::Must);

        if should_indices.is_empty() && must_indices.len() == 1 {
          let must_clause = &self.clauses[must_indices[0]];
          if let Query::Boolean(inner) = &must_clause.query {
            let inner_should_len = inner.get_clauses_idx(Occur::Should).len();

            if inner.clauses.len() == inner_should_len {
              let mut rewritten = Builder::new();

              for clause in &self.clauses {
                if clause.occur != Occur::Must {
                  rewritten.add_clause(clause.clone())?;
                }
              }

              for inner_clause in &inner.clauses {
                rewritten.add_clause(inner_clause.clone())?;
              }

              let msm = inner.get_minimum_number_should_match().max(1);
              rewritten.set_minimum_number_should_match(msm);

              return Ok(Some(rewritten.build().into()));
            }
          }
        }
      }
      Ok(None)
    };
    rewrite_non_recursive()
  }

  fn visit<QV>(&self, visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    let query = self.into();
    let mut sub_visitor = visitor.get_sub_visitor(Occur::Must, query);
    for occur in [Occur::Must, Occur::Filter, Occur::Should, Occur::MustNot] {
      let clause_indices = self.get_clauses_idx(occur);
      if !clause_indices.is_empty() {
        if occur == Occur::Must {
          for index in clause_indices {
            self.clauses[*index].query.visit(&mut sub_visitor)?;
          }
        } else {
          let mut visitor = sub_visitor.get_sub_visitor(occur, query);
          for index in clause_indices {
            self.clauses[*index].query.visit(&mut visitor)?;
          }
        }
      }
    }
    Ok(())
  }
}

/// A builder for boolean queries
#[derive(Clone)]
pub struct Builder {
  minimum_number_should_match: i32,
  pub(crate) clauses: Vec<BooleanClause>,
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
  /// Returns [`LuceneErrorKind::TooManyClauses`] if the new number of clauses exceeds the maximum
  /// clause count.
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
  /// Accepts arrays, vectors, or iterators. The complete batch is checked against
  /// the clause limit before any clauses are added to the builder.
  ///
  /// # Errors
  ///
  /// Returns [`LuceneErrorKind::TooManyClauses`] if the new number of clauses exceeds
  /// the maximum clause count.
  pub fn add_all<I>(&mut self, collection: I) -> Result<&mut Self>
  where
    I: IntoIterator<Item = BooleanClause>,
  {
    let collection: Vec<BooleanClause> = collection.into_iter().collect();
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
  /// Returns [`LuceneErrorKind::TooManyClauses`] if the new number of clauses exceeds
  /// the maximum clause count.
  pub fn add<Q>(&mut self, query: Q, occur: Occur) -> Result<&mut Self>
  where
    Q: IntoQuery,
  {
    self.add_clause(BooleanClause::new(query, occur))
  }

  /// Create a new [`BooleanQuery`] based on the parameters that have been set on this builder.
  pub fn build(self) -> BooleanQuery {
    BooleanQuery::new(self.minimum_number_should_match, self.clauses)
  }
}

impl crate::core::util::accountable::Accountable for BooleanQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED)
  }
}
