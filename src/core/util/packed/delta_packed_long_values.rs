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
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::packed::monotonic_long_values::{
  MonotonicLongValues, MonotonicLongValuesBuilder,
};
use crate::core::util::ram_usage_estimator::{size_of_slice, size_of_vec};

pub(crate) struct DeltaPackedLongValues {
  pub(crate) sub_long_value: Option<MonotonicLongValues>,
  pub(crate) mins: Box<[i64]>,
}

impl DeltaPackedLongValues {
  pub(crate) fn new(mins: Vec<i64>, sub_reader: Option<MonotonicLongValues>) -> Self {
    Self {
      sub_long_value: sub_reader,
      mins: mins.into_boxed_slice(),
    }
  }
  pub(crate) fn decode_block(&self, block: usize, dest: &mut [i64], count: usize) -> usize {
    let min = self.mins[block];
    if min != 0 {
      for item in dest.iter_mut().take(count) {
        *item = item.wrapping_add(min);
      }
    }
    match self.sub_long_value {
      Some(ref sub) => sub.decode_block(block, dest, count),
      _ => count,
    }
  }

  pub(crate) fn get_value(&self, block: usize, element: usize, _value: u64) -> i64 {
    let current = self.mins[block];
    match self.sub_long_value {
      Some(ref reader) => reader.get_value(block, element, current as u64),
      None => current,
    }
  }
}

impl Accountable for DeltaPackedLongValues {
  fn ram_bytes_used(&self) -> Result<i64> {
    let mut size = size_of_slice(&self.mins);
    if let Some(sub_reader) = &self.sub_long_value {
      size = size.saturating_add(sub_reader.ram_bytes_used()?);
    }
    Ok(size)
  }
}

pub struct DeltaPackedLongValuesBuilder {
  pub(crate) sub_builder: Option<MonotonicLongValuesBuilder>,
  pub(crate) mins: Vec<i64>,
}
impl Default for DeltaPackedLongValuesBuilder {
  fn default() -> Self {
    Self::new()
  }
}

impl DeltaPackedLongValuesBuilder {
  pub(crate) fn new() -> DeltaPackedLongValuesBuilder {
    Self::with_sub_builder(None)
  }
  pub(crate) fn with_sub_builder(
    sub_builder: Option<MonotonicLongValuesBuilder>,
  ) -> DeltaPackedLongValuesBuilder {
    Self {
      sub_builder,
      mins: Vec::new(),
    }
  }

  pub(crate) fn build(mut self, values_off: usize) -> Result<DeltaPackedLongValues> {
    let sub_reader = match self.sub_builder.take() {
      Some(sb) => Some(sb.build(values_off)?),
      None => None,
    };

    self.mins.truncate(values_off);

    Ok(DeltaPackedLongValues::new(self.mins, sub_reader))
  }
  pub(crate) fn pack(
    &mut self,
    values: &mut [i64],
    num_values: usize,
    block: usize,
  ) -> Option<i64> {
    if let Some(sub_builder) = self.sub_builder.as_mut() {
      sub_builder.pack(values, num_values, block);
    }

    let mut min = values[0];
    for &value in values.iter().take(num_values).skip(1) {
      min = min.min(value);
    }
    let normalized_bits = if min == 0 {
      None
    } else {
      use wide::i64x4;

      let length = num_values.min(values.len());
      let (groups, tail) = values[..length].as_chunks_mut::<4>();
      let base = i64x4::splat(min);
      let mut union = i64x4::ZERO;
      for group in groups {
        let deltas = i64x4::new(*group) - base;
        *group = deltas.to_array();
        union |= deltas;
      }
      let [a, b, c, d] = union.to_array();
      let mut bits = a | b | c | d;
      for value in tail {
        *value = value.wrapping_sub(min);
        bits |= *value;
      }
      Some(bits)
    };
    if block == self.mins.len() {
      self.mins.push(min);
    } else {
      self.mins[block] = min;
    }
    normalized_bits
  }

  pub(crate) fn grow(&mut self, new_block_count: usize) -> Result<()> {
    if let Some(ref mut builder) = self.sub_builder {
      builder.grow(new_block_count)?
    }
    if self.mins.capacity() < new_block_count {
      self.mins.reserve_exact(new_block_count - self.mins.len());
    }
    Ok(())
  }
  pub(crate) fn base_ram_bytes_used(&self) -> i64 {
    let mut size = size_of_vec(&self.mins);
    if let Some(sub_builder) = &self.sub_builder {
      size = size.saturating_add(sub_builder.base_ram_bytes_used());
    }
    size
  }
}
