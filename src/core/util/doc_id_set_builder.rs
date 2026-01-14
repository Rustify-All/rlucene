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
use crate::core::index::point_values::PointValues;
use crate::core::search::doc_id_set::DocIdSet;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::util::TryIntoInt;
use crate::core::util::accountable::Accountable;
use crate::core::util::bit_doc_id_set::BitDocIdSet;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bit_set_iterator::BitSetIterator;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::int_array_doc_id_set::{IntArrayDocIdSet, IntArrayDocIdSetIterator};
use std::sync::Arc;

/// A builder of [`DocIdSet`]s. Initially, it uses a sparse structure to gather
/// documents, and then upgrades to a non-sparse bit set once enough hits match.
///
///
/// # Note
/// This is an internal API.
pub struct DocIdSetBuilder {
    max_doc: i32,
    threshold: i32,
    // pkg-private for testing
    multi_valued: bool,
    num_values_per_doc: f64,

    buffer: Vec<i32>,
    bit_set: Option<FixedBitSet>,
    counter: i64,
}
impl DocIdSetBuilder {
    /// Create a builder that can contain doc IDs between  0 and maxDoc.
    pub fn new(max_doc: i32) -> DocIdSetBuilder {
        Self::with_count(max_doc, -1, -1)
    }

    pub fn with_point_values<PV>(max_doc: i32, values: &PV, _field: &str) -> Result<DocIdSetBuilder>
    where
        PV: PointValues,
    {
        let v: i64 = values.size()?.try_convert()?;
        Ok(Self::with_count(max_doc, values.get_doc_count()?, v))
    }

    pub fn with_count(max_doc: i32, doc_count: i32, value_count: i64) -> DocIdSetBuilder {
        let multi_valued = doc_count < 0 || doc_count as i64 != value_count;
        let num_values_per_doc = if doc_count <= 0 || value_count < 0 {
            // assume one value per doc, this means the cost will be
            // overestimated if the docs are actually multi-valued
            1f64
        } else {
            // otherwise compute from index stats
            value_count as f64 / doc_count as f64
        };
        debug_assert!(
            num_values_per_doc >= 1f64,
            "value_count = {value_count} doc_count = {doc_count}"
        );
        // For ridiculously small sets, we'll just use a sorted int[]
        // maxDoc >>> 7 is a good value if you want to save memory, lower values
        // such as maxDoc >>> 11 should provide faster building but at the
        // expense of using a full bitset even for quite sparse data
        Self {
            max_doc,
            multi_valued,
            num_values_per_doc,
            threshold: max_doc >> 7,
            buffer: Vec::new(),
            bit_set: None,
            counter: 0,
        }
    }
    pub fn add_disi(&mut self, iter: &mut impl DocIdSetIterator) -> Result<()> {
        let cost = std::cmp::min(iter.cost()?, i32::MAX as i64);
        self.grow(cost as i32);
        if self.bit_set.is_some() {
            BitSet::or(self.bit_set.as_mut().unwrap(), iter)?;
            return Ok(());
        }
        for _i in 0..cost {
            let doc = iter.next_doc()?;
            if doc == NO_MORE_DOCS {
                return Ok(());
            }
            self.add_doc(doc);
        }
        let mut doc = iter.next_doc()?;
        while doc != NO_MORE_DOCS {
            self.grow(1);
            self.add_doc(doc);
            doc = iter.next_doc()?;
        }
        Ok(())
    }
    pub fn add_doc(&mut self, doc: i32) {
        if self.bit_set.is_none() {
            self.buffer.push(doc);
        } else {
            self.bit_set.as_mut().unwrap().set(doc as usize);
        }
    }
    pub fn grow(&mut self, num_docs: i32) {
        if self.bit_set.is_none() {
            if self.buffer.len() as i32 + num_docs > self.threshold {
                self.upgrade_to_bitset();
                self.counter += num_docs as i64;
            }
        } else {
            self.counter += num_docs as i64;
        }
    }
    fn upgrade_to_bitset(&mut self) {
        debug_assert!(self.bit_set.is_none());
        let mut bitset = FixedBitSet::new(self.max_doc as usize);
        let mut counter = 0i64;
        for doc in self.buffer.iter() {
            bitset.set(*doc as usize);
            counter += 1;
        }
        self.bit_set = Some(bitset);
        self.counter = counter;
        self.buffer.clear();
    }
    pub fn build(&mut self) -> Result<DocIdSetBuilderEnum> {
        if self.bit_set.is_some() {
            debug_assert!(self.counter >= 0);
            let cost = (self.counter as f64 / self.num_values_per_doc).round();
            let result = BitDocIdSet::with_cost(self.bit_set.take(), cost as i64)?;
            Ok(DocIdSetBuilderEnum::BitDoc(result))
        } else {
            self.buffer.sort();
            if self.multi_valued {
                self.buffer.dedup();
            } else {
                debug_assert!(self.no_dups());
            }
            self.buffer.push(NO_MORE_DOCS);
            let l = self.buffer.len() - 1;
            let result = IntArrayDocIdSet::new(std::mem::take(&mut self.buffer), l as i32)?;
            Ok(DocIdSetBuilderEnum::IntArray(result))
        }
    }
    fn no_dups(&self) -> bool {
        for i in 1..self.buffer.len() {
            debug_assert_eq!(self.buffer[i], self.buffer[i - 1]);
        }
        true
    }
    #[cfg(debug_assertions)]
    pub fn get_num_values_per_doc(&self) -> f64 {
        self.num_values_per_doc
    }
    #[cfg(debug_assertions)]
    pub fn get_multi_valued(&self) -> bool {
        self.multi_valued
    }
}

pub enum DocIdSetBuilderEnum {
    BitDoc(BitDocIdSet<FixedBitSet>),
    IntArray(IntArrayDocIdSet),
}
impl Accountable for DocIdSetBuilderEnum {
    fn ram_bytes_used(&self) -> Result<i64> {
        todo!()
    }
}

impl DocIdSet for DocIdSetBuilderEnum {
    type DocIdSetIterator = DocIdSetBuilderIterator;

    fn iterator(&self) -> Result<Option<Self::DocIdSetIterator>> {
        match self {
            DocIdSetBuilderEnum::BitDoc(m) => Ok(Some(DocIdSetBuilderIterator::BitSet(
                m.iterator()?.unwrap(),
            ))),
            DocIdSetBuilderEnum::IntArray(m) => Ok(Some(DocIdSetBuilderIterator::IntArray(
                m.iterator()?.unwrap(),
            ))),
        }
    }

    type BitType = FixedBitSet;

    fn bits(&self) -> Option<Arc<Self::BitType>> {
        match self {
            DocIdSetBuilderEnum::BitDoc(bit_doc_id_set) => Some(bit_doc_id_set.bits().unwrap()),
            DocIdSetBuilderEnum::IntArray(_) => None,
        }
    }
}
pub enum DocIdSetBuilderIterator {
    BitSet(BitSetIterator<Arc<FixedBitSet>>),
    IntArray(IntArrayDocIdSetIterator),
}
impl DocIdSetIterator for DocIdSetBuilderIterator {
    fn doc_id(&self) -> i32 {
        match self {
            DocIdSetBuilderIterator::BitSet(bit_set) => bit_set.doc_id(),
            DocIdSetBuilderIterator::IntArray(int_array) => int_array.doc_id(),
        }
    }

    fn next_doc(&mut self) -> Result<i32> {
        match self {
            DocIdSetBuilderIterator::BitSet(bit_set) => bit_set.next_doc(),
            DocIdSetBuilderIterator::IntArray(int_array) => int_array.next_doc(),
        }
    }

    fn advance(&mut self, target: i32) -> Result<i32> {
        match self {
            DocIdSetBuilderIterator::BitSet(bit_set) => bit_set.advance(target),
            DocIdSetBuilderIterator::IntArray(int_array) => int_array.advance(target),
        }
    }

    fn cost(&self) -> Result<i64> {
        match self {
            DocIdSetBuilderIterator::BitSet(bit_set) => bit_set.cost(),
            DocIdSetBuilderIterator::IntArray(int_array) => int_array.cost(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::core::search::doc_id_set::DocIdSet;
    use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
    use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, RangeDISI};
    use crate::core::util::bit_doc_id_set::BitDocIdSet;
    use crate::core::util::bit_set::BitSet;
    use crate::core::util::bit_set_iterator::BitSetIterator;
    use crate::core::util::bits::Bits;
    use crate::core::util::doc_id_set_builder::{DocIdSetBuilder, DocIdSetBuilderEnum};
    use crate::core::util::error::lucene_error::Result;
    use crate::core::util::fixed_bit_set::FixedBitSet;
    use crate::core::util::int_array_doc_id_set::IntArrayDocIdSet;
    use crate::core::util::roaring_doc_id_set::builder::Builder;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{
        is_night_mode, random, rarely,
    };
    use crate::test::util::test_util::TestUtil;
    use rand::Rng;

    #[allow(dead_code)] // for quick search
    struct TestDocIdSetBuilder {}
    #[test]
    fn test_empty() -> Result<()> {
        let mut random = random();
        let max_doc = random.random_range(1..1000);
        let doc_id_set: Option<IntArrayDocIdSet> = None;
        assert_equals(
            doc_id_set,
            Some(DocIdSetBuilder::new(max_doc).build().unwrap()),
        )?;
        Ok(())
    }
    fn assert_equals<T1: DocIdSet, T2: DocIdSet>(
        mut d1: Option<T1>,
        mut d2: Option<T2>,
    ) -> Result<()> {
        match (d1.as_mut(), d2.as_mut()) {
            (None, None) => {
                assert_eq!(
                    d2.as_mut()
                        .unwrap()
                        .iterator()?
                        .as_mut()
                        .unwrap()
                        .next_doc()?,
                    NO_MORE_DOCS
                );
            },

            (None, Some(d2v)) => {
                assert_eq!(d2v.iterator()?.unwrap().next_doc()?, NO_MORE_DOCS);
            },

            (Some(d1v), None) => {
                assert_eq!(d1v.iterator()?.unwrap().next_doc()?, NO_MORE_DOCS);
            },

            (Some(d1v), Some(d2v)) => {
                let mut i1 = d1v.iterator()?.unwrap();
                let mut i2 = d2v.iterator()?.unwrap();

                let mut doc = i1.next_doc()?;
                while doc != NO_MORE_DOCS {
                    assert_eq!(doc, i2.next_doc()?);
                    doc = i1.next_doc()?;
                }
                assert_eq!(i2.next_doc()?, NO_MORE_DOCS);
            },
        }

        Ok(())
    }

    #[test]
    fn test_sparse() -> Result<()> {
        let mut random = random();
        let max_doc = 1000000 + random.random_range(0..1000000);
        let mut builder = DocIdSetBuilder::new(max_doc);
        let num_iterators = 1 + random.random_range(0..10);
        let mut fixed_set_bit = FixedBitSet::new(max_doc as usize);
        for _i in 0..num_iterators {
            let base_inc = 200000 + random.random_range(0..10000);
            let mut b = Builder::new(max_doc as usize);
            let mut doc = random.random_range(0..100);
            while doc < max_doc {
                b.add(doc)?;
                fixed_set_bit.set(doc as usize);
                doc += base_inc + random.random_range(0..10000);
            }
            let roaring_doc_id_set = b.build();
            let mut iter = roaring_doc_id_set.iterator()?.unwrap();
            builder.add_disi(&mut iter)?;
        }
        let result = builder.build()?;
        let enum_type1 = "BitDocIdSet<FixedBitSet>";
        let enum_type2 = "IntArrayDocIdSet";
        let doc_id_set_type = match result {
            DocIdSetBuilderEnum::BitDoc(_) => enum_type1,
            DocIdSetBuilderEnum::IntArray(_) => enum_type2,
        };
        assert_eq!(doc_id_set_type, enum_type2);
        let bit_doc_id_set = BitDocIdSet::new(Some(fixed_set_bit))?;
        assert_equals(Some(bit_doc_id_set), Some(result))?;
        Ok(())
    }
    #[test]
    fn test_dense() -> Result<()> {
        let mut random = random();
        let max_doc = 1000000 + random.random_range(0..1000000);
        let mut builder = DocIdSetBuilder::new(max_doc);
        let num_iterators = 1 + random.random_range(0..10);
        let mut fixed_set_bit = FixedBitSet::new(max_doc as usize);
        for _i in 0..num_iterators {
            let mut b = Builder::new(max_doc as usize);
            let mut doc = random.random_range(0..1000);
            while doc < max_doc {
                b.add(doc)?;
                fixed_set_bit.set(doc as usize);
                doc += 1 + random.random_range(0..100);
            }
            let roaring_doc_id_set = b.build();
            let mut iter = roaring_doc_id_set.iterator()?.unwrap();
            builder.add_disi(&mut iter)?;
        }
        let result = builder.build()?;
        let enum_type1 = "BitDocIdSet<FixedBitSet>";
        let enum_type2 = "IntArrayDocIdSet";
        let doc_id_set_type = match result {
            DocIdSetBuilderEnum::BitDoc(_) => enum_type1,
            DocIdSetBuilderEnum::IntArray(_) => enum_type2,
        };
        assert_eq!(doc_id_set_type, enum_type1);
        let bit_doc_id_set = BitDocIdSet::new(Some(fixed_set_bit))?;
        assert_equals(Some(bit_doc_id_set), Some(result))?;
        Ok(())
    }

    #[test]
    fn test_random() -> Result<()> {
        let mut random = random();
        let max_doc = if is_night_mode() {
            TestUtil::next_int(&mut random, 1, 10000000)
        } else {
            TestUtil::next_int(&mut random, 1, 100000)
        };
        let mut i = 1;
        while i < (max_doc / 2) {
            let num_docs = TestUtil::next_int(&mut random, 1, i);
            let mut docs = FixedBitSet::new(max_doc as usize);
            let mut c = 0;
            while c < num_docs {
                let d = random.random_range(0..max_doc);
                if !docs.get(d as usize)? {
                    docs.set(d as usize);
                    c += 1
                }
            }
            let mut array = vec![0; num_docs as usize + random.random_range(0..100)];
            let (mut j, v) = {
                let mut it = BitSetIterator::new(docs, 0)?;
                let mut j = 0;
                let mut doc = it.next_doc()?;
                while doc != NO_MORE_DOCS {
                    array[j] = doc;
                    j += 1;
                    doc = it.next_doc()?;
                }
                (j, it.bits)
            };

            let docs = v;
            assert_eq!(num_docs, j as i32);
            // add some duplicates
            while j < array.len() {
                array[j] = array[random.random_range(0..num_docs as usize)];
                j += 1;
            }

            // shuffle
            for j in (1..array.len()).rev() {
                let k = random.random_range(0..j);
                array.swap(j, k);
            }

            // add docs out of order
            let mut builder = DocIdSetBuilder::new(max_doc);
            let mut j = 0;
            while j < array.len() {
                let l = TestUtil::next_int(&mut random, 1, (array.len() - j) as i32);
                let mut k = 0;
                let mut budget = 0;
                while k < l {
                    let rarely = rarely(&mut random);
                    if budget == 0 || rarely {
                        budget = TestUtil::next_int(&mut random, 1, l - k + 5);
                        builder.grow(budget);
                    }
                    builder.add_doc(array[j]);
                    budget -= 1;
                    k += 1;
                    j += 1;
                }
            }
            i <<= 1;
            let expected = BitDocIdSet::new(Some(docs))?;
            let actual = builder.build()?;
            assert_equals(Some(expected), Some(actual))?;
        }
        Ok(())
    }
    #[test]
    fn test_misleading_disi_cost() -> Result<()> {
        let mut random = random();
        let max_doc = TestUtil::next_int(&mut random, 1000, 10000);
        let mut builder = DocIdSetBuilder::new(max_doc);
        let mut expected = FixedBitSet::new(max_doc as usize);
        for _i in 0..100 {
            let mut docs = FixedBitSet::new(max_doc as usize);
            let num_docs = random.random_range(1..=max_doc / 1000);
            for _ in 0..num_docs {
                let doc = random.random_range(0..max_doc);
                docs.set(doc as usize);
            }
            expected.or(&docs);
            // We provide a cost of 0 here to make sure the builder can deal
            // with wrong costs
            let mut bit_doc_id_set = BitSetIterator::new(docs, 0)?;
            builder.add_disi(&mut bit_doc_id_set)?;
        }
        let bit_doc_id_set = BitDocIdSet::new(Some(expected))?;
        assert_equals(Some(bit_doc_id_set), Some(builder.build()?))?;
        Ok(())
    }
    #[test]
    fn test_empty_points() -> Result<()> {
        // TODO: waiting for the implementation of `PointValues`
        Ok(())
    }

    #[test]
    fn test_leverage_stats() -> Result<()> {
        // TODO: waiting for the implementation of `PointValues`
        // TODO: waiting for the implementation of `Terms`
        // single-valued points
        let mut doc_count = 42;
        let mut value_count = 42;
        let mut builder = DocIdSetBuilder::with_count(100, doc_count, value_count);
        assert_eq!(1f64 - builder.get_num_values_per_doc(), 0f64);
        assert!(!builder.get_multi_valued());
        builder.grow(2);
        builder.add_doc(5);
        builder.add_doc(7);
        let mut set = builder.build()?;
        let enum_type1 = "BitDocIdSet<FixedBitSet>";
        let enum_type2 = "IntArrayDocIdSet";
        let doc_id_set_type = match set {
            DocIdSetBuilderEnum::BitDoc(_) => enum_type1,
            DocIdSetBuilderEnum::IntArray(_) => enum_type2,
        };
        assert_eq!(doc_id_set_type, enum_type1);
        assert_eq!(set.iterator()?.unwrap().cost()?, 2);

        // multi-valued
        doc_count = 42;
        value_count = 63;
        builder = DocIdSetBuilder::with_count(100, doc_count, value_count);
        assert_eq!(builder.get_num_values_per_doc() - 1.5, 0.0);
        assert!(builder.get_multi_valued());
        builder.grow(2);
        builder.add_doc(5);
        builder.add_doc(7);
        set = builder.build()?;
        let doc_id_set_type = match set {
            DocIdSetBuilderEnum::BitDoc(_) => enum_type1,
            DocIdSetBuilderEnum::IntArray(_) => enum_type2,
        };
        assert_eq!(doc_id_set_type, enum_type1);
        assert_eq!(set.iterator()?.unwrap().cost()?, 1);

        // incomplete stats
        doc_count = 42;
        value_count = -1;
        builder = DocIdSetBuilder::with_count(100, doc_count, value_count);
        assert_eq!(builder.get_num_values_per_doc() - 1.0, 0.0);
        assert!(builder.get_multi_valued());

        doc_count = -1;
        value_count = 82;
        builder = DocIdSetBuilder::with_count(100, doc_count, value_count);
        assert_eq!(builder.get_num_values_per_doc() - 1.0, 0.0);
        assert!(builder.get_multi_valued());
        Ok(())
    }

    #[test]
    fn test_cost_is_correct_after_bit_set_upgrade() -> Result<()> {
        let max_doc = 1000000;
        let mut builder = DocIdSetBuilder::new(max_doc);
        for i in 0..1000000 >> 6 {
            builder.add_disi(&mut RangeDISI::new(i, i + 1)?)?;
        }
        let set = builder.build()?;
        let enum_type1 = "BitDocIdSet<FixedBitSet>";
        let enum_type2 = "IntArrayDocIdSet";
        let doc_id_set_type = match set {
            DocIdSetBuilderEnum::BitDoc(_) => enum_type1,
            DocIdSetBuilderEnum::IntArray(_) => enum_type2,
        };
        assert_eq!(doc_id_set_type, enum_type1);
        assert_eq!(set.iterator()?.unwrap().cost()?, 1000000 >> 6);
        Ok(())
    }
}
