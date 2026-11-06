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
use crate::core::internal::vectorization::default_vector_util_support::DefaultVectorUtilSupport;
use crate::core::internal::vectorization::vector_util_support::VectorUtilSupport;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};

// The backend is a stateless concrete type selected at compile time.
// Static initialization avoids a lazy-initialization check on every score.
pub static VECTOR_UTIL: VectorUtil = VectorUtil {
  impl_: DefaultVectorUtilSupport,
};

pub struct VectorUtil {
  impl_: DefaultVectorUtilSupport,
}

impl VectorUtil {
  const EPSILON: f64 = 1e-4f64;
  const XOR_BIT_COUNT_STRIDE_AS_INT: bool = cfg!(target_arch = "aarch64");

  pub fn dot_product_f32(&self, a: &[f32], b: &[f32]) -> Result<f32> {
    if a.len() != b.len() {
      return Err(LuceneError::illegal_argument(format!(
        "vector dimensions differ: {}!={}",
        a.len(),
        b.len()
      )));
    }
    let r = self.impl_.dot_product_f32(a, b);
    debug_assert!(r.is_finite());
    Ok(r)
  }

  pub fn cosine_f32(&self, a: &[f32], b: &[f32]) -> Result<f32> {
    if a.len() != b.len() {
      return Err(LuceneError::illegal_argument(format!(
        "vector dimensions differ: {}!={}",
        a.len(),
        b.len()
      )));
    }
    let r = self.impl_.cosine_f32(a, b);
    debug_assert!(r.is_finite());
    Ok(r)
  }

  pub fn cosine_u8(&self, a: &[u8], b: &[u8]) -> Result<f32> {
    if a.len() != b.len() {
      return Err(LuceneError::illegal_argument(format!(
        "vector dimensions differ: {}!={}",
        a.len(),
        b.len()
      )));
    }
    Ok(self.impl_.cosine_u8(a, b))
  }

  #[inline]
  pub fn square_distance_f32(&self, a: &[f32], b: &[f32]) -> Result<f32> {
    if a.len() != b.len() {
      return Err(LuceneError::illegal_argument(format!(
        "vector dimensions differ: {}!={}",
        a.len(),
        b.len()
      )));
    }
    let r = self.impl_.square_distance_f32(a, b);
    debug_assert!(r.is_finite());
    Ok(r)
  }

  #[inline]
  pub fn square_distance_u8(&self, a: &[u8], b: &[u8]) -> Result<i32> {
    if a.len() != b.len() {
      return Err(LuceneError::illegal_argument(format!(
        "vector dimensions differ: {}!={}",
        a.len(),
        b.len()
      )));
    }
    Ok(self.impl_.square_distance_u8(a, b))
  }

  pub fn l2normalize(v: &mut [f32]) -> Result<()> {
    Self::l2normalize_with(v, true)
  }

  pub fn is_unit_vector(&self, v: &[f32]) -> bool {
    let l1norm = self.impl_.dot_product_f32(v, v) as f64;
    (l1norm - 1.0f64).abs() <= Self::EPSILON
  }

  pub fn l2normalize_with(v: &mut [f32], throw_on_zero: bool) -> Result<()> {
    let l1norm = VECTOR_UTIL.impl_.dot_product_f32(v, v) as f64;
    if l1norm == 0.0 {
      if throw_on_zero {
        return Err(LuceneError::illegal_argument(
          "Cannot normalize a zero-length vector",
        ));
      }
      return Ok(());
    }

    if (l1norm - 1.0f64).abs() <= Self::EPSILON {
      return Ok(());
    }

    let l2norm = l1norm.sqrt() as f32;
    for value in v.iter_mut() {
      *value /= l2norm;
    }
    Ok(())
  }

  pub fn add(u: &mut [f32], v: &[f32]) -> Result<()> {
    for i in 0..u.len() {
      u[i] += v[i];
    }
    Ok(())
  }

  pub fn dot_product_u8(&self, a: &[u8], b: &[u8]) -> Result<i32> {
    if a.len() != b.len() {
      return Err(LuceneError::illegal_argument(format!(
        "vector dimensions differ: {}!={}",
        a.len(),
        b.len()
      )));
    }
    Ok(self.impl_.dot_product_u8(a, b))
  }

  pub fn int4_dot_product(&self, a: &[u8], b: &[u8]) -> Result<i32> {
    if a.len() != b.len() {
      return Err(LuceneError::illegal_argument(format!(
        "vector dimensions differ: {}!={}",
        a.len(),
        b.len()
      )));
    }
    Ok(self.impl_.int4_dot_product(a, false, b, false))
  }

  pub fn int4_dot_product_packed(&self, unpacked: &[u8], packed: &[u8]) -> Result<i32> {
    if packed.len() != ((unpacked.len() + 1) >> 1) {
      return Err(LuceneError::illegal_argument(format!(
        "vector dimensions differ: {}!= 2 * {}",
        unpacked.len(),
        packed.len()
      )));
    }
    Ok(self.impl_.int4_dot_product(unpacked, false, packed, true))
  }

  pub fn xor_bit_count(&self, a: &[u8], b: &[u8]) -> Result<i32> {
    if a.len() != b.len() {
      return Err(LuceneError::illegal_argument(format!(
        "vector dimensions differ: {}!={}",
        a.len(),
        b.len()
      )));
    }
    if Self::XOR_BIT_COUNT_STRIDE_AS_INT {
      Ok(self.xor_bit_count_int(a, b))
    } else {
      Ok(self.xor_bit_count_long(a, b))
    }
  }

  pub(crate) fn xor_bit_count_int(&self, a: &[u8], b: &[u8]) -> i32 {
    let mut distance = 0u32;
    let mut i = 0usize;
    let stride = u32::BITS as usize / 8;
    let upper_bound = a.len() & !(stride - 1);

    while i < upper_bound {
      let lhs = BitUtil::get_i32_le(a, i);
      let rhs = BitUtil::get_i32_le(b, i);
      distance = distance.wrapping_add((lhs ^ rhs).count_ones());
      i += stride;
    }

    while i < a.len() {
      distance = distance.wrapping_add((a[i] ^ b[i]).count_ones());
      i += 1;
    }

    distance as i32
  }

  pub(crate) fn xor_bit_count_long(&self, a: &[u8], b: &[u8]) -> i32 {
    let mut distance = 0u32;
    let mut i = 0usize;
    let stride = BitUtil::LONG_BYTES;
    let upper_bound = a.len() & !(stride - 1);

    while i < upper_bound {
      let lhs = BitUtil::get_i64_le(a, i);
      let rhs = BitUtil::get_i64_le(b, i);
      distance = distance.wrapping_add((lhs ^ rhs).count_ones());
      i += stride;
    }

    while i < a.len() {
      distance = distance.wrapping_add((a[i] ^ b[i]).count_ones());
      i += 1;
    }

    distance as i32
  }

  pub fn dot_product_score(&self, a: &[u8], b: &[u8]) -> Result<f32> {
    let denom = (a.len() as i32).wrapping_mul(1 << 15) as f32;
    Ok(0.5f32 + self.dot_product_u8(a, b)? as f32 / denom)
  }

  pub fn scale_max_inner_product_score(vector_dot_product_similarity: f32) -> f32 {
    if vector_dot_product_similarity < 0.0 {
      return 1.0 / (1.0 + -vector_dot_product_similarity);
    }
    vector_dot_product_similarity + 1.0
  }

  /// Checks if a float vector only has finite components.
  ///
  /// # Arguments
  ///
  /// * `v` - bytes containing a vector
  ///
  /// # Errors
  ///
  /// returns [`LuceneErrorKind::IllegalArgument`] if any component of vector is not finite
  #[inline]
  pub fn check_finite(v: &[f32]) -> Result<()> {
    use wide::{bytemuck::cast, u32x16};

    let (groups, tail) = v.as_chunks::<16>();
    for (g, group) in groups.iter().enumerate() {
      let bits: u32x16 = cast(*group);
      if (bits << 1_u32).reduce_max() >= 0xff00_0000_u32 {
        for (j, &value) in group.iter().enumerate() {
          if !value.is_finite() {
            let i = g * 16 + j;
            return Err(LuceneError::illegal_argument(format!(
              "non-finite value at vector[{}]={}",
              i, value
            )));
          }
        }
      }
    }
    for (j, &value) in tail.iter().enumerate() {
      if !value.is_finite() {
        let i = groups.len() * 16 + j;
        return Err(LuceneError::illegal_argument(format!(
          "non-finite value at vector[{}]={}",
          i, value
        )));
      }
    }
    Ok(())
  }

  pub fn find_next_geq(&self, buffer: &[i32], target: i32, from: usize, to: usize) -> usize {
    debug_assert!({
      let mut ok = true;
      for i in 0..to.saturating_sub(1) {
        if buffer[i] > buffer[i + 1] {
          ok = false;
          break;
        }
      }
      ok
    });

    self.impl_.find_next_geq(buffer, target, from, to)
  }
}
