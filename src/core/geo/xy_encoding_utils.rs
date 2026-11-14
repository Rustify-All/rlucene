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
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::numeric_utils::NumericUtils;

pub struct XYEncodingUtils;

impl XYEncodingUtils {
  pub const MIN_VAL_INCL: f64 = -f32::MAX as f64;
  pub const MAX_VAL_INCL: f64 = f32::MAX as f64;

  /// validates value is a number and finite
  pub fn check_val(x: f32) -> Result<f32> {
    if !x.is_finite() {
      return Err(LuceneError::illegal_argument(format!(
        "invalid value {x}; must be between {} and {}",
        Self::MIN_VAL_INCL,
        Self::MAX_VAL_INCL
      )));
    }
    Ok(x)
  }

  /// Quantizes a Cartesian value into a 32-bit integer.
  ///
  /// # Errors
  ///
  /// Returns [`LuceneErrorKind::IllegalArgument`](crate::core::util::error::lucene_error::LuceneErrorKind::IllegalArgument) if `x` is out of bounds.
  pub fn encode(x: f32) -> Result<i32> {
    Ok(NumericUtils::float_to_sortable_int(Self::check_val(x)?))
  }

  /// Decodes a 32-bit quantized value produced by [`Self::encode`].
  pub fn decode(encoded: i32) -> f32 {
    let result = NumericUtils::sortable_int_to_float(encoded);
    debug_assert!(result >= Self::MIN_VAL_INCL as f32 && result <= Self::MAX_VAL_INCL as f32);
    result
  }

  /// Decodes four bytes from `src` starting at `offset`.
  pub fn decode_bytes(src: &[u8], offset: usize) -> f32 {
    Self::decode(NumericUtils::sortable_bytes_to_int(src, offset))
  }

  /// Converts a slice of `f32` values into `f64` values.
  pub fn float_array_to_double_array(f: &[f32]) -> Vec<f64> {
    f.iter().map(|&x| x as f64).collect()
  }
}
