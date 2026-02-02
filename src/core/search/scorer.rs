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
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::search::doc_id_set_iterator::{
    DocIdSetIterator, DocIdSetIteratorEnum2, DocIdSetIteratorEnum3, DocIdSetIteratorEnum4,
    DocIdSetIteratorEnum5, DocIdSetIteratorEnum6, DocIdSetIteratorEnum7, DocIdSetIteratorEnum8,
    DocIdSetIteratorEnum9, DocIdSetIteratorEnum10, DocIdSetIteratorEnum11, DocIdSetIteratorEnum12,
};
use crate::core::search::scorable::{
    ChildScorable, Scorable, ScorableEnum2, ScorableEnum3, ScorableEnum4, ScorableEnum5,
    ScorableEnum6, ScorableEnum7, ScorableEnum8, ScorableEnum9, ScorableEnum10, ScorableEnum11,
    ScorableEnum12,
};
use crate::core::search::two_phase_iterator::{
    TwoPhaseIterator, TwoPhaseIteratorEnum2, TwoPhaseIteratorEnum3, TwoPhaseIteratorEnum4,
    TwoPhaseIteratorEnum5, TwoPhaseIteratorEnum6, TwoPhaseIteratorEnum7, TwoPhaseIteratorEnum8,
    TwoPhaseIteratorEnum9, TwoPhaseIteratorEnum10, TwoPhaseIteratorEnum11, TwoPhaseIteratorEnum12,
};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::util::error::lucene_error::Result;

/// Expert: Common scoring functionality for different types of queries.
///
/// A `Scorer` exposes an `iterator_mut()` over documents matching a query in
/// increasing order of doc id.
pub trait Scorer: Scorable {
    /// Concrete iterator type over matching documents.
    type DocIdSetIterator: DocIdSetIterator;
    type DocIdSetIteratorRef<'a>: DocIdSetIterator
    where
        Self: 'a;
    type DocIdSetIteratorMut<'a>: DocIdSetIterator
    where
        Self: 'a;

    /// Optional two-phase iterator type (return `None` if unsupported).
    type TwoPhaseIter: TwoPhaseIterator;
    type TwoPhaseIterRef<'a>: TwoPhaseIterator
    where
        Self: 'a;
    type TwoPhaseIterMut<'a>: TwoPhaseIterator
    where
        Self: 'a;
    /// Returns the doc ID that is currently being scored.
    fn doc_id(&mut self) -> Result<i32>;

    /// Return a [`DocIdSetIterator`] over matching documents.
    ///
    /// The returned iterator will either be positioned on `-1` if no documents
    /// have been scored yet, `NO_MORE_DOCS` if all documents have been scored already,
    /// or the last document id that has been scored otherwise.
    /// # Warning
    /// The returned iterator is a *view*: calling this method several times must
    /// return iterators that share the same state.
    fn iterator(&self) -> Self::DocIdSetIteratorRef<'_>;

    fn iterator_mut(&mut self) -> Self::DocIdSetIteratorMut<'_>;

    /// Return a [`DocIdSetIterator`] over matching documents, transferring ownership.
    ///
    /// Unlike [`iterator`](Self::iterator), this method takes ownership of the
    /// underlying iterator rather than returning a view.
    fn take_iterator(self) -> Self::DocIdSetIterator;

    /// Optional: Return a two-phase iterator view of this scorer.
    ///
    /// A return value of `None` indicates that two-phase iteration is not supported.
    ///
    /// Note that the returned [`TwoPhaseIterator`]'s approximation must advance
    /// synchronously with `iterator()`: advancing the approximation must advance
    /// the iterator and vice-versa.
    ///
    /// The default implementation returns `None`.
    /// # Warning
    /// The returned iterator is a *view*: calling this method several times must
    /// return iterators that share the same state.
    fn two_phase_iterator(&self) -> Result<Option<Self::TwoPhaseIterRef<'_>>> {
        Ok(None)
    }

    /// Optional: Return a two-phase iterator view of this scorer.
    ///
    /// A return value of `None` indicates that two-phase iteration is not supported.
    ///
    /// Note that the returned [`TwoPhaseIterator`]'s approximation must advance
    /// synchronously with `iterator()`: advancing the approximation must advance
    /// the iterator and vice-versa.
    ///
    /// The default implementation returns `None`.
    /// # Warning
    /// The returned iterator is a *view*: calling this method several times must
    /// return iterators that share the same state.
    fn two_phase_iterator_mut(&mut self) -> Result<Option<Self::TwoPhaseIterMut<'_>>> {
        Ok(None)
    }

    /// Optional: Return a two-phase iterator for this scorer, transferring ownership.
    ///
    /// By default, this returns `None`.
    fn take_two_phase_iterator(self) -> Result<Option<Self::TwoPhaseIter>>
    where
        Self: Sized,
    {
        Ok(None)
    }

    /// Advance to the block of documents that contains `target` in order to get
    /// scoring information about this block.
    ///
    /// This method is implicitly called by `DocIdSetIterator::advance` and
    /// `DocIdSetIterator::next_doc` on the returned doc ID. Calling this method
    /// doesn't modify the current `doc_id()`. It returns a number that is greater
    /// than or equal to all documents contained in the current block, but less than
    /// any doc IDs of the next block. `target` must be `>= doc_id()` as well as all
    /// targets that have been passed to `advance_shallow` so far.
    ///
    /// The default implementation returns `NO_MORE_DOCS`.
    fn advance_shallow(&mut self, _target: i32) -> Result<i32> {
        Ok(NO_MORE_DOCS)
    }
    fn default_advance_shallow(&mut self, _target: i32) -> Result<i32> {
        Ok(NO_MORE_DOCS)
    }

    /// Return the maximum score that documents between the last `target` that this
    /// iterator was `advance_shallow`’d to (included) and `up_to` (included) can get.
    fn get_max_score(&mut self, up_to: i32) -> Result<f32>;

    fn default_cost(&mut self) -> Result<i64> {
        self.iterator_mut().cost()
    }
    fn has_two_phase_iterator(&self) -> TwoPhaseState;
}
pub type ScorerDisi<S> = <S as Scorer>::DocIdSetIterator;
pub type ScorerDisiMut<'a, S> = <S as Scorer>::DocIdSetIteratorMut<'a>;
pub type ScorerDisiRef<'a, S> = <S as Scorer>::DocIdSetIteratorRef<'a>;
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd)]
pub enum TwoPhaseState {
    /// Has two_phase_iterator
    Yes,
    /// no two_phase_iterator
    No,
    /// may or may not present, check with [`Scorer::two_phase_iterator`]
    MayBe,
}

macro_rules! either_scorer {
    (
        $vis:vis $name:ident {
            iter = $iter_ty:ident,
            two_phase = $two_phase_ty:ident,
            scorable = $scorable_ty:ident;
            $( $Variant:ident : $T:ident ),+ $(,)?
        }
    ) => {
        $vis enum $name<$( $T ),+> {
            $( $Variant($T), )+
        }

        impl<$( $T ),+> Scorable for $name<$( $T ),+>
        where
            $( $T: Scorer ),+
        {
            #[inline]
            fn score(&mut self) -> Result<f32> {
                match self { $( Self::$Variant(inner) => inner.score(), )+ }
            }

            #[inline]
            fn smoothing_score(&mut self, doc_id: i32) -> Result<f32> {
                match self { $( Self::$Variant(inner) => inner.smoothing_score(doc_id), )+ }
            }

            #[inline]
            fn set_min_competitive_score(&mut self, min_score: f32) -> Result<()> {
                match self { $( Self::$Variant(inner) => inner.set_min_competitive_score(min_score), )+ }
            }

            type Scorable = $scorable_ty<$( < $T as Scorable >::Scorable ),+>;

            #[inline]
            fn get_children(&self) -> Result<Vec<ChildScorable<Self::Scorable>>> {
                match self {
                    $(
                        Self::$Variant(inner) => {
                            let children = inner.get_children()?;
                            let mapped = children
                                .into_iter()
                                .map(|child| ChildScorable {
                                    child: Self::Scorable::$Variant(child.child),
                                    relationship: child.relationship,
                                })
                                .collect();
                            Ok(mapped)
                        }
                    ),+
                }
            }

            #[inline]
            fn cost(&mut self) -> Result<i64> {
                match self { $( Self::$Variant(inner) => inner.default_cost(), )+ }
            }
        }

        impl<$( $T ),+> Scorer for $name<$( $T ),+>
        where
            $( $T: Scorer ),+
        {
            type DocIdSetIterator =
                $iter_ty<$( < $T as Scorer >::DocIdSetIterator ),+>;

            type DocIdSetIteratorRef<'a> =
                $iter_ty<$( < $T as Scorer >::DocIdSetIteratorRef<'a> ),+>
            where
                Self: 'a;

            type DocIdSetIteratorMut<'a> =
                $iter_ty<$( < $T as Scorer >::DocIdSetIteratorMut<'a> ),+>
            where
                Self: 'a;

            type TwoPhaseIter =
                $two_phase_ty<$( < $T as Scorer >::TwoPhaseIter ),+>;
            type TwoPhaseIterRef<'a> =
                $two_phase_ty<$( < $T as Scorer >::TwoPhaseIterRef<'a> ),+>
            where
                Self: 'a;
            type TwoPhaseIterMut<'a> =
                $two_phase_ty<$( < $T as Scorer >::TwoPhaseIterMut<'a> ),+>
            where
                Self: 'a;

            #[inline]
            fn doc_id(&mut self) -> Result<i32> {
                match self { $( Self::$Variant(inner) => inner.doc_id(), )+ }
            }

            #[inline]
            fn iterator(&self) -> Self::DocIdSetIteratorRef<'_> {
                match self {
                    $( Self::$Variant(inner) => $iter_ty::$Variant(inner.iterator()), )+
                }
            }

            #[inline]
            fn iterator_mut(&mut self) -> Self::DocIdSetIteratorMut<'_> {
                match self {
                    $( Self::$Variant(inner) => $iter_ty::$Variant(inner.iterator_mut()), )+
                }
            }

            #[inline]
            fn take_iterator(self) -> Self::DocIdSetIterator {
                match self {
                    $( Self::$Variant(inner) => $iter_ty::$Variant(inner.take_iterator()), )+
                }
            }

            #[inline]
            fn two_phase_iterator(&self) -> Result<Option<Self::TwoPhaseIterRef<'_>>> {
                match self {
                    $( Self::$Variant(inner) =>
                        inner.two_phase_iterator().map(|res| res.map(|it| $two_phase_ty::$Variant(it))), )+
                }
            }

            #[inline]
            fn two_phase_iterator_mut(&mut self) -> Result<Option<Self::TwoPhaseIterMut<'_>>> {
                match self {
                    $( Self::$Variant(inner) =>
                        inner
                            .two_phase_iterator_mut()
                            .map(|res| res.map(|it| $two_phase_ty::$Variant(it))), )+
                }
            }

            #[inline]
            fn take_two_phase_iterator(self) -> Result<Option<Self::TwoPhaseIter>> {
                match self {
                    $( Self::$Variant(inner) =>
                        inner.take_two_phase_iterator().map(|res| res.map(|it| $two_phase_ty::$Variant(it))), )+
                }
            }

            #[inline]
            fn advance_shallow(&mut self, target: i32) -> Result<i32> {
                match self { $( Self::$Variant(inner) => inner.advance_shallow(target), )+ }
            }
            #[inline]
            fn default_advance_shallow(&mut self, target: i32) -> Result<i32> {
                match self { $( Self::$Variant(inner) => inner.default_advance_shallow(target), )+ }
            }

            #[inline]
            fn get_max_score(&mut self, up_to: i32) -> Result<f32> {
                match self { $( Self::$Variant(inner) => inner.get_max_score(up_to), )+ }
            }

            #[inline]
            fn default_cost(&mut self) -> Result<i64> {
                match self { $( Self::$Variant(inner) => inner.default_cost(), )+ }
            }
             #[inline]
            fn has_two_phase_iterator(&self) -> TwoPhaseState{
                match self { $( Self::$Variant(inner) => inner.has_two_phase_iterator(), )+ }
            }
        }
    };
}
either_scorer!(
    pub ScorerEnum2 {
        iter = DocIdSetIteratorEnum2,
        two_phase = TwoPhaseIteratorEnum2,
        scorable = ScorableEnum2;
        A: A, B: B,
    }
);
either_scorer!(
    pub ScorerEnum3 {
        iter = DocIdSetIteratorEnum3,
        two_phase = TwoPhaseIteratorEnum3,
        scorable = ScorableEnum3;
        A: A, B: B,C: C,
    }
);
either_scorer!(
    pub ScorerEnum4 {
        iter = DocIdSetIteratorEnum4,
        two_phase = TwoPhaseIteratorEnum4,
        scorable = ScorableEnum4;
        A: A, B: B,C: C,D:D
    }
);
either_scorer!(
    pub ScorerEnum5 {
        iter = DocIdSetIteratorEnum5,
        two_phase = TwoPhaseIteratorEnum5,
        scorable = ScorableEnum5;
        A: A, B: B,C: C, D: D,E: E,
    }
);
either_scorer!(
    pub ScorerEnum6 {
        iter = DocIdSetIteratorEnum6,
        two_phase = TwoPhaseIteratorEnum6,
        scorable = ScorableEnum6;
        A: A, B: B,C: C, D: D,E: E,F: F,
    }
);
either_scorer!(
    pub ScorerEnum7 {
        iter = DocIdSetIteratorEnum7,
        two_phase = TwoPhaseIteratorEnum7,
        scorable = ScorableEnum7;
        A: A, B: B,C: C, D: D,E: E,F: F,G: G,
    }
);
either_scorer!(
    pub ScorerEnum8 {
        iter = DocIdSetIteratorEnum8,
        two_phase = TwoPhaseIteratorEnum8,
        scorable = ScorableEnum8;
        A: A, B: B,C: C, D: D,E: E,F: F,G: G,H: H,
    }
);
either_scorer!(
    pub ScorerEnum9 {
        iter = DocIdSetIteratorEnum9,
        two_phase = TwoPhaseIteratorEnum9,
        scorable = ScorableEnum9;
        A: A, B: B,C: C, D: D,E: E,F: F,G: G,H: H,I: I,
    }
);
either_scorer!(
    pub ScorerEnum10 {
        iter = DocIdSetIteratorEnum10,
        two_phase = TwoPhaseIteratorEnum10,
        scorable = ScorableEnum10;
        A: A, B: B,C: C, D: D,E: E,F: F,G: G,H: H,I: I,J: J,
    }
);
either_scorer!(
    pub ScorerEnum11 {
        iter = DocIdSetIteratorEnum11,
        two_phase = TwoPhaseIteratorEnum11,
        scorable = ScorableEnum11;
        A: A, B: B,C: C, D: D,E: E,F: F,G: G,H: H,I: I,J: J,K: K,
    }
);
either_scorer!(
    pub ScorerEnum12 {
        iter = DocIdSetIteratorEnum12,
        two_phase = TwoPhaseIteratorEnum12,
        scorable = ScorableEnum12;
        A: A, B: B,C: C, D: D,E: E,F: F,G: G,H: H,I: I,J: J,K: K,L: L,
    }
);

pub enum ScorerEnum<LR>
where
    LR: LeafReader,
{
    Term(crate::core::search::term_query::TermSsScorer<LR>),
    MatchAll(crate::core::search::match_all_docs_query::MatchAllSsScorer),
    MatchNoDocs(crate::core::search::match_no_docs_query::MatchNoDocsSsScorer<LR>),
    Dummy(crate::core::search::dummy::dummy_scorer::DummyScorer),
    FieldExists(crate::core::search::field_exists_query::FieldExistsSsScorer<LR>),
    PointRange(crate::core::search::point_range_query::PointRangeSsScorer<LR>),
    SortedNumericDocValuesSet(
        crate::core::document::sorted_numeric_doc_values_set_query::SNDVSQSsScorer<LR>,
    ),
    SortedNumericDocValuesRange(
        crate::core::document::sorted_numeric_doc_values_range_query::SNDVRQSsScorer<LR>,
    ),
    SortedSetDocValuesRange(
        crate::core::document::sorted_set_doc_values_range_query::SSDVRQSsScorer<LR>,
    ),
    IndexSortSortedNumericDocValuesRange(
        crate::core::search::index_sort_sorted_numeric_doc_values_range_query::ISSNDVRQSsScorer<LR>,
    ),
    ConstantScore(crate::core::search::constant_score_query::ConstantScoreScorerEnum<LR>),
    Cached(crate::core::search::lru_query_cache::CachingWrapperWeightScorer<LR>),
}

impl<LR> Scorable for ScorerEnum<LR>
where
    LR: LeafReader,
{
    fn score(&mut self) -> Result<f32> {
        match self {
            Self::Term(inner) => inner.score(),
            Self::MatchAll(inner) => inner.score(),
            Self::MatchNoDocs(inner) => inner.score(),
            Self::Dummy(inner) => inner.score(),
            Self::FieldExists(inner) => inner.score(),
            Self::PointRange(inner) => inner.score(),
            Self::SortedNumericDocValuesSet(inner) => inner.score(),
            Self::SortedNumericDocValuesRange(inner) => inner.score(),
            Self::SortedSetDocValuesRange(inner) => inner.score(),
            Self::IndexSortSortedNumericDocValuesRange(inner) => inner.score(),
            Self::ConstantScore(inner) => inner.score(),
            Self::Cached(inner) => inner.score(),
        }
    }

    fn smoothing_score(&mut self, doc_id: i32) -> Result<f32> {
        match self {
            Self::Term(inner) => inner.smoothing_score(doc_id),
            Self::MatchAll(inner) => inner.smoothing_score(doc_id),
            Self::MatchNoDocs(inner) => inner.smoothing_score(doc_id),
            Self::Dummy(inner) => inner.smoothing_score(doc_id),
            Self::FieldExists(inner) => inner.smoothing_score(doc_id),
            Self::PointRange(inner) => inner.smoothing_score(doc_id),
            Self::SortedNumericDocValuesSet(inner) => inner.smoothing_score(doc_id),
            Self::SortedNumericDocValuesRange(inner) => inner.smoothing_score(doc_id),
            Self::SortedSetDocValuesRange(inner) => inner.smoothing_score(doc_id),
            Self::IndexSortSortedNumericDocValuesRange(inner) => inner.smoothing_score(doc_id),
            Self::ConstantScore(inner) => inner.smoothing_score(doc_id),
            Self::Cached(inner) => inner.smoothing_score(doc_id),
        }
    }

    fn set_min_competitive_score(&mut self, min_score: f32) -> Result<()> {
        match self {
            Self::Term(inner) => inner.set_min_competitive_score(min_score),
            Self::MatchAll(inner) => inner.set_min_competitive_score(min_score),
            Self::MatchNoDocs(inner) => inner.set_min_competitive_score(min_score),
            Self::Dummy(inner) => inner.set_min_competitive_score(min_score),
            Self::FieldExists(inner) => inner.set_min_competitive_score(min_score),
            Self::PointRange(inner) => inner.set_min_competitive_score(min_score),
            Self::SortedNumericDocValuesSet(inner) => inner.set_min_competitive_score(min_score),
            Self::SortedNumericDocValuesRange(inner) => inner.set_min_competitive_score(min_score),
            Self::SortedSetDocValuesRange(inner) => inner.set_min_competitive_score(min_score),
            Self::IndexSortSortedNumericDocValuesRange(inner) => {
                inner.set_min_competitive_score(min_score)
            },
            Self::ConstantScore(inner) => inner.set_min_competitive_score(min_score),
            Self::Cached(inner) => inner.set_min_competitive_score(min_score),
        }
    }

    type Scorable = ScorableEnum12<
        <crate::core::search::term_query::TermSsScorer<LR> as Scorable>::Scorable,
        <crate::core::search::match_all_docs_query::MatchAllSsScorer as Scorable>::Scorable,
        <crate::core::search::match_no_docs_query::MatchNoDocsSsScorer<LR> as Scorable>::Scorable,
        <crate::core::search::dummy::dummy_scorer::DummyScorer as Scorable>::Scorable,
        <crate::core::search::field_exists_query::FieldExistsSsScorer<LR> as Scorable>::Scorable,
        <crate::core::search::point_range_query::PointRangeSsScorer<LR> as Scorable>::Scorable,
        <crate::core::document::sorted_numeric_doc_values_set_query::SNDVSQSsScorer<LR> as Scorable>::Scorable,
        <crate::core::document::sorted_numeric_doc_values_range_query::SNDVRQSsScorer<LR> as Scorable>::Scorable,
        <crate::core::document::sorted_set_doc_values_range_query::SSDVRQSsScorer<LR> as Scorable>::Scorable,
        <crate::core::search::index_sort_sorted_numeric_doc_values_range_query::ISSNDVRQSsScorer<LR> as Scorable>::Scorable,
        <crate::core::search::constant_score_query::ConstantScoreScorerEnum<LR> as Scorable>::Scorable,
        <crate::core::search::lru_query_cache::CachingWrapperWeightScorer<LR> as Scorable>::Scorable,
    >;

    fn get_children(&self) -> Result<Vec<ChildScorable<Self::Scorable>>> {
        match self {
            Self::Term(inner) => inner.get_children().map(|children| {
                children
                    .into_iter()
                    .map(|child| ChildScorable {
                        child: Self::Scorable::A(child.child),
                        relationship: child.relationship,
                    })
                    .collect()
            }),
            Self::MatchAll(inner) => inner.get_children().map(|children| {
                children
                    .into_iter()
                    .map(|child| ChildScorable {
                        child: Self::Scorable::B(child.child),
                        relationship: child.relationship,
                    })
                    .collect()
            }),
            Self::MatchNoDocs(inner) => inner.get_children().map(|children| {
                children
                    .into_iter()
                    .map(|child| ChildScorable {
                        child: Self::Scorable::C(child.child),
                        relationship: child.relationship,
                    })
                    .collect()
            }),
            Self::Dummy(inner) => inner.get_children().map(|children| {
                children
                    .into_iter()
                    .map(|child| ChildScorable {
                        child: Self::Scorable::D(child.child),
                        relationship: child.relationship,
                    })
                    .collect()
            }),
            Self::FieldExists(inner) => inner.get_children().map(|children| {
                children
                    .into_iter()
                    .map(|child| ChildScorable {
                        child: Self::Scorable::E(child.child),
                        relationship: child.relationship,
                    })
                    .collect()
            }),
            Self::PointRange(inner) => inner.get_children().map(|children| {
                children
                    .into_iter()
                    .map(|child| ChildScorable {
                        child: Self::Scorable::F(child.child),
                        relationship: child.relationship,
                    })
                    .collect()
            }),
            Self::SortedNumericDocValuesSet(inner) => inner.get_children().map(|children| {
                children
                    .into_iter()
                    .map(|child| ChildScorable {
                        child: Self::Scorable::G(child.child),
                        relationship: child.relationship,
                    })
                    .collect()
            }),
            Self::SortedNumericDocValuesRange(inner) => inner.get_children().map(|children| {
                children
                    .into_iter()
                    .map(|child| ChildScorable {
                        child: Self::Scorable::H(child.child),
                        relationship: child.relationship,
                    })
                    .collect()
            }),
            Self::SortedSetDocValuesRange(inner) => inner.get_children().map(|children| {
                children
                    .into_iter()
                    .map(|child| ChildScorable {
                        child: Self::Scorable::I(child.child),
                        relationship: child.relationship,
                    })
                    .collect()
            }),
            Self::IndexSortSortedNumericDocValuesRange(inner) => inner.get_children().map(|children| {
                children
                    .into_iter()
                    .map(|child| ChildScorable {
                        child: Self::Scorable::J(child.child),
                        relationship: child.relationship,
                    })
                    .collect()
            }),
            Self::ConstantScore(inner) => inner.get_children().map(|children| {
                children
                    .into_iter()
                    .map(|child| ChildScorable {
                        child: Self::Scorable::K(child.child),
                        relationship: child.relationship,
                    })
                    .collect()
            }),
            Self::Cached(inner) => inner.get_children().map(|children| {
                children
                    .into_iter()
                    .map(|child| ChildScorable {
                        child: Self::Scorable::L(child.child),
                        relationship: child.relationship,
                    })
                    .collect()
            }),
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

impl<LR> Scorer for ScorerEnum<LR>
where
    LR: LeafReader,
{
    type DocIdSetIterator = DocIdSetIteratorEnum12<
        <crate::core::search::term_query::TermSsScorer<LR> as Scorer>::DocIdSetIterator,
        <crate::core::search::match_all_docs_query::MatchAllSsScorer as Scorer>::DocIdSetIterator,
        <crate::core::search::match_no_docs_query::MatchNoDocsSsScorer<LR> as Scorer>::DocIdSetIterator,
        <crate::core::search::dummy::dummy_scorer::DummyScorer as Scorer>::DocIdSetIterator,
        <crate::core::search::field_exists_query::FieldExistsSsScorer<LR> as Scorer>::DocIdSetIterator,
        <crate::core::search::point_range_query::PointRangeSsScorer<LR> as Scorer>::DocIdSetIterator,
        <crate::core::document::sorted_numeric_doc_values_set_query::SNDVSQSsScorer<LR> as Scorer>::DocIdSetIterator,
        <crate::core::document::sorted_numeric_doc_values_range_query::SNDVRQSsScorer<LR> as Scorer>::DocIdSetIterator,
        <crate::core::document::sorted_set_doc_values_range_query::SSDVRQSsScorer<LR> as Scorer>::DocIdSetIterator,
        <crate::core::search::index_sort_sorted_numeric_doc_values_range_query::ISSNDVRQSsScorer<LR> as Scorer>::DocIdSetIterator,
        <crate::core::search::constant_score_query::ConstantScoreScorerEnum<LR> as Scorer>::DocIdSetIterator,
        <crate::core::search::lru_query_cache::CachingWrapperWeightScorer<LR> as Scorer>::DocIdSetIterator,
    >;
    type DocIdSetIteratorRef<'a> = DocIdSetIteratorEnum12<
        <crate::core::search::term_query::TermSsScorer<LR> as Scorer>::DocIdSetIteratorRef<'a>,
        <crate::core::search::match_all_docs_query::MatchAllSsScorer as Scorer>::DocIdSetIteratorRef<'a>,
        <crate::core::search::match_no_docs_query::MatchNoDocsSsScorer<LR> as Scorer>::DocIdSetIteratorRef<'a>,
        <crate::core::search::dummy::dummy_scorer::DummyScorer as Scorer>::DocIdSetIteratorRef<'a>,
        <crate::core::search::field_exists_query::FieldExistsSsScorer<LR> as Scorer>::DocIdSetIteratorRef<'a>,
        <crate::core::search::point_range_query::PointRangeSsScorer<LR> as Scorer>::DocIdSetIteratorRef<'a>,
        <crate::core::document::sorted_numeric_doc_values_set_query::SNDVSQSsScorer<LR> as Scorer>::DocIdSetIteratorRef<'a>,
        <crate::core::document::sorted_numeric_doc_values_range_query::SNDVRQSsScorer<LR> as Scorer>::DocIdSetIteratorRef<'a>,
        <crate::core::document::sorted_set_doc_values_range_query::SSDVRQSsScorer<LR> as Scorer>::DocIdSetIteratorRef<'a>,
        <crate::core::search::index_sort_sorted_numeric_doc_values_range_query::ISSNDVRQSsScorer<LR> as Scorer>::DocIdSetIteratorRef<'a>,
        <crate::core::search::constant_score_query::ConstantScoreScorerEnum<LR> as Scorer>::DocIdSetIteratorRef<'a>,
        <crate::core::search::lru_query_cache::CachingWrapperWeightScorer<LR> as Scorer>::DocIdSetIteratorRef<'a>,
    >
    where
        Self: 'a;
    type DocIdSetIteratorMut<'a> = DocIdSetIteratorEnum12<
        <crate::core::search::term_query::TermSsScorer<LR> as Scorer>::DocIdSetIteratorMut<'a>,
        <crate::core::search::match_all_docs_query::MatchAllSsScorer as Scorer>::DocIdSetIteratorMut<'a>,
        <crate::core::search::match_no_docs_query::MatchNoDocsSsScorer<LR> as Scorer>::DocIdSetIteratorMut<'a>,
        <crate::core::search::dummy::dummy_scorer::DummyScorer as Scorer>::DocIdSetIteratorMut<'a>,
        <crate::core::search::field_exists_query::FieldExistsSsScorer<LR> as Scorer>::DocIdSetIteratorMut<'a>,
        <crate::core::search::point_range_query::PointRangeSsScorer<LR> as Scorer>::DocIdSetIteratorMut<'a>,
        <crate::core::document::sorted_numeric_doc_values_set_query::SNDVSQSsScorer<LR> as Scorer>::DocIdSetIteratorMut<'a>,
        <crate::core::document::sorted_numeric_doc_values_range_query::SNDVRQSsScorer<LR> as Scorer>::DocIdSetIteratorMut<'a>,
        <crate::core::document::sorted_set_doc_values_range_query::SSDVRQSsScorer<LR> as Scorer>::DocIdSetIteratorMut<'a>,
        <crate::core::search::index_sort_sorted_numeric_doc_values_range_query::ISSNDVRQSsScorer<LR> as Scorer>::DocIdSetIteratorMut<'a>,
        <crate::core::search::constant_score_query::ConstantScoreScorerEnum<LR> as Scorer>::DocIdSetIteratorMut<'a>,
        <crate::core::search::lru_query_cache::CachingWrapperWeightScorer<LR> as Scorer>::DocIdSetIteratorMut<'a>,
    >
    where
        Self: 'a;

    type TwoPhaseIter = TwoPhaseIteratorEnum12<
        <crate::core::search::term_query::TermSsScorer<LR> as Scorer>::TwoPhaseIter,
        <crate::core::search::match_all_docs_query::MatchAllSsScorer as Scorer>::TwoPhaseIter,
        <crate::core::search::match_no_docs_query::MatchNoDocsSsScorer<LR> as Scorer>::TwoPhaseIter,
        <crate::core::search::dummy::dummy_scorer::DummyScorer as Scorer>::TwoPhaseIter,
        <crate::core::search::field_exists_query::FieldExistsSsScorer<LR> as Scorer>::TwoPhaseIter,
        <crate::core::search::point_range_query::PointRangeSsScorer<LR> as Scorer>::TwoPhaseIter,
        <crate::core::document::sorted_numeric_doc_values_set_query::SNDVSQSsScorer<LR> as Scorer>::TwoPhaseIter,
        <crate::core::document::sorted_numeric_doc_values_range_query::SNDVRQSsScorer<LR> as Scorer>::TwoPhaseIter,
        <crate::core::document::sorted_set_doc_values_range_query::SSDVRQSsScorer<LR> as Scorer>::TwoPhaseIter,
        <crate::core::search::index_sort_sorted_numeric_doc_values_range_query::ISSNDVRQSsScorer<LR> as Scorer>::TwoPhaseIter,
        <crate::core::search::constant_score_query::ConstantScoreScorerEnum<LR> as Scorer>::TwoPhaseIter,
        <crate::core::search::lru_query_cache::CachingWrapperWeightScorer<LR> as Scorer>::TwoPhaseIter,
    >;
    type TwoPhaseIterRef<'a> = TwoPhaseIteratorEnum12<
        <crate::core::search::term_query::TermSsScorer<LR> as Scorer>::TwoPhaseIterRef<'a>,
        <crate::core::search::match_all_docs_query::MatchAllSsScorer as Scorer>::TwoPhaseIterRef<'a>,
        <crate::core::search::match_no_docs_query::MatchNoDocsSsScorer<LR> as Scorer>::TwoPhaseIterRef<'a>,
        <crate::core::search::dummy::dummy_scorer::DummyScorer as Scorer>::TwoPhaseIterRef<'a>,
        <crate::core::search::field_exists_query::FieldExistsSsScorer<LR> as Scorer>::TwoPhaseIterRef<'a>,
        <crate::core::search::point_range_query::PointRangeSsScorer<LR> as Scorer>::TwoPhaseIterRef<'a>,
        <crate::core::document::sorted_numeric_doc_values_set_query::SNDVSQSsScorer<LR> as Scorer>::TwoPhaseIterRef<'a>,
        <crate::core::document::sorted_numeric_doc_values_range_query::SNDVRQSsScorer<LR> as Scorer>::TwoPhaseIterRef<'a>,
        <crate::core::document::sorted_set_doc_values_range_query::SSDVRQSsScorer<LR> as Scorer>::TwoPhaseIterRef<'a>,
        <crate::core::search::index_sort_sorted_numeric_doc_values_range_query::ISSNDVRQSsScorer<LR> as Scorer>::TwoPhaseIterRef<'a>,
        <crate::core::search::constant_score_query::ConstantScoreScorerEnum<LR> as Scorer>::TwoPhaseIterRef<'a>,
        <crate::core::search::lru_query_cache::CachingWrapperWeightScorer<LR> as Scorer>::TwoPhaseIterRef<'a>,
    >
    where
        Self: 'a;
    type TwoPhaseIterMut<'a> = TwoPhaseIteratorEnum12<
        <crate::core::search::term_query::TermSsScorer<LR> as Scorer>::TwoPhaseIterMut<'a>,
        <crate::core::search::match_all_docs_query::MatchAllSsScorer as Scorer>::TwoPhaseIterMut<'a>,
        <crate::core::search::match_no_docs_query::MatchNoDocsSsScorer<LR> as Scorer>::TwoPhaseIterMut<'a>,
        <crate::core::search::dummy::dummy_scorer::DummyScorer as Scorer>::TwoPhaseIterMut<'a>,
        <crate::core::search::field_exists_query::FieldExistsSsScorer<LR> as Scorer>::TwoPhaseIterMut<'a>,
        <crate::core::search::point_range_query::PointRangeSsScorer<LR> as Scorer>::TwoPhaseIterMut<'a>,
        <crate::core::document::sorted_numeric_doc_values_set_query::SNDVSQSsScorer<LR> as Scorer>::TwoPhaseIterMut<'a>,
        <crate::core::document::sorted_numeric_doc_values_range_query::SNDVRQSsScorer<LR> as Scorer>::TwoPhaseIterMut<'a>,
        <crate::core::document::sorted_set_doc_values_range_query::SSDVRQSsScorer<LR> as Scorer>::TwoPhaseIterMut<'a>,
        <crate::core::search::index_sort_sorted_numeric_doc_values_range_query::ISSNDVRQSsScorer<LR> as Scorer>::TwoPhaseIterMut<'a>,
        <crate::core::search::constant_score_query::ConstantScoreScorerEnum<LR> as Scorer>::TwoPhaseIterMut<'a>,
        <crate::core::search::lru_query_cache::CachingWrapperWeightScorer<LR> as Scorer>::TwoPhaseIterMut<'a>,
    >
    where
        Self: 'a;

    fn doc_id(&mut self) -> Result<i32> {
        match self {
            Self::Term(inner) => inner.doc_id(),
            Self::MatchAll(inner) => inner.doc_id(),
            Self::MatchNoDocs(inner) => inner.doc_id(),
            Self::Dummy(inner) => inner.doc_id(),
            Self::FieldExists(inner) => inner.doc_id(),
            Self::PointRange(inner) => inner.doc_id(),
            Self::SortedNumericDocValuesSet(inner) => inner.doc_id(),
            Self::SortedNumericDocValuesRange(inner) => inner.doc_id(),
            Self::SortedSetDocValuesRange(inner) => inner.doc_id(),
            Self::IndexSortSortedNumericDocValuesRange(inner) => inner.doc_id(),
            Self::ConstantScore(inner) => inner.doc_id(),
            Self::Cached(inner) => inner.doc_id(),
        }
    }

    fn iterator(&self) -> Self::DocIdSetIteratorRef<'_> {
        match self {
            Self::Term(inner) => DocIdSetIteratorEnum12::A(inner.iterator()),
            Self::MatchAll(inner) => DocIdSetIteratorEnum12::B(inner.iterator()),
            Self::MatchNoDocs(inner) => DocIdSetIteratorEnum12::C(inner.iterator()),
            Self::Dummy(inner) => DocIdSetIteratorEnum12::D(inner.iterator()),
            Self::FieldExists(inner) => DocIdSetIteratorEnum12::E(inner.iterator()),
            Self::PointRange(inner) => DocIdSetIteratorEnum12::F(inner.iterator()),
            Self::SortedNumericDocValuesSet(inner) => DocIdSetIteratorEnum12::G(inner.iterator()),
            Self::SortedNumericDocValuesRange(inner) => DocIdSetIteratorEnum12::H(inner.iterator()),
            Self::SortedSetDocValuesRange(inner) => DocIdSetIteratorEnum12::I(inner.iterator()),
            Self::IndexSortSortedNumericDocValuesRange(inner) => DocIdSetIteratorEnum12::J(inner.iterator()),
            Self::ConstantScore(inner) => DocIdSetIteratorEnum12::K(inner.iterator()),
            Self::Cached(inner) => DocIdSetIteratorEnum12::L(inner.iterator()),
        }
    }

    fn iterator_mut(&mut self) -> Self::DocIdSetIteratorMut<'_> {
        match self {
            Self::Term(inner) => DocIdSetIteratorEnum12::A(inner.iterator_mut()),
            Self::MatchAll(inner) => DocIdSetIteratorEnum12::B(inner.iterator_mut()),
            Self::MatchNoDocs(inner) => DocIdSetIteratorEnum12::C(inner.iterator_mut()),
            Self::Dummy(inner) => DocIdSetIteratorEnum12::D(inner.iterator_mut()),
            Self::FieldExists(inner) => DocIdSetIteratorEnum12::E(inner.iterator_mut()),
            Self::PointRange(inner) => DocIdSetIteratorEnum12::F(inner.iterator_mut()),
            Self::SortedNumericDocValuesSet(inner) => DocIdSetIteratorEnum12::G(inner.iterator_mut()),
            Self::SortedNumericDocValuesRange(inner) => DocIdSetIteratorEnum12::H(inner.iterator_mut()),
            Self::SortedSetDocValuesRange(inner) => DocIdSetIteratorEnum12::I(inner.iterator_mut()),
            Self::IndexSortSortedNumericDocValuesRange(inner) => DocIdSetIteratorEnum12::J(inner.iterator_mut()),
            Self::ConstantScore(inner) => DocIdSetIteratorEnum12::K(inner.iterator_mut()),
            Self::Cached(inner) => DocIdSetIteratorEnum12::L(inner.iterator_mut()),
        }
    }

    fn take_iterator(self) -> Self::DocIdSetIterator {
        match self {
            Self::Term(inner) => DocIdSetIteratorEnum12::A(inner.take_iterator()),
            Self::MatchAll(inner) => DocIdSetIteratorEnum12::B(inner.take_iterator()),
            Self::MatchNoDocs(inner) => DocIdSetIteratorEnum12::C(inner.take_iterator()),
            Self::Dummy(inner) => DocIdSetIteratorEnum12::D(inner.take_iterator()),
            Self::FieldExists(inner) => DocIdSetIteratorEnum12::E(inner.take_iterator()),
            Self::PointRange(inner) => DocIdSetIteratorEnum12::F(inner.take_iterator()),
            Self::SortedNumericDocValuesSet(inner) => DocIdSetIteratorEnum12::G(inner.take_iterator()),
            Self::SortedNumericDocValuesRange(inner) => DocIdSetIteratorEnum12::H(inner.take_iterator()),
            Self::SortedSetDocValuesRange(inner) => DocIdSetIteratorEnum12::I(inner.take_iterator()),
            Self::IndexSortSortedNumericDocValuesRange(inner) => DocIdSetIteratorEnum12::J(inner.take_iterator()),
            Self::ConstantScore(inner) => DocIdSetIteratorEnum12::K(inner.take_iterator()),
            Self::Cached(inner) => DocIdSetIteratorEnum12::L(inner.take_iterator()),
        }
    }

    fn two_phase_iterator(&self) -> Result<Option<Self::TwoPhaseIterRef<'_>>> {
        match self {
            Self::Term(inner) => Ok(inner.two_phase_iterator()?.map(TwoPhaseIteratorEnum12::A)),
            Self::MatchAll(inner) => Ok(inner.two_phase_iterator()?.map(TwoPhaseIteratorEnum12::B)),
            Self::MatchNoDocs(inner) => Ok(inner.two_phase_iterator()?.map(TwoPhaseIteratorEnum12::C)),
            Self::Dummy(inner) => Ok(inner.two_phase_iterator()?.map(TwoPhaseIteratorEnum12::D)),
            Self::FieldExists(inner) => Ok(inner.two_phase_iterator()?.map(TwoPhaseIteratorEnum12::E)),
            Self::PointRange(inner) => Ok(inner.two_phase_iterator()?.map(TwoPhaseIteratorEnum12::F)),
            Self::SortedNumericDocValuesSet(inner) => {
                Ok(inner.two_phase_iterator()?.map(TwoPhaseIteratorEnum12::G))
            },
            Self::SortedNumericDocValuesRange(inner) => {
                Ok(inner.two_phase_iterator()?.map(TwoPhaseIteratorEnum12::H))
            },
            Self::SortedSetDocValuesRange(inner) => {
                Ok(inner.two_phase_iterator()?.map(TwoPhaseIteratorEnum12::I))
            },
            Self::IndexSortSortedNumericDocValuesRange(inner) => {
                Ok(inner.two_phase_iterator()?.map(TwoPhaseIteratorEnum12::J))
            },
            Self::ConstantScore(inner) => Ok(inner.two_phase_iterator()?.map(TwoPhaseIteratorEnum12::K)),
            Self::Cached(inner) => Ok(inner.two_phase_iterator()?.map(TwoPhaseIteratorEnum12::L)),
        }
    }

    fn two_phase_iterator_mut(&mut self) -> Result<Option<Self::TwoPhaseIterMut<'_>>> {
        match self {
            Self::Term(inner) => Ok(inner.two_phase_iterator_mut()?.map(TwoPhaseIteratorEnum12::A)),
            Self::MatchAll(inner) => Ok(inner.two_phase_iterator_mut()?.map(TwoPhaseIteratorEnum12::B)),
            Self::MatchNoDocs(inner) => Ok(inner.two_phase_iterator_mut()?.map(TwoPhaseIteratorEnum12::C)),
            Self::Dummy(inner) => Ok(inner.two_phase_iterator_mut()?.map(TwoPhaseIteratorEnum12::D)),
            Self::FieldExists(inner) => Ok(inner.two_phase_iterator_mut()?.map(TwoPhaseIteratorEnum12::E)),
            Self::PointRange(inner) => Ok(inner.two_phase_iterator_mut()?.map(TwoPhaseIteratorEnum12::F)),
            Self::SortedNumericDocValuesSet(inner) => {
                Ok(inner.two_phase_iterator_mut()?.map(TwoPhaseIteratorEnum12::G))
            },
            Self::SortedNumericDocValuesRange(inner) => {
                Ok(inner.two_phase_iterator_mut()?.map(TwoPhaseIteratorEnum12::H))
            },
            Self::SortedSetDocValuesRange(inner) => {
                Ok(inner.two_phase_iterator_mut()?.map(TwoPhaseIteratorEnum12::I))
            },
            Self::IndexSortSortedNumericDocValuesRange(inner) => {
                Ok(inner.two_phase_iterator_mut()?.map(TwoPhaseIteratorEnum12::J))
            },
            Self::ConstantScore(inner) => Ok(inner.two_phase_iterator_mut()?.map(TwoPhaseIteratorEnum12::K)),
            Self::Cached(inner) => Ok(inner.two_phase_iterator_mut()?.map(TwoPhaseIteratorEnum12::L)),
        }
    }

    fn take_two_phase_iterator(self) -> Result<Option<Self::TwoPhaseIter>> {
        match self {
            Self::Term(inner) => Ok(inner.take_two_phase_iterator()?.map(TwoPhaseIteratorEnum12::A)),
            Self::MatchAll(inner) => Ok(inner.take_two_phase_iterator()?.map(TwoPhaseIteratorEnum12::B)),
            Self::MatchNoDocs(inner) => Ok(inner.take_two_phase_iterator()?.map(TwoPhaseIteratorEnum12::C)),
            Self::Dummy(inner) => Ok(inner.take_two_phase_iterator()?.map(TwoPhaseIteratorEnum12::D)),
            Self::FieldExists(inner) => Ok(inner.take_two_phase_iterator()?.map(TwoPhaseIteratorEnum12::E)),
            Self::PointRange(inner) => Ok(inner.take_two_phase_iterator()?.map(TwoPhaseIteratorEnum12::F)),
            Self::SortedNumericDocValuesSet(inner) => {
                Ok(inner.take_two_phase_iterator()?.map(TwoPhaseIteratorEnum12::G))
            },
            Self::SortedNumericDocValuesRange(inner) => {
                Ok(inner.take_two_phase_iterator()?.map(TwoPhaseIteratorEnum12::H))
            },
            Self::SortedSetDocValuesRange(inner) => {
                Ok(inner.take_two_phase_iterator()?.map(TwoPhaseIteratorEnum12::I))
            },
            Self::IndexSortSortedNumericDocValuesRange(inner) => {
                Ok(inner.take_two_phase_iterator()?.map(TwoPhaseIteratorEnum12::J))
            },
            Self::ConstantScore(inner) => Ok(inner.take_two_phase_iterator()?.map(TwoPhaseIteratorEnum12::K)),
            Self::Cached(inner) => Ok(inner.take_two_phase_iterator()?.map(TwoPhaseIteratorEnum12::L)),
        }
    }

    fn advance_shallow(&mut self, target: i32) -> Result<i32> {
        match self {
            Self::Term(inner) => inner.advance_shallow(target),
            Self::MatchAll(inner) => inner.advance_shallow(target),
            Self::MatchNoDocs(inner) => inner.advance_shallow(target),
            Self::Dummy(inner) => inner.advance_shallow(target),
            Self::FieldExists(inner) => inner.advance_shallow(target),
            Self::PointRange(inner) => inner.advance_shallow(target),
            Self::SortedNumericDocValuesSet(inner) => inner.advance_shallow(target),
            Self::SortedNumericDocValuesRange(inner) => inner.advance_shallow(target),
            Self::SortedSetDocValuesRange(inner) => inner.advance_shallow(target),
            Self::IndexSortSortedNumericDocValuesRange(inner) => inner.advance_shallow(target),
            Self::ConstantScore(inner) => inner.advance_shallow(target),
            Self::Cached(inner) => inner.advance_shallow(target),
        }
    }

    fn get_max_score(&mut self, up_to: i32) -> Result<f32> {
        match self {
            Self::Term(inner) => inner.get_max_score(up_to),
            Self::MatchAll(inner) => inner.get_max_score(up_to),
            Self::MatchNoDocs(inner) => inner.get_max_score(up_to),
            Self::Dummy(inner) => inner.get_max_score(up_to),
            Self::FieldExists(inner) => inner.get_max_score(up_to),
            Self::PointRange(inner) => inner.get_max_score(up_to),
            Self::SortedNumericDocValuesSet(inner) => inner.get_max_score(up_to),
            Self::SortedNumericDocValuesRange(inner) => inner.get_max_score(up_to),
            Self::SortedSetDocValuesRange(inner) => inner.get_max_score(up_to),
            Self::IndexSortSortedNumericDocValuesRange(inner) => inner.get_max_score(up_to),
            Self::ConstantScore(inner) => inner.get_max_score(up_to),
            Self::Cached(inner) => inner.get_max_score(up_to),
        }
    }

    fn has_two_phase_iterator(&self) -> TwoPhaseState {
        match self {
            Self::Term(inner) => inner.has_two_phase_iterator(),
            Self::MatchAll(inner) => inner.has_two_phase_iterator(),
            Self::MatchNoDocs(inner) => inner.has_two_phase_iterator(),
            Self::Dummy(inner) => inner.has_two_phase_iterator(),
            Self::FieldExists(inner) => inner.has_two_phase_iterator(),
            Self::PointRange(inner) => inner.has_two_phase_iterator(),
            Self::SortedNumericDocValuesSet(inner) => inner.has_two_phase_iterator(),
            Self::SortedNumericDocValuesRange(inner) => inner.has_two_phase_iterator(),
            Self::SortedSetDocValuesRange(inner) => inner.has_two_phase_iterator(),
            Self::IndexSortSortedNumericDocValuesRange(inner) => inner.has_two_phase_iterator(),
            Self::ConstantScore(inner) => inner.has_two_phase_iterator(),
            Self::Cached(inner) => inner.has_two_phase_iterator(),
        }
    }
}
