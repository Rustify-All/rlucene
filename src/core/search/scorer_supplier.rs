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
use crate::core::search::bulk_scorer::{
    BulkScorer, BulkScorerEnum, BulkScorerEnum2, BulkScorerEnum3, BulkScorerEnum4,
    BulkScorerEnum5, BulkScorerEnum6, BulkScorerEnum7, BulkScorerEnum8, BulkScorerEnum9,
    BulkScorerEnum10, BulkScorerEnum11, BulkScorerEnum12,
};
use crate::core::search::scorer::{
    Scorer, ScorerEnum, ScorerEnum2, ScorerEnum3, ScorerEnum4, ScorerEnum5, ScorerEnum6,
    ScorerEnum7, ScorerEnum8, ScorerEnum9, ScorerEnum10, ScorerEnum11, ScorerEnum12,
};
use crate::core::search::weight::DefaultBulkScorer;
use crate::core::util::error::lucene_error::Result;
/// A supplier of [`Scorer`].
///
/// This allows to get an estimate of the cost before building the [`Scorer`].
pub trait ScorerSupplier<LR>
where
    LR: LeafReader,
{
    type Scorer: Scorer;
    type BulkScorer: BulkScorer;

    /// Get the [`Scorer`].
    /// This must be called at most once.
    ///
    /// # Parameters
    ///
    /// - `lead_cost`: Cost of the scorer that will be used in order to lead iteration.
    ///   This can be interpreted as an upper bound of the number of times that
    ///   [`DocIdSetIterator::next_doc`](crate::core::search::doc_id_set_iterator::DocIdSetIterator::next_doc), [`DocIdSetIterator::advance`](crate::core::search::doc_id_set_iterator::DocIdSetIterator::advance), and
    ///   [`TwoPhaseIterator::matches`](crate::core::search::two_phase_iterator::TwoPhaseIterator::matches) will be called.
    ///   If in doubt, pass `i64::MAX`, which will produce a [`Scorer`] that has good iteration capabilities.
    /// - `context`: The [`LeafReaderContext`] that this scorer supplier was created for.
    fn get(&mut self, lead_cost: i64, context: &LeafReaderContext<LR>) -> Result<Self::Scorer>;

    /// Optional: Get a bulk scorer that is optimized for bulk-scoring.
    ///
    /// The default implementation wraps `get(i64::MAX)` in a `DefaultBulkScorer`,
    /// which iterates matches from the scorer. Some queries can have more efficient
    /// approaches for matching all hits.
    fn bulk_scorer(&mut self, context: &LeafReaderContext<LR>) -> Result<Option<Self::BulkScorer>>;
    fn default_bulk_scorer(
        &mut self,
        context: &LeafReaderContext<LR>,
    ) -> Result<DefaultBulkScorer<Self::Scorer>> {
        let scorer = self.get(i64::MAX, context)?;
        Ok(DefaultBulkScorer::new(scorer))
    }

    /// Get an estimate of the [`Scorer`] that would be returned by [`ScorerSupplier::get`].
    /// This may be a costly operation, so it should only be called if necessary.
    ///
    /// Corresponds to [`DocIdSetIterator::cost`](crate::core::search::doc_id_set_iterator::DocIdSetIterator::cost).
    fn cost(&mut self, context: &LeafReaderContext<LR>) -> Result<i64>;

    /// Inform this [`ScorerSupplier`] that its returned scorers produce scores that get passed
    /// to the collector, as opposed to partial scores that then need to get combined (e.g. summed up).
    ///
    /// Note: This method also gets called if scores are not requested, e.g. because the score mode
    /// is [`ScoreMode::COMPLETE_NO_SCORES`](crate::core::search::score_mode::ScoreMode::CompleteNoScores).
    /// Implementations should look at both the score mode and this boolean to know whether to prepare
    /// for reacting to [`Scorer::set_min_competitive_score`] calls.
    fn set_top_level_scoring_clause(&mut self) -> Result<()> {
        Ok(())
    }
}
pub type SsScorer<SS, LR> = <SS as ScorerSupplier<LR>>::Scorer;
pub type SsBulkScorer<SS, LR> = <SS as ScorerSupplier<LR>>::BulkScorer;
macro_rules! either_scorer_supplier {
    (
        $vis:vis $name:ident
        => { bulk: $bulk:ident, scorer: $scorer:ident }
        { $( $Variant:ident : $T:ident ),+ $(,)? }
    ) => {
        $vis enum $name<$( $T ),+> {
            $( $Variant($T), )+
        }

        impl<LR, $( $T ),+> ScorerSupplier<LR> for $name<$( $T ),+>
        where
            LR: LeafReader,
            $( $T: ScorerSupplier<LR> ),+
        {
            type Scorer = $scorer<$( <$T as ScorerSupplier<LR>>::Scorer ),+>;
            type BulkScorer = $bulk<$( <$T as ScorerSupplier<LR>>::BulkScorer ),+>;

            fn get(
                &mut self,
                lead_cost: i64,
                context: &LeafReaderContext<LR>,
            ) -> Result<Self::Scorer> {
                match self {
                    $(
                        Self::$Variant(inner) => {
                            let scorer = inner.get(lead_cost, context)?;
                            Ok($scorer::$Variant(scorer))
                        }
                    ),+
                }
            }

            fn bulk_scorer(
                &mut self,
                context: &LeafReaderContext<LR>,
            ) -> Result<Option<Self::BulkScorer>> {
                match self {
                    $(
                        Self::$Variant(inner) => {
                            let opt = inner.bulk_scorer(context)?;
                            Ok(opt.map($bulk::$Variant))
                        }
                    ),+
                }
            }

            fn default_bulk_scorer(
                &mut self,
                context: &LeafReaderContext<LR>,
            ) -> Result<DefaultBulkScorer<Self::Scorer>> {
                match self {
                    $(
                        Self::$Variant(inner) => {
                            let scorer = inner.get(i64::MAX, context)?;
                            Ok(DefaultBulkScorer::new($scorer::$Variant(scorer)))
                        }
                    )+
                }
            }

            fn cost(&mut self, context: &LeafReaderContext<LR>) -> Result<i64> {
                match self {
                    $( Self::$Variant(inner) => inner.cost(context), )+
                }
            }

            fn set_top_level_scoring_clause(&mut self) -> Result<()> {
                match self {
                    $( Self::$Variant(inner) => inner.set_top_level_scoring_clause(), )+
                }
            }
        }
    };
}

either_scorer_supplier!(
    pub ScorerSupplierEnum2
    => { bulk: BulkScorerEnum2, scorer: ScorerEnum2 }
    { A: A, B: B }
);

either_scorer_supplier!(
    pub ScorerSupplierEnum3
    => { bulk: BulkScorerEnum3, scorer: ScorerEnum3 }
    { A: A, B: B ,C:C}
);

either_scorer_supplier!(
    pub ScorerSupplierEnum4
    => { bulk: BulkScorerEnum4, scorer: ScorerEnum4 }
    { A: A, B: B ,C:C,D:D}
);
either_scorer_supplier!(
    pub ScorerSupplierEnum5
    => { bulk: BulkScorerEnum5, scorer: ScorerEnum5 }
    { A: A, B: B ,C:C, D:D,E:E }
);
either_scorer_supplier!(
    pub ScorerSupplierEnum6
    => { bulk: BulkScorerEnum6, scorer: ScorerEnum6 }
    { A: A, B: B ,C:C, D:D,E:E,F:F }
);
either_scorer_supplier!(
    pub ScorerSupplierEnum7
    => { bulk: BulkScorerEnum7, scorer: ScorerEnum7 }
    { A: A, B: B ,C:C, D:D,E:E,F:F,G:G }
);
either_scorer_supplier!(
    pub ScorerSupplierEnum8
    => { bulk: BulkScorerEnum8, scorer: ScorerEnum8 }
    { A: A, B: B ,C:C, D:D,E:E,F:F,G:G,H:H }
);
either_scorer_supplier!(
    pub ScorerSupplierEnum9
    => { bulk: BulkScorerEnum9, scorer: ScorerEnum9 }
    { A: A, B: B ,C:C, D:D,E:E,F:F,G:G,H:H,I:I }
);
either_scorer_supplier!(
    pub ScorerSupplierEnum10
    => { bulk: BulkScorerEnum10, scorer: ScorerEnum10 }
    { A: A, B: B ,C:C, D:D,E:E,F:F,G:G,H:H,I:I,J:J }
);
either_scorer_supplier!(
    pub ScorerSupplierEnum11
    => { bulk: BulkScorerEnum11, scorer: ScorerEnum11 }
    { A: A, B: B ,C:C, D:D,E:E,F:F,G:G,H:H,I:I,J:J,K:K }
);
either_scorer_supplier!(
    pub ScorerSupplierEnum12
    => { bulk: BulkScorerEnum12, scorer: ScorerEnum12 }
    { A: A, B: B ,C:C, D:D,E:E,F:F,G:G,H:H,I:I,J:J,K:K,L:L }
);

pub enum ScorerSupplierEnum<LR>
where
    LR: LeafReader,
{
    Term(crate::core::search::term_query::TermSs<LR>),
    MatchAll(crate::core::search::match_all_docs_query::MatchAllSs),
    MatchNoDocs(crate::core::search::match_no_docs_query::MatchNoDocsSs),
    Dummy(crate::core::search::dummy::dummy_scorer_supplier::DummyScorerSupplier),
    FieldExists(crate::core::search::field_exists_query::FieldExistsESs<LR>),
    PointRange(crate::core::search::point_range_query::PointRangeSs<LR>),
    SortedNumericDocValuesSet(
        crate::core::document::sorted_numeric_doc_values_set_query::SNDVSQSs<LR>,
    ),
    SortedNumericDocValuesRange(
        crate::core::document::sorted_numeric_doc_values_range_query::SNDVRQSs<LR>,
    ),
    SortedSetDocValuesRange(
        crate::core::document::sorted_set_doc_values_range_query::SSDVRQSs<LR>,
    ),
    IndexSortSortedNumericDocValuesRange(
        crate::core::search::index_sort_sorted_numeric_doc_values_range_query::ISSNDVRQSs<LR>,
    ),
    ConstantScore(crate::core::search::constant_score_query::ConstantScoreScorerSupplier<LR>),
    Cached(crate::core::search::lru_query_cache::CachingWrapperWeightSupplier<LR>),
}

impl<LR> ScorerSupplier<LR> for ScorerSupplierEnum<LR>
where
    LR: LeafReader,
{
    type Scorer = ScorerEnum<LR>;
    type BulkScorer = BulkScorerEnum<LR>;

    fn get(&mut self, lead_cost: i64, context: &LeafReaderContext<LR>) -> Result<Self::Scorer> {
        match self {
            Self::Term(inner) => inner.get(lead_cost, context).map(ScorerEnum::Term),
            Self::MatchAll(inner) => inner.get(lead_cost, context).map(ScorerEnum::MatchAll),
            Self::MatchNoDocs(inner) => inner.get(lead_cost, context).map(ScorerEnum::MatchNoDocs),
            Self::Dummy(inner) => inner.get(lead_cost, context).map(ScorerEnum::Dummy),
            Self::FieldExists(inner) => inner.get(lead_cost, context).map(ScorerEnum::FieldExists),
            Self::PointRange(inner) => inner.get(lead_cost, context).map(ScorerEnum::PointRange),
            Self::SortedNumericDocValuesSet(inner) => inner
                .get(lead_cost, context)
                .map(ScorerEnum::SortedNumericDocValuesSet),
            Self::SortedNumericDocValuesRange(inner) => inner
                .get(lead_cost, context)
                .map(ScorerEnum::SortedNumericDocValuesRange),
            Self::SortedSetDocValuesRange(inner) => inner
                .get(lead_cost, context)
                .map(ScorerEnum::SortedSetDocValuesRange),
            Self::IndexSortSortedNumericDocValuesRange(inner) => inner
                .get(lead_cost, context)
                .map(ScorerEnum::IndexSortSortedNumericDocValuesRange),
            Self::ConstantScore(inner) => inner.get(lead_cost, context).map(ScorerEnum::ConstantScore),
            Self::Cached(inner) => inner.get(lead_cost, context).map(ScorerEnum::Cached),
        }
    }

    fn bulk_scorer(
        &mut self,
        context: &LeafReaderContext<LR>,
    ) -> Result<Option<Self::BulkScorer>> {
        match self {
            Self::Term(inner) => inner
                .bulk_scorer(context)
                .map(|opt| opt.map(BulkScorerEnum::Term)),
            Self::MatchAll(inner) => inner
                .bulk_scorer(context)
                .map(|opt| opt.map(BulkScorerEnum::MatchAll)),
            Self::MatchNoDocs(inner) => inner
                .bulk_scorer(context)
                .map(|opt| opt.map(BulkScorerEnum::MatchNoDocs)),
            Self::Dummy(inner) => inner
                .bulk_scorer(context)
                .map(|opt| opt.map(BulkScorerEnum::Dummy)),
            Self::FieldExists(inner) => inner
                .bulk_scorer(context)
                .map(|opt| opt.map(BulkScorerEnum::FieldExists)),
            Self::PointRange(inner) => inner
                .bulk_scorer(context)
                .map(|opt| opt.map(BulkScorerEnum::PointRange)),
            Self::SortedNumericDocValuesSet(inner) => inner
                .bulk_scorer(context)
                .map(|opt| opt.map(BulkScorerEnum::SortedNumericDocValuesSet)),
            Self::SortedNumericDocValuesRange(inner) => inner
                .bulk_scorer(context)
                .map(|opt| opt.map(BulkScorerEnum::SortedNumericDocValuesRange)),
            Self::SortedSetDocValuesRange(inner) => inner
                .bulk_scorer(context)
                .map(|opt| opt.map(BulkScorerEnum::SortedSetDocValuesRange)),
            Self::IndexSortSortedNumericDocValuesRange(inner) => inner
                .bulk_scorer(context)
                .map(|opt| opt.map(BulkScorerEnum::IndexSortSortedNumericDocValuesRange)),
            Self::ConstantScore(inner) => inner
                .bulk_scorer(context)
                .map(|opt| opt.map(BulkScorerEnum::ConstantScore)),
            Self::Cached(inner) => inner
                .bulk_scorer(context)
                .map(|opt| opt.map(BulkScorerEnum::Cached)),
        }
    }

    fn cost(&mut self, context: &LeafReaderContext<LR>) -> Result<i64> {
        match self {
            Self::Term(inner) => inner.cost(context),
            Self::MatchAll(inner) => inner.cost(context),
            Self::MatchNoDocs(inner) => inner.cost(context),
            Self::Dummy(inner) => inner.cost(context),
            Self::FieldExists(inner) => inner.cost(context),
            Self::PointRange(inner) => inner.cost(context),
            Self::SortedNumericDocValuesSet(inner) => inner.cost(context),
            Self::SortedNumericDocValuesRange(inner) => inner.cost(context),
            Self::SortedSetDocValuesRange(inner) => inner.cost(context),
            Self::IndexSortSortedNumericDocValuesRange(inner) => inner.cost(context),
            Self::ConstantScore(inner) => inner.cost(context),
            Self::Cached(inner) => inner.cost(context),
        }
    }

    fn set_top_level_scoring_clause(&mut self) -> Result<()> {
        match self {
            Self::Term(inner) => inner.set_top_level_scoring_clause(),
            Self::MatchAll(inner) => inner.set_top_level_scoring_clause(),
            Self::MatchNoDocs(inner) => inner.set_top_level_scoring_clause(),
            Self::Dummy(inner) => inner.set_top_level_scoring_clause(),
            Self::FieldExists(inner) => inner.set_top_level_scoring_clause(),
            Self::PointRange(inner) => inner.set_top_level_scoring_clause(),
            Self::SortedNumericDocValuesSet(inner) => inner.set_top_level_scoring_clause(),
            Self::SortedNumericDocValuesRange(inner) => inner.set_top_level_scoring_clause(),
            Self::SortedSetDocValuesRange(inner) => inner.set_top_level_scoring_clause(),
            Self::IndexSortSortedNumericDocValuesRange(inner) => inner.set_top_level_scoring_clause(),
            Self::ConstantScore(inner) => inner.set_top_level_scoring_clause(),
            Self::Cached(inner) => inner.set_top_level_scoring_clause(),
        }
    }
}
