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
use crate::core::search::doc_id_set_iterator::{AllDocIdSetIterator, DocIdSetIterator, EmptyDISI};
use crate::core::util::accountable::Accountable;
use crate::core::util::bits::{Bits, MatchAllBits, MatchNoBits};
use crate::core::util::error::lucene_error::Result;
use std::rc::Rc;

/// A `DocIdSet` contains a set of document IDs.
/// Implementing types must provide an [`iterator`](DocIdSet::iterator) method
/// to access the set.
pub trait DocIdSet: Accountable {
    type DocIdSetIterator: DocIdSetIterator;
    fn iterator(&self) -> Result<Option<Self::DocIdSetIterator>>;

    // TODO: somehow this struct should express the cost of
    // iteration vs the cost of random access Bits; for
    // expensive Filters (e.g. distance < 1 km) we should use
    // bits() after all other Query/Filters have matched, but
    // this is the opposite of what bits() is for now
    // (down-low filtering using e.g. FixedBitSet)

    /// Optionally provides a [`Bits`] interface for random access to matching
    /// documents.
    ///
    /// # Returns
    /// * `None` if this `DocIdSet` does not support random access.
    ///
    /// Note that, unlike [`iterator`](DocIdSet::iterator), a return value of
    /// `None` **does not** imply that no documents match the filter!
    ///
    /// The default implementation does not provide random access, so you only
    /// need to implement this method if your [`DocIdSet`] can guarantee
    /// random access to every document ID in `O(1)` time without external
    /// disk access (as the [`Bits`] interface cannot throw an `IOError`).
    /// This is generally true for bit sets like
    /// [`FixedBitSet`](crate::core::util::fixed_bit_set::FixedBitSet),
    /// which return themselves if used as a [`DocIdSet`].
    type BitType: Bits;
    fn bits(&self) -> Option<Rc<Self::BitType>>;

    /// Some implementations require calling the finish method before invoking iterator.
    /// # See
    /// [`DocsWithFieldSet`](crate::core::index::docs_with_field_set::DocsWithFieldSet)
    fn finish(&mut self) {}
}

/// A [`DocIdSet`] that matches all document IDs up to a specified document
/// (exclusive).
struct All {
    max_doc: i32,
    bits: Option<Rc<MatchAllBits>>,
}
impl All {
    fn new(max_doc: i32) -> Self {
        let bits = Some(Rc::new(MatchAllBits::new(max_doc)));
        All { max_doc, bits }
    }
}
/// A `DocIdSet` that matches all doc ids up to a specified doc (exclusive).
impl DocIdSet for All {
    type DocIdSetIterator = AllDocIdSetIterator;

    fn iterator(&self) -> Result<Option<Self::DocIdSetIterator>> {
        Ok(Some(AllDocIdSetIterator::new(self.max_doc)))
    }

    type BitType = MatchAllBits;

    fn bits(&self) -> Option<Rc<Self::BitType>> {
        self.bits.clone()
    }
}

impl Accountable for All {
    fn ram_bytes_used(&self) -> Result<i64> {
        Ok(0)
    }
}

pub struct EmptyDocIdSet;
impl Accountable for EmptyDocIdSet {
    fn ram_bytes_used(&self) -> Result<i64> {
        Ok(0)
    }
}
impl DocIdSet for EmptyDocIdSet {
    type DocIdSetIterator = EmptyDISI;

    fn iterator(&self) -> Result<Option<Self::DocIdSetIterator>> {
        Ok(None)
    }

    type BitType = MatchNoBits;

    fn bits(&self) -> Option<Rc<Self::BitType>> {
        None
    }
}
