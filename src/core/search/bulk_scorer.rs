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
use crate::core::search::leaf_collector::LeafCollector;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::Result;

/// This trait is used to score a range of documents at once, and is returned by [`Weight::bulk_scorer`](crate::core::search::weight::Weight::bulk_scorer).
///
/// Only queries that have a more optimized means of scoring across a range of
/// documents need to override this. Otherwise, a default implementation is
/// wrapped around the [`Scorer`] returned by [`Weight::scorer`](crate::core::search::weight::Weight::bulk_scorer).
pub trait BulkScorer {
    /// Collects matching documents in a range and returns an estimation of the
    /// next matching document which is on or after `max`.
    ///
    /// # Return value
    ///
    /// - `>= max`
    /// - [`NO_MORE_DOCS`](crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS) if there are no more matches
    /// - `<=` the first matching document that is `>= max` otherwise
    ///
    /// # Parameters
    ///
    /// - `collector`: The collector to which all matching documents are passed.
    /// - `accept_docs`: [`Bits`] that represents the allowed documents to match,
    ///   or `None` if all are allowed to match.
    /// - `min`: Score starting at, including, this document.
    /// - `max`: Score up to, but not including, this doc.
    ///
    /// # Notes
    ///
    /// - `min` is the minimum document to be considered for matching. All documents
    ///   strictly before this value must be ignored.
    /// - Although `max` would be a legal return value for this method, higher values
    ///   might help callers skip more efficiently over non-matching portions of the
    ///   docID space.
    ///
    /// # Returns
    ///
    /// An under-estimation of the next matching doc after `max`.
    fn score<LC, B>(
        &mut self,
        collector: &mut LC,
        accept_docs: Option<&B>,
        min: i32,
        max: i32,
    ) -> Result<i32>
    where
        LC: LeafCollector,
        B: Bits;

    /// Same as [`DocIdSetIterator::cost`](crate::core::search::doc_id_set_iterator::DocIdSetIterator::cost) for bulk scorers.
    fn cost(&mut self) -> Result<i64>;
}
macro_rules! either_bulk_scorer {
    ($vis:vis $name:ident { $( $Variant:ident : $T:ident ),+ $(,)? }) => {
        $vis enum $name<$( $T ),+> {
            $( $Variant($T), )+
        }

        impl<$( $T ),+> BulkScorer for $name<$( $T ),+>
        where
            $( $T: BulkScorer ),+
        {
            #[inline]
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
                match self {
                    $( Self::$Variant(inner) => inner.score(collector, accept_docs, min, max), )+
                }
            }

            #[inline]
            fn cost(&mut self) -> Result<i64> {
                match self {
                    $( Self::$Variant(inner) => inner.cost(), )+
                }
            }
        }
    };
}
either_bulk_scorer!(pub BulkScorerEnum2 { A: A1, B: B1});
either_bulk_scorer!(pub BulkScorerEnum3 { A: A1, B: B1, C: C1});
either_bulk_scorer!(pub BulkScorerEnum4 { A: A1, B: B1, C: C1, D: D1});
either_bulk_scorer!(pub BulkScorerEnum5 { A: A1, B: B1, C: C1, D: D1, E: E1});
either_bulk_scorer!(pub BulkScorerEnum6 {
    A: A1,
    B: B1,
    C: C1,
    D: D1,
    E: E1,
    F: F1
});
either_bulk_scorer!(pub BulkScorerEnum7 {
    A: A1,
    B: B1,
    C: C1,
    D: D1,
    E: E1,
    F: F1,
    G: G1
});
either_bulk_scorer!(pub BulkScorerEnum8 {
    A: A1,
    B: B1,
    C: C1,
    D: D1,
    E: E1,
    F: F1,
    G: G1,
    H: H1
});
either_bulk_scorer!(pub BulkScorerEnum9 {
    A: A1,
    B: B1,
    C: C1,
    D: D1,
    E: E1,
    F: F1,
    G: G1,
    H: H1,
    I: I1
});
either_bulk_scorer!(pub BulkScorerEnum10 {
    A: A1,
    B: B1,
    C: C1,
    D: D1,
    E: E1,
    F: F1,
    G: G1,
    H: H1,
    I: I1,
    J: J1
});
either_bulk_scorer!(pub BulkScorerEnum11 {
    A: A1,
    B: B1,
    C: C1,
    D: D1,
    E: E1,
    F: F1,
    G: G1,
    H: H1,
    I: I1,
    J: J1,
    K: K1
});
either_bulk_scorer!(pub BulkScorerEnum12 {
    A: A1,
    B: B1,
    C: C1,
    D: D1,
    E: E1,
    F: F1,
    G: G1,
    H: H1,
    I: I1,
    J: J1,
    K: K1,
    L: L1
});

pub enum BulkScorerEnum<LR>
where
    LR: crate::core::index::leaf_reader::LeafReader,
{
    Term(crate::core::search::term_query::TermBulkScorer<LR>),
    MatchAll(<crate::core::search::match_all_docs_query::MatchAllSs as crate::core::search::scorer_supplier::ScorerSupplier<LR>>::BulkScorer),
    MatchNoDocs(crate::core::search::match_no_docs_query::MatchNoDocsSsBulkScorer<LR>),
    Dummy(<crate::core::search::dummy::dummy_scorer_supplier::DummyScorerSupplier as crate::core::search::scorer_supplier::ScorerSupplier<LR>>::BulkScorer),
    FieldExists(crate::core::search::field_exists_query::FieldExistsSsBulkScorer<LR>),
    PointRange(crate::core::search::point_range_query::PointRangeSsBulkScorer<LR>),
    SortedNumericDocValuesSet(
        crate::core::document::sorted_numeric_doc_values_set_query::SNDVSQSsBulkScorer<LR>,
    ),
    SortedNumericDocValuesRange(
        crate::core::document::sorted_numeric_doc_values_range_query::SNDVRQSsBulkScorer<LR>,
    ),
    SortedSetDocValuesRange(
        crate::core::document::sorted_set_doc_values_range_query::SSDVRQSsBulkScorer<LR>,
    ),
    IndexSortSortedNumericDocValuesRange(
        crate::core::search::index_sort_sorted_numeric_doc_values_range_query::ISSNDVRQSsBulkScorer<LR>,
    ),
    ConstantScore(crate::core::search::constant_score_query::ConstantScoreBulkScorer<LR>),
    Cached(crate::core::search::lru_query_cache::CachingWrapperWeightBulkScorer<LR>),
}

impl<LR> BulkScorer for BulkScorerEnum<LR>
where
    LR: crate::core::index::leaf_reader::LeafReader,
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
        match self {
            Self::Term(inner) => inner.score(collector, accept_docs, min, max),
            Self::MatchAll(inner) => inner.score(collector, accept_docs, min, max),
            Self::MatchNoDocs(inner) => inner.score(collector, accept_docs, min, max),
            Self::Dummy(inner) => inner.score(collector, accept_docs, min, max),
            Self::FieldExists(inner) => inner.score(collector, accept_docs, min, max),
            Self::PointRange(inner) => inner.score(collector, accept_docs, min, max),
            Self::SortedNumericDocValuesSet(inner) => inner.score(collector, accept_docs, min, max),
            Self::SortedNumericDocValuesRange(inner) => {
                inner.score(collector, accept_docs, min, max)
            },
            Self::SortedSetDocValuesRange(inner) => inner.score(collector, accept_docs, min, max),
            Self::IndexSortSortedNumericDocValuesRange(inner) => {
                inner.score(collector, accept_docs, min, max)
            },
            Self::ConstantScore(inner) => inner.score(collector, accept_docs, min, max),
            Self::Cached(inner) => inner.score(collector, accept_docs, min, max),
        }
    }

    fn cost(&mut self) -> Result<i64> {
        match self {
            Self::Term(inner) => inner.cost(),
            Self::MatchAll(inner) => inner.cost(),
            Self::MatchNoDocs(inner) => inner.cost(),
            Self::Dummy(inner) => inner.cost(),
            Self::FieldExists(inner) => inner.cost(),
            Self::PointRange(inner) => inner.cost(),
            Self::SortedNumericDocValuesSet(inner) => inner.cost(),
            Self::SortedNumericDocValuesRange(inner) => inner.cost(),
            Self::SortedSetDocValuesRange(inner) => inner.cost(),
            Self::IndexSortSortedNumericDocValuesRange(inner) => inner.cost(),
            Self::ConstantScore(inner) => inner.cost(),
            Self::Cached(inner) => inner.cost(),
        }
    }
}
