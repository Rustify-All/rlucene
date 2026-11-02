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
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::long_values::{LongValues, Zeroes};

/// Retrieves an instance previously written by [`DirectWriter`](crate::core::util::packed::direct_writer::DirectWriter).
///
/// # See also
/// [`DirectWriter`](crate::core::util::packed::direct_writer::DirectWriter)
pub struct DirectReader;
impl DirectReader {
  pub(crate) const MERGE_BUFFER_SHIFT: usize = 7;
  const MERGE_BUFFER_SIZE: usize = 1 << DirectReader::MERGE_BUFFER_SHIFT;
  const MERGE_BUFFER_MASK: usize = DirectReader::MERGE_BUFFER_SIZE - 1;

  /// Retrieves an instance from the specified slice, decoding
  /// `bits_per_value` for each value.
  pub fn get_instance<R>(slice: R, bits_per_value: i32) -> Result<DirectPackedEnum<R>>
  where
    R: RandomAccessInput,
  {
    Self::get_instance_with_offset(Some(slice), bits_per_value, 0)
  }
  /// Retrieves an instance from the specified `offset` of the given slice,
  /// decoding `bits_per_value` for each value.
  pub fn get_instance_with_offset<R>(
    slice: Option<R>,
    bits_per_value: i32,
    offset: usize,
  ) -> Result<DirectPackedEnum<R>>
  where
    R: RandomAccessInput,
  {
    let v = match bits_per_value {
      1 => DirectPackedEnum::Direct1(DirectPackedReader1::new(slice, offset)),
      2 => DirectPackedEnum::Direct2(DirectPackedReader2::new(slice, offset)),
      4 => DirectPackedEnum::Direct4(DirectPackedReader4::new(slice, offset)),
      8 => DirectPackedEnum::Direct8(DirectPackedReader8::new(slice, offset)),
      12 => DirectPackedEnum::Direct12(DirectPackedReader12::new(slice, offset)),
      16 => DirectPackedEnum::Direct16(DirectPackedReader16::new(slice, offset)),
      20 => DirectPackedEnum::Direct20(DirectPackedReader20::new(slice, offset)),
      24 => DirectPackedEnum::Direct24(DirectPackedReader24::new(slice, offset)),
      28 => DirectPackedEnum::Direct28(DirectPackedReader28::new(slice, offset)),
      32 => DirectPackedEnum::Direct32(DirectPackedReader32::new(slice, offset)),
      40 => DirectPackedEnum::Direct40(DirectPackedReader40::new(slice, offset)),
      48 => DirectPackedEnum::Direct48(DirectPackedReader48::new(slice, offset)),
      56 => DirectPackedEnum::Direct56(DirectPackedReader56::new(slice, offset)),
      64 => DirectPackedEnum::Direct64(DirectPackedReader64::new(slice, offset)),
      _ => {
        return Err(LuceneError::illegal_argument(format!(
          "unsupported bits_per_value: {}",
          bits_per_value
        )));
      },
    };
    Ok(v)
  }
  /// Retrieves an instance specialized for merges, typically faster for
  /// sequential access but slower for random access.
  pub fn get_merge_instance<R>(
    slice: R,
    bits_per_value: i32,
    num_values: usize,
  ) -> DirectPackedEnum<R>
  where
    R: RandomAccessInput,
  {
    Self::get_merge_instance_with_base_offset(Some(slice), bits_per_value, 0, num_values)
  }
  /// Retrieves an instance specialized for merges, typically faster for
  /// sequential access.
  pub fn get_merge_instance_with_base_offset<R>(
    slice: Option<R>,
    bits_per_value: i32,
    base_offset: usize,
    num_values: usize,
  ) -> DirectPackedEnum<R>
  where
    R: RandomAccessInput,
  {
    DirectPackedEnum::Merge(LongValuesImpl::new(
      slice,
      bits_per_value,
      num_values,
      base_offset,
    ))
  }
}

pub struct LongValuesImpl<R> {
  slice: Option<R>,
  bits_per_value: i32,
  num_values: usize,
  base_offset: usize,
  buffer: Box<[i64]>,
  block_index: Option<usize>,
}
impl<R> LongValuesImpl<R> {
  fn new(
    slice: Option<R>,
    bits_per_value: i32,
    num_values: usize,
    base_offset: usize,
  ) -> LongValuesImpl<R> {
    let buffer = vec![-1; DirectReader::MERGE_BUFFER_SIZE];
    LongValuesImpl {
      slice,
      bits_per_value,
      num_values,
      base_offset,
      buffer: buffer.into_boxed_slice(),
      block_index: None,
    }
  }
}

impl<R> LongValuesImpl<R>
where
  R: RandomAccessInput,
{
  fn fill_buffer(&mut self, index: usize, slice: Option<&R>) -> Result<()> {
    // NOTE: we're not allowed to read more than 3 bytes past the last value
    let slice = match slice {
      Some(slice) => slice,
      None => self
        .slice
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("input is empty"))?,
    };
    let buffer = &mut self.buffer[..DirectReader::MERGE_BUFFER_SIZE];
    let bits_per_value_usize = self.bits_per_value as usize;
    if index + DirectReader::MERGE_BUFFER_SIZE >= self.num_values {
      // 128 values left or less
      let slow_instance =
        DirectReader::get_instance_with_offset(None, self.bits_per_value, self.base_offset)?;
      let num_values_last_block = self.num_values - index;
      macro_rules! decode_tail {
        ($reader:expr) => {
          for (i, value) in buffer[..num_values_last_block].iter_mut().enumerate() {
            *value = $reader.read_from_slice_shared(index + i, Some(slice))?;
          }
        };
      }
      // Keep the concrete reader for the whole tail, preserving scalar read order.
      match &slow_instance {
        DirectPackedEnum::Direct1(reader) => decode_tail!(reader),
        DirectPackedEnum::Direct2(reader) => decode_tail!(reader),
        DirectPackedEnum::Direct4(reader) => decode_tail!(reader),
        DirectPackedEnum::Direct8(reader) => decode_tail!(reader),
        DirectPackedEnum::Direct12(reader) => decode_tail!(reader),
        DirectPackedEnum::Direct16(reader) => decode_tail!(reader),
        DirectPackedEnum::Direct20(reader) => decode_tail!(reader),
        DirectPackedEnum::Direct24(reader) => decode_tail!(reader),
        DirectPackedEnum::Direct28(reader) => decode_tail!(reader),
        DirectPackedEnum::Direct32(reader) => decode_tail!(reader),
        DirectPackedEnum::Direct40(reader) => decode_tail!(reader),
        DirectPackedEnum::Direct48(reader) => decode_tail!(reader),
        DirectPackedEnum::Direct56(reader) => decode_tail!(reader),
        DirectPackedEnum::Direct64(reader) => decode_tail!(reader),
        reader => decode_tail!(reader),
      }
    } else if (self.bits_per_value & 0x07) == 0 {
      // bitsPerValue is a multiple of 8
      let bytes_per_value = bits_per_value_usize / 8;
      let mask = if self.bits_per_value == 64 {
        !0i64
      } else {
        (1i64 << self.bits_per_value) - 1
      };
      let mut offset = self.base_offset + (index * bits_per_value_usize) / 8;
      let bits_per_value = self.bits_per_value;
      for value in buffer.iter_mut() {
        if bits_per_value > i32::BITS as i32 {
          *value = slice.read_long(offset)? & mask;
        } else if bits_per_value > i16::BITS as i32 {
          *value = (slice.read_int(offset)? as u32 as i64) & mask;
        } else if bits_per_value > i8::BITS as i32 {
          *value = slice.read_short(offset)? as u16 as i64;
        } else {
          *value = slice.read_byte(offset)? as i64;
        }
        offset += bytes_per_value;
      }
    } else if self.bits_per_value < 8 {
      // bitsPerValue is 1, 2 or 4
      let values_per_long = u64::BITS as usize / bits_per_value_usize;
      let mask = (1i64 << self.bits_per_value) - 1;
      let mut offset = self.base_offset + (index * bits_per_value_usize) / 8;
      let mut i = 0;
      for _ in 0..(2 * bits_per_value_usize) {
        let bits = slice.read_long(offset)?;
        for j in 0..values_per_long {
          buffer[i] = (bits as u64 >> (j * bits_per_value_usize)) as i64 & mask;
          i += 1;
        }
        offset += BitUtil::LONG_BYTES;
      }
    } else {
      // bitsPerValue is 12, 20 or 28; read values 2 by 2
      let num_bytes_for_2_values = bits_per_value_usize * 2 / 8;
      let mask = (1i64 << self.bits_per_value) - 1;
      let mut offset = self.base_offset + (index * bits_per_value_usize) / 8;
      for i in (0..DirectReader::MERGE_BUFFER_SIZE).step_by(2) {
        let l = if num_bytes_for_2_values > BitUtil::INT_BYTES {
          slice.read_long(offset)?
        } else {
          slice.read_int(offset)? as i64
        };
        buffer[i] = l & mask;
        buffer[i + 1] = (l as u64 >> self.bits_per_value) as i64 & mask;
        offset += num_bytes_for_2_values;
      }
    }
    Ok(())
  }
}
impl<R> LongValues for LongValuesImpl<R>
where
  R: RandomAccessInput,
{
  fn get_mut(&mut self, index: usize) -> Result<i64> {
    self.read_from_slice(index, None)
  }
}
impl<R> FromSlice<R> for LongValuesImpl<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice(&mut self, index: usize, slice: Option<&R>) -> Result<i64> {
    debug_assert!(index < self.num_values);
    let block_index = index >> DirectReader::MERGE_BUFFER_SHIFT;
    let do_fill = match self.block_index {
      Some(b) => b != block_index,
      None => true,
    };
    if do_fill {
      self.fill_buffer(block_index << DirectReader::MERGE_BUFFER_SHIFT, slice)?;
      self.block_index = Some(block_index);
    }
    Ok(self.buffer[index & DirectReader::MERGE_BUFFER_MASK])
  }
}

pub struct DirectPackedReader1<R> {
  input: Option<R>,
  offset: usize,
}
impl<R> DirectPackedReader1<R> {
  pub fn new(input: Option<R>, offset: usize) -> DirectPackedReader1<R> {
    DirectPackedReader1 { input, offset }
  }
}
impl<R> LongValues for DirectPackedReader1<R>
where
  R: RandomAccessInput,
{
  fn get(&self, index: usize) -> Result<i64> {
    self.read_from_slice_shared(index, None)
  }
}
impl<R> FromSliceShared<R> for DirectPackedReader1<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice_shared(&self, index: usize, slice: Option<&R>) -> Result<i64> {
    let shift = index & 7;
    let slice = match slice {
      Some(slice) => slice,
      None => self
        .input
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("input is empty"))?,
    };
    let result = (slice.read_byte(self.offset + (index >> 3))? >> shift) & 0x1;
    Ok(result as i64)
  }
}
impl<R> FromSlice<R> for DirectPackedReader1<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice(&mut self, index: usize, slice: Option<&R>) -> Result<i64> {
    self.read_from_slice_shared(index, slice)
  }
}

pub struct DirectPackedReader2<R> {
  input: Option<R>,
  offset: usize,
}

impl<R> DirectPackedReader2<R> {
  pub fn new(input: Option<R>, offset: usize) -> Self {
    DirectPackedReader2 { input, offset }
  }
}

impl<R> LongValues for DirectPackedReader2<R>
where
  R: RandomAccessInput,
{
  fn get(&self, index: usize) -> Result<i64> {
    self.read_from_slice_shared(index, None)
  }
}
impl<R> FromSliceShared<R> for DirectPackedReader2<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice_shared(&self, index: usize, slice: Option<&R>) -> Result<i64> {
    let shift = (index & 3) << 1;
    let slice = match slice {
      Some(slice) => slice,
      None => self
        .input
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("input is empty"))?,
    };
    let byte = slice.read_byte(self.offset + (index >> 2))?;
    let result = (byte >> shift) & 0x3;
    Ok(result as i64)
  }
}
impl<R> FromSlice<R> for DirectPackedReader2<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice(&mut self, index: usize, slice: Option<&R>) -> Result<i64> {
    self.read_from_slice_shared(index, slice)
  }
}

pub struct DirectPackedReader4<R> {
  input: Option<R>,
  offset: usize,
}

impl<R> DirectPackedReader4<R> {
  pub fn new(input: Option<R>, offset: usize) -> Self {
    DirectPackedReader4 { input, offset }
  }
}

impl<R> LongValues for DirectPackedReader4<R>
where
  R: RandomAccessInput,
{
  fn get(&self, index: usize) -> Result<i64> {
    self.read_from_slice_shared(index, None)
  }
}
impl<R> FromSliceShared<R> for DirectPackedReader4<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice_shared(&self, index: usize, slice: Option<&R>) -> Result<i64> {
    let shift = (index & 1) << 2;
    let slice = match slice {
      Some(slice) => slice,
      None => self
        .input
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("input is empty"))?,
    };
    let byte = slice.read_byte(self.offset + (index >> 1))?;
    let result = (byte >> shift) & 0xF;
    Ok(result as i64)
  }
}
impl<R> FromSlice<R> for DirectPackedReader4<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice(&mut self, index: usize, slice: Option<&R>) -> Result<i64> {
    self.read_from_slice_shared(index, slice)
  }
}

pub struct DirectPackedReader8<R> {
  input: Option<R>,
  offset: usize,
}

impl<R> DirectPackedReader8<R> {
  pub fn new(input: Option<R>, offset: usize) -> Self {
    DirectPackedReader8 { input, offset }
  }
}

impl<R> LongValues for DirectPackedReader8<R>
where
  R: RandomAccessInput,
{
  fn get(&self, index: usize) -> Result<i64> {
    self.read_from_slice_shared(index, None)
  }
}
impl<R> FromSliceShared<R> for DirectPackedReader8<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice_shared(&self, index: usize, slice: Option<&R>) -> Result<i64> {
    let slice = match slice {
      Some(slice) => slice,
      None => self
        .input
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("input is empty"))?,
    };
    let byte = slice.read_byte(self.offset + index)?;
    let result = byte;
    Ok(result as i64)
  }
}
impl<R> FromSlice<R> for DirectPackedReader8<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice(&mut self, index: usize, slice: Option<&R>) -> Result<i64> {
    self.read_from_slice_shared(index, slice)
  }
}

pub struct DirectPackedReader12<R> {
  input: Option<R>,
  offset: usize,
}

impl<R> DirectPackedReader12<R> {
  pub fn new(input: Option<R>, offset: usize) -> Self {
    DirectPackedReader12 { input, offset }
  }
}

impl<R> LongValues for DirectPackedReader12<R>
where
  R: RandomAccessInput,
{
  fn get(&self, index: usize) -> Result<i64> {
    self.read_from_slice_shared(index, None)
  }
}
impl<R> FromSliceShared<R> for DirectPackedReader12<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice_shared(&self, index: usize, slice: Option<&R>) -> Result<i64> {
    let off = (index * 12) >> 3;
    let shift = (index & 1) << 2;
    let slice = match slice {
      Some(slice) => slice,
      None => self
        .input
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("input is empty"))?,
    };
    let short_val = slice.read_short(self.offset + off)?;
    let result = ((short_val as u16) >> shift) & 0xFFF;
    Ok(result as i64)
  }
}
impl<R> FromSlice<R> for DirectPackedReader12<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice(&mut self, index: usize, slice: Option<&R>) -> Result<i64> {
    self.read_from_slice_shared(index, slice)
  }
}

pub struct DirectPackedReader16<R> {
  input: Option<R>,
  offset: usize,
}

impl<R> DirectPackedReader16<R> {
  pub fn new(input: Option<R>, offset: usize) -> Self {
    DirectPackedReader16 { input, offset }
  }
}

impl<R> LongValues for DirectPackedReader16<R>
where
  R: RandomAccessInput,
{
  fn get(&self, index: usize) -> Result<i64> {
    self.read_from_slice_shared(index, None)
  }
}
impl<R> FromSliceShared<R> for DirectPackedReader16<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice_shared(&self, index: usize, slice: Option<&R>) -> Result<i64> {
    let slice = match slice {
      Some(slice) => slice,
      None => self
        .input
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("input is empty"))?,
    };
    let result = slice.read_short(self.offset + (index << 1))? as u16;
    Ok(result as i64)
  }
}
impl<R> FromSlice<R> for DirectPackedReader16<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice(&mut self, index: usize, slice: Option<&R>) -> Result<i64> {
    self.read_from_slice_shared(index, slice)
  }
}
pub struct DirectPackedReader20<R> {
  input: Option<R>,
  offset: usize,
}

impl<R> DirectPackedReader20<R> {
  pub fn new(input: Option<R>, offset: usize) -> Self {
    DirectPackedReader20 { input, offset }
  }
}

impl<R> LongValues for DirectPackedReader20<R>
where
  R: RandomAccessInput,
{
  fn get(&self, index: usize) -> Result<i64> {
    self.read_from_slice_shared(index, None)
  }
}
impl<R> FromSliceShared<R> for DirectPackedReader20<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice_shared(&self, index: usize, slice: Option<&R>) -> Result<i64> {
    let off = (index * 20) >> 3;
    let shift = (index & 1) << 2;
    let slice = match slice {
      Some(slice) => slice,
      None => self
        .input
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("input is empty"))?,
    };
    let int_val = slice.read_int(self.offset + off)?;
    let result = (int_val >> shift) & 0xFFFFF;
    Ok(result as i64)
  }
}
impl<R> FromSlice<R> for DirectPackedReader20<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice(&mut self, index: usize, slice: Option<&R>) -> Result<i64> {
    self.read_from_slice_shared(index, slice)
  }
}

pub struct DirectPackedReader24<R> {
  input: Option<R>,
  offset: usize,
}

impl<R> DirectPackedReader24<R> {
  pub fn new(input: Option<R>, offset: usize) -> Self {
    DirectPackedReader24 { input, offset }
  }
}

impl<R> LongValues for DirectPackedReader24<R>
where
  R: RandomAccessInput,
{
  fn get(&self, index: usize) -> Result<i64> {
    self.read_from_slice_shared(index, None)
  }
}
impl<R> FromSliceShared<R> for DirectPackedReader24<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice_shared(&self, index: usize, slice: Option<&R>) -> Result<i64> {
    let slice = match slice {
      Some(slice) => slice,
      None => self
        .input
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("input is empty"))?,
    };
    let int_val = slice.read_int(self.offset + index * 3)?;
    let result = int_val & 0xFFFFFF;
    Ok(result as i64)
  }
}
impl<R> FromSlice<R> for DirectPackedReader24<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice(&mut self, index: usize, slice: Option<&R>) -> Result<i64> {
    self.read_from_slice_shared(index, slice)
  }
}

pub struct DirectPackedReader28<R> {
  input: Option<R>,
  offset: usize,
}

impl<R> DirectPackedReader28<R> {
  pub fn new(input: Option<R>, offset: usize) -> Self {
    DirectPackedReader28 { input, offset }
  }
}

impl<R> LongValues for DirectPackedReader28<R>
where
  R: RandomAccessInput,
{
  fn get(&self, index: usize) -> Result<i64> {
    self.read_from_slice_shared(index, None)
  }
}
impl<R> FromSliceShared<R> for DirectPackedReader28<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice_shared(&self, index: usize, slice: Option<&R>) -> Result<i64> {
    let off = (index * 28) >> 3;
    let shift = (index & 1) << 2;
    let slice = match slice {
      Some(slice) => slice,
      None => self
        .input
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("input is empty"))?,
    };
    let int_val = slice.read_int(self.offset + off)?;
    let result = (int_val >> shift) & 0xFFFFFFF;
    Ok(result as i64)
  }
}
impl<R> FromSlice<R> for DirectPackedReader28<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice(&mut self, index: usize, slice: Option<&R>) -> Result<i64> {
    self.read_from_slice_shared(index, slice)
  }
}

pub struct DirectPackedReader32<R> {
  input: Option<R>,
  offset: usize,
}

impl<R> DirectPackedReader32<R> {
  pub fn new(input: Option<R>, offset: usize) -> Self {
    DirectPackedReader32 { input, offset }
  }
}

impl<R> LongValues for DirectPackedReader32<R>
where
  R: RandomAccessInput,
{
  fn get(&self, index: usize) -> Result<i64> {
    self.read_from_slice_shared(index, None)
  }
}
impl<R> FromSliceShared<R> for DirectPackedReader32<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice_shared(&self, index: usize, slice: Option<&R>) -> Result<i64> {
    let slice = match slice {
      Some(slice) => slice,
      None => self
        .input
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("input is empty"))?,
    };
    let int_val = slice.read_int(self.offset + (index << 2))?;
    let result = int_val as u32;
    Ok(result as i64)
  }
}
impl<R> FromSlice<R> for DirectPackedReader32<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice(&mut self, index: usize, slice: Option<&R>) -> Result<i64> {
    self.read_from_slice_shared(index, slice)
  }
}

pub struct DirectPackedReader40<R> {
  input: Option<R>,
  offset: usize,
}

impl<R> DirectPackedReader40<R> {
  pub fn new(input: Option<R>, offset: usize) -> Self {
    DirectPackedReader40 { input, offset }
  }
}

impl<R> LongValues for DirectPackedReader40<R>
where
  R: RandomAccessInput,
{
  fn get(&self, index: usize) -> Result<i64> {
    self.read_from_slice_shared(index, None)
  }
}
impl<R> FromSliceShared<R> for DirectPackedReader40<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice_shared(&self, index: usize, slice: Option<&R>) -> Result<i64> {
    let slice = match slice {
      Some(slice) => slice,
      None => self
        .input
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("input is empty"))?,
    };
    let long_val = slice.read_long(self.offset + index * 5)?;
    let result = long_val & 0xFFFFFFFFFF;
    Ok(result)
  }
}
impl<R> FromSlice<R> for DirectPackedReader40<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice(&mut self, index: usize, slice: Option<&R>) -> Result<i64> {
    self.read_from_slice_shared(index, slice)
  }
}

pub struct DirectPackedReader48<R> {
  input: Option<R>,
  offset: usize,
}

impl<R> DirectPackedReader48<R> {
  pub fn new(input: Option<R>, offset: usize) -> Self {
    DirectPackedReader48 { input, offset }
  }
}

impl<R> LongValues for DirectPackedReader48<R>
where
  R: RandomAccessInput,
{
  fn get(&self, index: usize) -> Result<i64> {
    self.read_from_slice_shared(index, None)
  }
}
impl<R> FromSliceShared<R> for DirectPackedReader48<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice_shared(&self, index: usize, slice: Option<&R>) -> Result<i64> {
    let slice = match slice {
      Some(slice) => slice,
      None => self
        .input
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("input is empty"))?,
    };
    let long_val = slice.read_long(self.offset + index * 6)?;
    let result = long_val & 0xFFFFFFFFFFFF;
    Ok(result)
  }
}
impl<R> FromSlice<R> for DirectPackedReader48<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice(&mut self, index: usize, slice: Option<&R>) -> Result<i64> {
    self.read_from_slice_shared(index, slice)
  }
}

pub struct DirectPackedReader56<R> {
  input: Option<R>,
  offset: usize,
}

impl<R> DirectPackedReader56<R> {
  pub fn new(input: Option<R>, offset: usize) -> Self {
    DirectPackedReader56 { input, offset }
  }
}

impl<R> LongValues for DirectPackedReader56<R>
where
  R: RandomAccessInput,
{
  fn get(&self, index: usize) -> Result<i64> {
    self.read_from_slice_shared(index, None)
  }
}
impl<R> FromSliceShared<R> for DirectPackedReader56<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice_shared(&self, index: usize, slice: Option<&R>) -> Result<i64> {
    let slice = match slice {
      Some(slice) => slice,
      None => self
        .input
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("input is empty"))?,
    };
    let long_val = slice.read_long(self.offset + index * 7)?;
    let result = long_val & 0xFFFFFFFFFFFFFF;
    Ok(result)
  }
}
impl<R> FromSlice<R> for DirectPackedReader56<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice(&mut self, index: usize, slice: Option<&R>) -> Result<i64> {
    self.read_from_slice_shared(index, slice)
  }
}

pub struct DirectPackedReader64<R> {
  input: Option<R>,
  offset: usize,
}

impl<R> DirectPackedReader64<R> {
  pub fn new(input: Option<R>, offset: usize) -> Self {
    DirectPackedReader64 { input, offset }
  }
}

impl<R> LongValues for DirectPackedReader64<R>
where
  R: RandomAccessInput,
{
  fn get(&self, index: usize) -> Result<i64> {
    self.read_from_slice_shared(index, None)
  }
}
impl<R> FromSliceShared<R> for DirectPackedReader64<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice_shared(&self, index: usize, slice: Option<&R>) -> Result<i64> {
    let slice = match slice {
      Some(slice) => slice,
      None => self
        .input
        .as_ref()
        .ok_or_else(|| LuceneError::illegal_state("input is empty"))?,
    };
    let result = slice.read_long(self.offset + (index << 3))?;
    Ok(result)
  }
}
impl<R> FromSlice<R> for DirectPackedReader64<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice(&mut self, index: usize, slice: Option<&R>) -> Result<i64> {
    self.read_from_slice_shared(index, slice)
  }
}

pub enum DirectPackedEnum<R> {
  Direct1(DirectPackedReader1<R>),
  Direct2(DirectPackedReader2<R>),
  Direct4(DirectPackedReader4<R>),
  Direct8(DirectPackedReader8<R>),
  Direct12(DirectPackedReader12<R>),
  Direct16(DirectPackedReader16<R>),
  Direct20(DirectPackedReader20<R>),
  Direct24(DirectPackedReader24<R>),
  Direct28(DirectPackedReader28<R>),
  Direct32(DirectPackedReader32<R>),
  Direct40(DirectPackedReader40<R>),
  Direct48(DirectPackedReader48<R>),
  Direct56(DirectPackedReader56<R>),
  Direct64(DirectPackedReader64<R>),
  Merge(LongValuesImpl<R>),
  Zeroes(Zeroes),
}

impl<R> LongValues for DirectPackedEnum<R>
where
  R: RandomAccessInput,
{
  fn get_mut(&mut self, index: usize) -> Result<i64> {
    match self {
      Self::Direct1(reader) => reader.get(index),
      Self::Direct2(reader) => reader.get(index),
      Self::Direct4(reader) => reader.get(index),
      Self::Direct8(reader) => reader.get(index),
      Self::Direct12(reader) => reader.get(index),
      Self::Direct16(reader) => reader.get(index),
      Self::Direct20(reader) => reader.get(index),
      Self::Direct24(reader) => reader.get(index),
      Self::Direct28(reader) => reader.get(index),
      Self::Direct32(reader) => reader.get(index),
      Self::Direct40(reader) => reader.get(index),
      Self::Direct48(reader) => reader.get(index),
      Self::Direct56(reader) => reader.get(index),
      Self::Direct64(reader) => reader.get(index),
      Self::Merge(reader) => reader.get_mut(index),
      Self::Zeroes(reader) => reader.get(index),
    }
  }

  fn get(&self, index: usize) -> Result<i64> {
    match self {
      Self::Direct1(reader) => reader.get(index),
      Self::Direct2(reader) => reader.get(index),
      Self::Direct4(reader) => reader.get(index),
      Self::Direct8(reader) => reader.get(index),
      Self::Direct12(reader) => reader.get(index),
      Self::Direct16(reader) => reader.get(index),
      Self::Direct20(reader) => reader.get(index),
      Self::Direct24(reader) => reader.get(index),
      Self::Direct28(reader) => reader.get(index),
      Self::Direct32(reader) => reader.get(index),
      Self::Direct40(reader) => reader.get(index),
      Self::Direct48(reader) => reader.get(index),
      Self::Direct56(reader) => reader.get(index),
      Self::Direct64(reader) => reader.get(index),
      Self::Merge(reader) => reader.get(index),
      Self::Zeroes(reader) => reader.get(index),
    }
  }
}

/// Absolute reads for direct packed readers that do not update decoder state.
/// Merge readers require the mutable cache path.
pub trait FromSliceShared<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice_shared(&self, index: usize, slice: Option<&R>) -> Result<i64>;
}

pub trait FromSlice<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice(&mut self, index: usize, slice: Option<&R>) -> Result<i64>;
}
impl<R> FromSlice<R> for DirectPackedEnum<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice(&mut self, index: usize, slice: Option<&R>) -> Result<i64> {
    match self {
      DirectPackedEnum::Direct1(reader) => reader.read_from_slice(index, slice),
      DirectPackedEnum::Direct2(reader) => reader.read_from_slice(index, slice),
      DirectPackedEnum::Direct4(reader) => reader.read_from_slice(index, slice),
      DirectPackedEnum::Direct8(reader) => reader.read_from_slice(index, slice),
      DirectPackedEnum::Direct12(reader) => reader.read_from_slice(index, slice),
      DirectPackedEnum::Direct16(reader) => reader.read_from_slice(index, slice),
      DirectPackedEnum::Direct20(reader) => reader.read_from_slice(index, slice),
      DirectPackedEnum::Direct24(reader) => reader.read_from_slice(index, slice),
      DirectPackedEnum::Direct28(reader) => reader.read_from_slice(index, slice),
      DirectPackedEnum::Direct32(reader) => reader.read_from_slice(index, slice),
      DirectPackedEnum::Direct40(reader) => reader.read_from_slice(index, slice),
      DirectPackedEnum::Direct48(reader) => reader.read_from_slice(index, slice),
      DirectPackedEnum::Direct56(reader) => reader.read_from_slice(index, slice),
      DirectPackedEnum::Direct64(reader) => reader.read_from_slice(index, slice),
      DirectPackedEnum::Merge(reader) => reader.read_from_slice(index, slice),
      DirectPackedEnum::Zeroes(reader) => reader.read_from_slice(index, slice),
    }
  }
}

impl<R> FromSliceShared<R> for DirectPackedEnum<R>
where
  R: RandomAccessInput,
{
  fn read_from_slice_shared(&self, index: usize, slice: Option<&R>) -> Result<i64> {
    match self {
      DirectPackedEnum::Direct1(reader) => reader.read_from_slice_shared(index, slice),
      DirectPackedEnum::Direct2(reader) => reader.read_from_slice_shared(index, slice),
      DirectPackedEnum::Direct4(reader) => reader.read_from_slice_shared(index, slice),
      DirectPackedEnum::Direct8(reader) => reader.read_from_slice_shared(index, slice),
      DirectPackedEnum::Direct12(reader) => reader.read_from_slice_shared(index, slice),
      DirectPackedEnum::Direct16(reader) => reader.read_from_slice_shared(index, slice),
      DirectPackedEnum::Direct20(reader) => reader.read_from_slice_shared(index, slice),
      DirectPackedEnum::Direct24(reader) => reader.read_from_slice_shared(index, slice),
      DirectPackedEnum::Direct28(reader) => reader.read_from_slice_shared(index, slice),
      DirectPackedEnum::Direct32(reader) => reader.read_from_slice_shared(index, slice),
      DirectPackedEnum::Direct40(reader) => reader.read_from_slice_shared(index, slice),
      DirectPackedEnum::Direct48(reader) => reader.read_from_slice_shared(index, slice),
      DirectPackedEnum::Direct56(reader) => reader.read_from_slice_shared(index, slice),
      DirectPackedEnum::Direct64(reader) => reader.read_from_slice_shared(index, slice),
      DirectPackedEnum::Merge(reader) => reader.get(index),
      DirectPackedEnum::Zeroes(reader) => reader.get(index),
    }
  }
}
