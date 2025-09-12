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
use std::rc::Rc;

use crate::core::store::{DataInput, DataOutput};
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fst_impl::fst::Either2BytesReader;
use crate::core::util::fst_impl::fst_compiler::get_on_heap_reader_writer;
use crate::core::util::fst_impl::fst_reader::FstReader;
use crate::core::util::fst_impl::read_write_data_output::{BytesReaderImpl, ReadWriteDataOutput};
use crate::core::util::fst_impl::reverse_bytes_reader::ReverseBytesReader;
/// Provides storage of finite state machine (FST), using byte array or byte
/// store allocated on heap.
pub struct OnHeapFSTStore {
    /// A [`ReadWriteDataOutput`], used during reading when the FST is very
    /// large (more than 1 GB). If the FST is less than 1 GB then
    /// bytesArray is set instead.
    data_output: Option<ReadWriteDataOutput>,
    ///  Used at read time when the FST fits into a single byte array.
    bytes_array: Option<Rc<Vec<u8>>>,
}

impl OnHeapFSTStore {
    pub fn new(max_block_bits: i32, input: &mut impl DataInput, num_bytes: i64) -> Result<Self> {
        if !(1..=30).contains(&max_block_bits) {
            return Err(LuceneError::illegal_argument(format!(
                "max_block_bits should be in 1..=30; got {max_block_bits}"
            )));
        }

        if num_bytes > (1_i64 << max_block_bits) {
            let mut data_output = get_on_heap_reader_writer(max_block_bits)?;
            data_output.copy_bytes(input, num_bytes)?;
            data_output.freeze()?;
            Ok(Self {
                data_output: Some(data_output),
                bytes_array: None,
            })
        } else {
            let mut bytes_array = vec![0u8; num_bytes as usize];
            let len = bytes_array.len() as i32;
            input.read_bytes(&mut bytes_array, 0, len)?;
            Ok(Self {
                data_output: None,
                bytes_array: Some(Rc::new(bytes_array)),
            })
        }
    }
}
impl Accountable for OnHeapFSTStore {
    fn ram_bytes_used(&self) -> Result<i64> {
        Ok(0)
    }
}

impl FstReader for OnHeapFSTStore {
    type FstBytesReader = Either2BytesReader<
        ReverseBytesReader,
        Either2BytesReader<BytesReaderImpl, ReverseBytesReader>,
    >;

    fn get_reverse_bytes_reader(&self) -> Result<Self::FstBytesReader> {
        if let Some(bytes_array) = &self.bytes_array {
            return Ok(Either2BytesReader::A(ReverseBytesReader::new(
                bytes_array.clone(),
            )));
        }

        if let Some(data_output) = &self.data_output {
            Ok(Either2BytesReader::B(
                data_output.get_reverse_bytes_reader()?,
            ))
        } else {
            Err(LuceneError::illegal_state(
                "OnHeapFSTStore has neither bytes_array nor data_output",
            ))
        }
    }
    // Note: After calling get_reverse_bytes_reader, the ownership of data_output
    // will be moved.
    fn write_to(&self, out: &mut impl DataOutput) -> Result<()> {
        if let Some(data_output) = &self.data_output {
            data_output.write_to(out)?;
        } else if let Some(bytes_array) = &self.bytes_array {
            let len = bytes_array.len();
            debug_assert!(len <= i32::MAX as usize);
            out.write_bytes_range(bytes_array, 0, len as i32)?;
        } else {
            return Err(LuceneError::illegal_state("OnHeapFSTStore is empty"));
        }
        Ok(())
    }

    fn init_reader(&mut self) {
        if let Some(data_output) = &mut self.data_output {
            data_output.init_reader();
        }
    }
}
