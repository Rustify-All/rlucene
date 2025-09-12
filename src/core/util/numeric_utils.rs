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

use crate::core::util::SliceCopyOps;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};

pub struct NumericUtils;

impl NumericUtils {
    /// Converts an `f64` value to a sortable signed `i64`.
    ///
    /// The value is converted by obtaining its IEEE 754 floating-point "double
    /// format" bit layout and then swapping certain bits to allow the
    /// result to be compared as an `i64`. This transformation preserves
    /// precision while making the value sortable as a signed integer.
    ///
    /// The sort order (including [`f64::NAN`]) is defined by
    /// [`f64::total_cmp`]. `NaN` is greater than positive infinity.
    ///
    /// # WARN
    /// This implementation normalizes all `NaN` values to a canonical
    /// representation (`0x7ff8000000000000`) to ensure consistent sorting
    /// and behavior, similar to Java's `Double.doubleToLongBits`.
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
    /// The value is converted by obtaining its IEEE 754 floating-point "float
    /// format" bit layout and then swapping certain bits to allow the
    /// result to be compared as an `i32`. This transformation preserves
    /// precision while making the value sortable as a signed integer.
    ///
    /// The sort order (including [`f32::NAN`]) is defined by
    /// [`f32::total_cmp`].
    ///
    /// # WARN
    /// This implementation normalizes all `NaN` values to a canonical
    /// representation (`0x7fc00000`) to ensure consistent sorting and
    /// behavior. similar to Java's `Float.floatToIntBits`. Non-standard
    /// `NaN` representations are not preserved.
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

    /// Result = a - b, where a >= b, else `LuceneError` is returned.
    pub fn subtract(
        bytes_per_dim: i32,
        dim: i32,
        a: &[u8],
        b: &[u8],
        result: &mut [u8],
    ) -> Result<()> {
        let start = (dim * bytes_per_dim) as usize;
        let end = start + bytes_per_dim as usize;
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
    /// `LuceneError` is returned.
    pub fn add(bytes_per_dim: u32, dim: u32, a: &[u8], b: &[u8], result: &mut [u8]) -> Result<()> {
        let start = (dim * bytes_per_dim) as usize;
        let end = start + bytes_per_dim as usize;
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
        let mut full_big_int_bytes = vec![0u8; big_int_size];
        let padding_size = big_int_size - big_int_bytes.len();
        full_big_int_bytes.copy_from(&big_int_bytes, padding_size);
        if big_int.sign() == Sign::Minus {
            full_big_int_bytes[..padding_size].fill(0xFF);
        }
        full_big_int_bytes[0] ^= 0x80;
        if offset + big_int_size > result.len() {
            return Err(LuceneError::illegal_argument(
                "Index out of bounds in result array",
            ));
        }
        result.copy_from(&full_big_int_bytes, offset);

        debug_assert!(
            {
                let converted = Self::sortable_bytes_to_big_int(result, offset, big_int_size)
                    .expect("Error decoding BigInt");
                converted == *big_int
            },
            "BigInt={} converted={}",
            big_int,
            Self::sortable_bytes_to_big_int(result, offset, big_int_size)
                .expect("Error decoding BigInt")
        );

        Ok(())
    }
    /// Decodes a `BigInt` value previously written with
    /// `big_int_to_sortable_bytes`.
    ///
    /// # See Also
    /// - [`big_int_to_sortable_bytes`](NumericUtils::big_int_to_sortable_bytes)
    pub fn sortable_bytes_to_big_int(
        encoded: &[u8],
        offset: usize,
        length: usize,
    ) -> Result<BigInt> {
        if offset + length > encoded.len() {
            return Err(LuceneError::illegal_argument(
                "Index out of bounds in encoded array",
            ));
        }
        let mut big_int_bytes = encoded[offset..offset + length].to_vec();
        // Flip the sign bit back to restore the original value
        big_int_bytes[0] ^= 0x80;

        // Convert the byte array back into a BigInt
        Ok(BigInt::from_signed_bytes_be(&big_int_bytes))
    }
}
#[cfg(test)]
mod tests {
    use std::cmp::Ordering;
    use std::ops::{Add, Sub};

    use num_bigint::{BigInt, Sign};
    use num_traits::{Float, FromPrimitive};
    use rand::Rng;

    use crate::core::index::BytesRef;
    use crate::core::util::SliceCopyOps;
    use crate::core::util::bit_util::BitUtil;
    use crate::core::util::error::lucene_error::Result;
    use crate::core::util::numeric_utils::NumericUtils;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{at_least, random};
    use crate::test::util::test_util::TestUtil;

    #[allow(dead_code)] // for quick search
    pub struct TestNumericUtils;
    /// generate a series of encoded longs, each numerical one bigger than the
    /// one before. check for correct ordering of the encoded bytes and that
    /// values round-trip.
    #[test]
    fn test_long_conversion_and_ordering() -> Result<()> {
        let mut previous: Option<BytesRef<Vec<u8>>> = None;
        let mut current = BytesRef::from_bytes(vec![0u8; BitUtil::LONG_BYTES]);
        for value in -100_000..100_000 {
            NumericUtils::long_to_sortable_bytes(
                value,
                current.bytes.as_mut_slice(),
                current.offset,
            );
            if let Some(ref prev) = previous {
                assert!(
                    *prev < current,
                    "Current value's encoded bytes are not larger than previous"
                );
            } else {
                previous = Some(BytesRef::from_bytes(vec![0u8; BitUtil::LONG_BYTES]));
            }
            let decoded_value =
                NumericUtils::sortable_bytes_to_long(current.bytes.as_slice(), current.offset);
            assert_eq!(
                decoded_value, value,
                "Forward and backward conversion failed for value"
            );
        }
        Ok(())
    }
    /// generate a series of encoded ints, each numerical one bigger than the
    /// one before. check for correct ordering of the encoded bytes and that
    /// values round-trip.
    #[test]
    fn test_int_conversion_and_ordering() -> Result<()> {
        let mut previous: Option<BytesRef<Vec<u8>>> = None;
        let mut current = BytesRef::from_bytes(vec![0u8; BitUtil::INT_BYTES]);

        for value in -100_000..100_000 {
            NumericUtils::int_to_sortable_bytes(value, current.bytes.as_mut_slice(), 0);

            if let Some(ref prev) = previous {
                assert!(
                    *prev < current,
                    "Current value's encoded bytes are not larger than previous"
                );
            }

            let decoded_value =
                NumericUtils::sortable_bytes_to_int(current.bytes.as_slice(), current.offset);
            assert_eq!(
                decoded_value, value,
                "Forward and backward conversion failed for value: {}",
                value
            );
            previous = Some(BytesRef::from_bytes(current.bytes.clone()));
        }
        Ok(())
    }
    /// generate a series of encoded BigIntegers, each numerical one bigger than
    /// the one before. check for correct ordering of the encoded bytes and
    /// that values round-trip.
    #[test]
    fn test_big_int_conversion_and_ordering() -> Result<()> {
        let mut random = random();
        // Generate a random size between 3 and 16
        let size = TestUtil::next_int(&mut random, 3, 16) as usize;
        let mut previous: Option<BytesRef<Vec<u8>>> = None;
        let mut current = BytesRef::from_bytes(vec![0u8; size]);

        for value in -100_000..100_000 {
            let big_int = BigInt::from_i64(value).unwrap();
            NumericUtils::big_int_to_sortable_bytes(
                &big_int,
                size,
                current.bytes.as_mut_slice(),
                0,
            )?;
            if let Some(ref prev) = previous {
                assert!(
                    *prev < current,
                    "Current value's encoded bytes are not larger than previous"
                );
            }
            let decoded = NumericUtils::sortable_bytes_to_big_int(
                current.bytes.as_slice(),
                current.offset,
                size,
            )?;
            assert_eq!(
                decoded, big_int,
                "Forward and backward conversion failed for value: {}",
                big_int
            );
            previous = Some(BytesRef::from_bytes(current.bytes.clone()));
        }

        Ok(())
    }
    /// Checks extreme values of `i64` for correct ordering of the encoded bytes
    /// and ensures that the values can be correctly encoded and decoded
    /// (round-trip conversion).
    #[test]
    fn test_long_special_values() -> Result<()> {
        let values: Vec<i64> = vec![
            i64::MIN,
            i64::MIN + 1,
            i64::MIN + 2,
            -5003400000000,
            -4000,
            -3000,
            -2000,
            -1000,
            -1,
            0,
            1,
            10,
            300,
            50006789999999999,
            i64::MAX - 2,
            i64::MAX - 1,
            i64::MAX,
        ];

        let mut encoded: Vec<BytesRef<Vec<u8>>> = values
            .iter()
            .map(|_| BytesRef::from_bytes(vec![0u8; BitUtil::LONG_BYTES]))
            .collect();

        for (i, &value) in values.iter().enumerate() {
            let offset = encoded[i].offset;
            NumericUtils::long_to_sortable_bytes(value, encoded[i].bytes.as_mut_slice(), offset);

            // Check that the value can be decoded back correctly
            let decoded_value =
                NumericUtils::sortable_bytes_to_long(encoded[i].bytes.as_slice(), offset);
            assert_eq!(
                decoded_value, value,
                "Forward and backward conversion failed for value: {}",
                value
            );
        }

        for i in 1..encoded.len() {
            assert_eq!(
                encoded[i - 1].cmp(&encoded[i]),
                Ordering::Less,
                "Encoded values are not in ascending order: {:?} >= {:?}",
                encoded[i - 1],
                encoded[i]
            );
        }

        Ok(())
    }
    /// Checks extreme values of `i32` for correct ordering of the encoded bytes
    /// and ensures that the values can be correctly encoded and decoded
    /// (round-trip conversion).
    #[test]
    fn test_int_special_values() -> Result<()> {
        let values: Vec<i32> = vec![
            i32::MIN,
            i32::MIN + 1,
            i32::MIN + 2,
            -64765767,
            -4000,
            -3000,
            -2000,
            -1000,
            -1,
            0,
            1,
            10,
            300,
            765878989,
            i32::MAX - 2,
            i32::MAX - 1,
            i32::MAX,
        ];

        let mut encoded: Vec<BytesRef<Vec<u8>>> = values
            .iter()
            .map(|_| BytesRef::from_bytes(vec![0u8; BitUtil::INT_BYTES]))
            .collect();

        for (i, &value) in values.iter().enumerate() {
            let offset = encoded[i].offset;
            NumericUtils::int_to_sortable_bytes(value, encoded[i].bytes.as_mut_slice(), offset);
            let decoded_value =
                NumericUtils::sortable_bytes_to_int(encoded[i].bytes.as_slice(), offset);
            assert_eq!(
                decoded_value, value,
                "Forward and backward conversion failed for value: {}",
                value
            );
        }
        for i in 1..encoded.len() {
            assert_eq!(
                encoded[i - 1].cmp(&encoded[i]),
                Ordering::Less,
                "Encoded values are not in ascending order: {:?} >= {:?}",
                encoded[i - 1],
                encoded[i]
            );
        }

        Ok(())
    }
    /// Checks extreme values of `BigInt` (4 bytes) for correct ordering of the
    /// encoded bytes and ensures that the values can be correctly encoded
    /// and decoded (round-trip conversion).
    #[test]
    fn test_big_int_special_values() -> Result<()> {
        use num_bigint::BigInt;
        use num_traits::FromPrimitive;
        let values: Vec<BigInt> = vec![
            BigInt::from_i32(i32::MIN).unwrap(),
            BigInt::from_i32(i32::MIN + 1).unwrap(),
            BigInt::from_i32(i32::MIN + 2).unwrap(),
            BigInt::from_i32(-64765767).unwrap(),
            BigInt::from_i32(-4000).unwrap(),
            BigInt::from_i32(-3000).unwrap(),
            BigInt::from_i32(-2000).unwrap(),
            BigInt::from_i32(-1000).unwrap(),
            BigInt::from_i32(-1).unwrap(),
            BigInt::from_i32(0).unwrap(),
            BigInt::from_i32(1).unwrap(),
            BigInt::from_i32(10).unwrap(),
            BigInt::from_i32(300).unwrap(),
            BigInt::from_i32(765878989).unwrap(),
            BigInt::from_i32(i32::MAX - 2).unwrap(),
            BigInt::from_i32(i32::MAX - 1).unwrap(),
            BigInt::from_i32(i32::MAX).unwrap(),
        ];
        let mut encoded: Vec<BytesRef<Vec<u8>>> = values
            .iter()
            .map(|_| BytesRef::from_bytes(vec![0u8; BitUtil::INT_BYTES]))
            .collect();
        for (i, value) in values.iter().enumerate() {
            let offset = encoded[i].offset;
            NumericUtils::big_int_to_sortable_bytes(
                value,
                4, // Integer.BYTES = 4
                encoded[i].bytes.as_mut_slice(),
                offset,
            )?;
            let decoded_value = NumericUtils::sortable_bytes_to_big_int(
                encoded[i].bytes.as_slice(),
                offset,
                BitUtil::INT_BYTES,
            )?;
            assert_eq!(
                decoded_value, *value,
                "Forward and backward conversion failed for BigInt: {}",
                value
            );
        }
        for i in 1..encoded.len() {
            assert!(
                encoded[i - 1] < encoded[i],
                "Encoded values are not in ascending order: {:?} >= {:?}",
                encoded[i - 1],
                encoded[i]
            );
        }

        Ok(())
    }
    /// Checks various sorted values of `f64` (including extreme values) for
    /// correct ordering of the encoded bytes and ensures that the values
    /// can be correctly encoded and decoded (round-trip conversion).
    #[test]
    fn test_doubles() -> Result<()> {
        let values: Vec<f64> = vec![
            f64::NEG_INFINITY,
            -2.3E25,
            -1.0E15,
            -1.0,
            -1.0E-1,
            -1.0E-2,
            -0.0,
            0.0,
            1.0E-2,
            1.0E-1,
            1.0,
            1.0E15,
            2.3E25,
            f64::INFINITY,
            f64::NAN,
        ];

        let mut encoded: Vec<i64> = vec![0; values.len()];

        // Check forward and back conversion
        for (i, &value) in values.iter().enumerate() {
            encoded[i] = NumericUtils::double_to_sortable_long(value);

            let decoded = NumericUtils::sortable_long_to_double(encoded[i]);
            assert!(
                value == decoded || (value.is_nan() && decoded.is_nan()),
                "Forward and backward conversion failed for value: {}, decoded: {}",
                value,
                decoded
            );
        }

        // Check sort order (encoded values should be ascending)
        for i in 1..encoded.len() {
            assert!(
                encoded[i - 1] < encoded[i],
                "Encoded values are not in ascending order: {} >= {}",
                encoded[i - 1],
                encoded[i]
            );
        }

        Ok(())
    }
    /// Tests that various representations of `NaN` for `f64` are correctly
    /// encoded such that their sortable representation is greater than
    /// positive infinity.
    #[test]
    fn test_sortable_double_nan() -> Result<()> {
        let double_nans: Vec<f64> = vec![
            f64::NAN,
            // f64::from_bits(0x7ff0000000000001),
            f64::from_bits(0x7fffffffffffffff),
            f64::from_bits(0xfff0000000000001),
            f64::from_bits(0xffffffffffffffff),
        ];
        let plus_inf = NumericUtils::double_to_sortable_long(f64::INFINITY);
        for &nan in &double_nans {
            assert!(nan.is_nan(), "Value is not NaN: {}", nan);
            let sortable = NumericUtils::double_to_sortable_long(nan);
            assert!(
                sortable > plus_inf,
                "Double not sorted correctly: {}, long repr: {}, positive inf.: {}",
                nan,
                sortable,
                plus_inf
            );
        }
        Ok(())
    }
    /// Checks various sorted values of `f32` (including extreme values) for
    /// correct ordering of the encoded bytes and ensures that the values
    /// can be correctly encoded and decoded (round-trip conversion).
    #[test]
    fn test_floats() -> Result<()> {
        let values: Vec<f32> = vec![
            f32::NEG_INFINITY,
            -2.3E25_f32,
            -1.0E15_f32,
            -1.0_f32,
            -1.0E-1_f32,
            -1.0E-2_f32,
            -0.0_f32,
            0.0_f32,
            1.0E-2_f32,
            1.0E-1_f32,
            1.0_f32,
            1.0E15_f32,
            2.3E25_f32,
            f32::INFINITY,
            f32::NAN,
        ];

        let mut encoded: Vec<i32> = vec![0; values.len()];

        // Check forward and backward conversion
        for (i, &value) in values.iter().enumerate() {
            encoded[i] = NumericUtils::float_to_sortable_int(value);

            let decoded = NumericUtils::sortable_int_to_float(encoded[i]);
            assert!(
                value == decoded || (value.is_nan() && decoded.is_nan()),
                "Forward and backward conversion failed for value: {}, decoded: {}",
                value,
                decoded
            );
        }

        // Check sort order (encoded values should be ascending)
        for i in 1..encoded.len() {
            assert!(
                encoded[i - 1] < encoded[i],
                "Encoded values are not in ascending order: {} >= {}",
                encoded[i - 1],
                encoded[i]
            );
        }

        Ok(())
    }
    #[test]
    fn test_sortable_float_nan() -> Result<()> {
        let float_nans: Vec<f32> = vec![
            f32::NAN,
            f32::from_bits(0x7f800001),
            f32::from_bits(0x7fffffff),
            f32::from_bits(0xff800001),
            f32::from_bits(0xffffffff),
        ];

        let plus_inf = NumericUtils::float_to_sortable_int(f32::INFINITY);

        for &nan in &float_nans {
            assert!(nan.is_nan(), "Value is not NaN: {}", nan);

            let sortable = NumericUtils::float_to_sortable_int(nan);

            assert!(
                sortable > plus_inf,
                "Float not sorted correctly: {}, int repr: {}, positive inf.: {}",
                nan,
                sortable,
                plus_inf
            );
        }

        Ok(())
    }
    #[test]
    fn test_add() -> Result<()> {
        let mut random = random();
        let iters = at_least(&mut random, 1000);
        let num_bytes = TestUtil::next_int(&mut random, 1, 100) as usize;

        for _ in 0..iters {
            let v1 = BigInt::from(random.random::<i128>().abs() % (1 << (8 * num_bytes - 1)));
            let v2 = BigInt::from(random.random::<i128>().abs() % (1 << (8 * num_bytes - 1)));

            let mut v1_bytes = vec![0u8; num_bytes];
            let v1_raw_bytes = v1.to_signed_bytes_be();
            assert!(v1_raw_bytes.len() <= num_bytes);
            let start_pos = num_bytes.saturating_sub(v1_raw_bytes.len());
            v1_bytes.copy_from(&v1_raw_bytes, start_pos);

            let mut v2_bytes = vec![0u8; num_bytes];
            let v2_raw_bytes = v2.to_signed_bytes_be();
            assert!(v2_raw_bytes.len() <= num_bytes);
            let start_pos = num_bytes.saturating_sub(v2_raw_bytes.len());
            v2_bytes.copy_from(&v2_raw_bytes, start_pos);

            let mut result = vec![0u8; num_bytes];

            assert!(num_bytes <= u32::MAX as usize);
            NumericUtils::add(num_bytes as u32, 0, &v1_bytes, &v2_bytes, &mut result)?;

            let v1_clone = v1.clone();
            let v2_clone = v2.clone();
            let sum = v1.add(v2);

            let result_bigint = BigInt::from_bytes_be(Sign::Plus, &result);
            assert_eq!(
                result_bigint, sum,
                "sum={} v1={} v2={} but result={}",
                sum, v1_clone, v2_clone, result_bigint
            );
        }

        Ok(())
    }
    #[test]
    fn test_illegal_add() {
        let bytes = vec![0xFF; 4];
        let mut one = vec![0x00; 4];
        one[3] = 1;
        let result = NumericUtils::add(4, 0, &bytes, &one, &mut [0u8; 4]);
        assert!(
            result.is_err(),
            "Expected an overflow error, but the operation succeeded"
        );

        if let Err(err) = result {
            assert_eq!(
                err.to_string(),
                "a + b overflows bytesPerDim=4",
                "Unexpected error message: {}",
                err
            );
        }
    }
    #[test]
    fn test_subtract() -> Result<()> {
        let mut random = random();
        let iters = at_least(&mut random, 1000);
        let num_bytes = TestUtil::next_int(&mut random, 1, 100) as usize;

        for _ in 0..iters {
            let mut v1 = BigInt::from(random.random::<i128>().abs() % (1 << (8 * num_bytes - 1)));
            let mut v2 = BigInt::from(random.random::<i128>().abs() % (1 << (8 * num_bytes - 1)));

            if v1 < v2 {
                std::mem::swap(&mut v1, &mut v2);
            }

            let mut v1_bytes = vec![0u8; num_bytes];
            let v1_raw_bytes = v1.to_signed_bytes_be();
            let start_pos = num_bytes.saturating_sub(v1_raw_bytes.len());
            v1_bytes.copy_from(&v1_raw_bytes, start_pos);

            let mut v2_bytes = vec![0u8; num_bytes];
            let v2_raw_bytes = v2.to_signed_bytes_be();
            let start_pos = num_bytes.saturating_sub(v2_raw_bytes.len());
            v2_bytes.copy_from(&v2_raw_bytes, start_pos);

            let mut result = vec![0u8; num_bytes];

            NumericUtils::subtract(num_bytes as i32, 0, &v1_bytes, &v2_bytes, &mut result)?;

            let v1_clone = v1.clone();
            let v2_clone = v2.clone();
            let diff = v1.sub(v2);

            let result_bigint = BigInt::from_signed_bytes_be(&result);
            assert_eq!(
                result_bigint, diff,
                "diff={} result={} v1={} v2={}",
                diff, result_bigint, v1_clone, v2_clone
            );
        }

        Ok(())
    }
    #[test]
    fn test_illegal_subtract() {
        let mut v1 = vec![0x00; 4];
        v1[3] = 0xF0;
        let mut v2 = vec![0x00; 4];
        v2[3] = 0xF1; // Represents a larger value
        let result = NumericUtils::subtract(4, 0, &v1, &v2, &mut [0u8; 4]);
        assert!(
            result.is_err(),
            "Expected an error, but the operation succeeded"
        );
        if let Err(err) = result {
            assert_eq!(
                err.to_string(),
                "a < b",
                "Unexpected error message: {}",
                err
            );
        }
    }
    /// Tests round-trip encoding and decoding of random `i32` values.
    #[test]
    fn test_ints_round_trip() -> Result<()> {
        let mut random = random();
        let mut encoded = vec![0u8; BitUtil::INT_BYTES];
        for _ in 0..10_000 {
            let value = random.random::<i32>();
            NumericUtils::int_to_sortable_bytes(value, &mut encoded, 0);
            let decoded = NumericUtils::sortable_bytes_to_int(&encoded, 0);
            assert_eq!(
                decoded, value,
                "Round-trip encoding failed for value: {}, decoded: {}",
                value, decoded
            );
        }
        Ok(())
    }
    /// Tests round-trip encoding and decoding of random `i64` values.
    #[test]
    fn test_longs_round_trip() -> Result<()> {
        let mut random = random();
        let mut encoded = vec![0u8; BitUtil::LONG_BYTES];
        for _ in 0..10_000 {
            let value = TestUtil::next_long(&mut random, i64::MIN, i64::MAX);
            NumericUtils::long_to_sortable_bytes(value, &mut encoded, 0);
            let decoded = NumericUtils::sortable_bytes_to_long(&encoded, 0);
            assert_eq!(
                decoded, value,
                "Round-trip encoding failed for value: {}, decoded: {}",
                value, decoded
            );
        }

        Ok(())
    }
    /// Tests round-trip encoding and decoding of random `f32` values.
    #[test]
    fn test_floats_round_trip() -> Result<()> {
        let mut random = random();
        let mut encoded = vec![0u8; BitUtil::INT_BYTES];
        for _ in 0..10_000 {
            let value = f32::from_bits(random.random::<i32>() as u32);
            let sortable_int = NumericUtils::float_to_sortable_int(value);
            NumericUtils::int_to_sortable_bytes(sortable_int, &mut encoded, 0);
            let decoded_sortable_int = NumericUtils::sortable_bytes_to_int(&encoded, 0);
            let actual = NumericUtils::sortable_int_to_float(decoded_sortable_int);
            let expected_vale = if value.is_nan() {
                BitUtil::FLOAT_NAN_BITS
            } else {
                value.to_bits()
            };
            let actual_value = if actual.is_nan() {
                BitUtil::FLOAT_NAN_BITS
            } else {
                actual.to_bits()
            };
            assert_eq!(
                expected_vale, actual_value,
                "Round-trip encoding failed for value: {}, decoded: {}",
                value, actual
            );
        }

        Ok(())
    }
    /// Tests round-trip encoding and decoding of random `f64` values.
    #[test]
    fn test_doubles_round_trip() -> Result<()> {
        let mut random = random();
        let mut encoded = vec![0u8; BitUtil::LONG_BYTES];

        for _ in 0..10_000 {
            let value = f64::from_bits(TestUtil::next_long(&mut random, i64::MIN, i64::MAX) as u64);
            let sortable_long = NumericUtils::double_to_sortable_long(value);
            NumericUtils::long_to_sortable_bytes(sortable_long, &mut encoded, 0);
            let decoded_sortable_long = NumericUtils::sortable_bytes_to_long(&encoded, 0);
            let actual = NumericUtils::sortable_long_to_double(decoded_sortable_long);
            let expected_value = if value.is_nan() {
                BitUtil::DOUBLE_NAN_BITS
            } else {
                value.to_bits()
            };
            let actual_value = if actual.is_nan() {
                BitUtil::DOUBLE_NAN_BITS
            } else {
                actual.to_bits()
            };
            assert_eq!(
                expected_value, actual_value,
                "Round-trip encoding failed for value: {}, decoded: {}",
                value, actual
            );
        }

        Ok(())
    }
    /// Tests round-trip encoding and decoding of random `BigInt` values.
    #[test]
    fn test_big_ints_round_trip() -> Result<()> {
        let mut random = random();
        for _ in 0..10_000 {
            let value = TestUtil::next_big_integer(&mut random, 16);
            let length = value.to_signed_bytes_be().len();
            let max_length =
                TestUtil::next_int(&mut random, length as i32, length as i32 + 3) as usize;
            let mut encoded = vec![0u8; max_length];
            NumericUtils::big_int_to_sortable_bytes(&value, max_length, &mut encoded, 0)?;
            let decoded = NumericUtils::sortable_bytes_to_big_int(&encoded, 0, max_length)?;
            assert_eq!(
                decoded, value,
                "Round-trip encoding failed for value: {}, decoded: {}",
                value, decoded
            );
        }

        Ok(())
    }
    /// Checks that the sort order of encoded integers is consistent with
    /// `i32::cmp`.
    #[test]
    fn test_ints_compare() -> Result<()> {
        let mut random = random();
        let mut left = BytesRef::from_bytes(vec![0u8; BitUtil::INT_BYTES]);
        let mut right = BytesRef::from_bytes(vec![0u8; BitUtil::INT_BYTES]);

        for _ in 0..10_000 {
            let left_value = random.random::<i32>();
            let right_value = random.random::<i32>();
            NumericUtils::int_to_sortable_bytes(left_value, left.bytes.as_mut_slice(), left.offset);
            NumericUtils::int_to_sortable_bytes(
                right_value,
                right.bytes.as_mut_slice(),
                right.offset,
            );
            let expected_sign = left_value.cmp(&right_value) as i32;
            let actual_sign = left.cmp(&right) as i32;
            assert_eq!(
                expected_sign, actual_sign,
                "Mismatch between numerical and lexicographic comparison for left: {}, right: {}",
                left_value, right_value
            );
        }

        Ok(())
    }
    /// Checks that the sort order of encoded `i64` values is consistent with
    /// `i64::cmp`.
    #[test]
    fn test_longs_compare() -> Result<()> {
        let mut random = random();
        let mut left = BytesRef::from_bytes(vec![0u8; BitUtil::LONG_BYTES]);
        let mut right = BytesRef::from_bytes(vec![0u8; BitUtil::LONG_BYTES]);

        for _ in 0..10_000 {
            let left_value = TestUtil::next_long(&mut random, i64::MIN, i64::MAX);
            let right_value = TestUtil::next_long(&mut random, i64::MIN, i64::MAX);
            NumericUtils::long_to_sortable_bytes(
                left_value,
                left.bytes.as_mut_slice(),
                left.offset,
            );
            NumericUtils::long_to_sortable_bytes(
                right_value,
                right.bytes.as_mut_slice(),
                right.offset,
            );
            let expected_sign = left_value.cmp(&right_value) as i32;
            let actual_sign = left.cmp(&right) as i32;
            assert_eq!(
                expected_sign, actual_sign,
                "Mismatch between numerical and lexicographic comparison for left: {}, right: {}",
                left_value, right_value
            );
        }

        Ok(())
    }
    /// Checks that the sort order of encoded `f32` values is consistent with
    /// `f32::total_cmp`.
    ///
    /// This test ensures that when two random `f32` values are encoded using
    /// `NumericUtils::float_to_sortable_int`, the lexicographic comparison of
    /// their encoded byte representations is consistent with their
    /// numerical comparison.
    #[test]
    fn test_floats_compare() -> Result<()> {
        let mut random = random();
        let mut left = BytesRef::from_bytes(vec![0u8; BitUtil::FLOAT_BYTES]);
        let mut right = BytesRef::from_bytes(vec![0u8; BitUtil::FLOAT_BYTES]);
        for _ in 0..10_000 {
            let left_value = to_positive_nan::<f32>(f32::from_bits(random.random::<u32>()));
            let right_value = to_positive_nan::<f32>(f32::from_bits(random.random::<u32>()));
            NumericUtils::int_to_sortable_bytes(
                NumericUtils::float_to_sortable_int(left_value),
                left.bytes.as_mut_slice(),
                left.offset,
            );
            NumericUtils::int_to_sortable_bytes(
                NumericUtils::float_to_sortable_int(right_value),
                right.bytes.as_mut_slice(),
                right.offset,
            );
            let expected_order = left_value.total_cmp(&right_value);
            let actual_order = left.cmp(&right);
            assert_eq!(
                expected_order, actual_order,
                "Mismatch between numerical and lexicographic comparison for left: {}, right: {}",
                left_value, right_value
            );
        }

        Ok(())
    }
    /// Checks that the sort order of encoded `f64` values is consistent with
    /// `f64::total_cmp`.
    ///
    /// This test ensures that when two random `f64` values are encoded using
    /// `NumericUtils::double_to_sortable_long`, the lexicographic comparison of
    /// their encoded byte representations is consistent with their
    /// numerical comparison.
    #[test]
    fn test_doubles_compare() -> Result<()> {
        let mut random = random();
        let mut left = BytesRef::from_bytes(vec![0u8; BitUtil::DOUBLE_BYTES]);
        let mut right = BytesRef::from_bytes(vec![0u8; BitUtil::DOUBLE_BYTES]);

        for _ in 0..10_000 {
            let left_value = to_positive_nan::<f64>(f64::from_bits(TestUtil::next_long(
                &mut random,
                i64::MIN,
                i64::MAX,
            ) as u64));
            let right_value = to_positive_nan::<f64>(f64::from_bits(TestUtil::next_long(
                &mut random,
                i64::MIN,
                i64::MAX,
            ) as u64));
            NumericUtils::long_to_sortable_bytes(
                NumericUtils::double_to_sortable_long(left_value),
                &mut left.bytes,
                left.offset,
            );
            NumericUtils::long_to_sortable_bytes(
                NumericUtils::double_to_sortable_long(right_value),
                &mut right.bytes,
                right.offset,
            );
            let expected_sign = left_value.total_cmp(&right_value) as i32;
            let actual_sign = left.cmp(&right) as i32;

            // Assert that the numerical comparison matches the lexicographic
            // comparison
            assert_eq!(
                expected_sign, actual_sign,
                "Mismatch between numerical and lexicographic comparison for left: {}, right: {}",
                left_value, right_value
            );
        }

        Ok(())
    }

    /// Checks that the sort order of encoded `BigInt` values is consistent with
    /// `BigInt::cmp`.
    #[test]
    fn test_big_ints_compare() -> Result<()> {
        let mut random = random();
        for _ in 0..10_000 {
            let max_length = TestUtil::next_int(&mut random, 1, 16) as usize;
            let left_value = TestUtil::next_big_integer(&mut random, max_length as i32);
            let right_value = TestUtil::next_big_integer(&mut random, max_length as i32);
            let mut left = BytesRef::from_bytes(vec![0u8; max_length]);
            NumericUtils::big_int_to_sortable_bytes(&left_value, max_length, &mut left.bytes, 0)?;
            let mut right = BytesRef::from_bytes(vec![0u8; max_length]);
            NumericUtils::big_int_to_sortable_bytes(&right_value, max_length, &mut right.bytes, 0)?;
            let expected_sign = left_value.cmp(&right_value) as i32;
            let actual_sign = left.cmp(&right) as i32;
            assert_eq!(
                expected_sign, actual_sign,
                "Mismatch between numerical and lexicographic comparison for left: {}, right: {}",
                left_value, right_value
            );
        }

        Ok(())
    }

    fn to_positive_nan<T: Float>(value: T) -> T {
        if value.is_nan() { Float::nan() } else { value }
    }
}
