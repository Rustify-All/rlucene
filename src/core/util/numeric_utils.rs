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
use num_bigint::{BigInt, Sign};

use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};

pub struct NumericUtils;

impl NumericUtils {
  /// Converts an `f64` value to a sortable signed `i64`.
  ///
  /// The value is converted by obtaining its IEEE 754 `f64` bit layout and
  /// then swapping certain bits to allow the
  /// result to be compared as an `i64`. This transformation preserves
  /// precision while making the value sortable as a signed integer.
  ///
  /// The sort order (including [`f64::NAN`]) is defined by
  /// [`f64::total_cmp`]. `NaN` is greater than positive infinity.
  ///
  /// # WARN
  /// This implementation normalizes all `NaN` values to a canonical
  /// representation (`0x7ff8000000000000`) to ensure consistent sorting
  /// and behavior.
  /// Non-standard `NaN` representations are not preserved.
  ///
  /// # See Also
  /// [`sortable_long_to_double`](NumericUtils::sortable_long_to_double)
  pub fn double_to_sortable_long(value: f64) -> i64 {
    let bits = if value.is_nan() {
      // Normalize NaN to a canonical representation
      f64::from_bits(BitUtil::DOUBLE_NAN_BITS).to_bits()
    } else {
      value.to_bits()
    };
    Self::sortable_double_bits(bits as i64)
  }
  /// Converts a sortable `i64` back to an `f64`.
  ///
  /// # See Also
  /// [`double_to_sortable_long`](NumericUtils::double_to_sortable_long)
  pub fn sortable_long_to_double(encoded: i64) -> f64 {
    f64::from_bits(Self::sortable_double_bits(encoded) as u64)
  }
  /// Converts an `f32` value to a sortable signed `i32`.
  ///
  /// The value is converted by obtaining its IEEE 754 `f32` bit layout and
  /// then swapping certain bits to allow the
  /// result to be compared as an `i32`. This transformation preserves
  /// precision while making the value sortable as a signed integer.
  ///
  /// The sort order (including [`f32::NAN`]) is defined by
  /// [`f32::total_cmp`].
  ///
  /// # See Also
  /// [`sortable_int_to_float`](NumericUtils::sortable_int_to_float)
  pub fn float_to_sortable_int(value: f32) -> i32 {
    let bits = if value.is_nan() {
      // Normalize NaN to a canonical representation
      f32::from_bits(BitUtil::FLOAT_NAN_BITS).to_bits()
    } else {
      value.to_bits()
    };
    Self::sortable_float_bits(bits as i32)
  }
  /// Converts a sortable `i32` back to an `f32`.
  ///
  /// # See Also
  /// [`float_to_sortable_int`](NumericUtils::float_to_sortable_int)
  pub fn sortable_int_to_float(encoded: i32) -> f32 {
    f32::from_bits(Self::sortable_float_bits(encoded) as u32)
  }

  /// Converts the IEEE 754 representation of an `f64` to sortable order (or
  /// back to the original).
  pub fn sortable_double_bits(bits: i64) -> i64 {
    bits ^ ((bits >> 63) & 0x7fff_ffff_ffff_ffff)
  }
  /// Converts the IEEE 754 representation of an `f32` to sortable order (or
  /// back to the original).
  pub fn sortable_float_bits(bits: i32) -> i32 {
    bits ^ ((bits >> 31) & 0x7fff_ffff)
  }

  /// Result = a - b, where a >= b, else [`LuceneError`] is returned.
  pub fn subtract(
    bytes_per_dim: usize,
    dim: usize,
    a: &[u8],
    b: &[u8],
    result: &mut [u8],
  ) -> Result<()> {
    let start = dim * bytes_per_dim;
    let end = start + bytes_per_dim;
    let mut borrow = 0;

    for i in (start..end).rev() {
      let a_val = a[i] as i32 & 0xff;
      let b_val = b[i] as i32 & 0xff;
      let diff = a_val - b_val - borrow;

      if diff < 0 {
        borrow = 1;
        result[i - start] = (diff + 256) as u8;
      } else {
        borrow = 0;
        result[i - start] = diff as u8;
      }
    }
    if borrow != 0 {
      return Err(LuceneError::illegal_argument("a < b"));
    }
    Ok(())
  }
  /// Result = a + b, where a and b are unsigned. If there is an overflow,
  /// [`LuceneError`] is returned.
  pub fn add(
    bytes_per_dim: usize,
    dim: usize,
    a: &[u8],
    b: &[u8],
    result: &mut [u8],
  ) -> Result<()> {
    let start = dim * bytes_per_dim;
    let end = start + bytes_per_dim;
    let mut carry = 0;

    for i in (start..end).rev() {
      let a_val = a[i] as i32 & 0xff;
      let b_val = b[i] as i32 & 0xff;
      let digit_sum = a_val + b_val + carry;
      if digit_sum > 255 {
        carry = 1;
        result[i - start] = (digit_sum - 256) as u8;
      } else {
        carry = 0;
        result[i - start] = digit_sum as u8;
      }
    }

    if carry != 0 {
      return Err(LuceneError::illegal_argument(format!(
        "a + b overflows bytesPerDim={bytes_per_dim}"
      )));
    }

    Ok(())
  }

  /// Encodes an `i32` value into a sortable byte array representation.
  ///
  /// The resulting byte array can be compared lexicographically to achieve
  /// the same order as the original `i32` values.
  /// # See Also
  /// - [`sortable_bytes_to_int`](NumericUtils::sortable_bytes_to_int)
  pub fn int_to_sortable_bytes(mut value: i32, result: &mut [u8], offset: usize) {
    debug_assert!(
      offset + BitUtil::INT_BYTES <= result.len(),
      "Index out of bounds: offset={} result.len()={}",
      offset,
      result.len()
    );
    // Flip the sign bit to ensure correct sortable order
    value ^= i32::MIN;
    BitUtil::set_i32_be(result, offset, value);
  }
  /// Decodes an `i32` value previously written with `int_to_sortable_bytes`.
  ///
  /// # See Also
  /// - [`int_to_sortable_bytes`](NumericUtils::int_to_sortable_bytes)
  pub fn sortable_bytes_to_int(encoded: &[u8], offset: usize) -> i32 {
    debug_assert!(
      offset + BitUtil::INT_BYTES <= encoded.len(),
      "Index out of bounds: offset={} encoded.len()={}",
      offset,
      encoded.len()
    );

    // Read the value as big-endian
    let x = BitUtil::get_i32_be(encoded, offset);
    x ^ i32::MIN
  }
  /// Encodes an `i64` value into a sortable byte array representation.
  ///
  /// The resulting byte array can be compared lexicographically to achieve
  /// the same order as the original `i64` values.
  ///
  /// # See Also
  /// - [`sortable_bytes_to_long`](NumericUtils::sortable_bytes_to_long)
  pub fn long_to_sortable_bytes(mut value: i64, result: &mut [u8], offset: usize) {
    debug_assert!(
      offset + BitUtil::LONG_BYTES <= result.len(),
      "Index out of bounds: offset={} result.len()={}",
      offset,
      result.len()
    );
    // Flip the sign bit to ensure correct sortable order
    value ^= i64::MIN;
    BitUtil::set_i64_be(result, offset, value);
  }
  /// Decodes an `i64` value previously written with `long_to_sortable_bytes`.
  ///
  /// # See Also
  /// - [`long_to_sortable_bytes`](NumericUtils::long_to_sortable_bytes)
  pub fn sortable_bytes_to_long(encoded: &[u8], offset: usize) -> i64 {
    debug_assert!(
      offset + BitUtil::LONG_BYTES <= encoded.len(),
      "Index out of bounds: offset={} encoded.len()={}",
      offset,
      encoded.len()
    );
    let mut v = BitUtil::get_i64_be(encoded, offset);
    // Flip the sign bit back
    v ^= i64::MIN;
    v
  }
  /// Encodes a `BigInt` value such that unsigned byte order comparison is
  /// consistent with the natural order of `BigInt`. This also
  /// sign-extends the value to `big_int_size` bytes if necessary,
  /// ensuring a fixed-width size.
  ///
  /// # See Also
  /// - [`sortable_bytes_to_big_int`](NumericUtils::sortable_bytes_to_big_int)
  pub fn big_int_to_sortable_bytes(
    big_int: &BigInt,
    big_int_size: usize,
    result: &mut [u8],
    offset: usize,
  ) -> Result<()> {
    let big_int_bytes = big_int.to_signed_bytes_be();
    if big_int_size < big_int_bytes.len() {
      return Err(LuceneError::illegal_argument(format!(
        "BigInt {big_int} requires more than {big_int_size} bytes of storage"
      )));
    }
    if offset + big_int_size > result.len() {
      return Err(LuceneError::illegal_argument(
        "Index out of bounds in result array",
      ));
    }
    let full_big_int_bytes = &mut result[offset..offset + big_int_size];
    let padding_size = big_int_size - big_int_bytes.len();
    full_big_int_bytes[..padding_size].fill(if big_int.sign() == Sign::Minus {
      0xFF
    } else {
      0
    });
    let copy_source = &big_int_bytes;
    full_big_int_bytes[padding_size..][..copy_source.len()].copy_from_slice(copy_source);
    full_big_int_bytes[0] ^= 0x80;

    #[cfg(debug_assertions)]
    {
      let converted = Self::sortable_bytes_to_big_int(result, offset, big_int_size)?;
      debug_assert_eq!(
        &converted, big_int,
        "BigInt={} converted={}",
        big_int, converted
      );
    }

    Ok(())
  }
  /// Decodes a `BigInt` value previously written with
  /// `big_int_to_sortable_bytes`.
  ///
  /// # See Also
  /// - [`big_int_to_sortable_bytes`](NumericUtils::big_int_to_sortable_bytes)
  pub fn sortable_bytes_to_big_int(encoded: &[u8], offset: usize, length: usize) -> Result<BigInt> {
    if offset + length > encoded.len() {
      return Err(LuceneError::illegal_argument(
        "Index out of bounds in encoded array",
      ));
    }
    stack_or_heap_buffer!(big_int_bytes, u8, length, 64, 0);
    big_int_bytes.copy_from_slice(&encoded[offset..offset + length]);
    // Flip the sign bit back to restore the original value
    big_int_bytes[0] ^= 0x80;

    // Convert the byte array back into a BigInt
    Ok(BigInt::from_signed_bytes_be(big_int_bytes))
  }
}
