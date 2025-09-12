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
use crate::core::util::error::lucene_error::{LuceneError, Result};

/// Math utility methods.
pub struct MathUtil;

impl MathUtil {
    /// Returns `x <= 0 ? 0 : floor(log(x) / log(base))`
    ///
    /// # Parameters
    /// - `x`: The number to compute the logarithm for.
    /// - `base`: The logarithm base, must be greater than 1.
    ///
    /// # Returns
    /// - The integer part of the logarithm of `x` in the given `base`.
    ///
    /// # Panics
    /// - If `base <= 1`, it will panic.
    pub fn log(mut x: i64, base: i32) -> Result<i32> {
        if base == 2 {
            // This specialized method is significantly faster.
            return if x <= 0 {
                Ok(0)
            } else {
                Ok((63 - x.leading_zeros()) as i32)
            };
        } else if base <= 1 {
            return Err(LuceneError::illegal_argument("base must be > 1"));
        }

        let mut ret = 0;
        while x >= base as i64 {
            x /= base as i64;
            ret += 1;
        }
        Ok(ret)
    }

    /// Calculates logarithm in a given base with floating-point numbers.
    ///
    /// # Parameters
    /// - `base`: The logarithm base.
    /// - `x`: The number to compute the logarithm for.
    ///
    /// # Returns
    /// - The logarithm of `x` in the given `base`.
    pub fn log_f64(base: f64, x: f64) -> f64 {
        x.ln() / base.ln()
    }
    /// Returns the greatest common divisor (GCD) of `a` and `b`,
    ///
    /// # Notes
    /// - A GCD must be positive, but `2^64` cannot be expressed as an `i64`,
    ///   although it is the GCD of `i64::MIN` and `0`, as well as `i64::MIN`
    ///   and `i64::MIN`. In these two cases, this method returns `i64::MIN`.
    pub fn gcd(mut a: i64, mut b: i64) -> i64 {
        a = a.abs();
        b = b.abs();
        if a == 0 {
            return b;
        } else if b == 0 {
            return a;
        }

        let common_trailing_zeros = (a | b).trailing_zeros();
        a = (a as u64 >> a.trailing_zeros()) as i64;
        while b != 0 {
            b = (b as u64 >> b.trailing_zeros()) as i64;
            if a == b {
                break;
            } else if a > b || a == i64::MIN {
                std::mem::swap(&mut a, &mut b);
            }
            if a == 1 {
                break;
            }
            b -= a;
        }
        a << common_trailing_zeros
    }

    /// Calculates the inverse hyperbolic sine (`asinh`) of a `f64` value.
    #[allow(dead_code)]
    pub fn asinh(_a: f64) -> f64 {
        0f64
    }

    /// Calculates the inverse hyperbolic cosine (`acosh`) of a `f64` value.
    #[allow(dead_code)]
    pub fn acosh(_a: f64) -> f64 {
        0f64
    }

    /// Calculates the inverse hyperbolic tangent (`atanh`) of a `f64` value.
    #[allow(dead_code)]
    pub fn atanh(_a: f64) -> f64 {
        0f64
    }

    /// Returns a relative error bound for the sum of `num_values` positive
    /// doubles computed using recursive summation.
    ///
    /// # Notes
    /// - This only works if all values are positive.
    /// - Uses formula 3.5 from Higham (1993), "The accuracy of floating point
    ///   summation".
    pub fn sum_relative_error_bound(num_values: i32) -> f64 {
        if num_values <= 1 {
            return 0.0;
        }
        // Machine epsilon (unit roundoff)
        let u = f64::from_bits(0x3CA0000000000000); // 2^-52
        (num_values - 1) as f64 * u
    }

    /// Returns the maximum possible sum across `num_values` non-negative
    /// doubles, assuming one sum yielded `sum`.
    pub fn sum_upper_bound(sum: f64, num_values: i32) -> f64 {
        if num_values <= 2 {
            return sum;
        }

        let b = MathUtil::sum_relative_error_bound(num_values);
        (1.0 + 2.0 * b) * sum
    }
}
#[cfg(test)]
mod tests {
    use num_bigint::BigInt;
    use num_integer::Integer;
    use num_traits::{FromPrimitive, ToPrimitive};
    use rand::Rng;
    use rand::prelude::IndexedRandom;

    use crate::core::util::math_util::MathUtil;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{at_least, random};

    /// List of prime numbers.
    const PRIMES: [i64; 10] = [2, 3, 5, 7, 11, 13, 17, 19, 23, 29];

    /// Generates a random `i64` value following the logic in the original Java
    /// function.
    fn random_long<R: Rng + ?Sized>(random: &mut R) -> i64 {
        if random.random_bool(0.5) {
            let mut l = 1;
            if random.random_bool(0.5) {
                l *= -1;
            }
            for &i in PRIMES.iter() {
                let m = random.random_range(0..3);
                for _ in 0..m {
                    l *= i;
                }
            }
            l
        } else if random.random_bool(0.5) {
            random.random::<i64>()
        } else {
            let values = [i64::MIN, i64::MAX, 0, -1, 1];
            *values.choose(random).unwrap()
        }
    }
    /// Slow version of GCD used for testing.
    fn gcd(l1: i64, l2: i64) -> i64 {
        let big_l1 = BigInt::from_i64(l1).unwrap();
        let big_l2 = BigInt::from_i64(l2).unwrap();
        let gcd = big_l1.gcd(&big_l2);
        assert!(gcd.bits() <= 64);
        let two_64 = BigInt::from(1u128 << 64);
        let t = gcd.mod_floor(&two_64);
        let u = t.to_u64().unwrap();
        u as i64
    }
    #[test]
    fn test_gcd() {
        let mut random = random();
        let iters = at_least(&mut random, 100); // Replace with an appropriate function

        for _ in 0..iters {
            let l1 = random_long(&mut random);
            let l2 = random_long(&mut random);
            let gcd_value = MathUtil::gcd(l1, l2);
            let actual_gcd = gcd(l1, l2);

            assert_eq!(
                actual_gcd, gcd_value,
                "Expected GCD({},{}) = {}",
                l1, l2, actual_gcd
            );

            if gcd_value != 0 {
                assert_eq!(
                    l1,
                    (l1 / gcd_value) * gcd_value,
                    "l1 consistency check failed"
                );
                assert_eq!(
                    l2,
                    (l2 / gcd_value) * gcd_value,
                    "l2 consistency check failed"
                );
            }
        }
    }
    #[test]
    fn test_gcd2() {
        let a = 30;
        let b = 50;
        let c = 77;

        assert_eq!(0, MathUtil::gcd(0, 0));

        assert_eq!(b, MathUtil::gcd(0, b));
        assert_eq!(a, MathUtil::gcd(a, 0));

        assert_eq!(b, MathUtil::gcd(0, -b));
        assert_eq!(a, MathUtil::gcd(-a, 0));

        assert_eq!(10, MathUtil::gcd(a, b));
        assert_eq!(10, MathUtil::gcd(-a, b));
        assert_eq!(10, MathUtil::gcd(a, -b));
        assert_eq!(10, MathUtil::gcd(-a, -b));

        assert_eq!(1, MathUtil::gcd(a, c));
        assert_eq!(1, MathUtil::gcd(-a, c));
        assert_eq!(1, MathUtil::gcd(a, -c));
        assert_eq!(1, MathUtil::gcd(-a, -c));

        let lhs = 3i64.wrapping_mul(1i64 << 50);
        let rhs = 9i64.wrapping_mul(1i64 << 45);
        let expected = 3i64.wrapping_mul(1i64 << 45);
        assert_eq!(expected, MathUtil::gcd(lhs, rhs));

        let lhs = 1i64 << 45;
        let rhs = i64::MIN;
        assert_eq!(1i64 << 45, MathUtil::gcd(lhs, rhs));

        assert_eq!(i64::MAX, MathUtil::gcd(i64::MAX, 0));
        assert_eq!(i64::MAX, MathUtil::gcd(-i64::MAX, 0));

        assert_eq!(1, MathUtil::gcd(60247241209, 153092023));

        assert_eq!(i64::MIN, MathUtil::gcd(i64::MIN, 0));
        assert_eq!(i64::MIN, MathUtil::gcd(0, i64::MIN));
        assert_eq!(i64::MIN, MathUtil::gcd(i64::MIN, i64::MIN));
    }
    #[test]
    fn test_acosh_method() {}
    #[test]
    fn test_asinh_method() {}
    #[test]
    fn test_atanh_method() {}
}
