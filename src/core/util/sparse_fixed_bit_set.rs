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
use crate::core::util::accountable::Accountable;
use crate::core::util::bit_set::{BitSet, check_unpositioned};
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{SliceCopyOps, TryIntoInt};

// TODO

const SPARSE_FIXED_BIT_SET_BASE_RAM_BYTES_USED: i64 = 0;

const SINGLE_ELEMENT_ARRAY_BYTES_USED: i64 = 0;
const MASK_4096: usize = (1 << 12) - 1;

fn block_count(length: usize) -> usize {
    let mut block_count = length >> 12;
    if (block_count << 12) < length {
        block_count += 1;
    }
    debug_assert!((block_count << 12) >= length);
    block_count
}

/// A bit set that only stores `i64` values that have at least one bit set. The
/// way it works is that the space of bits is divided into blocks of 4096 bits,
/// which is 64 `i64`s. Then for each block, we have:
///
/// - A `Vec<i64>` which stores the non-zero `i64`s for that block.
/// - A `i64` so that bit `i` being set means that the `i-th` `i64` of the block
///   is non-null, and its offset in the array of `i64`s is the number of one
///   bits on the right of the `i-th` bit.
///
/// # Note
/// This is an internal API.
#[derive(Default)]
pub struct SparseFixedBitSet {
    indices: Vec<usize>,
    bits: Vec<Option<Vec<u64>>>,
    length: usize,
    non_zero_long_count: usize,
    ram_bytes_used: i64,
}

impl SparseFixedBitSet {
    pub fn new(length: usize) -> Result<SparseFixedBitSet> {
        if length < 1 {
            return Err(LuceneError::illegal_argument("length needs to be >= 1"));
        }
        let block_count = block_count(length);
        let indices = vec![0; block_count];
        // todo
        let ram_bytes_used = 0;
        Ok(SparseFixedBitSet {
            indices,
            bits: vec![None; block_count],
            length,
            non_zero_long_count: 0,
            ram_bytes_used,
        })
    }
    fn consistent(&self, index: usize) -> bool {
        debug_assert!(
            index < self.length,
            "index= {} ,length= {}",
            index,
            self.length
        );
        true
    }
    fn insert_block(&mut self, i4096: usize, i64bit: usize, i: usize) {
        self.indices[i4096] = i64bit;
        debug_assert!(self.bits[i4096].is_none());
        let block: Vec<u64> = vec![1_u64 << (i % 64)];
        self.bits[i4096] = Some(block);
        self.non_zero_long_count += 1;
        //TODO
        self.ram_bytes_used = 0;
    }
    fn insert_long(&mut self, i4096: usize, i64bit: usize, i: usize, index: usize) {
        self.indices[i4096] |= i64bit;
        // we count the number of bits that are set on the right of i64
        // this gives us the index at which to perform the insertion
        let o = (index & (i64bit - 1)).count_ones() as usize;
        let bit_array = self.bits[i4096].as_mut().unwrap();
        if bit_array[bit_array.len() - 1] == 0 {
            // since we only store non-zero longs, if the last value is 0, it
            // means that we already have extra space, make use of
            // it
            let big_array_length = bit_array.len();
            bit_array.copy_within(o..big_array_length - 1, o + 1);
            bit_array[o] = 1_u64 << (i % 64);
        } else {
            let new_size = oversize(bit_array.len() as i32 + 1);
            let mut new_bit_array = vec![0; new_size as usize];
            new_bit_array.copy_from(&bit_array[..o], 0);
            new_bit_array[o] = 1_u64 << (i % 64);
            new_bit_array.copy_from(&bit_array[o..], o + 1);
            self.bits[i4096] = Some(new_bit_array);
            //TODO
            self.ram_bytes_used = 0;
        }
        self.non_zero_long_count += 1;
    }
    fn and(&mut self, i4096: usize, i64: usize, mask: usize) {
        let index = self.indices[i4096];
        if index as u64 & (1_u64 << (i64 % 64)) != 0 {
            // offset of the long bits we are interested in in the array
            let o = (index as u64 & ((1_u64 << (i64 % 64)) - 1)).count_ones() as usize;
            let bits = self.bits[i4096].as_ref().unwrap()[o] & mask as u64;
            if bits == 0 {
                self.remove_long(i4096, i64, index, o);
            } else {
                self.bits[i4096].as_mut().unwrap()[o] = bits;
            }
        }
    }
    fn remove_long(&mut self, i4096: usize, i64: usize, mut index: usize, o: usize) {
        let mask = !(1_usize << (i64 % 64));
        index &= mask;
        self.indices[i4096] = index;
        if index == 0 {
            self.bits[i4096].take();
        } else {
            let length = index.count_ones() as usize;
            let bit_array = self.bits[i4096].as_mut().unwrap();
            bit_array.copy_within(o + 1..length + 1, o);
            bit_array[length] = 0;
        }
        self.non_zero_long_count -= 1;
    }
    fn clear_within_block(&mut self, i4096: usize, from: usize, to: usize) {
        let first_long = from >> 6;
        let last_long = to >> 6;

        if first_long == last_long {
            self.and(i4096, first_long, !mask(from, to));
        } else {
            debug_assert!(first_long < last_long);
            self.and(i4096, last_long, !mask(0, to));
            for i in (first_long + 1..last_long).rev() {
                self.and(i4096, i, 0);
            }
            self.and(i4096, first_long, !mask(from, 63));
        }
    }
    /// Return the first document that occurs on or after the provided block
    /// index.
    fn first_doc(&self, mut i4096: usize, i4096_upper: usize) -> usize {
        debug_assert!(i4096_upper <= self.indices.len());
        let mut index;
        while i4096 < i4096_upper {
            index = self.indices[i4096];
            if index != 0 {
                let i64 = index.trailing_zeros() as usize;
                return (i4096 << 12)
                    | (i64 << 6)
                    | self.bits[i4096].as_ref().unwrap()[0].trailing_zeros() as usize;
            }
            i4096 += 1;
        }
        NO_MORE_DOCS as usize
    }
    /// Return the last document that occurs on or before the provided block
    /// index.
    fn last_doc(&self, i4096: usize) -> Option<usize> {
        let mut index;
        let mut i4096: i32 = i4096 as i32;
        while i4096 >= 0 {
            index = self.indices[i4096 as usize];
            if index != 0 {
                let i64 = 63 - index.leading_zeros() as usize;
                let bits =
                    self.bits[i4096 as usize].as_ref().unwrap()[index.count_ones() as usize - 1];
                return Option::from(
                    ((i4096 as usize) << 12) | (i64 << 6) | (63 - bits.count_ones() as usize),
                );
            }
            i4096 -= 1;
        }
        None
    }
    fn next_set_bit_in_range_impl(&self, start: usize, upper_bound: usize) -> usize {
        debug_assert!(start < self.length);
        debug_assert!(
            upper_bound > start && upper_bound <= self.length,
            "upper_bound= {}, start= {}, length= {}",
            upper_bound,
            start,
            self.length
        );
        let i4096 = start >> 12;
        let index = self.indices[i4096];
        let bit_array = self.bits[i4096].as_ref();
        let mut i64 = start >> 6;
        let i64bit = 1_usize << (i64 % 64);
        let mut o = (index & (i64bit - 1)).count_ones() as usize;
        if index & i64bit != 0 {
            // There is at least one bit that is set in the current long, check
            // if one of them is after i
            debug_assert!(bit_array.is_some());
            let bits = bit_array.unwrap()[o] >> (start % 64);
            if bits != 0 {
                return start + bits.trailing_zeros() as usize;
            }
            o += 1;
        }
        let index_bits = (index >> i64) >> 1;
        if index_bits == 0 {
            // no more bits are set in the current block of 4096 bits, go to the
            // next one
            let i4096_upper = if upper_bound == self.length {
                self.indices.len()
            } else {
                block_count(upper_bound)
            };
            return self.first_doc(i4096 + 1, i4096_upper);
        }
        // there are still set bits
        i64 += 1 + index_bits.trailing_zeros() as usize;
        debug_assert!(bit_array.is_some());
        let bits = bit_array.unwrap()[o];
        (i64 << 6) | bits.trailing_zeros() as usize
    }

    fn _or_other(&mut self, other: SparseFixedBitSet) {
        for i in 0..other.indices.len() {
            let index = other.indices[i];
            if index != 0 {
                self.or_impl(
                    i,
                    index,
                    other.bits[i].as_ref().unwrap(),
                    index.count_ones() as usize,
                );
            }
        }
    }

    fn or_impl(&mut self, i4096: usize, index: usize, bits: &[u64], non_zero_long_count: usize) {
        debug_assert_eq!(index.count_ones(), non_zero_long_count as u32);
        let current_index = self.indices[i4096];
        if current_index == 0 {
            // fast path: if we currently have nothing in the block, just copy
            // the data this especially happens all the time if you
            // call OR on an empty set
            self.indices[i4096] = index;
            let new_bits = bits[0..non_zero_long_count].to_vec();
            self.bits[i4096] = Some(new_bits);
            // we may slightly overestimate size here, but keep it cheap
            //TODO
            self.ram_bytes_used = 0;
            self.non_zero_long_count += non_zero_long_count;
            return;
        }
        let mut current_bits = self.bits[i4096].take();
        let new_index = current_index | index;
        let required_capacity = new_index.count_ones();
        let mut new_bits = if current_bits.as_ref().unwrap().len() >= required_capacity as usize {
            current_bits.take().unwrap()
        } else {
            //TODO
            self.ram_bytes_used = 0;
            vec![0; oversize(required_capacity as i32) as usize]
        };
        // we iterate backwards in order to not override data we might need on
        // the next iteration if the array is reused
        let mut i = new_index.count_ones();
        let mut new0 = new_index.count_ones() - 1;
        while i < 64 {
            // bitIndex is the index of a bit which is set in newIndex and newO
            // is the number of 1 bits on its right
            let bit_index = 63 - i;
            debug_assert!(new0 == (new_index as u64 & (1_u64 << (bit_index % 64))).count_ones());
            new_bits[new0 as usize] = (long_bits(
                current_index,
                current_bits.as_ref().unwrap(),
                bit_index as usize,
            ) | long_bits(index, bits, bit_index as usize))
                as u64;
            i += 1 + (new_index << (i + 1)).count_ones();
            new0 -= 1;
        }
        self.indices[i4096] = new_index;
        self.bits[i4096] = Some(new_bits);
        self.non_zero_long_count +=
            non_zero_long_count - (current_index & index).count_ones() as usize;
    }
    /// [`or`](#method.or) implementation that works best when `it` is dense.
    fn or_dense(&mut self, mut it: impl DocIdSetIterator) -> Result<()> {
        check_unpositioned(&it)?;
        // The goal here is to try to take advantage of the ordering of
        // documents to build the data-structure more efficiently
        // NOTE: this heavily relies on the fact that shifts are mod 64
        let first_doc = it.next_doc()?.try_convert()?;
        if first_doc == NO_MORE_DOCS as usize {
            return Ok(());
        }
        let mut i4096 = first_doc >> 12;
        let mut i64 = first_doc >> 6;
        let mut index = 1_u64 << (i64 % 64);
        let mut current_long = 1_u64 << (first_doc % 64);
        // we store at most 64 longs per block so preallocate in order never to
        // have to resize
        let mut longs = vec![0; 64];
        longs.resize(64, 0);
        let mut num_longs = 0;

        let mut doc = it.next_doc()?.try_convert()?;
        while doc != NO_MORE_DOCS as usize {
            let doc64 = doc >> 6;
            if doc64 == i64 {
                // still in the same long, just set the bit
                current_long |= 1_u64 << (doc % 64);
            } else {
                longs[num_longs] = current_long;
                num_longs += 1;
                let doc4096 = doc >> 12;
                if doc4096 == i4096 {
                    index |= 1_u64 << (doc64 % 64);
                } else {
                    // we are on a new block, flush what we buffered
                    self.or_impl(i4096, index as usize, &longs, num_longs);
                    // and reset state for the new block
                    i4096 = doc4096;
                    index = 1_u64 << (doc64 % 64);
                    num_longs = 0;
                }
                // we are on a new long, reset state

                i64 = doc4096;
                current_long = 1_u64 << (doc % 64);
            }
            doc = it.next_doc()?.try_convert()?;
        }
        // flush
        longs[num_longs] = current_long;
        num_longs += 1;
        self.or_impl(i4096, index as usize, &longs, num_longs);
        Ok(())
    }
    #[cfg(test)]
    pub fn get_indices(&self) -> &[usize] {
        &self.indices
    }
    #[cfg(test)]
    pub fn get_bits(&self) -> &[Option<Vec<u64>>] {
        &self.bits
    }
    #[cfg(test)]
    pub fn get_non_zero_long_count(&self) -> usize {
        self.non_zero_long_count
    }
}

fn mask(from: usize, to: usize) -> usize {
    let shift = ((to - from) % 64 + 64) % 64;
    (((1_u64 << shift << 1) - 1) << (from % 64)) as usize
}

fn oversize(s: i32) -> i32 {
    let mut new_size = s + (s >> 1);
    if new_size > 50 {
        new_size = 64
    }
    new_size
}

fn long_bits(index: usize, bits: &[u64], i64: usize) -> i64 {
    if ((index as u64) & (1_u64 << (i64 % 64))) == 0 {
        0
    } else {
        bits[(index as u64 & ((1_u64 << (i64 % 64)) - 1)).count_ones() as usize] as i64
    }
}

impl Bits for SparseFixedBitSet {
    fn get(&self, i: usize) -> Result<bool> {
        debug_assert!(self.consistent(i));
        let i4096 = i >> 12;
        let index = self.indices[i4096];
        let i64 = i >> 6;
        let i64bit = 1_u64 << (i64 % 64);
        // first check the index, if the i64-th bit is not set, then i is not
        // set note: this relies on the fact that shifts are mod 64 in
        // java
        if index as u64 & i64bit == 0 {
            return Ok(false);
        }
        // if it is set, then we count the number of bits that are set on the
        // right of i64, and that gives us the index of the long that
        // stores the bits we are interested in
        let bits =
            self.bits[i4096].as_ref().unwrap()[(index as u64 & (i64bit - 1)).count_ones() as usize];
        Ok((bits & (1_u64 << (i % 64))) != 0)
    }

    fn length(&self) -> usize {
        self.length
    }
}

impl Accountable for SparseFixedBitSet {
    fn ram_bytes_used(&self) -> Result<i64> {
        todo!()
    }
}
impl BitSet for SparseFixedBitSet {
    fn clear(&mut self) {
        self.bits = vec![None; self.bits.len()];
        self.indices = vec![0; self.indices.len()];
        self.non_zero_long_count = 0;
        //TODO
        self.ram_bytes_used = 0;
    }

    fn set(&mut self, i: usize) {
        debug_assert!(self.consistent(i));
        let i4096 = i >> 12;
        let index = self.indices[i4096];
        let i64 = i >> 6;
        let i64bit = 1_u64 << (i64 % 64);
        if (index as u64 & i64bit) != 0 {
            // in that case the sub 64-bits block we are interested in already
            // exists, we just need to set a bit in an existing
            // long: the number of ones on the right of i64 gives us
            // the index of the long we need to update
            let o = (index as u64 & (i64bit - 1)).count_ones() as usize;
            let bit = self.bits[i4096].as_ref().unwrap()[o] | (1_u64 << (i % 64));
            self.bits[i4096].as_mut().unwrap()[o] = bit;
        } else if index == 0 {
            // if the index is 0, it means that we just found a block of 4096
            // bits that has no bit that is set yet. So let's
            // initialize a new block:
            self.insert_block(i4096, i64bit as usize, i);
        } else {
            // in that case we found a block of 4096 bits that has some values,
            // but the sub-block of 64 bits that we are interested
            // in has no value yet, so we need to insert a new long
            self.insert_long(i4096, i64bit as usize, i, index);
        }
    }

    fn get_and_set(&mut self, i: usize) -> bool {
        debug_assert!(self.consistent(i));
        let i4096 = i >> 12;
        let index = self.indices[i4096];
        let i64 = i >> 6;
        let i64bit = 1_u64 << (i64 % 64);
        if index as u64 & i64bit != 0 {
            // in that case the sub 64-bits block we are interested in already
            // exists, we just need to set a bit in an existing
            // long: the number of ones on the right of i64 gives us
            // the index of the long we need to update
            let location = (index as u64 & (i64bit - 1)).count_ones() as usize;
            let bit = 1_u64 << (i % 64);
            let v = self.bits[i4096].as_mut().unwrap()[location] & bit != 0;
            let bits = self.bits[i4096].as_mut().unwrap()[location];
            self.bits[i4096].as_mut().unwrap()[location] = bits | bit;
            v
        } else if index == 0 {
            // if the index is 0, it means that we just found a block of 4096
            // bits that has no bit that is set yet. So let's
            // initialize a new block:
            self.insert_block(i4096, i64bit as usize, i);
            false
        } else {
            // in that case we found a block of 4096 bits that has some values,
            // but the sub-block of 64 bits that we are interested
            // in has no value yet, so we need to insert a new long
            self.insert_long(i4096, i64bit as usize, i, index);
            false
        }
    }

    fn clear_with_index(&mut self, i: usize) {
        debug_assert!(self.consistent(i));
        let i4096 = i >> 12;
        let i64 = i >> 6;
        self.and(i4096, i64, !(1_usize << (i % 64)));
    }

    fn clear_range(&mut self, start_index: usize, end_index: usize) {
        debug_assert!(end_index <= self.length);
        if start_index >= end_index {
            return;
        }
        let first_block = start_index >> 12;
        let last_block = (end_index - 1) >> 12;
        if first_block == last_block {
            self.clear_within_block(
                first_block,
                start_index & MASK_4096,
                (end_index - 1) & MASK_4096,
            );
        } else {
            self.clear_within_block(first_block, start_index & MASK_4096, MASK_4096);
            for i in first_block + 1..last_block {
                self.non_zero_long_count -= self.indices[i].count_ones() as usize;
                self.indices[i] = 0;
                self.bits[i].take();
            }
            self.clear_within_block(last_block, 0, (end_index - 1) & MASK_4096);
        }
    }

    fn cardinality(&self) -> usize {
        let mut cardinality = 0;
        for bit_array in self.bits.iter().flatten() {
            for bits in bit_array {
                cardinality += bits.count_ones() as usize;
            }
        }
        cardinality
    }

    fn approximate_cardinality(&self) -> usize {
        // we are assuming that bits are uniformly set and use the linear
        // counting algorithm to estimate the number of bits that are
        // set based on the number of longs that are different from zero
        let total_longs = (self.length + 63) >> 6; //  total number of longs in the space
        debug_assert!(total_longs >= self.non_zero_long_count);
        let zero_longs = total_longs - self.non_zero_long_count; // number of longs that are zero
        // No need to guard against division by zero, it will return +Infinity
        // and things will work as
        // expected
        let estimate =
            (total_longs as f64 * (total_longs as f64 / zero_longs as f64).ln()).round() as usize;
        std::cmp::min(self.length, estimate)
    }

    fn prev_set_bit(&self, i: usize) -> Option<usize> {
        let i4096 = i >> 12;
        let index = self.indices[i4096];
        let bit_array = self.bits[i4096].as_ref();
        let mut i64 = i >> 6;
        let index_bits = index as u64 & ((1_u64 << (i64 % 64)) - 1);
        let o = index_bits.count_ones() as usize;
        if index as u64 & (1_u64 << (i64 % 64)) != 0 {
            // There is at least one bit that is set in the same long, check if
            // there is one bit that is set that is lower than i
            debug_assert!(bit_array.is_some());
            let bits = bit_array.unwrap()[o] & ((1_u64 << (i % 64) << 1) - 1);
            if bits != 0 {
                return Option::from((i64 << 6) | (63 - bits.leading_zeros()) as usize);
            }
        }
        if index_bits == 0 {
            // no more bits are set in this block, go find the last bit in the
            // previous block
            return self.last_doc(i4096 - 1);
        }
        // go to the previous long
        i64 = 63 - index_bits.leading_zeros() as usize;
        debug_assert!(bit_array.is_some());
        let bits = bit_array.unwrap()[o - 1];
        Some((i4096 << 12) | (i64 << 6) | (63 - bits.leading_zeros() as usize))
    }

    fn next_set_bit(&self, index: usize) -> usize {
        self.next_set_bit_in_range_impl(index, self.length)
    }

    /// Returns the next set bit in the specified range, but treats
    /// `upper_bound` as a best-effort hint rather than a hard requirement.
    /// Note that this may return a result that is greater than or equal
    /// to `upper_bound` in some cases, so callers must add their own check if
    /// `upper_bound` is a hard requirement.
    fn next_set_bit_range(&self, start: usize, end: usize) -> usize {
        let res = self.next_set_bit_in_range_impl(start, end);
        if res < end {
            res
        } else {
            NO_MORE_DOCS as usize
        }
    }

    fn or<T: DocIdSetIterator>(&mut self, iter: &mut T) -> Result<()> {
        //TODO: this is a naive implementation, we can optimize it from Java
        // Lucene
        check_unpositioned(iter)?;
        let mut doc = iter.next_doc()?.try_convert()?;
        while doc != NO_MORE_DOCS as usize {
            self.set(doc);
            doc = iter.next_doc()?.try_convert()?;
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {

    use rand::Rng;

    use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
    use crate::core::util::bit_set::BitSet;
    use crate::core::util::bits::Bits;
    use crate::core::util::sparse_fixed_bit_set::SparseFixedBitSet;
    use crate::test::util::base_bit_set_test_case::{
        BaseBitSetTestCase, BaseBitSetTestCaseSupperImpl, RustUtilBitSet,
    };
    use crate::test::util::lucene_test_case::lucene_test_case_util::random;

    pub struct TestSparseFixedBitSet;

    impl BaseBitSetTestCase for TestSparseFixedBitSet {
        fn copy_of(
            &self,
            bs: &RustUtilBitSet,
            length: usize,
        ) -> (impl BitSet, Option<SparseFixedBitSet>) {
            let mut set = SparseFixedBitSet::new(length).unwrap();
            let mut set1 = SparseFixedBitSet::new(length).unwrap();
            let mut doc = bs.next_set_bit(0);
            while doc != NO_MORE_DOCS as usize {
                set.set(doc);
                set1.set(doc);
                if doc + 1 > length {
                    doc = NO_MORE_DOCS as usize;
                } else {
                    doc = bs.next_set_bit(doc + 1);
                }
            }
            (set, Some(set1))
        }

        fn assert_equals(
            &self,
            set1: &RustUtilBitSet,
            set2: &impl BitSet,
            max_doc: usize,
            sfbs: &Option<SparseFixedBitSet>,
        ) {
            // check invariants of the sparse set
            let mut non_zero_long_count = 0;
            let sparse_fixed_bit_set = sfbs.as_ref().unwrap();
            let length = sparse_fixed_bit_set.get_indices().len();
            for i in 0..length {
                let n = sparse_fixed_bit_set.get_indices()[i].count_ones();
                if n != 0 {
                    non_zero_long_count += n;
                    let mut j = n;
                    while j < sparse_fixed_bit_set.get_bits()[i].as_ref().unwrap().len() as u32 {
                        let array = sparse_fixed_bit_set.get_bits()[i].as_ref().unwrap();
                        assert_eq!(array[j as usize], 0);
                        j += 1;
                    }
                }
            }
            assert_eq!(
                non_zero_long_count,
                sfbs.as_ref().unwrap().get_non_zero_long_count() as u32
            );
            BaseBitSetTestCaseSupperImpl::assert_equals(self, set1, set2, max_doc, sfbs);
        }
    }

    impl BaseBitSetTestCaseSupperImpl for TestSparseFixedBitSet {}
    #[test]
    fn test_cardinality() {
        let mut random = random();
        let mut fbs = TestSparseFixedBitSet;
        fbs.test_cardinality(&mut random);
    }
    #[test]
    fn test_prev_set_bit() {
        let mut random = random();
        let mut fbs = TestSparseFixedBitSet;
        fbs.test_prev_set_bit(&mut random);
    }
    #[test]
    fn test_next_set_bit() {
        let mut random = random();
        let mut fbs = TestSparseFixedBitSet;
        fbs.test_next_set_bit(&mut random);
    }
    #[test]
    fn test_next_set_bit_in_range() {
        let mut random = random();
        let mut fbs = TestSparseFixedBitSet;
        fbs.test_next_set_bit_in_range(&mut random);
    }
    #[test]
    fn test_set() {
        let mut random = random();
        let fbs = TestSparseFixedBitSet;
        fbs.test_set(&mut random);
    }
    #[test]
    fn test_get_and_set() {
        let mut random = random();
        let fbs = TestSparseFixedBitSet;
        fbs.test_get_and_set(&mut random);
    }
    #[test]
    fn test_clear() {
        let mut random = random();
        let mut fbs = TestSparseFixedBitSet;
        fbs.test_clear(&mut random);
    }
    #[test]
    fn test_clear_range() {
        let mut random = random();
        let fbs = TestSparseFixedBitSet;
        fbs.test_clear_range(&mut random);
    }
    #[test]
    fn test_clear_all() {
        let mut random = random();
        let fbs = TestSparseFixedBitSet;
        fbs.test_clear_all(&mut random);
    }
    #[test]
    fn test_or_sparse() {
        let mut random = random();
        let mut fbs = TestSparseFixedBitSet;
        fbs.test_or_sparse(&mut random);
    }
    #[test]
    fn test_or_dense() {
        let mut random = random();
        let mut fbs = TestSparseFixedBitSet;
        fbs.test_or_dense(&mut random);
    }
    #[test]
    fn test_or_random() {
        let mut random = random();
        let mut fbs = TestSparseFixedBitSet;
        fbs.test_or_random(&mut random);
    }

    #[test]
    fn test_approximate_cardinality() {
        let mut random = random();
        let mut set = SparseFixedBitSet::new(100).unwrap();
        let first = random.random_range(1000..10000);
        let interval = 200 + random.random_range(100..1000);
        let mut i = first;
        while i < set.length() {
            set.set(i);
            i += interval;
        }
        let cardinality = set.cardinality();
        assert!(cardinality.abs_diff(set.approximate_cardinality()) <= 20);
    }
    #[test]
    fn test_approximate_cardinality_on_dense_set() {
        let mut random = random();
        let num_docs = random.random_range(1..=10000);
        let mut set = SparseFixedBitSet::new(num_docs).unwrap();
        for i in 0..set.length() {
            set.set(i);
        }
        assert_eq!(num_docs, set.approximate_cardinality());
    }
    #[test]

    fn test_ram_bytes_used() {
        // todo
    }
}
