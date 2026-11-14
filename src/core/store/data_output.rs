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

use crate::core::store::data_input::DataInput;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::group_vint_util::GroupVIntUtil;

/// Abstract base trait for performing write operations on Lucene's low-level
/// data types.
///
/// # Note
/// [`DataOutput`] is not thread-safe as it maintains internal state (e.g., file
/// position), and therefore should only be used from a single thread.
pub trait DataOutput {
  /// Writes a single byte.
  ///
  /// The most primitive data type is an eight-bit byte. Files are accessed as
  /// sequences of bytes. All other data types are defined as sequences of
  /// bytes, making file formats byte-order independent.
  ///
  /// # See Also
  /// [`IndexInput::read_byte`](DataInput::read_byte)
  fn write_byte(&mut self, b: u8) -> Result<()>;

  /// Writes an array of bytes.
  ///
  /// # Arguments
  /// * `b` - The bytes to write.
  /// * `length` - The number of bytes to write.
  ///
  /// # See Also
  /// [`DataInput::read_bytes`]
  fn write_bytes_with_len(&mut self, b: &[u8], len: usize) -> Result<()> {
    self.write_bytes_range(b, 0, len)
  }
  /// Writes an array of bytes.
  ///
  /// # Arguments
  /// * `b` - The bytes to write.
  /// * `offset` - The offset in the byte array.
  /// * `length` - The number of bytes to write.
  ///
  /// # See Also
  /// [`DataInput::read_bytes`].
  fn write_bytes_range(&mut self, b: &[u8], offset: usize, length: usize) -> Result<()>;

  /// Writes an `i32` as four bytes (little-endian byte order).
  ///
  /// # See Also
  /// [`DataInput::read_int`]
  /// [`BitUtil::set_i16_le`](BitUtil::set_i16_le)
  fn write_int(&mut self, i: i32) -> Result<()> {
    self.write_byte(i as u8)?;
    self.write_byte((i >> 8) as u8)?;
    self.write_byte((i >> 16) as u8)?;
    self.write_byte((i >> 24) as u8)?;
    Ok(())
  }

  /// Writes integers in little-endian order, preserving [`Self::write_int`] errors.
  ///
  /// This Rust bulk entry defaults to the original primitive calls. Implementations
  /// may batch only when their observable callbacks and failure state are preserved.
  fn write_ints(&mut self, values: &[i32]) -> Result<()> {
    for &value in values {
      self.write_int(value)?;
    }
    Ok(())
  }

  /// Writes an `i16` as two bytes (little-endian byte order).
  ///
  /// # See Also
  /// [`DataInput::read_short`]
  /// [`BitUtil::set_i16_le`](BitUtil::set_i16_le)
  fn write_short(&mut self, i: i16) -> Result<()> {
    self.write_byte(i as u8)?;
    self.write_byte((i >> 8) as u8)?;
    Ok(())
  }

  /// Writes an `i32` in a variable-length format. Writes between one and five
  /// bytes, with smaller values taking fewer bytes. Negative numbers are
  /// supported but should be avoided.
  ///
  /// # Format
  /// VByte is a variable-length format for positive integers, where the
  /// high-order bit of each byte indicates whether more bytes remain to
  /// be read. The low-order seven bits are appended as increasingly more
  /// significant bits in the resulting integer value.
  /// - Values from 0 to 127 are stored in a single byte.
  /// - Values from 128 to 16,383 are stored in two bytes, and so on.
  ///
  /// # VByte Encoding Example
  ///
  /// | Value     | Byte 1      | Byte 2      | Byte 3      |
  /// |-----------|-------------|-------------|-------------|
  /// | 0         | `00000000`  |             |             |
  /// | 1         | `00000001`  |             |             |
  /// | 127       | `01111111`  |             |             |
  /// | 128       | `10000000`  | `00000001`  |             |
  /// | 16,383    | `11111111`  | `01111111`  |             |
  /// | 16,384    | `10000000`  | `10000000`  | `00000001`  |
  ///
  /// This format provides compression while remaining efficient to decode.
  ///
  /// # Arguments
  /// * `i` - The integer to write. Smaller values take fewer bytes. Negative
  ///   numbers are supported but should be avoided.
  ///
  /// # Errors
  /// Returns [`LuceneErrorKind::Io`](crate::core::util::error::lucene_error::LuceneErrorKind::Io) if
  /// there is an error writing to the underlying medium.
  ///
  /// # See Also
  /// [`DataInput::read_vint`]
  fn write_vint(&mut self, i: i32) -> Result<()> {
    let mut i = i as u32;
    while (i & !0x7F) != 0 {
      self.write_byte(((i & 0x7F) | 0x80) as u8)?;
      i >>= 7;
    }
    self.write_byte(i as u8)?;
    Ok(())
  }

  /// Writes a [`zig-zag`](BitUtil::zig_zag_encode_i32)-encoded
  /// [`write_vint`](Self::write_vint) variable-length integer.
  /// This is typically useful for writing small signed integers and is
  /// equivalent to calling `write_vint(BitUtil::zig_zag_encode(i))`.
  ///
  /// # See Also
  /// [`DataInput::read_zint`]
  fn write_zint(&mut self, i: i32) -> Result<()> {
    self.write_vint(BitUtil::zig_zag_encode_i32(i))
  }

  /// Writes an `i64` as eight bytes (little-endian byte order).
  ///
  /// # See Also
  /// [`DataInput::read_long`]
  /// [`BitUtil::set_i64_le`](BitUtil::set_i64_le)
  fn write_long(&mut self, i: i64) -> Result<()> {
    self.write_int(i as i32)?;
    self.write_int((i >> 32) as i32)?;
    Ok(())
  }

  /// Writes an `i64` in a variable-length format. Writes between one and nine
  /// bytes, with smaller values taking fewer bytes. Negative numbers are
  /// not supported.
  ///
  /// # Format
  /// The format is described further in [`DataOutput::write_vint`]).
  ///
  /// # See Also
  /// [`DataInput::read_vlong`]
  fn write_vlong(&mut self, i: i64) -> Result<()> {
    if i < 0 {
      return Err(LuceneError::illegal_argument(format!(
        "cannot write negative vLong (got: {i})"
      )));
    }
    self.write_signed_vlong(i)?;
    Ok(())
  }

  fn write_signed_vlong(&mut self, i: i64) -> Result<()> {
    let mut i = i as u64;
    while (i & !0x7F) != 0 {
      self.write_byte(((i & 0x7F) | 0x80) as u8)?;
      i >>= 7;
    }
    self.write_byte(i as u8)?;
    Ok(())
  }
  /// Writes a [`zig-zag`](BitUtil::zig_zag_encode_i64)-encoded
  /// [`write_vlong`](Self::write_vlong) variable-length integer.
  /// Writes between one and ten bytes. This is typically useful for writing
  /// small signed integers.
  ///
  /// # See Also
  /// [`DataInput::read_zlong`]
  fn write_zlong(&mut self, i: i64) -> Result<()> {
    self.write_signed_vlong(BitUtil::zig_zag_encode_i64(i))
  }

  /// Writes a [`zig-zag`](BitUtil::zig_zag_encode_i64)-encoded
  /// [`write_vlong`](Self::write_vlong) variable-length integer.
  /// Writes between one and ten bytes. This is typically useful for writing
  /// small signed integers.
  ///
  /// # See Also
  /// [`DataInput::read_zlong`]
  fn write_string(&mut self, s: &str) -> Result<()> {
    let bytes = s.as_bytes();
    self.write_vint(bytes.len() as i32)?;
    self.write_bytes_range(bytes, 0, bytes.len())
  }

  /// Copy numBytes bytes from input to ourselves.
  fn copy_bytes<I>(&mut self, input: &mut I, num_bytes: usize) -> Result<()>
  where
    Self: Sized,
    I: DataInput + ?Sized,
  {
    copy_bytes_impl(self, input, num_bytes)
  }

  /// Writes a `HashMap<String, String>`.
  ///
  /// First, the size is written with [`write_vint`](Self::write_vint),
  /// followed by each key-value pair written as two consecutive
  /// [`write_string`](Self::write_string) calls.
  ///
  /// # Arguments
  /// * `map` - The input map.
  fn write_map_of_strings(&mut self, map: &HashMap<String, String>) -> Result<()> {
    self.write_vint(map.len() as i32)?;
    for (key, value) in map.iter() {
      self.write_string(key)?;
      self.write_string(value)?;
    }
    Ok(())
  }

  /// Writes a `HashSet<String>`.
  ///
  /// First, the size is written with [`write_vint`](Self::write_vint),
  /// followed by each value written as a
  /// [`write_string`](Self::write_string).
  ///
  /// # Arguments
  /// * `set` - The input set.
  fn write_set_of_strings(&mut self, set: &HashSet<String>) -> Result<()> {
    self.write_vint(set.len() as i32)?;
    for value in set.iter() {
      self.write_string(value)?;
    }
    Ok(())
  }
}
pub(crate) fn copy_bytes_impl<O, I>(out: &mut O, input: &mut I, num_bytes: usize) -> Result<()>
where
  O: DataOutput + ?Sized,
  I: DataInput + ?Sized,
{
  let mut left = num_bytes;
  while left > 0 {
    let to_copy = left.min(COPY_BUFFER_SIZE);
    let bytes = input.get_bytes(to_copy)?;
    out.write_bytes_with_len(bytes.as_ref(), to_copy)?;
    left -= to_copy;
  }
  Ok(())
}
pub(crate) const COPY_BUFFER_SIZE: usize = 16384;

/// Encodes integers using group-varint encoding. Tail values that do not fit
/// into a group are encoded using [`DataOutput::write_vint`]. An `i64` slice is
/// used because it aligns with posting requirements, but every value is
/// expected to fit in an `i32`.
///
/// # Arguments
/// * `values` - The values to write.
/// * `limit` - The number of values to write.
///
/// # Note
/// This is an experimental API.
pub fn write_group_vints_i64<D>(data_output: &mut D, values: &mut [i64], limit: usize) -> Result<()>
where
  D: DataOutput,
{
  let mut group_vint_bytes = [0; GroupVIntUtil::MAX_LENGTH_PER_GROUP];
  GroupVIntUtil::write_group_vints_i64(data_output, &mut group_vint_bytes, values, limit)?;
  Ok(())
}

/// Encodes integers using group-varint encoding. Tail values that do not fit
/// into a group are encoded using [`DataOutput::write_vint`]. Values are
/// supplied as an `i32` slice.
///
/// # Arguments
/// * `values` - The values to write.
/// * `limit` - The number of values to write.
///
/// # Note
/// This is an experimental API.
pub fn write_group_vints_i32<D>(data_output: &mut D, values: &mut [i32], limit: usize) -> Result<()>
where
  D: DataOutput,
{
  let mut group_vint_bytes = [0; GroupVIntUtil::MAX_LENGTH_PER_GROUP];
  GroupVIntUtil::write_group_vints_i32(data_output, &mut group_vint_bytes, values, limit)?;
  Ok(())
}
