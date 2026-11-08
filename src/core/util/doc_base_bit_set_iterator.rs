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
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::util::TryIntoInt;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;

/// A [`DocIdSetIterator`] like
/// [`BitSetIterator`](crate::core::util::bit_set_iterator::BitSetIterator) but has a
/// doc base in order to avoid storing previous 0s.
pub struct DocBaseBitSetIterator {
  pub(super) bits: FixedBitSet,
  length: i32,
  cost: i64,
  doc_base: usize,
  doc: i32,
  fast_next_doc: bool,
  pending_index: usize,
  pending_word: u64,
}

impl DocBaseBitSetIterator {
  pub fn new(bits: FixedBitSet, cost: i64, doc_base: usize) -> Result<DocBaseBitSetIterator> {
    if cost < 0 {
      return Err(LuceneError::illegal_argument(format!(
        "cost must be >= 0, got {cost}"
      )));
    }
    if (doc_base & 63) != 0 {
      return Err(LuceneError::illegal_argument(format!(
        "docBase need to be a multiple of 64, got {doc_base}"
      )));
    }
    let len: i32 = bits.length().try_convert()?;
    let length = len + doc_base as i32;
    let fast_next_doc = bits.length() > 0
      && doc_base
        .checked_add(bits.length())
        .is_some_and(|end| end < NO_MORE_DOCS as usize);
    Ok(DocBaseBitSetIterator {
      bits,
      length,
      cost,
      doc_base,
      doc: -1,
      fast_next_doc,
      pending_index: usize::MAX,
      pending_word: 0,
    })
  }
  /// Gets the [`FixedBitSet`]. A `docId` will exist in this
  /// [`DocIdSetIterator`] if the bitset
  /// contains `(docId - get_doc_base())`.
  ///
  /// # Returns
  /// The offset `docId` bitset.
  pub fn get_bit_set(&self) -> &FixedBitSet {
    &self.bits
  }

  /// Gets the `docBase`. It is guaranteed that `docBase` is a multiple of 64.
  ///
  /// # Returns
  /// The `docBase`.
  pub fn get_doc_base(&self) -> usize {
    self.doc_base
  }
}

impl DocIdSetIterator for DocBaseBitSetIterator {
  fn doc_id(&self) -> i32 {
    self.doc
  }

  fn next_doc(&mut self) -> Result<i32> {
    let target = self.doc + 1;
    if !self.fast_next_doc {
      return self.advance(target);
    }
    if target >= self.length {
      self.doc = NO_MORE_DOCS;
      self.pending_index = usize::MAX;
      return Ok(self.doc);
    }
    let start = 0.max(target - self.doc_base as i32) as usize;
    let mut word_index = start >> 6;
    let words = self.bits.get_bits();
    let limit = FixedBitSet::bits2words(self.bits.length());
    let mut word = if self.pending_index == word_index {
      self.pending_word
    } else {
      words[word_index] as u64
    } & (u64::MAX << (start & 63));
    while word == 0 {
      word_index += 1;
      if word_index == limit {
        self.doc = NO_MORE_DOCS;
        self.pending_index = usize::MAX;
        return Ok(self.doc);
      }
      word = words[word_index] as u64;
    }
    let bit = word.trailing_zeros() as usize;
    self.pending_index = word_index;
    self.pending_word = word & (word - 1);
    self.doc = (self.doc_base + (word_index << 6) + bit) as i32;
    Ok(self.doc)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.pending_index = usize::MAX;
    if target >= self.length {
      self.doc = NO_MORE_DOCS;
      return Ok(self.doc);
    }
    let next = self
      .bits
      .next_set_bit(0.max(target - self.doc_base as i32).try_convert()?);
    if next == NO_MORE_DOCS as usize {
      self.doc = NO_MORE_DOCS
    } else {
      let next: i32 = next.try_convert()?;
      self.doc = next + self.doc_base as i32;
    }
    Ok(self.doc)
  }

  fn cost(&self) -> Result<i64> {
    Ok(self.cost)
  }
}

impl crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for DocBaseBitSetIterator
{
  fn get_doc_base_fixed_bit_set(&self) -> Option<(usize, &FixedBitSet)> {
    Some((self.doc_base, &self.bits))
  }
}
impl crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for DocBaseBitSetIterator {}
