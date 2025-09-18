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
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::search::scorable::Scorable;
use crate::core::search::two_phase_iterator::TwoPhaseIterator;
use crate::core::util::error::lucene_error::Result;

/// Expert: Common scoring functionality for different types of queries.
///
/// A `Scorer` exposes an `iterator()` over documents matching a query in
/// increasing order of doc id.
pub trait Scorer: Scorable {
    /// Concrete iterator type over matching documents.
    type DocIdSetIterator: DocIdSetIterator;
    type DocIdSetIteratorRef<'a>: DocIdSetIterator
    where
        Self: 'a;

    /// Optional two-phase iterator type (return `None` if unsupported).
    type TwoPhaseIter: TwoPhaseIterator;

    /// Returns the doc ID that is currently being scored.
    fn doc_id(&mut self) -> Result<i32>;

    /// Return a [`DocIdSetIterator`] over matching documents.
    ///
    /// The returned iterator will either be positioned on `-1` if no documents
    /// have been scored yet, `NO_MORE_DOCS` if all documents have been scored already,
    /// or the last document id that has been scored otherwise.
    ///
    /// The returned iterator is a *view*: calling this method several times must
    /// return iterators that share the same state.
    fn iterator(&mut self) -> Self::DocIdSetIteratorRef<'_>;
    fn iterator_take(&mut self) -> Self::DocIdSetIterator;

    /// Optional: Return a two-phase iterator view of this scorer.
    ///
    /// A return value of `None` indicates that two-phase iteration is not supported.
    ///
    /// Note that the returned [`TwoPhaseIterator`]'s approximation must advance
    /// synchronously with `iterator()`: advancing the approximation must advance
    /// the iterator and vice-versa.
    ///
    /// The default implementation returns `None`.
    fn two_phase_iterator(&mut self) -> Option<&mut Self::TwoPhaseIter> {
        None
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

    /// Return the maximum score that documents between the last `target` that this
    /// iterator was `advance_shallow`’d to (included) and `up_to` (included) can get.
    fn get_max_score(&mut self, up_to: i32) -> Result<f32>;
}
