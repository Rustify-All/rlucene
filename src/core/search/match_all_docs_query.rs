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
use crate::core::search::constant_score_scorer::ConstantScoreDisiScorer;
use crate::core::search::query::QueryRef;
use crate::core::search::similarities_impl::similarities::Similarity;

use crate::core::index::index_reader::{Identity, IndexReader};
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::search::bulk_scorer::{BulkScorer, BulkScorerEnum2};
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::constant_score_weight::ConstantScoreWeight;
use crate::core::search::doc_id_set_iterator::AllDISI;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::doc_id_stream::{DocIdStream, DocIdStreamConsumer};
use crate::core::search::explanation::Explanation;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::query::QueryBase;
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::scorable::Scorable;
use crate::core::search::score::Score;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::weight::{DefaultBulkScorer, Weight};
use crate::core::util::bits::Bits;
use crate::core::util::core_helper::HasIdentity;
use crate::core::util::error::lucene_error::Result;
use std::fmt::Debug;
use std::hash::{Hash, Hasher};
use std::ops::Range;

/// A query that matches all documents.
#[derive(Debug, Clone)]
pub struct MatchAllDocsQuery {
  id: Identity,
}
impl Default for MatchAllDocsQuery {
  fn default() -> Self {
    Self::new()
  }
}

impl MatchAllDocsQuery {
  pub fn new() -> Self {
    MatchAllDocsQuery {
      id: Identity::new(),
    }
  }
}

impl PartialEq for MatchAllDocsQuery {
  fn eq(&self, _other: &Self) -> bool {
    true
  }
}
impl Eq for MatchAllDocsQuery {}

impl Hash for MatchAllDocsQuery {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
    0usize.hash(state);
  }
}

impl HasIdentity for MatchAllDocsQuery {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl QueryBase for MatchAllDocsQuery {
  type Weight<IRC, SIM>
    = MatchAllWeight
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static;

  fn to_string(&self, _field: &str) -> Result<String> {
    Ok("*:*".to_string())
  }

  fn create_weight<IRC, SIM>(
    self,
    _searcher: &IndexSearcher<IRC, SIM>,
    score_mode: &ScoreMode,
    boost: f32,
  ) -> Result<Self::Weight<IRC, SIM>>
  where
    SIM: Similarity,
    IRC: IndexReaderContext + 'static,
    Self: Sized,
    IndexSearcher<IRC, SIM>: Sync,
  {
    Ok(MatchAllWeight::new(boost, self, *score_mode))
  }

  fn visit<QV>(&self, visitor: &mut QV) -> Result<()>
  where
    QV: QueryVisitor,
  {
    visitor.visit_leaf(self.into())
  }
}

pub struct MatchAllWeight {
  base: ConstantScoreWeight,
  parent_query: MatchAllDocsQuery,
  score_mode: ScoreMode,
}
impl MatchAllWeight {
  pub fn new(score: f32, query: MatchAllDocsQuery, score_mode: ScoreMode) -> Self {
    Self {
      base: ConstantScoreWeight::new(score),
      parent_query: query,
      score_mode,
    }
  }
}

impl<IRC> SegmentCacheable<IRC> for MatchAllWeight
where
  IRC: IndexReaderContext + 'static,
{
  fn is_cacheable(&self, _ctx: &LeafReaderContext<IRCLeafReader<IRC>>) -> Result<bool> {
    Ok(true)
  }
}

impl<IRC, SIM> Weight<IRC, SIM> for MatchAllWeight
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
{
  fn explain(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    doc: i32,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Explanation>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let scorer = self.scorer(context, searcher)?;
    self
      .base
      .explain(scorer, doc, self.parent_query.to_string("")?)
  }

  fn get_query(&self) -> QueryRef<'_> {
    QueryRef::MatchAllDocs(&self.parent_query)
  }

  type ScorerSupplier = MatchAllDocsScorerSupplier;

  fn scorer_supplier(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Option<Self::ScorerSupplier>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let v = MatchAllDocsScorerSupplier::new(
      self.score_mode,
      self.base.clone(),
      context.reader().max_doc()?,
    );
    Ok(Some(v))
  }

  fn count(
    &self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<i32>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    context.reader().num_docs()
  }
}
impl Debug for MatchAllWeight {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(f, "weight({:?})", MatchAllDocsQuery::new())
  }
}
pub struct MatchAllDocsScorerSupplier {
  score_mode: ScoreMode,
  weight: ConstantScoreWeight,
  max_doc: i32,
}
impl MatchAllDocsScorerSupplier {
  pub fn new(score_mode: ScoreMode, weight: ConstantScoreWeight, max_doc: i32) -> Self {
    Self {
      score_mode,
      weight,
      max_doc,
    }
  }
}
impl<IRC, SIM> ScorerSupplier<IRC, SIM> for MatchAllDocsScorerSupplier
where
  SIM: Similarity,
  IRC: IndexReaderContext + 'static,
{
  type Scorer = ConstantScoreDisiScorer<AllDISI>;
  type BulkScorer = MatchAllBulkScorerEnum;

  fn get(
    &mut self,
    _lead_cost: i64,
    _context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Self::Scorer>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let score = self.weight.score();
    let v = ConstantScoreScorer::from_disi(score, self.score_mode, AllDISI::new(self.max_doc));
    Ok(v)
  }

  fn bulk_scorer(
    &mut self,
    context: &LeafReaderContext<IRCLeafReader<IRC>>,
    searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<Option<Self::BulkScorer>>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    let v = if !self.score_mode.is_exhaustive() {
      let opt = self.default_bulk_scorer(context, searcher)?;
      MatchAllBulkScorerEnum::B(opt)
    } else {
      let score = self.weight.score();
      MatchAllBulkScorerEnum::A(MatchAllBulkScorer::new(self.max_doc, score))
    };
    Ok(Some(v))
  }

  fn cost(
    &mut self,
    _context: &LeafReaderContext<IRCLeafReader<IRC>>,
    _searcher: &IndexSearcher<IRC, SIM>,
  ) -> Result<i64>
  where
    IndexSearcher<IRC, SIM>: Sync,
  {
    Ok(self.max_doc as i64)
  }
}
pub struct MatchAllBulkScorer {
  max_doc: i32,
  score: f32,
}
impl MatchAllBulkScorer {
  pub fn new(max_doc: i32, score: f32) -> Self {
    Self { max_doc, score }
  }
}
struct ContiguousDocs(Range<i32>);

impl DocIdStream for ContiguousDocs {
  fn for_each<DC: DocIdStreamConsumer + ?Sized>(&mut self, consumer: &mut DC) -> Result<()> {
    self.0.try_for_each(|doc| consumer.accept(doc))
  }

  fn count<SC: Scorable + ?Sized>(&mut self, _scorer: &mut SC) -> Result<i32> {
    let count = self.0.len() as i32;
    self.0.start = self.0.end;
    Ok(count)
  }
}

impl BulkScorer for MatchAllBulkScorer {
  fn score<CLC: crate::core::search::leaf_collector::LeafCollector, AB: Bits + ?Sized>(
    &mut self,
    collector: &mut CLC,
    accept_docs: Option<&AB>,
    min: i32,
    max: i32,
  ) -> Result<i32> {
    let max = std::cmp::min(max, self.max_doc);
    let mut scorer = Score::new(self.score);
    collector.set_scorer(&mut scorer)?;
    if let Some(bits) = accept_docs {
      for doc in min..max {
        if bits.get(doc as usize)? {
          collector.collect(doc, &mut scorer)?;
        }
      }
    } else if min < max {
      collector.collect_stream(&mut ContiguousDocs(min..max), &mut scorer)?;
    }
    if max == self.max_doc {
      Ok(NO_MORE_DOCS)
    } else {
      Ok(max)
    }
  }

  fn cost(&mut self) -> Result<i64> {
    Ok(self.max_doc as i64)
  }
}
pub type MatchAllBulkScorerEnum =
  BulkScorerEnum2<MatchAllBulkScorer, DefaultBulkScorer<ConstantScoreDisiScorer<AllDISI>>>;

impl crate::core::util::accountable::Accountable for MatchAllDocsQuery {
  fn ram_bytes_used(&self) -> crate::core::util::error::lucene_error::Result<i64> {
    Ok(crate::core::util::ram_usage_estimator::QUERY_DEFAULT_RAM_BYTES_USED)
  }
}
