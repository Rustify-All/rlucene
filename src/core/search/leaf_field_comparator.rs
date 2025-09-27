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
use crate::core::index::doc_values::Numeric;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::search::comparators::doc_comparator::{DocComparatorIterator, DocLeafComparator};
use crate::core::search::comparators::double_comparator::DoubleLeafComparator;
use crate::core::search::comparators::float_comparator::FloatLeafComparator;
use crate::core::search::comparators::int_comparator::IntLeafComparator;
use crate::core::search::comparators::long_comparator::LongLeafComparator;
use crate::core::search::comparators::numeric_comparator::{
    CompetitiveIterator, CompetitiveIteratorType,
};
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, Either3DocIdSetIterator};
use crate::core::search::dummy::dummy_doc_id_set_iterator::DummyDocIdSetIterator;
use crate::core::search::field_comparator::TermValLeafComparator;
use crate::core::search::scorable::{Scorable, ScorerEnum};
use crate::core::search::scorer::Scorer;
use crate::core::util::error::lucene_error::Result;

/// Expert: comparator that gets instantiated on each leaf from a top-level
/// [`FieldComparator`](crate::core::search::field_comparator::FieldComparator)
/// instance.
///
/// A leaf comparator must define these functions:
///
/// - [`set_bottom`](LeafFieldComparator::set_bottom) This method is called by
///   [`FieldValueHitQueue`](crate::core::search::field_value_hit_queue::FieldValueHitQueue)
///   to notify the `FieldComparator` of the current weakest ("bottom") slot.
///   Note that this slot may not hold the weakest value according to your
///   comparator, in cases where your comparator is not the primary one (i.e.,
///   is only used to break ties from the comparators before it).
/// - [`compare_bottom`](LeafFieldComparator::compare_bottom) Compare a new hit
///   (docID) against the "weakest" (bottom) entry in the queue.
/// - [`compare_top`](LeafFieldComparator::compare_top) Compares a new hit
///   (docID) against the top value previously set by a call to
///   [`FieldComparator::set_top_value`](crate::core::search::field_comparator::FieldComparator::set_top_value).
/// - [`copy`](LeafFieldComparator::copy) Installs a new hit into the priority
///   queue. The
///   [`FieldValueHitQueue`](crate::core::search::field_value_hit_queue::FieldValueHitQueue)
///   calls this method when a new hit is competitive.
///
/// # See Also
/// - [`FieldComparator`](crate::core::search::field_comparator::FieldComparator)
///
/// # Lucene Experimental
/// This API is experimental and may change in future versions.
pub trait LeafFieldComparator {
    /// Set the bottom slot, i.e., the "weakest" (sorted last) entry in the
    /// queue. When `compare_bottom` is called, you should compare against
    /// this slot.
    ///
    /// This will always be called before `compare_bottom`.
    ///
    /// # Arguments
    /// - `slot`: The currently weakest (sorted last) slot in the queue.
    ///
    /// # Errors
    /// Returns an error if an I/O error occurs.
    fn set_bottom(&mut self, slot: usize) -> Result<()>;

    /// Compare the bottom of the queue with this document.
    ///
    /// This will only be invoked after `set_bottom` has been called. This
    /// should return the same result as if `bottom` were slot1 and the new
    /// document were slot2.
    ///
    /// For a search that hits many results, this method will be the hotspot
    /// (invoked the most frequently).
    ///
    /// # Arguments
    /// - `doc`: The docID that was hit.
    /// - `scorer`: The scorer instance currently used to evaluate the hit.
    ///
    /// # Returns
    /// - `N < 0` if the doc's value is sorted after the bottom entry (not
    ///   competitive).
    /// - `N > 0` if the doc's value is sorted before the bottom entry.
    /// - `0` if they are equal.
    ///
    /// # Errors
    /// Returns an error if an I/O error occurs.
    fn compare_bottom<S1, S2>(&mut self, doc: i32, scorer: &mut ScorerEnum<S1, S2>) -> Result<i32>
    where
        S1: Scorer,
        S2: Scorable;

    /// Compare the top value with this document.
    ///
    /// This will only be invoked after `set_top_value` has been called. This
    /// should return the same result as if `top_value` were slot1 and the
    /// new document were slot2.
    ///
    /// This is only called for searches that use searchAfter (deep paging).
    /// # Arguments
    /// - `doc`: The docID that was hit.
    /// - `scorer`: The scorer instance currently used to evaluate the hit.
    ///
    /// # Returns
    /// - `N < 0` if the doc's value is sorted after the top entry (not
    ///   competitive).
    /// - `N > 0` if the doc's value is sorted before the top entry.
    /// - `0` if they are equal.
    ///
    /// # Errors
    /// Returns an error if an I/O error occurs.
    fn compare_top<S1, S2>(&mut self, doc: i32, scorer: &mut ScorerEnum<S1, S2>) -> Result<i32>
    where
        S1: Scorer,
        S2: Scorable;

    /// Called when a new hit is competitive.
    ///
    /// You should copy any state associated with this document that will be
    /// required for future comparisons into the specified slot.
    ///
    /// # Arguments
    /// - `slot`: The slot to copy the hit to.
    /// - `doc`: The docID relative to the current reader.
    /// - `scorer`: The scorer instance currently used to evaluate the hit.
    ///
    /// # Errors
    /// Returns an error if an I/O error occurs.
    fn copy<S1, S2>(
        &mut self,
        slot: usize,
        doc: i32,
        scorer: &mut ScorerEnum<S1, S2>,
    ) -> Result<()>
    where
        S1: Scorer,
        S2: Scorable;

    /// Sets the scorer to use in case a document's score is needed.
    ///
    /// # Arguments
    /// - `scorer`: Scorer instance to get the current hit's score, if
    ///   necessary.
    ///
    /// # Errors
    /// Returns an error if an I/O error occurs.
    fn set_scorer<S1, S2>(&mut self, scorer: &mut ScorerEnum<S1, S2>) -> Result<()>
    where
        S1: Scorer,
        S2: Scorable;

    type DocIdSetIterator: DocIdSetIterator;
    /// Returns a competitive iterator over documents stronger than already
    /// collected docs, or `None` if such an iterator is not available for
    /// the current comparator or segment.
    ///
    /// # Returns
    /// An iterator over competitive docs.
    fn competitive_iterator(&mut self) -> Option<Self::DocIdSetIterator> {
        None
    }

    /// Informs this leaf comparator that the hit's threshold is reached.
    ///
    /// This method is called from a collector when the hit's threshold is
    /// reached.
    fn set_hits_threshold_reached(&mut self) -> Result<()> {
        Ok(())
    }
}

type NumericCompetitiveIterator<LR> = CompetitiveIterator<CompetitiveIteratorType<Numeric<LR>>>;

pub type LeafFieldComparatorDocIdSetIterator<LR> = Either3DocIdSetIterator<
    DocComparatorIterator,
    NumericCompetitiveIterator<LR>,
    DummyDocIdSetIterator,
>;

pub enum LeafFieldComparatorEnum<LR>
where
    LR: LeafReader,
{
    Doc(DocLeafComparator),
    Double(DoubleLeafComparator<LR>),
    Float(FloatLeafComparator<LR>),
    Int(IntLeafComparator<LR>),
    Long(LongLeafComparator<LR>),
    TermVal(TermValLeafComparator<LR>),
}

impl<LR> LeafFieldComparator for LeafFieldComparatorEnum<LR>
where
    LR: LeafReader,
{
    fn set_bottom(&mut self, slot: usize) -> Result<()> {
        match self {
            Self::Doc(comparator) => comparator.set_bottom(slot),
            Self::Double(comparator) => comparator.set_bottom(slot),
            Self::Float(comparator) => comparator.set_bottom(slot),
            Self::Int(comparator) => comparator.set_bottom(slot),
            Self::Long(comparator) => comparator.set_bottom(slot),
            Self::TermVal(comparator) => comparator.set_bottom(slot),
        }
    }

    fn compare_bottom<S1, S2>(&mut self, doc: i32, scorer: &mut ScorerEnum<S1, S2>) -> Result<i32>
    where
        S1: Scorer,
        S2: Scorable,
    {
        match self {
            Self::Doc(comparator) => comparator.compare_bottom(doc, scorer),
            Self::Double(comparator) => comparator.compare_bottom(doc, scorer),
            Self::Float(comparator) => comparator.compare_bottom(doc, scorer),
            Self::Int(comparator) => comparator.compare_bottom(doc, scorer),
            Self::Long(comparator) => comparator.compare_bottom(doc, scorer),
            Self::TermVal(comparator) => comparator.compare_bottom(doc, scorer),
        }
    }

    fn compare_top<S1, S2>(&mut self, doc: i32, scorer: &mut ScorerEnum<S1, S2>) -> Result<i32>
    where
        S1: Scorer,
        S2: Scorable,
    {
        match self {
            Self::Doc(comparator) => comparator.compare_top(doc, scorer),
            Self::Double(comparator) => comparator.compare_top(doc, scorer),
            Self::Float(comparator) => comparator.compare_top(doc, scorer),
            Self::Int(comparator) => comparator.compare_top(doc, scorer),
            Self::Long(comparator) => comparator.compare_top(doc, scorer),
            Self::TermVal(comparator) => comparator.compare_top(doc, scorer),
        }
    }

    fn copy<S1, S2>(&mut self, slot: usize, doc: i32, scorer: &mut ScorerEnum<S1, S2>) -> Result<()>
    where
        S1: Scorer,
        S2: Scorable,
    {
        match self {
            Self::Doc(comparator) => comparator.copy(slot, doc, scorer),
            Self::Double(comparator) => comparator.copy(slot, doc, scorer),
            Self::Float(comparator) => comparator.copy(slot, doc, scorer),
            Self::Int(comparator) => comparator.copy(slot, doc, scorer),
            Self::Long(comparator) => comparator.copy(slot, doc, scorer),
            Self::TermVal(comparator) => comparator.copy(slot, doc, scorer),
        }
    }

    fn set_scorer<S1, S2>(&mut self, scorer: &mut ScorerEnum<S1, S2>) -> Result<()>
    where
        S1: Scorer,
        S2: Scorable,
    {
        match self {
            Self::Doc(comparator) => comparator.set_scorer(scorer),
            Self::Double(comparator) => comparator.set_scorer(scorer),
            Self::Float(comparator) => comparator.set_scorer(scorer),
            Self::Int(comparator) => comparator.set_scorer(scorer),
            Self::Long(comparator) => comparator.set_scorer(scorer),
            Self::TermVal(comparator) => comparator.set_scorer(scorer),
        }
    }

    type DocIdSetIterator = LeafFieldComparatorDocIdSetIterator<LR>;

    fn competitive_iterator(&mut self) -> Option<Self::DocIdSetIterator> {
        match self {
            Self::Doc(comparator) => comparator
                .competitive_iterator()
                .map(LeafFieldComparatorDocIdSetIterator::<LR>::A),
            Self::Double(comparator) => comparator
                .competitive_iterator()
                .map(LeafFieldComparatorDocIdSetIterator::<LR>::B),
            Self::Float(comparator) => comparator
                .competitive_iterator()
                .map(LeafFieldComparatorDocIdSetIterator::<LR>::B),
            Self::Int(comparator) => comparator
                .competitive_iterator()
                .map(LeafFieldComparatorDocIdSetIterator::<LR>::B),
            Self::Long(comparator) => comparator
                .competitive_iterator()
                .map(LeafFieldComparatorDocIdSetIterator::<LR>::B),
            Self::TermVal(comparator) => comparator
                .competitive_iterator()
                .map(LeafFieldComparatorDocIdSetIterator::<LR>::C),
        }
    }

    fn set_hits_threshold_reached(&mut self) -> Result<()> {
        match self {
            Self::Doc(comparator) => comparator.set_hits_threshold_reached(),
            Self::Double(comparator) => comparator.set_hits_threshold_reached(),
            Self::Float(comparator) => comparator.set_hits_threshold_reached(),
            Self::Int(comparator) => comparator.set_hits_threshold_reached(),
            Self::Long(comparator) => comparator.set_hits_threshold_reached(),
            Self::TermVal(comparator) => comparator.set_hits_threshold_reached(),
        }
    }
}
