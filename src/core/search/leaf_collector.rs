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
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_stream::DocIdStream;
use crate::core::search::scorable::{Scorable, ScorerEnum};
use crate::core::search::scorer::Scorer;
use crate::core::util::error::lucene_error::Result;

pub trait LeafCollector {
    /// Called before successive calls to [`LeafCollector::collect`].
    ///
    /// Implementations that need the score of the current document (passed in
    /// to `collect`) should save the passed-in [`Scorer`] and call
    /// `scorer.score()` when needed.
    fn set_scorer<S, C>(&mut self, scorer: ScorerEnum<S, C>) -> Result<()>
    where
        S: Scorer,
        C: Scorable;

    /// Returns the scorer that was most recently provided via
    /// [`LeafCollector::set_scorer`].
    fn scorer_mut(&mut self) -> Result<&mut impl Scorer>;

    /// Called once for every document matching a query, with the unbased document number.
    ///
    /// # Notes
    ///
    /// - The collection of the current segment can be terminated by returning an
    ///   error such as `LuceneError::CollectionTerminated`. In this case, the last
    ///   docs of the current [`LeafReaderContext`](crate::core::index::leaf_reader_context::LeafReaderContext) will be skipped and
    ///   [`IndexSearcher`](crate::core::search::index_searcher::IndexSearcher) will swallow the exception and continue collection with
    ///   the next leaf.
    ///
    /// - This is called in an inner search loop. For good search performance,
    ///   implementations of this method should **not** call
    ///   [`StoredFields::document`](crate::core::index::stored_fields::StoredFields::document) on every hit. Doing so can slow searches by an
    ///   order of magnitude or more.
    fn collect(&mut self, doc: i32) -> Result<()>;

    /// Bulk-collect doc IDs.
    ///
    /// # Notes
    ///
    /// - The provided [`DocIdStream`] may be reused across calls and should be
    ///   consumed immediately.
    /// - The provided [`DocIdStream`] typically only holds a small subset of query
    ///   matches. This method may be called multiple times per segment.
    /// - Like [`LeafCollector::collect`], it is guaranteed that doc IDs get
    ///   collected in order. Doc IDs are collected in order within a
    ///   [`DocIdStream`], and if called twice, all doc IDs from the second
    ///   [`DocIdStream`] will be greater than all doc IDs from the first
    ///   [`DocIdStream`].
    /// - It is legal for callers to mix calls to
    ///   [`LeafCollector::collect_stream`] and [`LeafCollector::collect`].
    ///
    /// # Default
    ///
    /// The default implementation calls `stream.for_each(|doc| self.collect(doc))`.
    fn collect_stream<DS>(&mut self, stream: &mut DS) -> Result<()>
    where
        DS: DocIdStream,
    {
        stream.for_each(|doc| self.collect(doc))
    }

    type DocIdSetIterator: DocIdSetIterator;
    /// Optionally returns an iterator over competitive documents.
    ///
    /// Collectors should delegate this method to their comparators if their
    /// comparators provide skipping functionality over non-competitive docs.
    ///
    /// The default is `None`, meaning no competitive iterator is provided.
    fn competitive_iterator(&mut self) -> Result<Option<&mut Self::DocIdSetIterator>> {
        Ok(None)
    }

    /// Hook that gets called once the leaf associated with this collector has
    /// finished collecting successfully, including when a
    /// [`CollectionTerminatedError`](crate::core::util::error::CollectionTerminatedError) is thrown.
    ///
    /// This is typically useful to compile data that has been collected on this
    /// leaf, e.g. to convert facet counts on leaf ordinals to facet counts on
    /// global ordinals.
    ///
    /// The default implementation does nothing.
    ///
    /// # Notes
    ///
    /// - It can be assumed that this method will only be called once per
    ///   [`LeafCollector`] instance.
    fn finish(&mut self) -> Result<()> {
        Ok(())
    }
}
