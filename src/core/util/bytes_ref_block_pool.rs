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

use crate::core::index::BytesRef;
use crate::core::util::access::ByteSource;
use crate::core::util::accountable::Accountable;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bytes_ref_hash::do_hash;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{
  BYTE_BLOCK_MASK, BYTE_BLOCK_SHIFT, BYTE_BLOCK_SIZE, ByteBlockPool, SliceCopyOps,
};

/// Location of a term in the byte block pool that produced it.
/// The location remains valid only while that pool's contents are unchanged.
pub struct BytesRefBlockPoolPosition {
  pub block_index: usize,
  pub offset: usize,
  pub length: usize,
}

pub struct BytesRefBlockPool;

impl Default for BytesRefBlockPool {
  fn default() -> Self {
    Self::new()
  }
}

impl BytesRefBlockPool {
  pub fn new() -> Self {
    Self {}
  }
  /// Resets this buffer to the empty state.
  pub fn reset(&mut self, byte_block_pool: &mut ByteBlockPool) {
    byte_block_pool.reset(false, false)
  }

  /// Resolves the term starting at `start` without copying its bytes.
  /// Callers can borrow the range from the same pool or copy it into owned storage.
  pub fn fill_bytes_ref(
    &self,
    start: i32,
    byte_block_pool: &ByteBlockPool,
  ) -> BytesRefBlockPoolPosition {
    let block_index = (start >> BYTE_BLOCK_SHIFT) as usize;
    let block = byte_block_pool.get_buffer(block_index);
    let pos = (start & BYTE_BLOCK_MASK) as usize;

    let (length, offset) = Self::length_and_offset(block, pos);
    BytesRefBlockPoolPosition {
      block_index,
      offset,
      length,
    }
  }
  #[inline]
  fn length_and_offset(block: &[u8], pos: usize) -> (usize, usize) {
    if (block[pos] & 0x80) == 0 {
      (block[pos] as usize, pos + 1)
    } else {
      ((BitUtil::get_i16_be(block, pos) & 0x7FFF) as usize, pos + 2)
    }
  }

  /// Borrows a term with a single block lookup, validating its visible range.
  #[inline]
  pub(crate) fn borrow_bytes_ref<'a>(
    &self,
    start: i32,
    byte_block_pool: &'a ByteBlockPool,
  ) -> BytesRef<&'a [u8]> {
    let block_index = (start >> BYTE_BLOCK_SHIFT) as usize;
    let block = byte_block_pool.get_buffer(block_index);
    let pos = (start & BYTE_BLOCK_MASK) as usize;
    let (length, offset) = Self::length_and_offset(block, pos);
    BytesRef {
      bytes: &block[offset..offset + length],
      offset: 0,
      length,
    }
  }

  /// Add a term, returning the start position on the underlying
  /// [`ByteBlockPool`]. This can be used to read back the value using
  /// `fill_bytes_ref`.
  ///
  /// # See Also
  /// * [`Self::fill_bytes_ref`]
  pub fn add_bytes_ref<BS>(&mut self, bytes: &BytesRef<BS>, pool: &mut ByteBlockPool) -> Result<i32>
  where
    BS: ByteSource,
  {
    let length = bytes.length;
    let len2 = 2 + length as i32;
    if len2 + pool.byte_upto > BYTE_BLOCK_SIZE {
      if len2 > BYTE_BLOCK_SIZE {
        return Err(LuceneError::max_bytes_length_exceeded(format!(
          "bytes can be at most {} in length; got {}",
          BYTE_BLOCK_SIZE - 2,
          bytes.length
        )));
      }
      pool.next_buffer()?;
    }

    let text_start = pool.byte_upto + pool.byte_offset;
    let buffer_upto = pool.byte_upto as usize;
    let buffer_index = pool.buffer_upto()?;
    let buffer = pool.get_buffer_mut(buffer_index);

    let bytes_slice = bytes.as_byte_slice();
    // We first encode the length, followed by the bytes. Length is
    // encoded as vInt, but will consume 1 or 2 bytes at
    // most (we reject too-long terms, above).
    let new_length = if length < 128 {
      // 1 byte to store length
      buffer[buffer_upto] = length as u8;
      buffer.copy_from(bytes_slice, buffer_upto + 1);
      length + 1
    } else {
      // 2 byte to store length
      BitUtil::set_i16_be(buffer, buffer_upto, (length | 0x8000) as i16);
      buffer.copy_from(bytes_slice, buffer_upto + 2);
      length + 2
    };
    pool.byte_upto += new_length as i32;
    Ok(text_start)
  }
  /// Computes the hash of the BytesRef at the given start.
  pub fn hash(&self, start: i32, pool: &ByteBlockPool) -> i32 {
    let offset = (start & BYTE_BLOCK_MASK) as usize;
    let bytes = pool.get_buffer((start >> BYTE_BLOCK_SHIFT) as usize);

    let (len, pos) = if (bytes[offset] & 0x80) == 0 {
      // length is 1 byte
      (bytes[offset] as usize, offset + 1)
    } else {
      // length is 2 bytes (16-bit value, but only using lower 15
      // bits)
      let len = BitUtil::get_i16_be(bytes, offset) & 0x7FFF;
      (len as usize, offset + 2)
    };

    do_hash(bytes, pos, len)
  }
  /// Computes the equality between the BytesRef at the given start position
  /// and the provided BytesRef.
  pub fn equals<BS>(&self, start: i32, b: &BytesRef<BS>, pool: &ByteBlockPool) -> bool
  where
    BS: ByteSource,
  {
    let pos = (start & BYTE_BLOCK_MASK) as usize;
    let bytes = pool.get_buffer((start >> BYTE_BLOCK_SHIFT) as usize);

    let (length, offset) = if (bytes[pos] & 0x80) == 0 {
      // length is 1 byte
      (bytes[pos] as usize, pos + 1)
    } else {
      // length is 2 bytes (16-bit value, but only using lower 15
      // bits)
      let length = BitUtil::get_i16_be(bytes, pos) & 0x7FFF;
      (length as usize, pos + 2)
    };

    // Compare slices of bytes
    bytes[offset..offset + length] == *b.as_byte_slice()
  }
}

impl Accountable for BytesRefBlockPool {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(0)
  }
}
