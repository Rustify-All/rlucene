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
use crate::core::search::dummy::dummy_bulk_scorer::DummyBulkScorer;
use crate::core::search::dummy::dummy_scorer::DummyScorer;
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::util::error::lucene_error::Result;

pub struct DummyScorerSupplier;

impl<LR> ScorerSupplier<LR> for DummyScorerSupplier
where
    LR: LeafReader,
{
    type Scorer = DummyScorer;
    type BulkScorer = DummyBulkScorer;

    fn get(
        &mut self,
        _lead_cost: i64,
        _context: &LeafReaderContext<LR>,
    ) -> Result<Option<Self::Scorer>> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn bulk_scorer(
        &mut self,
        _context: &LeafReaderContext<LR>,
    ) -> Result<Option<Self::BulkScorer>> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn cost(&mut self, _context: &LeafReaderContext<LR>) -> Result<i64> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn set_top_level_scoring_clause(&mut self) -> Result<()> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}
