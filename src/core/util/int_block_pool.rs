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
use crate::core::index::indexing_chain::IntBlockAllocator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
/// # Internal
/// A pool for int blocks similar to [`ByteBlockPool`](crate::core::util::byte_block_pool::ByteBlockPool).
pub struct IntBlockPool {
  /// array of buffers currently used in the pool. Buffers are allocated if
  /// needed don't modify this outside of this struct
  pub(crate) buffers: smallvec::SmallVec<[Vec<i32>; 4]>,
  /// index into the buffers array pointing to the current buffer used as the
  /// head.
  pub(crate) buffer_upto: Option<usize>,
  /// Pointer to the current position in head buffer.
  pub(crate) int_upto: i32,
  /// Current head offset.
  pub(crate) int_offset: i32,
  allocator: AllocatorIntEnum,
}
impl Default for IntBlockPool {
  fn default() -> Self {
    Self::new()
  }
}

impl IntBlockPool {
  /// Creates a new [`IntBlockPool`] with a default `Allocator`.
  ///
  /// See `IntBlockPool::next_buffer()` for more details.
  pub fn new() -> Self {
    let allocator = AllocatorIntEnum::DA(DirectAllocatorI32::new());
    Self::with_allocator(allocator)
  }
  /// Creates a new [`IntBlockPool`] with the given `Allocator`.
  ///
  /// See `IntBlockPool::next_buffer()` for more details.
  pub fn with_allocator(allocator: AllocatorIntEnum) -> Self {
    IntBlockPool {
      buffers: smallvec::SmallVec::new(),
      buffer_upto: None,
      int_upto: INT_BLOCK_SIZE,
      int_offset: -INT_BLOCK_SIZE,
      allocator,
    }
  }
  /// Expert: Resets the pool to its initial state, while optionally reusing
  /// the first buffer. Buffers that are not reused are reclaimed by
  /// [`AllocatorByte::recycle_byte_blocks`](crate::core::util::allocator_byte::AllocatorByte::recycle_byte_blocks).
  /// Buffers can be filled with zeros before recycling them. This is useful
  /// if a slice pool works on top of this int pool and relies on the
  /// buffers being filled with zeros to find the non-zero end of slices.
  ///
  /// # Parameters
  /// - `zero_fill_buffers`: if `true`, the buffers are filled with `0`.
  /// - `reuse_first`: if `true`, the first buffer will be reused and calling
  ///   `IntBlockPool::nextBuffer()` is not needed after reset if the block
  ///   pool was used before, i.e., `IntBlockPool::next_buffer()` was called
  ///   before.
  pub fn reset(&mut self, zero_fill_buffers: bool, reuse_first: bool) {
    if let Some(buffer_upto) = self.buffer_upto {
      if zero_fill_buffers {
        for i in 0..buffer_upto {
          // Fully zero fill buffers that we fully used
          self.buffers[i].fill(0);
        }
        // Partial zero fill the final buffer
        self.buffers[buffer_upto][..self.int_upto as usize].fill(0);
      }
      if buffer_upto > 0 || !reuse_first {
        let offset = if reuse_first { 1 } else { 0 };
        self
          .allocator
          .recycle_int_blocks(&self.buffers, offset, buffer_upto + 1);
        for _i in offset..buffer_upto + 1 {
          self.buffers.pop();
        }
      }

      if reuse_first {
        self.buffer_upto = Some(0);
        self.int_upto = 0;
        self.int_offset = 0;
      } else {
        self.buffer_upto = None;
        self.int_upto = INT_BLOCK_SIZE;
        self.int_offset = -INT_BLOCK_SIZE;
      }
    }
  }
  /// Advances the pool to its next buffer. This method should be called once
  /// after creation to initialize the pool. In contrast to initialization,
  /// [`IntBlockPool::reset`](crate::core::util::int_block_pool::IntBlockPool::reset)
  /// call will advance the pool to its first buffer immediately.
  pub fn next_buffer(&mut self) -> Result<()> {
    let buffer_upto = self.buffer_upto.map_or(0, |buffer_upto| buffer_upto + 1);
    if buffer_upto == self.buffers.len() {
      self.buffers.push(self.allocator.get_byte_block());
    }
    // Allocate new buffer and advance the pool to it
    self.buffer_upto = Some(buffer_upto);
    self.int_upto = 0;
    match self.int_offset.checked_add(INT_BLOCK_SIZE) {
      Some(val) => {
        self.int_offset = val;
        Ok(())
      },
      None => Err(LuceneError::number_overflow(
        "Overflow when calculating byte offset.",
      )),
    }
  }
  #[inline]
  pub fn get_buffer_mut(&mut self, buffer_index: usize) -> &mut Vec<i32> {
    &mut self.buffers[buffer_index]
  }
  pub fn get_buffer(&self, buffer_index: usize) -> &[i32] {
    &self.buffers[buffer_index]
  }
}

/// Abstract trait for allocating and freeing byte blocks.
pub trait AllocatorI32 {
  fn recycle_int_blocks(&mut self, blocks: &[Vec<i32>], start: usize, end: usize);
  fn get_byte_block(&mut self) -> Vec<i32>;
  fn get_block_size(&self) -> usize;
}

/// A simple [`AllocatorByte`](crate::core::util::allocator_byte::AllocatorByte) that never recycles.  */
pub struct DirectAllocatorI32 {
  block_size: usize,
}

impl Default for DirectAllocatorI32 {
  fn default() -> Self {
    Self::new()
  }
}

impl DirectAllocatorI32 {
  pub fn new() -> Self {
    DirectAllocatorI32 {
      block_size: INT_BLOCK_SIZE as usize,
    }
  }

  #[cfg(test)]
  pub(crate) fn with_block_size(block_size: usize) -> Self {
    DirectAllocatorI32 { block_size }
  }
}

impl AllocatorI32 for DirectAllocatorI32 {
  fn recycle_int_blocks(&mut self, _blocks: &[Vec<i32>], _start: usize, _end: usize) {}

  fn get_byte_block(&mut self) -> Vec<i32> {
    vec![0; self.block_size]
  }

  fn get_block_size(&self) -> usize {
    self.block_size
  }
}
pub enum AllocatorIntEnum {
  DA(DirectAllocatorI32),
  IBA(IntBlockAllocator),
}
impl AllocatorI32 for AllocatorIntEnum {
  fn recycle_int_blocks(&mut self, blocks: &[Vec<i32>], start: usize, end: usize) {
    match self {
      AllocatorIntEnum::DA(da) => da.recycle_int_blocks(blocks, start, end),
      AllocatorIntEnum::IBA(iba) => iba.recycle_int_blocks(blocks, start, end),
    }
  }

  fn get_byte_block(&mut self) -> Vec<i32> {
    match self {
      AllocatorIntEnum::DA(da) => da.get_byte_block(),
      AllocatorIntEnum::IBA(iba) => iba.get_byte_block(),
    }
  }

  fn get_block_size(&self) -> usize {
    match self {
      AllocatorIntEnum::DA(da) => da.get_block_size(),
      AllocatorIntEnum::IBA(iba) => iba.get_block_size(),
    }
  }
}

pub(crate) const INT_BLOCK_SHIFT: i32 = 13;
pub(crate) const INT_BLOCK_SIZE: i32 = 1 << INT_BLOCK_SHIFT;
pub(crate) const INT_BLOCK_MASK: i32 = INT_BLOCK_SIZE - 1;
