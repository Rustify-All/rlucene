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

use crate::core::internal::vectorization::posting_decoding_util::PostingDecodingUtil;
use crate::core::store::{DataOutput, IndexInput};
use crate::core::util::error::lucene_error::Result;
/// Inspired by [bitpacking](https://fulmicoton.com/posts/bitpacking/)
///
/// Encodes multiple integers into an `i64` to achieve SIMD-like speedups.
///
/// - If `bits_per_value <= 8`, then 8 integers are packed into each `i64`.
/// - If `bits_per_value <= 16`, then 4 integers per `i64`.
/// - Otherwise, 2 integers per `i64`.
pub struct ForUtil {
  tmp: Box<[i32]>,
}
impl ForUtil {
  pub(crate) fn new() -> Self {
    Self {
      tmp: vec![0i32; Self::BLOCK_SIZE].into_boxed_slice(),
    }
  }
  pub const BLOCK_SIZE: usize = 128;
  pub const BLOCK_SIZE_LOG2: usize = 7;

  const fn expand_mask16(mask16: i32) -> i32 {
    mask16 | (mask16 << 16)
  }

  const fn expand_mask8(mask8: i32) -> i32 {
    Self::expand_mask16(mask8 | (mask8 << 8))
  }

  const fn mask32(bits_per_value: i32) -> i32 {
    ((1u32 << (bits_per_value as u32)) - 1) as i32
  }

  const fn mask16(bits_per_value: i32) -> i32 {
    Self::expand_mask16((1i32 << bits_per_value) - 1)
  }

  const fn mask8(bits_per_value: i32) -> i32 {
    Self::expand_mask8((1i32 << bits_per_value) - 1)
  }
  pub(crate) fn expand8(arr: &mut [i32]) {
    if arr.len() >= Self::BLOCK_SIZE {
      let arr = &mut arr[..Self::BLOCK_SIZE];
      for i in 0..32 {
        let l = arr[i] as u32;
        arr[i] = ((l >> 24) & 0xFF) as i32;
        arr[32 + i] = ((l >> 16) & 0xFF) as i32;
        arr[64 + i] = ((l >> 8) & 0xFF) as i32;
        arr[96 + i] = (l & 0xFF) as i32;
      }
      return;
    }
    for i in 0..32 {
      let l = arr[i] as u32;
      arr[i] = ((l >> 24) & 0xFF) as i32;
      arr[32 + i] = ((l >> 16) & 0xFF) as i32;
      arr[64 + i] = ((l >> 8) & 0xFF) as i32;
      arr[96 + i] = (l & 0xFF) as i32;
    }
  }
  pub(crate) fn collapse8(arr: &mut [i32]) {
    if arr.len() >= 128 {
      let (a, b) = arr[..128].split_at_mut(32);
      let (b, c) = b.split_at_mut(32);
      let (c, d) = c.split_at_mut(32);
      for i in 0..32 {
        a[i] = (a[i] << 24) | (b[i] << 16) | (c[i] << 8) | d[i];
      }
      return;
    }
    for i in 0..32 {
      arr[i] = (arr[i] << 24) | (arr[32 + i] << 16) | (arr[64 + i] << 8) | arr[96 + i];
    }
  }

  pub(crate) fn expand16(arr: &mut [i32]) {
    if arr.len() >= Self::BLOCK_SIZE {
      let arr = &mut arr[..Self::BLOCK_SIZE];
      for i in 0..64 {
        let l = arr[i] as u32;
        arr[i] = ((l >> 16) & 0xFFFF) as i32;
        arr[64 + i] = (l & 0xFFFF) as i32;
      }
      return;
    }
    for i in 0..64 {
      let l = arr[i] as u32;
      arr[i] = ((l >> 16) & 0xFFFF) as i32;
      arr[64 + i] = (l & 0xFFFF) as i32;
    }
  }

  pub(crate) fn collapse16(arr: &mut [i32]) {
    if arr.len() >= 128 {
      let (a, b) = arr[..128].split_at_mut(64);
      for i in 0..64 {
        a[i] = (a[i] << 16) | b[i];
      }
      return;
    }
    for i in 0..64 {
      arr[i] = (arr[i] << 16) | (arr[64 + i]);
    }
  }

  /// Encode 128 integers from `ints` into out`.
  pub(crate) fn encode<DO>(
    &mut self,
    ints: &mut [i32],
    bits_per_value: i32,
    out: &mut DO,
  ) -> Result<()>
  where
    DO: DataOutput,
  {
    let next_primitive = if bits_per_value <= 8 {
      Self::collapse8(ints);
      8
    } else if bits_per_value <= 16 {
      Self::collapse16(ints);
      16
    } else {
      32
    };
    Self::encode_with_tmp(ints, bits_per_value, next_primitive, out, &mut self.tmp)
  }

  pub(crate) fn encode_with_tmp<DO>(
    ints: &[i32],
    bits_per_value: i32,
    primitive_size: i32,
    out: &mut DO,
    tmp: &mut [i32],
  ) -> Result<()>
  where
    DO: DataOutput,
  {
    let num_ints = Self::BLOCK_SIZE * (primitive_size as usize) / i32::BITS as usize;
    let num_ints_per_shift = (bits_per_value * 4) as usize;

    if bits_per_value <= 4
      && primitive_size == 8
      && ints.len() >= 32
      && tmp.len() >= num_ints_per_shift
    {
      match bits_per_value {
        1 => {
          let input = ints[..32].as_chunks::<4>().0;
          let lanes = |i: usize| wide::i32x4::new(input[i]);
          let packed = (lanes(0) << 7u32)
            | (lanes(1) << 6u32)
            | (lanes(2) << 5u32)
            | (lanes(3) << 4u32)
            | (lanes(4) << 3u32)
            | (lanes(5) << 2u32)
            | (lanes(6) << 1u32)
            | lanes(7);
          tmp[..4].copy_from_slice(&packed.to_array());
          out.write_ints(&tmp[..4])?;
          return Ok(());
        },
        2 => {
          let input = ints[..32].as_chunks::<4>().0;
          let lanes = |i: usize| wide::i32x4::new(input[i]);
          for (i, packed) in tmp[..8].as_chunks_mut::<4>().0.iter_mut().enumerate() {
            *packed =
              ((lanes(i) << 6u32) | (lanes(2 + i) << 4u32) | (lanes(4 + i) << 2u32) | lanes(6 + i))
                .to_array();
          }
          out.write_ints(&tmp[..8])?;
          return Ok(());
        },
        4 => {
          let (high, low) = ints[..32].split_at(16);
          let high = high.as_chunks::<4>().0;
          let low = low.as_chunks::<4>().0;
          let packed = tmp[..16].as_chunks_mut::<4>().0;
          for ((packed, &high), &low) in packed.iter_mut().zip(high).zip(low) {
            *packed = ((wide::i32x4::new(high) << 4u32) | wide::i32x4::new(low)).to_array();
          }
          out.write_ints(&tmp[..16])?;
          return Ok(());
        },
        _ => {},
      }
    }
    let mut idx = 0;
    let mut shift = primitive_size - bits_per_value;
    for (t, l) in tmp.iter_mut().take(num_ints_per_shift).zip(&ints[idx..]) {
      *t = *l << shift;
    }
    idx += num_ints_per_shift;

    shift -= bits_per_value;
    while shift >= 0 {
      for (t, l) in tmp.iter_mut().take(num_ints_per_shift).zip(&ints[idx..]) {
        *t |= *l << shift;
      }
      idx += num_ints_per_shift;
      shift -= bits_per_value;
    }

    let remaining_bits_per_int = shift + bits_per_value;
    let remaining_bits_per_int_index = remaining_bits_per_int as usize;
    let mask_remaining_bits_per_int = match primitive_size {
      8 => Self::MASKS8[remaining_bits_per_int_index],
      16 => Self::MASKS16[remaining_bits_per_int_index],
      _ => Self::MASKS32[remaining_bits_per_int_index],
    };

    let packed_tail = if bits_per_value == 18
      && primitive_size == 32
      && ints.len() >= num_ints
      && tmp.len() >= num_ints_per_shift
    {
      match (primitive_size, bits_per_value) {
        (32, 18) => {
          use wide::{bytemuck::cast, u32x4};

          const LEFT_SHIFTS: [u32x4; 2] = [u32x4::new([0, 10, 6, 2]), u32x4::new([0, 12, 8, 4])];
          const RIGHT_SHIFTS: [u32x4; 2] = [u32x4::new([4, 8, 12, 16]), u32x4::new([2, 6, 10, 14])];
          const RIGHT_MASKS: [u32x4; 2] = [
            u32x4::new([0x3fff, 0x03ff, 0x003f, 0x0003]),
            u32x4::new([0x3fff, 0x0fff, 0x00ff, 0x000f]),
          ];
          const TAIL_MASK: u32x4 = u32x4::splat(0x3fff);

          let groups = ints[idx..num_ints].as_chunks::<7>().0;
          let packed = tmp[..num_ints_per_shift].as_chunks_mut::<9>().0;
          for (&[a, b, c, d, e, f, g], packed) in groups.iter().zip(packed) {
            // Seven 18-bit inputs contribute nine 14-bit tails.
            let left: [u32x4; 2] = [cast([0, a, b, c]), cast([0, d, e, f])];
            let right: [u32x4; 2] = [cast([a, b, c, d]), cast([d, e, f, g])];
            let (lanes, last) = packed.as_chunks_mut::<4>();
            for (j, words) in lanes.iter_mut().enumerate() {
              let tail = ((left[j] << LEFT_SHIFTS[j]) & TAIL_MASK)
                | ((right[j] >> RIGHT_SHIFTS[j]) & RIGHT_MASKS[j]);
              *words = cast(cast::<_, u32x4>(*words) | tail);
            }
            last[0] |= g & Self::MASKS32[14];
          }
          true
        },
        _ => false,
      }
    } else if bits_per_value == 9
      && primitive_size == 16
      && ints.len() >= num_ints
      && tmp.len() >= num_ints_per_shift
    {
      use wide::{bytemuck::cast, u32x4};

      const LEFT_SHIFTS: [u32x4; 2] = [u32x4::new([0, 5, 3, 1]), u32x4::new([0, 6, 4, 2])];
      const RIGHT_SHIFTS: [u32x4; 2] = [u32x4::new([2, 4, 6, 8]), u32x4::new([1, 3, 5, 7])];
      const LEFT_MASKS: [u32x4; 2] = [
        u32x4::new([0, 0x0003_0003, 0x000f_000f, 0x003f_003f]),
        u32x4::new([0, 0x0001_0001, 0x0007_0007, 0x001f_001f]),
      ];
      const RIGHT_MASKS: [u32x4; 2] = [
        u32x4::new([0x007f_007f, 0x001f_001f, 0x0007_0007, 0x0001_0001]),
        u32x4::new([0x007f_007f, 0x003f_003f, 0x000f_000f, 0x0003_0003]),
      ];

      let groups = ints[idx..num_ints].as_chunks::<7>().0;
      let packed = tmp[..num_ints_per_shift].as_chunks_mut::<9>().0;
      for (&[a, b, c, d, e, f, g], packed) in groups.iter().zip(packed) {
        // Each 16-bit lane holds seven 9-bit inputs for nine 7-bit tails.
        let left: [u32x4; 2] = [cast([0, a, b, c]), cast([0, d, e, f])];
        let right: [u32x4; 2] = [cast([a, b, c, d]), cast([d, e, f, g])];
        let (lanes, last) = packed.as_chunks_mut::<4>();
        for (j, words) in lanes.iter_mut().enumerate() {
          let tail = ((left[j] & LEFT_MASKS[j]) << LEFT_SHIFTS[j])
            | ((right[j] >> RIGHT_SHIFTS[j]) & RIGHT_MASKS[j]);
          *words = cast(cast::<_, u32x4>(*words) | tail);
        }
        last[0] |= g & Self::MASKS16[7];
      }
      true
    } else {
      false
    };
    if !packed_tail {
      let mut tmp_idx = 0;
      let mut remaining_bits_per_value = bits_per_value;
      while idx < num_ints {
        if remaining_bits_per_value >= remaining_bits_per_int {
          remaining_bits_per_value -= remaining_bits_per_int;
          tmp[tmp_idx] |=
            (ints[idx] as u32 >> remaining_bits_per_value) as i32 & mask_remaining_bits_per_int;
          if remaining_bits_per_value == 0 {
            idx += 1;
            remaining_bits_per_value = bits_per_value;
          }
          tmp_idx += 1;
        } else {
          let remaining_bits_per_value_index = remaining_bits_per_value as usize;
          let (mask1, mask2) = match primitive_size {
            8 => (
              Self::MASKS8[remaining_bits_per_value_index],
              Self::MASKS8[remaining_bits_per_int_index - remaining_bits_per_value_index],
            ),
            16 => (
              Self::MASKS16[remaining_bits_per_value_index],
              Self::MASKS16[remaining_bits_per_int_index - remaining_bits_per_value_index],
            ),
            _ => (
              Self::MASKS32[remaining_bits_per_value_index],
              Self::MASKS32[remaining_bits_per_int_index - remaining_bits_per_value_index],
            ),
          };

          tmp[tmp_idx] |=
            (ints[idx] & mask1) << (remaining_bits_per_int - remaining_bits_per_value);
          idx += 1;
          remaining_bits_per_value += bits_per_value - remaining_bits_per_int;
          tmp[tmp_idx] |= (ints[idx] as u32 >> remaining_bits_per_value) as i32 & mask2;
          tmp_idx += 1;
        }
      }
    }
    out.write_ints(&tmp[..num_ints_per_shift.min(tmp.len())])?;

    Ok(())
  }
  /// Number of bytes required to encode 128 integers of `bitsPerValue` bits
  /// per value.
  pub(crate) fn num_bytes(bits_per_value: i32) -> i32 {
    bits_per_value << (Self::BLOCK_SIZE_LOG2 - 3)
  }

  pub(crate) fn decode_slow<I>(
    bits_per_value: i32,
    pdu: &mut PostingDecodingUtil<I>,
    tmp: &mut [i32],
    ints: &mut [i32],
  ) -> Result<()>
  where
    I: IndexInput,
  {
    let num_ints = (bits_per_value << 2) as usize;
    let bits_per_value_index = bits_per_value as usize;
    let mask = Self::MASKS32[bits_per_value_index];
    pdu.split_ints_diff(num_ints, ints, 32 - bits_per_value, 32, mask, tmp, 0, -1)?;

    if ints.len() >= 128 && tmp.len() >= num_ints {
      match bits_per_value {
        18 => {
          for group in 0..8 {
            let t = group * 9;
            ints[72 + group * 7] = ((tmp[t] & Self::MASKS32[14]) << 4)
              | (((tmp[t + 1] as u32 >> 10) as i32) & Self::MASKS32[4]);
            ints[72 + group * 7 + 1] = ((tmp[t + 1] & Self::MASKS32[10]) << 8)
              | (((tmp[t + 2] as u32 >> 6) as i32) & Self::MASKS32[8]);
            ints[72 + group * 7 + 2] = ((tmp[t + 2] & Self::MASKS32[6]) << 12)
              | (((tmp[t + 3] as u32 >> 2) as i32) & Self::MASKS32[12]);
            ints[72 + group * 7 + 3] = ((tmp[t + 3] & Self::MASKS32[2]) << 16)
              | ((tmp[t + 4] & Self::MASKS32[14]) << 2)
              | (((tmp[t + 5] as u32 >> 12) as i32) & Self::MASKS32[2]);
            ints[72 + group * 7 + 4] = ((tmp[t + 5] & Self::MASKS32[12]) << 6)
              | (((tmp[t + 6] as u32 >> 8) as i32) & Self::MASKS32[6]);
            ints[72 + group * 7 + 5] = ((tmp[t + 6] & Self::MASKS32[8]) << 10)
              | (((tmp[t + 7] as u32 >> 4) as i32) & Self::MASKS32[10]);
            ints[72 + group * 7 + 6] =
              ((tmp[t + 7] & Self::MASKS32[4]) << 14) | (tmp[t + 8] & Self::MASKS32[14]);
          }
          return Ok(());
        },
        20 => {
          for group in 0..16 {
            let t = group * 5;
            ints[80 + group * 3] = ((tmp[t] & Self::MASKS32[12]) << 8)
              | (((tmp[t + 1] as u32 >> 4) as i32) & Self::MASKS32[8]);
            ints[80 + group * 3 + 1] = ((tmp[t + 1] & Self::MASKS32[4]) << 16)
              | ((tmp[t + 2] & Self::MASKS32[12]) << 4)
              | (((tmp[t + 3] as u32 >> 8) as i32) & Self::MASKS32[4]);
            ints[80 + group * 3 + 2] =
              ((tmp[t + 3] & Self::MASKS32[8]) << 12) | (tmp[t + 4] & Self::MASKS32[12]);
          }
          return Ok(());
        },
        22 => {
          for group in 0..8 {
            let t = group * 11;
            ints[88 + group * 5] = ((tmp[t] & Self::MASKS32[10]) << 12)
              | ((tmp[t + 1] & Self::MASKS32[10]) << 2)
              | (((tmp[t + 2] as u32 >> 8) as i32) & Self::MASKS32[2]);
            ints[88 + group * 5 + 1] = ((tmp[t + 2] & Self::MASKS32[8]) << 14)
              | ((tmp[t + 3] & Self::MASKS32[10]) << 4)
              | (((tmp[t + 4] as u32 >> 6) as i32) & Self::MASKS32[4]);
            ints[88 + group * 5 + 2] = ((tmp[t + 4] & Self::MASKS32[6]) << 16)
              | ((tmp[t + 5] & Self::MASKS32[10]) << 6)
              | (((tmp[t + 6] as u32 >> 4) as i32) & Self::MASKS32[6]);
            ints[88 + group * 5 + 3] = ((tmp[t + 6] & Self::MASKS32[4]) << 18)
              | ((tmp[t + 7] & Self::MASKS32[10]) << 8)
              | (((tmp[t + 8] as u32 >> 2) as i32) & Self::MASKS32[8]);
            ints[88 + group * 5 + 4] = ((tmp[t + 8] & Self::MASKS32[2]) << 20)
              | ((tmp[t + 9] & Self::MASKS32[10]) << 10)
              | (tmp[t + 10] & Self::MASKS32[10]);
          }
          return Ok(());
        },
        24 => {
          for group in 0..32 {
            let t = group * 3;
            ints[96 + group] = ((tmp[t] & Self::MASKS32[8]) << 16)
              | ((tmp[t + 1] & Self::MASKS32[8]) << 8)
              | (tmp[t + 2] & Self::MASKS32[8]);
          }
          return Ok(());
        },
        26 => {
          for group in 0..8 {
            let t = group * 13;
            ints[104 + group * 3] = ((tmp[t] & Self::MASKS32[6]) << 20)
              | ((tmp[t + 1] & Self::MASKS32[6]) << 14)
              | ((tmp[t + 2] & Self::MASKS32[6]) << 8)
              | ((tmp[t + 3] & Self::MASKS32[6]) << 2)
              | (((tmp[t + 4] as u32 >> 4) as i32) & Self::MASKS32[2]);
            ints[104 + group * 3 + 1] = ((tmp[t + 4] & Self::MASKS32[4]) << 22)
              | ((tmp[t + 5] & Self::MASKS32[6]) << 16)
              | ((tmp[t + 6] & Self::MASKS32[6]) << 10)
              | ((tmp[t + 7] & Self::MASKS32[6]) << 4)
              | (((tmp[t + 8] as u32 >> 2) as i32) & Self::MASKS32[4]);
            ints[104 + group * 3 + 2] = ((tmp[t + 8] & Self::MASKS32[2]) << 24)
              | ((tmp[t + 9] & Self::MASKS32[6]) << 18)
              | ((tmp[t + 10] & Self::MASKS32[6]) << 12)
              | ((tmp[t + 11] & Self::MASKS32[6]) << 6)
              | (tmp[t + 12] & Self::MASKS32[6]);
          }
          return Ok(());
        },
        27 => {
          for group in 0..4 {
            let t = group * 27;
            ints[108 + group * 5] = ((tmp[t] & Self::MASKS32[5]) << 22)
              | ((tmp[t + 1] & Self::MASKS32[5]) << 17)
              | ((tmp[t + 2] & Self::MASKS32[5]) << 12)
              | ((tmp[t + 3] & Self::MASKS32[5]) << 7)
              | ((tmp[t + 4] & Self::MASKS32[5]) << 2)
              | (((tmp[t + 5] as u32 >> 3) as i32) & Self::MASKS32[2]);
            ints[108 + group * 5 + 1] = ((tmp[t + 5] & Self::MASKS32[3]) << 24)
              | ((tmp[t + 6] & Self::MASKS32[5]) << 19)
              | ((tmp[t + 7] & Self::MASKS32[5]) << 14)
              | ((tmp[t + 8] & Self::MASKS32[5]) << 9)
              | ((tmp[t + 9] & Self::MASKS32[5]) << 4)
              | (((tmp[t + 10] as u32 >> 1) as i32) & Self::MASKS32[4]);
            ints[108 + group * 5 + 2] = ((tmp[t + 10] & Self::MASKS32[1]) << 26)
              | ((tmp[t + 11] & Self::MASKS32[5]) << 21)
              | ((tmp[t + 12] & Self::MASKS32[5]) << 16)
              | ((tmp[t + 13] & Self::MASKS32[5]) << 11)
              | ((tmp[t + 14] & Self::MASKS32[5]) << 6)
              | ((tmp[t + 15] & Self::MASKS32[5]) << 1)
              | (((tmp[t + 16] as u32 >> 4) as i32) & Self::MASKS32[1]);
            ints[108 + group * 5 + 3] = ((tmp[t + 16] & Self::MASKS32[4]) << 23)
              | ((tmp[t + 17] & Self::MASKS32[5]) << 18)
              | ((tmp[t + 18] & Self::MASKS32[5]) << 13)
              | ((tmp[t + 19] & Self::MASKS32[5]) << 8)
              | ((tmp[t + 20] & Self::MASKS32[5]) << 3)
              | (((tmp[t + 21] as u32 >> 2) as i32) & Self::MASKS32[3]);
            ints[108 + group * 5 + 4] = ((tmp[t + 21] & Self::MASKS32[2]) << 25)
              | ((tmp[t + 22] & Self::MASKS32[5]) << 20)
              | ((tmp[t + 23] & Self::MASKS32[5]) << 15)
              | ((tmp[t + 24] & Self::MASKS32[5]) << 10)
              | ((tmp[t + 25] & Self::MASKS32[5]) << 5)
              | (tmp[t + 26] & Self::MASKS32[5]);
          }
          return Ok(());
        },
        28 => {
          for group in 0..16 {
            let t = group * 7;
            ints[112 + group] = ((tmp[t] & Self::MASKS32[4]) << 24)
              | ((tmp[t + 1] & Self::MASKS32[4]) << 20)
              | ((tmp[t + 2] & Self::MASKS32[4]) << 16)
              | ((tmp[t + 3] & Self::MASKS32[4]) << 12)
              | ((tmp[t + 4] & Self::MASKS32[4]) << 8)
              | ((tmp[t + 5] & Self::MASKS32[4]) << 4)
              | (tmp[t + 6] & Self::MASKS32[4]);
          }
          return Ok(());
        },
        30 => {
          for group in 0..8 {
            let t = group * 15;
            ints[120 + group] = ((tmp[t] & Self::MASKS32[2]) << 28)
              | ((tmp[t + 1] & Self::MASKS32[2]) << 26)
              | ((tmp[t + 2] & Self::MASKS32[2]) << 24)
              | ((tmp[t + 3] & Self::MASKS32[2]) << 22)
              | ((tmp[t + 4] & Self::MASKS32[2]) << 20)
              | ((tmp[t + 5] & Self::MASKS32[2]) << 18)
              | ((tmp[t + 6] & Self::MASKS32[2]) << 16)
              | ((tmp[t + 7] & Self::MASKS32[2]) << 14)
              | ((tmp[t + 8] & Self::MASKS32[2]) << 12)
              | ((tmp[t + 9] & Self::MASKS32[2]) << 10)
              | ((tmp[t + 10] & Self::MASKS32[2]) << 8)
              | ((tmp[t + 11] & Self::MASKS32[2]) << 6)
              | ((tmp[t + 12] & Self::MASKS32[2]) << 4)
              | ((tmp[t + 13] & Self::MASKS32[2]) << 2)
              | (tmp[t + 14] & Self::MASKS32[2]);
          }
          return Ok(());
        },
        31 => {
          for group in 0..4 {
            let t = group * 31;
            ints[124 + group] = ((tmp[t] & Self::MASKS32[1]) << 30)
              | ((tmp[t + 1] & Self::MASKS32[1]) << 29)
              | ((tmp[t + 2] & Self::MASKS32[1]) << 28)
              | ((tmp[t + 3] & Self::MASKS32[1]) << 27)
              | ((tmp[t + 4] & Self::MASKS32[1]) << 26)
              | ((tmp[t + 5] & Self::MASKS32[1]) << 25)
              | ((tmp[t + 6] & Self::MASKS32[1]) << 24)
              | ((tmp[t + 7] & Self::MASKS32[1]) << 23)
              | ((tmp[t + 8] & Self::MASKS32[1]) << 22)
              | ((tmp[t + 9] & Self::MASKS32[1]) << 21)
              | ((tmp[t + 10] & Self::MASKS32[1]) << 20)
              | ((tmp[t + 11] & Self::MASKS32[1]) << 19)
              | ((tmp[t + 12] & Self::MASKS32[1]) << 18)
              | ((tmp[t + 13] & Self::MASKS32[1]) << 17)
              | ((tmp[t + 14] & Self::MASKS32[1]) << 16)
              | ((tmp[t + 15] & Self::MASKS32[1]) << 15)
              | ((tmp[t + 16] & Self::MASKS32[1]) << 14)
              | ((tmp[t + 17] & Self::MASKS32[1]) << 13)
              | ((tmp[t + 18] & Self::MASKS32[1]) << 12)
              | ((tmp[t + 19] & Self::MASKS32[1]) << 11)
              | ((tmp[t + 20] & Self::MASKS32[1]) << 10)
              | ((tmp[t + 21] & Self::MASKS32[1]) << 9)
              | ((tmp[t + 22] & Self::MASKS32[1]) << 8)
              | ((tmp[t + 23] & Self::MASKS32[1]) << 7)
              | ((tmp[t + 24] & Self::MASKS32[1]) << 6)
              | ((tmp[t + 25] & Self::MASKS32[1]) << 5)
              | ((tmp[t + 26] & Self::MASKS32[1]) << 4)
              | ((tmp[t + 27] & Self::MASKS32[1]) << 3)
              | ((tmp[t + 28] & Self::MASKS32[1]) << 2)
              | ((tmp[t + 29] & Self::MASKS32[1]) << 1)
              | (tmp[t + 30] & Self::MASKS32[1]);
          }
          return Ok(());
        },
        _ => {},
      }
    }
    let remaining_bits_per_int = (32 - bits_per_value) as usize;
    let mask32_remaining_bits_per_int = Self::MASKS32[remaining_bits_per_int];

    let mut tmp_idx = 0;
    let mut remaining_bits = remaining_bits_per_int;
    if ints.len() < Self::BLOCK_SIZE {
      let mut out_idx = num_ints;
      while out_idx < Self::BLOCK_SIZE {
        let mut b = bits_per_value_index - remaining_bits;
        let mut l = (tmp[tmp_idx] & Self::MASKS32[remaining_bits]) << b;
        tmp_idx += 1;

        while b >= remaining_bits_per_int {
          b -= remaining_bits_per_int;
          l |= (tmp[tmp_idx] & mask32_remaining_bits_per_int) << b;
          tmp_idx += 1;
        }

        if b > 0 {
          l |= (tmp[tmp_idx] >> (remaining_bits_per_int - b)) & Self::MASKS32[b];
          remaining_bits = remaining_bits_per_int - b;
        } else {
          remaining_bits = remaining_bits_per_int;
        }
        ints[out_idx] = l;
        out_idx += 1;
      }
      return Ok(());
    }
    for out in ints.iter_mut().take(Self::BLOCK_SIZE).skip(num_ints) {
      let mut b = bits_per_value_index - remaining_bits;
      let mut l = (tmp[tmp_idx] & Self::MASKS32[remaining_bits]) << b;
      tmp_idx += 1;

      while b >= remaining_bits_per_int {
        b -= remaining_bits_per_int;
        l |= (tmp[tmp_idx] & mask32_remaining_bits_per_int) << b;
        tmp_idx += 1;
      }

      if b > 0 {
        l |= (tmp[tmp_idx] >> (remaining_bits_per_int - b)) & Self::MASKS32[b];
        remaining_bits = remaining_bits_per_int - b;
      } else {
        remaining_bits = remaining_bits_per_int;
      }
      *out = l;
    }

    Ok(())
  }

  const MASKS8: [i32; 8] = {
    let mut masks = [0i32; 8];
    let mut i = 0;
    while i < 8 {
      masks[i] = Self::mask8(i as i32);
      i += 1;
    }
    masks
  };

  const MASKS16: [i32; 16] = {
    let mut masks = [0i32; 16];
    let mut i = 0;
    while i < 16 {
      masks[i] = Self::mask16(i as i32);
      i += 1;
    }
    masks
  };

  const MASKS32: [i32; 32] = {
    let mut masks = [0i32; 32];
    let mut i = 0;
    while i < 32 {
      masks[i] = Self::mask32(i as i32);
      i += 1;
    }
    masks
  };

  pub const MASK8_1: i32 = Self::MASKS8[1];
  pub const MASK8_2: i32 = Self::MASKS8[2];
  pub const MASK8_3: i32 = Self::MASKS8[3];
  pub const MASK8_4: i32 = Self::MASKS8[4];
  pub const MASK8_5: i32 = Self::MASKS8[5];
  pub const MASK8_6: i32 = Self::MASKS8[6];
  pub const MASK8_7: i32 = Self::MASKS8[7];

  pub const MASK16_1: i32 = Self::MASKS16[1];
  pub const MASK16_2: i32 = Self::MASKS16[2];
  pub const MASK16_3: i32 = Self::MASKS16[3];
  pub const MASK16_4: i32 = Self::MASKS16[4];
  pub const MASK16_5: i32 = Self::MASKS16[5];
  pub const MASK16_6: i32 = Self::MASKS16[6];
  pub const MASK16_7: i32 = Self::MASKS16[7];
  pub const MASK16_8: i32 = Self::MASKS16[8];
  pub const MASK16_9: i32 = Self::MASKS16[9];
  pub const MASK16_10: i32 = Self::MASKS16[10];
  pub const MASK16_11: i32 = Self::MASKS16[11];
  pub const MASK16_12: i32 = Self::MASKS16[12];
  pub const MASK16_13: i32 = Self::MASKS16[13];
  pub const MASK16_14: i32 = Self::MASKS16[14];
  pub const MASK16_15: i32 = Self::MASKS16[15];

  pub const MASK32_1: i32 = Self::MASKS32[1];
  pub const MASK32_2: i32 = Self::MASKS32[2];
  pub const MASK32_3: i32 = Self::MASKS32[3];
  pub const MASK32_4: i32 = Self::MASKS32[4];
  pub const MASK32_5: i32 = Self::MASKS32[5];
  pub const MASK32_6: i32 = Self::MASKS32[6];
  pub const MASK32_7: i32 = Self::MASKS32[7];
  pub const MASK32_8: i32 = Self::MASKS32[8];
  pub const MASK32_9: i32 = Self::MASKS32[9];
  pub const MASK32_10: i32 = Self::MASKS32[10];
  pub const MASK32_11: i32 = Self::MASKS32[11];
  pub const MASK32_12: i32 = Self::MASKS32[12];
  pub const MASK32_13: i32 = Self::MASKS32[13];
  pub const MASK32_14: i32 = Self::MASKS32[14];
  pub const MASK32_15: i32 = Self::MASKS32[15];
  pub const MASK32_16: i32 = Self::MASKS32[16];
  /// Decode 128 integers into `[i32]`.
  pub(crate) fn decode<I>(
    &mut self,
    bits_per_value: i32,
    pdu: &mut PostingDecodingUtil<I>,
    ints: &mut [i32],
  ) -> Result<()>
  where
    I: IndexInput,
  {
    match bits_per_value {
      1 => {
        Self::decode1(pdu, ints)?;
        Self::expand8(ints);
      },
      2 => {
        Self::decode2(pdu, ints)?;
        Self::expand8(ints);
      },
      3 => {
        Self::decode3(pdu, &mut self.tmp, ints)?;
        Self::expand8(ints);
      },
      4 => {
        Self::decode4(pdu, ints)?;
        Self::expand8(ints);
      },
      5 => {
        Self::decode5(pdu, &mut self.tmp, ints)?;
        Self::expand8(ints);
      },
      6 => {
        Self::decode6(pdu, &mut self.tmp, ints)?;
        Self::expand8(ints);
      },
      7 => {
        Self::decode7(pdu, &mut self.tmp, ints)?;
        Self::expand8(ints);
      },
      8 => {
        Self::decode8(pdu, ints)?;
        Self::expand8(ints);
      },
      9 => {
        Self::decode9(pdu, &mut self.tmp, ints)?;
        Self::expand16(ints);
      },
      10 => {
        Self::decode10(pdu, &mut self.tmp, ints)?;
        Self::expand16(ints);
      },
      11 => {
        Self::decode11(pdu, &mut self.tmp, ints)?;
        Self::expand16(ints);
      },
      12 => {
        Self::decode12(pdu, &mut self.tmp, ints)?;
        Self::expand16(ints);
      },
      13 => {
        Self::decode13(pdu, &mut self.tmp, ints)?;
        Self::expand16(ints);
      },
      14 => {
        Self::decode14(pdu, &mut self.tmp, ints)?;
        Self::expand16(ints);
      },
      15 => {
        Self::decode15(pdu, &mut self.tmp, ints)?;
        Self::expand16(ints);
      },
      16 => {
        Self::decode16(pdu, ints)?;
        Self::expand16(ints);
      },
      _ => {
        Self::decode_slow(bits_per_value, pdu, &mut self.tmp, ints)?;
      },
    }
    Ok(())
  }

  pub(crate) fn decode1<I>(pdu: &mut PostingDecodingUtil<I>, ints: &mut [i32]) -> Result<()>
  where
    I: IndexInput,
  {
    pdu.split_ints_same(4, ints, 7, 1, Self::MASK8_1, 28, Self::MASK8_1)
  }
  pub(crate) fn decode2<I>(pdu: &mut PostingDecodingUtil<I>, ints: &mut [i32]) -> Result<()>
  where
    I: IndexInput,
  {
    pdu.split_ints_same(8, ints, 6, 2, Self::MASK8_2, 24, Self::MASK8_2)
  }

  pub(crate) fn decode3<I>(
    pdu: &mut PostingDecodingUtil<I>,
    tmp: &mut [i32],
    ints: &mut [i32],
  ) -> Result<()>
  where
    I: IndexInput,
  {
    pdu.split_ints_diff(12, ints, 5, 3, Self::MASK8_3, tmp, 0, Self::MASK8_2)?;

    let mut iter = 0;
    let mut tmp_idx = 0;
    let mut ints_idx = 24;

    while iter < 4 {
      let mut l0 = tmp[tmp_idx] << 1;
      l0 |= ((tmp[tmp_idx + 1] as u32) >> 1) as i32 & Self::MASK8_1;
      ints[ints_idx] = l0;
      let mut l1 = (tmp[tmp_idx + 1] & Self::MASK8_1) << 2;
      l1 |= tmp[tmp_idx + 2];
      ints[ints_idx + 1] = l1;
      iter += 1;
      tmp_idx += 3;
      ints_idx += 2;
    }
    Ok(())
  }
  pub(crate) fn decode4<I>(pdu: &mut PostingDecodingUtil<I>, ints: &mut [i32]) -> Result<()>
  where
    I: IndexInput,
  {
    pdu.split_ints_same(16, ints, 4, 4, Self::MASK8_4, 16, Self::MASK8_4)
  }
  fn decode5<I>(pdu: &mut PostingDecodingUtil<I>, tmp: &mut [i32], ints: &mut [i32]) -> Result<()>
  where
    I: IndexInput,
  {
    pdu.split_ints_diff(20, ints, 3, 5, Self::MASK8_5, tmp, 0, Self::MASK8_3)?;
    let mut tmp_idx = 0;
    let mut ints_idx = 20;
    for _ in 0..4 {
      let mut l0 = tmp[tmp_idx] << 2;
      l0 |= ((tmp[tmp_idx + 1] as u32) >> 1) as i32 & Self::MASK8_2;
      ints[ints_idx] = l0;

      let mut l1 = (tmp[tmp_idx + 1] & Self::MASK8_1) << 4;
      l1 |= tmp[tmp_idx + 2] << 1;
      l1 |= ((tmp[tmp_idx + 3] as u32) >> 2) as i32 & Self::MASK8_1;
      ints[ints_idx + 1] = l1;

      let mut l2 = (tmp[tmp_idx + 3] & Self::MASK8_2) << 3;
      l2 |= tmp[tmp_idx + 4];
      ints[ints_idx + 2] = l2;

      tmp_idx += 5;
      ints_idx += 3;
    }
    Ok(())
  }
  fn decode6<I>(pdu: &mut PostingDecodingUtil<I>, tmp: &mut [i32], ints: &mut [i32]) -> Result<()>
  where
    I: IndexInput,
  {
    pdu.split_ints_diff(24, ints, 2, 6, Self::MASK8_6, tmp, 0, Self::MASK8_2)?;
    for (offset, tmp_idx) in (0..24).step_by(3).enumerate() {
      let ints_idx = 24 + offset;
      let mut l0 = tmp[tmp_idx] << 4;
      l0 |= tmp[tmp_idx + 1] << 2;
      l0 |= tmp[tmp_idx + 2];
      ints[ints_idx] = l0;
    }
    Ok(())
  }

  fn decode7<I>(pdu: &mut PostingDecodingUtil<I>, tmp: &mut [i32], ints: &mut [i32]) -> Result<()>
  where
    I: IndexInput,
  {
    pdu.split_ints_diff(28, ints, 1, 7, Self::MASK8_7, tmp, 0, Self::MASK8_1)?;
    for (offset, tmp_idx) in (0..28).step_by(7).enumerate() {
      let ints_idx = 28 + offset;
      let mut l0 = tmp[tmp_idx] << 6;
      l0 |= tmp[tmp_idx + 1] << 5;
      l0 |= tmp[tmp_idx + 2] << 4;
      l0 |= tmp[tmp_idx + 3] << 3;
      l0 |= tmp[tmp_idx + 4] << 2;
      l0 |= tmp[tmp_idx + 5] << 1;
      l0 |= tmp[tmp_idx + 6];
      ints[ints_idx] = l0;
    }
    Ok(())
  }
  fn decode8<I>(pdu: &mut PostingDecodingUtil<I>, ints: &mut [i32]) -> Result<()>
  where
    I: IndexInput,
  {
    pdu.input.read_ints(ints, 0, 32)
  }

  pub(crate) fn decode9<I>(
    pdu: &mut PostingDecodingUtil<I>,
    tmp: &mut [i32],
    ints: &mut [i32],
  ) -> Result<()>
  where
    I: IndexInput,
  {
    pdu.split_ints_diff(36, ints, 7, 9, Self::MASK16_9, tmp, 0, Self::MASK16_7)?;
    let mut tmp_idx = 0;
    let mut ints_idx = 36;
    for _ in 0..4 {
      let mut l0 = tmp[tmp_idx] << 2;
      l0 |= ((tmp[tmp_idx + 1] as u32) >> 5) as i32 & Self::MASK16_2;
      ints[ints_idx] = l0;

      let mut l1 = (tmp[tmp_idx + 1] & Self::MASK16_5) << 4;
      l1 |= ((tmp[tmp_idx + 2] as u32) >> 3) as i32 & Self::MASK16_4;
      ints[ints_idx + 1] = l1;

      let mut l2 = (tmp[tmp_idx + 2] & Self::MASK16_3) << 6;
      l2 |= ((tmp[tmp_idx + 3] as u32) >> 1) as i32 & Self::MASK16_6;
      ints[ints_idx + 2] = l2;

      let mut l3 = (tmp[tmp_idx + 3] & Self::MASK16_1) << 8;
      l3 |= tmp[tmp_idx + 4] << 1;
      l3 |= ((tmp[tmp_idx + 5] as u32) >> 6) as i32 & Self::MASK16_1;
      ints[ints_idx + 3] = l3;

      let mut l4 = (tmp[tmp_idx + 5] & Self::MASK16_6) << 3;
      l4 |= ((tmp[tmp_idx + 6] as u32) >> 4) as i32 & Self::MASK16_3;
      ints[ints_idx + 4] = l4;

      let mut l5 = (tmp[tmp_idx + 6] & Self::MASK16_4) << 5;
      l5 |= ((tmp[tmp_idx + 7] as u32) >> 2) as i32 & Self::MASK16_5;
      ints[ints_idx + 5] = l5;

      let mut l6 = (tmp[tmp_idx + 7] & Self::MASK16_2) << 7;
      l6 |= tmp[tmp_idx + 8];
      ints[ints_idx + 6] = l6;

      tmp_idx += 9;
      ints_idx += 7;
    }
    Ok(())
  }

  pub(crate) fn decode10<I>(
    pdu: &mut PostingDecodingUtil<I>,
    tmp: &mut [i32],
    ints: &mut [i32],
  ) -> Result<()>
  where
    I: IndexInput,
  {
    pdu.split_ints_diff(40, ints, 6, 10, Self::MASK16_10, tmp, 0, Self::MASK16_6)?;
    let mut tmp_idx = 0;
    let mut ints_idx = 40;
    for _ in 0..8 {
      let mut l0 = tmp[tmp_idx] << 4;
      l0 |= ((tmp[tmp_idx + 1] as u32) >> 2) as i32 & Self::MASK16_4;
      ints[ints_idx] = l0;

      let mut l1 = (tmp[tmp_idx + 1] & Self::MASK16_2) << 8;
      l1 |= tmp[tmp_idx + 2] << 2;
      l1 |= ((tmp[tmp_idx + 3] as u32) >> 4) as i32 & Self::MASK16_2;
      ints[ints_idx + 1] = l1;

      let mut l2 = (tmp[tmp_idx + 3] & Self::MASK16_4) << 6;
      l2 |= tmp[tmp_idx + 4];
      ints[ints_idx + 2] = l2;

      tmp_idx += 5;
      ints_idx += 3;
    }
    Ok(())
  }

  pub(crate) fn decode11<I>(
    pdu: &mut PostingDecodingUtil<I>,
    tmp: &mut [i32],
    ints: &mut [i32],
  ) -> Result<()>
  where
    I: IndexInput,
  {
    pdu.split_ints_diff(44, ints, 5, 11, Self::MASK16_11, tmp, 0, Self::MASK16_5)?;
    let mut tmp_idx = 0;
    let mut ints_idx = 44;
    for _ in 0..4 {
      let mut l0 = tmp[tmp_idx] << 6;
      l0 |= tmp[tmp_idx + 1] << 1;
      l0 |= ((tmp[tmp_idx + 2] as u32) >> 4) as i32 & Self::MASK16_1;
      ints[ints_idx] = l0;

      let mut l1 = (tmp[tmp_idx + 2] & Self::MASK16_4) << 7;
      l1 |= tmp[tmp_idx + 3] << 2;
      l1 |= ((tmp[tmp_idx + 4] as u32) >> 3) as i32 & Self::MASK16_2;
      ints[ints_idx + 1] = l1;

      let mut l2 = (tmp[tmp_idx + 4] & Self::MASK16_3) << 8;
      l2 |= tmp[tmp_idx + 5] << 3;
      l2 |= ((tmp[tmp_idx + 6] as u32) >> 2) as i32 & Self::MASK16_3;
      ints[ints_idx + 2] = l2;

      let mut l3 = (tmp[tmp_idx + 6] & Self::MASK16_2) << 9;
      l3 |= tmp[tmp_idx + 7] << 4;
      l3 |= ((tmp[tmp_idx + 8] as u32) >> 1) as i32 & Self::MASK16_4;
      ints[ints_idx + 3] = l3;

      let mut l4 = (tmp[tmp_idx + 8] & Self::MASK16_1) << 10;
      l4 |= tmp[tmp_idx + 9] << 5;
      l4 |= tmp[tmp_idx + 10];
      ints[ints_idx + 4] = l4;

      tmp_idx += 11;
      ints_idx += 5;
    }
    Ok(())
  }
  fn decode12<I>(pdu: &mut PostingDecodingUtil<I>, tmp: &mut [i32], ints: &mut [i32]) -> Result<()>
  where
    I: IndexInput,
  {
    pdu.split_ints_diff(48, ints, 4, 12, Self::MASK16_12, tmp, 0, Self::MASK16_4)?;
    for (offset, tmp_idx) in (0..48).step_by(3).enumerate() {
      let ints_idx = 48 + offset;
      let l0 = (tmp[tmp_idx] << 8) | (tmp[tmp_idx + 1] << 4) | tmp[tmp_idx + 2];
      ints[ints_idx] = l0;
    }
    Ok(())
  }

  fn decode13<I>(pdu: &mut PostingDecodingUtil<I>, tmp: &mut [i32], ints: &mut [i32]) -> Result<()>
  where
    I: IndexInput,
  {
    pdu.split_ints_diff(52, ints, 3, 13, Self::MASK16_13, tmp, 0, Self::MASK16_3)?;
    let mut tmp_idx = 0;
    let mut ints_idx = 52;
    for _ in 0..4 {
      let mut l0 = tmp[tmp_idx] << 10;
      l0 |= tmp[tmp_idx + 1] << 7;
      l0 |= tmp[tmp_idx + 2] << 4;
      l0 |= tmp[tmp_idx + 3] << 1;
      l0 |= ((tmp[tmp_idx + 4] as u32) >> 2) as i32 & Self::MASK16_1;
      ints[ints_idx] = l0;

      let mut l1 = (tmp[tmp_idx + 4] & Self::MASK16_2) << 11;
      l1 |= tmp[tmp_idx + 5] << 8;
      l1 |= tmp[tmp_idx + 6] << 5;
      l1 |= tmp[tmp_idx + 7] << 2;
      l1 |= ((tmp[tmp_idx + 8] as u32) >> 1) as i32 & Self::MASK16_2;
      ints[ints_idx + 1] = l1;

      let mut l2 = (tmp[tmp_idx + 8] & Self::MASK16_1) << 12;
      l2 |= tmp[tmp_idx + 9] << 9;
      l2 |= tmp[tmp_idx + 10] << 6;
      l2 |= tmp[tmp_idx + 11] << 3;
      l2 |= tmp[tmp_idx + 12];
      ints[ints_idx + 2] = l2;

      tmp_idx += 13;
      ints_idx += 3;
    }
    Ok(())
  }

  fn decode14<I>(pdu: &mut PostingDecodingUtil<I>, tmp: &mut [i32], ints: &mut [i32]) -> Result<()>
  where
    I: IndexInput,
  {
    pdu.split_ints_diff(56, ints, 2, 14, Self::MASK16_14, tmp, 0, Self::MASK16_2)?;
    for (offset, tmp_idx) in (0..56).step_by(7).enumerate() {
      let ints_idx = 56 + offset;
      let l0 = (tmp[tmp_idx] << 12)
        | (tmp[tmp_idx + 1] << 10)
        | (tmp[tmp_idx + 2] << 8)
        | (tmp[tmp_idx + 3] << 6)
        | (tmp[tmp_idx + 4] << 4)
        | (tmp[tmp_idx + 5] << 2)
        | tmp[tmp_idx + 6];
      ints[ints_idx] = l0;
    }
    Ok(())
  }

  fn decode15<I>(pdu: &mut PostingDecodingUtil<I>, tmp: &mut [i32], ints: &mut [i32]) -> Result<()>
  where
    I: IndexInput,
  {
    pdu.split_ints_diff(60, ints, 1, 15, Self::MASK16_15, tmp, 0, Self::MASK16_1)?;
    for (offset, tmp_idx) in (0..60).step_by(15).enumerate() {
      let ints_idx = 60 + offset;
      let l0 = (tmp[tmp_idx] << 14)
        | (tmp[tmp_idx + 1] << 13)
        | (tmp[tmp_idx + 2] << 12)
        | (tmp[tmp_idx + 3] << 11)
        | (tmp[tmp_idx + 4] << 10)
        | (tmp[tmp_idx + 5] << 9)
        | (tmp[tmp_idx + 6] << 8)
        | (tmp[tmp_idx + 7] << 7)
        | (tmp[tmp_idx + 8] << 6)
        | (tmp[tmp_idx + 9] << 5)
        | (tmp[tmp_idx + 10] << 4)
        | (tmp[tmp_idx + 11] << 3)
        | (tmp[tmp_idx + 12] << 2)
        | (tmp[tmp_idx + 13] << 1)
        | tmp[tmp_idx + 14];
      ints[ints_idx] = l0;
    }
    Ok(())
  }

  fn decode16<I>(pdu: &mut PostingDecodingUtil<I>, ints: &mut [i32]) -> Result<()>
  where
    I: IndexInput,
  {
    pdu.input.read_ints(ints, 0, 64)
  }
}
