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
use crate::core::store::DataInput;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::longs_ref::LongsRef;
use crate::core::util::packed::abstract_block_packed_writer::{
  BPV_SHIFT, MAX_BLOCK_SIZE, MIN_BLOCK_SIZE, MIN_VALUE_EQUALS_0,
};
use crate::core::util::packed::{Decoder, Format, FormatBehavior, PackedImpl, PackedInts};

/// Reader for sequences of longs written with
/// [`BlockPackedWriter`](crate::core::util::packed::block_packed_writer::BlockPackedWriter).
///
///
/// # See Also
/// [`BlockPackedWriter`](crate::core::util::packed::block_packed_writer::BlockPackedWriter)
///
/// # Note
/// This is an internal implementation detail.
pub struct BlockPackedReaderIterator {
  packed_ints_version: i32,
  value_count: usize,
  block_size: usize,
  values_ref: LongsRef,
  blocks: Vec<u8>,
  off: usize,
  ord: usize,
}

impl BlockPackedReaderIterator {
  /// Reads a variable-length `i64` value (supports negative values).
  ///
  /// # Arguments
  ///
  /// * `data_input` - The input from which to read the value.
  ///
  /// # Returns
  ///
  /// A signed `i64` value.
  ///
  /// # Errors
  ///
  /// Returns [`LuceneErrorKind::Io`] if the
  /// read fails.
  fn read_vlong<DI>(data_input: &mut DI) -> Result<i64>
  where
    DI: DataInput,
  {
    let mut l = 0u64;
    for shift in (0..56).step_by(7) {
      let b = data_input.read_byte()?;
      l |= ((b & 0x7F) as u64) << shift;
      if b as i8 >= 0 {
        return Ok(l as i64);
      }
    }
    let last_byte = data_input.read_byte()?;
    Ok(l as i64 | ((last_byte as i64 & 0xFF) << 56))
  }

  pub fn new(packed_ints_version: i32, block_size: usize, value_count: usize) -> Result<Self> {
    PackedInts::check_block_size(
      i32::try_from(block_size).map_err(|_| {
        LuceneError::illegal_argument(format!(
          "block_size must be >= {MIN_BLOCK_SIZE} and <= {MAX_BLOCK_SIZE}, got {block_size}"
        ))
      })?,
      MIN_BLOCK_SIZE,
      MAX_BLOCK_SIZE,
    )?;
    let long_ref = LongsRef::from_slice(Vec::new(), 0, 0);
    Ok(Self {
      packed_ints_version,
      value_count,
      block_size,
      values_ref: long_ref,
      blocks: vec![],
      off: block_size,
      ord: 0,
    })
  }
  /// Reset the current reader to wrap a stream of `valueCount` values
  /// contained in `data_input`. The block size remains unchanged.
  ///
  /// # Arguments
  ///
  /// * `data_input` - The new input stream to read from.
  /// * `value_count` - The number of values to read from the input.
  pub fn reset(&mut self, value_count: usize) {
    self.value_count = value_count;
    self.off = self.block_size;
    self.ord = 0;
  }
  /// Skip exactly `count` values.
  ///
  /// # Arguments
  ///
  /// * `count` - The number of values to skip.
  ///
  /// # Errors
  ///
  /// Returns a [`LuceneError`] if `count` is invalid or if there is an issue
  /// reading the input.
  pub fn skip<DI>(&mut self, mut count: usize, data_input: &mut DI) -> Result<()>
  where
    DI: DataInput,
  {
    // debug_assert!(count >= 0);
    if self.ord + count > self.value_count {
      return Err(LuceneError::eof("Attempt to skip past end of file"));
    }

    // 1. Skip buffered values
    let skip_buffer = std::cmp::min(count, self.block_size - self.off);
    self.off += skip_buffer;
    self.ord += skip_buffer;
    count -= skip_buffer;
    if count == 0 {
      return Ok(());
    }

    // 2. Skip as many blocks as necessary
    debug_assert_eq!(self.off, self.block_size);
    while count >= self.block_size {
      let token = data_input.read_byte()? as i32;
      let bits_per_value = token >> BPV_SHIFT;

      if bits_per_value > 64 {
        return Err(LuceneError::corrupt_index("Corrupted: bits_per_value > 64"));
      }

      if (token & MIN_VALUE_EQUALS_0) == 0 {
        Self::read_vlong(data_input)?;
      }

      let block_bytes = Format::Packed(PackedImpl::new(0)).byte_count(
        self.packed_ints_version,
        self.block_size,
        bits_per_value,
      );
      self.skip_bytes(block_bytes, data_input)?;
      self.ord += self.block_size;
      count -= self.block_size;
    }

    if count == 0 {
      return Ok(());
    }
    // 3. Skip last values
    debug_assert!(count < self.block_size);
    self.refill(data_input)?;
    self.ord += count;
    debug_assert!(count <= i32::MAX as usize);
    self.off += count;
    Ok(())
  }
  fn skip_bytes<DI>(&mut self, count: usize, data_input: &mut DI) -> Result<()>
  where
    DI: DataInput,
  {
    if data_input.is_index_input() {
      let new_position = data_input.get_file_pointer_in_data_input()? + count;
      data_input.seek_in_data_input(new_position)?;
    } else {
      // Use a temporary buffer to skip bytes
      ArrayUtil::grow_no_copy(&mut self.blocks, self.block_size, || 0)?;

      let mut skipped = 0;
      while skipped < count {
        let to_skip = std::cmp::min(self.blocks.len(), count - skipped);
        debug_assert!(to_skip <= i32::MAX as usize);
        data_input.read_bytes(&mut self.blocks, 0, to_skip)?;
        skipped += to_skip;
      }
    }

    Ok(())
  }
  /// Reads the next value from the stream.
  ///
  /// # Errors
  ///
  /// Returns [`LuceneErrorKind::Io`] if the
  /// reader has reached the end of the value stream.
  ///
  /// # Behavior
  /// - If the current block is exhausted (`off == block_size`), it will
  ///   refill the block.
  /// - Increments the `ord` to track the current position in the stream.
  /// - Returns the next value from the `values` buffer.
  pub fn next_value<DI>(&mut self, data_input: &mut DI) -> Result<i64>
  where
    DI: DataInput,
  {
    if self.ord == self.value_count {
      return Err(LuceneError::eof("Reached end of value stream"));
    }
    if self.off == self.block_size {
      self.refill(data_input)?;
    }
    let value = self.values_ref.longs[self.off];
    self.off += 1;
    self.ord += 1;
    Ok(value)
  }

  /// Reads between `1` and `count` values and returns a reference to the
  /// values.
  ///
  /// # Arguments
  ///
  /// * `count` - The maximum number of values to read.
  ///
  /// # Returns
  ///
  /// A [`LongsRef`] containing a reference to the values read and their offset
  /// and length.
  ///
  /// # Errors
  ///
  /// Returns [`LuceneErrorKind::Io`] if the
  /// reader has reached the end of the value stream.
  pub fn next_batch<DI>(&mut self, mut count: usize, data_input: &mut DI) -> Result<&LongsRef>
  where
    DI: DataInput,
  {
    debug_assert!(count > 0);
    if self.ord == self.value_count {
      return Err(LuceneError::eof("Reached end of value stream"));
    }
    if self.off == self.block_size {
      self.refill(data_input)?;
    }
    count = count.min(self.block_size - self.off);
    count = count.min(self.value_count - self.ord);

    self.values_ref.offset = self.off;
    self.values_ref.length = count;
    self.off += count;
    self.ord += count;
    Ok(&self.values_ref)
  }

  fn refill<DI>(&mut self, data_input: &mut DI) -> Result<()>
  where
    DI: DataInput,
  {
    let token = data_input.read_byte()? as i32;
    let min_equals_0 = (token & MIN_VALUE_EQUALS_0) != 0;
    let bits_per_value = token >> BPV_SHIFT;

    if bits_per_value > 64 {
      return Err(LuceneError::corrupt_index("Corrupted: bits_per_value > 64"));
    }
    let min_value = if min_equals_0 {
      0
    } else {
      BitUtil::zig_zag_decode_i64(Self::read_vlong(data_input)?.wrapping_add(1) as u64)
    };
    debug_assert!(min_equals_0 || min_value != 0);

    if self.values_ref.longs.is_empty() {
      self.values_ref.longs.resize(self.block_size, 0);
    }
    if bits_per_value == 0 {
      self.values_ref.longs.fill(min_value);
    } else {
      let decoder = PackedInts::get_decoder(
        Format::Packed(PackedImpl::new(0)),
        self.packed_ints_version,
        bits_per_value,
      )?;

      let iterations = self.block_size / Decoder::byte_value_count(decoder);
      let blocks_size = iterations * Decoder::byte_block_count(decoder);

      ArrayUtil::grow_no_copy(&mut self.blocks, blocks_size, || 0)?;

      let value_count = std::cmp::min(self.value_count - self.ord, self.block_size);

      let blocks_count = Format::Packed(PackedImpl::new(0)).byte_count(
        self.packed_ints_version,
        value_count,
        bits_per_value,
      );
      data_input.read_bytes(&mut self.blocks, 0, blocks_count)?;

      decoder.decode_u8_to_i64(&self.blocks, 0, &mut self.values_ref.longs, 0, iterations);
      if min_value != 0 {
        let values = &mut self.values_ref.longs[..value_count];
        for value in values {
          *value = value.wrapping_add(min_value);
        }
      }
    }
    self.off = 0;
    Ok(())
  }
  /// Returns the offset of the next value to read.
  ///
  /// # Returns
  /// The current global position (`ord`) in the value stream.
  pub fn ord(&self) -> usize {
    self.ord
  }
}
