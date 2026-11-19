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
use crate::core::util::bit_util::BitUtil;
use crate::core::util::core_helper::CoreHelper;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
/// Maintains the maximum score and its corresponding document id concurrently
pub(crate) static DEFAULT_INTERVAL: AtomicUsize = AtomicUsize::new(0x3ff);
pub struct MaxScoreAccumulator {
  // we use 2^10-1 to check the remainder with a bitwise operation

  // scores are always positive
  acc: AtomicI64,

  // non-final and visible for tests
  pub(crate) mod_interval: usize,
}

impl MaxScoreAccumulator {
  pub(crate) fn new() -> Self {
    Self {
      acc: AtomicI64::new(i64::MIN),
      mod_interval: DEFAULT_INTERVAL.load(Ordering::Relaxed),
    }
  }

  /// Return the max encoded docId and score found in the two longs, following the encoding in accumulate.
  fn max_encode(v1: i64, v2: i64) -> i64 {
    let score1 = f32::from_bits((v1 >> 32) as u32);
    let score2 = f32::from_bits((v2 >> 32) as u32);
    match CoreHelper::compare_f32(score1, score2) {
      std::cmp::Ordering::Equal => {
        // tie-break on the minimum doc base
        if (v1 as i32) < (v2 as i32) { v1 } else { v2 }
      },
      std::cmp::Ordering::Greater => v1,
      std::cmp::Ordering::Less => v2,
    }
  }

  pub(crate) fn accumulate(&self, doc_id: i32, score: f32) {
    debug_assert!(doc_id >= 0 && score >= 0.0);
    let encode: i64 =
      ((BitUtil::float_to_int_bits(score) as i64) << 32) | (doc_id as i64 & 0xffffffff);
    let _ = self
      .acc
      .try_update(Ordering::Relaxed, Ordering::Relaxed, |prev| {
        let next = Self::max_encode(prev, encode);
        (next != prev).then_some(next)
      });
  }

  pub fn to_score(value: i64) -> f32 {
    f32::from_bits((value >> 32) as u32)
  }

  pub fn doc_id(value: i64) -> i32 {
    value as i32
  }

  pub(crate) fn get_raw(&self) -> i64 {
    self.acc.load(Ordering::Relaxed)
  }
}
