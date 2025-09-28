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
use crate::core::search::leaf_collector::LeafCollector;
use crate::core::search::score_mode::ScoreMode;
use crate::core::search::weight::Weight;
/// Expert: Collectors are primarily meant to be used to gather raw results from a search,
/// and implement sorting or custom result filtering, collation, etc.
///
/// Lucene's core collectors are derived from [`Collector`] and [`SimpleCollector`](crate::core::search::simple_collector::SimpleCollector).
/// Likely your application can use one of these classes, or subclass [`TopDocsCollector`](crate::core::search::top_docs_collector::TopDocsCollector),
/// instead of implementing Collector directly:
///
/// - [`TopDocsCollector`](crate::core::search::top_docs_collector::TopDocsCollector) is an abstract base class that assumes you will retrieve the top N
///   docs, according to some criteria, after collection is done.
/// - [`TopScoreDocCollector`](crate::core::search::top_score_doc_collector::TopScoreDocCollector) is a concrete subclass [`TopDocsCollector`](crate::core::search::top_docs_collector::TopDocsCollector) and sorts
///   according to score + docID. This is used internally by the [`IndexSearcher`](crate::core::search::index_searcher::IndexSearcher) search
///   methods that do not take an explicit [`Sort`](crate::core::index::sort::Sort). It is likely the most frequently used
///   collector.
/// - [`TopFieldCollector`](crate::core::search::top_field_collector::TopFieldCollector) subclasses [`TopDocsCollector`](crate::core::search::top_docs_collector::TopDocsCollector) and sorts according to a
///   specified [`Sort`](crate::core::index::sort::Sort) object (sort by field). This is used internally by the
///   [`IndexSearcher`](crate::core::search::index_searcher::IndexSearcher) search methods that take an explicit [`Sort`](crate::core::index::sort::Sort).
/// - [`PositiveScoresOnlyCollector`](crate::core::search::positive_scores_only_collector::PositiveScoresOnlyCollector) wraps any other Collector and prevents collection of
///   hits whose score is <= 0.0
///
/// @lucene.experimental
use crate::core::util::error::lucene_error::Result;
use std::rc::Rc;

pub trait Collector {
    type LeafCollector<'a>: LeafCollector
    where
        Self: 'a;
    /// Create a new [`LeafCollector`] to collect the given context.
    ///
    /// # Arguments
    /// * `context` - next atomic reader context
    fn get_leaf_collector<'a, LR>(
        &'a mut self,
        context: &LeafReaderContext<LR>,
    ) -> Result<Self::LeafCollector<'a>>
    where
        LR: LeafReader;

    /// Indicates what features are required from the scorer.
    fn score_mode(&self) -> &ScoreMode;

    type Weight: Weight;
    /// Set the [`Weight`] that will be used to produce scorers that will feed [`LeafCollector`]s.
    /// This is typically useful to have access to [`Weight::count`] from [`Collector::get_leaf_collector`].
    fn set_weight(&mut self, _weight: Rc<Self::Weight>) {}
}
