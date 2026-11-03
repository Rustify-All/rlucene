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
use crate::core::store::data_input::DataInput;
use crate::core::store::data_output::DataOutput;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};

/// Provides an abstraction for reading `i32` values, so that decoding logic can
/// be reused in different [`DataInput`] implementations.
pub trait IntReader {
  fn read(&mut self, pos: usize) -> Result<i32>;
}

/// This struct contains utility methods and constants for group varint.
pub struct GroupVIntUtil;

impl GroupVIntUtil {
  // Decode into `i64` values so negative `i32` bit patterns can be represented
  // as positive values.
  const LONG_MASKS: [u64; 4] = [0xFF, 0xFFFF, 0xFFFFFF, 0xFFFFFFFF];
  const INT_MASKS: [i32; 4] = [0xFF, 0xFFFF, 0xFFFFFF, !0];
  // the maximum length of a single group-varint is 4 integers + 1 byte flag.
  pub const MAX_LENGTH_PER_GROUP: usize = 17;
  /// Reads all the group varints, including the tail vints. We need a
  /// `Vec<i64>` because this is what postings use, even though every decoded
  /// value fits in 32 bits.
  ///
  /// # Arguments
  /// * `dst` - The array to read `i64` values into.
  /// * `limit` - The number of `i64` values to read.
  ///
  /// # Note
  /// This is an experimental API.
  pub fn read_group_vints_i64<DI>(input: &mut DI, dst: &mut [i64], limit: usize) -> Result<()>
  where
    DI: DataInput,
  {
    let mut i = 0;
    while i + 4 <= limit {
      Self::read_group_vint_i64(input, dst, i)?;
      i += 4;
    }
    while i < limit {
      dst[i] = input.read_vint()? as u32 as i64;
      i += 1;
    }
    Ok(())
  }
  /// Reads all the group varints, including the tail vints. We need a
  /// `Vec<i64>` because this is what postings are using, and all longs
  /// are actually required to be integers.
  ///
  /// # Arguments
  /// * `dst` - The array to read `i32` values into.
  /// * `limit` - The number of `i32` values to read.
  ///
  /// # Note
  /// This is an experimental API.
  pub fn read_group_vints_i32<DI>(input: &mut DI, dst: &mut [i32], limit: usize) -> Result<()>
  where
    DI: DataInput,
  {
    let mut i = 0;
    while i + 4 <= limit {
      input.read_group_vint(dst, i)?;
      i += 4;
    }

    while i < limit {
      dst[i] = input.read_vint()?;
      i += 1;
    }
    Ok(())
  }

  /// Default implementation of reading a single group. For optimal
  /// performance, you should use
  /// [`GroupVIntUtil::read_group_vints_i64`]
  /// instead.
  ///
  /// # Arguments
  /// * `in` - The input to use to read data.
  /// * `dst` - The array to read `i64` values into.
  /// * `offset` - The offset in the array to start storing `i64` values.
  pub fn read_group_vint_i64<DI>(data_input: &mut DI, dst: &mut [i64], offset: usize) -> Result<()>
  where
    DI: DataInput,
  {
    {
      let flag = data_input.read_byte()? as usize;

      let n1_minus1 = flag >> 6;
      let n2_minus1 = (flag >> 4) & 0x03;
      let n3_minus1 = (flag >> 2) & 0x03;
      let n4_minus1 = flag & 0x03;

      dst[offset] = Self::read_int_in_group(data_input, n1_minus1)? as u32 as i64;
      dst[offset + 1] = Self::read_int_in_group(data_input, n2_minus1)? as u32 as i64;
      dst[offset + 2] = Self::read_int_in_group(data_input, n3_minus1)? as u32 as i64;
      dst[offset + 3] = Self::read_int_in_group(data_input, n4_minus1)? as u32 as i64;

      Ok(())
    }
  }
  /// Default implementation of reading a single group. For optimal
  /// performance, you should use
  /// [`GroupVIntUtil::read_group_vints_i64`]
  /// instead.
  ///
  /// # Arguments
  /// * `in` - The input to use to read data.
  /// * `dst` - The array to read `i64` values into.
  /// * `offset` - The offset in the array to start storing `i64` values.
  pub fn read_group_vint_i32<DI>(data_input: &mut DI, dst: &mut [i32], offset: usize) -> Result<()>
  where
    DI: DataInput,
  {
    {
      let flag = data_input.read_byte()? as usize;

      let n1_minus1 = flag >> 6;
      let n2_minus1 = (flag >> 4) & 0x03;
      let n3_minus1 = (flag >> 2) & 0x03;
      let n4_minus1 = flag & 0x03;

      dst[offset] = Self::read_int_in_group(data_input, n1_minus1)?;
      dst[offset + 1] = Self::read_int_in_group(data_input, n2_minus1)?;
      dst[offset + 2] = Self::read_int_in_group(data_input, n3_minus1)?;
      dst[offset + 3] = Self::read_int_in_group(data_input, n4_minus1)?;

      Ok(())
    }
  }
  fn read_int_in_group<DI>(data_input: &mut DI, num_bytes_minus1: usize) -> Result<i32>
  where
    DI: DataInput,
  {
    match num_bytes_minus1 {
      0 => Ok(data_input.read_byte()? as i32),
      1 => Ok(data_input.read_short()? as u16 as i32),
      2 => {
        let short_part = data_input.read_short()? as u16 as u32;
        let byte_part = (data_input.read_byte()? as u32) << 16;
        Ok((short_part | byte_part) as i32)
      },
      _ => data_input.read_int(),
    }
  }
  /// Faster implementation of reading a single group. It reads values from
  /// the buffer that would not cross boundaries.
  ///
  /// # Arguments
  /// * `in` - The input to use to read data.
  /// * `remaining` - The number of remaining bytes allowed to read for the
  ///   current block/segment.
  /// * `pos` - The start position to read from the reader.
  /// * `dst` - The array to read `i64` values into.
  /// * `offset` - The offset in the array to start storing `i64` values.
  ///
  /// # Returns
  /// The number of bytes read excluding the flag. This indicates the number
  /// of positions that should be increased for the caller. It is a
  /// non-negative number less than `MAX_LENGTH_PER_GROUP`.
  pub fn read_group_vint_i64_with_reader<DI>(
    data_input: &mut DI,
    remaining: u64,
    mut pos: usize,
    dst: &mut [i64],
    offset: usize,
  ) -> Result<usize>
  where
    DI: DataInput + IntReader,
  {
    if remaining < Self::MAX_LENGTH_PER_GROUP as u64 {
      Self::read_group_vint_i64(data_input, dst, offset)?;
      return Ok(0);
    }

    let flag = DataInput::read_byte(data_input)? as usize;
    pos += 1; // exclude the flag bytes, the position has updated via read_byte().
    let pos_start = pos;

    let n1_minus1 = flag >> 6;
    let n2_minus1 = (flag >> 4) & 0x03;
    let n3_minus1 = (flag >> 2) & 0x03;
    let n4_minus1 = flag & 0x03;
    // This code path has fewer conditionals and tends to be significantly
    // faster in benchmarks

    dst[offset] = (IntReader::read(data_input, pos)? as u64 & Self::LONG_MASKS[n1_minus1]) as i64;
    pos += 1 + n1_minus1;

    dst[offset + 1] =
      (IntReader::read(data_input, pos)? as u64 & Self::LONG_MASKS[n2_minus1]) as i64;
    pos += 1 + n2_minus1;

    dst[offset + 2] =
      (IntReader::read(data_input, pos)? as u64 & Self::LONG_MASKS[n3_minus1]) as i64;
    pos += 1 + n3_minus1;

    dst[offset + 3] =
      (IntReader::read(data_input, pos)? as u64 & Self::LONG_MASKS[n4_minus1]) as i64;
    pos += 1 + n4_minus1;
    let result = pos - pos_start;
    Ok(result)
  }
  /// Faster implementation of reading a single group. It reads values from
  /// the buffer that would not cross boundaries.
  ///
  /// # Arguments
  /// * `in` - The input to use to read data.
  /// * `remaining` - The number of remaining bytes allowed to read for the
  ///   current block/segment.
  /// * `pos` - The start position to read from the reader.
  /// * `dst` - The array to read `i32` values into.
  /// * `offset` - The offset in the array to start storing `i32` values.
  ///
  /// # Returns
  /// The number of bytes read excluding the flag. This indicates the number
  /// of positions that should be increased for the caller. It is a
  /// non-negative number less than `MAX_LENGTH_PER_GROUP`.
  #[inline]
  pub fn read_group_vint_i32_with_reader<DI>(
    data_input: &mut DI,
    remaining: usize,
    mut pos: usize,
    dst: &mut [i32],
    offset: usize,
  ) -> Result<usize>
  where
    DI: DataInput + IntReader,
  {
    if remaining < Self::MAX_LENGTH_PER_GROUP {
      Self::read_group_vint_i32(data_input, dst, offset)?;
      return Ok(0);
    }

    let flag = DataInput::read_byte(data_input)? as usize;
    pos += 1; // exclude the flag bytes, the position has updated via read_byte().
    let pos_start = pos;

    let n1_minus1 = flag >> 6;
    let n2_minus1 = (flag >> 4) & 0x03;
    let n3_minus1 = (flag >> 2) & 0x03;
    let n4_minus1 = flag & 0x03;
    // This code path has fewer conditionals and tends to be significantly
    // faster in benchmarks

    dst[offset] = IntReader::read(data_input, pos)? & Self::INT_MASKS[n1_minus1];
    pos += 1 + n1_minus1;

    dst[offset + 1] = IntReader::read(data_input, pos)? & Self::INT_MASKS[n2_minus1];
    pos += 1 + n2_minus1;

    dst[offset + 2] = IntReader::read(data_input, pos)? & Self::INT_MASKS[n3_minus1];
    pos += 1 + n3_minus1;

    dst[offset + 3] = IntReader::read(data_input, pos)? & Self::INT_MASKS[n4_minus1];
    pos += 1 + n4_minus1;
    let result = pos - pos_start;
    Ok(result)
  }
  fn num_bytes(v: i32) -> usize {
    // | 1 ensures it returns 1 when v = 0
    BitUtil::INT_BYTES - ((v | 1).leading_zeros() / 8) as usize
  }
  /// Converts an i64 value to an i32, ensuring it fits within the valid
  /// range. Returns an error if the value is not within 0 to 0xFFFFFFFF.
  fn to_int(value: i64) -> Result<i32> {
    if (value as u64) > 0xFFFF_FFFF_u64 {
      Err(LuceneError::number_overflow(format!(
        "value: {value} is out of range to be converted to i32"
      )))
    } else {
      Ok(value as i32)
    }
  }
  /// The implementation for group-varint encoding. It uses a maximum of
  /// [`MAX_LENGTH_PER_GROUP`](GroupVIntUtil::MAX_LENGTH_PER_GROUP) bytes
  /// scratch buffer.
  pub fn write_group_vints_i64<DO>(
    data_output: &mut DO,
    scratch: &mut [u8],
    values: &mut [i64],
    limit: usize,
  ) -> Result<()>
  where
    DO: DataOutput,
  {
    let mut read_pos: usize = 0;

    // encode each group
    while (limit - read_pos) >= 4 {
      let mut write_pos: usize = 0;
      let n1_minus1 = Self::num_bytes(Self::to_int(values[read_pos])?);
      let n2_minus1 = Self::num_bytes(Self::to_int(values[read_pos + 1])?);
      let n3_minus1 = Self::num_bytes(Self::to_int(values[read_pos + 2])?);
      let n4_minus1 = Self::num_bytes(Self::to_int(values[read_pos + 3])?);

      let flag =
        ((n1_minus1 - 1) << 6) | ((n2_minus1 - 1) << 4) | ((n3_minus1 - 1) << 2) | (n4_minus1 - 1);
      scratch[write_pos] = flag as u8;
      write_pos += 1;

      scratch[write_pos..write_pos + 4].copy_from_slice(&(values[read_pos] as i32).to_le_bytes());
      write_pos += n1_minus1;

      scratch[write_pos..write_pos + 4]
        .copy_from_slice(&(values[read_pos + 1] as i32).to_le_bytes());
      write_pos += n2_minus1;

      scratch[write_pos..write_pos + 4]
        .copy_from_slice(&(values[read_pos + 2] as i32).to_le_bytes());
      write_pos += n3_minus1;

      scratch[write_pos..write_pos + 4]
        .copy_from_slice(&(values[read_pos + 3] as i32).to_le_bytes());
      write_pos += n4_minus1;

      data_output.write_bytes_with_len(scratch, write_pos)?;
      read_pos += 4;
    }

    // tail vints
    while read_pos < limit {
      data_output.write_vint(Self::to_int(values[read_pos])?)?;
      read_pos += 1;
    }

    Ok(())
  }
  /// The implementation for group-varint encoding. It uses a maximum of
  /// [`MAX_LENGTH_PER_GROUP`](GroupVIntUtil::MAX_LENGTH_PER_GROUP) bytes
  /// scratch buffer.
  pub fn write_group_vints_i32<DO>(
    data_output: &mut DO,
    scratch: &mut [u8],
    values: &mut [i32],
    limit: usize,
  ) -> Result<()>
  where
    DO: DataOutput,
  {
    let mut read_pos: usize = 0;

    // encode each group
    while (limit - read_pos) >= 4 {
      let group = [
        values[read_pos],
        values[read_pos + 1],
        values[read_pos + 2],
        values[read_pos + 3],
      ];
      let combined = (group[0] | group[1] | group[2] | group[3]) as u32;
      if scratch.len() >= 8 && combined <= u32::from(u8::MAX) {
        // The scalar stores also write the last value's three high bytes into
        // scratch, even though the output only consumes the first five bytes.
        let bytes: wide::u8x16 = wide::bytemuck::cast(group.map(i32::to_le));
        let order = wide::u8x16::new([
          255, 0, 4, 8, 12, 13, 14, 15, 255, 255, 255, 255, 255, 255, 255, 255,
        ]);
        // This shuffle is available in the minimum supported wide version.
        #[allow(deprecated)]
        let packed = bytes.swizzle_relaxed(order).to_array();
        scratch[..8].copy_from_slice(&packed[..8]);
        data_output.write_bytes_with_len(scratch, 5)?;
        read_pos += 4;
        continue;
      }

      let mut write_pos: usize = 0;
      let n1_minus1 = Self::num_bytes(group[0]);
      let n2_minus1 = Self::num_bytes(group[1]);
      let n3_minus1 = Self::num_bytes(group[2]);
      let n4_minus1 = Self::num_bytes(group[3]);

      let flag =
        ((n1_minus1 - 1) << 6) | ((n2_minus1 - 1) << 4) | ((n3_minus1 - 1) << 2) | (n4_minus1 - 1);
      scratch[write_pos] = flag as u8;
      write_pos += 1;

      scratch[write_pos..write_pos + 4].copy_from_slice(&group[0].to_le_bytes());
      write_pos += n1_minus1;

      scratch[write_pos..write_pos + 4].copy_from_slice(&group[1].to_le_bytes());
      write_pos += n2_minus1;

      scratch[write_pos..write_pos + 4].copy_from_slice(&group[2].to_le_bytes());
      write_pos += n3_minus1;

      scratch[write_pos..write_pos + 4].copy_from_slice(&group[3].to_le_bytes());
      write_pos += n4_minus1;

      data_output.write_bytes_with_len(scratch, write_pos)?;
      read_pos += 4;
    }

    // tail vints
    while read_pos < limit {
      data_output.write_vint(values[read_pos])?;
      read_pos += 1;
    }

    Ok(())
  }
}
