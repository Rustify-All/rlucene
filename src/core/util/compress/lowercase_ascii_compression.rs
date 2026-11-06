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
use crate::core::index::BytesRef;
use crate::core::store::{DataInput, DataOutput};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use wide::u8x16;

/// Utility that efficiently compresses arrays mostly containing characters in
/// the `[0x1F, 0x3F)` or `[0x5F, 0x7F)` ranges,
/// which notably include all digits, lowercase letters, `.`, `-`, and `_`.
pub struct LowercaseAsciiCompression;
impl LowercaseAsciiCompression {
  fn is_compressible(b: i32) -> bool {
    (b.wrapping_add(1) & 0xA0) == 0x20
  }
  /// Compresses `input[0..len]` into `out`.
  ///
  /// Returns `false` if the content cannot be compressed.
  /// If compression succeeds, the number of bytes written is guaranteed to be
  /// less than `len`.
  pub fn compress<DO>(input: &[u8], len: usize, tmp: &mut [u8], out: &mut DO) -> Result<bool>
  where
    DO: DataOutput,
  {
    if len < 8 {
      return Ok(false);
    }

    // 1. Count exceptions and fail compression if there are too many of them.
    let max_exceptions = len >> 5;
    let mut previous_exception_index = 0;
    let mut num_exceptions = 0;

    let mut block_start = 0;
    while block_start < len {
      let block_end = block_start + 16.min(len - block_start);
      let clean_block = block_end - block_start == 16
        && input.get(block_start..block_end).is_some_and(|block| {
          block.iter().fold(0u8, |bad, &b| {
            bad | u8::from((b.wrapping_add(1) & 0xA0) != 0x20)
          }) == 0
        });
      if !clean_block {
        // Indexed reads retain early false and partial-input failure timing.
        #[allow(clippy::needless_range_loop)]
        for i in block_start..block_end {
          let b = input[i] as i32;
          if !Self::is_compressible(b) {
            while i - previous_exception_index > 0xFF {
              num_exceptions += 1;
              previous_exception_index += 0xFF;
            }
            num_exceptions += 1;
            if num_exceptions > max_exceptions {
              return Ok(false);
            }
            previous_exception_index = i;
          }
        }
      }
      block_start = block_end;
    }

    debug_assert!(num_exceptions <= max_exceptions);
    let input = &input[..len];

    // 2. Move to 6-bit space
    let compressed_len = len - (len >> 2);
    debug_assert!(compressed_len < len);
    for i in 0..len {
      let b = (input[i] as i32) + 1;
      tmp[i] = ((b & 0x1F) | ((b & 0x40) >> 1)) as u8;
    }

    // 3. Pack exception bits into tmp[0..compressed_len]
    let saved = len >> 2;
    {
      let (packed, tail) = tmp[..len].split_at_mut(compressed_len);
      let (a, rest) = packed.split_at_mut(saved);
      let (b, rest) = rest.split_at_mut(saved);
      let c = &mut rest[..saved];
      for (dest, &source) in a.iter_mut().zip(tail.iter()) {
        *dest |= (source & 0x30) << 2; // bits 4-5
      }
      for (dest, &source) in b.iter_mut().zip(tail.iter()) {
        *dest |= (source & 0x0C) << 4; // bits 2-3
      }
      for (dest, &source) in c.iter_mut().zip(tail.iter()) {
        *dest |= (source & 0x03) << 6; // bits 0-1
      }
    }
    let o = saved * 3;

    debug_assert!(o <= compressed_len);
    out.write_bytes_range(tmp, 0, compressed_len)?;

    // 4. Write exception deltas
    debug_assert!(num_exceptions <= i32::MAX as usize);
    out.write_vint(num_exceptions as i32)?;
    if num_exceptions > 0 {
      previous_exception_index = 0;
      let mut num_exceptions2 = 0;

      let (blocks, tail) = input.as_chunks::<16>();
      const ONE: u8x16 = u8x16::splat(1);
      const CLASS_MASK: u8x16 = u8x16::splat(0xA0);
      const COMPRESSIBLE: u8x16 = u8x16::splat(0x20);
      let block_masks = blocks.iter().map(|&block| {
        let bad = ((u8x16::new(block) + ONE) & CLASS_MASK).simd_ne(COMPRESSIBLE);
        if bad.any() { bad.to_bitmask() } else { 0 }
      });
      let tail_mask = tail.iter().enumerate().fold(0, |mask, (lane, &byte)| {
        mask | (u32::from(!Self::is_compressible(byte as i32)) << lane)
      });

      for (block, mut mask) in block_masks.chain(std::iter::once(tail_mask)).enumerate() {
        while mask != 0 {
          let i = block * 16 + mask.trailing_zeros() as usize;
          mask &= mask - 1;
          while i - previous_exception_index > 0xFF {
            // We record deltas between exceptions as bytes, so we need to create
            // "artificial" exceptions if the delta between two of them is greater
            // than the maximum unsigned byte value.
            out.write_byte(0xFF)?;
            previous_exception_index += 0xFF;
            out.write_byte(input[previous_exception_index])?;
            num_exceptions2 += 1;
          }

          out.write_byte((i - previous_exception_index) as u8)?;
          previous_exception_index = i;
          out.write_byte(input[i])?;
          num_exceptions2 += 1;
        }
      }

      if num_exceptions != num_exceptions2 {
        return Err(LuceneError::illegal_state(format!(
          "{} <> {} {}",
          num_exceptions,
          num_exceptions2,
          BytesRef {
            bytes: input,
            offset: 0,
            length: len,
          },
        )));
      }
    }

    Ok(true)
  }
  /// Decompresses data that was previously compressed using
  /// [`Self::compress`].
  ///
  /// `len` must be the original (uncompressed) length, not the compressed
  /// length.
  pub fn decompress<DI>(input: &mut DI, out: &mut [u8], len: usize) -> Result<()>
  where
    DI: DataInput,
  {
    let saved = len >> 2;
    let compressed_len = len - saved;

    // 1. Copy the packed bytes
    debug_assert!(compressed_len <= i32::MAX as usize);
    input.read_bytes(out, 0, compressed_len)?;

    // 2. Restore the leading 2 bits into whole bytes
    if len <= out.len() {
      // Only the complete success range is split; short targets retain
      // the original indexed order and partial state below.
      let (packed, tail) = out.split_at_mut(compressed_len);
      let (a, rest) = packed.split_at(saved);
      let (b, rest) = rest.split_at(saved);
      let c = &rest[..saved];
      let full = (saved / 16) * 16;
      let mask = u8x16::splat(0xC0);
      for (((dest, a), b), c) in tail[..full]
        .as_chunks_mut::<16>()
        .0
        .iter_mut()
        .zip(a[..full].as_chunks::<16>().0)
        .zip(b[..full].as_chunks::<16>().0)
        .zip(c[..full].as_chunks::<16>().0)
      {
        let restored: u8x16 = ((u8x16::from(a.as_slice()) & mask) >> 2_u32)
          | ((u8x16::from(b.as_slice()) & mask) >> 4_u32)
          | ((u8x16::from(c.as_slice()) & mask) >> 6_u32);
        dest.copy_from_slice(&restored.to_array());
      }
      for (((dest, &a), &b), &c) in tail[full..saved]
        .iter_mut()
        .zip(&a[full..])
        .zip(&b[full..])
        .zip(&c[full..])
      {
        *dest = ((a & 0xC0) >> 2) | ((b & 0xC0) >> 4) | ((c & 0xC0) >> 6);
      }
    } else {
      for i in 0..saved {
        let a = (out[i] & 0xC0) >> 2;
        let b = (out[saved + i] & 0xC0) >> 4;
        let c = (out[(saved << 1) + i] & 0xC0) >> 6;
        out[compressed_len + i] = a | b | c;
      }
    }

    // 3. Move back to original range
    for b in out.iter_mut().take(len) {
      *b = ((*b & 0x1F) | 0x20 | ((*b & 0x20) << 1)) - 1;
    }

    // 4. Restore exceptions
    let num_exceptions = input.read_vint()?;
    let mut i = 0usize;

    for _ in 0..num_exceptions {
      i += input.read_byte()? as usize;
      out[i] = input.read_byte()?;
    }

    Ok(())
  }
}
