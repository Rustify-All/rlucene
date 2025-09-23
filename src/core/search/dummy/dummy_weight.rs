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
use crate::core::search::dummy::dummy_matches::DummyMatches;
use crate::core::search::dummy::dummy_query::DummyQuery;
use crate::core::search::dummy::dummy_scorer_supplier::DummyScorerSupplier;
use crate::core::search::explanation::Explanation;
use crate::core::search::matches_utils::MatchWithNoTerms;
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::weight::Weight;
use crate::core::util::error::lucene_error::Result;
use std::marker::PhantomData;

pub struct DummyWeight<LR> {
    _phantom: PhantomData<LR>,
}

impl<LR> SegmentCacheable for DummyWeight<LR>
where
    LR: LeafReader,
{
    type LeafReader = LR;

    fn is_cacheable(&self, _ctx: &LeafReaderContext<Self::LeafReader>) -> bool {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}

impl<LR> Weight for DummyWeight<LR>
where
    LR: LeafReader,
{
    type Matches = DummyMatches;

    fn matches(
        &mut self,
        _context: &LeafReaderContext<Self::LeafReader>,
        _doc: i32,
    ) -> Result<Option<Self::Matches>> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn default_matches(
        &mut self,
        _context: &LeafReaderContext<Self::LeafReader>,
        _doc: i32,
    ) -> Result<Option<MatchWithNoTerms>> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn explain(
        &mut self,
        _context: &LeafReaderContext<Self::LeafReader>,
        _doc: i32,
    ) -> Result<Explanation> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    type Query = DummyQuery;

    fn get_query(&self) -> &Self::Query {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn scorer(
        &mut self,
        _context: &LeafReaderContext<Self::LeafReader>,
    ) -> Result<Option<<Self::ScorerSupplier as ScorerSupplier>::Scorer>> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    type ScorerSupplier = DummyScorerSupplier;

    fn scorer_supplier(
        &mut self,
        _context: &LeafReaderContext<Self::LeafReader>,
    ) -> Result<Option<Self::ScorerSupplier>> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn bulk_scorer(
        &mut self,
        _context: &LeafReaderContext<Self::LeafReader>,
    ) -> Result<Option<<Self::ScorerSupplier as ScorerSupplier>::BulkScorer>> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn count(&self, _context: &LeafReaderContext<Self::LeafReader>) -> Result<i32> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn default_count(&self, _context: &LeafReaderContext<Self::LeafReader>) -> Result<i32> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}
