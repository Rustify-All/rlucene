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
use std::fmt::{Display, Formatter};

use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::packed::bulk_operation::of;
use crate::core::util::packed::format_behavior::{FormatBehavior, PackedImpl};
use crate::core::util::packed::{Decoder, Encoder, Format, Mutable, PackedInts, Reader};
use crate::core::util::ram_usage_estimator::size_of_slice;

/// Space-optimized random access array of values with a fixed number of bits
/// per value. Values are packed contiguously.
///
/// The implementation strives to achieve maximum performance under the
/// constraint of contiguous bits by avoiding expensive operations. This comes
/// at the cost of code clarity.
///
/// # Technical Details
/// This implementation is a refinement of a non-branching version. The
/// non-branching `get` and `set` methods meant that 2 or 4 atomic accesses in
/// the underlying array were always performed, even for cases where only 1 or 2
/// accesses were needed. Even with caching, this had a detrimental
/// effect on performance. To address this issue, this implementation avoids
/// using lookup tables for shifts and masks. Instead, shifts and masks are
/// calculated on the fly, which proved to be faster.
///
/// See [LUCENE-4062](https://issues.apache.org/jira/browse/LUCENE-4062) for details.
pub struct Packed64 {
  /// Values are stored contiguously in the blocks array.
  blocks: Box<[u64]>,
  /// A right-aligned mask of width `bits_per_value` used by the `get`
  /// method.
  mask_right: u64,
  /// Optimization: Saves one lookup in the `get` method.
  bpv_minus_block_size: i32,
  /// The number of elements in the array.
  value_count: usize,
  /// The number of bits available for any given value.
  bits_per_value: i32,
}
impl Packed64 {
  pub const BLOCK_SIZE: i32 = 64; // 32 = int, 64 = long
  pub const BLOCK_BITS: i32 = 6; // The #bits representing BLOCK_SIZE
  pub const MOD_MASK: i32 = Self::BLOCK_SIZE - 1; // x % BLOCK_SIZE

  /// Creates an array with the internal structures adjusted for the given
  /// limits and initialized to 0.
  ///
  /// # Arguments
  ///
  /// * `value_count` - The number of elements.
  /// * `bits_per_value` - The number of bits available for any given value.
  ///
  /// # Returns
  ///
  /// A new instance of [`Packed64`].
  pub fn new(value_count: usize, bits_per_value: i32) -> Self {
    debug_assert!(
      bits_per_value > 0 && bits_per_value <= 64,
      "bitsPerValue must be > 0 and <= 64"
    );
    let format = Format::Packed(PackedImpl::new(0));
    let long_count = format.long_count(PackedInts::VERSION_CURRENT, value_count, bits_per_value);
    let blocks = vec![0; long_count];

    let mask_right =
      (!0u64) << (Self::BLOCK_SIZE - bits_per_value) >> (Self::BLOCK_SIZE - bits_per_value);
    let bpv_minus_block_size = bits_per_value - Self::BLOCK_SIZE;

    Self {
      blocks: blocks.into_boxed_slice(),
      mask_right,
      bpv_minus_block_size,
      value_count,
      bits_per_value,
    }
  }
  pub fn gcd(mut a: i32, mut b: i32) -> i32 {
    if a < b {
      std::mem::swap(&mut a, &mut b);
    }
    while b != 0 {
      let temp = b;
      b = a % b;
      a = temp;
    }
    a
  }
}

impl Reader for Packed64 {
  fn get(&self, index: usize) -> i64 {
    // The abstract index in a bit stream
    let major_bit_pos = (index as u64) * (self.bits_per_value as u64);

    // The index in the backing long-array
    let element_pos = (major_bit_pos >> Self::BLOCK_BITS) as usize;

    // The number of value-bits in the second block
    let end_bits =
      (major_bit_pos & Self::MOD_MASK as u64) as i64 + self.bpv_minus_block_size as i64;

    if end_bits <= 0 {
      // let value = self.blocks[element_pos] >> -end_bits;
      // let value1= (self.blocks[element_pos] >> -end_bits) as i64;
      // let value2= value & self.mask_right;
      // let value3= (value & self.mask_right) as i64;
      // println!("value: {}, value1: {}, value2: {}, value3: {}", value,
      // value1, value2, value3);
      return ((self.blocks[element_pos] >> -end_bits) & self.mask_right) as i64;
    }
    (((self.blocks[element_pos] << end_bits)
      | (self.blocks[element_pos + 1] >> (Self::BLOCK_SIZE as i64 - end_bits)))
      & self.mask_right) as i64
  }

  fn get_bulk(
    &self,
    mut index: usize,
    arr: &mut [i64],
    mut off: usize,
    mut len: usize,
  ) -> Result<usize> {
    debug_assert!(len > 0, "len must be > 0 (got {len})");
    debug_assert!(index < self.value_count, "index out of bounds");
    len = len.min(self.value_count - index);
    debug_assert!(
      (off + len) <= arr.len(),
      "not enough space in the target array"
    );

    let original_index = index;
    let decoder = of(Format::Packed(PackedImpl::new(0)), self.bits_per_value)?;
    let long_value_count = Decoder::long_value_count(decoder);

    // Go to the next block where the value does not span across two blocks
    let offset_in_blocks = index % long_value_count;

    if offset_in_blocks != 0 {
      for _i in offset_in_blocks..long_value_count {
        if len == 0 {
          return Ok(index - original_index);
        }
        arr[off] = self.get(index);
        off += 1;
        index += 1;
        len -= 1;
      }
      if len == 0 {
        return Ok(index - original_index);
      }
    }

    // Bulk get
    debug_assert_eq!(index % long_value_count, 0);
    let block_index = ((index as u64 * self.bits_per_value as u64) >> Self::BLOCK_BITS) as usize;
    debug_assert_eq!(
      (index as u64 * self.bits_per_value as u64) & Self::MOD_MASK as u64,
      0
    );

    let iterations = len / long_value_count;
    decoder.decode_u64_to_i64(&self.blocks, block_index, arr, off, iterations);

    let got_values = iterations * long_value_count;
    index += got_values;
    debug_assert!(len >= got_values, "Remaining length is negative");
    len -= got_values;

    if index > original_index {
      // Stay at the block boundary
      Ok(index - original_index)
    } else {
      // No progress so far => already at a block boundary but no full
      // block to get
      debug_assert_eq!(index, original_index, "Index mismatch");
      self.default_get_bulk(index, arr, off, len)
    }
  }

  fn size(&self) -> usize {
    self.value_count
  }
}

impl Accountable for Packed64 {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(size_of_slice(&self.blocks))
  }
}

impl Display for Packed64 {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "{}(bitsPerValue={}, size={}, blocks={})",
      std::any::type_name::<Self>(),
      self.bits_per_value,
      self.size(),
      self.blocks.len()
    )
  }
}

impl Mutable for Packed64 {
  fn get_bits_per_value(&self) -> i32 {
    self.bits_per_value
  }

  fn set(&mut self, index: usize, value: i64) -> Result<()> {
    // The abstract index in a contiguous bit stream
    let major_bit_pos = (index as u64) * self.bits_per_value as u64;
    // The index in the backing blocks array
    let element_pos = (major_bit_pos >> Self::BLOCK_BITS) as usize;
    // The number of value-bits in the second block
    let end_bits =
      (major_bit_pos & Self::MOD_MASK as u64) as i64 + self.bpv_minus_block_size as i64;

    if end_bits <= 0 {
      // Single block case
      self.blocks[element_pos] =
        (self.blocks[element_pos] & !(self.mask_right << -end_bits)) | (value << -end_bits) as u64;
      return Ok(());
    }

    // Two blocks case
    self.blocks[element_pos] =
      (self.blocks[element_pos] & !(self.mask_right >> end_bits)) | ((value as u64) >> end_bits);

    self.blocks[element_pos + 1] = (self.blocks[element_pos + 1] & (!0u64 >> end_bits))
      | (value << (Self::BLOCK_SIZE as i64 - end_bits)) as u64;
    Ok(())
  }

  fn set_bulk(
    &mut self,
    mut index: usize,
    arr: &[i64],
    mut off: usize,
    mut len: usize,
  ) -> Result<usize> {
    debug_assert!(len > 0, "len must be > 0 (got {len})");
    debug_assert!(index < self.value_count, "index out of bounds");
    len = len.min(self.value_count - index);
    debug_assert!(
      (off + len) <= arr.len(),
      "not enough values in the source array"
    );

    // At 64 bits each value occupies one complete block.
    if self.bits_per_value == 64
      && let Some(input) = arr.get(off..).and_then(|arr| arr.get(..len))
      && let Some(output) = self
        .blocks
        .get_mut(index..)
        .and_then(|blocks| blocks.get_mut(..len))
    {
      output.copy_from_slice(wide::bytemuck::cast_slice(input));
      return Ok(len);
    }

    let original_index = index;
    let encoder = of(Format::Packed(PackedImpl::new(0)), self.bits_per_value)?;
    let long_value_count = Encoder::long_value_count(encoder);

    // Go to the next block where the value does not span across two blocks
    // PACKED value groups always have a power-of-two length.
    let offset_in_blocks = index & (long_value_count - 1);
    if offset_in_blocks != 0 {
      for _ in offset_in_blocks..long_value_count {
        if len == 0 {
          return Ok(index - original_index);
        }
        self.set(index, arr[off])?;
        index += 1;
        off += 1;
        len -= 1;
      }
      if len == 0 {
        return Ok(index - original_index);
      }
    }

    // Bulk set
    debug_assert_eq!(index % long_value_count, 0);
    let block_index = ((index as u64 * self.bits_per_value as u64) >> Self::BLOCK_BITS) as usize;
    debug_assert_eq!(
      (index as u64 * self.bits_per_value as u64) & Self::MOD_MASK as u64,
      0
    );

    let iterations = len / long_value_count;
    let input = &arr[off..];
    if iterations != 0 {
      encoder.encode_i64_to_u64(input, 0, &mut self.blocks, block_index, iterations);
    }

    let set_values = iterations * long_value_count;
    index += set_values;
    len -= set_values;

    if index > original_index {
      // Stay at the block boundary
      Ok(index - original_index)
    } else {
      // No progress so far => already at a block boundary but no full
      // block to set
      debug_assert_eq!(index, original_index);
      self.default_set_bulk(index, arr, off, len)
    }
  }

  fn fill(&mut self, mut from_index: usize, to_index: usize, val: i64) -> Result<()> {
    debug_assert!(
      PackedInts::unsigned_bits_required(val) <= self.bits_per_value,
      "Value requires more bits than allowed by bits_per_value"
    );
    debug_assert!(from_index <= to_index, "from_index must be <= to_index");

    // Minimum number of values that use an exact number of full blocks
    let n_aligned_values = (64 / Packed64::gcd(64, self.bits_per_value)) as usize;
    let span = to_index - from_index;

    // If the span is too small, fall back to naive filling
    if span <= (3 * n_aligned_values) {
      self.default_fill(from_index, to_index, val)?;
      return Ok(());
    }

    // Fill the first values naively until the next block start
    let from_index_mod_n_aligned_values = from_index % n_aligned_values;
    if from_index_mod_n_aligned_values != 0 {
      for _ in from_index_mod_n_aligned_values..n_aligned_values {
        self.set(from_index, val)?;
        from_index += 1;
      }
    }
    debug_assert_eq!(from_index % n_aligned_values, 0);

    // Compute the `i64` blocks for `n_aligned_values` consecutive values and
    // use them to set as many values as possible without applying any mask
    // or shift
    let n_aligned_blocks = (n_aligned_values * self.bits_per_value as usize) >> 6;
    let n_aligned_values_blocks = {
      let mut values = Packed64::new(n_aligned_values, self.bits_per_value);
      for i in 0..n_aligned_values {
        values.set(i, val)?;
      }
      values.blocks
    };
    debug_assert!(n_aligned_blocks <= n_aligned_values_blocks.len());

    // Bulk set values using precomputed blocks
    let start_block = ((from_index as i64 * self.bits_per_value as i64) >> 6) as usize;
    let end_block = ((to_index as i64 * self.bits_per_value as i64) >> 6) as usize;
    if (n_aligned_blocks == 1
      || n_aligned_values_blocks
        .iter()
        .all(|&word| word == n_aligned_values_blocks[0]))
      && let Some(blocks) = self.blocks.get_mut(start_block..end_block)
    {
      blocks.fill(n_aligned_values_blocks[0]);
    } else if let Some(blocks) = self.blocks.get_mut(start_block..end_block) {
      let offset = start_block % n_aligned_blocks;
      let mut remaining = blocks;
      if offset != 0 {
        let length = remaining.len().min(n_aligned_blocks - offset);
        remaining[..length].copy_from_slice(&n_aligned_values_blocks[offset..offset + length]);
        remaining = &mut remaining[length..];
      }
      // Seed one period, then reuse the initialized prefix.
      let len = remaining.len();
      let seed = n_aligned_blocks.min(len);
      remaining[..seed].copy_from_slice(&n_aligned_values_blocks[..seed]);
      let mut filled = seed;
      while filled < len {
        let length = filled.min(len - filled);
        remaining.copy_within(..length, filled);
        filled += length;
      }
    } else {
      for block in start_block..end_block {
        let block_value = n_aligned_values_blocks[block % n_aligned_blocks];
        self.blocks[block] = block_value;
      }
    }

    // Fill the gap
    for i in (((end_block as i64) << 6) / self.bits_per_value as i64) as usize..to_index {
      self.set(i, val)?;
    }
    Ok(())
  }

  fn clear(&mut self) -> Result<()> {
    self.blocks.fill(0);
    Ok(())
  }
}
