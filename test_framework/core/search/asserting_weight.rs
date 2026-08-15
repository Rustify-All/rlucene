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
use crate::core::search::explanation::Explanation;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::query::{
  Query, QueryWeight, QueryWeightMatches, QueryWeightSs, QueryWeightSsBulkScorer,
};
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::weight::Weight;
use crate::core::util::error::lucene_error::Result;
use crate::test_framework::core::search::asserting_bulk_scorer::AssertingBulkScorer;
use crate::test_framework::core::search::asserting_matches::AssertingMatches;
use crate::test_framework::core::search::asserting_scorer::AssertingScorer;
use crate::test_framework::core::util::lucene_test_case::{random_from_seed, usually};
use rand::RngExt;
use std::sync::Arc;

pub(crate) struct AssertingWeight<IRC> {
  random_seed: u64,
  in_: QueryWeight<IRC>,
  _score_mode: ScoreMode,
}

impl<IRC> AssertingWeight<IRC>
where
  IRC: IndexReaderContext,
{
  pub(crate) fn new(random_seed: u64, in_: QueryWeight<IRC>, score_mode: ScoreMode) -> Self {
    Self {
      random_seed,
      in_,
      _score_mode: score_mode,
    }
  }
}

impl<IRC> SegmentCacheable<IRC> for AssertingWeight<IRC>
where
  IRC: IndexReaderContext,
{
  fn is_cacheable(&self, ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    self.in_.is_cacheable(ctx)
  }
}

impl<IRC> Weight<IRC> for AssertingWeight<IRC>
where
  IRC: IndexReaderContext,
{
  fn matches<'a>(
    &'a self,
    context: &'a LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &'a IndexSearcher<IRC>,
  ) -> Result<Option<crate::core::search::query::QueryWeightMatches<'a>>> {
    Ok(
      self
        .in_
        .matches(context, doc, searcher)?
        .map(|matches| QueryWeightMatches::Matches(Box::new(AssertingMatches::new(matches)))),
    )
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
    let in_scorer_supplier = self.in_.scorer_supplier(context, searcher)?;
    let in_scorer_supplier = match in_scorer_supplier {
      None => return Ok(None),
      Some(in_scorer_supplier) => in_scorer_supplier,
    };
    let mut random = random_from_seed(self.random_seed);
    Ok(Some(Box::new(AssertingScorerSupplier {
      random_seed: random.random(),
      score_mode: self._score_mode,
      in_scorer_supplier,
      get_called: false,
      top_level_scoring_clause: false,
    })))
  }

  fn count(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<i32> {
    let count = self.in_.count(context, searcher)?;
    let num_docs = context.reader().num_docs()?;
    assert!(
      count >= -1 && count <= num_docs,
      "count={}, numDocs={}",
      count,
      num_docs
    );
    Ok(count)
  }
}

struct AssertingScorerSupplier<IRC> {
  random_seed: u64,
  score_mode: ScoreMode,
  in_scorer_supplier: QueryWeightSs<IRC>,
  get_called: bool,
  top_level_scoring_clause: bool,
}

impl<IRC> ScorerSupplier<IRC> for AssertingScorerSupplier<IRC>
where
  IRC: IndexReaderContext,
{
  type Scorer = crate::core::search::query::QueryWeightSsScorer;
  type BulkScorer = QueryWeightSsBulkScorer;

  fn get(
    &mut self,
    lead_cost: i64,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Self::Scorer> {
    assert!(!self.get_called);
    self.get_called = true;
    assert!(lead_cost >= 0, "{}", lead_cost);
    let scorer = self.in_scorer_supplier.get(lead_cost, context, searcher)?;
    Ok(Box::new(AssertingScorer::wrap(
      self.random_seed,
      scorer,
      self.score_mode,
      self.top_level_scoring_clause,
    )))
  }

  fn bulk_scorer(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<Option<Self::BulkScorer>> {
    assert!(!self.get_called);

    let mut random = random_from_seed(self.random_seed);
    if usually(&mut random) {
      self.get_called = true;
      Ok(
        self
          .in_scorer_supplier
          .bulk_scorer(context, searcher)?
          .map(|bulk_scorer| {
            AssertingBulkScorer::wrap(
              random.random(),
              bulk_scorer,
              context
                .reader()
                .max_doc()
                .expect("max_doc should be available"),
              self.score_mode,
            )
          }),
      )
    } else {
      let scorer = self.default_bulk_scorer(context, searcher)?;
      assert!(self.get_called);
      Ok(Some(AssertingBulkScorer::wrap(
        random.random(),
        Box::new(scorer),
        context.reader().max_doc()?,
        self.score_mode,
      )))
    }
  }

  fn cost(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC>,
  ) -> Result<i64> {
    let cost = self.in_scorer_supplier.cost(context, searcher)?;
    assert!(cost >= 0);
    Ok(cost)
  }

  fn set_top_level_scoring_clause(&mut self) -> Result<()> {
    assert!(!self.get_called);
    self.top_level_scoring_clause = true;
    self.in_scorer_supplier.set_top_level_scoring_clause()
  }
}
