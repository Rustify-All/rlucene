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
use crate::core::util::bit_set::{BitSet, check_unpositioned};
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::ram_usage_estimator::size_of_slice;
use crate::core::util::{HasIdentity, SliceCopyOps, TryIntoInt};
use smallvec::SmallVec;

// Most newly populated sparse blocks contain only a few non-zero words.
// Count inline storage in the outer Vec, and only spilled capacity separately.
fn block_heap_bytes(block: &SmallVec<[u64; 4]>) -> i64 {
  if block.spilled() {
    block
      .capacity()
      .saturating_mul(size_of::<u64>())
      .min(i64::MAX as usize) as i64
  } else {
    0
  }
}

const MASK_4096: usize = (1 << 12) - 1;

fn block_count(length: usize) -> usize {
  let mut block_count = length >> 12;
  if (block_count << 12) < length {
    block_count += 1;
  }
  debug_assert!((block_count << 12) >= length);
  block_count
}

/// A bit set that only stores `u64` values that have at least one bit set. The
/// way it works is that the space of bits is divided into blocks of 4096 bits,
/// which is 64 `u64`s. Then for each block, we have:
///
/// - A small `u64` buffer which stores the non-zero `u64`s for that block.
/// - A `u64` so that bit `i` being set means that the `i-th` `u64` of the block
///   is present, and its offset in the array of `u64`s is the number of one
///   bits on the right of the `i-th` bit.
///
/// # Note
/// This is an internal API.
#[derive(Default)]
pub struct SparseFixedBitSet {
  indices: Box<[u64]>,
  bits: Box<[SmallVec<[u64; 4]>]>,
  length: usize,
  non_zero_long_count: usize,
  ram_bytes_used: i64,
  id: Identity,
}

impl SparseFixedBitSet {
  pub fn new(length: usize) -> Result<SparseFixedBitSet> {
    if length < 1 {
      return Err(LuceneError::illegal_argument("length needs to be >= 1"));
    }
    let block_count = block_count(length);
    let indices = vec![0; block_count].into_boxed_slice();
    let bits = vec![SmallVec::<[u64; 4]>::new(); block_count].into_boxed_slice();
    let ram_bytes_used = size_of_slice(&indices).saturating_add(size_of_slice(&bits));
    Ok(SparseFixedBitSet {
      indices,
      bits,
      length,
      non_zero_long_count: 0,
      ram_bytes_used,
      id: Identity::new(),
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
  fn insert_block(&mut self, i4096: usize, i64bit: u64, i: usize) {
    self.indices[i4096] = i64bit;
    debug_assert!(self.bits[i4096].is_empty());
    let block = SmallVec::from_elem(1_u64 << (i % 64), 1);
    self.ram_bytes_used = self.ram_bytes_used.saturating_add(block_heap_bytes(&block));
    self.bits[i4096] = block;
    self.non_zero_long_count += 1;
  }
  fn insert_long(&mut self, i4096: usize, i64bit: u64, i: usize, index: u64) {
    self.indices[i4096] |= i64bit;
    // we count the number of bits that are set on the right of i64
    // this gives us the index at which to perform the insertion
    let o = (index & (i64bit - 1)).count_ones() as usize;
    let bit_array = &mut self.bits[i4096];
    if bit_array[bit_array.len() - 1] == 0 {
      // since we only store non-zero longs, if the last value is 0, it
      // means that we already have extra space, make use of
      // it
      let big_array_length = bit_array.len();
      bit_array.copy_within(o..big_array_length - 1, o + 1);
      bit_array[o] = 1_u64 << (i % 64);
    } else {
      let old_bytes = block_heap_bytes(bit_array);
      let new_size = oversize(bit_array.len() + 1);
      let mut new_bit_array = SmallVec::from_elem(0, new_size);
      new_bit_array.copy_from(&bit_array[..o], 0);
      new_bit_array[o] = 1_u64 << (i % 64);
      new_bit_array.copy_from(&bit_array[o..], o + 1);
      let new_bytes = block_heap_bytes(&new_bit_array);
      self.bits[i4096] = new_bit_array;
      self.ram_bytes_used = self
        .ram_bytes_used
        .saturating_sub(old_bytes)
        .saturating_add(new_bytes);
    }
    self.non_zero_long_count += 1;
  }
  fn and(&mut self, i4096: usize, i64: usize, mask: u64) {
    let index = self.indices[i4096];
    if index & (1_u64 << (i64 % 64)) != 0 {
      // offset of the long bits we are interested in in the array
      let o = (index & ((1_u64 << (i64 % 64)) - 1)).count_ones() as usize;
      let bits = self.bits[i4096][o] & mask;
      if bits == 0 {
        self.remove_long(i4096, i64, index, o);
      } else {
        self.bits[i4096][o] = bits;
      }
    }
  }
  fn remove_long(&mut self, i4096: usize, i64: usize, mut index: u64, o: usize) {
    let mask = !(1_u64 << (i64 % 64));
    index &= mask;
    self.indices[i4096] = index;
    if index == 0 {
      let bit_array = std::mem::take(&mut self.bits[i4096]);
      self.ram_bytes_used = self
        .ram_bytes_used
        .saturating_sub(block_heap_bytes(&bit_array));
    } else {
      let length = index.count_ones() as usize;
      let bit_array = &mut self.bits[i4096];
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
    if i4096 < i4096_upper {
      let index = self.indices[i4096];
      if index != 0 {
        let i64 = index.trailing_zeros() as usize;
        return (i4096 << 12) | (i64 << 6) | self.bits[i4096][0].trailing_zeros() as usize;
      }
      i4096 += 1;
    }
    let scan_limit = i4096_upper.min(self.indices.len());
    for group in self.indices[i4096.min(scan_limit)..scan_limit].chunks(16) {
      if group.len() < 16 || group.iter().fold(0u64, |all, &index| all | index) != 0 {
        for (j, &index) in group.iter().enumerate() {
          if index != 0 {
            let block = i4096 + j;
            let i64 = index.trailing_zeros() as usize;
            return (block << 12) | (i64 << 6) | self.bits[block][0].trailing_zeros() as usize;
          }
        }
      }
      i4096 += group.len();
    }
    let mut index;
    while i4096 < i4096_upper {
      index = self.indices[i4096];
      if index != 0 {
        let i64 = index.trailing_zeros() as usize;
        return (i4096 << 12) | (i64 << 6) | self.bits[i4096][0].trailing_zeros() as usize;
      }
      i4096 += 1;
    }
    NO_MORE_DOCS as usize
  }
  /// Return the last document that occurs on or before the provided block
  /// index.
  fn last_doc(&self, mut i4096: Option<usize>) -> Option<usize> {
    while let Some(block) = i4096 {
      let index = self.indices[block];
      if index != 0 {
        let i64 = 63 - index.leading_zeros() as usize;
        let bits = self.bits[block][index.count_ones() as usize - 1];
        return Some((block << 12) | (i64 << 6) | (63 - bits.leading_zeros() as usize));
      }
      i4096 = block.checked_sub(1);
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
    let bit_array = &self.bits[i4096];
    let mut i64 = start >> 6;
    let i64bit = 1_u64 << (i64 % 64);
    let mut o = (index & (i64bit - 1)).count_ones() as usize;
    if index & i64bit != 0 {
      // There is at least one bit that is set in the current long, check
      // if one of them is after i
      debug_assert!(!bit_array.is_empty());
      let bits = bit_array[o] >> (start % 64);
      if bits != 0 {
        return start + bits.trailing_zeros() as usize;
      }
      o += 1;
    }
    let index_bits = (index >> (i64 % 64)) >> 1;
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
    debug_assert!(!bit_array.is_empty());
    let bits = bit_array[o];
    (i64 << 6) | bits.trailing_zeros() as usize
  }

  fn or_sparse(&mut self, other: &SparseFixedBitSet) {
    for i in 0..other.indices.len() {
      let index = other.indices[i];
      if index != 0 {
        self.or_impl(i, index, &other.bits[i], index.count_ones() as usize);
      }
    }
  }

  fn or_impl(&mut self, i4096: usize, index: u64, bits: &[u64], non_zero_long_count: usize) {
    debug_assert_eq!(index.count_ones(), non_zero_long_count as u32);
    let current_index = self.indices[i4096];
    if current_index == 0 {
      // fast path: if we currently have nothing in the block, just copy
      // the data this especially happens all the time if you
      // call OR on an empty set
      self.indices[i4096] = index;
      let new_bits = SmallVec::from_slice(&bits[0..non_zero_long_count]);
      self.ram_bytes_used = self
        .ram_bytes_used
        .saturating_add(block_heap_bytes(&new_bits));
      self.bits[i4096] = new_bits;
      self.non_zero_long_count += non_zero_long_count;
      return;
    }
    let current_bits = std::mem::take(&mut self.bits[i4096]);
    let new_index = current_index | index;
    let required_capacity = new_index.count_ones() as usize;
    let current_bytes = block_heap_bytes(&current_bits);
    let (mut new_bits, old_bits) = if current_bits.len() >= required_capacity {
      (current_bits, None)
    } else {
      let new_bits = SmallVec::from_elem(0, oversize(required_capacity));
      self.ram_bytes_used = self
        .ram_bytes_used
        .saturating_sub(current_bytes)
        .saturating_add(block_heap_bytes(&new_bits));
      (new_bits, Some(current_bits))
    };
    // we iterate backwards in order to not override data we might need on
    // the next iteration if the array is reused
    let mut i = new_index.leading_zeros() as usize;
    let mut new_o = required_capacity;
    while i < 64 {
      new_o -= 1;
      // bitIndex is the index of a bit which is set in newIndex and newO
      // is the number of 1 bits on its right
      let bit_index = 63 - i;
      debug_assert_eq!(
        new_o as u32,
        (new_index & (1_u64.wrapping_shl(bit_index as u32).wrapping_sub(1))).count_ones()
      );
      let current_bits = old_bits.as_ref().unwrap_or(&new_bits);
      let merged_bits =
        long_bits(current_index, current_bits, bit_index) | long_bits(index, bits, bit_index);
      new_bits[new_o] = merged_bits;
      i += 1 + new_index.wrapping_shl((i + 1) as u32).leading_zeros() as usize;
    }
    self.indices[i4096] = new_index;
    self.bits[i4096] = new_bits;
    self.non_zero_long_count += non_zero_long_count - (current_index & index).count_ones() as usize;
  }
  /// [`or`](Self::or) implementation that works best when `it` is dense.
  fn or_dense<I>(&mut self, mut it: I) -> Result<()>
  where
    I: DocIdSetIterator,
  {
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
    let mut longs = [0; 64];
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
          self.or_impl(i4096, index, &longs, num_longs);
          // and reset state for the new block
          i4096 = doc4096;
          index = 1_u64 << (doc64 % 64);
          num_longs = 0;
        }
        // we are on a new long, reset state
        i64 = doc64;
        current_long = 1_u64 << (doc % 64);
      }
      doc = it.next_doc()?.try_convert()?;
    }
    // flush
    longs[num_longs] = current_long;
    num_longs += 1;
    self.or_impl(i4096, index, &longs, num_longs);
    Ok(())
  }
  #[cfg(test)]
  pub fn get_indices(&self) -> &[u64] {
    &self.indices
  }
  #[cfg(test)]
  pub fn get_bits(&self) -> &[SmallVec<[u64; 4]>] {
    &self.bits
  }
  #[cfg(test)]
  pub fn get_non_zero_long_count(&self) -> usize {
    self.non_zero_long_count
  }
}

fn mask(from: usize, to: usize) -> u64 {
  let shift = (to as isize - from as isize).rem_euclid(64) as u32;
  (1_u64 << shift << 1).wrapping_sub(1) << (from % 64)
}

fn oversize(s: usize) -> usize {
  let mut new_size = s + (s >> 1);
  if new_size > 50 {
    new_size = 64
  }
  new_size
}

fn long_bits(index: u64, bits: &[u64], i64: usize) -> u64 {
  if (index & (1_u64 << (i64 % 64))) == 0 {
    0
  } else {
    bits[(index & ((1_u64 << (i64 % 64)) - 1)).count_ones() as usize]
  }
}

impl HasIdentity for SparseFixedBitSet {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl Bits for SparseFixedBitSet {
  fn get(&self, i: usize) -> Result<bool> {
    debug_assert!(self.consistent(i));
    let i4096 = i >> 12;
    let index = self.indices[i4096];
    let i64 = i >> 6;
    let i64bit = 1_u64 << (i64 % 64);
    // If the i64-th index bit is not set, then bit i is not set.
    // Shift distances are reduced modulo 64.
    if index & i64bit == 0 {
      return Ok(false);
    }
    // if it is set, then we count the number of bits that are set on the
    // right of i64, and that gives us the index of the long that
    // stores the bits we are interested in
    let offset = (index & (i64bit - 1)).count_ones() as usize;
    let bits = *self.bits[i4096]
      .get(offset)
      .ok_or_else(|| LuceneError::illegal_state("sparse bit block is missing"))?;
    Ok((bits & (1_u64 << (i % 64))) != 0)
  }

  fn length(&self) -> usize {
    self.length
  }
}

impl Accountable for SparseFixedBitSet {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(self.ram_bytes_used)
  }
}
impl BitSet for SparseFixedBitSet {
  fn clear(&mut self) -> Result<()> {
    for bit_array in &mut self.bits {
      *bit_array = SmallVec::new();
    }
    self.indices.fill(0);
    self.non_zero_long_count = 0;
    self.ram_bytes_used = size_of_slice(&self.indices).saturating_add(size_of_slice(&self.bits));
    Ok(())
  }

  fn set(&mut self, i: usize) -> Result<()> {
    debug_assert!(self.consistent(i));
    let i4096 = i >> 12;
    let index = self.indices[i4096];
    let i64 = i >> 6;
    let i64bit = 1_u64 << (i64 % 64);
    if (index & i64bit) != 0 {
      // in that case the sub 64-bits block we are interested in already
      // exists, we just need to set a bit in an existing
      // long: the number of ones on the right of i64 gives us
      // the index of the long we need to update
      let o = (index & (i64bit - 1)).count_ones() as usize;
      self.bits[i4096][o] |= 1_u64 << (i % 64);
    } else if index == 0 {
      // if the index is 0, it means that we just found a block of 4096
      // bits that has no bit that is set yet. So let's
      // initialize a new block:
      self.insert_block(i4096, i64bit, i);
    } else {
      // in that case we found a block of 4096 bits that has some values,
      // but the sub-block of 64 bits that we are interested
      // in has no value yet, so we need to insert a new long
      self.insert_long(i4096, i64bit, i, index);
    }
    Ok(())
  }

  #[inline(always)]
  fn get_and_set(&mut self, i: usize) -> Result<bool> {
    debug_assert!(self.consistent(i));
    let i4096 = i >> 12;
    let index = self.indices[i4096];
    let i64 = i >> 6;
    let i64bit = 1_u64 << (i64 % 64);
    if index & i64bit != 0 {
      // in that case the sub 64-bits block we are interested in already
      // exists, we just need to set a bit in an existing
      // long: the number of ones on the right of i64 gives us
      // the index of the long we need to update
      let location = (index & (i64bit - 1)).count_ones() as usize;
      let bit = 1_u64 << (i % 64);
      let word = &mut self.bits[i4096][location];
      let v = *word & bit != 0;
      if !v {
        *word |= bit;
      }
      Ok(v)
    } else if index == 0 {
      // if the index is 0, it means that we just found a block of 4096
      // bits that has no bit that is set yet. So let's
      // initialize a new block:
      self.insert_block(i4096, i64bit, i);
      Ok(false)
    } else {
      // in that case we found a block of 4096 bits that has some values,
      // but the sub-block of 64 bits that we are interested
      // in has no value yet, so we need to insert a new long
      self.insert_long(i4096, i64bit, i, index);
      Ok(false)
    }
  }

  fn clear_with_index(&mut self, i: usize) -> Result<()> {
    debug_assert!(self.consistent(i));
    let i4096 = i >> 12;
    let i64 = i >> 6;
    self.and(i4096, i64, !(1_u64 << (i % 64)));
    Ok(())
  }

  fn clear_range(&mut self, from: usize, to: usize) -> Result<()> {
    debug_assert!(to <= self.length);
    if from >= to {
      return Ok(());
    }
    let first_block = from >> 12;
    let last_block = (to - 1) >> 12;
    if first_block == last_block {
      self.clear_within_block(first_block, from & MASK_4096, (to - 1) & MASK_4096);
    } else {
      self.clear_within_block(first_block, from & MASK_4096, MASK_4096);
      for i in first_block + 1..last_block {
        self.non_zero_long_count -= self.indices[i].count_ones() as usize;
        self.indices[i] = 0;
        let bit_array = std::mem::take(&mut self.bits[i]);
        self.ram_bytes_used = self
          .ram_bytes_used
          .saturating_sub(block_heap_bytes(&bit_array));
      }
      self.clear_within_block(last_block, 0, (to - 1) & MASK_4096);
    }
    Ok(())
  }

  fn cardinality(&self) -> usize {
    let mut cardinality = 0;
    for bit_array in &self.bits {
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
    let bit_array = &self.bits[i4096];
    let mut i64 = i >> 6;
    let index_bits = index & ((1_u64 << (i64 % 64)) - 1);
    let o = index_bits.count_ones() as usize;
    if index & (1_u64 << (i64 % 64)) != 0 {
      // There is at least one bit that is set in the same long, check if
      // there is one bit that is set that is lower than i
      debug_assert!(!bit_array.is_empty());
      let bits = bit_array[o] & (1_u64 << (i % 64) << 1).wrapping_sub(1);
      if bits != 0 {
        return Option::from((i64 << 6) | (63 - bits.leading_zeros()) as usize);
      }
    }
    if index_bits == 0 {
      // no more bits are set in this block, go find the last bit in the
      // previous block
      return self.last_doc(i4096.checked_sub(1));
    }
    // go to the previous long
    i64 = 63 - index_bits.leading_zeros() as usize;
    debug_assert!(!bit_array.is_empty());
    let bits = bit_array[o - 1];
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

  fn or<T>(&mut self, iter: &mut T) -> Result<()>
  where
    T: DocIdSetIterator,
  {
    if let Some(other) = iter.get_sparse_fixed_bit_set() {
      check_unpositioned(iter)?;
      self.or_sparse(other);
      return Ok(());
    }
    if iter.cost()? < self.indices.len() as i64 {
      self.default_or(iter)
    } else {
      self.or_dense(iter)
    }
  }
}

impl crate::core::util::bit_set::BitSetExtensions for SparseFixedBitSet {
  fn as_sparse_fixed_bit_set(&self) -> Option<&SparseFixedBitSet> {
    Some(self)
  }
}
