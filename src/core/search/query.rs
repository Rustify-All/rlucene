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
use crate::core::search::dummy::dummy_query::DummyQuery;
use crate::core::search::dummy::dummy_weight::DummyWeight;
use crate::core::search::index_searcher::IndexSearcher;
use crate::core::search::query_visitor::QueryVisitor;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::term_query::TermQuery;
use crate::core::search::weight::Weight;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::fmt::{Debug, Display, Formatter};
use std::hash::{Hash, Hasher};

pub trait Query: Eq + Hash + Display + Debug {
    fn wrap(self) -> QueryEnum;

    type Weight: Weight;
    fn crate_weight<IRC, LR>(
        &self,
        _search: &IndexSearcher<IRC, LR>,
        _score_mod: &ScoreMode,
        _boost: f32,
    ) -> Result<Self::Weight>
    where
        IRC: IndexReaderContext<LR>,
        LR: LeafReader,
    {
        Err(LuceneError::unsupported_operation(format!(
            "Query {} does not implement create_weight",
            std::any::type_name::<Self>()
        )))
    }
    type Query: Query;
    fn rewrite<IRC, LR>(&self, _searcher: &IndexSearcher<IRC, LR>) -> Result<Option<Self::Query>>
    where
        IRC: IndexReaderContext<LR>,
        LR: LeafReader,
    {
        Ok(None)
    }
    fn visit<QV>(&self, visitor: &QV)
    where
        QV: QueryVisitor;
}

pub enum QueryEnum {
    Term(TermQuery),
}

impl Eq for QueryEnum {}

impl PartialEq<Self> for QueryEnum {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (QueryEnum::Term(t1), QueryEnum::Term(t2)) => t1 == t2,
        }
    }
}

impl Hash for QueryEnum {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            QueryEnum::Term(t) => t.hash(state),
        }
    }
}

impl Display for QueryEnum {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            QueryEnum::Term(t) => Display::fmt(&t, f),
        }
    }
}

impl Debug for QueryEnum {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            QueryEnum::Term(t) => Debug::fmt(&t, f),
        }
    }
}

impl Query for QueryEnum {
    fn wrap(self) -> QueryEnum {
        match self {
            QueryEnum::Term(t) => QueryEnum::Term(t),
        }
    }

    type Weight = DummyWeight;

    fn crate_weight<IRC, LR>(
        &self,
        _search: &IndexSearcher<IRC, LR>,
        _score_mod: &ScoreMode,
        _boost: f32,
    ) -> Result<Self::Weight>
    where
        IRC: IndexReaderContext<LR>,
        LR: LeafReader,
    {
        todo!()
    }

    type Query = DummyQuery;

    fn rewrite<IRC, LR>(&self, _searcher: &IndexSearcher<IRC, LR>) -> Result<Option<Self::Query>>
    where
        IRC: IndexReaderContext<LR>,
        LR: LeafReader,
    {
        todo!()
    }

    fn visit<QV>(&self, visitor: &QV)
    where
        QV: QueryVisitor,
    {
        todo!()
    }
}
