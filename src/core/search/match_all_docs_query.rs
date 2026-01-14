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
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::query_timeout::QueryTimeout;
use crate::core::index::term_states::TermStates;
use crate::core::search::QueryCache;
use crate::core::search::bulk_scorer::{BulkScorer, BulkScorerEnum2};
use crate::core::search::constant_score_scorer::ConstantScoreScorer;
use crate::core::search::constant_score_weight::ConstantScoreWeight;
use crate::core::search::doc_id_set_iterator::AllDISI;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::search::dummy::dummy_two_phase_iterator::DummyTwoPhaseIterator;
use crate::core::search::explanation::Explanation;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::leaf_collector::LeafCollector;
use crate::core::search::matches_utils::MatchWithNoTerms;
use crate::core::search::query::{Query, QueryBase};
use crate::core::search::query_caching_policy::QueryCachingPolicy;
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score::Score;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::weight::{DefaultBulkScorer, Weight};
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::Result;
use std::fmt::Debug;
use std::hash::Hash;
use std::marker::PhantomData;
use std::sync::Arc;

/// A query that matches all documents.
#[derive(Hash, PartialEq, Eq, Debug, Clone)]
pub struct MatchAllDocsQuery;
impl Default for MatchAllDocsQuery {
    fn default() -> Self {
        Self::new()
    }
}

impl MatchAllDocsQuery {
    pub fn new() -> Self {
        MatchAllDocsQuery
    }
}

impl QueryBase for MatchAllDocsQuery {
    fn as_string(&self, _field: &str) -> String {
        "*:*".to_string()
    }

    type Weight<S, IRC, QCP, QC>
        = MatchAllWeight<IRC::LeafReader>
    where
        S: Similarity,
        IRC: IndexReaderContext,
        QCP: QueryCachingPolicy,
        QC: QueryCache<IRC::LeafReader>;

    fn create_weight<S, IRC, QT, QCP, QC>(
        self,
        _searcher: &IndexSearcher<IRC, S, QT, QCP, QC>,
        score_mode: &ScoreMode,
        boost: f32,
        _per_reader_term_state: Option<TermStates<IRCTermState<IRC>>>,
    ) -> Result<Self::Weight<S, IRC, QCP, QC>>
    where
        IRC: IndexReaderContext,
        S: Similarity,
        QT: QueryTimeout,
        QCP: QueryCachingPolicy,
        QC: QueryCache<IRC::LeafReader>,
        Self: Sized,
    {
        Ok(MatchAllWeight::new(boost, self, *score_mode))
    }

    type RewriteQuery = MatchAllDocsQuery;

    fn visit<QV>(&self, _visitor: &QV)
    where
        QV: QueryVisitor,
    {
        todo!()
    }
}

pub struct MatchAllWeight<LR>
where
    LR: LeafReader,
{
    base: ConstantScoreWeight,
    parent_query: Arc<Query>,
    score_mode: ScoreMode,
    _leaf_reader: PhantomData<LR>,
}
impl<LR> MatchAllWeight<LR>
where
    LR: LeafReader,
{
    pub fn new(score: f32, query: MatchAllDocsQuery, score_mode: ScoreMode) -> Self {
        Self {
            base: ConstantScoreWeight::new(score),
            parent_query: Arc::new(query.into()),
            score_mode,
            _leaf_reader: PhantomData,
        }
    }
}

impl<LR> SegmentCacheable<LR> for MatchAllWeight<LR>
where
    LR: LeafReader,
{
    fn is_cacheable(&self, _ctx: &LeafReaderContext<LR>) -> Result<bool> {
        Ok(true)
    }
}

impl<LR> Weight<LR> for MatchAllWeight<LR>
where
    LR: LeafReader,
{
    type Matches = MatchWithNoTerms;

    fn matches(&self, context: &LeafReaderContext<LR>, doc: i32) -> Result<Option<Self::Matches>> {
        self.default_matches(context, doc)
    }

    fn explain(&self, context: &LeafReaderContext<LR>, doc: i32) -> Result<Explanation> {
        let scorer = self.scorer(context)?;
        self.base
            .explain(scorer, doc, self.parent_query.as_string(""))
    }

    fn get_query(&self) -> Arc<Query> {
        self.parent_query.clone()
    }

    type ScorerSupplier = MatchAllDocsScorerSupplier;

    fn scorer_supplier(
        &self,
        context: &LeafReaderContext<LR>,
    ) -> Result<Option<Self::ScorerSupplier>> {
        Ok(Some(MatchAllDocsScorerSupplier::new(
            self.score_mode,
            self.base.clone(),
            context.reader().max_doc()?,
        )))
    }

    fn count(&self, context: &LeafReaderContext<LR>) -> Result<i32> {
        context.reader().num_docs()
    }
}
impl<LR> std::fmt::Debug for MatchAllWeight<LR>
where
    LR: LeafReader,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "weight({:?})", MatchAllDocsQuery)
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
impl<LR> ScorerSupplier<LR> for MatchAllDocsScorerSupplier
where
    LR: LeafReader,
{
    type Scorer = ConstantScoreScorer<AllDISI, DummyTwoPhaseIterator>;
    type BulkScorer = MatchAllBulkScorerEnum<Self::Scorer>;

    fn get(
        &mut self,
        _lead_cost: i64,
        _context: &LeafReaderContext<LR>,
    ) -> Result<Option<Self::Scorer>> {
        let score = self.weight.score();
        Ok(Some(ConstantScoreScorer::with_disi(
            score,
            self.score_mode,
            AllDISI::new(self.max_doc),
        )))
    }

    fn bulk_scorer(&mut self, context: &LeafReaderContext<LR>) -> Result<Option<Self::BulkScorer>> {
        if !self.score_mode.is_exhaustive() {
            let opt = self.default_bulk_scorer(context)?;
            Ok(Some(MatchAllBulkScorerEnum::B(opt)))
        } else {
            let score = self.weight.score();
            Ok(Some(MatchAllBulkScorerEnum::A(MatchAllBulkScorer::new(
                self.score_mode,
                self.max_doc,
                score,
            ))))
        }
    }

    fn cost(&mut self, _context: &LeafReaderContext<LR>) -> Result<i64> {
        Ok(self.max_doc as i64)
    }
}
pub struct MatchAllBulkScorer {
    score_mode: ScoreMode,
    max_doc: i32,
    score: f32,
}
impl MatchAllBulkScorer {
    pub fn new(score_mode: ScoreMode, max_doc: i32, score: f32) -> Self {
        Self {
            score_mode,
            max_doc,
            score,
        }
    }
}
impl BulkScorer for MatchAllBulkScorer {
    fn score<LC, B>(
        &mut self,
        collector: &mut LC,
        accept_docs: Option<&B>,
        min: i32,
        max: i32,
    ) -> Result<i32>
    where
        LC: LeafCollector,
        B: Bits,
    {
        let max = std::cmp::min(max, self.max_doc);
        let mut scorer = Score::new(self.score);
        collector.set_scorer(&mut scorer)?;
        for doc in min..max {
            if match accept_docs {
                None => true,
                Some(bits) => bits.get(doc as usize)?,
            } {
                collector.collect(doc, &mut scorer)?;
            }
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
pub type MatchAllBulkScorerEnum<T> = BulkScorerEnum2<MatchAllBulkScorer, DefaultBulkScorer<T>>;

#[cfg(test)]
mod tests {
    use crate::core::document::document::Document;
    use crate::core::document::field::Store;
    use crate::core::document::field_type::FieldType;
    use crate::core::index::directory_reader::directory_reader_util;
    use crate::core::index::index_writer::{IndexWriter, IndexWriterBase};
    use crate::core::index::live_index_writer_config::LiveIndexWriterConfig;
    use crate::core::search::match_all_docs_query::MatchAllDocsQuery;
    use crate::core::search::top_score_doc_collector_manager::TopScoreDocCollectorManager;
    use crate::core::search::total_hits::Relation;
    use crate::core::store::directory::Directory;
    use crate::core::util::error::lucene_error::Result;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{
        new_directory_shared, new_index_writer_config, new_searcher_with_reader,
        new_searcher_with_threads, new_text_field, random,
    };
    use std::collections::HashMap;
    use std::sync::Arc;

    #[allow(dead_code)] // for quick search
    struct TestMatchAllDocsQuery;
    #[test]
    fn test_query() -> Result<()> {
        // TODO BooleanQuery未实现
        Ok(())
    }
    #[test]
    fn test_equals() -> Result<()> {
        let q1 = MatchAllDocsQuery::new();
        let q2 = MatchAllDocsQuery::new();
        assert_eq!(q1, q2);
        Ok(())
    }
    fn add_doc<D, L, B>(
        text: &str,
        iw: &IndexWriter<D, L, B>,
        field_to_type: &mut HashMap<String, FieldType>,
    ) -> Result<()>
    where
        D: Directory,
        L: LiveIndexWriterConfig,
        B: IndexWriterBase,
    {
        let mut doc = Document::new();
        let field = new_text_field("key", text, Store::Yes, field_to_type)?;
        doc.add(field);
        iw.add_document(doc)?;
        Ok(())
    }
    #[test]
    fn test_early_termination() -> Result<()> {
        let mut random = random();
        let dir = new_directory_shared(&mut random)?;
        // TODO: 未实现 MockAnalyzer / NoMergePolicy
        let mut config = new_index_writer_config(&mut random);
        config.set_max_buffered_docs(2);
        let iw = IndexWriter::new(dir.clone(), config)?;
        let mut field_types = HashMap::new();
        let num_docs = 500;
        for i in 0..num_docs {
            let text = format!("doc{}", i);
            add_doc(&text, &iw, &mut field_types)?;
        }

        let ir = directory_reader_util::open_with_writer(&iw)?;
        let ir_arc = Arc::new(ir);

        let single_threaded_searcher =
            new_searcher_with_threads(ir_arc.clone(), true, true, false)?;

        let total_hits_threshold = 200;
        let collector_mgr = TopScoreDocCollectorManager::new(10, total_hits_threshold)?;

        let top_docs = single_threaded_searcher
            .search_with_collector_manager(MatchAllDocsQuery::new(), &collector_mgr)?;

        assert_eq!(top_docs.total_hits.value(), total_hits_threshold + 1);
        assert_eq!(
            top_docs.total_hits.relation(),
            Relation::GreaterThanOrEqualTo
        );

        let searcher = new_searcher_with_reader(ir_arc.clone())?;
        let collector_mgr = TopScoreDocCollectorManager::new(10, num_docs)?;

        let top_docs =
            searcher.search_with_collector_manager(MatchAllDocsQuery::new(), &collector_mgr)?;

        assert_eq!(top_docs.total_hits.value(), num_docs);
        assert_eq!(top_docs.total_hits.relation(), Relation::EqualTo);
        iw.close()?;
        Ok(())
    }
}
