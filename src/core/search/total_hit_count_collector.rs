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
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::search::collector::Collector;
use crate::core::search::doc_id_stream::DocIdStream;
use crate::core::search::dummy::dummy_doc_id_set_iterator::DummyDocIdSetIterator;
use crate::core::search::dummy::dummy_scorer::DummyScorer;
use crate::core::search::leaf_collector::LeafCollector;
use crate::core::search::scorable::{Scorable, ScorerEnum};
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer::Scorer;
use crate::core::search::weight::Weight;
use crate::core::util::error::lucene_error::{LuceneError, Result};
/// Just counts the total number of hits. This is the collector behind [`IndexSearcher::count`](crate::core::search::index_searcher::IndexSearcher::count).
/// When the [`Weight`] implements [`Weight::count`], this collector will skip collecting segments.
pub struct TotalHitCountCollector {
    pub(crate) total_hit: i32,
}
impl TotalHitCountCollector {
    pub fn new() -> Self {
        Self { total_hit: 0 }
    }
    /// Returns how many hits matched the search.
    pub fn get_total_hits(&self) -> i32 {
        self.total_hit
    }
}
impl Collector for TotalHitCountCollector {
    type LeafCollector<'a>
        = TotalHitCountLeafCollector<'a>
    where
        Self: 'a;

    fn get_leaf_collector<'a, W>(
        &'a mut self,
        context: &LeafReaderContext<<W::IndexReaderContext as IndexReaderContext>::LeafReader>,
        weight: Option<&mut W>,
    ) -> Result<Self::LeafCollector<'a>>
    where
        W: Weight,
    {
        let leaf_count = match weight {
            Some(w) => w.count(context)?,
            None => -1,
        };
        if leaf_count != -1 {
            self.total_hit += leaf_count;
            return Err(LuceneError::collection_terminated_error(""));
        }
        Ok(TotalHitCountLeafCollector::new(self))
    }

    fn score_mode(&self) -> &ScoreMode {
        &ScoreMode::CompleteNoScores
    }
}

pub struct TotalHitCountLeafCollector<'a> {
    collector: &'a mut TotalHitCountCollector,
}

impl<'a> TotalHitCountLeafCollector<'a> {
    fn new(collector: &'a mut TotalHitCountCollector) -> Self {
        Self { collector }
    }
}

impl<'a> LeafCollector for TotalHitCountLeafCollector<'a> {
    fn set_scorer<S, C>(&mut self, _scorer: ScorerEnum<S, C>) -> Result<()>
    where
        S: Scorer,
        C: Scorable,
    {
        Ok(())
    }

    type Scorer = DummyScorer;

    fn collect(&mut self, _doc: i32) -> Result<()> {
        self.collector.total_hit += 1;
        Ok(())
    }

    fn collect_stream<DS>(&mut self, stream: &mut DS) -> Result<()>
    where
        DS: DocIdStream,
    {
        self.collector.total_hit += stream.count()?;
        Ok(())
    }

    type DocIdSetIterator = DummyDocIdSetIterator;
}
