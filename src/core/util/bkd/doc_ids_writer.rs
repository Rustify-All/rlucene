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
use crate::core::index::point_values::IntersectVisitor;
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, NO_MORE_DOCS};
use crate::core::store::{DataOutput, IndexInput};
use crate::core::util::TryIntoInt;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::doc_base_bit_set_iterator::DocBaseBitSetIterator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::ints_ref::IntsRef;
use crate::core::util::longs_ref::LongsRef;

pub struct DocIdsWriter {
  scratch_longs: LongsRef,
  /// IntsRef to be used to iterate over the scratch buffer. A single
  /// instance owns the scratch buffer; only its length is reset for each visit.
  ///
  /// The main reason for existing is to be able to call the
  /// [`IntersectVisitor::visit_with_ints_ref`]
  /// method rather than the
  /// [`IntersectVisitor::visit`]
  /// method. This seems to make a difference in performance, probably due to
  /// fewer virtual calls then happening (once per read call rather than
  /// once per doc).
  scratch_ints_ref: IntsRef<Vec<i32>>,
}
impl Clone for DocIdsWriter {
  fn clone(&self) -> Self {
    let max_points_in_leaf = self.scratch_ints_ref.ints.len();
    DocIdsWriter::new(max_points_in_leaf)
  }
}

impl DocIdsWriter {
  pub const CONTINUOUS_IDS: i8 = -2;
  pub const BITSET_IDS: i8 = -1;
  pub const DELTA_BPV_16: i8 = 16;
  pub const BPV_24: i8 = 24;
  pub const BPV_32: i8 = 32;
  // These signs are legacy, should no longer be used in the writing side.
  pub const LEGACY_DELTA_VINT: i8 = 0;

  pub fn new(max_points_in_leaf: usize) -> Self {
    Self {
      scratch_longs: LongsRef::new(),
      scratch_ints_ref: IntsRef::from_slice(vec![0; max_points_in_leaf], 0, 0),
    }
  }
  pub(crate) fn write_doc_ids<DO>(
    &mut self,
    doc_ids: &[i32],
    start: usize,
    count: usize,
    out: &mut DO,
  ) -> Result<()>
  where
    DO: DataOutput,
  {
    // docs can be sorted either when all docs in a block have the same
    // value or when a segment is sorted
    let mut strictly_sorted = true;
    let mut min = doc_ids[0];
    let mut max = doc_ids[0];
    for i in 1..count {
      let last = doc_ids[start + i - 1];
      let current = doc_ids[start + i];
      if last >= current {
        strictly_sorted = false;
      }
      min = min.min(current);
      max = max.max(current);
    }

    let min2max: usize = (max - min + 1).try_convert()?;
    if strictly_sorted {
      if min2max == count {
        // continuous ids, typically happens when segment is sorted
        out.write_byte(DocIdsWriter::CONTINUOUS_IDS as u8)?;
        out.write_vint(doc_ids[start])?;
        return Ok(());
      } else if min2max <= ((count) << 4) {
        debug_assert!(min2max > count, "min2max: {min2max}, count: {count}");
        // Only trigger bitset optimization when max - min + 1 <= 16 *
        // count in order to avoid expanding too much
        // storage. A field with lower cardinality will
        // have higher probability to trigger this optimization.
        out.write_byte(DocIdsWriter::BITSET_IDS as u8)?;
        Self::write_ids_as_bit_set(doc_ids, start, count, out)?;
        return Ok(());
      }
    }
    if min2max <= 0xffff {
      out.write_byte(DocIdsWriter::DELTA_BPV_16 as u8)?;
      let mut temporary = Vec::new();
      let scratch = if count <= self.scratch_ints_ref.ints.len() {
        &mut self.scratch_ints_ref.ints[..count]
      } else {
        temporary.resize(count, 0);
        &mut temporary[..]
      };
      for i in 0..count {
        scratch[i] = doc_ids[start + i] - min;
      }
      out.write_vint(min)?;
      let half_len = count >> 1;
      for i in 0..half_len {
        scratch[i] = scratch[half_len + i] | (scratch[i] << 16);
      }
      for value in scratch.iter().take(half_len) {
        out.write_int(*value)?;
      }
      if count & 1 == 1 {
        out.write_short(scratch[count - 1] as i16)?;
      }
    } else if max <= 0xFFFFFF {
      out.write_byte(DocIdsWriter::BPV_24 as u8)?;
      // write them the same way we are reading them.
      let (groups, tail) = doc_ids[start..start + count].as_chunks::<8>();
      for &[doc1, doc2, doc3, doc4, doc5, doc6, doc7, doc8] in groups {
        // Rearrange the low three bytes of each DocID into the original
        // three long values, with the same byte order on every platform.
        let first: wide::u8x16 =
          wide::bytemuck::cast([doc1.to_le(), doc2.to_le(), doc3.to_le(), doc4.to_le()]);
        let second: wide::u8x16 =
          wide::bytemuck::cast([doc5.to_le(), doc6.to_le(), doc7.to_le(), doc8.to_le()]);
        // This safe API also supports the minimum wide dependency version.
        #[allow(deprecated)]
        let first_two = first.swizzle(wide::i8x16::new([
          9, 10, 4, 5, 6, 0, 1, 2, -1, -1, -1, -1, 12, 13, 14, 8,
        ]))
          | second.swizzle(wide::i8x16::new([
            -1, -1, -1, -1, -1, -1, -1, -1, 6, 0, 1, 2, -1, -1, -1, -1,
          ]));
        #[allow(deprecated)]
        let last = second.swizzle(wide::i8x16::new([
          12, 13, 14, 8, 9, 10, 4, 5, -1, -1, -1, -1, -1, -1, -1, -1,
        ]));
        let first_two: [u64; 2] = wide::bytemuck::cast(first_two);
        let last: [u64; 2] = wide::bytemuck::cast(last);
        let l1 = u64::from_le(first_two[0]) as i64;
        let l2 = u64::from_le(first_two[1]) as i64;
        let l3 = u64::from_le(last[0]) as i64;
        out.write_long(l1)?;
        out.write_long(l2)?;
        out.write_long(l3)?;
      }

      for &doc in tail {
        out.write_short(((doc as u32) >> 8) as i16)?;
        out.write_byte(doc as u8)?;
      }
    } else {
      out.write_byte(DocIdsWriter::BPV_32 as u8)?;
      for i in 0..count {
        out.write_int(doc_ids[start + i])?;
      }
    }
    Ok(())
  }
  fn write_ids_as_bit_set<DO>(
    doc_ids: &[i32],
    start: usize,
    count: usize,
    out: &mut DO,
  ) -> Result<()>
  where
    DO: DataOutput,
  {
    let min = doc_ids[start];
    let max = doc_ids[start + count - 1];

    let offset_words = min >> 6;
    let offset_bits = offset_words << 6;
    let total_word_count = FixedBitSet::bits2words((max - offset_bits + 1).try_convert()?);
    let mut current_word: i64 = 0;
    let mut current_word_index = 0;

    out.write_vint(offset_words)?;
    out.write_vint(total_word_count.try_convert()?)?;
    // build bit set streaming
    let mut remaining = &doc_ids[start..start + count];
    while let Some((&doc, tail)) = remaining.split_first() {
      let index = doc - offset_bits;
      let next_word_index = index >> 6;
      debug_assert!(
        current_word_index <= next_word_index,
        "current_word_index: {current_word_index}, next_word_index: {next_word_index}"
      );
      if current_word_index < next_word_index {
        out.write_long(current_word)?;
        current_word = 0;
        current_word_index += 1;
        while current_word_index < next_word_index {
          current_word_index += 1;
          out.write_long(0)?;
        }
      }
      // In this sorted slice, matching endpoint words also cover both inner IDs.
      if let Some(&[first, second, third, fourth]) = remaining.first_chunk::<4>()
        && (first >> 6) == (fourth >> 6)
      {
        let first_pair = wide::u64x2::new([first as u32 as u64, second as u32 as u64]);
        let second_pair = wide::u64x2::new([third as u32 as u64, fourth as u32 as u64]);
        let bits = (wide::u64x2::ONE << first_pair) | (wide::u64x2::ONE << second_pair);
        let [low, high] = bits.to_array();
        current_word |= (low | high) as i64;
        remaining = &remaining[4..];
      } else {
        current_word |= 1i64.wrapping_shl(index as u32);
        remaining = tail;
      }
    }
    out.write_long(current_word)?;
    debug_assert!(
      current_word_index + 1 == total_word_count as i32,
      "current_word_index + 1: {}, total_word_count: {}",
      current_word_index + 1,
      total_word_count
    );
    Ok(())
  }

  /// Read `count` integers into `doc_ids`.
  pub(crate) fn read_ints<II>(
    &mut self,
    input: &mut II,
    count: usize,
    doc_ids: &mut [i32],
  ) -> Result<()>
  where
    II: IndexInput,
  {
    let bpv = input.read_byte()? as i8;
    match bpv {
      DocIdsWriter::CONTINUOUS_IDS => Self::read_continuous_ids(input, count, doc_ids),
      DocIdsWriter::BITSET_IDS => self.read_bit_set(input, count, doc_ids),
      DocIdsWriter::DELTA_BPV_16 => Self::read_delta16(input, count, doc_ids),
      DocIdsWriter::BPV_24 => Self::read_ints24(input, count, doc_ids),
      DocIdsWriter::BPV_32 => Self::read_ints32(input, count, doc_ids),
      DocIdsWriter::LEGACY_DELTA_VINT => Self::read_legacy_delta_vints(input, count, doc_ids),
      _ => Err(LuceneError::illegal_state(format!(
        "Unsupported number of bits per value: {bpv}"
      ))),
    }
  }
  fn read_bit_set_iterator<II>(
    &mut self,
    input: &mut II,
    count: usize,
  ) -> Result<DocBaseBitSetIterator>
  where
    II: IndexInput,
  {
    let offset_words = input.read_vint()?;
    let long_len = input.read_vint()?.try_convert()?;
    ArrayUtil::grow_no_copy(&mut self.scratch_longs.longs, long_len, || 0)?;
    input.read_longs(&mut self.scratch_longs.longs, 0, long_len)?;
    // make ghost bits clear for FixedBitSet.
    if (long_len) < self.scratch_longs.length {
      self.scratch_longs.longs[long_len..].fill(0);
    }
    self.scratch_longs.length = long_len;
    let bit_set =
      FixedBitSet::with_capacity(std::mem::take(&mut self.scratch_longs.longs), long_len << 6)?;
    DocBaseBitSetIterator::new(bit_set, count as i64, (offset_words << 6).try_convert()?)
  }

  fn read_continuous_ids<II>(input: &mut II, count: usize, doc_ids: &mut [i32]) -> Result<()>
  where
    II: IndexInput,
  {
    let start = input.read_vint()?;
    if doc_ids.len() < count {
      for (i, doc_id) in doc_ids.iter_mut().enumerate() {
        *doc_id = start + i as i32;
      }
      return Err(LuceneError::array_index_out_of_bounds(format!(
        "Index {} out of bounds for length {}",
        doc_ids.len(),
        doc_ids.len()
      )));
    }
    for (i, doc_id) in doc_ids.iter_mut().take(count).enumerate() {
      *doc_id = start + i as i32;
    }
    Ok(())
  }

  fn read_legacy_delta_vints<II>(input: &mut II, count: usize, doc_ids: &mut [i32]) -> Result<()>
  where
    II: IndexInput,
  {
    let mut doc = 0;
    let length = doc_ids.len();
    let mut i = 0;
    while i < count {
      doc += input.read_vint()?;
      let doc_id = doc_ids.get_mut(i).ok_or_else(|| {
        LuceneError::array_index_out_of_bounds(format!(
          "Index {i} out of bounds for length {length}"
        ))
      })?;
      *doc_id = doc;
      i += 1;
    }
    Ok(())
  }

  fn read_bit_set<II>(&mut self, input: &mut II, count: usize, doc_ids: &mut [i32]) -> Result<()>
  where
    II: IndexInput,
  {
    let mut iterator = self.read_bit_set_iterator(input, count)?;
    let doc_base = iterator.get_doc_base();
    let word_count = self.scratch_longs.length;
    let end = word_count
      .checked_mul(64)
      .and_then(|bits| doc_base.checked_add(bits));
    if word_count == 0 || end.is_none_or(|end| end >= NO_MORE_DOCS as usize) {
      let mut pos = 0;
      loop {
        let doc_id = iterator.next_doc()?;
        if doc_id == NO_MORE_DOCS {
          break;
        }
        doc_ids[pos] = doc_id;
        pos += 1;
      }
      debug_assert!(pos == count, "pos: {pos}, count: {count}");
      return Ok(());
    }
    let mut pos = 0usize;
    for (word_index, &bits) in iterator
      .get_bit_set()
      .get_bits()
      .iter()
      .take(self.scratch_longs.length)
      .enumerate()
    {
      let mut word = bits as u64;
      if word == u64::MAX
        && let Some(end) = pos.checked_add(64)
        && let Some(dst) = doc_ids.get_mut(pos..end)
      {
        let base = doc_base + (word_index << 6);
        for (bit, doc_id) in dst.iter_mut().enumerate() {
          *doc_id = (base + bit) as i32;
        }
        pos = end;
        continue;
      }
      while word != 0 {
        let bit = word.trailing_zeros() as usize;
        doc_ids[pos] = (doc_base + (word_index << 6) + bit) as i32;
        pos += 1;
        word &= word - 1;
      }
    }
    debug_assert!(pos == count, "pos: {pos}, count: {count}");
    Ok(())
  }

  fn read_delta16<II>(input: &mut II, count: usize, doc_ids: &mut [i32]) -> Result<()>
  where
    II: IndexInput,
  {
    let min = input.read_vint()?;
    let half_len = count >> 1;
    input.read_ints(doc_ids, 0, half_len)?;
    for i in 0..half_len {
      let l = doc_ids[i];
      doc_ids[i] = ((l as u32) >> 16) as i32 + min;
      doc_ids[half_len + i] = (l & 0xffff) + min;
    }
    if count & 1 == 1 {
      doc_ids[count - 1] = (input.read_short()? as u16 as i32) + min;
    }
    Ok(())
  }

  fn read_ints24<II>(input: &mut II, count: usize, doc_ids: &mut [i32]) -> Result<()>
  where
    II: IndexInput,
  {
    let mut i = 0;
    let count_usize = count;
    while i < count_usize.saturating_sub(7) {
      let l1 = input.read_long()? as u64;
      let l2 = input.read_long()? as u64;
      let l3 = input.read_long()? as u64;
      let ids = [
        (l1 >> 40) as i32,
        ((l1 >> 16) & 0xffffff) as i32,
        (((l1 & 0xffff) << 8) | ((l2 >> 56) & 0xff)) as i32,
        ((l2 >> 32) & 0xffffff) as i32,
        ((l2 >> 8) & 0xffffff) as i32,
        (((l2 & 0xff) << 16) | ((l3 >> 48) & 0xffff)) as i32,
        ((l3 >> 24) & 0xffffff) as i32,
        (l3 & 0xffffff) as i32,
      ];
      if let Some(dst) = doc_ids.get_mut(i..i + 8) {
        dst.copy_from_slice(&ids);
      } else {
        for (j, doc_id) in ids.into_iter().enumerate() {
          doc_ids[i + j] = doc_id;
        }
      }
      i += 8;
    }
    while i < count_usize {
      doc_ids[i] =
        ((input.read_short()? as u16 as i32) << 8) | ((input.read_byte()? as i32) & 0xff);
      i += 1;
    }
    Ok(())
  }

  fn read_ints32<II>(input: &mut II, count: usize, doc_ids: &mut [i32]) -> Result<()>
  where
    II: IndexInput,
  {
    input.read_ints(doc_ids, 0, count)?;
    Ok(())
  }
  pub(crate) fn read_ints_with_visitor<II, V>(
    &mut self,
    input: &mut II,
    count: usize,
    visitor: &mut V,
  ) -> Result<()>
  where
    II: IndexInput,
    V: IntersectVisitor,
  {
    let bpv = input.read_byte()? as i8;
    match bpv {
      DocIdsWriter::CONTINUOUS_IDS => Self::read_continuous_ids_with_visitor(input, count, visitor),
      DocIdsWriter::BITSET_IDS => self.read_bit_set_with_visitor(input, count, visitor),
      DocIdsWriter::DELTA_BPV_16 => self.read_delta16_with_visitor(input, count, visitor),
      DocIdsWriter::BPV_24 => Self::read_ints24_with_visitor(input, count, visitor),
      DocIdsWriter::BPV_32 => self.read_ints32_with_visitor(input, count, visitor),
      DocIdsWriter::LEGACY_DELTA_VINT => {
        Self::read_legacy_delta_vints_with_visitor(input, count, visitor)
      },
      _ => Err(LuceneError::illegal_state(format!(
        "Unsupported number of bits per value: {bpv}"
      ))),
    }
  }

  fn read_bit_set_with_visitor<II, V>(
    &mut self,
    input: &mut II,
    count: usize,
    visitor: &mut V,
  ) -> Result<()>
  where
    II: IndexInput,
    V: IntersectVisitor,
  {
    let mut bit_set_iterator = self.read_bit_set_iterator(input, count)?;
    visitor.visit_with_iterator(&mut bit_set_iterator)?;
    Ok(())
  }
  fn read_continuous_ids_with_visitor<II, V>(
    input: &mut II,
    count: usize,
    visitor: &mut V,
  ) -> Result<()>
  where
    II: IndexInput,
    V: IntersectVisitor,
  {
    let start: usize = input.read_vint()?.try_convert()?;
    let extra = start & 63;
    let offset = start - extra;
    let num_bits = count + extra;
    let mut bit_set = FixedBitSet::new(num_bits);
    bit_set.set_with_range(extra, num_bits);
    let mut disi = DocBaseBitSetIterator::new(bit_set, count as i64, offset)?;
    visitor.visit_with_iterator(&mut disi)?;
    Ok(())
  }
  fn read_legacy_delta_vints_with_visitor<II, V>(
    input: &mut II,
    count: usize,
    visitor: &mut V,
  ) -> Result<()>
  where
    II: IndexInput,
    V: IntersectVisitor,
  {
    let mut doc = 0;
    for _ in 0..count {
      doc += input.read_vint()?;
      visitor.visit(doc)?;
    }
    Ok(())
  }
  fn read_delta16_with_visitor<II, V>(
    &mut self,
    input: &mut II,
    count: usize,
    visitor: &mut V,
  ) -> Result<()>
  where
    II: IndexInput,
    V: IntersectVisitor,
  {
    Self::read_delta16(input, count, &mut self.scratch_ints_ref.ints)?;
    self.scratch_ints_ref.length = count;
    visitor.visit_with_ints_ref(&self.scratch_ints_ref)?;
    Ok(())
  }
  fn read_ints24_with_visitor<II, V>(input: &mut II, count: usize, visitor: &mut V) -> Result<()>
  where
    II: IndexInput,
    V: IntersectVisitor,
  {
    let mut i = 0;
    let count_usize = count;
    while i < count_usize.saturating_sub(7) {
      let l1 = input.read_long()? as u64;
      let l2 = input.read_long()? as u64;
      let l3 = input.read_long()? as u64;
      visitor.visit((l1 >> 40) as i32)?;
      visitor.visit(((l1 >> 16) & 0xffffff) as i32)?;
      visitor.visit((((l1 & 0xffff) << 8) | ((l2 >> 56) & 0xff)) as i32)?;
      visitor.visit(((l2 >> 32) & 0xffffff) as i32)?;
      visitor.visit(((l2 >> 8) & 0xffffff) as i32)?;
      visitor.visit((((l2 & 0xff) << 16) | ((l3 >> 48) & 0xffff)) as i32)?;
      visitor.visit(((l3 >> 24) & 0xffffff) as i32)?;
      visitor.visit((l3 & 0xffffff) as i32)?;
      i += 8;
    }
    while i < count_usize {
      let s = input.read_short()? as u16 as i32;
      let b = input.read_byte()? as i32;
      visitor.visit((s << 8) | b)?;
      i += 1;
    }
    Ok(())
  }
  fn read_ints32_with_visitor<II, V>(
    &mut self,
    input: &mut II,
    count: usize,
    visitor: &mut V,
  ) -> Result<()>
  where
    II: IndexInput,
    V: IntersectVisitor,
  {
    input.read_ints(&mut self.scratch_ints_ref.ints, 0, count)?;
    self.scratch_ints_ref.length = count;
    visitor.visit_with_ints_ref(&self.scratch_ints_ref)?;
    Ok(())
  }
}
