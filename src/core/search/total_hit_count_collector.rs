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
use crate::core::index::leaf_reader::LeafReader;
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
use crate::core::util::error::lucene_error::Result;
use std::rc::Rc;

pub struct TotalHitCountCollector<W>
where
    W: Weight,
{
    weight: Option<Rc<W>>,
    total_hit: i32,
}
impl<W> TotalHitCountCollector<W>
where
    W: Weight,
{
    pub fn new() -> Self {
        Self {
            weight: None,
            total_hit: 0,
        }
    }
}
impl<W> Collector for TotalHitCountCollector<W>
where
    W: Weight,
{
    type LeafCollector = TotalHitCountLeafCollector;

    fn get_leaf_collector<LR>(&self, context: &LeafReaderContext<LR>) -> Result<Self::LeafCollector>
    where
        LR: LeafReader,
    {
        Ok(TotalHitCountLeafCollector)
    }

    fn score_mode(&self) -> &ScoreMode {
        &ScoreMode::CompleteNoScores
    }

    type Weight = W;

    fn set_weight(&mut self, weight: Rc<Self::Weight>) {
        self.weight = Some(weight);
    }
}

pub struct TotalHitCountLeafCollector;

impl LeafCollector for TotalHitCountLeafCollector {
    fn set_scorer<S, C>(&mut self, scorer: ScorerEnum<S, C>) -> Result<()>
    where
        S: Scorer,
        C: Scorable,
    {
        todo!()
    }

    type Scorer = DummyScorer;

    fn collect(&mut self, doc: i32) -> Result<()> {
        todo!()
    }

    fn collect_stream<DS>(&mut self, stream: &mut DS) -> Result<()>
    where
        DS: DocIdStream,
    {
        todo!()
    }

    type DocIdSetIterator = DummyDocIdSetIterator;
}
