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
use crate::core::store::data_output::DataOutput;
use crate::core::util::access::ByteSourceMut;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::Result;

/// [`DataOutput`] backed by a byte array.
///
/// # Warning
/// This struct omits most low-level checks, so be sure to test thoroughly with
/// assertions enabled.
///
/// # Note
/// This is an experimental API.
#[derive(Default)]
pub struct ByteArrayDataOutput<B> {
  pub bytes: B,
  pos: usize,
  limit: usize,
}

impl<B> ByteArrayDataOutput<B>
where
  B: ByteSourceMut,
{
  pub fn with_bytes(bytes: B) -> Self {
    let len = bytes.as_slice().len();
    Self::with_range(bytes, 0, len)
  }
  pub fn with_range(bytes: B, offset: usize, length: usize) -> Self {
    Self {
      bytes,
      pos: offset,
      limit: offset + length,
    }
  }
  pub fn reset(&mut self) -> Result<()> {
    let len = self.bytes.as_slice().len();
    let offset = 0;
    self.reset_with_range(offset, len)
  }
  pub fn reset_with_range(&mut self, offset: usize, length: usize) -> Result<()> {
    self.pos = offset;
    self.limit = offset + length;
    Ok(())
  }

  pub fn get_position(&self) -> usize {
    self.pos
  }
}

impl<B> DataOutput for ByteArrayDataOutput<B>
where
  B: ByteSourceMut,
{
  fn write_byte(&mut self, b: u8) -> Result<()> {
    debug_assert!(self.pos < self.limit);
    self.bytes.as_slice_mut()[self.pos] = b;
    self.pos += 1;
    Ok(())
  }

  fn write_bytes_range(&mut self, b: &[u8], offset: usize, length: usize) -> Result<()> {
    debug_assert!(
      self.pos + length <= self.limit,
      "Write exceeds the allowed limit: pos={}, length={}, limit={}",
      self.pos,
      length,
      self.limit
    );
    let (copy_target, copy_source, copy_offset) = (
      &mut (self.bytes.as_slice_mut())[..],
      &b[offset..(offset + length)],
      self.pos,
    );
    copy_target[copy_offset..][..copy_source.len()].copy_from_slice(copy_source);
    self.pos += length;
    Ok(())
  }

  fn write_int(&mut self, i: i32) -> Result<()> {
    debug_assert!(
      self.pos + BitUtil::INT_BYTES <= self.limit,
      "Write exceeds the allowed limit"
    );
    BitUtil::set_i32_le(self.bytes.as_slice_mut(), self.pos, i);
    self.pos += BitUtil::INT_BYTES;
    Ok(())
  }

  fn write_short(&mut self, i: i16) -> Result<()> {
    debug_assert!(
      self.pos + BitUtil::SHORT_BYTES <= self.limit,
      "Write exceeds the allowed limit"
    );
    BitUtil::set_i16_le(self.bytes.as_slice_mut(), self.pos, i);
    self.pos += BitUtil::SHORT_BYTES;
    Ok(())
  }

  fn write_long(&mut self, i: i64) -> Result<()> {
    debug_assert!(
      self.pos + BitUtil::LONG_BYTES <= self.limit,
      "Write exceeds the allowed limit"
    );
    BitUtil::set_i64_le(self.bytes.as_slice_mut(), self.pos, i);
    self.pos += BitUtil::LONG_BYTES;
    Ok(())
  }
}
