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
use std::collections::VecDeque;
use std::io::{Cursor, Seek};

use byteorder::WriteBytesExt;

use crate::core::store::DataInput;
use crate::core::store::byte_buffers_data_input::{
    ByteBuffersDataInput, ByteBuffersDataInputOwned,
};
use crate::core::store::data_output::DataOutput;
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::{ReadableCursorExt, WritableCursorExt};

/// A [`DataOutput`] storing data in a list of [`Cursor<Vec<u8>>`](Cursor).
pub struct ByteBuffersDataOutput {
    //In Rust Lucene, all data within each block is considered valid.
    // However, in Java Lucene, the valid data range can be controlled
    // by the `limit` parameter of the `java.nio.ByteBuffer` encapsulation.
    blocks: VecDeque<Cursor<Vec<u8>>>,
    max_bits_per_block: i32,
    block_bits: i32,
    ram_bytes_used: i64,
    // it is necessary when we want to reuse the data output
    current_block_index: i32,
    reuse: bool,
}
#[cfg(test)]
impl Clone for ByteBuffersDataOutput {
    fn clone(&self) -> Self {
        Self {
            blocks: self.blocks.clone(),
            max_bits_per_block: self.max_bits_per_block,
            block_bits: self.block_bits,
            ram_bytes_used: self.ram_bytes_used,
            current_block_index: self.current_block_index,
            reuse: self.reuse,
        }
    }
}
impl Default for ByteBuffersDataOutput {
    // It is used for padding
    fn default() -> Self {
        Self {
            max_bits_per_block: Self::DEFAULT_MAX_BITS_PER_BLOCK,
            block_bits: Self::DEFAULT_MIN_BITS_PER_BLOCK,
            blocks: VecDeque::new(),
            ram_bytes_used: 0,
            current_block_index: 0,
            reuse: false,
        }
    }
}

impl ByteBuffersDataOutput {
    /// Smallest `minBitsPerBlock` allowed
    pub const LIMIT_MIN_BITS_PER_BLOCK: i32 = 1;
    /// Largest `maxBitsPerBlock` allowed
    pub const LIMIT_MAX_BITS_PER_BLOCK: i32 = 31;
    ///Maximum number of blocks at the current `blockBits` block size before we
    /// increase the block size (and thus decrease the number of blocks).
    pub const MAX_BLOCKS_BEFORE_BLOCK_EXPANSION: i32 = 100;
    ///Default `maxBitsPerBlock`
    pub const DEFAULT_MAX_BITS_PER_BLOCK: i32 = 26;
    /// Default `minBitsPerBlock`
    pub const DEFAULT_MIN_BITS_PER_BLOCK: i32 = 10;

    pub fn new() -> Self {
        let result = Self::with_reuse(
            Self::DEFAULT_MIN_BITS_PER_BLOCK,
            Self::DEFAULT_MAX_BITS_PER_BLOCK,
            false,
        );
        debug_assert!(result.is_ok());
        result.unwrap()
    }
    ///Creates a new output with all defaults.
    pub fn new_resettable_instance() -> Self {
        let result = Self::with_reuse(
            Self::DEFAULT_MIN_BITS_PER_BLOCK,
            Self::DEFAULT_MAX_BITS_PER_BLOCK,
            true,
        );
        debug_assert!(result.is_ok());
        result.unwrap()
    }
    /// Expert: Creates a new output with custom parameters.
    ///
    /// # Arguments
    /// * `min_bits_per_block` - Minimum bits per block.
    /// * `max_bits_per_block` - Maximum bits per block.
    /// * `reuse` - Reuse this Instance.
    pub fn with_reuse(
        min_bits_per_block: i32,
        max_bits_per_block: i32,
        reuse: bool,
    ) -> Result<Self> {
        if min_bits_per_block < Self::LIMIT_MIN_BITS_PER_BLOCK {
            return Err(LuceneError::illegal_argument(format!(
                "minBitsPerBlock ({}) too small, must be at least {}",
                min_bits_per_block,
                Self::LIMIT_MIN_BITS_PER_BLOCK
            )));
        }
        if max_bits_per_block > Self::LIMIT_MAX_BITS_PER_BLOCK {
            return Err(LuceneError::illegal_argument(format!(
                "maxBitsPerBlock ({}) too large, must not exceed {}",
                max_bits_per_block,
                Self::LIMIT_MAX_BITS_PER_BLOCK
            )));
        }
        if min_bits_per_block > max_bits_per_block {
            return Err(LuceneError::illegal_argument(format!(
                "minBitsPerBlock ({min_bits_per_block}) cannot exceed maxBitsPerBlock ({max_bits_per_block})"
            )));
        }
        let block = Cursor::new(vec![0u8; 1 << min_bits_per_block]);
        let mut blocks = VecDeque::new();
        blocks.push_back(block);
        Ok(Self {
            max_bits_per_block,
            block_bits: min_bits_per_block,
            blocks,
            ram_bytes_used: 0,
            current_block_index: 0,
            reuse,
        })
    }
    /// Creates a new output, suitable for writing a file of approximately
    /// `expected_size` bytes.
    ///
    /// Memory allocation will be optimized based on the `expected_size` hint to
    /// reduce overhead for larger files.
    ///
    /// # Arguments
    /// * `expected_size` - Estimated size of the output file.
    pub fn with_size(expected_size: i64) -> Result<Self> {
        let block_bits = compute_block_size_bits_for(expected_size);
        Self::with_reuse(block_bits, Self::DEFAULT_MAX_BITS_PER_BLOCK, false)
    }

    fn append_block(&mut self) {
        if self.blocks.len() > Self::MAX_BLOCKS_BEFORE_BLOCK_EXPANSION as usize
            && self.block_bits < self.max_bits_per_block
        {
            self.rewrite_to_block_size(self.block_bits + 1);
            if self
                .blocks
                .get_mut(self.current_block_index as usize)
                .unwrap()
                .remain()
                > 0
            {
                return;
            }
        }
        let required_block_size = 1 << self.block_bits;
        self.blocks
            .push_back(Cursor::new(vec![0u8; required_block_size]));
        // TODO: self.ramBytesUsed += 0;
        self.ram_bytes_used += 0;
        self.current_block_index += 1;
    }
    fn rewrite_to_block_size(&mut self, target_block_bits: i32) {
        debug_assert!(target_block_bits <= self.max_bits_per_block);
        self.rewrite_blocks(target_block_bits);
        // TODO:
        self.ram_bytes_used += 0;
    }
    // create larger blocks and copy data from smaller blocks
    // TODO: the first old_block's data could be reused ,first do expansion by
    // `push_back` and then move to tail and continue copy the second
    // old_block's data to it
    pub fn rewrite_blocks(&mut self, target_block_bits: i32) {
        debug_assert!(target_block_bits > self.block_bits);
        self.block_bits = target_block_bits;
        let block_size = 1 << self.block_bits;
        let mut new_block = Cursor::new(vec![0; block_size]);
        let mut old_block_count = self.blocks.len();
        while let Some(mut old_block) = self.blocks.pop_front() {
            // read from head
            old_block.set_position(0);
            while old_block.remain() > 0 {
                let mut available_space = new_block.remain();
                if available_space == 0 {
                    self.blocks.push_back(new_block);
                    new_block = Cursor::new(vec![0; block_size]);
                    available_space = 1 << self.block_bits;
                }
                let bytes_to_copy = available_space.min(old_block.remain()) as usize;
                let old_position = old_block.position() as usize;
                let old_data = &old_block.get_ref()[old_position..old_position + bytes_to_copy];
                debug_assert!(
                    new_block.remain() as usize >= bytes_to_copy,
                    "Insufficient space in new_block: remaining={}, required={}",
                    new_block.remain(),
                    bytes_to_copy
                );
                new_block.write_from_slice(old_data).unwrap();
                old_block.set_position((old_position + bytes_to_copy) as u64);
            }
            old_block_count -= 1;
            if old_block_count == 0 {
                break;
            }
        }
        if new_block.position() > 0 {
            self.blocks.push_back(new_block);
        }
        debug_assert!(self.blocks.len() <= i32::MAX as usize);
        self.current_block_index = (self.blocks.len() - 1) as i32;
    }
    /// Copies the current content of this object into another [`DataOutput`].
    pub(crate) fn copy_to<DA: DataOutput>(&self, output: &mut DA) -> Result<()> {
        debug_assert!(!self.blocks.is_empty());
        for (index, block) in self.blocks.iter().enumerate() {
            if index == self.current_block_index as usize {
                let end = block.position() as usize;
                output.write_bytes_range(block.get_ref(), 0, end as i32)?;
            } else {
                let len = block.get_ref().len();
                debug_assert!(len <= i32::MAX as usize);
                debug_assert!(len == 1 << self.block_bits);
                output.write_bytes_with_len(block.get_ref(), len as i32)?;
            }
        }
        Ok(())
    }
    /// The number of bytes written to this output so far.
    pub fn size(&self) -> i64 {
        let mut size = 0;
        let block_count = self.current_block_index + 1;
        if block_count >= 1 {
            let full_block_size = (block_count - 1) as i64 * self.block_size();
            let last_block_size = self
                .blocks
                .get(self.current_block_index as usize)
                .unwrap()
                .position();
            debug_assert!(last_block_size <= i64::MAX as u64);
            size = full_block_size + last_block_size as i64;
        }
        size
    }
    fn block_size(&self) -> i64 {
        1 << self.block_bits
    }
    /// Resets this object to a clean (zero-size) state and publishes any
    /// currently allocated buffers for reuse according to the reuse
    /// strategy provided in the constructor.
    ///
    /// # Warning
    /// Sharing byte buffers for reads and writes is dangerous and may lead to
    /// hard-to-debug issues. Use with great caution.
    pub fn reset(&mut self) {
        if self.reuse {
            for block in &mut self.blocks {
                let _ = block.rewind();
            }
        }
        self.current_block_index = 0;
        self.ram_bytes_used = 0;
    }

    /// Returns a list of read-only views of [`Cursor<Vec<u8>>`](Cursor) blocks
    /// over the current content written to the output.
    pub fn to_buffer_list_ref(&self) -> (i64, Vec<Cursor<&[u8]>>) {
        let data = self
            .blocks
            .iter()
            .map(|cursor| {
                let slice: &[u8] = cursor.get_ref().as_slice();
                let mut new_cursor = Cursor::new(slice);
                new_cursor.set_position(0);
                new_cursor
            })
            .collect();
        (self.size(), data)
    }
    /// Moves the blocks out of the current object, transferring ownership.
    pub fn get_buffer_list_owner(&mut self) -> (i64, Vec<Cursor<Vec<u8>>>) {
        let size = self.size();
        let old_blocks = std::mem::take(&mut self.blocks);

        let data = old_blocks
            .into_iter()
            .map(|mut cursor| {
                cursor.set_position(0);
                cursor
            })
            .collect();

        (size, data)
    }
    pub fn get_writeable_buffer_list(&mut self) -> Vec<&mut Cursor<Vec<u8>>> {
        todo!()
    }
    /// Returns a contiguous array containing the current content written to the
    /// output. The returned array is always a copy and can be safely
    /// mutated. # Note
    /// If reset is called immediately after get_array_copy,
    /// or if ByteBuffersDataOutput will no longer be used,
    /// then [`try_get_array_ownership`](Self::try_get_array_ownership) should
    /// be used instead. If the number of blocks is 1, we take ownership to
    /// avoid copying. See
    /// [`try_get_array_ownership`](Self::try_get_array_ownership)
    pub fn get_array_copy(&self) -> Vec<u8> {
        let mut buffer = Vec::with_capacity(self.size() as usize);
        for block in &self.blocks {
            let end = block.position() as usize;
            buffer.extend_from_slice(&block.get_ref()[..end]);
        }
        buffer
    }
    /// See [`get_array_copy`](Self::get_array_copy) Before use this method.
    pub fn try_get_array_ownership(&mut self) -> Vec<u8> {
        match self.blocks.len() {
            0 => vec![0u8; 1 << self.block_bits],
            // If the number of blocks is 1, take ownership to avoid copying.
            1 => {
                let cursor = self.blocks.front_mut().unwrap();
                let end = cursor.position() as usize;

                let old_vec = std::mem::replace(cursor.get_mut(), vec![0u8; 1 << self.block_bits]);

                old_vec.into_iter().take(end).collect()
            },
            _ => {
                let mut buffer = Vec::with_capacity(self.size() as usize);
                for block in &self.blocks {
                    let end = block.position() as usize;
                    buffer.extend_from_slice(&block.get_ref()[..end]);
                }
                buffer
            },
        }
    }

    /// Returns a `ByteBuffersDataInput` backed by references to internal
    /// buffers.
    ///
    /// This method borrows the internal buffer data as `&[u8]`,
    /// and constructs a read-only view over the current written content.
    ///
    /// The returned input is only valid as long as `self` is not mutated.
    pub fn get_data_input_ref(&mut self) -> ByteBuffersDataInput<'_, &[u8]> {
        let (length, data) = self.to_buffer_list_ref();
        ByteBuffersDataInput::new(data, length)
    }

    /// Returns a `ByteBuffersDataInput` that owns its internal buffers.
    ///
    /// This method consumes the written buffer content into owned `[u8]`
    /// vectors, and constructs a self-contained input stream that can
    /// outlive `self`.
    ///
    /// Use this when the data needs to be retained or passed independently.
    pub fn get_data_input_owner(&mut self) -> ByteBuffersDataInputOwned {
        let (length, data) = self.get_buffer_list_owner();
        ByteBuffersDataInput::new(data, length)
    }

    fn append_block_if_needed(&mut self) -> i64 {
        let mut last_block = self
            .blocks
            .get_mut(self.current_block_index as usize)
            .unwrap();
        if last_block.remain() == 0 {
            if self.reuse && (self.current_block_index as usize) < self.blocks.len() - 1 {
                self.current_block_index += 1;
                last_block = self
                    .blocks
                    .get_mut(self.current_block_index as usize)
                    .unwrap();
            } else {
                self.append_block();
                // it is safe to get by `back_mut` because blocks are not reused
                last_block = self.blocks.back_mut().unwrap();
            }
        }
        let value = last_block.remain();
        debug_assert!(value <= i64::MAX as u64);
        value as i64
    }
    #[cfg(debug_assertions)]
    pub fn write_bytes(&mut self, b: &[u8]) -> Result<()> {
        debug_assert!(b.len() <= u32::MAX as usize);
        self.write_bytes_range(b, 0, b.len() as i32)
    }

    #[cfg(debug_assertions)]
    pub fn write_byte(&mut self, b: u8) -> Result<()> {
        self.write_bytes_range(&[b], 0, 1)
    }
}

impl DataOutput for ByteBuffersDataOutput {
    fn write_byte(&mut self, b: u8) -> Result<()> {
        self.append_block_if_needed();
        let last_block = self
            .blocks
            .get_mut(self.current_block_index as usize)
            .unwrap();
        Ok(last_block.write_u8(b)?)
    }

    fn write_bytes_with_len(&mut self, b: &[u8], len: i32) -> Result<()> {
        self.write_bytes_range(b, 0, len)
    }

    fn write_bytes_range(&mut self, b: &[u8], mut offset: i32, mut length: i32) -> Result<()> {
        while length > 0 {
            let available_space = self.append_block_if_needed();
            let last_block = self
                .blocks
                .get_mut(self.current_block_index as usize)
                .unwrap();
            let chunk = available_space.min(length as i64);
            debug_assert!(chunk <= i32::MAX as i64);
            last_block.write_from(b, offset, chunk as i32)?;
            length -= chunk as i32;
            offset += chunk as i32;
        }
        Ok(())
    }

    fn write_int(&mut self, i: i32) -> Result<()> {
        let value = i.to_le_bytes();
        self.write_bytes_range(&value, 0, 4)
    }

    fn write_short(&mut self, i: i16) -> Result<()> {
        let value = i.to_le_bytes();
        self.write_bytes_range(&value, 0, 2)
    }

    fn write_long(&mut self, i: i64) -> Result<()> {
        let value = i.to_le_bytes();
        self.write_bytes_range(&value, 0, 8)
    }

    fn write_string(&mut self, s: &str) -> Result<()> {
        let bytes = s.as_bytes();
        let length = bytes.len();
        debug_assert!(length <= i32::MAX as usize);
        self.write_vint(length as i32)?;
        self.write_bytes_range(bytes, 0, length as i32)
    }

    fn copy_bytes(&mut self, input: &mut impl DataInput, mut num_bytes: i64) -> Result<()> {
        while num_bytes > 0 {
            let available_space = self.append_block_if_needed();
            let last_block = self
                .blocks
                .get_mut(self.current_block_index as usize)
                .unwrap();
            let bytes_to_copy = available_space.min(num_bytes);
            debug_assert!(bytes_to_copy <= i32::MAX as i64);

            let current_pos = last_block.position();
            debug_assert!(current_pos <= u32::MAX as u64);
            let current_block_mut = last_block.get_mut();
            input.read_bytes(current_block_mut, current_pos as i32, bytes_to_copy as i32)?;
            last_block.set_position(current_pos + bytes_to_copy as u64);
            num_bytes -= bytes_to_copy;
        }
        Ok(())
    }
}

impl Accountable for ByteBuffersDataOutput {
    fn ram_bytes_used(&self) -> Result<i64> {
        todo!()
    }
}

fn compute_block_size_bits_for(bytes: i64) -> i32 {
    let avg_block_size =
        (bytes / ByteBuffersDataOutput::MAX_BLOCKS_BEFORE_BLOCK_EXPANSION as i64) as u64;
    let power_of_two = avg_block_size.next_power_of_two();
    if power_of_two == 0 {
        return ByteBuffersDataOutput::DEFAULT_MIN_BITS_PER_BLOCK;
    }
    let mut block_bits = power_of_two.trailing_zeros();
    block_bits = block_bits.min(ByteBuffersDataOutput::DEFAULT_MAX_BITS_PER_BLOCK as u32);
    block_bits = block_bits.max(ByteBuffersDataOutput::DEFAULT_MIN_BITS_PER_BLOCK as u32);
    debug_assert!(block_bits <= i32::MAX as u32);
    block_bits as i32
}

#[cfg(feature = "not_required_in_rust_lucene")]
fn write_long_string(_byte_len: usize, _s: String) {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use rand::Rng;

    use crate::core::store::data_output::DataOutput;
    use crate::core::store::{ByteArrayDataInput, ByteBuffersDataOutput};
    use crate::core::util::error::lucene_error::Result;
    use crate::test::store::base_data_output_test_case::{BaseDataOutputTestCase, add_random_data};
    use crate::test::util::lucene_test_case::lucene_test_case_util::is_night_mode;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{random, random_from_seed};

    struct TestByteBuffersDataOutput;
    impl BaseDataOutputTestCase for TestByteBuffersDataOutput {
        type DO = ByteBuffersDataOutput;

        fn new_instance(&self) -> Result<Self::DO> {
            Ok(ByteBuffersDataOutput::new_resettable_instance())
        }

        fn get_bytes(&mut self, instance: Self::DO) -> Vec<u8> {
            instance.get_array_copy()
        }
    }

    #[test]
    fn test_reuse() -> Result<()> {
        let mut random = random();
        let mut o = ByteBuffersDataOutput::with_reuse(
            ByteBuffersDataOutput::DEFAULT_MIN_BITS_PER_BLOCK,
            ByteBuffersDataOutput::DEFAULT_MAX_BITS_PER_BLOCK,
            true,
        )?;
        // add some random data first
        let gen_seed: u64 = random.random();
        let mut random1 = random_from_seed(gen_seed);
        let mut random2 = random_from_seed(gen_seed);
        let add_count = random.random_range(1000..=5000);
        add_random_data(&mut o, &mut random1, add_count);
        let dta = match random.random_bool(0.5) {
            true => o.get_array_copy(),
            false => o.try_get_array_ownership(),
        };

        o.reset();
        add_random_data(&mut o, &mut random2, add_count);
        match random.random_bool(0.5) {
            true => {
                assert_eq!(dta, o.get_array_copy());
            },
            false => {
                assert_eq!(dta, o.try_get_array_ownership());
            },
        }
        Ok(())
    }
    #[test]
    fn test_constructor_with_expected_size() -> Result<()> {
        let mut random = random();
        let mut o = ByteBuffersDataOutput::with_size(0)?;
        o.write_byte(0)?;
        let (_length, mut result) = o.to_buffer_list_ref();
        let capacity = result.get_mut(0).unwrap().get_ref().len();
        assert_eq!(
            1 << ByteBuffersDataOutput::DEFAULT_MIN_BITS_PER_BLOCK,
            capacity
        );

        let mb = 1024 * 1024;
        let expected_size: i64 = random.random_range(mb..mb * 1024);
        let mut o = ByteBuffersDataOutput::with_size(expected_size)?;
        let _ = o.write_byte(0);
        let (_length, mut result) = o.to_buffer_list_ref();
        let cap = result.get_mut(0).unwrap().get_ref().len();
        assert!(
            ((cap >> 1) * ByteBuffersDataOutput::MAX_BLOCKS_BEFORE_BLOCK_EXPANSION as usize)
                < expected_size as usize
        );
        assert!(
            cap * ByteBuffersDataOutput::MAX_BLOCKS_BEFORE_BLOCK_EXPANSION as usize
                >= expected_size as usize
        );
        Ok(())
    }

    #[test]
    fn test_randomized_writes() -> Result<()> {
        let mut test = TestByteBuffersDataOutput;
        let mut random = random();
        // here could use any DataInput impl because this test does not test
        // ByteArrayDataInput
        test.test_randomized_writes::<ByteArrayDataInput<Vec<u8>>, _>(&mut random)
    }

    #[test]
    fn test_illegal_min_bits_per_block() {
        let o = ByteBuffersDataOutput::with_reuse(
            ByteBuffersDataOutput::LIMIT_MIN_BITS_PER_BLOCK - 1,
            ByteBuffersDataOutput::DEFAULT_MAX_BITS_PER_BLOCK,
            false,
        );
        assert!(o.is_err());
    }
    #[test]
    fn test_illegal_max_bits_per_block() {
        let o = ByteBuffersDataOutput::with_reuse(
            ByteBuffersDataOutput::DEFAULT_MIN_BITS_PER_BLOCK,
            ByteBuffersDataOutput::LIMIT_MIN_BITS_PER_BLOCK + 1,
            false,
        );
        assert!(o.is_err());
    }
    #[test]
    fn test_illegal_bits_per_block_range() {
        let o = ByteBuffersDataOutput::with_reuse(20, 19, false);
        assert!(o.is_err());
    }
    #[test]
    fn test_sanity() -> Result<()> {
        let mut random = random();
        let case = TestByteBuffersDataOutput;
        let mut o = case.new_instance()?;

        assert_eq!(o.size(), 0);
        assert_eq!(o.get_array_copy().len(), 0);
        // TODO
        // assert_eq!(o.ram_bytes_used(), 0);

        o.write_byte(1)?;
        assert_eq!(o.size(), 1);
        // TODO
        // assert!(o.ram_bytes_used() > 0);
        assert_eq!(o.get_array_copy(), vec![1]);

        o.write_bytes_with_len(&[2, 3, 4], 3)?;
        assert_eq!(o.size(), 4);

        match random.random_bool(0.5) {
            true => {
                assert_eq!(o.get_array_copy(), vec![1, 2, 3, 4]);
            },
            false => {
                assert_eq!(o.try_get_array_ownership(), vec![1, 2, 3, 4]);
            },
        }
        Ok(())
    }
    #[test]
    fn test_large_array_add() -> Result<()> {
        let mut random = random();
        let mut o = ByteBuffersDataOutput::new_resettable_instance();
        let mb = 1024 * 1024;
        let mut bytes = if is_night_mode() {
            let size = random.random_range(5 * mb..=15 * mb);
            vec![0u8; size]
        } else {
            let size = random.random_range(mb / 2..=mb);
            vec![0u8; size]
        };

        bytes.iter_mut().for_each(|byte| *byte = random.random());
        let offset = random.random_range(0..=100);
        let len = bytes.len() - offset;
        o.write_bytes_range(&bytes, offset as i32, len as i32)?;
        assert_eq!(len as i64, o.size());
        let expected = bytes[offset..offset + len].to_vec();
        assert_eq!(expected, o.get_array_copy());
        match random.random_bool(0.5) {
            true => {
                assert_eq!(expected, o.get_array_copy());
            },
            false => {
                assert_eq!(expected, o.try_get_array_ownership());
            },
        }
        Ok(())
    }
    #[test]
    fn test_copy_bytes_on_heap() -> Result<()> {
        let mut random = random();
        let mut bytes = vec![0u8; 1024 * 8 + 10];
        random.fill(&mut bytes[..]);
        let offset = random.random_range(0..=100);
        let len = bytes.len() - offset;
        let bytes_clone = bytes.clone();
        let mut input = ByteArrayDataInput::with_range(bytes, offset, len);

        let mut o = ByteBuffersDataOutput::with_reuse(
            ByteBuffersDataOutput::DEFAULT_MIN_BITS_PER_BLOCK,
            ByteBuffersDataOutput::DEFAULT_MAX_BITS_PER_BLOCK,
            false,
        )?;
        o.copy_bytes(&mut input, len as i64)?;
        let expected = bytes_clone[offset..offset + len].to_vec();
        match random.random_bool(0.5) {
            true => {
                assert_eq!(o.get_array_copy(), expected);
            },
            false => {
                assert_eq!(o.try_get_array_ownership(), expected);
            },
        }
        Ok(())
    }
    #[test]
    fn test_copy_bytes_on_direct_byte_buffer() -> Result<()> {
        let mut random = random();
        let mut bytes = vec![0u8; 1024 * 8 + 10];
        random.fill(&mut bytes[..]);
        let offset = random.random_range(0..=100);
        let len = bytes.len() - offset;
        let bytes_clone = bytes.clone();
        let mut input = ByteArrayDataInput::with_range(bytes, offset, len);
        let mut o = ByteBuffersDataOutput::with_reuse(
            ByteBuffersDataOutput::DEFAULT_MIN_BITS_PER_BLOCK,
            ByteBuffersDataOutput::DEFAULT_MAX_BITS_PER_BLOCK,
            false,
        )?;
        o.copy_bytes(&mut input, len as i64)?;
        let expected = bytes_clone[offset..offset + len].to_vec();
        match random.random_bool(0.5) {
            true => {
                assert_eq!(o.get_array_copy(), expected);
            },
            false => {
                assert_eq!(o.try_get_array_ownership(), expected);
            },
        }
        Ok(())
    }
    #[test]
    fn test_to_buffer_list_returns_read_only_buffers() -> Result<()> {
        // this test is not required in Rust Lucene
        Ok(())
    }
    #[test]
    fn test_to_writeable_buffer_list_returns_original_buffers() -> Result<()> {
        // this test is not required in Rust Lucene
        Ok(())
    }

    #[test]
    #[allow(dead_code)]
    fn test_ram_bytes_used() {
        // TODO
    }
    #[allow(dead_code)]
    fn compute_ram_bytes_used() {
        // TODO
    }
}
