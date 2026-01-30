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

use crate::core::index::BytesRef;
use crate::core::store::data_input::DataInput;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::group_vint_util::GroupVIntUtil;

/// Abstract base trait for performing write operations on Lucene's low-level
/// data types.
///
/// # Note
/// `DataOutput` is not thread-safe as it maintains internal state (e.g., file
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

    /// Writes an `int` as four bytes (little-endian byte order).
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

    /// Writes a `short` as two bytes (little-endian byte order).
    ///
    /// # See Also
    /// [`DataInput::read_short`]
    /// [`BitUtil::set_i16_le`](BitUtil::set_i16_le)
    fn write_short(&mut self, i: i16) -> Result<()> {
        self.write_byte(i as u8)?;
        self.write_byte((i >> 8) as u8)?;
        Ok(())
    }

    /// Writes an `int` in a variable-length format. Writes between one and five
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
    /// Returns an `IOError` if there is an error writing to the underlying
    /// medium.
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
    /// [`write_vint`](#method.write_vint) variable-length integer.
    /// This is typically useful for writing small signed integers and is
    /// equivalent to calling `write_vint(BitUtil::zig_zag_encode(i))`.
    ///
    /// # See Also
    /// [`DataInput::read_zint`]
    fn write_zint(&mut self, i: i32) -> Result<()> {
        self.write_vint(BitUtil::zig_zag_encode_i32(i))
    }

    /// Writes a `long` as eight bytes (little-endian byte order).
    ///
    /// # See Also
    /// [`DataInput::read_long`]
    /// [`BitUtil::set_i64_le`](BitUtil::set_i64_le)
    fn write_long(&mut self, i: i64) -> Result<()> {
        self.write_int(i as i32)?;
        self.write_int((i >> 32) as i32)?;
        Ok(())
    }

    /// Writes a `long` in a variable-length format. Writes between one and nine
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
            return Err(LuceneError::illegal_argument(
                "cannot write negative vLong (got: ".to_string() + &i.to_string() + ")",
            ));
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
    /// [`write_vlong`](#method.write_vlong) variable-length `long`.
    /// Writes between one and ten bytes. This is typically useful for writing
    /// small signed integers.
    ///
    /// # See Also
    /// [`DataInput::read_zlong`]
    fn write_zlong(&mut self, i: i64) -> Result<()> {
        self.write_signed_vlong(BitUtil::zig_zag_encode_i64(i))
    }

    /// Writes a [`zig-zag`](BitUtil::zig_zag_encode_i64)-encoded
    /// [`write_vlong`](#method.write_vlong) variable-length `long`.
    /// Writes between one and ten bytes. This is typically useful for writing
    /// small signed integers.
    ///
    /// # See Also
    /// [`DataInput::read_zlong`]
    fn write_string(&mut self, s: &str) -> Result<()> {
        let utf8_result: BytesRef<Vec<u8>> = BytesRef::from_string(s);
        let len = utf8_result.length;
        let offset = utf8_result.offset;
        self.write_vint(len as i32)?;
        self.write_bytes_range(&utf8_result.bytes, offset, len)
    }

    /// Copy numBytes bytes from input to ourselves.
    fn copy_bytes(&mut self, input: &mut impl DataInput, num_bytes: usize) -> Result<()> {
        let mut buffer = vec![0u8; COPY_BUFFER_SIZE];
        let mut left = num_bytes;
        while left > 0 {
            let to_copy = if left > COPY_BUFFER_SIZE {
                COPY_BUFFER_SIZE
            } else {
                left
            };
            input.read_bytes(&mut buffer, 0, to_copy)?;
            self.write_bytes_with_len(&buffer, to_copy)?;
            left -= to_copy;
        }
        Ok(())
    }
    /// Writes a `HashMap<String, String>`.
    ///
    /// First, the size is written as a [`write_vint`](#method.write_vint),
    /// followed by each key-value pair written as two consecutive
    /// [`write_string`](#method.write_string) calls.
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
    /// First, the size is written as a [`write_vint`](#method.write_vint),
    /// followed by each value written as a
    /// [`write_string`](#method.write_string).
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
const COPY_BUFFER_SIZE: usize = 16384;

/// Encodes integers using group-varint encoding. Tail values that do not fit
/// into a group are encoded using [`DataOutput::write_vint`]. Note: A `long[]`
/// is used because it aligns with posting requirements, but all longs are
/// actually expected to be integers.
///
/// # Arguments
/// * `values` - The values to write.
/// * `limit` - The number of values to write.
///
/// # Note
/// This is an experimental API.
pub fn write_group_vints_i64<D>(data_output: &mut D, values: &mut [i64], limit: i32) -> Result<()>
where
    D: DataOutput,
{
    let mut group_vint_bytes: Vec<u8> = vec![0; GroupVIntUtil::MAX_LENGTH_PER_GROUP];
    GroupVIntUtil::write_group_vints_i64(data_output, &mut group_vint_bytes, values, limit)?;
    Ok(())
}

/// Encodes integers using group-varint encoding. Tail values that do not fit
/// into a group are encoded using [`DataOutput::write_vint`]. Note: A `long[]`
/// is used because it aligns with posting requirements, but all longs are
/// actually expected to be integers.
///
/// # Arguments
/// * `values` - The values to write.
/// * `limit` - The number of values to write.
///
/// # Note
/// This is an experimental API.
pub fn write_group_vints_i32<D>(data_output: &mut D, values: &mut [i32], limit: i32) -> Result<()>
where
    D: DataOutput,
{
    let mut group_vint_bytes: Vec<u8> = vec![0; GroupVIntUtil::MAX_LENGTH_PER_GROUP];
    GroupVIntUtil::write_group_vints_i32(data_output, &mut group_vint_bytes, values, limit)?;
    Ok(())
}

pub enum DataOutputEnum2<A, B> {
    A(A),
    B(B),
}
impl<A, B> DataOutput for DataOutputEnum2<A, B>
where
    A: DataOutput,
    B: DataOutput,
{
    fn write_byte(&mut self, b: u8) -> Result<()> {
        match self {
            DataOutputEnum2::A(f) => f.write_byte(b),
            DataOutputEnum2::B(s) => s.write_byte(b),
        }
    }

    fn write_bytes_with_len(&mut self, b: &[u8], len: usize) -> Result<()> {
        match self {
            DataOutputEnum2::A(f) => f.write_bytes_with_len(b, len),
            DataOutputEnum2::B(s) => s.write_bytes_with_len(b, len),
        }
    }

    fn write_bytes_range(&mut self, b: &[u8], offset: usize, length: usize) -> Result<()> {
        match self {
            DataOutputEnum2::A(f) => f.write_bytes_range(b, offset, length),
            DataOutputEnum2::B(s) => s.write_bytes_range(b, offset, length),
        }
    }

    fn write_int(&mut self, i: i32) -> Result<()> {
        match self {
            DataOutputEnum2::A(f) => f.write_int(i),
            DataOutputEnum2::B(s) => s.write_int(i),
        }
    }

    fn write_short(&mut self, i: i16) -> Result<()> {
        match self {
            DataOutputEnum2::A(f) => f.write_short(i),
            DataOutputEnum2::B(s) => s.write_short(i),
        }
    }

    fn write_vint(&mut self, i: i32) -> Result<()> {
        match self {
            DataOutputEnum2::A(f) => f.write_vint(i),
            DataOutputEnum2::B(s) => s.write_vint(i),
        }
    }

    fn write_zint(&mut self, i: i32) -> Result<()> {
        match self {
            DataOutputEnum2::A(f) => f.write_zint(i),
            DataOutputEnum2::B(s) => s.write_zint(i),
        }
    }

    fn write_long(&mut self, i: i64) -> Result<()> {
        match self {
            DataOutputEnum2::A(f) => f.write_long(i),
            DataOutputEnum2::B(s) => s.write_long(i),
        }
    }

    fn write_vlong(&mut self, i: i64) -> Result<()> {
        match self {
            DataOutputEnum2::A(f) => f.write_vlong(i),
            DataOutputEnum2::B(s) => s.write_vlong(i),
        }
    }

    fn write_signed_vlong(&mut self, i: i64) -> Result<()> {
        match self {
            DataOutputEnum2::A(f) => f.write_signed_vlong(i),
            DataOutputEnum2::B(s) => s.write_signed_vlong(i),
        }
    }

    fn write_zlong(&mut self, i: i64) -> Result<()> {
        match self {
            DataOutputEnum2::A(f) => f.write_zlong(i),
            DataOutputEnum2::B(s) => s.write_zlong(i),
        }
    }

    fn write_string(&mut self, s: &str) -> Result<()> {
        match self {
            DataOutputEnum2::A(f) => f.write_string(s),
            DataOutputEnum2::B(s1) => s1.write_string(s),
        }
    }

    fn copy_bytes(&mut self, input: &mut impl DataInput, num_bytes: usize) -> Result<()> {
        match self {
            DataOutputEnum2::A(f) => f.copy_bytes(input, num_bytes),
            DataOutputEnum2::B(s) => s.copy_bytes(input, num_bytes),
        }
    }

    fn write_map_of_strings(&mut self, map: &HashMap<String, String>) -> Result<()> {
        match self {
            DataOutputEnum2::A(f) => f.write_map_of_strings(map),
            DataOutputEnum2::B(s) => s.write_map_of_strings(map),
        }
    }

    fn write_set_of_strings(&mut self, set: &HashSet<String>) -> Result<()> {
        match self {
            DataOutputEnum2::A(f) => f.write_set_of_strings(set),
            DataOutputEnum2::B(s) => s.write_set_of_strings(set),
        }
    }
}
