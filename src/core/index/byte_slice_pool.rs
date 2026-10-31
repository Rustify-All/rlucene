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
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{BYTE_BLOCK_SIZE, ByteBlockPool};

/// struct that Posting and PostingVector use to write interleaved byte streams
/// into shared fixed-size byte buffers. The idea is to allocate slices of
/// increasing lengths. For example, the first slice is 5 bytes, the next slice
/// is 14, etc. We start by writing our bytes into the first 5 bytes. When
/// we hit the end of the slice, we allocate the next slice and then write the
/// address of the new slice into the last 4 bytes of the previous slice (the
/// "forwarding address").
///
/// Each slice is filled with 0's initially, and we mark the end with a non-zero
/// byte. This way the methods that are writing into the slice don't need to
/// record its length and instead allocate a new slice once they hit a non-zero
/// byte.
#[derive(Default)]
pub(crate) struct ByteSlicePool;
impl ByteSlicePool {
  /// An array holding the level sizes for byte slices. The first slice is 5
  /// bytes, the second is 14, and so on.
  pub(crate) const LEVEL_SIZE_ARRAY: [i32; 10] = [5, 14, 20, 30, 40, 40, 80, 80, 120, 200];

  /// An array holding indexes for the LEVEL_SIZE_ARRAY, to quickly navigate
  /// to the next slice level. These are encoded on 4 bits in the slice,
  /// so the values in this array should be less than 16.
  ///
  /// `NEXT_LEVEL_ARRAY[x] == x + 1`, except for the last element, where
  /// `NEXT_LEVEL_ARRAY[x] == x`, pointing at the maximum slice size.
  pub(crate) const NEXT_LEVEL_ARRAY: [usize; 10] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 9];

  /// The first level size for new slices.
  pub(crate) const FIRST_LEVEL_SIZE: i32 = Self::LEVEL_SIZE_ARRAY[0];

  /// Allocates a new slice with the given size and level 0.
  ///
  /// # Returns
  /// The position where the slice starts
  #[inline]
  pub fn new_slice(&mut self, size: i32, pool: &mut ByteBlockPool) -> Result<i32> {
    if size > BYTE_BLOCK_SIZE {
      return Err(LuceneError::illegal_argument(format!(
        "Slice size {} should be less than the block size {}",
        size, BYTE_BLOCK_SIZE
      )));
    }

    if pool.byte_upto > BYTE_BLOCK_SIZE - size {
      pool.next_buffer()?;
    }
    let upto = pool.byte_upto;
    pool.byte_upto += size;
    let buffer_upto = pool.buffer_upto()?;
    let byte_upto = pool.byte_upto as usize;
    pool.get_buffer_mut(buffer_upto)[byte_upto - 1] = 16; // This codifies level 0.
    Ok(upto)
  }

  /// Creates a new byte slice in continuation of the provided slice and
  /// returns its offset into the pool.
  ///
  /// # Parameters
  /// - `slice`: The current byte slice.
  /// - `upto`: The offset into the current slice, which is expected to point
  ///   to the last byte of the slice.
  ///
  /// # Returns
  /// The offset of the new slice in the pool.
  pub fn alloc_slice(
    &self,
    slice_index: usize,
    upto: usize,
    pool: &mut ByteBlockPool,
  ) -> Result<usize> {
    Ok((self.alloc_known_size_slice(slice_index, upto, pool)? >> 8) as usize)
  }
  /// Creates a new byte slice in continuation of the provided slice and
  /// returns its length and offset into the pool.
  ///
  /// # Parameters
  /// - `slice`: The current byte slice.
  /// - `upto`: The offset into the current slice, which is expected to point
  ///   to the last byte of the slice.
  ///
  /// # Returns
  /// A value where the lower 8 bits represent the new slice's length, and the
  /// other 24 bits represent the offset into the pool.
  pub fn alloc_known_size_slice(
    &self,
    slice_index: usize,
    upto: usize,
    pool: &mut ByteBlockPool,
  ) -> Result<i32> {
    let level;
    {
      let slice = pool.get_buffer(slice_index);
      level = slice[upto] & 15; // The last 4 bits codify the level.
    }
    let new_level = Self::NEXT_LEVEL_ARRAY[level as usize];
    let new_size = Self::LEVEL_SIZE_ARRAY[new_level];
    // Maybe allocate another block
    if pool.byte_upto > BYTE_BLOCK_SIZE - new_size {
      pool.next_buffer()?;
    }

    let new_upto = pool.byte_upto;
    let offset = new_upto + pool.byte_offset;
    pool.byte_upto += new_size;

    // Copy forward the past 3 bytes (which we are about to overwrite with
    // the forwarding address). We actually copy 4 bytes at once
    // since VarHandles make it cheap.
    let past3_bytes;
    {
      let slice = pool.get_buffer(slice_index);
      past3_bytes = (BitUtil::get_i32_le(slice, upto - 3)) & 0xFFFFFF;
    }
    // Ensure we're not changing the content of `buffer` by setting 4 bytes
    // instead of 3. This should never happen since the next
    // `new_size` bytes must be equal to 0.
    let buffer_upto = pool.buffer_upto()?;
    {
      let current_buffer = pool.get_buffer_mut(buffer_upto);
      let new_upto = new_upto as usize;
      debug_assert!(current_buffer[new_upto + 3] == 0);
      BitUtil::set_i32_le(current_buffer, new_upto, past3_bytes);
    }

    // Write forwarding address at end of last slice:
    {
      let slice = pool.get_buffer_mut(slice_index);
      BitUtil::set_i32_le(slice, upto - 3, offset);
    }

    // Write new level:
    let byte_upto = pool.byte_upto as usize;
    let current_buffer = pool.get_buffer_mut(buffer_upto);
    current_buffer[byte_upto - 1] = (16 | new_level) as u8;

    Ok(((new_upto + 3) << 8) | (new_size - 3))
  }
}
