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
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::util::access::{SharedAccess, SharedAccessVec};
use crate::core::util::accountable::Accountable;
use crate::core::util::allocator_byte::{
    AllocatorByte, AllocatorByteEnum, MTAllocatorByteEnum, STAllocatorByteEnum,
};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{CounterEnum, CounterEnumBorrow, CounterEnumLock, SliceCopyOps};

/// This struct enables the allocation of fixed-size buffers and their
/// management as part of a buffer array. Allocation is done through the use of
/// an [`AllocatorByte`] which can
/// be customized, e.g., to allow recycling old buffers. There are methods for
/// writing ([`append`](#method.append)) and reading from the buffers (e.g.,
/// [`read_bytes`](#method.read_bytes)), which handle read/write operations
/// across buffer boundaries.
///
/// # Note
/// This is an internal API.
#[derive(Debug)]
pub struct ByteBlockPool<A>
where
    A: SharedAccess<CounterEnum>,
{
    buffers: Vec<Vec<u8>>,
    // Current head buffer's index
    pub buffer_upto: i32,
    allocator: AllocatorByteEnum<A>,
    /// Offset from the start of the first buffer to the start of the current
    /// buffer, which is `buffer_upto * BYTE_BLOCK_SIZE`. The buffer pool
    /// maintains this offset because it is the first to overflow if there
    /// are too many allocated blocks.
    pub(crate) byte_offset: i32,
    pub(crate) byte_upto: i32,
}

macro_rules! impl_byte_block_pool {
    ($enum_ty:ty, $alloc_ty:ty, $method:ident) => {
        impl ByteBlockPool<$enum_ty> {
            pub fn $method(allocator: $alloc_ty) -> Self {
                ByteBlockPool {
                    buffers: vec![],
                    buffer_upto: -1,
                    allocator,
                    byte_offset: -BYTE_BLOCK_SIZE,
                    byte_upto: BYTE_BLOCK_SIZE,
                }
            }
        }
    };
}
impl_byte_block_pool!(CounterEnumBorrow, STAllocatorByteEnum, new);
impl_byte_block_pool!(CounterEnumLock, MTAllocatorByteEnum, new_sync);

impl<A> ByteBlockPool<A>
where
    A: SharedAccess<CounterEnum>,
{
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
    ///   calling [`ByteBlockPool::next_buffer`](#method.next_buffer) is not
    ///   needed after reset, if the block pool was used before (i.e.,
    ///   [`ByteBlockPool::next_buffer`](#method.next_buffer) was called
    ///   before).
    pub fn reset(&mut self, zero_fill_buffers: bool, reuse_first: bool) {
        if self.buffer_upto != -1 {
            if zero_fill_buffers {
                for i in 0..(self.buffer_upto + 1) as usize {
                    self.buffers[i].fill(0);
                }
            }
            if self.buffer_upto > 0 || !reuse_first {
                let offset = if reuse_first { 1 } else { 0 };
                self.allocator.recycle_byte_blocks(
                    &self.buffers,
                    offset,
                    (self.buffer_upto + 1) as usize,
                );
                for _i in offset..(self.buffer_upto + 1) as usize {
                    self.buffers.pop();
                }
            }

            if reuse_first {
                self.buffer_upto = 0;
                self.byte_upto = 0;
                self.byte_offset = 0;
            } else {
                self.buffer_upto = -1;
                self.byte_upto = BYTE_BLOCK_SIZE;
                self.byte_offset = -BYTE_BLOCK_SIZE;
            }
        }
    }
    /// Allocates a new buffer and advances the pool to it. This method should
    /// be called once after the constructor to initialize the pool. In
    /// contrast to the constructor, a [`ByteBlockPool::reset`](#method.
    /// reset) call will advance the pool to its first buffer immediately.
    pub fn next_buffer(&mut self) -> Result<()> {
        if self.buffer_upto + 1 == self.buffers.len() as i32 {
            self.buffers.push(self.allocator.get_byte_block());
        }
        // Allocate new buffer and advance the pool to it
        self.buffer_upto += 1;
        self.byte_upto = 0;
        match self.byte_offset.checked_add(BYTE_BLOCK_SIZE) {
            Some(val) => self.byte_offset = val,
            None => {
                return Err(LuceneError::number_overflow(
                    "Overflow when calculating byte offset.",
                ));
            },
        }
        Ok(())
    }

    /// Fills the provided [`BytesRef`] with the bytes at the specified offset
    /// and length. # Parameters
    /// - `_builder`: This parameter is currently unused but retained for future
    ///   compatibility.See Note
    /// # Note
    /// In Java, the length of result is adjusted through BytesRefBuilder,
    /// whereas in Rust Lucene, to avoid copying, we operate directly on result.
    ///
    /// However, we still retain the interface definitions from Java Lucene to
    /// maintain consistency with the original implementation as much as
    /// possible.
    pub fn set_bytes_ref<AV: SharedAccessVec<u8>>(
        &self,
        _builder: &mut BytesRefBuilder<AV>,
        result: &mut BytesRef<AV>,
        offset: i64,
        length: i32,
    ) -> Result<()> {
        if result.length < length as usize {
            result.bytes = AV::from_vec(vec![0; length as usize]);
        }
        result.length = length as usize;
        let buffer_index = offset >> BYTE_BLOCK_SHIFT;
        let pos = (offset & BYTE_BLOCK_MASK as i64) as i32;
        if pos + length <= BYTE_BLOCK_SIZE {
            // Common case: The slice lives in a single block.
            result.bytes.copy(
                &self.buffers[buffer_index as usize][pos as usize..(pos + length) as usize],
                0,
            );
            result.offset = 0;
        } else {
            // builder.grow_no_copy(length);
            result.offset = 0;
            result.bytes.access_mut(|bytes| {
                self.read_bytes(offset, bytes, 0, length)?;
                // Help the compiler infer types.
                Ok::<(), LuceneError>(())
            })?;
            // builder.get().bytes.clone_from(&result.bytes);
        }
        Ok(())
    }
    /// Appends the bytes in the provided BytesRef at the current position.
    pub fn append_bytes_ref<AV: SharedAccessVec<u8>>(
        &mut self,
        bytes: &BytesRef<AV>,
    ) -> Result<()> {
        bytes.bytes.access(|bytes_ref| {
            self.append_range(bytes_ref, bytes.offset as i32, bytes.length as i32)
        })
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
        src_pool: &ByteBlockPool<A>,
        mut src_offset: i64,
        length: i32,
    ) -> Result<()> {
        let mut bytes_left = length;
        while bytes_left > 0 {
            let buffer_left = BYTE_BLOCK_SIZE - self.byte_upto;
            if bytes_left < buffer_left {
                // fits within current buffer
                self.append_bytes_single_buffer(src_pool, src_offset, bytes_left);
                break;
            } else {
                // fill up this buffer and move to next one
                if buffer_left > 0 {
                    self.append_bytes_single_buffer(src_pool, src_offset, buffer_left);
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
        src_pool: &ByteBlockPool<A>,
        mut src_offset: i64,
        mut length: i32,
    ) {
        debug_assert!(length <= BYTE_BLOCK_SIZE - self.byte_upto);
        while length > 0 {
            let src_pos = src_offset & BYTE_BLOCK_MASK as i64;
            let bytes_to_copy = std::cmp::min(BYTE_BLOCK_SIZE - src_pos as i32, length);
            self.buffers[self.buffer_upto as usize].copy_from(
                &src_pool.buffers[(src_offset >> BYTE_BLOCK_SHIFT) as usize]
                    [src_pos as usize..(src_pos + bytes_to_copy as i64) as usize],
                self.byte_upto as usize,
            );

            length -= bytes_to_copy;
            src_offset += bytes_to_copy as i64;
            self.byte_upto += bytes_to_copy;
        }
    }

    /// Appends the provided byte array at the current position.
    ///
    /// # Arguments
    /// * `bytes` - The byte array to write.
    pub fn append(&mut self, bytes: &[u8]) -> Result<()> {
        let length = bytes.len() as i32;
        self.append_range(bytes, 0, length)
    }
    /// Appends the bytes from a source [`ByteBlockPool`] at a given offset and
    /// length.
    ///
    /// # Arguments
    /// * `src_pool` - The source pool to copy from.
    /// * `src_offset` - The source pool offset.
    /// * `length` - The number of bytes to copy.
    pub fn append_range(&mut self, bytes: &[u8], mut offset: i32, length: i32) -> Result<()> {
        let mut bytes_left = length;
        while bytes_left > 0 {
            let buffer_left = BYTE_BLOCK_SIZE - self.byte_upto;
            if bytes_left < buffer_left {
                // fits within current buffer
                self.buffers[self.buffer_upto as usize].copy_from(
                    &bytes[offset as usize..(offset + bytes_left) as usize],
                    self.byte_upto as usize,
                );
                self.byte_upto += bytes_left;
                break;
            } else {
                // fill up this buffer and move to next one
                if buffer_left > 0 {
                    self.buffers[self.buffer_upto as usize].copy_from(
                        &bytes[offset as usize..(offset + buffer_left) as usize],
                        self.byte_upto as usize,
                    );
                }
                self.next_buffer()?;
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
        mut bytes_offset: i32,
        bytes_length: i32,
    ) -> Result<()> {
        let mut bytes_left = bytes_length;
        let buffer_index: i32 = (offset >> BYTE_BLOCK_SHIFT).try_into()?;
        let mut buffer_index = buffer_index as usize;
        let mut pos = (offset & BYTE_BLOCK_MASK as i64) as i32;
        while bytes_left > 0 {
            let chunk = std::cmp::min(BYTE_BLOCK_SIZE - pos, bytes_left);
            bytes.copy_from(
                &self.buffers[buffer_index][pos as usize..(pos + chunk) as usize],
                bytes_offset as usize,
            );

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
    pub fn read_byte(&self, offset: i64) -> u8 {
        debug_assert!(offset >= 0);
        let buffer_index = (offset >> BYTE_BLOCK_SHIFT) as usize;
        let pos = (offset & BYTE_BLOCK_MASK as i64) as i32;
        self.buffers[buffer_index][pos as usize]
    }
    /// the current position (in absolute value) of this byte pool .
    pub fn get_position(&mut self) -> i64 {
        debug_assert!(self.allocator.get_block_size() <= i32::MAX as usize);
        (self.buffer_upto * self.allocator.get_block_size() as i32 + self.byte_upto) as i64
    }
    pub fn get_buffer_mut(&mut self, buffer_index: i32) -> &mut Vec<u8> {
        &mut self.buffers[buffer_index as usize]
    }
    pub fn get_buffer(&mut self, buffer_index: i32) -> &Vec<u8> {
        &self.buffers[buffer_index as usize]
    }
    pub fn get_bytes_used(&self) -> i64 {
        self.allocator.get_used()
    }
}
impl<A> Accountable for ByteBlockPool<A>
where
    A: SharedAccess<CounterEnum>,
{
    fn ram_bytes_used(&self) -> Result<i64> {
        todo!()
    }
}
// for single thread
pub type ByteBlockPoolBorrow = Rc<RefCell<ByteBlockPool<CounterEnumBorrow>>>;
// for multi thread
pub type ByteBlockPoolLock = Arc<Mutex<ByteBlockPool<CounterEnumLock>>>;

//TODO
const BASE_RAM_BYTES: i64 = 0;
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use rand::distr::Alphanumeric;
    use rand::{Rng, RngCore};

    use crate::core::index::{BytesRef, BytesRefBuilder};
    use crate::core::util::allocator_byte::{
        AllocatorByteEnum, DirectAllocatorByte, DirectTrackingAllocatorByte,
    };
    use crate::core::util::error::lucene_error::{LuceneError, Result};
    use crate::core::util::{
        BYTE_BLOCK_SIZE, ByteBlockPool, CounterEnum, CounterEnumBorrow, SliceCopyOps,
    };
    use crate::test::util::lucene_test_case::lucene_test_case_util::{at_least, random};
    use crate::test::util::test_util::TestUtil;

    #[allow(dead_code)] // for quick search
    struct TestByteBlockPool {}
    #[test]
    fn test_append_from_other_pool() -> Result<()> {
        let mut random = random();
        let allocator = AllocatorByteEnum::DA(DirectAllocatorByte::new());
        let mut pool = ByteBlockPool::new(allocator);
        let num_bytes = at_least(&mut random, 2 << 16) as usize;
        let bytes = (&mut random)
            .sample_iter(&Alphanumeric)
            .take(num_bytes)
            .map(char::from)
            .collect::<String>()
            .as_bytes()
            .to_vec();
        pool.append(&bytes)?;
        let bytes_length = bytes.len();

        let allocator = AllocatorByteEnum::DA(DirectAllocatorByte::new());
        let mut another_pool = ByteBlockPool::new(allocator);
        let existing_bytes = vec![0; at_least(&mut random, 500) as usize];
        another_pool.append(&existing_bytes)?;

        // now slice and append to another pool
        let offset = TestUtil::next_int(&mut random, 1, 2 << 15) as usize;
        let mut length = bytes_length - offset;
        if random.random_bool(0.5) {
            length = TestUtil::next_int(&mut random, 1, length as i32) as usize;
        }
        another_pool.append_from_byte_block_pool(&pool, offset as i64, length as i32)?;
        assert_eq!(
            (existing_bytes.len() + length) as i64,
            another_pool.get_position()
        );

        let mut result = vec![0; length];
        let result_length = result.len();
        another_pool.read_bytes(
            existing_bytes.len() as i64,
            &mut result,
            0,
            result_length as i32,
        )?;
        for i in 0..length {
            assert_eq!(bytes[offset + i], result[i], "byte @ index= {}", i);
        }
        Ok(())
    }
    #[test]
    fn test_read_and_write() -> Result<()> {
        let mut random = random();
        let byte_used = Rc::new(RefCell::new(CounterEnum::new_counter(false)));
        let allocator = AllocatorByteEnum::DTA(DirectTrackingAllocatorByte::new(byte_used.clone()));
        let mut pool = ByteBlockPool::new(allocator);
        pool.next_buffer()?;
        let reuse_first = random.random_bool(0.5);
        for _j in 0..2 {
            let mut list: Vec<BytesRef<Vec<u8>>> = Vec::new();
            let max_length = at_least(&mut random, 500) as usize;
            let num_values = at_least(&mut random, 100) as usize;
            let mut bytes_ref_builder: BytesRefBuilder<Vec<u8>> = BytesRefBuilder::new();
            for _i in 0..num_values {
                let value = (&mut random)
                    .sample_iter(&Alphanumeric)
                    .take(max_length)
                    .map(char::from)
                    .collect::<String>();
                let value_copy = value.clone();
                list.push(BytesRef::from_string(&value));
                bytes_ref_builder.copy_chars_with_string(&value_copy);
                pool.append_bytes_ref(bytes_ref_builder.get_bytes_mut_ref())?;
            }
            let mut position = 0;
            let mut builder = BytesRefBuilder::new();
            for expected in list.iter() {
                bytes_ref_builder.set_length(expected.length);
                let bytes_ref_builder_length = bytes_ref_builder.length();
                let value = random.random_range(0..2);
                match value {
                    0 => {
                        pool.read_bytes(
                            position,
                            &mut bytes_ref_builder.get_bytes_mut_ref().bytes,
                            0,
                            bytes_ref_builder_length as i32,
                        )?;
                    },
                    1 => {
                        let mut scratch = BytesRef::new();
                        scratch.bytes = vec![0; bytes_ref_builder_length];
                        pool.set_bytes_ref(
                            &mut builder,
                            &mut scratch,
                            position,
                            bytes_ref_builder.length() as i32,
                        )?;
                        bytes_ref_builder.get_bytes_mut_ref().bytes.copy_from(
                            &scratch.bytes
                                [scratch.offset..(scratch.offset + bytes_ref_builder_length)],
                            0,
                        );
                    },
                    _ => {
                        unreachable!()
                    },
                }
                assert!(bytes_ref_builder.get_bytes_mut_ref().bytes_equals(expected));
                position += bytes_ref_builder.length() as i64;
            }
            pool.reset(random.random_bool(0.5), reuse_first);
            if reuse_first {
                assert_eq!(BYTE_BLOCK_SIZE as i64, pool.get_bytes_used())
            } else {
                assert_eq!(0, pool.get_bytes_used());
                pool.next_buffer()?;
            }
        }
        Ok(())
    }
    #[test]
    fn test_large_random_block() -> Result<()> {
        let mut random = random();
        let byte_used = Rc::new(RefCell::new(CounterEnum::new_counter(false)));
        let allocator = AllocatorByteEnum::DTA(DirectTrackingAllocatorByte::new(byte_used.clone()));
        let mut pool = ByteBlockPool::new(allocator);
        let _ = pool.next_buffer();

        let mut total_bytes = 0;
        let iter = 100;
        let mut iterms: Vec<Vec<u8>> = vec![vec![]; iter];

        let mut size: i32;
        for _i in 0..iter {
            if random.random_bool(0.5) {
                size = TestUtil::next_int(&mut random, 100, 1000);
            } else {
                size = TestUtil::next_int(&mut random, 50000, 100000);
            }
            let mut bytes = vec![0; size as usize];
            random.fill_bytes(&mut bytes);
            let bytes_clone = bytes.clone();
            iterms.push(bytes);
            pool.append_bytes_ref(&BytesRef::from_bytes(bytes_clone))?;
            total_bytes += size;

            // make sure we report the correct position
            assert_eq!(total_bytes as i64, pool.get_position());
        }

        let mut position = 0;
        for expected in iterms {
            let mut actual: Vec<u8> = vec![0; expected.len()];
            let actual_len = actual.len();
            pool.read_bytes(position, &mut actual, 0, actual_len as i32)?;
            assert_eq!(expected, actual);
            position += expected.len() as i64;
        }
        Ok(())
    }

    #[test]
    fn test_too_many_allocs() -> Result<()> {
        // Use a mock allocator that doesn't waste memory
        let allocator = AllocatorByteEnum::<CounterEnumBorrow>::DA(DirectAllocatorByte::new());
        let mut pool = ByteBlockPool::new(allocator);
        pool.next_buffer()?;

        let result = (|| {
            for _ in 0..(i32::MAX / BYTE_BLOCK_SIZE + 1) {
                pool.next_buffer()?;
            }
            Ok(())
        })();

        assert!(matches!(result, Err(LuceneError::NumberOverflow(_))));
        assert!(pool.byte_offset + BYTE_BLOCK_SIZE < pool.byte_offset);

        Ok(())
    }
}
