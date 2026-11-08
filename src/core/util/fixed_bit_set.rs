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
use crate::core::index::index_reader::Identity;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::util::accountable::Accountable;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bit_set::{BitSet, check_unpositioned};
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::ram_usage_estimator::size_of_vec;
use crate::core::util::{HasIdentity, TryIntoInt};
use std::hash::{Hash, Hasher};

/// [`BitSet`] of fixed length (`num_bits`), backed by accessible (`get_bits`)
/// an `i64` slice, accessed with a `usize` index, implementing [`Bits`] and
/// [`DocIdSet`](crate::core::search::doc_id_set). If you need to manage more than
/// 2.1B bits, use [`LongBitSet`](crate::core::util::long_bit_set::LongBitSet).
///
/// # Note
/// This is an internal API.
#[derive(Default, Debug)]
pub struct FixedBitSet {
  // `i64` words holding the bits.
  pub(super) bits: Vec<i64>,
  // The number of bits in use
  num_bits: usize,
  // The exact number of words needed to hold `num_bits` (`<= bits.len()`).
  num_words: usize,
  id: Identity,
}

impl Hash for FixedBitSet {
  fn hash<H>(&self, state: &mut H)
  where
    H: Hasher,
  {
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
    Self::from_validated_parts(self.bits.clone(), self.num_bits, self.num_words)
  }
}

/// If the given [`LongBitSet`](crate::core::util::long_bit_set::LongBitSet) is large
/// enough to hold `num_bits + 1`, returns the given bits, otherwise returns a
/// new [`LongBitSet`](crate::core::util::long_bit_set::LongBitSet) that can hold the
/// requested number of bits.
///
/// # Note
/// The returned bitset reuses the underlying `Vec<i64>` of the given `bits` if
/// possible. Also, calling `length()` on the returned bits may return a value
/// greater than `num_bits`.
impl FixedBitSet {
  /// returns the number of 64-bit words it would take to hold numBits
  pub fn bits2words(num_bits: usize) -> usize {
    num_bits.div_ceil(64)
  }

  /// Returns the popcount or cardinality of the intersection of the two sets.
  /// Neither set is modified.
  pub fn intersection_count<A, B>(a: A, b: B) -> i64
  where
    A: std::borrow::Borrow<FixedBitSet>,
    B: std::borrow::Borrow<FixedBitSet>,
  {
    let a = a.borrow();
    let b = b.borrow();
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
  /// Creates a new [`FixedBitSet`]. The internally allocated `Vec<u64>` array
  /// will be exactly the size needed to accommodate the `num_bits`
  /// specified.
  ///
  /// # Arguments
  /// * `num_bits` - The number of bits needed.
  pub fn new(num_bits: usize) -> FixedBitSet {
    let size: usize = Self::bits2words(num_bits);
    let bits: Vec<i64> = vec![0; size];
    let exact_size = bits.len();
    Self::from_validated_parts(bits, num_bits, exact_size)
  }
  pub(crate) fn with_set_prefix(num_bits: usize, prefix: usize) -> Self {
    assert!(prefix <= num_bits);
    let num_words = Self::bits2words(num_bits);
    let full_words = prefix / 64;
    let mut bits = Vec::with_capacity(num_words);
    bits.resize(full_words, -1);
    let remaining = prefix % 64;
    if remaining != 0 {
      bits.push(((1u64 << remaining) - 1) as i64);
    }
    bits.resize(num_words, 0);
    Self::from_validated_parts(bits, num_bits, num_words)
  }

  /// Creates a new [`FixedBitSet`] using the provided bits as the
  /// backing store. The `stored_bits` array must be large enough to
  /// accommodate the `num_bits` specified, but may be larger. In that
  /// case, the 'extra' or 'ghost' bits must be clear (or they may provoke
  /// spurious side effects).
  ///
  /// # Arguments
  /// * `stored_bits` - The array to use as the backing store (`Vec<i64>`).
  /// * `num_bits` - The number of bits actually needed.
  pub fn with_capacity<B>(stored_bits: B, num_bits: usize) -> Result<FixedBitSet>
  where
    B: Into<Vec<i64>>,
  {
    let stored_bits = stored_bits.into();
    let num_words = Self::bits2words(num_bits);
    if num_words > stored_bits.len() {
      return Err(LuceneError::illegal_argument(format!(
        "The given long array is too small  to hold {num_bits} bits"
      )));
    }
    Ok(Self::from_validated_parts(stored_bits, num_bits, num_words))
  }

  fn from_validated_parts(bits: Vec<i64>, num_bits: usize, num_words: usize) -> Self {
    debug_assert_eq!(num_words, Self::bits2words(num_bits));
    debug_assert!(num_words <= bits.len());
    let bit_set = Self {
      bits,
      num_bits,
      num_words,
      id: Identity::new(),
    };
    debug_assert!(Self::verify_ghost_bits_clear(&bit_set));
    bit_set
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
    if pos == 0 {
      return;
    }
    if let Some(words) = offset
      .checked_add(pos)
      .and_then(|end| self.bits.get_mut(offset..end))
      && let Some(other_words) = other_arr.get(..pos)
    {
      for (word, &other_word) in words.iter_mut().zip(other_words).rev() {
        *word |= other_word;
      }
    } else {
      for i in (0..pos).rev() {
        self.bits[i + offset] |= other_arr[i];
      }
    }
  }

  /// this = this XOR other
  pub fn xor(&mut self, other: &FixedBitSet) {
    self.xor_impl(&other.bits, other.num_words);
  }
  /// Does in-place XOR of the bits provided by the iterator.
  pub fn xor_disi<I>(&mut self, iter: &mut I) -> Result<()>
  where
    I: DocIdSetIterator,
  {
    check_unpositioned(iter)?;
    if let Some(bits) = iter.get_fixed_bit_set() {
      self.xor(bits);
    } else {
      loop {
        let doc: usize = iter.next_doc()?.try_convert()?;
        if doc >= self.num_bits {
          break;
        }
        self.flip(doc);
      }
    }
    Ok(())
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
  /// returns true if the sets have any elements in common
  pub fn intersects(&self, other: &FixedBitSet) -> bool {
    // Depends on the ghost bits being clear!
    fn scan_rest(a: &[i64], b: &[i64]) -> bool {
      if a.len() < 2 {
        return a.iter().zip(b).rev().any(|(&x, &y)| (x & y) != 0);
      }
      let a_chunks = a.rchunks_exact(4);
      let b_chunks = b.rchunks_exact(4);
      let a_tail = a_chunks.remainder();
      let b_tail = b_chunks.remainder();
      for (x, y) in a_chunks.zip(b_chunks) {
        if ((x[3] & y[3]) | (x[2] & y[2]) | (x[1] & y[1]) | (x[0] & y[0])) != 0 {
          return true;
        }
      }
      a_tail.iter().zip(b_tail).rev().any(|(&x, &y)| (x & y) != 0)
    }
    let pos = std::cmp::min(self.num_words, other.num_words);
    if pos == 0 {
      return false;
    }
    let last = pos - 1;
    if (self.bits[last] & other.bits[last]) != 0 {
      return true;
    }
    scan_rest(&self.bits[..last], &other.bits[..last])
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

  pub fn and_not_iter<I>(&mut self, iter: &mut I) -> Result<()>
  where
    I: DocIdSetIterator,
  {
    if let Some(bits) = iter.get_fixed_bit_set() {
      check_unpositioned(iter)?;
      self.and_not_fixed_bit_set(bits);
      return Ok(());
    }
    if let Some((doc_base, bits)) = iter.get_doc_base_fixed_bit_set() {
      check_unpositioned(iter)?;
      self.and_not_offset(doc_base >> 6, bits);
      return Ok(());
    }
    check_unpositioned(iter)?;
    let mut doc = iter.next_doc()?;
    while doc != NO_MORE_DOCS {
      self.clear_with_index(doc.try_convert()?)?;
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

  fn and_not_impl(&mut self, other_offset_words: usize, other_arr: &[i64], other_num_words: usize) {
    let pos = std::cmp::min(
      self.num_words.saturating_sub(other_offset_words),
      other_num_words,
    );
    if pos == 0 {
      return;
    }
    let offset = other_offset_words;
    if let Some((dest, source)) = self
      .bits
      .get_mut(offset..)
      .and_then(|bits| bits.get_mut(..pos))
      .zip(other_arr.get(..pos))
    {
      for (dest, source) in dest.iter_mut().zip(source).rev() {
        *dest &= !*source;
      }
    } else {
      for i in (0..pos).rev() {
        self.bits[i + offset] &= !other_arr[i];
      }
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
    let shift = 0usize.wrapping_sub(end_index) & 63;
    let end_mask = (u64::MAX >> shift) as i64;
    if start_word == end_word {
      self.bits[start_word] ^= start_mask & end_mask;
      return;
    }

    self.bits[start_word] ^= start_mask;

    for i in start_word + 1..end_word {
      self.bits[i] = !self.bits[i];
    }

    self.bits[end_word] ^= end_mask;
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

    let start_mask = -1_i64 << (start_index % 64);
    let shift = 0usize.wrapping_sub(end_index) & 63;
    let end_mask = (u64::MAX >> shift) as i64;

    if start_word == end_word {
      self.bits[start_word] |= start_mask & end_mask;
      return;
    }

    self.bits[start_word] |= start_mask;
    for i in (start_word + 1)..end_word {
      self.bits[i] = -1_i64;
    }
    self.bits[end_word] |= end_mask;
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
    if i < limit {
      word = self.bits[i];
      if word != 0 {
        return (i << 6) + word.trailing_zeros() as usize;
      }
      i += 1;
    }
    let scan_limit = limit.min(self.bits.len());
    for group in self.bits[i.min(scan_limit)..scan_limit].chunks(16) {
      if group.len() < 16 || group.iter().fold(0i64, |all, &word| all | word) != 0 {
        for (j, &word) in group.iter().enumerate() {
          if word != 0 {
            return ((i + j) << 6) + word.trailing_zeros() as usize;
          }
        }
      }
      i += group.len();
    }
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
    FixedBit::new(self)
  }
}

impl HasIdentity for FixedBitSet {
  fn identity(&self) -> &Identity {
    &self.id
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
    // array-index-out-of-bounds-error, removing the need for an
    // explicit check.
    let bit_mask = 1_i64 << (index % 64);
    Ok((bit_mask & self.bits[i]) != 0)
  }

  fn length(&self) -> usize {
    self.num_bits
  }

  fn fixed_bit_set(&self) -> Option<&FixedBitSet> {
    Some(self)
  }

  fn copy_of(&self) -> Result<FixedBitSet> {
    Ok(self.clone())
  }
}

impl Accountable for FixedBitSet {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(size_of_vec(&self.bits))
  }
}

impl BitSet for FixedBitSet {
  fn clear(&mut self) -> Result<()> {
    self.bits.fill(0);
    Ok(())
  }

  fn set(&mut self, i: usize) -> Result<()> {
    debug_assert!(
      i < self.num_bits,
      "index = {}, num_bits = {}",
      i,
      self.num_bits
    );
    let word_num = i >> 6;
    let bit_mask = 1_i64 << (i % 64);
    self.bits[word_num] |= bit_mask;
    Ok(())
  }

  fn get_and_set(&mut self, i: usize) -> Result<bool> {
    debug_assert!(
      i < self.num_bits,
      "index = {}, num_bits = {}",
      i,
      self.num_bits
    );
    let word_num = i >> 6;
    let bit_mask = 1_i64 << (i % 64);
    let word = &mut self.bits[word_num];
    let val = (*word & bit_mask) != 0;
    if !val {
      *word |= bit_mask;
    }
    Ok(val)
  }

  fn clear_with_index(&mut self, i: usize) -> Result<()> {
    debug_assert!(
      i < self.num_bits,
      "index = {}, num_bits = {}",
      i,
      self.num_bits
    );
    let word_num = i >> 6;
    let bit_mask = 1_i64 << (i % 64);
    self.bits[word_num] &= !bit_mask;
    Ok(())
  }

  fn clear_range(&mut self, start_index: usize, end_index: usize) -> Result<()> {
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
      return Ok(());
    }
    let start_word = start_index >> 6;
    let end_word = (end_index - 1) >> 6;

    let mut start_mask = -1_i64 << (start_index % 64);
    let shift = 0usize.wrapping_sub(end_index) & 63;
    let mut end_mask = (u64::MAX >> shift) as i64;

    start_mask = !start_mask;
    end_mask = !end_mask;
    if start_word == end_word {
      self.bits[start_word] &= start_mask | end_mask;
      return Ok(());
    }

    self.bits[start_word] &= start_mask;
    for i in (start_word + 1)..end_word {
      self.bits[i] = 0;
    }
    self.bits[end_word] &= end_mask;
    Ok(())
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
    for i in (0..i).rev() {
      word = self.bits[i];
      if word != 0 {
        return Option::from((i << 6) + 63 - word.leading_zeros() as usize);
      }
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

  fn or<T>(&mut self, iter: &mut T) -> Result<()>
  where
    T: DocIdSetIterator,
  {
    if let Some(bits) = iter.get_fixed_bit_set() {
      check_unpositioned(iter)?;
      self.or(bits);
      return Ok(());
    }
    if let Some((doc_base, bits)) = iter.get_doc_base_fixed_bit_set() {
      check_unpositioned(iter)?;
      self.or_offset(doc_base >> 6, bits);
      return Ok(());
    }
    self.default_or(iter)
  }

  #[inline]
  fn ensure_capacity(&mut self, num_bits: usize) -> Result<()> {
    if num_bits < self.num_bits {
    } else {
      let num_words = Self::bits2words(num_bits);
      let length = self.bits.len();
      if num_words >= length {
        ArrayUtil::grow_with(&mut self.bits, num_words + 1, || 0)?;
      }
      debug_assert!(self.bits.len() <= i32::MAX as usize);
      self.num_bits = (self.bits.len()) << 6;
      self.num_words = Self::bits2words(self.num_bits);
    }
    Ok(())
  }
}
impl crate::core::util::bit_set::BitSetExtensions for FixedBitSet {
  fn as_fixed_bit_set(&self) -> Option<&FixedBitSet> {
    Some(self)
  }
}

/// Immutable of FixedBitSet.
#[derive(Clone)]
pub struct FixedBit {
  pub(crate) fix_bit_set: FixedBitSet,
  id: Identity,
}
impl FixedBit {
  pub fn new(fix_bit_set: FixedBitSet) -> FixedBit {
    FixedBit {
      fix_bit_set,
      id: Identity::new(),
    }
  }
}

impl HasIdentity for FixedBit {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl Bits for FixedBit {
  fn get(&self, index: usize) -> Result<bool> {
    self.fix_bit_set.get(index)
  }

  fn length(&self) -> usize {
    self.fix_bit_set.length()
  }

  fn fixed_bit_set(&self) -> Option<&FixedBitSet> {
    Some(&self.fix_bit_set)
  }

  fn copy_of(&self) -> Result<FixedBitSet> {
    self.fix_bit_set.copy_of()
  }
}
