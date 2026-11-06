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
use crate::core::store::{DataOutput, IndexInput};
use crate::core::util::error::lucene_error::{LuceneError, Result};

pub(crate) struct StoredFieldsInts;
impl StoredFieldsInts {
  const BLOCK_SIZE: usize = 128;
  pub(crate) fn write_ints<DO>(
    values: &[i32],
    start: usize,
    count: usize,
    out: &mut DO,
  ) -> Result<()>
  where
    DO: DataOutput,
  {
    let mut all_equal = true;
    if count > 1 {
      let second = values[start + 1];
      let first = values[start];
      if second != first {
        all_equal = false;
      } else if count >= 10
        && let Some(active) = values.get(start..).and_then(|s| s.get(..count))
      {
        let (chunks, tail) = active[2..].as_chunks::<8>();
        for chunk in chunks {
          if !((chunk[0] == first)
            & (chunk[1] == first)
            & (chunk[2] == first)
            & (chunk[3] == first)
            & (chunk[4] == first)
            & (chunk[5] == first)
            & (chunk[6] == first)
            & (chunk[7] == first))
          {
            all_equal = false;
            break;
          }
        }
        if all_equal {
          for &value in tail {
            if value != first {
              all_equal = false;
              break;
            }
          }
        }
      } else {
        for i in 2..count {
          if values[start + i] != first {
            all_equal = false;
            break;
          }
        }
      }
    }

    if all_equal {
      out.write_byte(0)?;
      out.write_vint(values[0])?;
    } else {
      let mut max: u64 = 0;
      for i in 0..count {
        max |= values[start + i] as u32 as u64;
      }
      if max <= 0xff {
        out.write_byte(8)?;
        Self::write_ints8(out, count, values, start)?;
      } else if max <= 0xffff {
        out.write_byte(16)?;
        Self::write_ints16(out, count, values, start)?;
      } else {
        out.write_byte(32)?;
        Self::write_ints32(out, count, values, start)?;
      }
    }

    Ok(())
  }

  fn write_ints8<DO>(out: &mut DO, count: usize, values: &[i32], offset: usize) -> Result<()>
  where
    DO: DataOutput,
  {
    let mut k = 0;
    while count - k >= Self::BLOCK_SIZE {
      let step = offset + k;
      for i in 0..16 {
        let l = ((values[step + i] as i64) << 56)
          | ((values[step + 16 + i] as i64) << 48)
          | ((values[step + 32 + i] as i64) << 40)
          | ((values[step + 48 + i] as i64) << 32)
          | ((values[step + 64 + i] as i64) << 24)
          | ((values[step + 80 + i] as i64) << 16)
          | ((values[step + 96 + i] as i64) << 8)
          | (values[step + 112 + i] as i64);
        out.write_long(l)?;
      }
      k += Self::BLOCK_SIZE;
    }
    for i in k..count {
      out.write_byte(values[offset + i] as u8)?;
    }

    Ok(())
  }
  #[inline(never)]
  fn write_ints16<DO>(out: &mut DO, count: usize, values: &[i32], offset: usize) -> Result<()>
  where
    DO: DataOutput,
  {
    let mut k = 0;
    while count - k >= Self::BLOCK_SIZE {
      let step = offset + k;
      let columns = values[step..step + Self::BLOCK_SIZE].as_chunks::<4>().0;
      for i in 0..8 {
        let row = |j: usize| wide::i32x4::new(columns[8 * j + i]);
        let high = (row(0) << 16u32) | row(1);
        let low = (row(2) << 16u32) | row(3);
        for (high, low) in high.to_array().into_iter().zip(low.to_array()) {
          out.write_long(((high as i64) << 32) | i64::from(low as u32))?;
        }
      }
      k += Self::BLOCK_SIZE;
    }
    for i in k..count {
      out.write_short(values[offset + i] as i16)?;
    }

    Ok(())
  }

  fn write_ints32<DO>(out: &mut DO, count: usize, values: &[i32], offset: usize) -> Result<()>
  where
    DO: DataOutput,
  {
    let mut k = 0;
    while count - k >= Self::BLOCK_SIZE {
      let step = offset + k;
      for i in 0..64 {
        let l = ((values[step + i] as i64) << 32) | (values[step + 64 + i] as i64);
        out.write_long(l)?;
      }
      k += Self::BLOCK_SIZE;
    }
    for i in k..count {
      out.write_int(values[offset + i])?;
    }

    Ok(())
  }
  pub(crate) fn read_ints<II>(
    input: &mut II,
    count: usize,
    values: &mut [i64],
    offset: usize,
  ) -> Result<()>
  where
    II: IndexInput,
  {
    let bpv = input.read_byte()? as i32;
    match bpv {
      0 => {
        let v = input.read_vint()? as i64;
        let start = offset;
        let end = start + count;
        values[start..end].fill(v);
      },
      8 => Self::read_ints8(input, count, values, offset)?,
      16 => Self::read_ints16(input, count, values, offset)?,
      32 => Self::read_ints32(input, count, values, offset)?,
      _ => {
        return Err(LuceneError::illegal_state(format!(
          "Unsupported number of bits per value: {bpv}"
        )));
      },
    }
    Ok(())
  }

  fn read_ints8<II>(input: &mut II, count: usize, values: &mut [i64], offset: usize) -> Result<()>
  where
    II: IndexInput,
  {
    let mut k = 0;
    while count - k >= Self::BLOCK_SIZE {
      let step = offset + k;
      input.read_longs(values, step, 16)?;
      for i in 0..16 {
        let l = values[step + i];
        values[step + i] = (l >> 56) & 0xFF;
        values[step + 16 + i] = (l >> 48) & 0xFF;
        values[step + 32 + i] = (l >> 40) & 0xFF;
        values[step + 48 + i] = (l >> 32) & 0xFF;
        values[step + 64 + i] = (l >> 24) & 0xFF;
        values[step + 80 + i] = (l >> 16) & 0xFF;
        values[step + 96 + i] = (l >> 8) & 0xFF;
        values[step + 112 + i] = l & 0xFF;
      }
      k += Self::BLOCK_SIZE;
    }
    for i in k..count {
      values[offset + i] = input.read_byte()? as i64;
    }
    Ok(())
  }

  fn read_ints16<II>(input: &mut II, count: usize, values: &mut [i64], offset: usize) -> Result<()>
  where
    II: IndexInput,
  {
    let mut k = 0;
    while count - k >= Self::BLOCK_SIZE {
      let step = offset + k;
      input.read_longs(values, step, 32)?;
      for i in 0..32 {
        let l = values[step + i];
        values[step + i] = (l >> 48) & 0xFFFF;
        values[step + 32 + i] = (l >> 32) & 0xFFFF;
        values[step + 64 + i] = (l >> 16) & 0xFFFF;
        values[step + 96 + i] = l & 0xFFFF;
      }
      k += Self::BLOCK_SIZE;
    }
    for i in k..count {
      values[offset + i] = input.read_short()? as u16 as i64;
    }
    Ok(())
  }

  fn read_ints32<II>(input: &mut II, count: usize, values: &mut [i64], offset: usize) -> Result<()>
  where
    II: IndexInput,
  {
    let mut k = 0;
    while count - k >= Self::BLOCK_SIZE {
      let step = offset + k;
      input.read_longs(values, step, 64)?;
      for i in 0..64 {
        let l = values[step + i];
        values[step + i] = (l >> 32) & 0xFFFFFFFF;
        values[step + 64 + i] = l & 0xFFFFFFFF;
      }
      k += Self::BLOCK_SIZE;
    }
    for i in k..count {
      values[offset + i] = input.read_int()? as i64;
    }
    Ok(())
  }
}
