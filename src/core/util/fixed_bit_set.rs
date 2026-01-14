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
use std::hash::{Hash, Hasher};

use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::util::TryIntoInt;
use crate::core::util::accountable::Accountable;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bit_set::{BitSet, check_unpositioned};
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
// todo

const FIXED_BIT_SET_BASE_RAM_BYTES_USED: i64 = 0;

/// `BitSet` of fixed length (`num_bits`), backed by accessible (`get_bits`)
/// `long[]`, accessed with an `int` index, implementing [`Bits`] and
/// [`DocIdSet`](crate::core::search::doc_id_set). If you need to manage more than
/// 2.1B bits, use [`LongBitSet`](crate::core::util::long_bit_set::LongBitSet).
///
/// # Note
/// This is an internal API.
#[derive(Default, Debug)]
pub struct FixedBitSet {
    // Array of longs holding the bits
    bits: Vec<i64>,
    // The number of bits in use
    num_bits: usize,
    // The exact number of longs needed to hold numBits (<= bits.length)
    num_words: usize,
}

impl Hash for FixedBitSet {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.bits.hash(state);
        self.num_bits.hash(state);
        self.num_words.hash(state);
    }
}

impl PartialEq for FixedBitSet {
    fn eq(&self, other: &Self) -> bool {
        if self.num_bits == other.num_bits
            && self.num_words == other.num_words
            && self.bits == other.bits
        {
            return true;
        }
        false
    }
}

impl Clone for FixedBitSet {
    fn clone(&self) -> Self {
        let bits = self.bits.clone();
        Self::with_capacity(bits, self.num_bits).unwrap()
    }
}

/// If the given [`LongBitSet`](crate::core::util::long_bit_set::LongBitSet) is large
/// enough to hold `num_bits + 1`, returns the given bits, otherwise returns a
/// new [`LongBitSet`](crate::core::util::long_bit_set::LongBitSet) that can hold the
/// requested number of bits.
///
/// # Note
/// The returned bitset reuses the underlying `long[]` of the given `bits` if
/// possible. Also, calling `length()` on the returned bits may return a value
/// greater than `num_bits`.
impl FixedBitSet {
    /// returns the number of 64-bit words it would take to hold numBits
    pub fn bits2words(num_bits: usize) -> usize {
        let num_bits = num_bits as i32;
        (((num_bits - 1) >> 6) + 1) as usize
    }

    /// Returns the popcount or cardinality of the intersection of the two sets.
    /// Neither set is modified.
    pub fn intersection_count(a: FixedBitSet, b: FixedBitSet) -> i64 {
        // Depends on the ghost bits being clear!
        let mut tot = 0;
        let num_common_words = std::cmp::min(a.num_words, b.num_words);
        for i in 0..num_common_words {
            tot += (a.bits[i] & b.bits[i]).count_ones();
        }
        tot as i64
    }

    //// Returns the popcount or cardinality of the union of the two sets.
    //// Neither set is modified.
    pub fn union_count(a: &FixedBitSet, b: &FixedBitSet) -> i64 {
        // Depends on the ghost bits being clear!
        let mut tot = 0;
        let num_common_words = std::cmp::min(a.num_words, b.num_words);
        for i in 0..num_common_words {
            tot += (a.bits[i] | b.bits[i]).count_ones();
        }
        for i in num_common_words..a.num_words {
            tot += a.bits[i].count_ones();
        }
        for i in num_common_words..b.num_words {
            tot += b.bits[i].count_ones();
        }
        tot as i64
    }

    /// Returns the popcount or cardinality of "a and not b" or "intersection(a
    /// not(b))". Neither set is modified.
    pub fn and_not_count(a: &FixedBitSet, b: &FixedBitSet) -> i64 {
        let mut tot = 0;
        let num_common_words = std::cmp::min(a.num_words, b.num_words);
        for i in 0..num_common_words {
            tot += (a.bits[i] & !b.bits[i]).count_ones();
        }
        for i in num_common_words..a.num_words {
            tot += a.bits[i].count_ones();
        }
        tot as i64
    }
    /// Creates a new `FixedBitSet`. The internally allocated `Vec<u64>` array
    /// will be exactly the size needed to accommodate the `num_bits`
    /// specified.
    ///
    /// # Arguments
    /// * `num_bits` - The number of bits needed.
    pub fn new(num_bits: usize) -> FixedBitSet {
        let size: usize = Self::bits2words(num_bits);
        let bits: Vec<i64> = vec![0; size];
        let exact_size = bits.len();
        FixedBitSet {
            bits,
            num_bits,
            num_words: exact_size,
        }
    }
    /// Creates a new `FixedBitSet` using the provided `Vec<u64>` array as the
    /// backing store. The `stored_bits` array must be large enough to
    /// accommodate the `num_bits` specified, but may be larger. In that
    /// case, the 'extra' or 'ghost' bits must be clear (or they may provoke
    /// spurious side effects).
    ///
    /// # Arguments
    /// * `stored_bits` - The array to use as the backing store (`Vec<i64>`).
    /// * `num_bits` - The number of bits actually needed.
    pub fn with_capacity(stored_bits: Vec<i64>, num_bits: usize) -> Result<FixedBitSet> {
        let num_words = Self::bits2words(num_bits);
        if num_words > stored_bits.len() {
            return Err(LuceneError::illegal_argument(format!(
                "The given long array is too small  to hold {num_words} bits"
            )));
        }
        let result = FixedBitSet {
            bits: stored_bits,
            num_bits,
            num_words,
        };
        debug_assert!(Self::verify_ghost_bits_clear(&result));
        Ok(result)
    }

    /// Checks if the bits past `num_bits` are clear. Some methods rely on this
    /// implicit assumption: search for "Depends on the ghost bits being
    /// clear!"
    ///
    /// # Returns
    /// `true` if the bits past `num_bits` are clear.
    fn verify_ghost_bits_clear(fixed_bit_set: &FixedBitSet) -> bool {
        for i in fixed_bit_set.num_words..fixed_bit_set.bits.len() {
            if fixed_bit_set.bits[i] != 0 {
                return false;
            }
        }
        if (fixed_bit_set.num_bits & 0x3f) == 0 {
            return true;
        }

        let mask = -1 << (fixed_bit_set.num_bits % 64);
        (fixed_bit_set.bits[fixed_bit_set.num_words - 1] & mask) == 0
    }

    pub fn get_bits(&self) -> &[i64] {
        &self.bits
    }

    pub fn get_and_clear(&mut self, index: usize) -> bool {
        debug_assert!(
            index < self.num_bits,
            "index = {}, num_bits = {}",
            index,
            self.num_bits
        );
        let word_num = index >> 6;
        let bit_mask = 1_i64 << (index % 64);
        let val = (self.bits[word_num] & bit_mask) != 0;
        self.bits[word_num] &= !bit_mask;
        val
    }

    /// this = this OR other
    pub fn or(&mut self, other: &FixedBitSet) {
        self.or_impl(0, &other.bits, other.num_words);
    }

    fn or_offset(&mut self, other_offset_words: usize, other: &FixedBitSet) {
        self.or_impl(other_offset_words, &other.bits, other.num_words);
    }

    fn or_impl(&mut self, other_offset_words: usize, other_arr: &[i64], other_num_words: usize) {
        debug_assert!(
            other_num_words + other_offset_words <= self.num_words,
            "num_words = {} other_num_words = {}",
            self.num_words,
            other_num_words
        );
        let pos = std::cmp::min(self.num_words - other_offset_words, other_num_words);
        let offset = other_offset_words;
        for i in (0..pos).rev() {
            self.bits[i + offset] |= other_arr[i];
        }
    }

    /// this = this XOR other
    pub fn xor(&mut self, other: &FixedBitSet) {
        self.xor_impl(&other.bits, other.num_words);
    }
    pub fn xor_disi(&self, _iter: impl DocIdSetIterator) {
        // not used in Java Lucene, so we did not impl it
        todo!()
    }
    fn xor_impl(&mut self, other_bits: &[i64], other_num_words: usize) {
        debug_assert!(
            other_num_words <= self.num_words,
            "num_words = {} other_num_words = {}",
            self.num_words,
            other_num_words
        );
        let pos = std::cmp::min(self.num_words, other_num_words);
        for i in (0..pos).rev() {
            self.bits[i] ^= other_bits[i];
        }
    }

    pub fn intersects(&self, other: &FixedBitSet) -> bool {
        // Depends on the ghost bits being clear!
        let pos = std::cmp::min(self.num_words, other.num_words);
        for i in (0..pos).rev() {
            if self.bits[i] != other.bits[i] {
                return true;
            }
        }
        false
    }

    /// this = this AND other
    pub fn and(&mut self, other: &FixedBitSet) {
        self.and_self(&other.bits, other.num_words);
    }

    pub fn and_self(&mut self, other_arr: &[i64], other_num_words: usize) {
        let pos = std::cmp::min(self.num_words, other_num_words);
        for i in (0..pos).rev() {
            self.bits[i] &= other_arr[i];
        }

        if self.num_words > other_num_words {
            for i in other_num_words..self.num_words {
                self.bits[i] = 0;
            }
        }
    }

    pub fn and_not_iter(&mut self, iter: &mut impl DocIdSetIterator) -> Result<()> {
        let mut doc = iter.next_doc()?;
        while doc != NO_MORE_DOCS {
            self.clear_with_index(doc.try_convert()?);
            doc = iter.next_doc()?;
        }
        Ok(())
    }

    /// this = this AND NOT other
    pub fn and_not_fixed_bit_set(&mut self, other: &FixedBitSet) {
        self.and_not_impl(0, &other.bits, other.num_words)
    }

    fn and_not_offset(&mut self, other_offset_words: usize, other: &FixedBitSet) {
        self.and_not_impl(other_offset_words, &other.bits, other.num_words);
    }

    fn and_not_impl(
        &mut self,
        other_offset_words: usize,
        other_arr: &[i64],
        other_num_words: usize,
    ) {
        let pos = std::cmp::min(self.num_words - other_offset_words, other_num_words);
        let offset = other_offset_words;
        for i in (0..pos).rev() {
            self.bits[i + offset] &= !other_arr[i];
        }
    }

    /// Flips a range of bits.
    ///
    /// # Arguments
    /// * `start_index` - The lower index.
    /// * `end_index` - One-past the last bit to flip.
    pub fn flip_range(&mut self, start_index: usize, end_index: usize) {
        debug_assert!(start_index < self.num_bits);
        debug_assert!(end_index <= self.num_bits);
        if end_index <= start_index {
            return;
        }
        let start_word = start_index >> 6;
        let end_word = (end_index - 1) >> 6;

        let start_mask = -1_i64 << (start_index % 64);
        let shift: u32 = ((0usize).wrapping_sub(end_index) & 63) as u32;
        let end_mask: u64 = u64::MAX >> shift;
        if start_word == end_word {
            self.bits[start_word] ^= start_mask & end_mask as i64;
            return;
        }

        self.bits[start_word] ^= start_mask;

        for i in start_word + 1..end_word {
            self.bits[i] = !self.bits[i];
        }

        self.bits[end_word] ^= end_mask as i64;
    }

    /// Flip the bit at the provided index.
    pub fn flip(&mut self, index: usize) {
        debug_assert!(
            index < self.num_bits,
            "index = {}, num_bits = {}",
            index,
            self.num_bits
        );
        let word_num = index >> 6;
        let bit_mask = 1_i64 << (index % 64);
        self.bits[word_num] ^= bit_mask;
    }

    /// Sets a range of bits.
    ///
    /// # Arguments
    /// * `start_index` - The lower index.
    /// * `end_index` - One-past the last bit to set.
    pub fn set_with_range(&mut self, start_index: usize, end_index: usize) {
        debug_assert!(
            start_index < self.num_bits,
            "start_index = {start_index}, num_bits = {end_index}"
        );
        debug_assert!(
            end_index <= self.num_bits,
            "end_index = {end_index}, num_bits = {start_index}"
        );
        if end_index <= start_index {
            return;
        }

        let start_word = start_index >> 6;
        let end_word = (end_index - 1) >> 6;

        let start_mask = !0u64 << (start_index % 64);
        let shift: u32 = ((0usize).wrapping_sub(end_index) & 63) as u32;
        let end_mask: u64 = u64::MAX >> shift;

        if start_word == end_word {
            self.bits[start_word] |= start_mask as i64 & end_mask as i64;
            return;
        }

        self.bits[start_word] |= start_mask as i64;
        for i in (start_word + 1)..end_word {
            self.bits[i] = -1_i64;
        }
        self.bits[end_word] |= end_mask as i64;
    }
    fn next_set_bit_impl(&self, start: usize, upper_bound: usize) -> usize {
        // Depends on the ghost bits being clear!
        debug_assert!(
            start < self.num_bits,
            "index = {}, num_bits = {}",
            start,
            self.num_bits
        );
        debug_assert!(
            start < upper_bound,
            "index = {start}, upper_bound= {upper_bound}"
        );
        debug_assert!(
            upper_bound <= self.num_bits,
            "upper_bound = {}, num_bits = {}",
            upper_bound,
            self.num_bits
        );
        let mut i = start >> 6;
        let mut word = self.bits[i] >> (start % 64); //skip all the bits to the right of index

        if word != 0 {
            return start + word.trailing_zeros() as usize;
        }

        let limit = if upper_bound == self.num_bits {
            self.num_words
        } else {
            Self::bits2words(upper_bound)
        };
        i += 1;
        while i < limit {
            word = self.bits[i];
            if word != 0 {
                return (i << 6) + word.trailing_zeros() as usize;
            }
            i += 1;
        }
        NO_MORE_DOCS as usize
    }

    /// Converts this instance to a read-only [`Bits`].
    /// This is useful in cases where this [`FixedBitSet`]
    /// is returned as a [`Bits`] instance, to ensure that consumers cannot
    /// get write access by casting to a [`FixedBitSet`].
    pub fn to_read_only_bits(self) -> FixedBit {
        FixedBit(self)
    }
}

impl Bits for FixedBitSet {
    fn get(&self, index: usize) -> Result<bool> {
        debug_assert!(
            index < self.num_bits,
            "index = {}, num_bits = {}",
            index,
            self.num_bits
        );
        let i = index >> 6;
        // signed shift will keep a negative index and force an
        // array-index-out-of-bounds-exception, removing the need for an
        // explicit check.
        let bit_mask = 1_i64 << (index % 64);
        Ok((bit_mask & self.bits[i]) != 0)
    }

    fn length(&self) -> usize {
        self.num_bits
    }

    fn copy_of(&self) -> Result<FixedBitSet> {
        Ok(self.clone())
    }
}

impl Accountable for FixedBitSet {
    fn ram_bytes_used(&self) -> Result<i64> {
        // TODO
        Ok(0)
    }
}

impl BitSet for FixedBitSet {
    fn set(&mut self, i: usize) {
        debug_assert!(
            i < self.num_bits,
            "index = {}, num_bits = {}",
            i,
            self.num_bits
        );
        let word_num = i >> 6;
        let bit_mask = 1_i64 << (i % 64);
        self.bits[word_num] |= bit_mask;
    }

    fn get_and_set(&mut self, i: usize) -> bool {
        debug_assert!(
            i < self.num_bits,
            "index = {}, num_bits = {}",
            i,
            self.num_bits
        );
        let word_num = i >> 6;
        let bit_mask = 1_i64 << (i % 64);
        let val = (self.bits[word_num] & bit_mask) != 0;
        self.bits[word_num] |= bit_mask;
        val
    }

    fn clear_with_index(&mut self, i: usize) {
        debug_assert!(
            i < self.num_bits,
            "index = {}, num_bits = {}",
            i,
            self.num_bits
        );
        let word_num = i >> 6;
        let bit_mask = 1_i64 << (i % 64);
        self.bits[word_num] &= !bit_mask;
    }

    fn clear_range(&mut self, start_index: usize, end_index: usize) {
        debug_assert!(
            start_index < self.num_bits,
            "start_index = {}, num_bits = {}",
            start_index,
            self.num_bits
        );
        debug_assert!(
            end_index <= self.num_bits,
            "end_index = {}, num_bits = {}",
            end_index,
            self.num_bits
        );
        if end_index <= start_index {
            return;
        }
        let start_word = start_index >> 6;
        let end_word = (end_index - 1) >> 6;

        let mut start_mask = u64::MAX << (start_index % 64);
        let shift: u32 = ((0usize).wrapping_sub(end_index) & 63) as u32;
        let mut end_mask: u64 = u64::MAX >> shift;

        start_mask = !start_mask;
        end_mask = !end_mask;
        if start_word == end_word {
            self.bits[start_word] &= start_mask as i64 | end_mask as i64;
            return;
        }

        self.bits[start_word] &= start_mask as i64;
        for i in (start_word + 1)..end_word {
            self.bits[i] = 0;
        }
        self.bits[end_word] &= end_mask as i64
    }

    /// Returns the number of set bits.
    ///
    /// # Note
    /// This visits every `u64` in the backing bits array, and the result is not
    /// internally cached.
    fn cardinality(&self) -> usize {
        // Depends on the ghost bits being clear!
        let mut tot = 0;
        for i in 0..self.num_words {
            tot += self.bits[i].count_ones() as usize;
        }

        tot
    }

    fn approximate_cardinality(&self) -> usize {
        // Naive sampling: compute the number of bits that are set on the first
        // 16 longs every 1024 longs and scale the result by 1024/16.
        // This computes the pop count on ranges instead of single longs in
        // order to take advantage of vectorization.
        let range_length = 16;
        let interval = 1024;

        if self.num_words <= interval {
            return self.cardinality();
        }

        let mut pop_count = 0;
        let mut max_word = 0;
        let num = self.num_words;
        while max_word + interval < num {
            for i in 0..range_length {
                pop_count += self.bits[max_word + i].count_ones() as usize;
            }
            max_word += interval;
        }
        pop_count *= (interval / range_length) * self.num_words / max_word;

        pop_count
    }

    fn prev_set_bit(&self, index: usize) -> Option<usize> {
        debug_assert!(
            index < self.num_bits,
            "index = {}, num_bits = {}",
            index,
            self.num_bits
        );
        let i = index >> 6;
        let sub_index = index & 0x3f; //  index within the word

        let mut word = self.bits[i] << (63 - (sub_index % 64));

        if word != 0 {
            return Option::from((i << 6) + sub_index - word.leading_zeros() as usize);
        }
        let mut i: i32 = i as i32;
        i -= 1;

        while i >= 0 {
            word = self.bits[i as usize];
            if word != 0 {
                return Option::from(((i as usize) << 6) + 63 - word.leading_zeros() as usize);
            }
            i -= 1;
        }
        None
    }

    fn next_set_bit(&self, index: usize) -> usize {
        self.next_set_bit_range(index, self.num_bits)
    }

    /// Returns the next set a bit in the specified range, but treats
    /// `upper_bound` as a best-effort hint rather than a hard requirement.
    /// Note that this may return a result that is greater than or equal
    /// to `upper_bound` in some cases, so callers must add their own check if
    /// `upper_bound` is a hard requirement.
    fn next_set_bit_range(&self, start: usize, end: usize) -> usize {
        let res = self.next_set_bit_impl(start, end);
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
        loop {
            let doc = iter.next_doc()?;
            if doc == NO_MORE_DOCS {
                break;
            }
            self.set(doc.try_convert()?);
        }
        Ok(())
    }

    fn ensure_capacity(&mut self, num_bits: usize) {
        if num_bits < self.num_bits {
        } else {
            let num_words = Self::bits2words(num_bits);
            let length = self.bits.len();
            if num_words >= length {
                ArrayUtil::grow_with_len(&mut self.bits, num_words + 1);
            }
            debug_assert!(self.bits.len() <= i32::MAX as usize);
            self.num_bits = (self.bits.len()) << 6;
            self.num_words = Self::bits2words(self.num_bits);
        }
    }
}
/// Immutable of FixedBitSet.
#[derive(Clone, Default)]
pub struct FixedBit(FixedBitSet);
impl Bits for FixedBit {
    fn get(&self, index: usize) -> Result<bool> {
        self.0.get(index)
    }

    fn length(&self) -> usize {
        self.0.length()
    }

    fn copy_of(&self) -> Result<FixedBitSet> {
        self.0.copy_of()
    }
}

#[cfg(test)]
mod tests {
    use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
    use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
    use crate::core::util::TryIntoInt;
    use crate::core::util::bit_set::BitSet;
    use crate::core::util::bit_set_iterator::BitSetIterator;
    use crate::core::util::bits::Bits;
    use crate::core::util::doc_base_bit_set_iterator::DocBaseBitSetIterator;
    use crate::core::util::error::lucene_error::Result;
    use crate::core::util::fixed_bit_set::FixedBitSet;
    use crate::core::util::int_array_doc_id_set::IntArrayDocIdSetIterator;
    use crate::core::util::sparse_fixed_bit_set::SparseFixedBitSet;
    use crate::test::util::base_bit_set_test_case::{
        BaseBitSetTestCase, BaseBitSetTestCaseSupperImpl, RustUtilBitSet,
    };
    use crate::test::util::id_set_common::{clear_range, flip_bit, flip_bit_range, set_range};
    use crate::test::util::lucene_test_case::lucene_test_case_util::{is_night_mode, random};
    use rand::Rng;
    use std::hash::{DefaultHasher, Hash, Hasher};
    use std::rc::Rc;

    struct TestFixedBitSet;

    impl BaseBitSetTestCase for TestFixedBitSet {
        fn copy_of(
            &self,
            bs: &RustUtilBitSet,
            length: usize,
        ) -> (impl BitSet, Option<SparseFixedBitSet>) {
            let mut set = FixedBitSet::new(length);
            let mut doc = bs.next_set_bit(0);
            while doc != NO_MORE_DOCS as usize {
                set.set(doc);
                if doc + 1 > length {
                    doc = NO_MORE_DOCS as usize;
                } else {
                    doc = bs.next_set_bit(doc + 1);
                }
            }
            (set, None)
        }

        fn assert_equals(
            &self,
            set1: &RustUtilBitSet,
            set2: &impl BitSet,
            max_doc: usize,
            sfbs: &Option<SparseFixedBitSet>,
        ) {
            BaseBitSetTestCaseSupperImpl::assert_equals(self, set1, set2, max_doc, sfbs);
        }

        fn test_prev_set_bit<R: Rng + ?Sized>(&mut self, random: &mut R) {
            check_prev_set_bit_array(random, vec![], 0);
            check_prev_set_bit_array(random, vec![0], 1);
            check_prev_set_bit_array(random, vec![0, 2], 3);
        }
    }

    impl BaseBitSetTestCaseSupperImpl for TestFixedBitSet {}

    #[test]
    fn test_cardinality() {
        let mut random = random();
        let mut fbs = TestFixedBitSet;
        fbs.test_cardinality(&mut random);
    }
    #[test]
    fn test_prev_set_bit() {
        let mut random = random();
        let mut fbs = TestFixedBitSet;
        fbs.test_prev_set_bit(&mut random);
    }
    #[test]
    fn test_next_set_bit() {
        let mut random = random();
        let mut fbs = TestFixedBitSet;
        fbs.test_next_set_bit(&mut random);
    }
    #[test]
    fn test_next_set_bit_in_range() {
        let mut random = random();
        let mut fbs = TestFixedBitSet;
        fbs.test_next_set_bit_in_range(&mut random);
    }
    #[test]
    fn test_set() {
        let mut random = random();
        let fbs = TestFixedBitSet;
        fbs.test_set(&mut random);
    }
    #[test]
    fn test_get_and_set() {
        let mut random = random();
        let fbs = TestFixedBitSet;
        fbs.test_get_and_set(&mut random);
    }
    #[test]
    fn test_clear() {
        let mut random = random();
        let mut fbs = TestFixedBitSet;
        fbs.test_clear(&mut random);
    }
    #[test]
    fn test_clear_range() {
        let mut random = random();
        let fbs = TestFixedBitSet;
        fbs.test_clear_range(&mut random);
    }
    #[test]
    fn test_clear_all() {
        let mut random = random();
        let fbs = TestFixedBitSet;
        fbs.test_clear_all(&mut random);
    }
    #[test]
    fn test_or_sparse() {
        let mut random = random();
        let mut fbs = TestFixedBitSet;
        fbs.test_or_sparse(&mut random);
    }
    #[test]
    fn test_or_dense() {
        let mut random = random();
        let mut fbs = TestFixedBitSet;
        fbs.test_or_dense(&mut random);
    }
    #[test]
    fn test_or_random() {
        let mut random = random();
        let mut fbs = TestFixedBitSet;
        fbs.test_or_random(&mut random);
    }

    #[test]
    fn test_approximate_cardinality() {
        // The approximate cardinality works in such a way that it should be
        // pretty accurate on a bitset whose bits are uniformly
        // distributed.
        let mut random = random();
        let mut set = FixedBitSet::new(random.random_range(100000..=200000));
        let first = random.random_range(0..=10);
        let interval = random.random_range(10..=20);
        let mut i = first;
        while i < set.length() {
            set.set(i);
            i += interval;
        }
        let cardinality = set.cardinality();
        assert!(cardinality.abs_diff(set.approximate_cardinality()) <= (cardinality / 20))
    }

    fn do_get(a: &bit_set::BitSet, b: &FixedBitSet) {
        assert_eq!(a.len(), b.cardinality());
        let max = b.length();
        for i in 0..max {
            assert_eq!(a.contains(i), b.get(i).unwrap());
        }
    }

    fn do_next_set_bit(a: &bit_set::BitSet, b: &FixedBitSet) {
        assert_eq!(a.len(), b.cardinality());
        let mut bb = 0;
        loop {
            bb = b.next_set_bit(bb);

            if bb == NO_MORE_DOCS as usize {
                assert!(!a.contains(bb));
                break;
            }
            assert!(a.contains(bb));
            bb += 1;
            if bb > b.length() - 1 {
                assert!(!a.contains(bb));
                break;
            }
        }

        let iter = a.iter();
        for index in iter {
            assert_eq!(index, b.next_set_bit(index));
        }
    }

    fn do_prev_set_bit(a: &bit_set::BitSet, b: &FixedBitSet) {
        assert_eq!(a.len(), b.cardinality());
        let mut bb = b.length().checked_sub(1);
        let mut count = 0;
        let mut iter: Vec<_> = a.iter().collect();
        iter.reverse();
        // check set a bit in BitSet should be in FixedBitSet
        for index in iter {
            bb = b.prev_set_bit(index);
            assert_eq!(*bb.as_ref().unwrap(), index);
        }
        if let Some(bb) = bb {
            // bb is the last match value, so prev_set_bit(bb - 1) should return None
            if bb > 0 {
                assert_eq!(b.prev_set_bit(bb - 1), None);
            }
        }

        bb = if b.length() < 1 {
            None
        } else {
            Option::from(b.length() - 1)
        };

        if bb.is_none() {
            assert_eq!(a.iter().count(), 0);
            return;
        }

        loop {
            bb = b.prev_set_bit(*bb.as_ref().unwrap());
            if bb.is_none() {
                break;
            }
            count += 1;
            assert!(a.contains(*bb.as_ref().unwrap()));
            if *bb.as_ref().unwrap() == 0 {
                break;
            }
            bb = bb.map(|x| x - 1);
        }
        assert_eq!(b.cardinality(), count);
    }

    fn do_iterate<R: Rng + ?Sized>(
        random: &mut R,
        a: &bit_set::BitSet,
        b: FixedBitSet,
    ) -> Result<FixedBitSet> {
        assert_eq!(a.len(), b.cardinality());
        let mut iterator = BitSetIterator::new(b, 0)?;
        let iter = a.iter();
        for index in iter {
            let bb = if random.random_bool(0.5) {
                iterator.next_doc()?
            } else {
                iterator.advance(index as i32)?
            };
            assert_eq!(index, bb as usize);
        }
        assert_eq!(iterator.next_doc()?, NO_MORE_DOCS);
        Ok(iterator.bits)
    }

    fn do_random_sets<R: Rng + ?Sized>(random: &mut R, iter: i32) -> Result<()> {
        // let max_size = random.random_range(1200..=i32::MAX);
        let max_size = random.random_range(1200..=100000);
        let mut a0: bit_set::BitSet = Default::default();
        let mut b0: FixedBitSet = Default::default();
        let mut flag = 0;
        for _i in 0..iter {
            let sz = random.random_range(2..max_size);
            let mut a = bit_set::BitSet::with_capacity(sz);
            let mut b = FixedBitSet::new(sz);
            let n_oper = random.random_range(0..sz);
            for _j in 0..n_oper {
                let mut idx = random.random_range(0..sz);
                a.insert(idx);
                b.set(idx);

                idx = random.random_range(0..sz);
                a.remove(idx);
                b.clear_with_index(idx);

                idx = random.random_range(0..sz);
                flip_bit_range(&mut a, idx, idx + 1);
                b.flip_range(idx, idx + 1);

                idx = random.random_range(0..sz);
                flip_bit(&mut a, idx);
                b.flip(idx);

                let val2 = b.get(idx)?;
                let val = b.get_and_set(idx);
                assert_eq!(val2, val);
                assert!(b.get(idx)?);

                if !val {
                    b.clear_with_index(idx);
                }
                assert_eq!(b.get(idx)?, val);
            }

            // test that the various ways of accessing the bits are equivalent
            do_get(&a, &b);

            // test ranges, including possible extension
            let mut from_index;
            let mut to_index;
            from_index = random.random_range(0..(sz / 2));
            to_index = from_index + random.random_range(0..(sz - from_index));
            let mut aa = a.clone();
            flip_bit_range(&mut aa, from_index, to_index);
            let mut bb = b.clone();
            bb.flip_range(from_index, to_index);

            do_iterate(random, &aa, bb)?; //  a problem here is from flip or doIterate

            from_index = random.random_range(0..(sz / 2));
            to_index = from_index + random.random_range(0..(sz - from_index));
            aa.clone_from(&a);
            clear_range(&mut aa, from_index, to_index);
            bb = b.clone();
            bb.clear_range(from_index, to_index);

            do_next_set_bit(&aa, &bb); // a problem here is from clear() or nextSetBit

            do_prev_set_bit(&aa, &bb);

            from_index = random.random_range(0..(sz / 2));
            to_index = from_index + random.random_range(0..(sz - from_index));
            aa.clone_from(&a);
            set_range(&mut aa, from_index, to_index);
            bb = b.clone();
            bb.set_with_range(from_index, to_index);

            do_next_set_bit(&aa, &bb); // a problem here is from set() or nextSetBit

            do_prev_set_bit(&aa, &bb);

            if flag == 1 && b0.length() <= b.length() {
                assert_eq!(a.len(), b.cardinality());

                let mut a_and = a.clone();
                a_and.intersect_with(&a0);
                let mut a_or = a.clone();
                a_or.union_with(&a0);
                let mut a_xor = a.clone();
                a_xor.symmetric_difference_with(&a0);
                let mut a_andn = a.clone();
                a_andn.difference_with(&a0);

                let mut b_and = b.clone();
                assert_eq!(b, b_and);
                b_and.and(&b0);
                let mut b_or = b.clone();
                b_or.or(&b0);
                let mut b_xor = b.clone();
                b_xor.xor(&b0);
                let mut b_andn = b.clone();
                b_andn.and_not_fixed_bit_set(&b0);

                assert_eq!(a0.len(), b0.cardinality());
                assert_eq!(a_or.len(), b_or.cardinality());

                assert_eq!(a_and.len(), b_and.cardinality());
                assert_eq!(a_or.len(), b_or.cardinality());
                assert_eq!(a_andn.len(), b_andn.cardinality());
                assert_eq!(a_xor.len(), b_xor.cardinality());

                do_iterate(random, &a_and, b_and)?;
                do_iterate(random, &a_xor, b_xor)?;
                do_iterate(random, &a_or, b_or)?;
                do_iterate(random, &a_andn, b_andn)?;

                a0 = a;
                b0 = b;
            } else {
                flag = 1;
                a0 = a;
                b0 = b;
            }
        }
        Ok(())
    }

    #[test]
    fn test_small() -> Result<()> {
        let mut random = random();
        let iters = if is_night_mode() {
            random.random_range(1000..100000)
        } else {
            100
        };
        do_random_sets(&mut random, iters)?;
        Ok(())
    }

    #[test]
    fn test_equals() {
        // This test can't handle numBits==0:
        let mut random = random();
        let num_bits = random.random_range(0..2000) + 1;
        let mut b1 = FixedBitSet::new(num_bits);
        let mut b2 = FixedBitSet::new(num_bits);
        assert!(b1.eq(&b2));
        assert!(b2.eq(&b1));
        for _i in 0..random.random_range(1000..5000) {
            let idx = random.random_range(0..num_bits);
            if !b1.get(idx).unwrap() {
                b1.set(idx);
                assert!(!b1.eq(&b2));
                assert!(!b2.eq(&b1));
                b2.set(idx);
                assert!(b1.eq(&b2));
                assert!(b2.eq(&b1));
            }
        }
    }

    #[test]
    fn test_hash_code_equals() {
        let mut random = random();

        let num_bits = random.random_range(0..2000) + 1;
        let mut b1 = FixedBitSet::new(num_bits);
        let mut b2 = FixedBitSet::new(num_bits);
        for _i in 0..random.random_range(1000..5000) {
            let idx = random.random_range(0..num_bits);
            if !b1.get(idx).unwrap() {
                b1.set(idx);
                assert!(!b1.eq(&b2));
                assert_ne!(calculate_hash(&b1), calculate_hash(&b2));
                b2.set(idx);
                assert!(b1.eq(&b2));
                assert_eq!(calculate_hash(&b1), calculate_hash(&b2));
            }
        }
    }

    fn calculate_hash(a: &FixedBitSet) -> u64 {
        let mut hasher = DefaultHasher::new();
        a.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn test_small_bitsets() {
        // Make sure size 0-10 bit sets are OK:
        for num_bits in 0..10 {
            let mut b1 = FixedBitSet::new(num_bits);
            let b2 = FixedBitSet::new(num_bits);
            assert!(b1.eq(&b2));
            assert_eq!(calculate_hash(&b1), calculate_hash(&b2));
            assert_eq!(0, b1.cardinality());
            if num_bits > 0 {
                b1.set_with_range(0, num_bits);
                assert_eq!(num_bits, b1.cardinality());
                b1.flip_range(0, num_bits);
                assert_eq!(0, b1.cardinality());
            }
        }
    }

    fn make_fixed_bitset<R: Rng + ?Sized>(
        random: &mut R,
        a: &[usize],
        num_bits: usize,
    ) -> Result<FixedBitSet> {
        let mut bs: FixedBitSet;
        if random.random_bool(0.5) {
            let bits_2_words = FixedBitSet::bits2words(num_bits);
            let mut words: Vec<i64> = Vec::with_capacity(bits_2_words);
            words.resize(num_bits, 0);
            bs = FixedBitSet::with_capacity(words, num_bits)?
        } else {
            bs = FixedBitSet::new(num_bits)
        }
        for e in a {
            bs.set(*e);
        }
        Ok(bs)
    }

    fn make_bitset(a: &[usize]) -> bit_set::BitSet {
        let mut bs: bit_set::BitSet = bit_set::BitSet::with_capacity(a.len());
        for x in a {
            bs.insert(*x);
        }
        bs
    }

    fn check_prev_set_bit_array<R: Rng + ?Sized>(random: &mut R, a: Vec<usize>, num_bits: usize) {
        let obs = make_fixed_bitset(random, &a, num_bits).unwrap();
        let bs = make_bitset(&a);
        do_prev_set_bit(&bs, &obs);
    }

    fn check_next_set_bit_array<R: Rng + ?Sized>(random: &mut R, a: Vec<usize>, num_bits: usize) {
        let obs = make_fixed_bitset(random, &a, num_bits).unwrap();
        let bs = make_bitset(&a);
        do_next_set_bit(&bs, &obs);
    }

    #[test]
    fn test_next_bitset() {
        let mut random = random();
        let capacity = random.random_range(0..1000);
        let mut set_bits = Vec::with_capacity(capacity);
        for _i in 0..capacity {
            set_bits.push(random.random_range(0..capacity));
        }
        let num_bits = set_bits.len() + random.random_range(0..10);
        check_next_set_bit_array(&mut random, set_bits, num_bits);
        check_next_set_bit_array(&mut random, vec![], num_bits);
    }

    #[test]
    fn test_ensure_capacity() -> Result<()> {
        let mut bits = FixedBitSet::new(5);
        bits.set(1);
        bits.set(4);
        bits.ensure_capacity(8);
        let mut new_bits = bits.clone();
        assert!(bits.get(1)?);
        assert!(bits.get(4)?);
        bits.clear_with_index(1);
        assert!(!bits.get(1)?);
        assert!(new_bits.get(1)?);

        new_bits.set(1);
        let length = bits.length();
        new_bits.ensure_capacity(length - 2);
        assert!(new_bits.get(1)?);

        new_bits.set(1);
        new_bits.ensure_capacity(72);
        assert!(new_bits.get(1)?);
        assert!(new_bits.get(4)?);
        new_bits.clear_with_index(1);
        // we grew the long[], so it's not shared
        assert!(!bits.get(1)?);
        assert!(!new_bits.get(1)?);
        Ok(())
    }

    #[test]
    fn test_bits2words() {
        assert_eq!(0, FixedBitSet::bits2words(0));
        assert_eq!(1, FixedBitSet::bits2words(1));

        assert_eq!(1, FixedBitSet::bits2words(64));
        assert_eq!(2, FixedBitSet::bits2words(65));

        assert_eq!(2, FixedBitSet::bits2words(128));
        assert_eq!(3, FixedBitSet::bits2words(129));

        assert_eq!(1024, FixedBitSet::bits2words(65536));
        assert_eq!(1025, FixedBitSet::bits2words(65537));

        assert_eq!(1 << (31 - 6), FixedBitSet::bits2words(i32::MAX as usize));
    }

    fn make_int_array<R: Rng + ?Sized>(
        random: &mut R,
        count: usize,
        min: usize,
        max: usize,
    ) -> Vec<usize> {
        let mut rv = vec![0; count];
        for _i in 0..count {
            rv.push(random.random_range(min..=max));
        }
        rv
    }

    #[test]
    fn test_intersection_count() {
        let mut random = random();

        let num_bits1 = random.random_range(1000..=2000);
        let num_bits2 = random.random_range(1000..=2000);

        let count1 = random.random_range(0..=num_bits1 - 1);
        let count2 = random.random_range(0..=num_bits2 - 1);

        let bits1 = make_int_array(&mut random, count1, 0, num_bits1 - 1);
        let bits2 = make_int_array(&mut random, count2, 0, num_bits2 - 1);

        let fixed_bit_set1 = make_fixed_bitset(&mut random, &bits1, num_bits1);
        let fixed_bit_set2 = make_fixed_bitset(&mut random, &bits2, num_bits2);
        // If ghost bits are present, these may fail too, but that's not what we
        // want to demonstrate here
        // assertTrue(fixedBitSet1.cardinality() <= bits1.length);
        // assertTrue(fixedBitSet2.cardinality() <= bits2.length);
        let intersection_count =
            FixedBitSet::intersection_count(fixed_bit_set1.unwrap(), fixed_bit_set2.unwrap());

        let mut bit_set1 = make_bitset(&bits1);
        let bit_set2 = make_bitset(&bits2);
        // If ghost bits are present, these may fail too, but that's not what we
        // want to demonstrate here
        // assertEquals(bitSet1.cardinality(), fixedBitSet1.cardinality());
        // assertEquals(bitSet2.cardinality(), fixedBitSet2.cardinality());

        bit_set1.intersect_with(&bit_set2);
        assert_eq!(bit_set1.len(), intersection_count as usize);
    }

    #[test]
    fn test_and_not() -> Result<()> {
        let mut random = random();

        let num_bits2 = random.random_range(1000..=2000);
        let num_bits1 = random.random_range(1000..=num_bits2);

        let count1 = random.random_range(0..=num_bits1 - 1);
        let count2 = random.random_range(0..=num_bits2 - 1);

        let min = random.random_range(0..=(num_bits1 - 1));
        let off_set_word1 = min >> 6;
        let offset1 = off_set_word1 >> 6;
        let bits1 = make_int_array(&mut random, count1, min, num_bits1 - 1);
        let bits2 = make_int_array(&mut random, count2, 0, num_bits2 - 1);

        let bitset1 = make_bitset(&bits1);
        let mut bitset2 = make_bitset(&bits2);
        bitset2.difference_with(&bitset1);

        {
            // test BitSetIterator
            let mut fixed_bit_set2 = make_fixed_bitset(&mut random, &bits2, num_bits2)?;
            let fixed_bit = make_fixed_bitset(&mut random, &bits1, num_bits1)?;
            let mut disi = BitSetIterator::new(fixed_bit, count1 as i64)?;
            fixed_bit_set2.and_not_iter(&mut disi)?;
            do_get(&bitset2, &fixed_bit_set2);
        }
        {
            // test DocBaseBitSetIterator
            let mut fixed_bit_set2 = make_fixed_bitset(&mut random, &bits2, num_bits2)?;
            let offset_bits: Vec<usize> = bits1.iter().map(|&i| i - offset1).collect();
            let fixed_bit = make_fixed_bitset(&mut random, &offset_bits, num_bits1 - offset1)?;
            let mut disi = DocBaseBitSetIterator::new(fixed_bit, count1 as i64, offset1)?;
            fixed_bit_set2.and_not_iter(&mut disi)?;
            do_get(&bitset2, &fixed_bit_set2);
        }
        {
            // test other
            let mut fixed_bit_set2 = make_fixed_bitset(&mut random, &bits2, num_bits2)?;
            let mut sorted: Vec<i32> = bits1
                .iter()
                .map(|&x| {
                    debug_assert!(x <= i32::MAX as usize);
                    x as i32
                })
                .collect();
            sorted.push(0);
            sorted[bits1.len()] = NO_MORE_DOCS;
            let mut disi = IntArrayDocIdSetIterator::new(Rc::new(sorted), count1.try_convert()?);
            fixed_bit_set2.and_not_iter(&mut disi)?;
            do_get(&bitset2, &fixed_bit_set2);
        }
        Ok(())
    }

    // Demonstrates that the presence of ghost bits in the last used word can
    // cause spurious failures
    #[test]
    fn test_union_count() -> Result<()> {
        let mut random = random();
        let num_bits1 = random.random_range(1000..=2000);
        let num_bits2 = random.random_range(1000..=2000);

        let count1 = random.random_range(0..=num_bits1 - 1);
        let count2 = random.random_range(0..=num_bits2 - 1);

        let bits1 = make_int_array(&mut random, count1, 0, num_bits1 - 1);
        let bits2 = make_int_array(&mut random, count2, 0, num_bits2 - 1);

        let fixed_bit_set1 = make_fixed_bitset(&mut random, &bits1, num_bits1)?;
        let fixed_bit_set2 = make_fixed_bitset(&mut random, &bits2, num_bits2)?;

        let union_count = FixedBitSet::union_count(&fixed_bit_set1, &fixed_bit_set2);

        let mut bit_set1 = make_bitset(&bits1);
        let bit_set2 = make_bitset(&bits2);
        bit_set1.union_with(&bit_set2);

        assert_eq!(bit_set1.len(), union_count as usize);
        Ok(())
    }

    #[test]
    fn test_and_not_count() -> Result<()> {
        let mut random = random();

        let num_bits1 = random.random_range(1000..=2000);
        let num_bits2 = random.random_range(1000..=2000);

        let count1 = random.random_range(0..=num_bits1 - 1);
        let count2 = random.random_range(0..=num_bits2 - 1);

        let bits1 = make_int_array(&mut random, count1, 0, num_bits1 - 1);
        let bits2 = make_int_array(&mut random, count2, 0, num_bits2 - 1);

        let fixed_bit_set1 = make_fixed_bitset(&mut random, &bits1, num_bits1)?;
        let fixed_bit_set2 = make_fixed_bitset(&mut random, &bits2, num_bits2)?;

        let and_not_count = FixedBitSet::and_not_count(&fixed_bit_set1, &fixed_bit_set2);

        let mut bit_set1 = make_bitset(&bits1);
        let bit_set2 = make_bitset(&bits2);

        bit_set1.difference_with(&bit_set2);

        assert_eq!(bit_set1.len(), and_not_count as usize);
        Ok(())
    }

    #[test]
    fn test_copy_of() {
        // this test is not required in Rust Lucene
    }

    #[test]
    fn test_as_bits() {
        // this test is not required in Rust Lucene
    }
}
