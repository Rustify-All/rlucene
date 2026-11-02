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
use std::fmt::Display;

use crate::core::store::IndexInput;
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::long_values::LongValuesEnum2;
use crate::core::util::long_values::{LongValues, Zeroes};
use crate::core::util::packed::abstract_block_packed_writer::{MAX_BLOCK_SIZE, MIN_BLOCK_SIZE};
use crate::core::util::packed::{Format, FormatBehavior, PackedImpl, PackedInts};
use crate::core::util::ram_usage_estimator::size_of_slice;

const BLOCK_SIZE: i32 = u8::BITS as i32; // #bits in a block
const BLOCK_BITS: i32 = 3; //The #bits representing BLOCK_SIZE
const MOD_MASK: i32 = BLOCK_SIZE - 1; //  x % BLOCK_SIZE
/// Provides random access to a stream written with
/// [`MonotonicBlockPackedWriter`](crate::core::util::packed::monotonic_block_packed_writer::MonotonicBlockPackedWriter).
///
///
/// # Internal
/// This is an internal structure for efficient monotonic block-packed reading.
pub struct MonotonicBlockPackedReader {
  block_shift: i32,
  block_mask: usize,
  value_count: usize,
  min_values: Box<[i64]>,
  averages: Box<[f32]>,
  sub_readers: Box<[LongValuesEnum2<Zeroes, MonotonicLongValues>]>,
  sum_bpv: i64,
  total_byte_count: i64,
}

impl MonotonicBlockPackedReader {
  pub fn of<II>(
    input: &mut II,
    packed_ints_version: i32,
    block_size: i32,
    value_count: usize,
  ) -> Result<Self>
  where
    II: IndexInput,
  {
    Self::new(input, packed_ints_version, block_size, value_count)
  }
  fn new<II>(
    input: &mut II,
    packed_ints_version: i32,
    block_size: i32,
    value_count: usize,
  ) -> Result<Self>
  where
    II: IndexInput,
  {
    let block_shift = PackedInts::check_block_size(block_size, MIN_BLOCK_SIZE, MAX_BLOCK_SIZE)?;
    let block_size = block_size as usize;
    let num_blocks = PackedInts::num_blocks(value_count, block_size)?;
    let block_mask = block_size - 1;
    let mut min_values = vec![0; num_blocks];
    let mut averages = vec![0.0; num_blocks];
    let mut sub_readers: Vec<_> = (0..num_blocks)
      .map(|_| LongValuesEnum2::A(Zeroes))
      .collect();
    let mut sum_bpv: i64 = 0;
    let mut total_byte_count = 0;
    for i in 0..num_blocks {
      min_values[i] = input.read_zlong()?;
      averages[i] = f32::from_bits(input.read_int()? as u32);
      let bits_per_value = input.read_vint()?;
      sum_bpv += bits_per_value as i64;
      if bits_per_value > 64 {
        return Err(LuceneError::corrupt_index("Corrupted: bits_per_value > 64"));
      }

      if bits_per_value == 0 {
        // sub_readers inited with Zeroes,so no-op here
        continue;
      } else {
        let size = std::cmp::min(block_size, value_count - i * block_size);
        let byte_count =
          Format::Packed(PackedImpl::new(0)).byte_count(packed_ints_version, size, bits_per_value);
        total_byte_count += byte_count as i64;
        let blocks = input.get_bytes(byte_count)?.into_owned();
        let mask_right = 1u64.wrapping_shl(bits_per_value as u32).wrapping_sub(1);
        let bpv_minus_block_size = bits_per_value - BLOCK_SIZE;
        sub_readers[i] = LongValuesEnum2::B(MonotonicLongValues {
          bits_per_values: bits_per_value,
          bpv_minus_block_size,
          blocks: blocks.into_boxed_slice(),
          mask_right,
        });
      }
    }

    Ok(Self {
      block_shift,
      block_mask,
      value_count,
      min_values: min_values.into_boxed_slice(),
      averages: averages.into_boxed_slice(),
      sub_readers: sub_readers.into_boxed_slice(),
      sum_bpv,
      total_byte_count,
    })
  }
  /// Returns the number of values.
  pub fn size(&self) -> usize {
    self.value_count
  }
}
pub fn expected(origin: i64, average: f32, index: usize) -> i64 {
  origin.wrapping_add((average * index as f32) as i64)
}

pub struct MonotonicLongValues {
  bits_per_values: i32,
  bpv_minus_block_size: i32,
  blocks: Box<[u8]>,
  mask_right: u64,
}
impl LongValues for MonotonicLongValues {
  fn get(&self, index: usize) -> Result<i64> {
    // The abstract index in a bit stream
    let major_bit_pos = index as i64 * self.bits_per_values as i64;
    // The offset of the first block in the backing byte-array
    let mut block_offset = (major_bit_pos >> BLOCK_BITS) as usize;
    let mut end_bits = (major_bit_pos & MOD_MASK as i64) + self.bpv_minus_block_size as i64;
    let first_block = *self.blocks.get(block_offset).ok_or_else(|| {
      LuceneError::array_index_out_of_bounds(format!("block offset: {block_offset}"))
    })? as u64;
    if end_bits <= 0 {
      // Single block
      return Ok(((first_block >> -end_bits) & self.mask_right) as i64);
    }
    // Multiple blocks
    let mut value = (first_block << end_bits) & self.mask_right;
    block_offset += 1;
    while end_bits > BLOCK_SIZE as i64 {
      end_bits -= BLOCK_SIZE as i64;
      value |= (self.blocks[block_offset] as u64) << end_bits;
      block_offset += 1;
    }
    value |= (self.blocks[block_offset] as u64) >> (BLOCK_SIZE as i64 - end_bits);
    Ok(value as i64)
  }
}
impl LongValues for MonotonicBlockPackedReader {
  fn get(&self, index: usize) -> Result<i64> {
    debug_assert!(
      index < self.value_count,
      "Index out of bounds: {} >= {}",
      index,
      self.value_count
    );
    let block = index >> self.block_shift;
    let idx = index & self.block_mask;
    let expected_value = expected(self.min_values[block], self.averages[block], idx);
    let sub_reader_value = self.sub_readers[block].get(idx)?;

    Ok(expected_value.wrapping_add(sub_reader_value))
  }
}
impl Display for MonotonicBlockPackedReader {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    debug_assert!(self.sub_readers.len() <= i64::MAX as usize);
    let avg_bpv = if self.sub_readers.is_empty() {
      0
    } else {
      self.sum_bpv / self.sub_readers.len() as i64
    };
    write!(
      f,
      "{}(blocksize={}, size={}, avgBPV={})",
      std::any::type_name::<Self>(),
      1 << self.block_shift,
      self.value_count,
      avg_bpv
    )
  }
}
impl Accountable for MonotonicBlockPackedReader {
  fn ram_bytes_used(&self) -> Result<i64> {
    let mut size_in_bytes = 0;
    size_in_bytes += size_of_slice(&self.min_values);
    size_in_bytes += size_of_slice(&self.averages);
    size_in_bytes += size_of_slice(&self.sub_readers);
    size_in_bytes += self.total_byte_count;
    Ok(size_in_bytes)
  }
}
