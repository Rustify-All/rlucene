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
use std::fmt::{Display, Formatter};
use std::rc::Rc;

use crate::core::store::{ByteBuffersDataOutput, DataInput, DataOutput};
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fst_impl::fst::{BytesReader, Either2BytesReader};
use crate::core::util::fst_impl::fst_reader::FstReader;
use crate::core::util::fst_impl::reverse_bytes_reader::ReverseBytesReader;
/// An adapter struct to use [`ByteBuffersDataOutput`] as a
/// [`FSTReader`](FstReader). It allows the FST to be readable immediately after
/// writing
#[derive(Default)]
pub struct ReadWriteDataOutput {
    pub data_output: ByteBuffersDataOutput,
    pub block_bits: i32,
    pub block_size: i32,
    pub block_mask: i32,
    pub byte_buffers: Option<Rc<Vec<Vec<u8>>>>,
    pub byte_buffer: Option<Rc<Vec<u8>>>,
    pub frozen: bool,
    /// Indicates whether the byte_buffer/byte_buffers have been initialized.
    pub finish: bool,
}

impl ReadWriteDataOutput {
    pub(crate) fn new(block_bits: i32) -> Result<Self> {
        let block_size = 1 << block_bits;
        let block_mask = block_size - 1;
        let data_output = ByteBuffersDataOutput::with_reuse(block_bits, block_bits, false)?;
        Ok(Self {
            data_output,
            block_bits,
            block_size,
            block_mask,
            byte_buffers: None,
            byte_buffer: None,
            frozen: false,
            finish: false,
        })
    }

    pub fn freeze(&mut self) -> Result<()> {
        self.frozen = true;
        // We only move the ownership of self.data_output when get_reverse_bytes_reader
        // is called, so that the write_to method can still function correctly.
        Ok(())
    }
}

impl Accountable for ReadWriteDataOutput {
    fn ram_bytes_used(&self) -> Result<i64> {
        self.data_output.ram_bytes_used()
    }
}

impl FstReader for ReadWriteDataOutput {
    type FstBytesReader = Either2BytesReader<BytesReaderImpl, ReverseBytesReader>;

    fn get_reverse_bytes_reader(&self) -> Result<Self::FstBytesReader> {
        if !self.finish {
            return Err(LuceneError::illegal_state(
                "Call ReadWriteDataOutput#init_byte_buffer before Call ReadWriteDataOutput#get_reverse_bytes_reader",
            ));
        }
        if self.byte_buffers.is_some() && self.byte_buffer.is_none() {
            let buffers = self.byte_buffers.as_ref().unwrap().clone();
            Ok(Either2BytesReader::A(BytesReaderImpl::new(
                buffers,
                self.block_bits,
                self.block_size,
                self.block_mask,
            )))
        } else if self.byte_buffer.is_some() && self.byte_buffers.is_none() {
            let buffer = self.byte_buffer.as_ref().unwrap().clone();
            Ok(Either2BytesReader::B(ReverseBytesReader::new(buffer)))
        } else {
            Err(LuceneError::illegal_state("Only one buffer is some"))
        }
    }

    fn write_to(&self, out: &mut impl DataOutput) -> Result<()> {
        debug_assert!(!self.finish);
        // Note: After calling get_reverse_bytes_reader, the ownership of data_output
        // will be moved.
        self.data_output.copy_to(out)
    }

    fn init_reader(&mut self) {
        self.finish = true;
        if self.byte_buffer.is_none() && self.byte_buffers.is_none() {
            let (_, byte_buffers_raw) = self.data_output.get_buffer_list_owner();
            let mut data: Vec<Vec<u8>> = byte_buffers_raw
                .into_iter()
                .map(|b| b.into_inner())
                .collect();

            if data.len() == 1 {
                self.byte_buffer = Some(Rc::new(data.remove(0)));
            } else {
                self.byte_buffers = Some(Rc::new(data));
            }
        }
    }
}

impl DataOutput for ReadWriteDataOutput {
    fn write_byte(&mut self, b: u8) -> Result<()> {
        debug_assert!(!self.frozen);
        DataOutput::write_byte(&mut self.data_output, b)
    }

    fn write_bytes_range(&mut self, b: &[u8], offset: i32, length: i32) -> Result<()> {
        debug_assert!(!self.frozen);
        self.data_output.write_bytes_range(b, offset, length)
    }
}

pub struct BytesReaderImpl {
    byte_buffers: Rc<Vec<Vec<u8>>>,
    next_buffer: i32,
    next_read: i32,
    current: i32,
    block_size: i32,
    block_bits: i32,
    block_mask: i32,
}

impl BytesReaderImpl {
    pub fn new(
        byte_buffers: Rc<Vec<Vec<u8>>>,
        block_bits: i32,
        block_size: i32,
        block_mask: i32,
    ) -> Self {
        Self {
            byte_buffers,
            next_buffer: -1,
            next_read: 0,
            current: 0,
            block_size,
            block_bits,
            block_mask,
        }
    }
}

impl DataInput for BytesReaderImpl {
    fn read_byte(&mut self) -> Result<u8> {
        if self.next_read == -1 {
            self.current = self.next_buffer;
            self.next_buffer -= 1;
            self.next_read = self.block_size - 1;
        }
        let byte = &self.byte_buffers[self.current as usize][self.next_read as usize];
        self.next_read -= 1;
        Ok(*byte)
    }

    fn read_bytes(&mut self, b: &mut [u8], offset: i32, len: i32) -> Result<()> {
        for i in 0..len {
            b[(offset + i) as usize] = self.read_byte()?;
        }
        Ok(())
    }

    fn skip_bytes(&mut self, count: i64) -> Result<()> {
        self.set_position(self.get_position() - count);
        Ok(())
    }
}

impl Display for BytesReaderImpl {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}({})", std::any::type_name::<Self>(), self.block_bits)
    }
}

impl BytesReader for BytesReaderImpl {
    fn get_position(&self) -> i64 {
        (((self.next_buffer + 1) * self.block_size) + self.next_read) as i64
    }

    fn set_position(&mut self, pos: i64) {
        let buffer_index = (pos >> self.block_bits) as i32;
        if self.next_buffer != buffer_index - 1 {
            self.next_buffer = buffer_index - 1;
            self.current = buffer_index;
        }
        self.next_read = (pos & self.block_mask as i64) as i32;
        debug_assert_eq!(
            self.get_position(),
            pos,
            "pos={} get_pos={}",
            pos,
            self.get_position()
        );
    }
}
