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
use crate::core::store::random_access_input::RandomAccessInput;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::long_values::{Either16LongValues, LongValues, Zeroes};
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::AtomicI64;
use std::sync::atomic::Ordering::SeqCst;

/// Retrieves an instance previously written by `DirectWriter`.
///
/// # See also
/// [`DirectWriter`](crate::core::util::packed::direct_writer::DirectWriter)
pub struct DirectReader;
impl DirectReader {
    pub(crate) const MERGE_BUFFER_SHIFT: i32 = 7;
    const MERGE_BUFFER_SIZE: i32 = 1 << DirectReader::MERGE_BUFFER_SHIFT;
    const MERGE_BUFFER_MASK: i32 = DirectReader::MERGE_BUFFER_SIZE - 1;

    /// Retrieves an instance from the specified slice, decoding
    /// `bits_per_value` for each value.
    // TODO: 参数slice应该实现编译多态 能接受 R或者Arc<Mutex<R>>类型
    // TODO: 另外我们并需要一定要传递slice,而是通过参数传递,那么可能需要不实现LongValues
    pub(crate) fn get_instance<R>(slice: Arc<Mutex<R>>, bits_per_value: i32) -> DirectPackedEnum<R>
    where
        R: RandomAccessInput,
    {
        Self::get_instance_with_offset(slice, bits_per_value, 0)
    }
    /// Retrieves an instance from the specified `offset` of the given slice,
    /// decoding `bits_per_value` for each value.
    pub(crate) fn get_instance_with_offset<R>(
        slice: Arc<Mutex<R>>,
        bits_per_value: i32,
        offset: i64,
    ) -> DirectPackedEnum<R>
    where
        R: RandomAccessInput,
    {
        match bits_per_value {
            1 => DirectPackedEnum::A(DirectPackedReader1::new(slice, offset)),
            2 => DirectPackedEnum::B(DirectPackedReader2::new(slice, offset)),
            4 => DirectPackedEnum::C(DirectPackedReader4::new(slice, offset)),
            8 => DirectPackedEnum::D(DirectPackedReader8::new(slice, offset)),
            12 => DirectPackedEnum::E(DirectPackedReader12::new(slice, offset)),
            16 => DirectPackedEnum::F(DirectPackedReader16::new(slice, offset)),
            20 => DirectPackedEnum::G(DirectPackedReader20::new(slice, offset)),
            24 => DirectPackedEnum::H(DirectPackedReader24::new(slice, offset)),
            28 => DirectPackedEnum::I(DirectPackedReader28::new(slice, offset)),
            32 => DirectPackedEnum::J(DirectPackedReader32::new(slice, offset)),
            40 => DirectPackedEnum::K(DirectPackedReader40::new(slice, offset)),
            48 => DirectPackedEnum::L(DirectPackedReader48::new(slice, offset)),
            56 => DirectPackedEnum::M(DirectPackedReader56::new(slice, offset)),
            64 => DirectPackedEnum::N(DirectPackedReader64::new(slice, offset)),
            _ => unreachable!(),
        }
    }
    /// Retrieves an instance specialized for merges, typically faster for
    /// sequential access but slower for random access.
    pub(crate) fn get_merge_instance<R>(
        slice: Arc<Mutex<R>>,
        bits_per_value: i32,
        num_values: i64,
    ) -> DirectPackedEnum<R>
    where
        R: RandomAccessInput,
    {
        Self::get_merge_instance_with_base_offset(slice, bits_per_value, 0, num_values)
    }
    /// Retrieves an instance specialized for merges, typically faster for
    /// sequential access.
    pub(crate) fn get_merge_instance_with_base_offset<R>(
        slice: Arc<Mutex<R>>,
        bits_per_value: i32,
        base_offset: i64,

        num_values: i64,
    ) -> DirectPackedEnum<R>
    where
        R: RandomAccessInput,
    {
        DirectPackedEnum::O(LongValuesImpl::new(
            slice,
            bits_per_value,
            num_values,
            base_offset,
        ))
    }
}

pub(crate) struct LongValuesImpl<R>
where
    R: RandomAccessInput,
{
    slice: Arc<Mutex<R>>,
    bits_per_value: i32,
    num_values: i64,
    base_offset: i64,
    buffer: Vec<AtomicI64>,
    block_index: AtomicI64,
}
impl<R> LongValuesImpl<R>
where
    R: RandomAccessInput,
{
    fn new(
        slice: Arc<Mutex<R>>,
        bits_per_value: i32,
        num_values: i64,
        base_offset: i64,
    ) -> LongValuesImpl<R> {
        let mut buffer = Vec::with_capacity(DirectReader::MERGE_BUFFER_SIZE as usize);
        for _ in 0..DirectReader::MERGE_BUFFER_SIZE as usize {
            buffer.push(AtomicI64::new(-1));
        }
        LongValuesImpl {
            slice,
            bits_per_value,
            num_values,
            base_offset,
            buffer,
            block_index: AtomicI64::new(-1),
        }
    }

    fn fill_buffer(&self, index: i64) -> Result<()> {
        // NOTE: we're not allowed to read more than 3 bytes past the last value
        let mut slice = self.slice.lock();
        if index >= self.num_values - DirectReader::MERGE_BUFFER_SIZE as i64 {
            // 128 values left or less
            let slow_instance = DirectReader::get_instance_with_offset(
                self.slice.clone(),
                self.bits_per_value,
                self.base_offset,
            );
            drop(slice);
            let num_values_last_block = (self.num_values - index) as usize;
            for i in 0..num_values_last_block {
                self.buffer[i].store(slow_instance.get(index + i as i64)?, SeqCst);
            }
        } else if (self.bits_per_value & 0x07) == 0 {
            // bitsPerValue is a multiple of 8
            let bytes_per_value = self.bits_per_value / u8::BITS as i32;
            let mask = if self.bits_per_value == 64 {
                !0i64
            } else {
                (1i64 << self.bits_per_value) - 1
            };
            let mut offset = self.base_offset + (index * self.bits_per_value as i64) / 8;
            for i in 0..DirectReader::MERGE_BUFFER_SIZE as usize {
                if self.bits_per_value > i32::BITS as i32 {
                    self.buffer[i].store(slice.read_long(offset)? & mask, SeqCst);
                } else if self.bits_per_value > i16::BITS as i32 {
                    self.buffer[i].store((slice.read_int(offset)? as u32 as i64) & mask, SeqCst);
                } else if self.bits_per_value > i8::BITS as i32 {
                    self.buffer[i].store(slice.read_short(offset)? as u16 as i64, SeqCst);
                } else {
                    self.buffer[i].store(slice.read_byte(offset)? as i64, SeqCst);
                }
                offset += bytes_per_value as i64;
            }
        } else if self.bits_per_value < 8 {
            // bitsPerValue is 1, 2 or 4
            let values_per_long = u64::BITS as i32 / self.bits_per_value;
            let mask = (1i64 << self.bits_per_value) - 1;
            let mut offset = self.base_offset + (index * self.bits_per_value as i64) / 8;
            let mut i = 0;
            for _ in 0..(2 * self.bits_per_value) {
                let bits = slice.read_long(offset)?;
                for j in 0..values_per_long {
                    self.buffer[i].store(
                        (bits as u64 >> (j * self.bits_per_value)) as i64 & mask,
                        SeqCst,
                    );
                    i += 1;
                }
                offset += BitUtil::LONG_BYTES as i64;
            }
        } else {
            // bitsPerValue is 12, 20 or 28; read values 2 by 2
            let num_bytes_for_2_values = (self.bits_per_value * 2) / i8::BITS as i32;
            let mask = (1i64 << self.bits_per_value) - 1;
            let mut offset = self.base_offset + (index * self.bits_per_value as i64) / 8;
            for i in (0..DirectReader::MERGE_BUFFER_SIZE as usize).step_by(2) {
                let l = if num_bytes_for_2_values > BitUtil::INT_BYTES as i32 {
                    slice.read_long(offset)?
                } else {
                    slice.read_int(offset)? as i64
                };
                self.buffer[i].store(l & mask, SeqCst);
                self.buffer[i + 1].store((l as u64 >> self.bits_per_value) as i64 & mask, SeqCst);
                offset += num_bytes_for_2_values as i64;
            }
        }
        Ok(())
    }
}
impl<R> LongValues for LongValuesImpl<R>
where
    R: RandomAccessInput,
{
    fn get(&self, index: i64) -> Result<i64> {
        debug_assert!(index >= 0);
        debug_assert!(index < self.num_values);
        let block_index = index >> DirectReader::MERGE_BUFFER_SHIFT;
        if self.block_index.load(SeqCst) != block_index {
            self.fill_buffer(block_index << DirectReader::MERGE_BUFFER_SHIFT)?;
            self.block_index.store(block_index, SeqCst);
        }
        Ok(self.buffer[(index & DirectReader::MERGE_BUFFER_MASK as i64) as usize].load(SeqCst))
    }
}

pub(crate) struct DirectPackedReader1<R>
where
    R: RandomAccessInput,
{
    input: Arc<Mutex<R>>,
    offset: i64,
}
impl<R> DirectPackedReader1<R>
where
    R: RandomAccessInput,
{
    pub fn new(input: Arc<Mutex<R>>, offset: i64) -> DirectPackedReader1<R> {
        DirectPackedReader1 { input, offset }
    }
}
impl<R> LongValues for DirectPackedReader1<R>
where
    R: RandomAccessInput,
{
    fn get(&self, index: i64) -> Result<i64> {
        let shift = (index & 7) as i32;
        let mut slice = self.input.lock();
        let result = (slice.read_byte(self.offset + (index >> 3))? >> shift) & 0x1;
        Ok(result as i64)
    }
}

pub(crate) struct DirectPackedReader2<R>
where
    R: RandomAccessInput,
{
    input: Arc<Mutex<R>>,
    offset: i64,
}

impl<R> DirectPackedReader2<R>
where
    R: RandomAccessInput,
{
    pub fn new(input: Arc<Mutex<R>>, offset: i64) -> Self {
        DirectPackedReader2 { input, offset }
    }
}

impl<R> LongValues for DirectPackedReader2<R>
where
    R: RandomAccessInput,
{
    fn get(&self, index: i64) -> Result<i64> {
        debug_assert!(index >= 0);
        let shift = ((index & 3) as i32) << 1;
        let mut slice = self.input.lock();
        let byte = slice.read_byte(self.offset + (index >> 2))?;
        let result = (byte >> shift) & 0x3;
        Ok(result as i64)
    }
}

pub(crate) struct DirectPackedReader4<R>
where
    R: RandomAccessInput,
{
    input: Arc<Mutex<R>>,
    offset: i64,
}

impl<R> DirectPackedReader4<R>
where
    R: RandomAccessInput,
{
    pub fn new(input: Arc<Mutex<R>>, offset: i64) -> Self {
        DirectPackedReader4 { input, offset }
    }
}

impl<R> LongValues for DirectPackedReader4<R>
where
    R: RandomAccessInput,
{
    fn get(&self, index: i64) -> Result<i64> {
        debug_assert!(index >= 0);
        let shift = ((index & 1) as i32) << 2;
        let mut slice = self.input.lock();

        let byte = slice.read_byte(self.offset + (index >> 1))?;
        let result = (byte >> shift) & 0xF;
        Ok(result as i64)
    }
}

pub(crate) struct DirectPackedReader8<R>
where
    R: RandomAccessInput,
{
    input: Arc<Mutex<R>>,
    offset: i64,
}

impl<R> DirectPackedReader8<R>
where
    R: RandomAccessInput,
{
    pub fn new(input: Arc<Mutex<R>>, offset: i64) -> Self {
        DirectPackedReader8 { input, offset }
    }
}

impl<R> LongValues for DirectPackedReader8<R>
where
    R: RandomAccessInput,
{
    fn get(&self, index: i64) -> Result<i64> {
        debug_assert!(index >= 0);
        let mut slice = self.input.lock();

        let byte = slice.read_byte(self.offset + index)?;
        let result = byte;
        Ok(result as i64)
    }
}

pub(crate) struct DirectPackedReader12<R>
where
    R: RandomAccessInput,
{
    input: Arc<Mutex<R>>,
    offset: i64,
}

impl<R> DirectPackedReader12<R>
where
    R: RandomAccessInput,
{
    pub fn new(input: Arc<Mutex<R>>, offset: i64) -> Self {
        DirectPackedReader12 { input, offset }
    }
}

impl<R> LongValues for DirectPackedReader12<R>
where
    R: RandomAccessInput,
{
    fn get(&self, index: i64) -> Result<i64> {
        debug_assert!(index >= 0);
        let off = (index * 12) >> 3;
        let shift = ((index & 1) as i32) << 2;
        let mut slice = self.input.lock();

        let short_val = slice.read_short(self.offset + off)?;
        let result = ((short_val as u16) >> shift) & 0xFFF;
        Ok(result as i64)
    }
}

pub(crate) struct DirectPackedReader16<R>
where
    R: RandomAccessInput,
{
    input: Arc<Mutex<R>>,
    offset: i64,
}

impl<R> DirectPackedReader16<R>
where
    R: RandomAccessInput,
{
    pub fn new(input: Arc<Mutex<R>>, offset: i64) -> Self {
        DirectPackedReader16 { input, offset }
    }
}

impl<R> LongValues for DirectPackedReader16<R>
where
    R: RandomAccessInput,
{
    fn get(&self, index: i64) -> Result<i64> {
        debug_assert!(index >= 0);
        let mut slice = self.input.lock();

        let result = slice.read_short(self.offset + (index << 1))? as u16;
        Ok(result as i64)
    }
}
pub(crate) struct DirectPackedReader20<R>
where
    R: RandomAccessInput,
{
    input: Arc<Mutex<R>>,
    offset: i64,
}

impl<R> DirectPackedReader20<R>
where
    R: RandomAccessInput,
{
    pub fn new(input: Arc<Mutex<R>>, offset: i64) -> Self {
        DirectPackedReader20 { input, offset }
    }
}

impl<R> LongValues for DirectPackedReader20<R>
where
    R: RandomAccessInput,
{
    fn get(&self, index: i64) -> Result<i64> {
        debug_assert!(index >= 0);
        let off = (index * 20) >> 3;
        let shift = ((index & 1) as i32) << 2;
        let mut slice = self.input.lock();

        let int_val = slice.read_int(self.offset + off)?;
        let result = (int_val >> shift) & 0xFFFFF;
        Ok(result as i64)
    }
}

pub(crate) struct DirectPackedReader24<R>
where
    R: RandomAccessInput,
{
    input: Arc<Mutex<R>>,
    offset: i64,
}

impl<R> DirectPackedReader24<R>
where
    R: RandomAccessInput,
{
    pub fn new(input: Arc<Mutex<R>>, offset: i64) -> Self {
        DirectPackedReader24 { input, offset }
    }
}

impl<R> LongValues for DirectPackedReader24<R>
where
    R: RandomAccessInput,
{
    fn get(&self, index: i64) -> Result<i64> {
        debug_assert!(index >= 0);
        let mut slice = self.input.lock();

        let int_val = slice.read_int(self.offset + index * 3)?;
        let result = int_val & 0xFFFFFF;
        Ok(result as i64)
    }
}

pub(crate) struct DirectPackedReader28<R>
where
    R: RandomAccessInput,
{
    input: Arc<Mutex<R>>,
    offset: i64,
}

impl<R> DirectPackedReader28<R>
where
    R: RandomAccessInput,
{
    pub fn new(input: Arc<Mutex<R>>, offset: i64) -> Self {
        DirectPackedReader28 { input, offset }
    }
}

impl<R> LongValues for DirectPackedReader28<R>
where
    R: RandomAccessInput,
{
    fn get(&self, index: i64) -> Result<i64> {
        debug_assert!(index >= 0);
        let off = (index * 28) >> 3;
        let shift = ((index & 1) as i32) << 2;
        let mut slice = self.input.lock();

        let int_val = slice.read_int(self.offset + off)?;
        let result = (int_val >> shift) & 0xFFFFFFF;
        Ok(result as i64)
    }
}

pub(crate) struct DirectPackedReader32<R>
where
    R: RandomAccessInput,
{
    input: Arc<Mutex<R>>,
    offset: i64,
}

impl<R> DirectPackedReader32<R>
where
    R: RandomAccessInput,
{
    pub fn new(input: Arc<Mutex<R>>, offset: i64) -> Self {
        DirectPackedReader32 { input, offset }
    }
}

impl<R> LongValues for DirectPackedReader32<R>
where
    R: RandomAccessInput,
{
    fn get(&self, index: i64) -> Result<i64> {
        debug_assert!(index >= 0);
        let mut slice = self.input.lock();

        let int_val = slice.read_int(self.offset + (index << 2))?;
        let result = int_val as u32;
        Ok(result as i64)
    }
}

pub(crate) struct DirectPackedReader40<R>
where
    R: RandomAccessInput,
{
    input: Arc<Mutex<R>>,
    offset: i64,
}

impl<R> DirectPackedReader40<R>
where
    R: RandomAccessInput,
{
    pub fn new(input: Arc<Mutex<R>>, offset: i64) -> Self {
        DirectPackedReader40 { input, offset }
    }
}

impl<R> LongValues for DirectPackedReader40<R>
where
    R: RandomAccessInput,
{
    fn get(&self, index: i64) -> Result<i64> {
        debug_assert!(index >= 0);
        let mut slice = self.input.lock();

        let long_val = slice.read_long(self.offset + index * 5)?;
        let result = long_val & 0xFFFFFFFFFF;
        Ok(result)
    }
}

pub(crate) struct DirectPackedReader48<R>
where
    R: RandomAccessInput,
{
    input: Arc<Mutex<R>>,
    offset: i64,
}

impl<R> DirectPackedReader48<R>
where
    R: RandomAccessInput,
{
    pub fn new(input: Arc<Mutex<R>>, offset: i64) -> Self {
        DirectPackedReader48 { input, offset }
    }
}

impl<R> LongValues for DirectPackedReader48<R>
where
    R: RandomAccessInput,
{
    fn get(&self, index: i64) -> Result<i64> {
        debug_assert!(index >= 0);
        let mut slice = self.input.lock();

        let long_val = slice.read_long(self.offset + index * 6)?;
        let result = long_val & 0xFFFFFFFFFFFF;
        Ok(result)
    }
}

pub(crate) struct DirectPackedReader56<R>
where
    R: RandomAccessInput,
{
    input: Arc<Mutex<R>>,
    offset: i64,
}

impl<R> DirectPackedReader56<R>
where
    R: RandomAccessInput,
{
    pub fn new(input: Arc<Mutex<R>>, offset: i64) -> Self {
        DirectPackedReader56 { input, offset }
    }
}

impl<R> LongValues for DirectPackedReader56<R>
where
    R: RandomAccessInput,
{
    fn get(&self, index: i64) -> Result<i64> {
        debug_assert!(index >= 0);
        let mut slice = self.input.lock();

        let long_val = slice.read_long(self.offset + index * 7)?;
        let result = long_val & 0xFFFFFFFFFFFFFF;
        Ok(result)
    }
}

pub(crate) struct DirectPackedReader64<R>
where
    R: RandomAccessInput,
{
    input: Arc<Mutex<R>>,
    offset: i64,
}

impl<R> DirectPackedReader64<R>
where
    R: RandomAccessInput,
{
    pub fn new(input: Arc<Mutex<R>>, offset: i64) -> Self {
        DirectPackedReader64 { input, offset }
    }
}

impl<R> LongValues for DirectPackedReader64<R>
where
    R: RandomAccessInput,
{
    fn get(&self, index: i64) -> Result<i64> {
        debug_assert!(index >= 0);
        let mut slice = self.input.lock();

        let result = slice.read_long(self.offset + (index << 3))?;
        Ok(result)
    }
}

pub(crate) type DirectPackedEnum<R> = Either16LongValues<
    DirectPackedReader1<R>,
    DirectPackedReader2<R>,
    DirectPackedReader4<R>,
    DirectPackedReader8<R>,
    DirectPackedReader12<R>,
    DirectPackedReader16<R>,
    DirectPackedReader20<R>,
    DirectPackedReader24<R>,
    DirectPackedReader28<R>,
    DirectPackedReader32<R>,
    DirectPackedReader40<R>,
    DirectPackedReader48<R>,
    DirectPackedReader56<R>,
    DirectPackedReader64<R>,
    LongValuesImpl<R>,
    Zeroes,
>;
