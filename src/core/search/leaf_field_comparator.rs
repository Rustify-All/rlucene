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
use crate::core::search::dummy::dummy_doc_id_set_iterator::DummyDocIdSetIterator;
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
    fn compare_bottom<S1, S2>(&mut self, doc: i32, scorer: &ScorerEnum<S1, S2>) -> Result<i32>
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
    ///
    /// # Arguments
    /// - `doc`: The docID that was hit.
    ///
    /// # Returns
    /// - `N < 0` if the doc's value is sorted after the top entry (not
    ///   competitive).
    /// - `N > 0` if the doc's value is sorted before the top entry.
    /// - `0` if they are equal.
    ///
    /// # Errors
    /// Returns an error if an I/O error occurs.
    fn compare_top(&mut self, doc: i32) -> Result<i32>;

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
    fn copy<S1, S2>(&mut self, slot: usize, doc: i32, scorer: &ScorerEnum<S1, S2>) -> Result<()>
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
    fn set_scorer<S1, S2>(&mut self, scorer: &ScorerEnum<S1, S2>) -> Result<()>
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
pub enum LeafFieldComparatorEnum {}
impl LeafFieldComparator for LeafFieldComparatorEnum {
    fn set_bottom(&mut self, _slot: usize) -> Result<()> {
        todo!()
    }

    fn compare_bottom<S1, S2>(&mut self, _doc: i32, _scorer: &ScorerEnum<S1, S2>) -> Result<i32>
    where
        S1: Scorer,
        S2: Scorable,
    {
        todo!()
    }

    fn compare_top(&mut self, _doc: i32) -> Result<i32> {
        todo!()
    }

    fn copy<S1, S2>(&mut self, _slot: usize, _doc: i32, _scorer: &ScorerEnum<S1, S2>) -> Result<()>
    where
        S1: Scorer,
        S2: Scorable,
    {
        todo!()
    }

    fn set_scorer<S1, S2>(&mut self, _scorer: &ScorerEnum<S1, S2>) -> Result<()>
    where
        S1: Scorer,
        S2: Scorable,
    {
        todo!()
    }

    type DocIdSetIterator = DummyDocIdSetIterator;

    fn competitive_iterator(&mut self) -> Option<Self::DocIdSetIterator> {
        todo!()
    }

    fn set_hits_threshold_reached(&mut self) -> Result<()> {
        todo!()
    }
}
