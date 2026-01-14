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

use crate::core::index::merge_state::DocMap;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::priority_queue::{Compare, PriorityQueue};

/// Reuse API, currently only used by postings during merge
pub trait DocIDMerger<S>
where
    S: SubBase,
{
    /// Reuse API, currently only used by postings during merge
    fn reset(&mut self) -> Result<()>;

    /// Returns `None` when done.
    /// # NOTE:
    /// after the iterator has exhausted you should not call this method,
    /// as it may result in unpredicted behavior.
    fn next(&mut self) -> Result<Option<usize>>;
}

pub(crate) struct SequentialDocIDMerger<S>
where
    S: SubBase,
{
    subs: Vec<Sub<S>>,
    current: Option<usize>,
    next_index: usize,
}
impl<S> SequentialDocIDMerger<S>
where
    S: SubBase,
{
    pub fn new(subs: Vec<Sub<S>>) -> Result<Self> {
        let mut doc_id_merger = Self {
            subs,
            current: None,
            next_index: 0,
        };
        doc_id_merger.reset()?;
        Ok(doc_id_merger)
    }
}

impl<S> DocIDMerger<S> for SequentialDocIDMerger<S>
where
    S: SubBase,
{
    fn reset(&mut self) -> Result<()> {
        if !self.subs.is_empty() {
            self.current = Some(0);
            self.next_index = 1;
        } else {
            self.current = None;
            self.next_index = 0;
        }
        Ok(())
    }

    fn next(&mut self) -> Result<Option<usize>> {
        loop {
            match self.current {
                Some(current) => {
                    let next_mapped_doc = {
                        let current = &mut self.subs[current];
                        current.next_mapped_doc()?
                    };
                    if next_mapped_doc != NO_MORE_DOCS {
                        return Ok(Some(current));
                    }
                    if self.next_index == self.subs.len() {
                        self.current = None;
                        return Ok(None);
                    }

                    self.current = Some(self.next_index);
                    self.next_index += 1;
                },
                None => return Err(LuceneError::illegal_state("current is None")),
            }
        }
    }
}

pub(crate) struct SortedDocIDMerger<S>
where
    S: SubBase,
{
    current: Option<usize>,
    queue: PriorityQueue<usize, SubCompare<S>>,
    queue_min_doc_id: i32,
}
impl<S> SortedDocIDMerger<S>
where
    S: SubBase,
{
    fn new(subs: Vec<Sub<S>>, max_count: usize) -> Result<Self> {
        if max_count <= 1 {
            return Err(LuceneError::illegal_argument(""));
        }
        let sub_compare = SubCompare::new(subs);
        let queue = PriorityQueue::new(max_count, sub_compare)?;
        let mut merger = Self {
            current: None,
            queue,
            queue_min_doc_id: 0,
        };
        merger.reset()?;
        Ok(merger)
    }
    fn set_queue_min_doc_id(&mut self) {
        if self.queue.size() > 0 {
            let idx = self
                .queue
                .top()
                .expect("priority queue top element should exist");
            let v = self.queue.compare.subs[*idx].mapped_doc_id;
            self.queue_min_doc_id = v;
        } else {
            self.queue_min_doc_id = NO_MORE_DOCS;
        }
    }
}
impl<S> DocIDMerger<S> for SortedDocIDMerger<S>
where
    S: SubBase,
{
    fn reset(&mut self) -> Result<()> {
        // caller may not have fully consumed the queue:
        self.queue.clear();
        self.current = None;
        let mut first = true;
        let mut to_add = Vec::new();
        for (i, sub) in self.queue.compare.subs.iter_mut().enumerate() {
            if first {
                // by setting mappedDocID = -1, this entry is guaranteed to be
                // the top of the queue so the first call to
                // next() will advance it
                sub.mapped_doc_id = -1;
                self.current = Some(i);
                first = false;
            } else {
                let next_mapped_doc = sub.next_mapped_doc()?;
                if next_mapped_doc != NO_MORE_DOCS {
                    to_add.push(i);
                } // else all docs in this sub were deleted; do not add it to the
                // queue!
            }
        }
        for i in to_add {
            self.queue.add(i)?;
        }

        self.set_queue_min_doc_id();
        Ok(())
    }

    fn next(&mut self) -> Result<Option<usize>> {
        let (next_doc, current) = match self.current {
            Some(current) => (self.queue.compare.subs[current].next_mapped_doc()?, current),
            None => {
                return Err(LuceneError::illegal_state("current is None"))?;
            },
        };

        if next_doc < self.queue_min_doc_id {
            // This should be the common case when index sorting is either
            // disabled, or enabled on a low-cardinality field, or
            // enabled on a field that correlates with index order.
            return Ok(Some(current));
        }

        if next_doc == NO_MORE_DOCS {
            if self.queue.size() == 0 {
                self.current = None;
            } else {
                self.current = self.queue.pop()?;
            }
        } else if self.queue.size() > 0 {
            debug_assert!({
                let top_idx = **self.queue.top().as_ref().unwrap();
                let top = self.queue.compare.subs[top_idx].mapped_doc_id;
                self.queue_min_doc_id == top
            });
            debug_assert!(next_doc > self.queue_min_doc_id);
            let new_current_idx = *self
                .queue
                .top()
                .expect("priority queue top element should exist");
            self.queue
                .update_top_with_new_top(self.current.take().unwrap())?;
            self.current = Some(new_current_idx);
        }

        self.set_queue_min_doc_id();
        match self.current {
            Some(current) => Ok(Some(current)),
            None => Ok(None),
        }
    }
}
pub(crate) enum DocIDMergerEnum<S>
where
    S: SubBase,
{
    Sequential(SequentialDocIDMerger<S>),
    Sorted(SortedDocIDMerger<S>),
}
impl<S> DocIDMergerEnum<S>
where
    S: SubBase,
{
    pub(crate) fn get_subs_mut(&mut self) -> &mut [Sub<S>] {
        match self {
            DocIDMergerEnum::Sequential(merger) => &mut merger.subs,
            DocIDMergerEnum::Sorted(merger) => &mut merger.queue.compare.subs,
        }
    }
    pub(crate) fn get_subs_vec(&mut self) -> &mut Vec<Sub<S>> {
        match self {
            DocIDMergerEnum::Sequential(merger) => &mut merger.subs,
            DocIDMergerEnum::Sorted(merger) => &mut merger.queue.compare.subs,
        }
    }
    pub(crate) fn clear_subs(&mut self) {
        match self {
            DocIDMergerEnum::Sequential(merger) => merger.subs.clear(),
            DocIDMergerEnum::Sorted(merger) => merger.queue.compare.subs.clear(),
        }
    }
    pub(crate) fn take_subs(self) -> Vec<Sub<S>> {
        match self {
            DocIDMergerEnum::Sequential(merger) => merger.subs,
            DocIDMergerEnum::Sorted(merger) => merger.queue.compare.subs,
        }
    }

    pub(crate) fn get_subs(&self) -> &[Sub<S>] {
        match self {
            DocIDMergerEnum::Sequential(merger) => &merger.subs,
            DocIDMergerEnum::Sorted(merger) => &merger.queue.compare.subs,
        }
    }
}

impl<S> DocIDMerger<S> for DocIDMergerEnum<S>
where
    S: SubBase,
{
    fn reset(&mut self) -> Result<()> {
        match self {
            DocIDMergerEnum::Sequential(merger) => merger.reset(),
            DocIDMergerEnum::Sorted(merger) => merger.reset(),
        }
    }

    fn next(&mut self) -> Result<Option<usize>> {
        match self {
            DocIDMergerEnum::Sequential(merger) => merger.next(),
            DocIDMergerEnum::Sorted(merger) => merger.next(),
        }
    }
}

/// Represents one sub-reader being merged
pub struct Sub<S>
where
    S: SubBase,
{
    /// Mapped doc ID
    pub(crate) sub: S,
    pub mapped_doc_id: i32,
}
impl<S> Sub<S>
where
    S: SubBase,
{
    pub fn new(sub: S) -> Self {
        Self {
            sub,
            mapped_doc_id: 0,
        }
    }
    /// Like `next_doc()` but skips over unmapped docs and returns the next
    /// mapped doc ID, or `DocIdSetIterator::NO_MORE_DOCS` when exhausted.
    /// This method sets `mapped_doc_id` as a side effect.
    fn next_mapped_doc(&mut self) -> Result<i32> {
        loop {
            let doc = self.sub.next_doc()?;
            if doc == NO_MORE_DOCS {
                self.mapped_doc_id = NO_MORE_DOCS;
                return Ok(NO_MORE_DOCS);
            }
            let mapped_doc = self.sub.get_doc_map()?.get(doc)?;
            if mapped_doc != -1 {
                self.mapped_doc_id = mapped_doc;
                return Ok(mapped_doc);
            }
        }
    }
}
pub trait SubBase {
    /// Returns the next document ID from this sub reader,
    /// and `DocIdSetIterator::NO_MORE_DOCS` when done
    fn next_doc(&mut self) -> Result<i32>;
    type DocMap: DocMap;
    fn get_doc_map(&self) -> Result<&Self::DocMap>;
}

struct SubCompare<S>
where
    S: SubBase,
{
    subs: Vec<Sub<S>>,
}
impl<S> SubCompare<S>
where
    S: SubBase,
{
    fn new(subs: Vec<Sub<S>>) -> Self {
        Self { subs }
    }
}
impl<S> Compare<usize> for SubCompare<S>
where
    S: SubBase,
{
    fn less_than(&self, a: &usize, b: &usize) -> Result<bool> {
        debug_assert!(self.subs[*a].mapped_doc_id != self.subs[*b].mapped_doc_id);
        Ok(self.subs[*a].mapped_doc_id < self.subs[*b].mapped_doc_id)
    }
}

/// Construct this from the provided subs, specifying the maximum sub count.
pub(crate) fn of_with_max_count<S: SubBase>(
    subs: Vec<Sub<S>>,
    max_count: usize,
    index_is_sorted: bool,
) -> Result<DocIDMergerEnum<S>> {
    if index_is_sorted && max_count > 1 {
        Ok(DocIDMergerEnum::Sorted(SortedDocIDMerger::new(
            subs, max_count,
        )?))
    } else {
        Ok(DocIDMergerEnum::Sequential(SequentialDocIDMerger::new(
            subs,
        )?))
    }
}
/// Construct this from the provided subs.
pub(crate) fn of<S: SubBase>(
    subs: Vec<Sub<S>>,
    index_is_sorted: bool,
) -> Result<DocIDMergerEnum<S>> {
    let max_count = subs.len();
    of_with_max_count(subs, max_count, index_is_sorted)
}

#[cfg(test)]
pub mod tests {
    use std::rc::Rc;

    use rand::Rng;

    use crate::core::index::doc_id_merger::{DocIDMerger, Sub, SubBase};
    use crate::core::index::merge_state::DocMap;
    use crate::core::index::of;
    use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
    use crate::core::util::bit_set::BitSet;
    use crate::core::util::bits::Bits;
    use crate::core::util::error::lucene_error::Result;
    use crate::core::util::fixed_bit_set::FixedBitSet;
    use crate::test::util::lucene_test_case::lucene_test_case_util::random;
    use crate::test::util::test_util::TestUtil;

    #[allow(dead_code)] // for quick search
    struct TestDocIDMerger;
    pub struct TestSubUnsorted {
        doc_id: i32,
        value_start: i32,
        max_doc: i32,
        doc_map: Rc<DocMapMock1>,
    }

    impl TestSubUnsorted {
        pub fn new(doc_map: Rc<DocMapMock1>, max_doc: i32, value_start: i32) -> Self {
            Self {
                doc_id: -1,
                value_start,
                max_doc,
                doc_map,
            }
        }

        pub fn get_value(&self) -> i32 {
            self.value_start + self.doc_id
        }
    }

    impl SubBase for TestSubUnsorted {
        fn next_doc(&mut self) -> Result<i32> {
            self.doc_id += 1;
            if self.doc_id == self.max_doc {
                Ok(NO_MORE_DOCS)
            } else {
                Ok(self.doc_id)
            }
        }

        type DocMap = DocMapMock1;

        fn get_doc_map(&self) -> Result<&Self::DocMap> {
            Ok(&self.doc_map)
        }
    }
    pub struct DocMapMock1 {
        doc_base: usize,
    }
    impl DocMap for DocMapMock1 {
        fn get(&self, doc_id: i32) -> Result<i32> {
            Ok(self.doc_base as i32 + doc_id)
        }
    }

    #[test]
    fn test_no_sort() -> Result<()> {
        let mut random = random();
        let sub_count = TestUtil::next_int(&mut random, 1, 200);
        let mut subs = vec![];
        let mut value_start: i32 = 0;

        for _ in 0..sub_count {
            let max_doc = TestUtil::next_int(&mut random, 1, 1000);
            let doc_base = value_start;
            let doc_map = Rc::new(DocMapMock1 {
                doc_base: doc_base as usize,
            });
            let sub = Sub::new(TestSubUnsorted::new(doc_map.clone(), max_doc, value_start));
            subs.push(sub);
            value_start += max_doc;
        }

        let mut merger = of(subs, false)?;

        let mut count = 0;

        while let Some(sub) = merger.next()? {
            assert_eq!(count, merger.get_subs()[sub].mapped_doc_id);
            assert_eq!(count, merger.get_subs()[sub].sub.get_value());
            count += 1;
        }

        assert_eq!(value_start, count);
        Ok(())
    }

    pub struct TestSubSorted {
        doc_id: i32,
        max_doc: i32,
        #[allow(dead_code)]
        index: i32,
        pub(crate) doc_map: Rc<DocMapMock2>,
    }

    impl TestSubSorted {
        pub fn new(doc_map: Rc<DocMapMock2>, max_doc: i32, index: i32) -> Self {
            Self {
                doc_id: -1,
                max_doc,
                index,
                doc_map,
            }
        }
    }

    impl SubBase for TestSubSorted {
        fn next_doc(&mut self) -> Result<i32> {
            self.doc_id += 1;
            if self.doc_id == self.max_doc {
                Ok(NO_MORE_DOCS)
            } else {
                Ok(self.doc_id)
            }
        }

        type DocMap = DocMapMock2;

        fn get_doc_map(&self) -> Result<&Self::DocMap> {
            Ok(&self.doc_map)
        }
    }

    pub struct DocMapMock2 {
        doc_map: Vec<i32>,
        live_docs: Option<Rc<FixedBitSet>>,
    }
    impl DocMapMock2 {
        fn new(doc_map: Vec<i32>, live_docs: &Option<Rc<FixedBitSet>>) -> Self {
            let live_docs = if live_docs.is_none() {
                None
            } else {
                Some(live_docs.as_ref().unwrap().clone())
            };
            Self { doc_map, live_docs }
        }
    }
    impl DocMap for DocMapMock2 {
        fn get(&self, doc_id: i32) -> Result<i32> {
            let mapped = self.doc_map[doc_id as usize];
            if self.live_docs.is_none() || self.live_docs.as_ref().unwrap().get(mapped as usize)? {
                Ok(mapped)
            } else {
                Ok(-1)
            }
        }
    }
    #[test]
    fn test_with_sort() -> Result<()> {
        let mut random = random();
        let sub_count = TestUtil::next_int(&mut random, 1, 20);
        let mut old_to_new: Vec<Vec<i32>> = Vec::new();
        // how many docs we've written to each sub:
        let mut uptos: Vec<usize> = Vec::new();
        let mut tot_doc_count = 0;

        for _ in 0..sub_count {
            let max_doc = TestUtil::next_usize(&mut random, 1, 1000);
            uptos.push(0);
            old_to_new.push(vec![0; max_doc as usize]);
            tot_doc_count += max_doc;
        }

        let mut completed_subs: Vec<Vec<i32>> = vec![];

        // Randomly assign global docIDs to subs
        for doc_id in 0..tot_doc_count {
            let sub = random.random_range(0..old_to_new.len());
            let mut upto = uptos[sub];
            old_to_new[sub][upto] = doc_id as i32;
            upto += 1;
            if upto == old_to_new[sub].len() {
                completed_subs.push(old_to_new[sub].clone());
                old_to_new.remove(sub);
                uptos.remove(sub);
            } else {
                uptos[sub] = upto;
            }
        }

        assert_eq!(old_to_new.len(), 0);

        // Optional deletions
        let mut live_docs: Option<Rc<FixedBitSet>> = None;
        if random.random_bool(0.5) {
            let mut bitset = FixedBitSet::new(tot_doc_count);
            bitset.set_with_range(0, tot_doc_count);
            let delete_attempts = TestUtil::next_int(&mut random, 1, tot_doc_count as i32);
            for _ in 0..delete_attempts {
                bitset.clear_with_index(random.random_range(0..tot_doc_count));
            }
            live_docs = Some(Rc::new(bitset));
        }

        let mut subs: Vec<Sub<TestSubSorted>> = Vec::new();

        for (i, doc_map) in completed_subs.iter().enumerate() {
            let len = doc_map.len();
            let doc_map_enum = Rc::new(DocMapMock2::new(doc_map.clone(), &live_docs));

            let sub = Sub::new(TestSubSorted::new(doc_map_enum, len as i32, i as i32));

            subs.push(sub);
        }

        let mut merger = of(subs, true)?;

        let mut count = 0;
        while let Some(sub) = merger.next()? {
            if let Some(ref live) = live_docs {
                count = live.next_set_bit(count);
            }
            assert_eq!(
                count,
                merger.get_subs()[sub].mapped_doc_id as usize,
                "doc mismatch at count {}",
                count
            );
            count += 1;
        }

        if let Some(ref live) = live_docs {
            if count < tot_doc_count {
                assert_eq!(live.next_set_bit(count), NO_MORE_DOCS as usize);
            } else {
                assert_eq!(count, tot_doc_count);
            }
        } else {
            assert_eq!(count, tot_doc_count);
        }

        Ok(())
    }
}
