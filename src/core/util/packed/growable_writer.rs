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
use std::fmt::{Display, Formatter};

use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::packed::mutable_packed64_enum::MutablePacked64Enum;
use crate::core::util::packed::{Mutable, PackedInts, Reader};

/// Implements [`Mutable`], but grows the bit count of the underlying packed
/// ints on-demand.
///
/// # Note
/// - Beware that this struct will accept setting negative values. However, in
///   order to do this, it will grow the number of bits per value to 64.
///
/// # Internal
/// This is an internal API and may change in future versions.
pub struct GrowableWriter {
  current_mask: i64,
  current: MutablePacked64Enum,
  acceptable_overhead_ratio: f32,
}
impl GrowableWriter {
  pub fn new(
    start_bits_per_value: i32,
    value_count: usize,
    acceptable_overhead_ratio: f32,
  ) -> Result<GrowableWriter> {
    let current =
      PackedInts::get_mutable(value_count, start_bits_per_value, acceptable_overhead_ratio)?;
    let current_mask = Self::mask(current.get_bits_per_value());
    Ok(GrowableWriter {
      current_mask,
      current,
      acceptable_overhead_ratio,
    })
  }
  fn mask(bits_per_value: i32) -> i64 {
    if bits_per_value == 64 {
      !0i64
    } else {
      PackedInts::max_value(bits_per_value)
    }
  }

  pub fn get_mutable(&self) -> &impl Mutable {
    &self.current
  }
  fn ensure_capacity(&mut self, value: i64) -> Result<()> {
    if (value & self.current_mask) == value {
      return Ok(());
    }
    let bits_required = PackedInts::unsigned_bits_required(value);
    debug_assert!(bits_required > self.current.get_bits_per_value());
    let value_count = self.size();
    let mut next =
      PackedInts::get_mutable(value_count, bits_required, self.acceptable_overhead_ratio)?;

    PackedInts::copy(
      &mut self.current,
      0,
      &mut next,
      0,
      value_count,
      PackedInts::DEFAULT_BUFFER_SIZE as usize,
    )?;

    self.current = next;
    self.current_mask = Self::mask(self.current.get_bits_per_value());
    Ok(())
  }

  pub fn resize(&mut self, new_size: usize) -> Result<GrowableWriter> {
    let mut next = GrowableWriter::new(
      self.current.get_bits_per_value(),
      new_size,
      self.acceptable_overhead_ratio,
    )?;
    let limit = std::cmp::min(self.size(), new_size);
    PackedInts::copy(
      &mut self.current,
      0,
      &mut next,
      0,
      limit,
      PackedInts::DEFAULT_BUFFER_SIZE as usize,
    )?;
    Ok(next)
  }
}

impl Reader for GrowableWriter {
  fn get(&self, index: usize) -> i64 {
    self.current.get(index)
  }

  fn get_bulk(&self, index: usize, arr: &mut [i64], off: usize, len: usize) -> Result<usize> {
    self.current.get_bulk(index, arr, off, len)
  }

  fn size(&self) -> usize {
    self.current.size()
  }
}

impl Accountable for GrowableWriter {
  fn ram_bytes_used(&self) -> Result<i64> {
    self.current.ram_bytes_used()
  }
}

impl Display for GrowableWriter {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}

impl Mutable for GrowableWriter {
  fn get_bits_per_value(&self) -> i32 {
    self.current.get_bits_per_value()
  }

  fn set(&mut self, index: usize, value: i64) -> Result<()> {
    self.ensure_capacity(value)?;
    self.current.set(index, value)
  }

  fn set_bulk(&mut self, index: usize, arr: &[i64], off: usize, len: usize) -> Result<usize> {
    if self.current_mask == -1
      && arr
        .get(off..)
        .and_then(|values| values.get(..len))
        .is_some()
    {
      return self.current.set_bulk(index, arr, off, len);
    }
    let mut max = 0i64;
    for i in off..off.wrapping_add(len) {
      max |= *arr.get(i).ok_or_else(|| {
        LuceneError::array_index_out_of_bounds(format!(
          "Index {i} out of bounds for length {}",
          arr.len()
        ))
      })?;
    }
    self.ensure_capacity(max)?;
    self.current.set_bulk(index, arr, off, len)
  }

  fn fill(&mut self, from_index: usize, to_index: usize, val: i64) -> Result<()> {
    self.ensure_capacity(val)?;
    self.current.fill(from_index, to_index, val)
  }

  fn clear(&mut self) -> Result<()> {
    self.current.clear()
  }
}
