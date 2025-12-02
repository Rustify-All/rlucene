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
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};

use crate::core::store::DataInput;
use crate::core::store::byte_buffers_data_input::ByteBuffersDataInput;
use crate::core::store::index_input::IndexInput;
use crate::core::store::random_access_input::RandomAccessInput;
use crate::core::util::error::lucene_error::Result;

/// An [`IndexInput`] implementing [`RandomAccessInput`]
/// and backed by a [`ByteBuffersDataInput`](crate::core::store::byte_buffers_data_input::ByteBuffersDataInput).
pub type ByteBuffersIndexInputRef<'a> = ByteBuffersIndexInput<'a, &'a [u8]>;
pub type ByteBuffersIndexInputOwned = ByteBuffersIndexInput<'static, Vec<u8>>;

pub struct ByteBuffersIndexInput<'a, B: AsRef<[u8]>> {
    data_input: ByteBuffersDataInput<'a, B>,
    resource_description: String,
}
impl<'a, B> ByteBuffersIndexInput<'a, B>
where
    B: AsRef<[u8]> + Clone,
{
    pub fn new(data_input: ByteBuffersDataInput<'a, B>, resource_description: &str) -> Self {
        Self {
            data_input,
            resource_description: resource_description.to_string(),
        }
    }
}

impl<B> DataInput for ByteBuffersIndexInput<'_, B>
where
    B: AsRef<[u8]> + Clone,
{
    fn read_byte(&mut self) -> Result<u8> {
        DataInput::read_byte(&mut self.data_input)
    }

    fn read_bytes(&mut self, b: &mut [u8], offset: i32, len: i32) -> Result<()> {
        DataInput::read_bytes(&mut self.data_input, b, offset, len)
    }

    fn read_bytes_with_buffer(
        &mut self,
        b: &mut [u8],
        offset: i32,
        len: i32,
        _use_buffer: bool,
    ) -> Result<()> {
        self.data_input
            .read_bytes_with_buffer(b, offset, len, false)
    }

    fn read_short(&mut self) -> Result<i16> {
        DataInput::read_short(&mut self.data_input)
    }

    fn read_int(&mut self) -> Result<i32> {
        DataInput::read_int(&mut self.data_input)
    }

    fn read_group_vint(&mut self, dst: &mut [i32], offset: i32) -> Result<()> {
        self.data_input.read_group_vint(dst, offset)
    }

    fn read_vint(&mut self) -> Result<i32> {
        DataInput::read_vint(&mut self.data_input)
    }

    fn read_zint(&mut self) -> Result<i32> {
        DataInput::read_zint(&mut self.data_input)
    }

    fn read_long(&mut self) -> Result<i64> {
        DataInput::read_long(&mut self.data_input)
    }

    fn read_longs(&mut self, dst: &mut [i64], offset: i32, len: i32) -> Result<()> {
        self.data_input.read_longs(dst, offset, len)
    }

    fn read_floats(&mut self, dst: &mut [f32], offset: i32, len: i32) -> Result<()> {
        self.data_input.read_floats(dst, offset, len)
    }

    fn read_vlong(&mut self) -> Result<i64> {
        self.data_input.read_vlong()
    }

    fn read_zlong(&mut self) -> Result<i64> {
        self.data_input.read_zlong()
    }

    fn read_string(&mut self) -> Result<String> {
        self.data_input.read_string()
    }

    fn read_map_of_strings(&mut self) -> Result<HashMap<String, String>> {
        self.data_input.read_map_of_strings()
    }

    fn read_set_of_strings(&mut self) -> Result<HashSet<String>> {
        self.data_input.read_set_of_strings()
    }

    fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
        DataInput::skip_bytes(&mut self.data_input, num_bytes)
    }

    fn is_index_input(&self) -> bool {
        true
    }

    fn seek_in_data_input(&mut self, pos: i64) -> Result<()> {
        debug_assert!(self.is_index_input());
        IndexInput::seek(self, pos)
    }

    fn get_file_pointer_in_data_input(&self) -> i64 {
        debug_assert!(self.is_index_input());
        IndexInput::get_file_pointer(self)
    }
}
impl<B> RandomAccessInput for ByteBuffersIndexInput<'_, B>
where
    B: AsRef<[u8]> + Clone,
{
    fn length(&self) -> i64 {
        RandomAccessInput::length(&self.data_input)
    }

    fn read_byte(&mut self, pos: i64) -> Result<u8> {
        RandomAccessInput::read_byte(&mut self.data_input, pos)
    }

    fn read_short(&mut self, pos: i64) -> Result<i16> {
        RandomAccessInput::read_short(&mut self.data_input, pos)
    }

    fn read_int(&mut self, pos: i64) -> Result<i32> {
        RandomAccessInput::read_int(&mut self.data_input, pos)
    }

    fn read_long(&mut self, pos: i64) -> Result<i64> {
        RandomAccessInput::read_long(&mut self.data_input, pos)
    }

    fn prefetch(&mut self, _pos: i64, _len: i64) -> Result<()> {
        Ok(())
    }
}

impl<B> Display for ByteBuffersIndexInput<'_, B>
where
    B: AsRef<[u8]> + Clone,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.resource_description)
    }
}

impl<B> crate::core::util::clone::TryClone for ByteBuffersIndexInput<'_, B>
where
    B: AsRef<[u8]> + Clone,
{
    fn try_clone(&self) -> Result<Self>
    where
        Self: Sized,
    {
        let slice = self.data_input.slice(0, self.data_input.length())?;
        Ok(ByteBuffersIndexInput::new(
            slice,
            format!("(clone of) {self}").as_str(),
        ))
    }
}

impl<'a, B> IndexInput for ByteBuffersIndexInput<'a, B>
where
    B: AsRef<[u8]> + Clone,
{
    fn get_file_pointer(&self) -> i64 {
        self.data_input.position()
    }

    fn seek(&mut self, pos: i64) -> Result<()> {
        self.data_input.seek(pos)
    }

    fn length(&self) -> i64 {
        self.data_input.length()
    }

    type Slice = ByteBuffersIndexInput<'a, B>;

    fn slice(&self, slice_description: &str, offset: i64, length: i64) -> Result<Self::Slice> {
        Ok(ByteBuffersIndexInput::new(
            self.data_input.slice(offset, length)?,
            slice_description,
        ))
    }

    type RandomAccessSlice = Self::Slice;

    fn random_access_slice(&self, offset: i64, length: i64) -> Result<Self::Slice> {
        self.slice("", offset, length)
    }
}
