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
use std::ptr;

pub struct BitUtil {}
impl BitUtil {
  pub const BYTE_BYTES: usize = size_of::<i8>();
  pub const SHORT_BYTES: usize = size_of::<i16>();
  pub const INT_BYTES: usize = size_of::<i32>();
  pub const LONG_BYTES: usize = size_of::<i64>();
  pub const FLOAT_BYTES: usize = size_of::<f32>();
  pub const DOUBLE_BYTES: usize = size_of::<f64>();
  pub const USIZE_BYTES: usize = size_of::<usize>();
  /// The smallest positive subnormal f32 value.
  pub const F32_MIN_VALUE: f32 = f32::from_bits(1);
  /// The smallest positive subnormal f64 value.
  pub const F64_MIN_VALUE: f64 = f64::from_bits(1);
  pub const FLOAT_NAN_BITS: u32 = 0x7fc00000;
  pub const DOUBLE_NAN_BITS: u64 = 0x7ff8000000000000;

  /// Returns f32 bits with all NaN representations canonicalized.
  #[inline]
  pub fn float_to_int_bits(value: f32) -> i32 {
    if value.is_nan() {
      Self::FLOAT_NAN_BITS as i32
    } else {
      value.to_bits() as i32
    }
  }

  /// Returns f64 bits with all NaN representations canonicalized.
  #[inline]
  pub fn double_to_long_bits(value: f64) -> i64 {
    if value.is_nan() {
      Self::DOUBLE_NAN_BITS as i64
    } else {
      value.to_bits() as i64
    }
  }

  // i16 big_endian
  pub fn get_i16_be(bytes: &[u8], pos: usize) -> i16 {
    debug_assert!(
      pos + Self::SHORT_BYTES <= bytes.len(),
      "Index out of bounds"
    );

    unsafe {
      let raw_value = ptr::read_unaligned(bytes.as_ptr().add(pos) as *const i16);
      i16::from_be(raw_value)
    }
  }

  #[cfg(target_endian = "little")]
  pub fn set_i16_be(bytes: &mut [u8], pos: usize, value: i16) {
    Self::set_i16_be_with_len(bytes, pos, value, Self::SHORT_BYTES);
  }

  #[cfg(target_endian = "little")]
  pub fn set_i16_be_with_len(bytes: &mut [u8], pos: usize, value: i16, len: usize) {
    debug_assert!(
      pos + len <= bytes.len(),
      "Index out of bounds: pos={} len={} bytes.len()={}",
      pos,
      len,
      bytes.len()
    );
    debug_assert!(
      (0..=Self::SHORT_BYTES).contains(&len),
      "Invalid length: len={len} (must be <= 2)"
    );

    let value_be = value.to_be();

    unsafe {
      let value_ptr = &value_be as *const i16 as *const u8;
      let dest_ptr = bytes.as_mut_ptr().add(pos);
      ptr::copy_nonoverlapping(value_ptr, dest_ptr, len);
    }
  }
  // i16 little_endian
  #[cfg(target_endian = "little")]
  pub fn get_i16_le(bytes: &[u8], pos: usize) -> i16 {
    let mut value = [0; Self::SHORT_BYTES];
    value.copy_from_slice(&bytes[pos..][..Self::SHORT_BYTES]);
    i16::from_le_bytes(value)
  }
  #[cfg(target_endian = "little")]
  pub fn set_i16_le(bytes: &mut [u8], pos: usize, value: i16) {
    // Call the more flexible implementation with len = 2 (write all bytes)
    Self::set_i16_le_with_len(bytes, pos, value, Self::SHORT_BYTES);
  }

  #[cfg(target_endian = "little")]
  pub fn set_i16_le_with_len(bytes: &mut [u8], pos: usize, value: i16, len: usize) {
    debug_assert!(
      pos + len <= bytes.len(),
      "Index out of bounds: pos={} len={} bytes.len()={}",
      pos,
      len,
      bytes.len()
    );
    debug_assert!(
      (0..=Self::SHORT_BYTES).contains(&len),
      "Invalid length: len={} (must be <= {})",
      len,
      Self::SHORT_BYTES
    );

    let value_le = value.to_le();

    unsafe {
      let value_ptr = &value_le as *const i16 as *const u8;
      let dest_ptr = bytes.as_mut_ptr().add(pos);
      ptr::copy_nonoverlapping(value_ptr, dest_ptr, len);
    }
  }

  // i32 big_endian
  pub fn get_i32_be(bytes: &[u8], pos: usize) -> i32 {
    assert!(
      bytes.len() >= Self::INT_BYTES && pos <= bytes.len() - Self::INT_BYTES,
      "Index out of bounds"
    );
    let mut value = [0; Self::INT_BYTES];
    value.copy_from_slice(&bytes[pos..][..Self::INT_BYTES]);
    i32::from_be_bytes(value)
  }
  #[cfg(target_endian = "little")]
  pub fn set_i32_be(bytes: &mut [u8], pos: usize, value: i32) {
    Self::set_i32_be_with_len(bytes, pos, value, Self::INT_BYTES);
  }

  #[cfg(target_endian = "little")]
  pub fn set_i32_be_with_len(bytes: &mut [u8], pos: usize, value: i32, len: usize) {
    debug_assert!(
      pos + len <= bytes.len(),
      "Index out of bounds: pos={} len={} bytes.len()={}",
      pos,
      len,
      bytes.len()
    );
    debug_assert!(
      (0..=Self::INT_BYTES).contains(&len),
      "Invalid length: len={len} (must be <= 4)"
    );

    let value_be = value.to_be();

    unsafe {
      let value_ptr = &value_be as *const i32 as *const u8;
      let dest_ptr = bytes.as_mut_ptr().add(pos);
      ptr::copy_nonoverlapping(value_ptr, dest_ptr, len);
    }
  }
  // i32 little_endian
  #[cfg(target_endian = "little")]
  #[inline]
  pub fn get_i32_le(bytes: &[u8], pos: usize) -> i32 {
    assert!(
      bytes.len() >= Self::INT_BYTES && pos <= bytes.len() - Self::INT_BYTES,
      "Index out of bounds"
    );
    let mut value = [0; Self::INT_BYTES];
    value.copy_from_slice(&bytes[pos..][..Self::INT_BYTES]);
    i32::from_le_bytes(value)
  }

  #[cfg(target_endian = "little")]
  pub fn set_i32_le(bytes: &mut [u8], pos: usize, value: i32) {
    Self::set_i32_le_with_len(bytes, pos, value, Self::INT_BYTES);
  }
  #[cfg(target_endian = "little")]
  pub fn set_i32_le_with_len(bytes: &mut [u8], pos: usize, value: i32, len: usize) {
    debug_assert!(
      pos + len <= bytes.len(),
      "Index out of bounds: pos={} len={} bytes.len()={}",
      pos,
      len,
      bytes.len()
    );
    debug_assert!(
      (0..=Self::INT_BYTES).contains(&len),
      "Invalid length: len={len} (must be <= 4)"
    );

    let value_le = value.to_le();

    unsafe {
      let value_ptr = &value_le as *const i32 as *const u8;
      let dest_ptr = bytes.as_mut_ptr().add(pos);
      ptr::copy_nonoverlapping(value_ptr, dest_ptr, len);
    }
  }

  // i64 big_endian
  pub fn get_i64_be(bytes: &[u8], pos: usize) -> i64 {
    debug_assert!(pos + Self::LONG_BYTES <= bytes.len(), "Index out of bounds");

    unsafe {
      let raw_value = ptr::read_unaligned(bytes.as_ptr().add(pos) as *const i64);
      i64::from_be(raw_value)
    }
  }

  pub fn set_i64_be(bytes: &mut [u8], pos: usize, value: i64) {
    Self::set_i64_be_with_len(bytes, pos, value, Self::LONG_BYTES);
  }

  pub fn set_i64_be_with_len(bytes: &mut [u8], pos: usize, value: i64, len: usize) {
    debug_assert!(
      pos + len <= bytes.len(),
      "Index out of bounds: pos={} len={} bytes.len()={}",
      pos,
      len,
      bytes.len()
    );
    debug_assert!(
      (0..=Self::LONG_BYTES).contains(&len),
      "Invalid length: len={} (must be <= {})",
      len,
      Self::LONG_BYTES
    );

    let value_be = value.to_be();

    unsafe {
      let value_ptr = &value_be as *const i64 as *const u8;
      let dest_ptr = bytes.as_mut_ptr().add(pos);
      ptr::copy_nonoverlapping(value_ptr, dest_ptr, len);
    }
  }
  // i64 little_endian
  #[cfg(target_endian = "little")]
  pub fn get_i64_le(bytes: &[u8], pos: usize) -> i64 {
    let mut value = [0; Self::LONG_BYTES];
    value.copy_from_slice(&bytes[pos..][..Self::LONG_BYTES]);
    i64::from_le_bytes(value)
  }

  #[cfg(target_endian = "little")]
  pub fn set_i64_le(bytes: &mut [u8], pos: usize, value: i64) {
    // Call the more flexible implementation with len = 8 (write all bytes)
    Self::set_i64_le_with_len(bytes, pos, value, Self::LONG_BYTES);
  }

  #[cfg(target_endian = "little")]
  pub fn set_i64_le_with_len(bytes: &mut [u8], pos: usize, value: i64, len: usize) {
    debug_assert!(
      pos + len <= bytes.len(),
      "Index out of bounds: pos={} len={} bytes.len()={}",
      pos,
      len,
      bytes.len()
    );
    debug_assert!(
      (0..=Self::LONG_BYTES).contains(&len),
      "Invalid length: len={} (must be <= {})",
      len,
      Self::LONG_BYTES
    );

    let value_le = value.to_le();

    unsafe {
      let value_ptr = &value_le as *const i64 as *const u8;
      let dest_ptr = bytes.as_mut_ptr().add(pos);
      ptr::copy_nonoverlapping(value_ptr, dest_ptr, len);
    }
  }

  /// Returns the next highest power of two, or the current value if it's
  /// already a power of two or zero.
  pub fn next_highest_power_of_two_with_i32(mut v: i32) -> i32 {
    v -= 1;
    v |= v >> 1;
    v |= v >> 2;
    v |= v >> 4;
    v |= v >> 8;
    v |= v >> 16;
    v + 1
  }
  /// Returns the next highest power of two, or the current value if it's
  /// already a power of two or zero.
  pub fn next_highest_power_of_two_with_i64(mut v: i64) -> i64 {
    v -= 1;
    v |= v >> 1;
    v |= v >> 2;
    v |= v >> 4;
    v |= v >> 8;
    v |= v >> 16;
    v |= v >> 32;
    v + 1
  }
  const MAGIC0: u64 = 0x5555_5555_5555_5555;
  const MAGIC1: u64 = 0x3333_3333_3333_3333;
  const MAGIC2: u64 = 0x0F0F_0F0F_0F0F_0F0F;
  const MAGIC3: u64 = 0x00FF_00FF_00FF_00FF;
  const MAGIC4: u64 = 0x0000_FFFF_0000_FFFF;
  const MAGIC5: u64 = 0x0000_0000_FFFF_FFFF;
  const MAGIC6: u64 = 0xAAAA_AAAA_AAAA_AAAA;

  const SHIFT0: u32 = 1;
  const SHIFT1: u32 = 2;
  const SHIFT2: u32 = 4;
  const SHIFT3: u32 = 8;
  const SHIFT4: u32 = 16;

  /// Interleaves the low 32 bits of each `i32` value.
  ///
  /// Adapted from: <http://graphics.stanford.edu/~seander/bithacks.html#InterleaveBMN>
  pub fn interleave(even: i32, odd: i32) -> i64 {
    let mut v1 = Self::MAGIC5 & (even as u32 as u64);
    let mut v2 = Self::MAGIC5 & (odd as u32 as u64);

    v1 = (v1 | (v1 << Self::SHIFT4)) & Self::MAGIC4;
    v1 = (v1 | (v1 << Self::SHIFT3)) & Self::MAGIC3;
    v1 = (v1 | (v1 << Self::SHIFT2)) & Self::MAGIC2;
    v1 = (v1 | (v1 << Self::SHIFT1)) & Self::MAGIC1;
    v1 = (v1 | (v1 << Self::SHIFT0)) & Self::MAGIC0;

    v2 = (v2 | (v2 << Self::SHIFT4)) & Self::MAGIC4;
    v2 = (v2 | (v2 << Self::SHIFT3)) & Self::MAGIC3;
    v2 = (v2 | (v2 << Self::SHIFT2)) & Self::MAGIC2;
    v2 = (v2 | (v2 << Self::SHIFT1)) & Self::MAGIC1;
    v2 = (v2 | (v2 << Self::SHIFT0)) & Self::MAGIC0;

    ((v2 << 1) | v1) as i64
  }

  /// Swaps odd and even bits.
  pub fn flip_flop(b: i64) -> i64 {
    let b = b as u64;
    (((b & Self::MAGIC6) >> 1) | ((b & Self::MAGIC0) << 1)) as i64
  }

  pub fn zig_zag_decode_i32(i: u32) -> i32 {
    ((i >> 1) as i32) ^ -((i & 1) as i32)
  }

  pub fn zig_zag_encode_i32(i: i32) -> i32 {
    (i >> 31) ^ (i << 1)
  }

  pub fn zig_zag_decode_i64(l: u64) -> i64 {
    ((l >> 1) as i64) ^ -((l & 1) as i64)
  }

  pub fn zig_zag_encode_i64(l: i64) -> i64 {
    (l >> 63) ^ (l << 1)
  }
  #[cfg(not(target_endian = "little"))]
  compile_error!("This code can only be compiled on little-endian systems.");

  /// Return true if, and only if, the provided integer - treated as an unsigned integer - is either
  /// 0 or a power of two.
  pub fn is_zero_or_power_of_two(x: i32) -> bool {
    (x & x.wrapping_sub(1)) == 0
  }
}
