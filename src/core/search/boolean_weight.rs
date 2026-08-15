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
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::search::abstract_multi_term_query_constant_score_wrapper::BOOLEAN_REWRITE_TERM_COUNT_THRESHOLD;
use crate::core::search::boolean_clause::Occur::{Filter, Must};
use crate::core::search::boolean_clause::{BooleanClause, Occur};
use crate::core::search::boolean_query::BooleanQuery;
use crate::core::search::boolean_scorer_supplier::BooleanScorerSupplier;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::explanation::Explanation;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::matches_utils::from_sub_matches;
use crate::core::search::query::{Query, QueryBase, QueryWeight, QueryWeightSs};
use crate::core::search::scorable::Scorable;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::Scorer;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::similarities_impl::similarities::SimilarityEnum;
use crate::core::search::weight::Weight;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::collections::HashMap;
use std::sync::Arc;

pub struct BooleanWeight<IRC> {
  pub(crate) similarity: Arc<SimilarityEnum>,
  pub(crate) weighted_clauses: Vec<WeightedBooleanClause<IRC>>,
  pub(crate) query: BooleanQuery,
  pub(crate) score_mode: ScoreMode,
}

impl<IRC> BooleanWeight<IRC>
where
  IRC: IndexReaderContext,
{
  /// Return the number of matches of required clauses, or -1 if unknown, or numDocs if there are no
  /// required clauses.
  fn req_count(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<i32> {
    let num_docs = context.reader().num_docs()?;
    let mut req_count = num_docs;

    for weighted_clause in &self.weighted_clauses {
      if !weighted_clause.clause.is_required() {
        continue;
      }

      let count = weighted_clause.weight.count(context, searcher)?;

      if count == -1 || count == 0 {
        // If the count of one clause is unknown, then the count of the conjunction is unknown
        // too. If one clause doesn't match any docs then the conjunction doesn't match any docs
        // either.
        return Ok(count);
      } else if count == num_docs {
        // the query matches all docs, it can be safely ignored
      } else if req_count == num_docs {
        // all clauses seen so far match all docs, so the count of the new clause is also the
        // count of the conjunction
        req_count = count;
      } else {
        // We have two clauses whose count is in [1, numDocs), we can't figure out the number of
        // docs that match the conjunction without running the query.
        return Ok(-1);
      }
    }

    Ok(req_count)
  }

  /// Return the number of matches of optional clauses, or -1 if unknown, or 0 if there are no
  /// optional clauses.
  fn opt_count(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    occur: Occur,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<i32> {
    let num_docs = context.reader().num_docs()?;
    let mut opt_count = 0i32;
    let mut unknown_count = false;

    for weighted_clause in &self.weighted_clauses {
      if *weighted_clause.clause.occur() != occur {
        continue;
      }

      let count = weighted_clause.weight.count(context, searcher)?;

      if count == -1 {
        // If one clause has a number of matches that is unknown, let's be more aggressive to
        // check whether remain clauses could match all docs.
        unknown_count = true;
        continue;
      } else if count == num_docs {
        // If either clause matches all docs, then the disjunction matches all docs.
        return Ok(count);
      } else if count == 0 {
        // We can safely ignore this clause, it doesn't affect the count.
      } else if opt_count == 0 {
        // This is the first clause we see that has a non-zero count, it becomes the count of
        // the disjunction.
        opt_count = count;
      } else {
        // We have two clauses whose count is in [1, numDocs), we can't figure out the number of
        // docs that match the disjunction without running the query.
        unknown_count = true;
      }
    }

    // If at least one of clauses has a number of matches that is unknown and no clause matches all
    // docs, then the number of matches of the disjunction is unknown.
    Ok(if unknown_count { -1 } else { opt_count })
  }
}

impl<IRC> SegmentCacheable<IRC> for BooleanWeight<IRC>
where
  IRC: IndexReaderContext,
{
  fn is_cacheable(&self, ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    // Disallow caching large boolean queries to not encourage users
    // to build large boolean queries as a workaround to the fact that
    // we disallow caching large TermInSetQueries.
    if self.query.clauses().len() > BOOLEAN_REWRITE_TERM_COUNT_THRESHOLD {
      return Ok(false);
    }

    for wc in &self.weighted_clauses {
      if !wc.weight.is_cacheable(ctx)? {
        return Ok(false);
      }
    }

    Ok(true)
  }
}

impl<IRC> Weight<IRC> for BooleanWeight<IRC>
where
  IRC: IndexReaderContext + 'static,
{
  fn matches<'a>(
    &'a self,
    context: &'a LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &'a IndexSearcher<IRC>,
  ) -> Result<Option<crate::core::search::query::QueryWeightMatches<'a>>> {
    let mut matches = Vec::new();
    let mut should_match_count = 0;
    for weighted_clause in &self.weighted_clauses {
      let clause = &weighted_clause.clause;
      let clause_matches = weighted_clause.weight.matches(context, doc, searcher)?;
      if clause.is_prohibited() {
        if clause_matches.is_some() {
          return Ok(None);
        }
      } else if clause.is_required() {
        let Some(clause_matches) = clause_matches else {
          return Ok(None);
        };
        matches.push(clause_matches);
      } else if let Some(clause_matches) = clause_matches {
        matches.push(clause_matches);
        should_match_count += 1;
      }
    }
    if should_match_count < self.query.get_minimum_number_should_match() {
      return Ok(None);
    }
    Ok(from_sub_matches(matches))
  }

  fn explain(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Explanation> {
    let min_should_match = self.query.get_minimum_number_should_match();

    let mut subs: Vec<Explanation> = Vec::new();
    let mut fail = false;
    let mut match_count = 0;
    let mut should_match_count = 0;

    for wc in &self.weighted_clauses {
      let clause = &wc.clause;
      let weight = &wc.weight;

      let e = weight.explain(context, doc, searcher)?;

      if e.is_match() {
        if clause.is_scoring() {
          subs.push(e);
        } else if clause.is_required() {
          subs.push(Explanation::match_(
            0.0,
            "match on required clause, product of:",
            vec![
              Explanation::match_(0.0, format!("{} clause", Occur::Filter), vec![]),
              e,
            ],
          ));
        } else if clause.is_prohibited() {
          subs.push(Explanation::no_match(
            format!(
              "match on prohibited clause ({})",
              clause.query.to_string("")?
            ),
            vec![e],
          ));
          fail = true;
        }

        if !clause.is_prohibited() {
          match_count += 1;
        }

        if clause.occur() == &Occur::Should {
          should_match_count += 1;
        }
      } else if clause.is_required() {
        subs.push(Explanation::no_match(
          format!(
            "no match on required clause ({})",
            clause.query.to_string("")?
          ),
          vec![e],
        ));
        fail = true;
      }
    }

    if fail {
      Ok(Explanation::no_match(
        "Failure to meet condition(s) of required/prohibited clause(s)",
        subs,
      ))
    } else if match_count == 0 {
      Ok(Explanation::no_match("No matching clauses", subs))
    } else if should_match_count < min_should_match {
      Ok(Explanation::no_match(
        format!(
          "Failure to match minimum number of optional clauses: {}",
          min_should_match
        ),
        subs,
      ))
    } else {
      // Replicating the same floating-point errors as the scorer does is quite
      // complex (essentially because of how ReqOptSumScorer casts intermediate
      // contributions to the score to floats), so in order to make sure that
      // explanations have the same value as the score, we pull a scorer and
      // use it to compute the score.

      let mut scorer = self
        .scorer(context, searcher)?
        .ok_or_else(|| LuceneError::illegal_state("no scorer available for explanation"))?;

      let advanced = scorer.iterator_mut().advance(doc)?;
      debug_assert!(advanced == doc);

      Ok(Explanation::match_(scorer.score()?, "sum of:", subs))
    }
  }

  fn get_query(&self) -> Arc<Query> {
    let v: Query = self.query.clone().into();
    Arc::new(v)
  }

  type ScorerSupplier = QueryWeightSs<IRC>;
  // type ScorerSupplier = DummyScorerSupplier;

  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::ScorerSupplier>> {
    let mut min_should_match = self.query.get_minimum_number_should_match();

    let mut must = Vec::new();
    let mut should = Vec::new();
    let mut filter = Vec::new();
    let mut must_not = Vec::new();

    for wc in &self.weighted_clauses {
      let sub_supplier = wc.weight.scorer_supplier(context, searcher)?;
      match sub_supplier {
        None => {
          if wc.clause.is_required() {
            return Ok(None);
          }
        },
        Some(sub_scorer) => match wc.clause.occur() {
          Occur::Must => must.push(sub_scorer),
          Occur::Should => should.push(sub_scorer),
          Occur::Filter => filter.push(sub_scorer),
          Occur::MustNot => must_not.push(sub_scorer),
        },
      }
    }
    // scorer simplifications:
    if (should.len() as i32) == min_should_match {
      must.append(&mut should);
      min_should_match = 0;
    }

    if (filter.is_empty() && must.is_empty() && should.is_empty())
      || (should.len() as i32) < min_should_match
    {
      return Ok(None);
    }

    if !self.score_mode.needs_scores() && min_should_match == 0 && (must.len() + filter.len()) > 0 {
      should.clear();
    }
    let max_doc = context.reader().max_doc()?;
    let mut scores = HashMap::new();
    scores.insert(Occur::Must, must);
    scores.insert(Occur::Should, should);
    scores.insert(Occur::Filter, filter);
    scores.insert(Occur::MustNot, must_not);
    let v = BooleanScorerSupplier::new(scores, self.score_mode, min_should_match, max_doc)?;
    Ok(Some(Box::new(v)))
  }

  fn count(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<i32> {
    let num_docs = context.reader().num_docs()?;

    if self.query.is_pure_disjunction() {
      return self.opt_count(context, Occur::Should, searcher);
    }

    let positive_count = if (!self.query.get_clauses_idx(Filter).is_empty()
      || !self.query.get_clauses_idx(Must).is_empty())
      && self.query.get_minimum_number_should_match() == 0
    {
      self.req_count(context, searcher)?
    } else {
      // The query has a non-zero min-should match. We could handle some cases, e.g.
      // minShouldMatch=N and we can find N SHOULD clauses that match all docs, but are there
      // real-world queries that would benefit from Lucene handling this case?
      -1
    };

    if positive_count == 0 {
      return Ok(0);
    }

    let prohibited_count = self.opt_count(context, Occur::MustNot, searcher)?;

    if prohibited_count == -1 {
      Ok(-1)
    } else if prohibited_count == 0 {
      Ok(positive_count)
    } else if prohibited_count == num_docs {
      Ok(0)
    } else if positive_count == num_docs {
      Ok(num_docs - prohibited_count)
    } else {
      Ok(-1)
    }
  }

  #[cfg(test)]
  fn as_any(&mut self) -> &mut dyn std::any::Any {
    self
  }
}
pub(crate) struct WeightedBooleanClause<IRC> {
  pub(crate) clause: BooleanClause,
  pub(crate) weight: QueryWeight<IRC>,
}

impl<IRC> WeightedBooleanClause<IRC>
where
  IRC: IndexReaderContext,
{
  pub(crate) fn new(clause: BooleanClause, weight: QueryWeight<IRC>) -> Self {
    Self { clause, weight }
  }
}
struct BooleanQueryMeta {
  minimum_number_should_match: i32,
  is_pure_disjunction: bool,
  has_no_filter: bool,
  has_no_must: bool,
  clause_size: i32,
}
impl BooleanQueryMeta {
  pub fn new(
    minimum_number_should_match: i32,
    is_pure_disjunction: bool,
    has_no_filter: bool,
    has_no_must: bool,
    clause_size: i32,
  ) -> Self {
    Self {
      minimum_number_should_match,
      is_pure_disjunction,
      has_no_filter,
      has_no_must,
      clause_size,
    }
  }
}
