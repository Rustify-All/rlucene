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

use std::borrow::Cow;

use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::util::access::SharedAccessVec;
use crate::core::util::accountable::Accountable;
use crate::core::util::allocator_byte::{AllocatorByte, AllocatorByteEnum, DirectAllocatorByte};
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::ram_usage_estimator::size_of_vec;
use crate::core::util::{SliceCopyOps, TryIntoInt};

/// This struct enables the allocation of fixed-size buffers and their
/// management as part of a buffer array. Allocation is done through the use of
/// an [`AllocatorByte`] which can
/// be customized, e.g., to allow recycling old buffers. There are methods for
/// writing ([`append`](Self::append)) and reading from the buffers (for example,
/// [`read_bytes`](Self::read_bytes)), which handle read/write operations
/// across buffer boundaries.
///
/// # Note
/// This is an internal API.
#[derive(Debug)]
pub struct ByteBlockPool {
  buffers: Vec<Vec<u8>>,
  // Current head buffer's index
  pub(crate) buffer_upto: Option<usize>,
  allocator: AllocatorByteEnum,
  /// Offset from the start of the first buffer to the start of the current
  /// buffer, which is `buffer_upto * BYTE_BLOCK_SIZE`. The buffer pool
  /// maintains this offset because it is the first to overflow if there
  /// are too many allocated blocks.
  pub(crate) byte_offset: i32,
  pub(crate) byte_upto: i32,
}
impl Default for ByteBlockPool {
  fn default() -> Self {
    let allocator = DirectAllocatorByte::new();
    Self::new(allocator)
  }
}
impl ByteBlockPool {
  pub fn new<T>(allocator: T) -> Self
  where
    T: Into<AllocatorByteEnum>,
  {
    let allocator = allocator.into();
    ByteBlockPool {
      buffers: vec![],
      buffer_upto: None,
      allocator,
      byte_offset: -BYTE_BLOCK_SIZE,
      byte_upto: BYTE_BLOCK_SIZE,
    }
  }
  /// Expert: Resets the pool to its initial state, while optionally reusing
  /// the first buffer. Buffers that are not reused are reclaimed by
  /// [`AllocatorByte::recycle_byte_blocks`].
  /// Buffers can be filled with zeros before recycling them. This is
  /// useful if a slice pool works on top of this byte pool and relies on
  /// the buffers being filled with zeros to find the non-zero end of slices.
  ///
  /// # Arguments
  /// * `zero_fill_buffers` - If `true`, the buffers are filled with `0`. This
  ///   should be set to `true` if this pool is used with slices.
  /// * `reuse_first` - If `true`, the first buffer will be reused, and
  ///   calling [`ByteBlockPool::next_buffer`] is not
  ///   needed after reset, if the block pool was used before (i.e.,
  ///   [`ByteBlockPool::next_buffer`] was called
  ///   before).
  pub fn reset(&mut self, zero_fill_buffers: bool, reuse_first: bool) {
    if let Some(buffer_upto) = self.buffer_upto {
      if zero_fill_buffers {
        for i in 0..(buffer_upto + 1) {
          self.buffers[i].fill(0);
        }
      }
      if buffer_upto > 0 || !reuse_first {
        let offset = if reuse_first { 1 } else { 0 };
        self
          .allocator
          .recycle_byte_blocks(&mut self.buffers, offset, buffer_upto + 1);
        for _i in offset..(buffer_upto + 1) {
          self.buffers.pop();
        }
      }

      if reuse_first {
        self.buffer_upto = Some(0);
        self.byte_upto = 0;
        self.byte_offset = 0;
      } else {
        self.buffer_upto = None;
        self.byte_upto = BYTE_BLOCK_SIZE;
        self.byte_offset = -BYTE_BLOCK_SIZE;
      }
    }
  }
  /// Allocates a new buffer and advances the pool to it. This method should
  /// be called once after creation to initialize the pool. In contrast to
  /// initialization, [`ByteBlockPool::reset`](Self::reset)
  /// reset) call will advance the pool to its first buffer immediately.
  pub fn next_buffer(&mut self) -> Result<usize> {
    let next_upto = match self.buffer_upto {
      Some(upto) => upto + 1,
      None => 0,
    };

    if next_upto == self.buffers.len() {
      self.buffers.push(self.allocator.get_byte_block());
    }
    // Allocate new buffer and advance the pool to it
    self.buffer_upto = Some(next_upto);
    self.byte_upto = 0;
    match self.byte_offset.checked_add(BYTE_BLOCK_SIZE) {
      Some(val) => self.byte_offset = val,
      None => {
        return Err(LuceneError::number_overflow(
          "Overflow when calculating byte offset.",
        ));
      },
    }
    Ok(next_upto)
  }

  /// Point at a pool block when the value is contiguous. The builder retains
  /// the owned buffer between cross-block reads so switching to a borrowed
  /// page does not discard its capacity.
  pub fn set_bytes_ref<'a>(
    &'a self,
    builder: &mut BytesRefBuilder<Vec<u8>>,
    result: &mut BytesRef<Cow<'a, Vec<u8>>>,
    offset: i64,
    length: usize,
  ) -> Result<()> {
    // Preserve the existing growth error before changing the result or
    // converting the offset, including when no allocation is needed now.
    if result.bytes.len() < length {
      ArrayUtil::oversize(length, size_of::<u8>())?;
    }
    let pos = (offset & BYTE_BLOCK_MASK as i64) as usize;
    if pos + length > BYTE_BLOCK_SIZE as usize {
      if matches!(result.bytes, Cow::Borrowed(_)) {
        result.bytes = Cow::Owned(std::mem::take(&mut builder.get_bytes_mut_ref().bytes));
      }
      ArrayUtil::grow_no_copy(result.bytes.to_mut(), length, || 0)?;
    }
    result.length = length;
    let buffer_index: i32 = (offset >> BYTE_BLOCK_SHIFT).try_convert()?;
    if pos + length <= BYTE_BLOCK_SIZE as usize {
      let block = &self.buffers[buffer_index as usize];
      let _ = &block[pos..pos + length];
      let previous = std::mem::replace(&mut result.bytes, Cow::Borrowed(block));
      if let Cow::Owned(bytes) = previous {
        builder.get_bytes_mut_ref().bytes = bytes;
      }
      result.offset = pos;
    } else {
      let bytes = result.bytes.to_mut();
      result.offset = 0;
      self.read_bytes(offset, bytes, 0, length)?;
    }
    Ok(())
  }
  /// Appends the bytes in the provided BytesRef at the current position.
  pub fn append_bytes_ref<AV>(&mut self, bytes: &BytesRef<AV>) -> Result<()>
  where
    AV: SharedAccessVec<u8>,
  {
    bytes
      .bytes
      .access(|bytes_ref| self.append_range(bytes_ref, bytes.offset, bytes.length))
  }
  /// Appends the bytes from a source [`ByteBlockPool`] at a given offset and
  /// length.
  ///
  /// # Arguments
  /// * `src_pool` - The source pool to copy from.
  /// * `src_offset` - The source pool offset.
  /// * `length` - The number of bytes to copy.
  pub fn append_from_byte_block_pool(
    &mut self,
    src_pool: &ByteBlockPool,
    mut src_offset: i64,
    length: usize,
  ) -> Result<()> {
    let mut bytes_left = length;
    while bytes_left > 0 {
      let buffer_left = (BYTE_BLOCK_SIZE - self.byte_upto) as usize;
      if bytes_left < buffer_left {
        // Fits within the current target page without advancing the pool.
        let source_pos = (src_offset & BYTE_BLOCK_MASK as i64) as usize;
        let target_pos = self.byte_upto as usize;
        if bytes_left <= BYTE_BLOCK_SIZE as usize - source_pos
          && let Some(buffer_upto) = self.buffer_upto
          && let Ok(source_index) = i32::try_from(src_offset >> BYTE_BLOCK_SHIFT)
          && let Some(source) = src_pool.buffers.get(source_index as usize)
          && let Some(source) = source.get(source_pos..source_pos + bytes_left)
          && let Some(target) = self.buffers.get_mut(buffer_upto)
          && let Some(target) = target.get_mut(target_pos..target_pos + bytes_left)
        {
          target.copy_from_slice(source);
          self.byte_upto += bytes_left as i32;
        } else {
          self.append_bytes_single_buffer(src_pool, src_offset, bytes_left)?;
        }
        break;
      } else {
        // fill up this buffer and move to next one
        if buffer_left > 0 {
          self.append_bytes_single_buffer(src_pool, src_offset, buffer_left)?;
          bytes_left -= buffer_left;
          src_offset += buffer_left as i64;
        }
        self.next_buffer()?;
      }
    }
    Ok(())
  }
  fn append_bytes_single_buffer(
    &mut self,
    src_pool: &ByteBlockPool,
    mut src_offset: i64,
    mut length: usize,
  ) -> Result<()> {
    debug_assert!(length <= (BYTE_BLOCK_SIZE - self.byte_upto) as usize);
    debug_assert!(self.buffer_upto.is_some());
    let buffer_upto = self
      .buffer_upto
      .ok_or_else(|| LuceneError::number_format("buffer not initialized"))?;
    while length > 0 {
      let src_buffer_index: i32 = (src_offset >> BYTE_BLOCK_SHIFT).try_convert()?;
      let src_pos = (src_offset & BYTE_BLOCK_MASK as i64) as usize;
      let bytes_to_copy = std::cmp::min(BYTE_BLOCK_SIZE as usize - src_pos, length);
      self.buffers[buffer_upto].copy_from(
        &src_pool.buffers[src_buffer_index as usize][src_pos..src_pos + bytes_to_copy],
        self.byte_upto as usize,
      );

      length -= bytes_to_copy;
      src_offset += bytes_to_copy as i64;
      self.byte_upto += bytes_to_copy as i32;
    }
    Ok(())
  }

  /// Appends the provided byte array at the current position.
  ///
  /// # Arguments
  /// * `bytes` - The byte array to write.
  pub fn append(&mut self, bytes: &[u8]) -> Result<()> {
    let length = bytes.len();
    self.append_range(bytes, 0, length)
  }
  /// Appends the bytes from a source [`ByteBlockPool`] at a given offset and
  /// length.
  ///
  /// # Arguments
  /// * `src_pool` - The source pool to copy from.
  /// * `src_offset` - The source pool offset.
  /// * `length` - The number of bytes to copy.
  pub fn append_range(&mut self, bytes: &[u8], mut offset: usize, length: usize) -> Result<()> {
    let mut bytes_left = length;
    let mut buffer_upto = self.buffer_upto.unwrap_or_default();
    while bytes_left > 0 {
      let buffer_left = (BYTE_BLOCK_SIZE - self.byte_upto) as usize;
      if bytes_left < buffer_left {
        // fits within current buffer
        self.buffers[buffer_upto]
          .copy_from(&bytes[offset..offset + bytes_left], self.byte_upto as usize);
        self.byte_upto += bytes_left as i32;
        break;
      } else {
        // fill up this buffer and move to next one
        if buffer_left > 0 {
          self.buffers[buffer_upto].copy_from(
            &bytes[offset..offset + buffer_left],
            self.byte_upto as usize,
          );
        }
        buffer_upto = self.next_buffer()?;
        bytes_left -= buffer_left;
        offset += buffer_left;
      }
    }
    Ok(())
  }

  /// Reads bytes out of the pool starting at the given offset with the given
  /// length into the given byte array at offset `off`.
  ///
  /// # Note
  /// This method allows copying across block boundaries.
  pub fn read_bytes(
    &self,
    offset: i64,
    bytes: &mut [u8],
    mut bytes_offset: usize,
    bytes_length: usize,
  ) -> Result<()> {
    let mut bytes_left = bytes_length;
    let buffer_index: i32 = (offset >> BYTE_BLOCK_SHIFT).try_convert()?;
    let mut buffer_index = buffer_index as usize;
    let mut pos = (offset & BYTE_BLOCK_MASK as i64) as usize;
    while bytes_left > 0 {
      let chunk = std::cmp::min(BYTE_BLOCK_SIZE as usize - pos, bytes_left);
      bytes.copy_from(&self.buffers[buffer_index][pos..pos + chunk], bytes_offset);

      bytes_offset += chunk;
      bytes_left -= chunk;
      buffer_index += 1;
      pos = 0;
    }
    Ok(())
  }
  /// Reads a single byte at the given offset.
  ///
  /// # Arguments
  /// * `offset` - The offset to read.
  ///
  /// # Returns
  /// The byte at the specified offset.
  pub fn read_byte(&self, offset: usize) -> u8 {
    let buffer_index = offset >> BYTE_BLOCK_SHIFT;
    let pos = offset & BYTE_BLOCK_MASK as usize;
    self.buffers[buffer_index][pos]
  }
  /// the current position (in absolute value) of this byte pool .
  pub fn get_position(&mut self) -> i64 {
    debug_assert!(self.allocator.get_block_size() <= i32::MAX as usize);
    let buffer_upto = match self.buffer_upto {
      Some(upto) => upto as i32,
      None => -1,
    };
    (buffer_upto * self.allocator.get_block_size() as i32 + self.byte_upto) as i64
  }
  pub fn get_buffer_mut(&mut self, buffer_index: usize) -> &mut Vec<u8> {
    &mut self.buffers[buffer_index]
  }
  pub fn get_buffer(&self, buffer_index: usize) -> &Vec<u8> {
    &self.buffers[buffer_index]
  }
  pub(crate) fn get_buffer_or_empty(&self, buffer_index: usize) -> &[u8] {
    self.buffers.get(buffer_index).map_or(&[], Vec::as_slice)
  }
  pub fn get_bytes_used(&self) -> i64 {
    self.allocator.get_used()
  }
  /// Get valid buffer_upto
  #[inline]
  pub fn buffer_upto(&self) -> Result<usize> {
    match self.buffer_upto {
      Some(upto) => Ok(upto),
      None => Err(LuceneError::illegal_state(
        "buffer_upto not initialized yet, call next_buffer first.",
      )),
    }
  }
}
impl Accountable for ByteBlockPool {
  fn ram_bytes_used(&self) -> Result<i64> {
    let mut bytes = size_of_vec(&self.buffers);
    for buffer in &self.buffers {
      bytes = bytes.saturating_add(size_of_vec(buffer));
    }
    Ok(bytes)
  }
}

/// Finds the index of the buffer containing a byte, given an offset to that
/// byte.
///
/// The calculation for `buffer_upto` is as follows:
///
/// - `buffer_upto = global_offset >>
///   BYTE_BLOCK_SHIFT`
/// - `buffer_upto = global_offset / BYTE_BLOCK_SIZE`
///
/// # Parameters
/// - `global_offset`: The offset to the target byte.
pub const BYTE_BLOCK_SHIFT: i32 = 15;
/// The size of each buffer in the pool.
pub const BYTE_BLOCK_SIZE: i32 = 1 << BYTE_BLOCK_SHIFT;
/// Use this to find the position of a global offset in a particular buffer.
///
/// # Formula
/// `position_in_current_buffer = global_offset & BYTE_BLOCK_MASK`
///
/// `position_in_current_buffer = global_offset % BYTE_BLOCK_SIZE`
pub(crate) const BYTE_BLOCK_MASK: i32 = BYTE_BLOCK_SIZE - 1;
