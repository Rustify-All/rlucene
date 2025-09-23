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
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::explanation::Explanation;
use crate::core::search::matches::Matches;
use crate::core::search::matches_utils::MatchWithNoTerms;
use crate::core::search::query::Query;
use crate::core::search::scorer::Scorer;
use crate::core::search::scorer_supplier::ScorerSupplier;
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::two_phase_iterator::TwoPhaseIterator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
/// Expert: Calculate query weights and build query scorers.
///
/// The purpose of [`Weight`] is to ensure searching does not modify a [`Query`],
/// so that a [`Query`] instance can be reused.
///
/// - [`IndexSearcher`](crate::core::search::index_searcher::IndexSearcher)-dependent state of the query should reside in the [`Weight`].
/// - [`LeafReader`](crate::core::index::leaf_reader::LeafReader)-dependent state should reside in the [`Scorer`].
///
/// Since [`Weight`] creates [`Scorer`] instances for a given [`LeafReaderContext`]
/// (via [`Weight::scorer`]), callers must maintain the relationship between the
/// searcher's top-level [`IndexReaderContext`](crate::core::index::index_reader_context::IndexReaderContext) and the context used to create a
/// [`Scorer`].
///
/// A `Weight` is used in the following way:
///
/// 1. A `Weight` is constructed by a top-level query, given an [`IndexSearcher`](crate::core::search::index_searcher::IndexSearcher)
///    (see [`Query::create_weight`]).
/// 2. A [`Scorer`] is constructed by [`Weight::scorer`].
pub trait Weight: SegmentCacheable {
    type Matches: Matches;
    /// Returns [`Matches`] for a specific document, or `None` if the document
    /// does not match the parent query.
    ///
    /// A query match that contains no position information (for example, a
    /// Point or DocValues query) will return
    /// [`MatchesUtils::MATCH_WITH_NO_TERMS`].
    ///
    /// # Parameters
    /// - `context`: the reader's context to create the [`Matches`] for
    /// - `doc`: the document's id relative to the given context's reader
    fn matches(
        &mut self,
        context: &LeafReaderContext<Self::LeafReader>,
        doc: i32,
    ) -> Result<Option<Self::Matches>>;
    fn default_matches(
        &mut self,
        context: &LeafReaderContext<Self::LeafReader>,
        doc: i32,
    ) -> Result<Option<MatchWithNoTerms>> {
        let scorer_supplier = self.scorer_supplier(context)?;
        let scorer_supplier = match scorer_supplier {
            None => return Ok(None),
            Some(s) => s,
        };

        let mut scorer = scorer_supplier.get(1)?;
        match scorer {
            None => {
                return Err(LuceneError::illegal_state(
                    "scorer_supplier returned None Scorer",
                ));
            },
            Some(ref mut scorer) => {
                if let Some(two_phase) = scorer.two_phase_iterator() {
                    if two_phase.approximation_mut().advance(doc)? != doc || !two_phase.matches()? {
                        return Ok(None);
                    }
                } else if scorer.iterator().advance(doc)? != doc {
                    return Ok(None);
                }
            },
        };
        Ok(Some(MatchWithNoTerms))
    }

    /// An explanation of the score computation for the named document.
    ///
    /// # Parameters
    /// - `context`: the reader's context to create the [`Explanation`] for
    /// - `doc`: the document's id relative to the given context's reader
    fn explain(
        &mut self,
        context: &LeafReaderContext<Self::LeafReader>,
        doc: i32,
    ) -> Result<Explanation>;

    type Query: Query;
    /// The query that this weight concerns.
    fn get_query(&self) -> &Self::Query;

    /// Optional method that delegates to [`Weight::scorer_supplier`].
    ///
    /// Returns a [`Scorer`](crate::core::search::scorer::Scorer) which can iterate in order over all matching documents
    /// and assign them a score. A scorer for the same [`LeafReaderContext`] instance
    /// may be requested multiple times as part of a single search call.
    ///
    /// # Notes
    ///
    /// - May return `None` if no documents will be scored by this query.
    /// - The returned [`Scorer`](crate::core::search::scorer::Scorer) does **not** have [`LeafReader::get_live_docs`](crate::core::index::leaf_reader::LeafReader::get_live_docs)
    ///   applied; callers must check live docs on top.
    ///
    /// # Parameters
    ///
    /// - `context`: the [`LeafReaderContext`] for which to return the [`Scorer`](crate::core::search::scorer::Scorer).
    ///
    /// # Returns
    ///
    /// An optional [`Scorer`](crate::core::search::scorer::Scorer) which scores documents in/out-of-order.
    ///
    /// # Errors
    ///
    /// Returns an error if a low-level I/O error occurs.
    fn scorer(
        &mut self,
        context: &LeafReaderContext<Self::LeafReader>,
    ) -> Result<Option<<Self::ScorerSupplier as ScorerSupplier>::Scorer>> {
        let scorer_supplier = match self.scorer_supplier(context)? {
            None => return Ok(None),
            Some(s) => s,
        };
        scorer_supplier.get(i64::MAX)
    }

    type ScorerSupplier: ScorerSupplier;
    /// Get a [`ScorerSupplier`], which allows knowing the cost of the [`Scorer`]
    /// before building it.
    ///
    /// A scorer supplier for the same [`LeafReaderContext`] instance may be requested
    /// multiple times as part of a single search call.
    ///
    /// # Notes
    ///
    /// - Must return `None` if the scorer is `None`.
    ///
    /// # Parameters
    ///
    /// - `context`: the leaf reader context
    ///
    /// # Returns
    ///
    /// A [`ScorerSupplier`] providing the scorer, or `None` if the scorer is absent.
    ///
    /// # Errors
    ///
    /// Returns an error if a low-level I/O error occurs.
    ///
    /// # See also
    ///
    /// - [`Scorer`]
    /// - [`DefaultScorerSupplier`]
    fn scorer_supplier(
        &mut self,
        context: &LeafReaderContext<Self::LeafReader>,
    ) -> Result<Option<Self::ScorerSupplier>>;
    /// Helper method that delegates to [`Weight::scorer_supplier`].
    ///
    /// A bulk scorer for the same [`LeafReaderContext`] instance may be requested
    /// multiple times as part of a single search call.
    fn bulk_scorer(
        &mut self,
        context: &LeafReaderContext<Self::LeafReader>,
    ) -> Result<Option<<Self::ScorerSupplier as ScorerSupplier>::BulkScorer>> {
        let mut scorer_supplier = match self.scorer_supplier(context)? {
            None => return Ok(None),
            Some(s) => s,
        };

        scorer_supplier.set_top_level_scoring_clause()?;
        Ok(Some(scorer_supplier.bulk_scorer()?))
    }

    /// Counts the number of live documents that match this weight's parent query
    /// in a leaf.
    ///
    /// # Default
    ///
    /// The default implementation returns `-1` for every query. This indicates
    /// that the count could not be computed in sub-linear time.
    ///
    /// # Notes
    ///
    /// - Specific query classes should override this to provide other accurate
    ///   sub-linear implementations (that actually return the count).
    ///   For example, see how [`MatchAllDocsQuery::create_weight`] does it.
    /// - This method is used by [`IndexSearcher::count`] to count hits.
    ///
    /// # Parameters
    ///
    /// - `context`: the [`LeafReaderContext`] for which to return the count.
    ///
    /// # Returns
    ///
    /// An integer count of the number of matches, or `-1` if it cannot be
    /// determined efficiently.
    ///
    /// # Errors
    ///
    /// Returns an error if a low-level I/O error occurs.
    fn count(&self, context: &LeafReaderContext<Self::LeafReader>) -> Result<i32> {
        self.default_count(context)
    }
    fn default_count(&self, _context: &LeafReaderContext<Self::LeafReader>) -> Result<i32> {
        Ok(-1)
    }
}
