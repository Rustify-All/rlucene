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
use crate::core::search::doc_id_set::DocIdSet;
use crate::core::search::doc_id_set_iterator::{AllDocIdSetIterator, Either2DocIdSetIterator};
use crate::core::util::accountable::Accountable;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bit_set_iterator::BitSetIterator;
use crate::core::util::bits::MatchNoBits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use std::rc::Rc;
use std::sync::Arc;

/// Accumulator for documents that have a value for a field.
/// This is optimized for the case where all documents have a value.
pub struct DocsWithFieldSet {
    set: Option<FixedBitSet>,
    cardinality: i32,
    last_doc_id: i32,
    set_iter: Option<Arc<FixedBitSet>>,
    finish: bool,
}
impl Default for DocsWithFieldSet {
    fn default() -> Self {
        Self::new()
    }
}

impl DocsWithFieldSet {
    pub fn new() -> DocsWithFieldSet {
        DocsWithFieldSet {
            set: None,
            cardinality: 0,
            last_doc_id: -1,
            set_iter: None,
            finish: false,
        }
    }
    /// Adds a document to the set.
    ///
    /// # Parameters
    /// - `doc_id`: The document ID to be added.
    pub fn add(&mut self, doc_id: i32) -> Result<()> {
        if doc_id <= self.last_doc_id {
            return Err(LuceneError::illegal_argument(format!(
                "Out of order doc ids: last= {}, next= {}",
                self.last_doc_id, doc_id
            )));
        }
        if self.set.is_some() || self.set_iter.is_some() {
            if self.set_iter.is_some() {
                self.finish = false;
                let fixed_set = match Arc::try_unwrap(self.set_iter.take().unwrap()) {
                    Ok(value) => value,
                    Err(_) => return Err(LuceneError::illegal_state("Rc count should be 1")),
                };
                self.set = Some(fixed_set);
            }
            let set = self.set.as_mut().unwrap();
            set.ensure_capacity(doc_id);
            set.set(doc_id);
        } else if doc_id != self.cardinality {
            let mut set = FixedBitSet::new(doc_id + 1);
            set.set_with_range(0, self.cardinality);
            set.set(doc_id);
            self.set = Some(set);
        }
        self.last_doc_id = doc_id;
        self.cardinality += 1;
        Ok(())
    }
    /// Returns the number of documents in this set.
    pub fn cardinality(&self) -> i32 {
        self.cardinality
    }

    pub fn finish(&mut self) {
        self.finish = true;
        if self.set_iter.is_none() {
            self.set_iter = Some(Arc::new(self.set.take().unwrap()));
        }
    }
}

impl Accountable for DocsWithFieldSet {
    fn ram_bytes_used(&self) -> Result<i64> {
        Ok(0)
    }
}

pub(crate) type DocsWithFieldSetDISI =
    Either2DocIdSetIterator<AllDocIdSetIterator, BitSetIterator<FixedBitSet, Arc<FixedBitSet>>>;

impl DocIdSet for DocsWithFieldSet {
    type DocIdSetIterator = DocsWithFieldSetDISI;

    fn iterator(&self) -> Result<Option<Self::DocIdSetIterator>> {
        if self.set.is_some() || self.set_iter.is_some() {
            if !self.finish {
                return Err(LuceneError::illegal_state(
                    "DocsWithFieldSet must be call finish() before creating an iterator"
                        .to_string(),
                ));
            }
            debug_assert!(self.cardinality > 0);
            Ok(Some(Either2DocIdSetIterator::B(BitSetIterator::new(
                self.set_iter.as_ref().unwrap().clone(),
                self.cardinality as i64,
            )?)))
        } else {
            Ok(Some(Either2DocIdSetIterator::A(AllDocIdSetIterator::new(
                self.cardinality,
            ))))
        }
    }

    type BitType = MatchNoBits;

    fn bits(&self) -> Option<Rc<Self::BitType>> {
        None
    }
}

//TODO
const BASE_RAM_BYTES_USED: i64 = 0;

#[cfg(test)]
mod tests {
    use rand::Rng;

    use crate::core::index::docs_with_field_set::DocsWithFieldSet;
    use crate::core::search::doc_id_set::DocIdSet;
    use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
    use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
    use crate::core::util::error::lucene_error::Result;
    use crate::test::util::lucene_test_case::lucene_test_case_util::random;
    use crate::test::util::test_util::TestUtil;

    #[allow(dead_code)] // for quick search
    struct TestDocsWithFieldSet {}
    #[test]
    fn test_dense() -> Result<()> {
        let mut set = DocsWithFieldSet::new();
        let mut it = set.iterator()?.unwrap();
        assert_eq!(it.next_doc()?, NO_MORE_DOCS);

        let _ = set.add(0);
        it = set.iterator()?.unwrap();
        assert_eq!(0, it.next_doc()?);
        assert_eq!(it.next_doc()?, NO_MORE_DOCS);

        //TODO
        // let ram_bytes_used = set.ram_bytes_used();
        for i in 0..1000 {
            let _ = set.add(i);
        }
        //TODO:
        // assert_eq!(ram_bytes_used, set.ram_bytes_used());
        it = set.iterator()?.unwrap();
        for i in 0..1000 {
            assert_eq!(i, it.next_doc()?);
        }
        assert_eq!(NO_MORE_DOCS, it.next_doc()?);
        Ok(())
    }

    #[test]
    fn test_sparse() -> Result<()> {
        let mut random = random();
        let mut set = DocsWithFieldSet::new();
        let doc = random.random_range(0..10000);
        let _ = set.add(doc);
        set.finish();
        {
            let mut it = set.iterator()?.unwrap();
            assert_eq!(doc, it.next_doc()?);
            assert_eq!(it.next_doc()?, NO_MORE_DOCS);
        }
        let doc2 = doc + TestUtil::next_int(&mut random, 1, 100);
        let _ = set.add(doc2);
        set.finish();
        let mut it = set.iterator()?.unwrap();
        assert_eq!(doc, it.next_doc()?);
        assert_eq!(doc2, it.next_doc()?);
        assert_eq!(it.next_doc()?, NO_MORE_DOCS);
        Ok(())
    }

    #[test]
    fn test_dense_then_sparse() -> Result<()> {
        let mut random = random();
        let dense_count = random.random_range(1..10000);
        let next_doc = dense_count + random.random_range(1..10000);
        let mut set = DocsWithFieldSet::new();
        for i in 0..dense_count {
            let _ = set.add(i);
        }
        let _ = set.add(next_doc);
        set.finish();
        let mut it = set.iterator()?.unwrap();
        for i in 0..dense_count {
            assert_eq!(i, it.next_doc()?);
        }
        assert_eq!(next_doc, it.next_doc()?);
        assert_eq!(NO_MORE_DOCS, it.next_doc()?);
        Ok(())
    }
}
