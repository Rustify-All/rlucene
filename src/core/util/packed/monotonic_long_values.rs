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
use crate::core::util::packed::monotonic_block_packed_reader::expected;
use crate::core::util::packed::packed_long_values::INITIAL_PAGE_COUNT;
use crate::core::util::ram_usage_estimator::size_of_vec;

pub(crate) struct MonotonicLongValues {
  averages: Vec<f32>,
}

impl MonotonicLongValues {
  pub(crate) fn new(averages: Vec<f32>) -> Self {
    Self { averages }
  }
  pub(crate) fn decode_block(&self, block: usize, dest: &mut [i64], count: usize) -> usize {
    let average = self.averages[block];
    if average != 0.0 {
      for (i, item) in dest.iter_mut().enumerate().take(count) {
        *item = item.wrapping_add(expected(0, average, i));
      }
    }
    count
  }

  pub(crate) fn get_value(&self, block: usize, element: usize, value: u64) -> i64 {
    expected(value as i64, self.averages[block], element)
  }
}

impl Accountable for MonotonicLongValues {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(size_of_vec(&self.averages))
  }
}

pub struct MonotonicLongValuesBuilder {
  averages: Vec<f32>,
}

impl Default for MonotonicLongValuesBuilder {
  fn default() -> Self {
    Self::new()
  }
}

impl MonotonicLongValuesBuilder {
  pub(crate) fn new() -> Self {
    Self {
      averages: Vec::with_capacity(INITIAL_PAGE_COUNT),
    }
  }

  pub(crate) fn build(mut self, values_off: usize) -> Result<MonotonicLongValues> {
    self.averages.drain(values_off..);

    Ok(MonotonicLongValues::new(std::mem::take(&mut self.averages)))
  }
  pub(crate) fn base_ram_bytes_used(&self) -> i64 {
    size_of_vec(&self.averages)
  }

  pub(crate) fn pack(&mut self, values: &mut [i64], num_values: usize, block: usize) {
    let average = if num_values == 1 {
      0.0
    } else {
      values[num_values - 1].wrapping_sub(values[0]) as f32 / (num_values - 1) as f32
    };

    if average != 0.0 {
      for (i, value) in values.iter_mut().enumerate().take(num_values) {
        *value = value.wrapping_sub(expected(0, average, i));
      }
    }
    if block == self.averages.len() {
      self.averages.push(average);
    } else {
      self.averages[block] = average;
    }
  }

  pub(crate) fn grow(&mut self, new_block_count: usize) -> Result<()> {
    if self.averages.capacity() < new_block_count {
      self
        .averages
        .reserve_exact(new_block_count - self.averages.len());
    }
    Ok(())
  }
}
