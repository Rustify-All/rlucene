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
use std::collections::HashSet;

use rand::Rng;

use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::util::accountable::Accountable;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::sparse_fixed_bit_set::SparseFixedBitSet;
use crate::test::util::id_set_common;
use crate::test::util::id_set_common::clear_range;
use crate::test::util::lucene_test_case::lucene_test_case_util::{at_least, random};

pub fn random_set<R: Rng + ?Sized>(
    random: &mut R,
    num_bits: usize,
    percent_set: f32,
) -> bit_set::BitSet {
    random_set_impl(random, num_bits, (percent_set * num_bits as f32) as usize)
}

pub fn random_set_impl<R: Rng + ?Sized>(
    random: &mut R,
    num_bits: usize,
    num_bits_set: usize,
) -> bit_set::BitSet {
    assert!(num_bits_set <= num_bits);
    let mut set = bit_set::BitSet::with_capacity(num_bits);
    if num_bits_set == num_bits {
        id_set_common::set_range(&mut set, 0, num_bits)
    } else {
        for _i in 0..num_bits_set {
            loop {
                let o = random.random_range(0..num_bits);
                if !set.contains(o) {
                    set.insert(o);
                    break;
                }
            }
        }
    }
    set
}
pub trait BaseBitSetTestCase {
    fn copy_of(
        &self,
        bs: &RustUtilBitSet,
        length: usize,
    ) -> (impl BitSet, Option<SparseFixedBitSet>);
    fn assert_equals(
        &self,
        set1: &RustUtilBitSet,
        set2: &impl BitSet,
        max_doc: usize,
        sfbs: &Option<SparseFixedBitSet>,
    );
    fn test_cardinality<R: Rng + ?Sized>(&mut self, random: &mut R) {
        let num_bits = 1 + random.random_range(0..100000);
        for percent_set in [0f32, 0.01, 0.1, 0.5, 0.9, 0.99, 1f32] {
            let set1 = RustUtilBitSet::new(random_set(random, num_bits, percent_set), num_bits);
            let (set2, _sfbs) = self.copy_of(&set1, num_bits);
            assert_eq!(set1.cardinality(), set2.cardinality());
        }
    }
    fn test_prev_set_bit<R: Rng + ?Sized>(&mut self, random: &mut R) {
        // TODO: 1000 should be 100000
        let num_bits = 1 + random.random_range(0..1000);
        for percent_set in [0f32, 0.01, 0.1, 0.5, 0.9, 0.99, 1f32] {
            let set1 = RustUtilBitSet::new(random_set(random, num_bits, percent_set), num_bits);
            let (set2, _sfbs) = self.copy_of(&set1, num_bits);
            for i in 0..num_bits {
                assert_eq!(set1.prev_set_bit(i), set2.prev_set_bit(i));
            }
        }
    }
    fn test_next_set_bit<R: Rng + ?Sized>(&mut self, random: &mut R) {
        // TODO: 1000 should be 100000
        let num_bits = 1 + random.random_range(0..1000);
        for percent_set in [0f32, 0.01, 0.1, 0.5, 0.9, 0.99, 1f32] {
            let set1 = RustUtilBitSet::new(random_set(random, num_bits, percent_set), num_bits);
            let (set2, _sfbs) = self.copy_of(&set1, num_bits);
            for i in 0..num_bits {
                assert_eq!(set1.next_set_bit(i), set2.next_set_bit(i));
            }
        }
    }
    fn test_next_set_bit_in_range<R: Rng + ?Sized>(&mut self, random: &mut R) {
        // TODO: 1000 should be 100000
        let num_bits = 1 + random.random_range(0..1000);
        for percent_set in [0f32, 0.01, 0.1, 0.5, 0.9, 0.99, 1f32] {
            let set1 = RustUtilBitSet::new(random_set(random, num_bits, percent_set), num_bits);
            let (set2, _sfbs) = self.copy_of(&set1, num_bits);
            for start in 0..num_bits {
                let end = if start + 1 == num_bits {
                    num_bits
                } else {
                    random.random_range(start + 1..num_bits)
                };
                assert_eq!(
                    set1.next_set_bit_range(start, end),
                    set2.next_set_bit_range(start, end),
                    "start={}, end={}, num_bits = {}",
                    start,
                    end,
                    num_bits
                );
            }
        }
    }
    fn test_set<R: Rng + ?Sized>(&self, random: &mut R) {
        let num_bits = 1 + random.random_range(0..100000) as usize;
        let set3 = RustUtilBitSet::new(random_set_impl(random, num_bits, 0), num_bits);
        let mut set1 = set3.clone();
        let (mut set2, sfbs) = self.copy_of(&set3, num_bits);
        let iters = 10000 + random.random_range(0..10000);
        for _i in 0..iters {
            let index = random.random_range(0..num_bits);
            set1.set(index);
            set2.set(index);
        }
        self.assert_equals(&set1, &set2, num_bits, &sfbs);
    }
    fn test_get_and_set<R: Rng + ?Sized>(&self, random: &mut R) {
        let num_bits = 1 + random.random_range(0..100000);
        let set3 = RustUtilBitSet::new(random_set_impl(random, num_bits, 0), num_bits);
        let mut set1 = set3.clone();
        let (mut set2, sfbs) = self.copy_of(&set3, num_bits);
        let iters = 10000 + random.random_range(0..10000);
        for _i in 0..iters {
            let index = random.random_range(0..num_bits);
            let v1 = set1.get_and_set(index);
            let v2 = set2.get_and_set(index);
            assert_eq!(v1, v2);
        }
        self.assert_equals(&set1, &set2, num_bits, &sfbs);
    }
    fn test_clear<R: Rng + ?Sized>(&mut self, random: &mut R) {
        let num_bits = 1 + random.random_range(0..100000);
        for percent_set in [0f32, 0.01, 0.1, 0.5, 0.9, 0.99, 1f32] {
            let set3 = RustUtilBitSet::new(random_set(random, num_bits, percent_set), num_bits);
            let mut set1 = set3.clone();
            let (mut set2, _sfbs) = self.copy_of(&set3, num_bits);
            let iters = 1 + random.random_range(0..(num_bits * 2));
            for _i in 0..iters {
                let index = random.random_range(0..num_bits);
                set1.clear_with_index(index);
                set2.clear_with_index(index);
            }
        }
    }

    fn test_clear_range<R: Rng + ?Sized>(&self, random: &mut R) {
        let num_bits = 1 + random.random_range(0..100000);
        for percent_set in [0f32, 0.01, 0.1, 0.5, 0.9, 0.99, 1f32] {
            let set3 = RustUtilBitSet::new(random_set(random, num_bits, percent_set), num_bits);
            let mut set1 = set3.clone();
            let (mut set2, sfbs) = self.copy_of(&set3, num_bits);
            let iters = at_least(random, 10);
            for _i in 0..iters {
                let from = random.random_range(0..num_bits);
                let to = random.random_range(0..(num_bits + 1));
                set1.clear_range(from, to);
                set2.clear_range(from, to);
                self.assert_equals(&set1, &set2, num_bits, &sfbs);
            }
        }
    }
    fn test_clear_all<R: Rng + ?Sized>(&self, random: &mut R) {
        let num_bits = 1 + random.random_range(0..100000);
        for percent_set in [0f32, 0.01, 0.1, 0.5, 0.9, 0.99, 1f32] {
            let set3 = RustUtilBitSet::new(random_set(random, num_bits, percent_set), num_bits);
            let mut set1 = set3.clone();
            let (mut set2, sfbs) = self.copy_of(&set3, num_bits);
            let iters = at_least(random, 10);
            for _i in 0..iters {
                set1.clear();
                set2.clear();
                self.assert_equals(&set1, &set2, num_bits, &sfbs);
            }
        }
    }

    fn test_or_sparse<R: Rng + ?Sized>(&mut self, random: &mut R) {
        self.test_or_impl(random, 0.001)
    }
    fn test_or_dense<R: Rng + ?Sized>(&mut self, random: &mut R) {
        self.test_or_impl(random, 0.5)
    }
    fn test_or_random<R: Rng + ?Sized>(&mut self, random: &mut R) {
        let random_float: f32 = random.random();
        self.test_or_impl(random, random_float)
    }
    fn test_or_impl<R: Rng + ?Sized>(&self, _random: &mut R, _load: f32) {
        // let num_bits = 1 + random.random_range(0..100000);
        // let set1 = RustUtilBitSet::new(random_set(random, num_bits, 0f32),
        // num_bits); let (mut set2, sfbs) = self.copy_of(&set1,
        // num_bits);
        //
        // let iteration = random.random_range(10..1000);
        // for iter in 0..iteration {
        //     let bitset = RustUtilBitSet::new(random_set(random, num_bits,
        // 0f32), num_bits);     // let other_set =
        // random_copy(random,bitset,num_bits);     todo!()
        // }
    }
}

pub trait BaseBitSetTestCaseSupperImpl {
    fn assert_equals<T: BitSet>(
        &self,
        set1: &RustUtilBitSet,
        set2: &T,
        max_doc: usize,
        _sfbs: &Option<SparseFixedBitSet>,
    ) {
        for i in 0..max_doc {
            assert_eq!(
                set1.get(i).unwrap(),
                set2.get(i).unwrap(),
                "Different at: {}",
                i
            );
        }
    }
}

//TODO
fn random_copy<R: Rng + ?Sized>(_random: &mut R, _set: impl BitSet, _num_bits: usize) {
    todo!()
}
pub struct RustUtilBitSet {
    bitset: bit_set::BitSet,
    num_bits: usize,
    index_hash_set: HashSet<usize>,
}

impl RustUtilBitSet {
    pub(crate) fn new(bitset: bit_set::BitSet, num_bits: usize) -> Self {
        let iter = bitset.iter();
        let mut index_hash_set = HashSet::new();
        for index in iter {
            index_hash_set.insert(index);
        }
        RustUtilBitSet {
            bitset,
            num_bits,
            index_hash_set,
        }
    }
}

impl Clone for RustUtilBitSet {
    fn clone(&self) -> Self {
        let bitset = self.bitset.clone();
        let num_bits = self.num_bits;
        let index_hash_set = self.index_hash_set.clone();
        RustUtilBitSet {
            bitset,
            num_bits,
            index_hash_set,
        }
    }
}

impl PartialEq for RustUtilBitSet {
    fn eq(&self, other: &Self) -> bool {
        if self.bitset == other.bitset && self.num_bits == other.num_bits {
            return true;
        }
        false
    }
}

impl Bits for RustUtilBitSet {
    fn get(&self, index: usize) -> Result<bool> {
        Ok(self.bitset.contains(index))
    }

    fn length(&self) -> usize {
        self.num_bits
    }
}

impl Accountable for RustUtilBitSet {
    fn ram_bytes_used(&self) -> Result<i64> {
        Ok(0)
    }
}

impl BitSet for RustUtilBitSet {
    fn clear(&mut self) {
        self.bitset.clear();
    }

    fn set(&mut self, i: usize) {
        self.bitset.insert(i);
    }

    fn get_and_set(&mut self, i: usize) -> bool {
        let v = self.get(i).unwrap_or(false);
        self.set(i);
        v
    }

    fn clear_with_index(&mut self, i: usize) {
        self.bitset.remove(i);
    }

    fn clear_range(&mut self, start_index: usize, end_index: usize) {
        if start_index >= end_index {
            return;
        }
        clear_range(&mut self.bitset, start_index, end_index);
    }

    fn cardinality(&self) -> usize {
        self.bitset.len()
    }

    fn approximate_cardinality(&self) -> usize {
        self.bitset.len()
    }

    fn prev_set_bit(&self, index: usize) -> Option<usize> {
        let mut index = index as i32;
        while index >= 0 {
            if self.bitset.contains((index) as usize) {
                return Option::from(index as usize);
            }
            index -= 1
        }
        None
    }

    fn next_set_bit_range(&self, start: usize, end: usize) -> usize {
        // TODO:: this implement too slow
        for index in start..end {
            if self.index_hash_set.contains(&index) {
                return index;
            }
        }
        NO_MORE_DOCS as usize
    }

    fn or<T: DocIdSetIterator>(&mut self, _iter: &mut T) -> Result<()> {
        todo!()
    }
}
#[test]
fn bit_set_util_equal_and_clone() {
    let mut random = random();
    let num_bits = 10;
    let mut bit1 = bit_set::BitSet::new();
    let mut bit2 = bit_set::BitSet::new();
    let iter = random.random_range(0..100000);
    for i in 0..iter {
        bit1.insert(i);
        bit2.insert(i);
    }
    let bit_set_util1 = RustUtilBitSet::new(bit1, num_bits);
    let bit_set_util2 = RustUtilBitSet::new(bit2, num_bits);
    assert!(bit_set_util1.eq(&bit_set_util2));

    let bit_set_util3 = bit_set_util2.clone();
    assert!(bit_set_util3.eq(&bit_set_util2));
}
