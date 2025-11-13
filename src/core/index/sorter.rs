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
use crate::core::index::index_sorter::DocComparator;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::sort::Sort;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::long_values::LongValues;
use crate::core::util::packed::PackedInts;
use crate::core::util::packed::packed_long_values::PackedLongValues;
use crate::core::util::sorter::Sorter as ASorter;
use crate::core::util::{SliceCopyOps, TimSorter, TimSorterBase, ToInt};
use std::fmt::{Display, Formatter};

/// Sorts documents of a given index by returning a permutation on the document IDs.
pub struct Sorter {
    sort: Sort,
}
impl Sorter {
    pub(crate) fn new(sort: Sort) -> Result<Self> {
        if sort.needs_scores() {
            return Err(LuceneError::illegal_argument(
                "Cannot sort an index with a Sort that refers to the relevance score",
            ));
        }
        Ok(Self { sort })
    }

    /// Check consistency of a [`DocMap`], useful for assertions.
    pub(crate) fn is_consistent<DM>(doc_map: &DM) -> bool
    where
        DM: DocMap,
    {
        let max_doc = doc_map.size();
        for i in 0..max_doc {
            let new_id = doc_map.old_to_new(i);
            let old_id = doc_map.new_to_old(new_id);
            debug_assert!(
                (0..max_doc).contains(&new_id),
                "doc IDs must be in [0-{max_doc}), got {new_id}"
            );
            debug_assert_eq!(
                { i },
                old_id,
                "mapping is inconsistent: {i} --oldToNew--> {new_id} --newToOld--> {old_id}"
            );
            if old_id != i || new_id < 0 || new_id >= max_doc {
                return false;
            }
        }
        true
    }

    /// Returns the identifier of this [`Sorter`].
    pub fn get_id(&self) -> String {
        self.sort.to_string()
    }
    /// Computes the old-to-new permutation over the given comparator.
    fn sort_impl<DC>(max_doc: i32, comparator: DC) -> Result<Option<DocMapImpl>>
    where
        DC: DocComparator,
    {
        // check if the index is sorted
        let mut sorted = true;
        for i in 1..max_doc {
            if comparator.compare(i - 1, i) > 0 {
                sorted = false;
                break;
            }
        }
        if sorted {
            return Ok(None);
        }

        // sort doc IDs
        let mut docs: Vec<i32> = (0..max_doc).collect();
        let mut sorter = DocValueSorter::new(&mut docs, comparator);
        // It can be common to sort a reader, add docs, sort it again, ... and in
        // that case timSort can save a lot of time
        sorter.sort(0, max_doc)?; // docs is now the newToOld mapping

        // The reason why we use MonotonicAppendingLongBuffer here is that it
        // wastes very little memory if the index is in random order but can save
        // a lot of memory if the index is already "almost" sorted
        let mut new_to_old_builder =
            PackedLongValues::monotonic_long_values_builder_default(PackedInts::COMPACT)?;
        for &doc in &docs {
            new_to_old_builder.add(doc as i64)?;
        }
        let new_to_old = new_to_old_builder.build()?;

        // invert the docs mapping:
        for i in 0..max_doc {
            let old = new_to_old.get(i as i64)?;
            docs[old as usize] = i;
        } // docs is now the oldToNew mapping

        let mut old_to_new_builder =
            PackedLongValues::monotonic_long_values_builder_default(PackedInts::COMPACT)?;
        for i in 0..max_doc {
            old_to_new_builder.add(docs[i as usize] as i64)?;
        }
        let old_to_new = old_to_new_builder.build()?;

        Ok(Some(DocMapImpl::new(new_to_old, old_to_new, max_doc)))
    }
    /// Returns a mapping from the old document ID to its new location in the sorted index.
    ///
    /// Implementations can use [`sort(max_doc, comparator)`] to compute the old-to-new permutation
    /// given a list of documents and their corresponding values.
    ///
    /// A return value of `None` indicates that the reader is already sorted.
    ///
    /// **Note:** Deleted documents are expected to appear in the mapping as well; they will
    /// still be marked as deleted in the sorted view.
    pub(crate) fn sort_with_reader<LR>(&self, _reader: &mut LR) -> Result<Option<DocMapImpl>>
    where
        LR: LeafReader,
    {
        todo!()
    }

    pub(crate) fn sort<DC>(max_doc: i32, comparators: Vec<DC>) -> Result<Option<DocMapImpl>>
    where
        DC: DocComparator,
    {
        let composite = DocComparatorImpl::new(comparators);
        Self::sort_impl(max_doc, composite)
    }
}
impl Display for Sorter {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.get_id())
    }
}

pub trait DocMap {
    /// Given a doc ID from the original index, return its ordinal in the sorted
    /// index.
    fn old_to_new(&self, doc_id: i32) -> i32;

    /// Given the ordinal of a doc ID, return its doc ID in the original index.
    fn new_to_old(&self, doc_id: i32) -> i32;

    /// Return the number of documents in this map.
    /// This must equal the number of documents in the sorted `LeafReader`.
    fn size(&self) -> i32;
}

struct DocValueSorter<'a, DC>
where
    DC: DocComparator,
{
    docs: &'a mut [i32],
    comparator: DC,
    tmp: Vec<i32>,
    pivot_index: i32,
}
impl<'a, DC> DocValueSorter<'a, DC>
where
    DC: DocComparator,
{
    pub fn new(docs: &'a mut [i32], comparator: DC) -> TimSorter<DocValueSorter<'a, DC>> {
        let max_temp_slots = docs.len() / 64;
        let tmp = vec![0i32; max_temp_slots];
        let sub = DocValueSorter {
            docs,
            comparator,
            tmp,
            pivot_index: 0,
        };
        TimSorter::new(max_temp_slots as i32, sub)
    }
}
impl<'a, DC> TimSorterBase for DocValueSorter<'a, DC>
where
    DC: DocComparator,
{
    fn copy(&mut self, src: i32, dest: i32) {
        self.docs[dest as usize] = self.docs[src as usize];
    }

    fn save(&mut self, i: i32, len: i32) {
        self.tmp
            .copy_from(&self.docs[i as usize..(i + len) as usize], 0);
    }

    fn restore(&mut self, i: i32, j: i32) {
        self.docs[j as usize] = self.tmp[i as usize];
    }

    fn compare_saved(&self, i: i32, j: i32) -> i32 {
        self.comparator
            .compare(self.tmp[i as usize], self.docs[j as usize])
    }
}
impl<'a, DC> crate::core::util::Sorter for DocValueSorter<'a, DC>
where
    DC: DocComparator,
{
    fn compare(&mut self, i: i32, j: i32) -> Result<i32> {
        Ok(self
            .comparator
            .compare(self.docs[i as usize], self.docs[j as usize]))
    }

    fn swap(&mut self, i: i32, j: i32) -> Result<()> {
        self.docs.swap(i as usize, j as usize);
        Ok(())
    }

    fn set_pivot(&mut self, i: i32) -> Result<()> {
        self.pivot_index = i;
        Ok(())
    }

    fn compare_pivot(&mut self, j: i32) -> Result<i32> {
        self.compare(self.pivot_index, j)
    }
}
pub struct DocMapImpl {
    old_to_new: PackedLongValues,
    new_to_old: PackedLongValues,
    max_doc: i32,
}
impl DocMapImpl {
    pub fn new(old_to_new: PackedLongValues, new_to_old: PackedLongValues, max_doc: i32) -> Self {
        DocMapImpl {
            old_to_new,
            new_to_old,
            max_doc,
        }
    }
}
impl DocMap for DocMapImpl {
    fn old_to_new(&self, doc_id: i32) -> i32 {
        self.old_to_new.get(doc_id as i64).expect("should not fail") as i32
    }

    fn new_to_old(&self, doc_id: i32) -> i32 {
        self.new_to_old.get(doc_id as i64).expect("should not fail") as i32
    }

    fn size(&self) -> i32 {
        self.max_doc
    }
}

struct DocComparatorImpl<DC>
where
    DC: DocComparator,
{
    comparators: Vec<DC>,
}
impl<DC> DocComparatorImpl<DC>
where
    DC: DocComparator,
{
    pub fn new(comparators: Vec<DC>) -> Self {
        DocComparatorImpl { comparators }
    }
}
impl<DC> DocComparator for DocComparatorImpl<DC>
where
    DC: DocComparator,
{
    fn compare(&self, doc_id1: i32, doc_id2: i32) -> i32 {
        for cmp in self.comparators.iter() {
            let comp = cmp.compare(doc_id1, doc_id2);
            if comp != 0 {
                return comp;
            }
        }
        // docid order tiebreak
        doc_id1.cmp(&doc_id2).to_int()
    }
}
