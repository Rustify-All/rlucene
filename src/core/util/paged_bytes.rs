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
use crate::core::store::{DataInput, DataOutput, IndexInput};
use crate::core::util::SliceCopyOps;
use crate::core::util::accountable::Accountable;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::fmt;
use std::fmt::{Display, Formatter};
use std::rc::Rc;
use std::sync::Arc;
/// Represents a logical byte[] as a series of pages. You can write-once into
/// the logical byte[] (append only), using copy, and then retrieve slices
/// (BytesRef) into it using fill.
#[derive(Default)]
pub struct PagedBytes {
    blocks: Vec<Vec<u8>>,
    blocks_rc: Option<Vec<Arc<Vec<u8>>>>,
    num_blocks: usize,
    block_size: usize,
    block_bits: usize,
    block_mask: usize,
    did_skip_bytes: bool,
    frozen: bool,
    upto: usize,
    current_block: Option<Vec<u8>>,
    bytes_used_per_block: i64,
}
impl PagedBytes {
    pub fn new(block_bits: usize) -> Self {
        debug_assert!(
            block_bits > 0 && block_bits <= 31,
            "blockBits: {block_bits}"
        );
        let block_size = 1 << block_bits;
        let block_mask = block_size - 1;
        let upto = block_size;
        // TODO: memory calculation not implemented
        let bytes_used_per_block = 0;

        PagedBytes {
            blocks: Vec::with_capacity(16),
            blocks_rc: None,
            num_blocks: 0,
            block_size,
            block_bits,
            block_mask,
            did_skip_bytes: false,
            frozen: false,
            upto,
            current_block: None,
            bytes_used_per_block,
        }
    }
    fn add_block(&mut self, block: Vec<u8>) {
        ArrayUtil::grow_with_len(&mut self.blocks, self.num_blocks + 1);
        self.blocks[self.num_blocks] = block;
        self.num_blocks += 1;
    }
    /// Read this many bytes from in
    pub fn copy_with_input(
        &mut self,
        input: &mut impl IndexInput,
        mut byte_count: usize,
    ) -> Result<()> {
        while byte_count > 0 {
            let mut left = self.block_size - self.upto;
            if left == 0 {
                if let Some(block) = self.current_block.take() {
                    self.add_block(block);
                }
                self.current_block = Some(vec![0u8; self.block_size]);
                self.upto = 0;
                left = self.block_size;
            }
            let current_block = self.current_block.as_mut().unwrap();
            if left < byte_count {
                input.read_bytes_with_buffer(
                    current_block,
                    self.upto as i32,
                    left as i32,
                    false,
                )?;
                self.upto = self.block_size;
                byte_count -= left;
            } else {
                input.read_bytes_with_buffer(
                    current_block,
                    self.upto as i32,
                    byte_count as i32,
                    false,
                )?;
                self.upto += byte_count;
                break;
            }
        }
        Ok(())
    }
    /// Copy `BytesRef` into the pool, setting the output `BytesRef` to the
    /// result.
    ///
    /// Do **not** use this method if `freeze(true)` will be called afterward.
    ///
    /// This only supports `bytes.len() <= block_size`.
    pub fn copy_with_bytes_ref(
        &mut self,
        _bytes: &BytesRef<Vec<u8>>,
        _out: &mut BytesRef<Vec<u8>>,
    ) {
        unimplemented!("not used in Java Lucene")
    }
    /// Commits final byte[], trimming it if necessary and if trim=true
    pub fn freeze(&mut self, trim: bool) -> Result<PagedBytesReader> {
        if self.frozen {
            return Err(LuceneError::illegal_state("already frozen"));
        }
        if self.did_skip_bytes {
            return Err(LuceneError::illegal_state(
                "cannot freeze when copy(BytesRef, BytesRef) was used",
            ));
        }

        if let Some(mut block) = self.current_block.take() {
            if trim && self.upto < self.block_size {
                block.truncate(self.upto);
            }
            self.add_block(block);
        } else {
            self.add_block(Vec::new());
        }

        self.frozen = true;
        self.current_block = None;

        let mut block = Vec::new();
        for i in 0..self.num_blocks {
            block.push(Arc::new(std::mem::take(&mut self.blocks[i])));
        }
        self.blocks_rc = Some(block);

        Ok(PagedBytesReader::new(self))
    }
    pub fn get_pointer(&self) -> i64 {
        if self.current_block.is_none() {
            0
        } else {
            (self.num_blocks as i64 * self.block_size as i64) + self.upto as i64
        }
    }
    /// Copy bytes in, writing the length as a 1 or 2 byte vInt prefix.
    pub fn copy_using_length_prefix(&mut self, _bytes: &BytesRef<Vec<u8>>) -> Result<i64> {
        unimplemented!("not used in Java Lucene")
    }
}
impl Accountable for PagedBytes {
    fn ram_bytes_used(&self) -> Result<i64> {
        // TODO: memory calculation not implemented
        Ok(0)
    }
}
/// Provides methods to read BytesRefs from a frozen PagedBytes.
#[derive(Clone, Default)]
pub struct PagedBytesReader {
    blocks: Vec<Arc<Vec<u8>>>,
    block_bits: usize,
    block_mask: usize,
    block_size: usize,
    bytes_used_per_block: i64,
}

impl PagedBytesReader {
    /// 1<<blockBits must be bigger than biggest single BytesRef slice that will
    /// be pulled
    pub fn new(paged_bytes: &PagedBytes) -> Self {
        PagedBytesReader {
            blocks: paged_bytes.blocks_rc.as_ref().unwrap().clone(),
            block_bits: paged_bytes.block_bits,
            block_mask: paged_bytes.block_mask,
            block_size: paged_bytes.block_size,
            bytes_used_per_block: paged_bytes.bytes_used_per_block,
        }
    }
    /// Gets a slice out of [`PagedBytes`] starting at `start` with the given
    /// `length`.
    ///
    /// If the slice spans across a block boundary, this method will allocate
    /// sufficient resources and copy the paged data.
    ///
    /// Slices spanning more than two blocks are **not supported**.
    pub fn fill_slice(&self, b: &mut BytesRef<Vec<u8>>, start: usize, length: usize) {
        assert!(length <= self.block_size + 1, "length={length}");
        b.length = length;

        if length == 0 {
            return;
        }

        let index = start >> self.block_bits;
        let offset = start & self.block_mask;

        if self.block_size - offset >= length {
            // TODO: always copy here, could we avoid copying
            // Within block
            b.bytes = self.blocks[index].as_ref().clone();
            b.offset = offset;
        } else {
            // Split across two blocks
            let mut new_bytes = vec![0u8; length];
            let first_len = self.block_size - offset;
            new_bytes.copy_from(&self.blocks[index][offset..offset + first_len], 0);
            new_bytes.copy_from(
                &self.blocks[index + 1][..length - first_len],
                self.block_size - offset,
            );

            b.bytes = new_bytes;
            b.offset = 0;
        }
    }
    /// Get the byte at the given offset.
    pub fn get_byte(&self, o: usize) -> u8 {
        let index = o >> self.block_bits;
        let offset = o & self.block_mask;
        self.blocks[index][offset]
    }
    pub fn fill(_b: &mut BytesRef<Rc<Vec<u8>>>, _start: i64) {
        unimplemented!("not used in Java Lucene");
    }
}
impl Accountable for PagedBytesReader {
    fn ram_bytes_used(&self) -> Result<i64> {
        // TODO:  memory calculation not implemented
        Ok(0)
    }
}
impl Display for PagedBytes {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}(blocksize={})",
            std::any::type_name::<Self>(),
            self.block_size
        )
    }
}
/// Input that transparently iterates over pages
pub struct PagedBytesDataInput {
    current_block_index: usize,
    current_block_upto: usize,
    block_size: usize,
    block_bits: usize,
    block_mask: usize,
    blocks: Vec<Arc<Vec<u8>>>,
}

impl PagedBytesDataInput {
    fn new(blocks: &PagedBytes) -> Self {
        debug_assert!(blocks.blocks_rc.is_some());
        Self {
            current_block_index: 0,
            current_block_upto: 0,
            block_size: blocks.block_size,
            block_bits: blocks.block_bits,
            block_mask: blocks.block_mask,
            blocks: blocks.blocks_rc.as_ref().unwrap().clone(),
        }
    }
    /// Returns the current byte position.
    pub fn get_position(&self) -> usize {
        (self.current_block_index * self.block_size) + self.current_block_upto
    }
    /// Seek to a position previously obtained from `get_position()`.
    pub fn set_position(&mut self, pos: usize) {
        self.current_block_index = pos >> self.block_bits;
        self.current_block_upto = pos & self.block_mask;
    }
    fn next_block(&mut self) {
        self.current_block_index += 1;
        self.current_block_upto = 0;
    }
}

impl Display for PagedBytesDataInput {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}(blocks={:?}, current_block_index={}, current_block_upto={})",
            std::any::type_name::<Self>(),
            self.blocks,
            self.current_block_index,
            self.current_block_upto
        )
    }
}

impl DataInput for PagedBytesDataInput {
    fn read_byte(&mut self) -> Result<u8> {
        if self.current_block_upto == self.block_size {
            self.next_block();
        }

        let byte = self.blocks[self.current_block_index][self.current_block_upto];
        self.current_block_upto += 1;
        Ok(byte)
    }

    fn read_bytes(&mut self, b: &mut [u8], offset: i32, len: i32) -> Result<()> {
        assert!(
            b.len() >= (offset + len) as usize,
            "b.len()={}, offset={}, len={}",
            b.len(),
            offset,
            len
        );
        let mut offset = offset as usize;
        let len = len as usize;
        let offset_end = offset + len;

        loop {
            let block = &self.blocks[self.current_block_index];
            let block_left = self.block_size - self.current_block_upto;
            let left = offset_end - offset;

            if block_left < left {
                b.copy_from(
                    &block[self.current_block_upto..self.current_block_upto + block_left],
                    offset,
                );
                self.next_block();
                offset += block_left;
            } else {
                b.copy_from(
                    &block[self.current_block_upto..self.current_block_upto + left],
                    offset,
                );
                self.current_block_upto += left;
                break;
            }
        }
        Ok(())
    }

    fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
        if num_bytes < 0 {
            return Err(LuceneError::illegal_argument(format!(
                "num_bytes must be >= 0, got {num_bytes}"
            )));
        }
        let skip_to = self.get_position() + num_bytes as usize;
        self.set_position(skip_to);
        Ok(())
    }
}

pub struct PagedBytesDataOutput {
    pub(crate) paged_bytes: PagedBytes,
}
impl PagedBytesDataOutput {
    fn new(paged_bytes: PagedBytes) -> Self {
        PagedBytesDataOutput { paged_bytes }
    }
    /// Return the current byte position.
    pub fn get_position(&self) -> i64 {
        self.paged_bytes.get_pointer()
    }
}
impl DataOutput for PagedBytesDataOutput {
    fn write_byte(&mut self, b: u8) -> Result<()> {
        if self.paged_bytes.upto == self.paged_bytes.block_size {
            if let Some(block) = self.paged_bytes.current_block.take() {
                self.paged_bytes.add_block(block);
            }
            self.paged_bytes.current_block = Some(vec![0u8; self.paged_bytes.block_size]);
            self.paged_bytes.upto = 0;
        }

        let block = self.paged_bytes.current_block.as_mut().unwrap();
        block[self.paged_bytes.upto] = b;
        self.paged_bytes.upto += 1;
        Ok(())
    }

    fn write_bytes_range(&mut self, b: &[u8], offset: i32, length: i32) -> Result<()> {
        assert!(
            b.len() >= (offset + length) as usize,
            "b.len={} offset={} length={}",
            b.len(),
            offset,
            length
        );
        if length == 0 {
            return Ok(());
        }

        if self.paged_bytes.upto == self.paged_bytes.block_size {
            if let Some(block) = self.paged_bytes.current_block.take() {
                self.paged_bytes.add_block(block);
            }
            self.paged_bytes.current_block = Some(vec![0u8; self.paged_bytes.block_size]);
            self.paged_bytes.upto = 0;
        }
        let mut offset = offset as usize;
        let length = length as usize;
        let offset_end = offset + length;

        loop {
            let left = offset_end - offset;
            let block_left = self.paged_bytes.block_size - self.paged_bytes.upto;

            let current_block = self.paged_bytes.current_block.as_mut().unwrap();
            if block_left < left {
                current_block.copy_from(&b[offset..offset + block_left], self.paged_bytes.upto);
                let block = self.paged_bytes.current_block.take().unwrap();
                self.paged_bytes.add_block(block);
                self.paged_bytes.current_block = Some(vec![0u8; self.paged_bytes.block_size]);
                self.paged_bytes.upto = 0;
                offset += block_left;
            } else {
                current_block.copy_from(&b[offset..offset + left], self.paged_bytes.upto);
                self.paged_bytes.upto += left;
                break;
            }
        }
        Ok(())
    }
}

/// Returns a DataInput to read values from this PagedBytes instance.
pub fn get_data_input(paged_bytes: &PagedBytes) -> Result<PagedBytesDataInput> {
    if !paged_bytes.frozen {
        return Err(LuceneError::illegal_state(
            "must call freeze() before get_data_input()",
        ));
    }

    Ok(PagedBytesDataInput::new(paged_bytes))
}
/// Returns a DataOutput that you may use to write into this PagedBytes
/// instance. If you do this,  you should not call the other writing methods
/// (eg, copy); results are undefined.
pub fn get_data_output(paged_bytes: PagedBytes) -> Result<PagedBytesDataOutput> {
    if paged_bytes.frozen {
        return Err(LuceneError::illegal_state(
            "cannot get DataOutput after freeze()",
        ));
    }

    Ok(PagedBytesDataOutput::new(paged_bytes))
}

#[cfg(test)]
mod tests {
    use rand::Rng;

    use crate::core::index::BytesRef;
    use crate::core::store::directory::Directory;
    use crate::core::store::{DataInput, DataOutput, IOContext, IndexInput, IndexOutput};
    use crate::core::util::clone::TryClone;
    use crate::core::util::error::lucene_error::Result;
    use crate::core::util::paged_bytes::{PagedBytes, get_data_input, get_data_output};
    use crate::test::util::lucene_test_case::lucene_test_case_util::{
        at_least, is_night_mode, new_directory, random,
    };
    use crate::test::util::test_util::TestUtil;

    #[allow(dead_code)] // for quick search
    struct TestPagedBytes;
    #[test]
    fn test_data_input_output() -> Result<()> {
        let mut random = random();
        let num_iters = at_least(&mut random, 1);

        for _ in 0..num_iters {
            // TODO: BaseDirectoryWrapper not implement
            let dir = new_directory(&mut random)?;
            let block_bits = TestUtil::next_int(&mut random, 1, 20);
            let block_size = 1 << block_bits;
            let mut paged_bytes = PagedBytes::new(block_bits as usize);

            let num_bytes = if is_night_mode() {
                TestUtil::next_int(&mut random, 2, 10_000_000) as usize
            } else {
                TestUtil::next_int(&mut random, 2, 1_000_000) as usize
            };

            let mut answer = vec![0u8; num_bytes];
            random.fill(&mut answer[..]);

            {
                let mut out = dir.create_output("foo", &IOContext::default_io_context()?)?;
                let mut written: usize = 0;
                while written < num_bytes {
                    if random.random_range(0..100) == 7 {
                        out.write_byte(answer[written])?;
                        written += 1;
                    } else {
                        let chunk =
                            std::cmp::min(random.random_range(1..1000), num_bytes - written);
                        out.write_bytes_range(&answer, written as i32, chunk as i32)?;
                        written += chunk;
                    }
                }
            }

            let mut input = dir.open_input("foo", &IOContext::default_io_context()?)?;
            let mut clone_input = input.try_clone()?;

            let len = input.length() as usize;
            paged_bytes.copy_with_input(&mut input, len)?;
            let reader = paged_bytes.freeze(random.random_bool(0.5))?;

            let mut verify = vec![0u8; num_bytes];
            let mut read = 0;
            while read < num_bytes {
                if random.random_range(0..100) == 7 {
                    verify[read] = clone_input.read_byte()?;
                    read += 1;
                } else {
                    let chunk = std::cmp::min(random.random_range(1..1000), num_bytes - read);
                    clone_input.read_bytes(&mut verify, read as i32, chunk as i32)?;
                    read += chunk;
                }
            }

            assert_eq!(answer, verify);

            let mut slice = BytesRef::new();
            for _ in 0..100 {
                let pos = random.random_range(0..num_bytes - 1);
                assert_eq!(reader.get_byte(pos), answer[pos]);

                let len = random.random_range(0..std::cmp::min(block_size + 1, num_bytes - pos));
                reader.fill_slice(&mut slice, pos, len);

                for i in 0..len {
                    assert_eq!(
                        slice.bytes[slice.offset + i],
                        answer[pos + i],
                        "byte mismatch at pos {} + {}",
                        pos,
                        i
                    );
                }
            }
        }

        Ok(())
    }
    // Writes random byte/s into PagedBytes via
    // .getDataOutput(), then verifies with
    // PagedBytes.getDataInput():
    #[test]
    fn test_data_input_output_2() -> Result<()> {
        let mut random = random();
        let num_iters = at_least(&mut random, 1);

        for _ in 0..num_iters {
            let block_bits = TestUtil::next_int(&mut random, 1, 20);
            let block_size = 1 << block_bits;
            let paged_bytes = PagedBytes::new(block_bits as usize);
            let mut out = get_data_output(paged_bytes)?;

            let num_bytes = if is_night_mode() {
                TestUtil::next_int(&mut random, 1, 10_000_000)
            } else {
                TestUtil::next_int(&mut random, 1, 1_000_000)
            } as usize;

            let mut answer = vec![0u8; num_bytes];
            random.fill(&mut answer[..]);

            let mut written = 0;
            while written < num_bytes {
                if random.random_range(0..10) == 7 {
                    out.write_byte(answer[written])?;
                    written += 1;
                } else {
                    let chunk = std::cmp::min(random.random_range(1..1000), num_bytes - written);
                    out.write_bytes_range(&answer, written as i32, chunk as i32)?;
                    written += chunk;
                }
            }

            let reader = out.paged_bytes.freeze(random.random_bool(0.5))?;
            let paged_bytes = std::mem::take(&mut out.paged_bytes);
            let mut input = get_data_input(&paged_bytes)?;

            let mut verify = vec![0u8; num_bytes];
            let mut read = 0;
            while read < num_bytes {
                if random.random_range(0..10) == 7 {
                    verify[read] = input.read_byte()?;
                    read += 1;
                } else {
                    let chunk = std::cmp::min(random.random_range(1..1000), num_bytes - read);
                    input.read_bytes(&mut verify, read as i32, chunk as i32)?;
                    read += chunk;
                }
            }

            assert_eq!(answer, verify);

            let mut slice = BytesRef::new();
            for _ in 0..100 {
                let pos = random.random_range(0..num_bytes - 1);
                let len = random.random_range(0..std::cmp::min(block_size + 1, num_bytes - pos));
                reader.fill_slice(&mut slice, pos, len);
                for byte_upto in 0..len {
                    assert_eq!(
                        slice.bytes[slice.offset + byte_upto],
                        answer[pos + byte_upto],
                        "byte mismatch at pos {} + {}",
                        pos,
                        byte_upto
                    );
                }
            }

            let mut input2 = get_data_input(&paged_bytes)?;
            let mut curr = 0;
            let max_skip_to = num_bytes - 1;
            while curr < max_skip_to {
                let skip_to =
                    TestUtil::next_int(&mut random, curr as i32, max_skip_to as i32) as usize;
                let step = skip_to - curr;
                input2.skip_bytes(step as i64)?;
                assert_eq!(answer[skip_to], input2.read_byte()?);
                curr = skip_to + 1;
            }
        }

        Ok(())
    }
    #[test]
    #[ignore] // memory hole
    fn test_overflow() -> Result<()> {
        let mut random = random();
        // TODO: BaseDirectoryWrapper not implement
        let dir = new_directory(&mut random)?;
        let block_bits = TestUtil::next_int(&mut random, 14, 28);
        let block_size = 1 << block_bits;

        let arr_len = TestUtil::next_int(&mut random, block_size / 2, block_size * 2) as usize;
        let mut arr = vec![0u8; arr_len];
        for (i, byte) in arr.iter_mut().enumerate().take(arr_len) {
            *byte = i as u8;
        }

        let extra = TestUtil::next_int(&mut random, 1, block_size * 3) as i64;
        let num_bytes: i64 = (1i64 << 31) + extra;

        let mut paged_bytes = PagedBytes::new(block_bits as usize);
        {
            let mut out = dir.create_output("foo", &IOContext::default_io_context()?)?;

            let mut written: i64 = 0;
            while written < num_bytes {
                assert_eq!(written, out.get_file_pointer());
                let len = std::cmp::min(arr.len() as i64, num_bytes - written) as usize;
                out.write_bytes_range(&arr, 0, len as i32)?;
                written += len as i64;
            }
            assert_eq!(num_bytes, out.get_file_pointer());
        }

        let mut input = dir.open_input("foo", &IOContext::default_io_context()?)?;
        paged_bytes.copy_with_input(&mut input, num_bytes as usize)?;
        let reader = paged_bytes.freeze(random.random_bool(0.5))?;

        let test_offsets = [
            0_i64,
            i32::MAX as i64,
            num_bytes - 1,
            TestUtil::next_long(&mut random, 1, num_bytes - 2),
        ];

        let mut b = BytesRef::new();
        for &offset in &test_offsets {
            reader.fill_slice(&mut b, offset as usize, 1);
            let expected = arr[(offset % arr.len() as i64) as usize];
            assert_eq!(expected, b.bytes[b.offset], "Mismatch at offset {}", offset);
        }
        Ok(())
    }
    #[test]
    fn test_ram_bytes_used() -> Result<()> {
        // TODO: memory calculation not implemented
        Ok(())
    }
}
