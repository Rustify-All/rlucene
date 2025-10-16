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
use crate::core::search::bulk_scorer::{BulkScorer, Either2BulkScorer};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::search::explanation::Explanation;
use crate::core::search::leaf_collector::LeafCollector;
use crate::core::search::matches::{Either2Matches, Matches};
use crate::core::search::matches_utils::MatchWithNoTerms;
use crate::core::search::query::Query;
use crate::core::search::scorer::{Either2Scorer, Scorer};
use crate::core::search::scorer_supplier::{Either2ScorerSupplier, ScorerSupplier};
use crate::core::search::segment_cacheable::SegmentCacheable;
use crate::core::search::two_phase_iterator::TwoPhaseIterator;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::sync::Arc;

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
pub trait Weight<LR>: SegmentCacheable<LR>
where
    LR: LeafReader,
{
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
    fn matches(&self, context: &LeafReaderContext<LR>, doc: i32) -> Result<Option<Self::Matches>>;
    fn default_matches(
        &self,
        context: &LeafReaderContext<LR>,
        doc: i32,
    ) -> Result<Option<MatchWithNoTerms>> {
        let scorer_supplier = self.scorer_supplier(context)?;
        let mut scorer_supplier = match scorer_supplier {
            None => return Ok(None),
            Some(s) => s,
        };

        let mut scorer = scorer_supplier.get(1, context)?;
        match scorer {
            None => {
                return Err(LuceneError::illegal_state(
                    "scorer_supplier returned None Scorer",
                ));
            },
            Some(ref mut scorer) => {
                if let Some(mut two_phase) = scorer.two_phase_iterator() {
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
    fn explain(&self, context: &LeafReaderContext<LR>, doc: i32) -> Result<Explanation>;

    fn get_query(&self) -> Arc<Query>;

    /// Optional method that delegates to [`Weight::scorer_supplier`].
    ///
    /// Returns a [`Scorer`] which can iterate in order over all matching documents
    /// and assign them a score. A scorer for the same [`LeafReaderContext`] instance
    /// may be requested multiple times as part of a single search call.
    ///
    /// # Notes
    ///
    /// - May return `None` if no documents will be scored by this query.
    /// - The returned [`Scorer`] does **not** have [`LeafReader::get_live_docs`](crate::core::index::leaf_reader::LeafReader::get_live_docs)
    ///   applied; callers must check live docs on top.
    ///
    /// # Parameters
    ///
    /// - `context`: the [`LeafReaderContext`] for which to return the [`Scorer`].
    ///
    /// # Returns
    ///
    /// An optional [`Scorer`] which scores documents in/out-of-order.
    ///
    /// # Errors
    ///
    /// Returns an error if a low-level I/O error occurs.
    fn scorer(
        &self,
        context: &LeafReaderContext<LR>,
    ) -> Result<Option<<Self::ScorerSupplier as ScorerSupplier<LR>>::Scorer>> {
        let mut scorer_supplier = match self.scorer_supplier(context)? {
            None => return Ok(None),
            Some(s) => s,
        };
        scorer_supplier.get(i64::MAX, context)
    }

    type ScorerSupplier: ScorerSupplier<LR>;
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
        &self,
        context: &LeafReaderContext<LR>,
    ) -> Result<Option<Self::ScorerSupplier>>;
    /// Helper method that delegates to [`Weight::scorer_supplier`].
    ///
    /// A bulk scorer for the same [`LeafReaderContext`] instance may be requested
    /// multiple times as part of a single search call.
    fn bulk_scorer(
        &self,
        context: &LeafReaderContext<LR>,
    ) -> Result<Option<<Self::ScorerSupplier as ScorerSupplier<LR>>::BulkScorer>> {
        let mut scorer_supplier = match self.scorer_supplier(context)? {
            None => return Ok(None),
            Some(s) => s,
        };

        scorer_supplier.set_top_level_scoring_clause()?;
        Ok(Some(scorer_supplier.bulk_scorer(context)?))
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
    fn count(&self, context: &LeafReaderContext<LR>) -> Result<i32> {
        self.default_count(context)
    }
    fn default_count(&self, _context: &LeafReaderContext<LR>) -> Result<i32> {
        Ok(-1)
    }
    fn is_weight_cacheable(&self) -> bool {
        true
    }
}

pub struct DefaultBulkScorer<S>
where
    S: Scorer,
{
    scorer: S,
}
impl<S> DefaultBulkScorer<S>
where
    S: Scorer,
{
    pub fn new(scorer: S) -> Self {
        Self { scorer }
    }
}
impl<S> BulkScorer for DefaultBulkScorer<S>
where
    S: Scorer,
{
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
        collector.set_scorer(&mut self.scorer)?;
        let has_two_phase = self.scorer.two_phase_iterator().is_some();
        let doc_id = if has_two_phase {
            let two_phase = self.scorer.two_phase_iterator().unwrap();
            two_phase.approximation().doc_id()
        } else {
            self.scorer.iterator().doc_id()
        };

        let has_competitive_iterator = collector.competitive_iterator()?.is_some();

        if !has_competitive_iterator
            && doc_id == -1
            && accept_docs.is_none()
            && min == 0
            && max == NO_MORE_DOCS
        {
            score_all(collector, accept_docs, &mut self.scorer)?;
            Ok(NO_MORE_DOCS)
        } else {
            score_range(collector, accept_docs, min, max, &mut self.scorer)
        }
    }

    fn cost(&mut self) -> Result<i64> {
        self.scorer.iterator().cost()
    }
}
pub struct DefaultScorerSupplier<S>
where
    S: Scorer,
{
    scorer: Option<S>,
}
impl<S> DefaultScorerSupplier<S>
where
    S: Scorer,
{
    pub fn new(scorer: S) -> Self {
        Self {
            scorer: Some(scorer),
        }
    }
}
impl<S, LR> ScorerSupplier<LR> for DefaultScorerSupplier<S>
where
    LR: LeafReader,
    S: Scorer,
{
    type Scorer = S;
    type BulkScorer = DefaultBulkScorer<S>;

    fn get(
        &mut self,
        _lead_cost: i64,
        _context: &LeafReaderContext<LR>,
    ) -> Result<Option<Self::Scorer>> {
        Ok(self.scorer.take())
    }

    fn bulk_scorer(&mut self, context: &LeafReaderContext<LR>) -> Result<Self::BulkScorer> {
        <Self as ScorerSupplier<LR>>::default_bulk_scorer(self, context)
    }

    fn cost(&mut self) -> Result<i64> {
        self.scorer.as_mut().unwrap().iterator().cost()
    }
}
/// Specialized method to bulk-score all hits;
/// we separate this from scoreRange to help out hotspot. See [`LUCENE-5487`](https://issues.apache.org/jira/browse/LUCENE-5487">LUCENE-5487)
fn score_all<C, B, S>(collector: &mut C, accept_docs: Option<&B>, scorer: &mut S) -> Result<()>
where
    C: LeafCollector,
    B: Bits,
    S: Scorer,
{
    let has_two_phase = scorer.two_phase_iterator().is_some();

    if has_two_phase {
        loop {
            let (doc, matches) = {
                let mut two_phase = scorer.two_phase_iterator().unwrap();
                let doc = {
                    let mut iter = two_phase.approximation_mut();
                    iter.next_doc()?
                };
                if doc == NO_MORE_DOCS {
                    (doc, false)
                } else {
                    (doc, two_phase.matches()?)
                }
            };
            if doc == NO_MORE_DOCS {
                break;
            }
            if accept_docs.is_none_or(|a| a.get(doc)) && matches {
                collector.collect(doc, scorer)?;
            }
        }
    } else {
        loop {
            let doc = {
                let mut iter = scorer.iterator();
                iter.next_doc()?
            };
            if doc == NO_MORE_DOCS {
                break;
            }
            if accept_docs.is_none_or(|a| a.get(doc)) {
                collector.collect(doc, scorer)?;
            }
        }
    }
    Ok(())
}
/// Specialized method to bulk-score a range of hits;
/// we separate this from scoreAll to help out hotspot. See [`LUCENE-5487`](https://issues.apache.org/jira/browse/LUCENE-5487">LUCENE-5487)
fn score_range<C, B, S>(
    collector: &mut C,
    accept_docs: Option<&B>,
    mut min: i32,
    max: i32,
    scorer: &mut S,
) -> Result<i32>
where
    C: LeafCollector,
    B: Bits,
    S: Scorer,
{
    if let Some(iterator) = collector.competitive_iterator()?
        && iterator.doc_id() > min
    {
        min = iterator.doc_id().min(max);
    }

    let mut doc = {
        let mut iter = scorer.iterator();
        let d = iter.doc_id();
        if d < min {
            if d == min - 1 {
                iter.next_doc()?
            } else {
                iter.advance(min)?
            }
        } else {
            d
        }
    };

    let has_two_phase = scorer.two_phase_iterator().is_some();

    let has_competitive = collector.competitive_iterator()?.is_some();

    if !has_two_phase && !has_competitive {
        while doc < max {
            if accept_docs.is_none_or(|a| a.get(doc)) {
                collector.collect(doc, scorer)?;
            }
            doc = {
                let mut iter = scorer.iterator();
                iter.next_doc()?
            };
        }
        return Ok(doc);
    }

    while doc < max {
        if has_competitive {
            let mut advance_to = None;
            if let Some(mut iterator) = collector.competitive_iterator()? {
                debug_assert!(iterator.doc_id() <= doc);
                let candidate = if iterator.doc_id() < doc {
                    iterator.advance(doc)?
                } else {
                    iterator.doc_id()
                };
                if candidate != doc {
                    advance_to = Some(candidate);
                }
            }

            if let Some(target) = advance_to {
                doc = {
                    let mut iter = scorer.iterator();
                    iter.advance(target)?
                };
                continue;
            }
        }

        let matches = if has_two_phase {
            let mut two_phase = scorer.two_phase_iterator().unwrap();
            two_phase.matches()?
        } else {
            true
        };

        if accept_docs.is_none_or(|a| a.get(doc)) && matches {
            collector.collect(doc, scorer)?;
        }

        doc = {
            let mut iter = scorer.iterator();
            iter.next_doc()?
        };
    }

    Ok(doc)
}

macro_rules! define_either_weight {
    (
        $name:ident,
        matches = $either_matches:ident,
        scorer = $either_scorer:ident,
        supplier = $either_supplier:ident,
        bulk = $either_bulk:ident,
        A = $A:ident, $( $T:ident ),+ $(,)?
    ) => {
        pub enum $name<$A, $( $T ),+> {
            $A($A),
            $( $T($T) ),+
        }

        impl<LR, $A, $( $T ),+> SegmentCacheable<LR> for $name<$A, $( $T ),+>
        where
            LR: LeafReader,
            $A: Weight<LR>,
            $( $T: Weight<LR> ),+
        {
            fn is_cacheable(&self, ctx: &LeafReaderContext<LR>) -> bool {
                match self {
                    Self::$A(inner) => inner.is_cacheable(ctx),
                    $( Self::$T(inner) => inner.is_cacheable(ctx), )+
                }
            }
        }

        impl<LR, $A, $( $T ),+> Weight<LR> for $name<$A, $( $T ),+>
        where
            LR: LeafReader,
            $A: Weight<LR>,
            $( $T: Weight<LR> ),+
        {
            type Matches = $either_matches<
                <$A as Weight<LR>>::Matches,
                $( <$T as Weight<LR>>::Matches ),+
            >;

            type ScorerSupplier = $either_supplier<
                <$A as Weight<LR>>::ScorerSupplier,
                $( <$T as Weight<LR>>::ScorerSupplier ),+
            >;

            fn matches(
                &self,
                context: &LeafReaderContext<LR>,
                doc: i32,
            ) -> Result<Option<Self::Matches>> {
                match self {
                    Self::$A(inner) => {
                        let opt = inner.matches(context, doc)?;
                        Ok(opt.map($either_matches::$A))
                    },
                    $( Self::$T(inner) => {
                        let opt = inner.matches(context, doc)?;
                        Ok(opt.map($either_matches::$T))
                    }, )+
                }
            }

            fn default_matches(
                &self,
                context: &LeafReaderContext<LR>,
                doc: i32,
            ) -> Result<Option<MatchWithNoTerms>> {
                match self {
                    Self::$A(inner) => inner.default_matches(context, doc),
                    $( Self::$T(inner) => inner.default_matches(context, doc), )+
                }
            }

            fn explain(
                &self,
                context: &LeafReaderContext<LR>,
                doc: i32,
            ) -> Result<Explanation> {
                match self {
                    Self::$A(inner) => inner.explain(context, doc),
                    $( Self::$T(inner) => inner.explain(context, doc), )+
                }
            }

            fn get_query(&self) -> Arc<Query> {
                match self {
                    Self::$A(inner) => inner.get_query(),
                    $( Self::$T(inner) => inner.get_query(), )+
                }
            }

            fn scorer(
                &self,
                context: &LeafReaderContext<LR>,
            ) -> Result<Option<<Self::ScorerSupplier as ScorerSupplier<LR>>::Scorer>> {
                match self {
                    Self::$A(inner) => {
                        let opt = inner.scorer(context)?;
                        Ok(opt.map($either_scorer::$A))
                    },
                    $( Self::$T(inner) => {
                        let opt = inner.scorer(context)?;
                        Ok(opt.map($either_scorer::$T))
                    }, )+
                }
            }

            fn scorer_supplier(
                &self,
                context: &LeafReaderContext<LR>,
            ) -> Result<Option<Self::ScorerSupplier>> {
                match self {
                    Self::$A(inner) => {
                        let opt = inner.scorer_supplier(context)?;
                        Ok(opt.map($either_supplier::$A))
                    },
                    $( Self::$T(inner) => {
                        let opt = inner.scorer_supplier(context)?;
                        Ok(opt.map($either_supplier::$T))
                    }, )+
                }
            }

            fn bulk_scorer(
                &self,
                context: &LeafReaderContext<LR>,
            ) -> Result<Option<<Self::ScorerSupplier as ScorerSupplier<LR>>::BulkScorer>> {
                match self {
                    Self::$A(inner) => {
                        let opt = inner.bulk_scorer(context)?;
                        Ok(opt.map($either_bulk::$A))
                    },
                    $( Self::$T(inner) => {
                        let opt = inner.bulk_scorer(context)?;
                        Ok(opt.map($either_bulk::$T))
                    }, )+
                }
            }

            fn count(&self, context: &LeafReaderContext<LR>) -> Result<i32> {
                match self {
                    Self::$A(inner) => inner.count(context),
                    $( Self::$T(inner) => inner.count(context), )+
                }
            }

            fn default_count(&self, _context: &LeafReaderContext<LR>) -> Result<i32> {
                Ok(-1)
            }
            fn is_weight_cacheable(&self) -> bool {
                match self {
                    Self::$A(inner) => inner.is_weight_cacheable(),
                    $( Self::$T(inner) => inner.is_weight_cacheable(), )+
                }
            }
        }
    };
}

define_either_weight!(
    Either2Weight,
    matches = Either2Matches,
    scorer = Either2Scorer,
    supplier = Either2ScorerSupplier,
    bulk = Either2BulkScorer,
    A = A,
    B,
);
