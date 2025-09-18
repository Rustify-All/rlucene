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
use std::fmt;
use std::fmt::{Display, Formatter};
use std::io::Cursor;
use std::marker::PhantomData;

use byteorder::{ByteOrder, LE};

use crate::core::store::DataInput;
use crate::core::store::random_access_input::RandomAccessInput;
use crate::core::util::accountable::Accountable;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::group_vint_util::GroupVIntUtil;
use crate::core::util::{ReadableCursorExt, SliceCopyOps};
pub type ByteBuffersDataInputRef<'a> = ByteBuffersDataInput<'a, &'a [u8]>;
pub type ByteBuffersDataInputOwned = ByteBuffersDataInput<'static, Vec<u8>>;

/// A [`DataInput`] implementing [`RandomAccessInput`]
/// and reading data from a list of [`Cursor<Vec<u8>>`](Cursor).
pub struct ByteBuffersDataInput<'a, B: AsRef<[u8]>> {
    /// In Java Lucene, hierarchical data is encapsulated using
    /// List<`java.nio.ByteBuffer`>, where each ByteBuffer limits the
    /// readable data using the limit parameter. In Rust Lucene, however,
    /// this is managed by controlling the readable data using
    /// Cursor#setPosition.
    blocks: Vec<Cursor<B>>,
    block_mask: i32,
    block_bits: i32,
    length: i64,
    offset: i64,
    pos: i64,
    _phantom: PhantomData<&'a B>,
}
/// Reads data from a set of contiguous buffers.
/// All data buffers except for the last one must have an identical number of
/// remaining bytes (which must be a power of two). The last buffer can have an
/// arbitrary remaining length.
impl<'a, B: AsRef<[u8]>> ByteBuffersDataInput<'a, B> {
    pub fn new(blocks: Vec<Cursor<B>>, length: i64) -> Self {
        let (block_bits, block_mask) = if blocks.is_empty() {
            (32, !0)
        } else {
            let block_bytes = blocks[0].get_ref().as_ref().len() as u64;
            let block_bits = block_bytes.trailing_zeros();
            (block_bits, (1 << block_bits) - 1)
        };
        // The initial "position" of this stream is shifted by the position of
        // the first block.
        let offset = blocks.first().map_or(0, |block| block.position()) as i64;
        Self {
            blocks,
            block_mask,
            block_bits: block_bits as i32,
            length,
            offset,
            pos: offset,
            _phantom: PhantomData,
        }
    }
    fn block_index(&self, pos: i64) -> i32 {
        let value = pos >> self.block_bits;
        debug_assert!(value <= i32::MAX as i64);
        value as i32
    }
    fn block_offset(&self, pos: i64) -> i32 {
        let value = pos & (self.block_mask as i64);
        debug_assert!(value <= i32::MAX as i64,);
        value as i32
    }
    fn read_buffer<T, C>(
        &self,
        mut pos: i64,
        len: i32,
        output: &mut [T],
        type_size: i32,
        converter: C,
    ) -> Result<()>
    where
        C: Fn(&[u8]) -> T,
        T: Copy,
    {
        let mut bytes_read = len * type_size;
        // TODO: use This bytes would made additional data copy
        // TODO: we should convert directly from block
        let mut bytes = vec![0; bytes_read as usize];
        let mut bytes_offset = 0;
        while bytes_read > 0 {
            let block_index = self.block_index(pos);
            let block_offset = self.block_offset(pos);

            if block_index as usize >= self.blocks.len()
                || pos + bytes_read as i64 > self.length + self.offset
            {
                return Err(LuceneError::eof(format!("{pos}")));
            }

            let block = self.blocks.get(block_index as usize).unwrap();
            let available =
                block.remain_between(block_offset as u64, block.get_ref().as_ref().len() as u64);

            debug_assert!(available <= i32::MAX as u64);

            debug_assert!(available > 0);
            let chunk = bytes_read.min(available as i32);
            block.read_to_buffer(
                &mut bytes,
                bytes_offset as usize,
                block_offset as u64,
                chunk as usize,
            )?;
            bytes_offset += chunk;
            pos += chunk as i64;
            bytes_read -= chunk;
        }

        debug_assert!(bytes.len() % type_size as usize == 0);
        if type_size == 1 {
            let output_bytes = unsafe {
                std::slice::from_raw_parts_mut(output.as_mut_ptr() as *mut u8, output.len())
            };
            output_bytes.copy_from(&bytes, 0);
        } else {
            output
                .iter_mut()
                .zip(bytes.chunks_exact(type_size as usize).map(converter))
                .for_each(|(out, value)| *out = value);
        }

        Ok(())
    }
    fn do_read_longs(&self, pos: i64, len: i32, output: &mut [i64]) -> Result<()> {
        self.read_buffer(pos, len, output, BitUtil::LONG_BYTES as i32, LE::read_i64)
    }
    fn do_read_bytes(&self, pos: i64, len: i32, output: &mut [u8]) -> Result<()> {
        // This closure is not expected to be called under any circumstances.
        self.read_buffer(pos, len, output, 1, |_| unreachable!())
    }
    fn do_read_ints(&self, pos: i64, len: i32, output: &mut [i32]) -> Result<()> {
        self.read_buffer(pos, len, output, BitUtil::INT_BYTES as i32, LE::read_i32)
    }
    fn do_read_shorts(&self, pos: i64, len: i32, output: &mut [i16]) -> Result<()> {
        self.read_buffer(pos, len, output, BitUtil::SHORT_BYTES as i32, LE::read_i16)
    }
    fn do_read_floats(&self, pos: i64, len: i32, output: &mut [f32]) -> Result<()> {
        self.read_buffer(pos, len, output, BitUtil::FLOAT_BYTES as i32, LE::read_f32)
    }

    pub fn seek(&mut self, position: i64) -> Result<()> {
        self.pos = position + self.offset;
        if position > self.length() {
            self.pos = self.length;
            return Err(LuceneError::eof(format!("{}", self.pos)));
        }
        Ok(())
    }
    pub fn position(&self) -> i64 {
        self.pos - self.offset
    }
}
impl<'a> ByteBuffersDataInput<'a, &'a [u8]> {
    pub fn slice(&self, offset: i64, length: i64) -> Result<ByteBuffersDataInput<'a, &'a [u8]>> {
        if offset < 0 || length < 0 || offset + length > self.length {
            return Err(LuceneError::illegal_argument(format!(
                "slice(offset={}, length={}) is out of bounds: {}",
                offset, length, self.length
            )));
        }
        let blocks = Self::slice_buffer_list(&self.blocks, offset, length);
        Ok(Self::new(blocks, length))
    }
    pub fn slice_buffer_list(
        blocks: &[Cursor<&'a [u8]>],
        offset: i64,
        length: i64,
    ) -> Vec<Cursor<&'a [u8]>> {
        debug_assert!(!blocks.is_empty(), "blocks cannot be empty");

        let abs_start = blocks[0].position() + offset as u64;
        let abs_end = abs_start + length as u64;

        let block_bytes = blocks[0].get_ref().len() as u64;
        debug_assert!(block_bytes.is_power_of_two());
        let block_bits = block_bytes.trailing_zeros() as u64;
        let block_mask = (1u64 << block_bits) - 1;

        let start_block_index = (abs_start / block_bytes) as usize;
        let end_block_index = ((abs_end / block_bytes) as usize).min(blocks.len() - 1);

        // Create a new Cursor for each block and adjust the position and underlying
        // data range as needed
        blocks[start_block_index..=end_block_index]
            .iter()
            .enumerate()
            .map(|(i, block)| {
                let vec_data = *block.get_ref();

                let mut new_cursor = Cursor::new(vec_data);
                if i == 0 {
                    // first block we need to set position to start_offset to keep
                    // al blocks same length
                    let block_offset = abs_start & block_mask;
                    new_cursor.set_position(block_offset);
                } else {
                    // other blocks we can use full block, so we only need set
                    // position to 0
                    new_cursor.set_position(0);
                }
                new_cursor
            })
            .collect()
    }
}

impl<B> Display for ByteBuffersDataInput<'_, B>
where
    B: AsRef<[u8]>,
{
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let blocks_len = self.blocks.len();
        let offset_str = if self.offset == 0 {
            String::new()
        } else {
            format!(" [offset: {}]", self.offset)
        };

        write!(
            f,
            "{} bytes, block size: {}, blocks: {}, position: {}{}",
            self.length,
            1u64 << self.block_bits,
            blocks_len,
            self.position(),
            offset_str
        )
    }
}

impl<B> DataInput for ByteBuffersDataInput<'_, B>
where
    B: AsRef<[u8]>,
{
    fn read_byte(&mut self) -> Result<u8> {
        let mut bytes = [0; 1];
        self.do_read_bytes(self.pos, 1, &mut bytes)?;
        self.pos += 1;
        Ok(bytes[0])
    }
    fn read_bytes(&mut self, arr: &mut [u8], off: i32, len: i32) -> Result<()> {
        self.do_read_bytes(self.pos, len, &mut arr[off as usize..(off + len) as usize])?;
        self.pos += len as i64;
        Ok(())
    }

    fn read_short(&mut self) -> Result<i16> {
        let mut output = [0; 1];
        self.do_read_shorts(self.pos, 1, &mut output)?;
        self.pos += BitUtil::SHORT_BYTES as i64;
        Ok(output[0])
    }

    fn read_int(&mut self) -> Result<i32> {
        let mut output = [0; 1];
        self.do_read_ints(self.pos, 1, &mut output)?;
        self.pos += BitUtil::INT_BYTES as i64;
        Ok(output[0])
    }

    fn read_group_vint(&mut self, dst: &mut [i32], offset: i32) -> Result<()> {
        let block_index = self.block_index(self.pos);
        let block_offset = self.block_offset(self.pos);
        let block = self.blocks.get_mut(block_index as usize).unwrap();
        let remain = block
            .remain_between(block_offset as u64, block.get_ref().as_ref().len() as u64)
            as usize;
        let len = GroupVIntUtil::read_group_vint_i32_with_reader(
            self,
            remain as u64,
            block_offset as i64,
            dst,
            offset,
        )?;
        self.pos += len as i64;
        Ok(())
    }
    fn read_long(&mut self) -> Result<i64> {
        let mut output = [0; 1];
        self.do_read_longs(self.pos, 1, &mut output)?;
        self.pos += BitUtil::LONG_BYTES as i64;
        Ok(output[0])
    }

    fn read_longs(&mut self, dst: &mut [i64], offset: i32, len: i32) -> Result<()> {
        self.do_read_longs(
            self.pos,
            len,
            &mut dst[offset as usize..(offset + len) as usize],
        )?;
        self.pos += len as i64;
        Ok(())
    }

    fn read_floats(&mut self, dst: &mut [f32], offset: i32, len: i32) -> Result<()> {
        self.do_read_floats(
            self.pos,
            len,
            &mut dst[offset as usize..(offset + len) as usize],
        )?;
        self.pos += len as i64;
        Ok(())
    }

    fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
        let skip_to = self.position() + num_bytes;
        self.seek(skip_to)
    }
}
// TODO: In the current implementation, after performing a random read of a
// specific value, it is not possible to use sequential reads to access the next
// value at the subsequent position. TODO: should we support this feature?
impl<B> RandomAccessInput for ByteBuffersDataInput<'_, B>
where
    B: AsRef<[u8]>,
{
    fn length(&self) -> i64 {
        self.length
    }

    fn read_byte(&mut self, pos: i64) -> Result<u8> {
        let pos = pos + self.offset;
        let mut bytes = [0; 1];
        self.do_read_bytes(pos, 1, &mut bytes)?;
        Ok(bytes[0])
    }

    fn read_short(&mut self, pos: i64) -> Result<i16> {
        let pos = pos + self.offset;
        let mut bytes = [0; BitUtil::SHORT_BYTES];
        self.do_read_shorts(pos, 1, &mut bytes)?;
        Ok(bytes[0])
    }

    fn read_int(&mut self, pos: i64) -> Result<i32> {
        let pos = pos + self.offset;
        let mut bytes = [0; BitUtil::INT_BYTES];
        self.do_read_ints(pos, 1, &mut bytes)?;
        Ok(bytes[0])
    }

    fn read_long(&mut self, pos: i64) -> Result<i64> {
        let pos = pos + self.offset;
        let mut bytes = [0; BitUtil::LONG_BYTES];
        self.do_read_longs(pos, 1, &mut bytes)?;
        Ok(bytes[0])
    }

    fn prefetch(&mut self, _pos: i64, _len: i64) -> Result<()> {
        Ok(())
    }
}

impl<B> Accountable for ByteBuffersDataInput<'_, B>
where
    B: AsRef<[u8]>,
{
    fn ram_bytes_used(&self) -> Result<i64> {
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use rand::Rng;
    use rand_xoshiro::Xoroshiro128Plus;
    use rand_xoshiro::rand_core::SeedableRng;

    use crate::core::store::random_access_input::RandomAccessInput;
    use crate::core::store::{ByteBuffersDataOutput, DataInput};
    use crate::core::util::error::lucene_error::Result;
    use crate::test::store::base_data_output_test_case::add_random_data;
    use crate::test::util::lucene_test_case::lucene_test_case_util::is_night_mode;
    use crate::test::util::lucene_test_case::lucene_test_case_util::random;

    #[allow(dead_code)] // for quick search
    struct TestByteBuffersDataInput;

    #[test]
    fn test_sanity() -> Result<()> {
        let mut out = ByteBuffersDataOutput::new();
        let mut o1 = out.get_data_input();
        assert_eq!(0, o1.length());
        let mut result = DataInput::read_byte(&mut o1);
        assert!(result.is_err());

        out.write_byte(1)?;
        // TODO: how to assert o1's length not modified?
        // assert_eq!(0, o1.length());
        let mut o2 = out.get_data_input();
        assert_eq!(1, o2.length());
        assert_eq!(0, o2.position());

        //TODO
        // assert!(o2.ram_bytes_used() > 0)
        assert_eq!(1, DataInput::read_byte(&mut o2)? as i32);
        assert_eq!(1, o2.position());
        assert_eq!(1, RandomAccessInput::read_byte(&mut o2, 0)? as i32);

        result = DataInput::read_byte(&mut o2);
        assert!(result.is_err());
        assert_eq!(1, o2.position());
        Ok(())
    }

    #[test]
    fn test_random_reads() -> Result<()> {
        let mut random = random();
        let mut dst = ByteBuffersDataOutput::new();
        let seed: u64 = random.random();
        let mut random1 = Xoroshiro128Plus::seed_from_u64(seed);
        let max = if is_night_mode() { 1000000 } else { 100000 };
        let reply = add_random_data(&mut dst, &mut random1, max);
        let mut src = dst.get_data_input();
        for action in reply {
            action.verify(&mut src);
        }
        let result = DataInput::read_byte(&mut src);
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn test_random_reads_on_slices() -> Result<()> {
        let mut random = random();
        let reps = random.random_range(1..=20);
        for _i in 0..=reps {
            let mut dst = ByteBuffersDataOutput::new();
            let prefix = vec![0; random.random_range(0..=1024 * 8)];
            let prefix_len = prefix.len() as i64;
            dst.write_bytes(prefix.as_slice())?;
            let seed: u64 = random.random();
            let max = 10000;
            let mut random1 = Xoroshiro128Plus::seed_from_u64(seed);
            let reply = add_random_data(&mut dst, &mut random1, max);
            let suffix = vec![0; random.random_range(0..=1024 * 8)];
            let suffix_len = suffix.len() as i64;
            dst.write_bytes(suffix.as_slice())?;
            let size = dst.size();
            let mut src = dst
                .get_data_input()
                .slice(prefix_len, size - suffix_len - prefix_len)?;
            assert_eq!(0, src.position());
            assert_eq!(size - prefix_len - suffix_len, src.length());
            for action in reply {
                action.verify(&mut src);
            }
            let result = DataInput::read_byte(&mut src);
            assert!(result.is_err());
        }
        Ok(())
    }
    #[test]
    fn test_seek_empty() -> Result<()> {
        let mut dst = ByteBuffersDataOutput::new();
        let mut data_input = dst.get_data_input();
        let mut result = data_input.seek(0);
        assert!(result.is_ok());
        result = data_input.seek(1);
        assert!(result.is_err());
        result = data_input.seek(0);
        assert!(result.is_ok());
        let read_result = DataInput::read_byte(&mut data_input);
        assert!(read_result.is_err());
        Ok(())
    }

    #[test]
    fn test_seek_and_skip() -> Result<()> {
        let mut random = random();
        let reps = random.random_range(1..=20);
        for _i in 0..reps {
            let mut dst = ByteBuffersDataOutput::new();
            let prefix;
            let mut prefix_len: i64 = 0;
            if random.random_bool(0.5) {
                let len = random.random_range(1..=1024 * 8);
                prefix = vec![0; len];
                prefix_len = prefix.len() as i64;
                dst.write_bytes(prefix.as_slice())?;
            }
            let seed: u64 = random.random();
            let max = 1000;
            let mut random1 = Xoroshiro128Plus::seed_from_u64(seed);
            let reply = add_random_data(&mut dst, &mut random1, max);
            let size = dst.size();
            let mut array = dst.get_array_copy();
            array = Vec::from(&array[prefix_len as usize..array.len()]);
            let mut data_input = dst.get_data_input().slice(prefix_len, size - prefix_len)?;
            data_input.seek(0)?;
            for action in &reply {
                action.verify(&mut data_input);
            }
            data_input.seek(0)?;
            for action in &reply {
                action.verify(&mut data_input);
            }
            for _i in 0..1000 {
                let offs = random.random_range(0..=array.len() - 1);
                data_input.seek(offs as i64)?;
                assert_eq!(offs as i64, data_input.position());
                assert_eq!(array[offs], DataInput::read_byte(&mut data_input)?);
            }
            // test skipping
            let max_skip_to = array.len() - 1;
            data_input.seek(0)?;
            // skip chunks of bytes until exhausted
            let mut curr = 0;
            while curr < max_skip_to {
                let skip_to = random.random_range(curr..=max_skip_to);
                let step = skip_to - curr;
                data_input.skip_bytes(step as i64)?;
                assert_eq!(array[skip_to], DataInput::read_byte(&mut data_input)?);
                curr = skip_to + 1;
            }

            data_input.seek(data_input.length())?;
            assert_eq!(data_input.length(), data_input.position());
            let result = DataInput::read_byte(&mut data_input);
            assert!(result.is_err());
        }
        Ok(())
    }
    #[test]
    fn test_slicing_window() -> Result<()> {
        let mut random = random();
        let mut dst = ByteBuffersDataOutput::new();
        assert_eq!(0, dst.get_data_input().slice(0, 0)?.length());
        let random_bytes = vec![0; random.random_range(0..=1024 * 8)];
        dst.write_bytes(random_bytes.as_slice())?;
        let max = dst.size();
        let data_input = dst.get_data_input();
        let mut offset = 0;
        while offset < max {
            assert_eq!(0, data_input.slice(offset, 0)?.length());
            assert_eq!(1, data_input.slice(offset, 1)?.length());

            let window = (max - offset).min(1024);
            assert_eq!(window, data_input.slice(offset, window)?.length());
            offset += 1;
        }
        assert_eq!(0, data_input.slice(max, 0)?.length());
        Ok(())
    }

    #[test]
    fn test_eof_on_array_read_past_buffer_size() -> Result<()> {
        let mut dst = ByteBuffersDataOutput::new();
        let bytes = vec![0; 10];
        dst.write_bytes(bytes.as_slice())?;
        let mut data_input = dst.get_data_input();
        let mut output: Vec<u8> = vec![0; 100];
        let result = DataInput::read_bytes(&mut data_input, &mut output, 0, 100);
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn test_slicing_large_buffers() -> Result<()> {
        // Simulate a "large" (> 4GB) input by duplicating
        // buffers with the same content.
        let mut random = random();
        let mb = 1024 * 1024;
        let page_bytes: Vec<u8> = vec![0; 4 * mb];
        let simulated_length: i64 = random.random_range(0..2018) as i64 + 4 * i32::MAX as i64;
        let mut remaining = simulated_length;
        let mut dst = ByteBuffersDataOutput::new();
        while remaining > 0 {
            let mut block = page_bytes.clone();
            if block.len() > remaining as usize {
                block.truncate(remaining as usize);
            }
            let len = block.len();
            dst.write_bytes(block.as_slice())?;
            remaining -= len as i64;
        }
        let data_input = dst.get_data_input();
        assert_eq!(simulated_length, data_input.length());
        let max = data_input.length();
        let mut offset = 0;
        while offset < max {
            assert_eq!(0, data_input.slice(offset, 0)?.length());
            assert_eq!(1, data_input.slice(offset, 1)?.length());

            let window = (max - offset).min(1024);
            let mut slice = data_input.slice(offset, window)?;
            assert_eq!(window, slice.length());
            // Sanity check of the content against original pages.
            for i in 0..window {
                let index = (offset + i) % page_bytes.len() as i64;
                let expected = page_bytes[index as usize];
                assert_eq!(expected, RandomAccessInput::read_byte(&mut slice, i)?);
            }
            offset += random.random_range(mb..4 * mb) as i64;
        }
        Ok(())
    }
}
