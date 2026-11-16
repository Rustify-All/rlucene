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
use crate::test_framework::core::util::lucene_test_case::random;
use rand::Rng;
use rand::RngExt;
use std::sync::Arc;

use crate::core::index::byte_slice_pool::ByteSlicePool;
use crate::core::util::allocator_byte::{DirectAllocatorByte, DirectTrackingAllocatorByte};
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, LuceneErrorKind, Result};
use crate::core::util::{AtomicCounter, BYTE_BLOCK_SIZE, ByteBlockPool};
use crate::test_framework::core::util::test_util::TestUtil;

#[allow(dead_code)] // for quick search
struct TestByteSlicePool;
#[test]
fn test_alloc_known_size_slice() -> Result<()> {
  let mut random = random();
  let byte_used = Arc::new(AtomicCounter::new());
  let allocator = DirectTrackingAllocatorByte::new(byte_used);
  let mut block_pool = ByteBlockPool::new(allocator);
  block_pool.next_buffer()?;
  let mut slice_pool = ByteSlicePool;

  let mut random_data = Vec::new();
  for _ in 0..100 {
    let size: usize = if random.random_bool(0.5) {
      TestUtil::next_usize(&mut random, 100, 1000)
    } else {
      TestUtil::next_usize(&mut random, 50000, 100000)
    };

    random_data.clear();
    random_data.resize(size, 0u8);
    random.fill(&mut random_data[..]);

    let mut upto = slice_pool.new_slice(ByteSlicePool::FIRST_LEVEL_SIZE, &mut block_pool)? as usize;

    let mut offset = 0;
    while offset < size {
      let mut buffer_upto = block_pool.buffer_upto()?;
      let buffer_index = upto;
      if block_pool.get_buffer(buffer_upto)[buffer_index] & 16 == 0 {
        block_pool.get_buffer_mut(buffer_upto)[buffer_index] = random_data[offset];
        offset += 1;
        upto += 1;
      } else {
        let offset_and_length =
          slice_pool.alloc_known_size_slice(buffer_upto, upto, &mut block_pool)?;
        let slice_length = (offset_and_length & 0xff) as usize;
        upto = (offset_and_length >> 8) as usize;
        buffer_upto = block_pool.buffer_upto()?;
        assert_ne!(
          0,
          block_pool.get_buffer(buffer_upto)[upto + slice_length - 1]
        );
        let buffer_index = upto;
        assert_eq!(0, block_pool.get_buffer(buffer_upto)[buffer_index]);
        let write_length = std::cmp::min(slice_length - 1, size - offset);
        let buffer = block_pool.get_buffer_mut(buffer_upto);
        let copy_source = &random_data[offset..offset + write_length];
        buffer[buffer_index..][..copy_source.len()].copy_from_slice(copy_source);
        offset += write_length;
        upto += write_length;
      }
    }
  }
  Ok(())
}
#[test]
fn test_alloc_large_slice() -> Result<()> {
  let allocator = DirectAllocatorByte::new();
  let mut block_pool = ByteBlockPool::new(allocator);
  let mut slice_pool = ByteSlicePool;
  assert_eq!(0, slice_pool.new_slice(BYTE_BLOCK_SIZE, &mut block_pool)?);
  {
    let buffer_upto = block_pool.buffer_upto()?;
    let buffer = block_pool.get_buffer(buffer_upto);
    let buffer_0 = block_pool.get_buffer(0);
    assert_eq!(buffer, buffer_0);
    block_pool.next_buffer()?;
  }
  let result = slice_pool.new_slice(BYTE_BLOCK_SIZE + 1, &mut block_pool);
  assert!(matches!(
    result.as_ref().map_err(LuceneError::kind),
    Err(LuceneErrorKind::IllegalArgument(_))
  ));
  Ok(())
}
/// Creates a random byte array and writes it to a [`ByteSlicePool`] one
/// slice at a time.
struct SliceWriter {
  has_started: bool,
  size: usize,
  random_data: Vec<u8>,
  data_offset: usize,

  slice: usize,
  slice_length: i32,
  slice_offset: usize,

  first_slice_offset: usize,
  first_slice: usize,
}

impl SliceWriter {
  /// Creates a new `SliceWriter` instance.
  pub fn new<R>(random: &mut R) -> Self
  where
    R: Rng + ?Sized,
  {
    let size: usize = if random.random_bool(0.5) {
      // size < ByteBlockPool.BYTE_BLOCK_SIZE
      TestUtil::next_usize(random, 100, 1000)
    } else {
      // size > ByteBlockPool.BYTE_BLOCK_SIZE
      TestUtil::next_usize(random, 50000, 100000)
    };

    let mut random_data = vec![0u8; size];
    random.fill(&mut random_data[..]);

    SliceWriter {
      has_started: false,
      size,
      random_data,
      data_offset: 0,
      slice: 0,
      slice_length: 0,
      slice_offset: 0,
      first_slice_offset: 0,
      first_slice: 0,
    }
  }

  /// Writes the next slice of data.
  ///
  /// # Returns
  /// `true` if a slice is written and `false` if we're out of data to
  /// write.
  pub fn write_slice(
    &mut self,
    block_pool: &mut ByteBlockPool,
    slice_pool: &mut ByteSlicePool,
  ) -> Result<bool> {
    // The first slice is special
    if !self.has_started {
      self.data_offset = 0;
      self.slice_length = ByteSlicePool::FIRST_LEVEL_SIZE;
      self.slice_offset = slice_pool.new_slice(self.slice_length, block_pool)? as usize;
      self.first_slice_offset = self.slice_offset;
      self.first_slice = block_pool.buffer_upto()?;
      self.slice = self.first_slice;

      let write_length = std::cmp::min(self.size, (self.slice_length - 1) as usize);
      let buffer = block_pool.get_buffer_mut(self.first_slice);
      let copy_source = &self.random_data[self.data_offset..self.data_offset + write_length];
      buffer[self.slice_offset..][..copy_source.len()].copy_from_slice(copy_source);
      self.data_offset += write_length;
      self.has_started = true;
      return Ok(true);
    }

    // Have we written everything?
    if self.data_offset == self.size {
      return Ok(false);
    }

    let offset_and_length = slice_pool.alloc_known_size_slice(
      self.slice,
      self.slice_offset + (self.slice_length - 1) as usize,
      block_pool,
    )?;

    // No, write more
    #[allow(unused_assignments)]
    let mut current_pool_buffer = block_pool.get_buffer_mut(self.slice);
    self.slice = block_pool.buffer_upto()?;
    self.slice_length = offset_and_length & 0xff;
    self.slice_offset = (offset_and_length >> 8) as usize;
    let write_length = std::cmp::min(
      self.size - self.data_offset,
      (self.slice_length - 1) as usize,
    );
    current_pool_buffer = block_pool.get_buffer_mut(self.slice);
    let copy_source = &self.random_data[self.data_offset..self.data_offset + write_length];
    current_pool_buffer[self.slice_offset..][..copy_source.len()].copy_from_slice(copy_source);
    self.data_offset += write_length;
    Ok(true)
  }
}
/// Reads a sequence of slices into a byte array.
struct SliceReader {
  has_started: bool,
  size: usize,
  read_data: Vec<u8>,
  data_offset: usize,

  slice_length: usize,
  slice_offset: usize,

  slice: usize,
  slice_size_idx: usize,
}

impl SliceReader {
  /// Creates a new `SliceReader` instance.
  pub fn new(size: usize, first_slice_offset: usize, first_slice: usize) -> Self {
    SliceReader {
      has_started: false,
      size,
      read_data: vec![0u8; size],
      data_offset: 0,
      slice_length: 0,
      slice_offset: first_slice_offset,
      slice: first_slice,
      slice_size_idx: 0,
    }
  }

  /// Reads the next slice of data.
  ///
  /// # Returns
  /// `true` if a slice is read and `false` if the entire sequence has
  /// been read.
  pub fn read_slice(&mut self, block_pool: &ByteBlockPool) -> bool {
    // The first slice is special
    if !self.has_started {
      self.data_offset = 0;
      // Index into LEVEL_SIZE_ARRAY, allowing us to find the size of
      // the current slice
      self.slice_size_idx = 0;
      // 4 bytes are for the offset to the next slice, we can't use
      // them for data
      self.slice_length = (ByteSlicePool::LEVEL_SIZE_ARRAY[self.slice_size_idx] - 4) as usize;
      let read_length = if self.data_offset + self.slice_length + 3 >= self.size {
        // We are reading the last slice, no more offset, just a
        // byte for the level
        self.size - self.data_offset
      } else {
        self.slice_length
      };

      let current_buffer = block_pool.get_buffer(self.slice);
      let copy_source = &current_buffer[self.slice_offset..self.slice_offset + read_length];
      self.read_data[self.data_offset..][..copy_source.len()].copy_from_slice(copy_source);
      self.data_offset += read_length;
      self.slice_size_idx = std::cmp::min(
        self.slice_size_idx + 1,
        ByteSlicePool::LEVEL_SIZE_ARRAY.len() - 1,
      );
      self.has_started = true;
      return true;
    }

    // Have we read everything?
    if self.data_offset == self.size {
      return false;
    }

    // No, read more
    let mut slice_buffer = block_pool.get_buffer(self.slice);
    let global_slice_offset =
      BitUtil::get_i32_le(slice_buffer, self.slice_offset + self.slice_length);
    self.slice = (global_slice_offset / BYTE_BLOCK_SIZE) as usize;
    self.slice_offset = (global_slice_offset % BYTE_BLOCK_SIZE) as usize;
    self.slice_length = (ByteSlicePool::LEVEL_SIZE_ARRAY[self.slice_size_idx] - 4) as usize;
    let read_length = if self.data_offset + self.slice_length + 3 >= self.size {
      // Reading the last slice
      self.size - self.data_offset
    } else {
      self.slice_length
    };

    slice_buffer = block_pool.get_buffer(self.slice);
    let copy_source = &slice_buffer[self.slice_offset..self.slice_offset + read_length];
    self.read_data[self.data_offset..][..copy_source.len()].copy_from_slice(copy_source);
    self.data_offset += read_length;
    self.slice_size_idx = std::cmp::min(
      self.slice_size_idx + 1,
      ByteSlicePool::LEVEL_SIZE_ARRAY.len() - 1,
    );
    true
  }
}
#[test]
fn test_random_interleaved_slices() -> Result<()> {
  let mut random = random();
  let byte_used = Arc::new(AtomicCounter::new());
  let allocator = DirectTrackingAllocatorByte::new(byte_used);
  let mut pool = ByteBlockPool::new(allocator);
  let mut slice_pool = ByteSlicePool;

  let n_iterations = random.random_range(1..=3); // 1-3 iterations with buffer resets
  for _ in 0..n_iterations {
    let n = TestUtil::next_usize(&mut random, 2, 3);
    let mut slice_writers: Vec<SliceWriter> = Vec::with_capacity(n);
    let mut slice_readers: Vec<SliceReader> = Vec::with_capacity(n);

    // Init slice writers
    for _ in 0..n {
      slice_writers.push(SliceWriter::new(&mut random));
    }
    // Write slices
    loop {
      let i = random.random_range(0..n);
      let succeeded = slice_writers[i].write_slice(&mut pool, &mut slice_pool)?;
      if !succeeded {
        for writer in slice_writers.iter_mut().take(n) {
          while writer.write_slice(&mut pool, &mut slice_pool)? {}
        }
        break;
      }
    }

    // Init slice readers
    slice_writers.iter().take(n).for_each(|writer| {
      slice_readers.push(SliceReader::new(
        writer.size,
        writer.first_slice_offset,
        writer.first_slice,
      ));
    });

    // Read slices
    loop {
      let i = random.random_range(0..n);
      let succeeded = slice_readers[i].read_slice(&pool);
      if !succeeded {
        for j in slice_readers.iter_mut().take(n) {
          while j.read_slice(&pool) {}
        }
        break;
      }
    }

    // Compare written data with read data
    for i in 0..n {
      assert_eq!(slice_writers[i].random_data, slice_readers[i].read_data);
    }

    // We don't rely on the buffers being filled with zeros because the
    // SliceWriter keeps the slice length as state, but
    // ByteSlicePool.allocKnownSizeSlice asserts on zeros in the
    // buffer.
    pool.reset(true, random.random_bool(0.5));
  }

  Ok(())
}
