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
use crate::core::search::scorer::Scorer;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::Result;

/// This trait is used to score a range of documents at once, and is returned by [`Weight::bulk_scorer`](crate::core::search::weight::Weight::bulk_scorer).
///
/// Only queries that have a more optimized means of scoring across a range of
/// documents need to override this. Otherwise, a default implementation is
/// wrapped around the [`Scorer`] returned by [`Weight::scorer`](crate::core::search::weight::Weight::bulk_scorer).
pub trait BulkScorer {
    type CollectorScorer: Scorer;
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
