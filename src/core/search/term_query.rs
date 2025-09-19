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
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::term::Term;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::search::dummy::dummy_bulk_scorer::DummyBulkScorer;
use crate::core::search::dummy::dummy_matches::DummyMatches;
use crate::core::search::dummy::dummy_scorer::DummyScorer;
use crate::core::search::dummy::dummy_scorer_supplier::DummyScorerSupplier;
use crate::core::search::explanation::Explanation;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::query::{Query, QueryEnum};
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::similarities_impl::similarities::{Either2SimScorer, SimScorer};
use crate::core::search::weight::Weight;
use crate::core::util::error::lucene_error::Result;
use std::fmt::Display;
use std::hash::{Hash, Hasher};

#[derive(Eq, Debug)]
pub struct TermQuery {
    term: Term,
}
impl TermQuery {
    pub fn new(term: Term) -> Self {
        Self { term }
    }
}

impl PartialEq<Self> for TermQuery {
    fn eq(&self, other: &Self) -> bool {
        self.term == other.term
    }
}

impl Hash for TermQuery {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // TODO
        self.term.hash(state);
    }
}

impl Query for TermQuery {
    fn wrap(self) -> QueryEnum {
        QueryEnum::Term(self)
    }

    type Weight = TermWeight;

    fn crate_weight(
        &self,
        _search: &IndexSearcher,
        _score_mod: &ScoreMode,
        _boost: f32,
    ) -> Result<Self::Weight> {
        todo!()
    }

    type Query = TermQuery;

    fn visit<QV>(&self, visitor: &QV)
    where
        QV: QueryVisitor,
    {
        todo!()
    }
}

impl Display for TermQuery {
    fn fmt(&self, _f: &mut std::fmt::Formatter) -> std::fmt::Result {
        todo!()
    }
}

pub struct TermWeight;
impl TermWeight {
    fn get_terms_enum<LR>(
        &self,
        _context: &LeafReaderContext<LR>,
    ) -> Result<Option<<LR::Terms as Terms>::TermsEnum>>
    where
        LR: LeafReader,
    {
        todo!()
    }
}

impl SegmentCacheable for TermWeight {
    fn is_cacheable<LR>(&self, ctx: &LeafReaderContext<LR>) -> bool
    where
        LR: LeafReader,
    {
        todo!()
    }
}

impl Weight for TermWeight {
    type Matches = DummyMatches;

    fn matches<LR>(
        &mut self,
        context: &LeafReaderContext<LR>,
        doc: i32,
    ) -> Result<Option<Self::Matches>>
    where
        LR: LeafReader,
    {
        todo!()
    }

    fn explain<LR>(&self, context: &LeafReaderContext<LR>, doc: i32) -> Result<Explanation>
    where
        LR: LeafReader,
    {
        todo!()
    }

    type Query = TermQuery;

    fn get_query(&self) -> &Self::Query {
        todo!()
    }

    type ScorerSupplier = DummyScorerSupplier;

    fn scorer_supplier<LR>(
        &mut self,
        context: &LeafReaderContext<LR>,
    ) -> Result<Option<Self::ScorerSupplier>>
    where
        LR: LeafReader,
    {
        todo!()
    }

    fn count<LR>(&self, context: &LeafReaderContext<LR>) -> Result<i32>
    where
        LR: LeafReader,
    {
        if !context.reader().has_deletions()? {
            if let Some(mut terms_enum) = self.get_terms_enum(context)? {
                terms_enum.doc_freq()
            } else {
                Ok(0)
            }
        } else {
            self.default_count(context)
        }
    }
}

pub(crate) struct ScorerSupplierImpl;
impl ScorerSupplier for ScorerSupplierImpl {
    type Scorer = DummyScorer;
    type BulkScorer = DummyBulkScorer;

    fn get(&self, lead_cost: i64) -> Result<Option<Self::Scorer>> {
        todo!()
    }

    fn cost(&self) -> i64 {
        todo!()
    }
}
pub(crate) struct SimScorerImpl;
impl SimScorer for SimScorerImpl {
    fn score(&self, _freq: f32, _norm: i64) -> f32 {
        0f32
    }
}
pub(crate) type TermQuerySimScorer<S> = Either2SimScorer<S, SimScorerImpl>;
