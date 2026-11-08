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
use crate::core::search::similarities_impl::similarities::Similarity;
use std::borrow::Cow;
use std::fmt::Write as _;

use crate::core::index::index_reader::Identity;
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::search::abstract_multi_term_query_constant_score_wrapper::BOOLEAN_REWRITE_TERM_COUNT_THRESHOLD;
use crate::core::search::boolean_clause::Occur;
use crate::core::search::boolean_query::Builder;
use crate::core::search::disjunction_max_bulk_scorer::DisjunctionMaxBulkScorer;
use crate::core::search::disjunction_max_scorer::DisjunctionMaxScorer;
use crate::core::search::disjunction_scorer::DisjunctionScorer;
use crate::core::search::explanation::Explanation;
use crate::core::search::index_searcher::{IndexSearcher, get_max_clause_count};
use crate::core::search::match_no_docs_query::MatchNoDocsQuery;
use crate::core::search::matches_utils::from_sub_matches;
use crate::core::search::query::{
  Query, QueryBase, QueryWeight, QueryWeightSs, QueryWeightSsBulkScorer, QueryWeightSsScorer,
};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::weight::Weight;
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::{CoreHelper, HasIdentity};
use smallvec::SmallVec;
use std::cell::OnceCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

/// A query that generates the union of documents produced by its subqueries, and that scores each
/// document with the maximum score for that document as produced by any subquery, plus a tie
/// breaking increment for any additional matching subqueries. This is useful when searching for a
/// word in multiple fields with different boost factors (so that the fields cannot be combined
/// equivalently into a single search field). We want the primary score to be the one associated with
/// the highest boost, not the sum of the field scores (as BooleanQuery would give). If the query is
/// "albino elephant" this ensures that "albino" matching one field and "elephant" matching another
/// gets a higher score than "albino" matching both fields. To get this result, use both BooleanQuery
/// and DisjunctionMaxQuery: for each term a DisjunctionMaxQuery searches for it in each field, while
/// the set of these DisjunctionMaxQuery's is combined into a BooleanQuery. The tie breaker
/// capability allows results that include the same term in multiple fields to be judged better than
/// results that include this term in only the best of those multiple fields, without confusing this
/// with the better case of two different terms in the multiple fields.
#[derive(Clone, Debug)]
pub struct DisjunctionMaxQuery {
  disjuncts: HashMap<Query, usize>,
  tie_breaker_multiplier: f32,
  ordered_queries: Arc<Vec<Query>>,
  id: Identity,
}
impl DisjunctionMaxQuery {
  /// Creates a new DisjunctionMaxQuery
  ///
  /// # Parameters
  ///
  /// - `disjuncts`: an array, vector, or iterator of [`Query`] values to add
  /// - `tie_breaker_multiplier`: the score of each non-maximum disjunct for a document is multiplied
  ///   by this weight and added into the final score. If non-zero, the value should be small, on
  ///   the order of 0.1, which says that 10 occurrences of word in a lower-scored field that is
  ///   also in a higher scored field is just as good as a unique word in the lower scored field
  ///   (i.e., one that is not in any higher scored field.
  #[cfg_attr(test, allow(clippy::mutable_key_type))]
  pub fn new<I>(disjuncts: I, tie_breaker_multiplier: f32) -> Result<Self>
  where
    I: IntoIterator<Item = Query>,
  {
    // NaN passes this pair of primitive comparisons.
    if tie_breaker_multiplier < 0.0 || tie_breaker_multiplier > 1.0 {
      return Err(LuceneError::illegal_argument(
        "tie_breaker_multiplier must be in [0, 1]",
      ));
    }

    let disjuncts: Vec<Query> = disjuncts.into_iter().collect();
    let mut multiset = HashMap::new();
    for query in disjuncts.iter() {
      if matches!(query, Query::Term(_))
        && let Some(count) = multiset.get_mut(query)
      {
        *count += 1;
        continue;
      }
      *multiset.entry(query.clone()).or_insert(0usize) += 1;
    }

    Ok(Self {
      disjuncts: multiset,
      tie_breaker_multiplier,
      ordered_queries: Arc::new(disjuncts),
      id: Identity::new(),
    })
  }
  #[cfg_attr(test, allow(clippy::mutable_key_type))]
  pub fn get_disjuncts(&self) -> &HashMap<Query, usize> {
    &self.disjuncts
  }
  pub fn get_tie_breaker_multiplier(&self) -> f32 {
    self.tie_breaker_multiplier
  }
}

impl HasIdentity for DisjunctionMaxQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl QueryBase for DisjunctionMaxQuery {
  type Weight<IRC, SIM>
    = DisjunctionMaxWeight<IRC, SIM>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static;

  fn to_string(&self, field: &str) -> Result<String> {
    let mut result = String::from("(");

    for (index, subquery) in self.ordered_queries.iter().enumerate() {
      if index > 0 {
        result.push_str(" | ");
      }
      if matches!(subquery, Query::Boolean(_)) {
        let text = subquery.to_string(field)?;
        result.push('(');
        result.push_str(&text);
        result.push(')');
      } else {
        result.push_str(&subquery.to_string(field)?);
      }
    }

    result.push(')');

    if self.tie_breaker_multiplier != 0.0 {
      result.push('~');
      write!(result, "{:.1}", self.tie_breaker_multiplier)?;
    }

    Ok(result)
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
    DisjunctionMaxWeight::new(searcher, self, *score_mode, boost)
  }

  fn rewrite<IRC, SIM>(&self, index_searcher: &IndexSearcher<IRC, SIM>) -> Result<Option<Query>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    IndexSearcher<IRC, SIM>: Sync,
    Self: Sized,
  {
    if self.ordered_queries.is_empty() {
      return Ok(Some(
        MatchNoDocsQuery::with_reason("empty DisjunctionMaxQuery").into(),
      ));
    }

    if self.ordered_queries.len() == 1 {
      return Ok(Some(self.ordered_queries[0].clone()));
    }

    if self.tie_breaker_multiplier == 1.0 {
      let mut builder = Builder::new();
      if self.ordered_queries.len() <= get_max_clause_count() {
        builder.clauses.reserve_exact(self.ordered_queries.len());
      }
      for sub in self.ordered_queries.iter() {
        builder.add(sub.clone(), Occur::Should)?;
      }
      return Ok(Some(builder.build().into()));
    }

    let mut rewritten_disjuncts = None;
    for (index, sub) in self.ordered_queries.iter().enumerate() {
      let rewritten_sub = sub.rewrite(index_searcher)?;
      if rewritten_sub.is_some() {
        let disjuncts = rewritten_disjuncts.get_or_insert_with(|| {
          let mut disjuncts = Vec::with_capacity(self.ordered_queries.len());
          disjuncts.resize_with(index, || None);
          disjuncts
        });
        disjuncts.push(rewritten_sub);
      } else if let Some(disjuncts) = &mut rewritten_disjuncts {
        disjuncts.push(None);
      }
    }

    if let Some(rewritten_disjuncts) = rewritten_disjuncts {
      Ok(Some(
        DisjunctionMaxQuery::new(
          rewritten_disjuncts
            .into_iter()
            .zip(self.ordered_queries.iter())
            .map(|(rewritten, original)| rewritten.unwrap_or_else(|| original.clone())),
          self.tie_breaker_multiplier,
        )?
        .into(),
      ))
    } else {
      Ok(None)
    }
  }

  fn visit<QV>(&self, visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    let query = self.into();
    let mut visitor = visitor.get_sub_visitor(Occur::Should, query);
    for (disjunct, count) in &self.disjuncts {
      for _ in 0..*count {
        disjunct.visit(&mut visitor)?;
      }
    }
    Ok(())
  }
}
impl PartialEq for DisjunctionMaxQuery {
  fn eq(&self, other: &Self) -> bool {
    // Compare distinct instances with primitive `==`; the identity case keeps `Eq`
    // reflexive when the accepted tie breaker is NaN.
    std::ptr::eq(self, other)
      || (self.tie_breaker_multiplier == other.tie_breaker_multiplier
        && self.disjuncts == other.disjuncts)
  }
}

impl Eq for DisjunctionMaxQuery {}

impl Hash for DisjunctionMaxQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: std::hash::Hasher,
  {
    CoreHelper::hash_bits_f32_for_primitive_eq(self.tie_breaker_multiplier).hash(state);

    let mut entries: SmallVec<[_; 8]> = SmallVec::with_capacity(self.disjuncts.len());
    entries.extend(
      self
        .disjuncts
        .iter()
        .map(|(query, count)| (query, count, OnceCell::new())),
    );
    entries.sort_by(|a, b| {
      // compare name
      let cmp = a.0.name().cmp(b.0.name());
      if cmp != std::cmp::Ordering::Equal {
        return cmp;
      }
      // compare hash
      let ah = a.2.get_or_init(|| {
        let mut hash = DefaultHasher::new();
        a.0.hash(&mut hash);
        hash.finish()
      });

      let bh = b.2.get_or_init(|| {
        let mut hash = DefaultHasher::new();
        b.0.hash(&mut hash);
        hash.finish()
      });

      // compare count
      let cmp = ah.cmp(bh);
      if cmp != std::cmp::Ordering::Equal {
        return cmp;
      }
      a.1.cmp(b.1)
    });

    for (query, count, _) in entries {
      query.hash(state);
      count.hash(state);
    }
  }
}
/// the Weight for DisjunctionMaxQuery, used to normalize, score and explain these queries
pub struct DisjunctionMaxWeight<
  IRC: IndexReaderContext + 'static,
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> where
  SIM: Similarity,
{
  parent_query: DisjunctionMaxQuery,
  tie_breaker_multiplier: f32,
  score_mode: ScoreMode,
  weights: Box<[QueryWeight<IRC, SIM>]>,
}
impl<IRC, SIM> DisjunctionMaxWeight<IRC, SIM>
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
  IndexSearcher<IRC, SIM>: Sync,
{
  pub fn new(
    searcher: &IndexSearcher<IRC, SIM>,
    query: DisjunctionMaxQuery,
    score_mode: ScoreMode,
    boost: f32,
  ) -> Result<Self>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let mut weights = Vec::with_capacity(query.get_disjuncts().len());
    for query in query.disjuncts.keys().cloned() {
      let weight = query.create_weight(searcher, &score_mode, boost)?;
      weights.push(weight);
    }
    let tie_breaker_multiplier = query.get_tie_breaker_multiplier();
    Ok(Self {
      parent_query: query,
      tie_breaker_multiplier,
      score_mode,
      weights: weights.into_boxed_slice(),
    })
  }
}

impl<IRC, SIM> SegmentCacheable<IRC> for DisjunctionMaxWeight<IRC, SIM>
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
{
  fn is_cacheable(&self, ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    if self.weights.len() > BOOLEAN_REWRITE_TERM_COUNT_THRESHOLD {
      // Disallow caching large dismax queries to not encourage users
      // to build large dismax queries as a workaround to the fact that
      // we disallow caching large TermInSetQueries.
      return Ok(false);
    }

    for w in &self.weights {
      if !w.is_cacheable(ctx)? {
        return Ok(false);
      }
    }

    Ok(true)
  }
}

impl<IRC, SIM> Weight<IRC, SIM> for DisjunctionMaxWeight<IRC, SIM>
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
{
  fn matches<'a>(
    &'a self,
    context: &'a LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &'a IndexSearcher<IRC, SIM>,
  ) -> Result<Option<crate::core::search::query::QueryWeightMatches<'a, IRC, SIM>>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let mut matches = Vec::new();
    for weight in &self.weights {
      if let Some(weight_matches) = weight.matches(context, doc, searcher)? {
        if matches.is_empty() {
          matches.reserve_exact(self.weights.len());
        }
        matches.push(weight_matches);
      }
    }
    Ok(from_sub_matches(matches))
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
    let mut matched = false;
    let mut max = 0.0f64;
    let mut other_sum = 0.0f64;
    let mut subs = Vec::with_capacity(self.weights.len());

    for wt in &self.weights {
      let e = wt.explain(context, doc, searcher)?;
      if e.is_match() {
        if !matched {
          subs.clear();
          matched = true;
        }
        let score = e.get_value().to_f64().ok_or_else(|| {
          LuceneError::illegal_state(format!(
            "Explanation value is not a number: {:?}",
            e.get_value()
          ))
        })?;
        subs.push(e);
        if score >= max {
          other_sum += max;
          max = score;
        } else {
          other_sum += score;
        }
      } else if !matched {
        subs.push(e);
      }
    }

    if matched {
      let score = (max + other_sum * self.tie_breaker_multiplier as f64) as f32;
      let desc = if self.tie_breaker_multiplier == 0.0 {
        Cow::Borrowed("max of:")
      } else {
        Cow::Owned(format!(
          "max plus {} times others of:",
          self.tie_breaker_multiplier
        ))
      };
      Ok(Explanation::match_(score, desc, subs))
    } else {
      Ok(Explanation::no_match("No matching clause", subs))
    }
  }

  fn get_query(&self) -> QueryRef<'_> {
    QueryRef::DisjunctionMax(&self.parent_query)
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
    let mut scorer_suppliers = Vec::with_capacity(self.weights.len());
    for w in &self.weights {
      let ss = w.scorer_supplier(context, searcher)?;
      if let Some(ss) = ss {
        scorer_suppliers.push(ss);
      }
    }

    if scorer_suppliers.is_empty() {
      Ok(None)
    } else if scorer_suppliers.len() == 1 {
      Ok(Some(scorer_suppliers.remove(0)))
    } else {
      let v = ScorerSupplierImpl::new(
        -1,
        scorer_suppliers,
        self.tie_breaker_multiplier,
        self.score_mode,
      );
      Ok(Some(Box::new(v).into()))
    }
  }
}

pub struct ScorerSupplierImpl<
  IRC: IndexReaderContext + 'static,
  SIM = crate::core::search::similarities_impl::bm25_similarity::BM25Similarity,
> where
  SIM: Similarity,
{
  cost: i64,
  scorer_suppliers: Vec<QueryWeightSs<IRC, SIM>>,
  tie_breaker_multiplier: f32,
  score_mode: ScoreMode,
}
impl<IRC, SIM> ScorerSupplierImpl<IRC, SIM>
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
{
  pub fn new(
    cost: i64,
    scorer_suppliers: Vec<QueryWeightSs<IRC, SIM>>,
    tie_breaker_multiplier: f32,
    score_mode: ScoreMode,
  ) -> Self {
    Self {
      cost,
      scorer_suppliers,
      tie_breaker_multiplier,
      score_mode,
    }
  }
}
impl<IRC, SIM> ScorerSupplier<IRC, SIM> for ScorerSupplierImpl<IRC, SIM>
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
{
  type Scorer = QueryWeightSsScorer<IRC, SIM>;
  type BulkScorer = crate::core::search::bulk_scorer::BulkScorerEnum2<
    DisjunctionMaxBulkScorer<QueryWeightSsBulkScorer<IRC, SIM>>,
    crate::core::search::weight::DefaultBulkScorer<Self::Scorer>,
  >;

  fn get(
    &mut self,
    lead_cost: i64,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Self::Scorer>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let mut scorers = Vec::with_capacity(self.scorer_suppliers.len());
    for ss in self.scorer_suppliers.iter_mut() {
      scorers.push(ss.get(lead_cost, context, searcher)?);
    }
    let sub =
      DisjunctionMaxScorer::new(self.tie_breaker_multiplier, &mut scorers, self.score_mode)?;
    match DisjunctionScorer::<_, DisjunctionMaxScorer>::prepare(scorers, self.score_mode)? {
      crate::core::search::doc_id_set_iterator::DocIdSetIteratorEnum2::A(disi) => Ok(
        QueryWeightSsScorer::<IRC, SIM>::new(DisjunctionScorer::from_disi(disi, sub)),
      ),
      crate::core::search::doc_id_set_iterator::DocIdSetIteratorEnum2::B(disi) => Ok(
        QueryWeightSsScorer::<IRC, SIM>::new(DisjunctionScorer::from_tpi(disi, sub)),
      ),
    }
  }

  fn bulk_scorer(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Option<Self::BulkScorer>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    if self.tie_breaker_multiplier == 0.0 && self.score_mode == ScoreMode::TopScores {
      let mut scorers = Vec::with_capacity(self.scorer_suppliers.len());
      for ss in self.scorer_suppliers.iter_mut() {
        if let Some(scorer) = ss.bulk_scorer(context, searcher)? {
          scorers.push(scorer);
        }
      }
      return Ok(Some(crate::core::search::bulk_scorer::BulkScorerEnum2::A(
        DisjunctionMaxBulkScorer::new(scorers)?,
      )));
    }
    Ok(Some(crate::core::search::bulk_scorer::BulkScorerEnum2::B(
      self.default_bulk_scorer(context, searcher)?,
    )))
  }

  fn cost(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<i64>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    if self.cost == -1 {
      let mut cost = 0i64;
      for ss in self.scorer_suppliers.iter_mut() {
        cost += ss.cost(context, searcher)?;
      }
      self.cost = cost;
    }
    Ok(self.cost)
  }

  fn set_top_level_scoring_clause(&mut self) -> Result<()> {
    if self.tie_breaker_multiplier == 0.0 {
      for ss in self.scorer_suppliers.iter_mut() {
        // sub scorers need to be able to skip too as calls to setMinCompetitiveScore get
        // propagated
        ss.set_top_level_scoring_clause()?;
      }
    }
    Ok(())
  }
}

impl crate::core::util::accountable::Accountable for DisjunctionMaxQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED)
  }
}
