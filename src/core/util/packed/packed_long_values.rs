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
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::long_values::LongValues;
use crate::core::util::packed::delta_packed_long_values::{
  DeltaPackedLongValues, DeltaPackedLongValuesBuilder,
};
use crate::core::util::packed::monotonic_long_values::MonotonicLongValuesBuilder;
use crate::core::util::packed::mutable_packed64_enum::MutablePacked64Enum;
use crate::core::util::packed::read_enum::PackedIntsReadEnum;
use crate::core::util::packed::{Mutable, NullReader, PackedInts, Reader};
use crate::core::util::ram_usage_estimator::{size_of_slice, size_of_vec};
use std::borrow::Borrow;
use std::mem::size_of_val;
use std::sync::Arc;

/// Utility struct to compress integers into a [`LongValues`] instance.
#[derive(Clone)]
pub struct PackedLongValues {
  page_shift: i32,
  pub(crate) page_mask: usize,
  pub(crate) values: Arc<[PackedIntsReadEnum]>,
  pub(crate) size: i64,

  ram_bytes_used: i64,
  sub_long_values: Option<Arc<DeltaPackedLongValues>>,
}

const MIN_PAGE_SIZE: i32 = 64;
// More than 1M doesn't really makes sense with these appending buffers
// since their goal is to try to have small numbers of bits per value
const MAX_PAGE_SIZE: i32 = 1 << 20;
impl PackedLongValues {
  pub const DEFAULT_PAGE_SIZE: i32 = 256;
  /// Return a new [`Builder`] that will compress efficiently
  /// positive integers.
  pub fn packed_long_values_builder(
    page_size: i32,
    acceptable_overhead_ratio: f32,
  ) -> Result<Builder> {
    Builder::new(page_size, acceptable_overhead_ratio)
  }
  /// See [`Builder`].
  pub fn packed_long_values_builder_default(acceptable_overhead_ratio: f32) -> Result<Builder> {
    Self::packed_long_values_builder(Self::DEFAULT_PAGE_SIZE, acceptable_overhead_ratio)
  }

  /// Return a new `DeltaPackedLongValuesBuilder` that will compress
  /// efficiently integers that are close to each other.
  pub fn delta_packed_long_values_builder(
    page_size: i32,
    acceptable_overhead_ratio: f32,
  ) -> Result<Builder> {
    let sub_builder = DeltaPackedLongValuesBuilder::new();
    Builder::with_sub_builder(page_size, acceptable_overhead_ratio, Some(sub_builder))
  }

  /// See `delta_packed_long_values_builder`.
  pub fn delta_packed_long_values_builder_default(
    acceptable_overhead_ratio: f32,
  ) -> Result<Builder> {
    Self::delta_packed_long_values_builder(Self::DEFAULT_PAGE_SIZE, acceptable_overhead_ratio)
  }

  /// Return a new [`MonotonicLongValuesBuilder`] that will compress
  /// efficiently integers that would be a monotonic function of their index.
  pub fn monotonic_long_values_builder(
    page_size: i32,
    acceptable_overhead_ratio: f32,
  ) -> Result<Builder> {
    let sub_builder = MonotonicLongValuesBuilder::new();
    let sub_delta_builder = DeltaPackedLongValuesBuilder::with_sub_builder(Some(sub_builder));
    Builder::with_sub_builder(
      page_size,
      acceptable_overhead_ratio,
      Some(sub_delta_builder),
    )
  }

  /// See [`monotonic_long_values_builder`](MonotonicLongValuesBuilder).
  pub fn monotonic_long_values_builder_default(acceptable_overhead_ratio: f32) -> Result<Builder> {
    PackedLongValues::monotonic_long_values_builder(
      Self::DEFAULT_PAGE_SIZE,
      acceptable_overhead_ratio,
    )
  }
  fn new(
    page_shift: i32,
    page_mask: usize,
    values: Vec<PackedIntsReadEnum>,
    size: i64,
    sub_packed_long_values: Option<DeltaPackedLongValues>,
  ) -> Result<Self> {
    let sub_long_values = sub_packed_long_values.map(Arc::new);
    let values: Arc<[PackedIntsReadEnum]> = values.into();
    // Page slots are inline in the shared slice; readers account for their backing storage.
    let mut ram_bytes_used = size_of_slice(&values);
    for value in values.iter() {
      ram_bytes_used = ram_bytes_used.saturating_add(value.ram_bytes_used()?);
    }
    if let Some(sub_long_values) = &sub_long_values {
      ram_bytes_used = ram_bytes_used
        .saturating_add(size_of_val(sub_long_values.as_ref()) as i64)
        .saturating_add(sub_long_values.ram_bytes_used()?);
    }
    Ok(Self {
      page_shift,
      page_mask,
      values,
      size,
      ram_bytes_used,
      sub_long_values,
    })
  }
  pub fn size(&self) -> i64 {
    self.size
  }

  fn decode_block(&self, block: usize, dest: &mut [i64], _count: usize) -> Result<usize> {
    let vals = &self.values[block];
    let size = vals.size();
    let mut k = 0;
    while k < size {
      k += vals.get_bulk(k, dest, k, size - k)?;
    }
    match self.sub_long_values {
      Some(ref sub) => Ok(sub.decode_block(block, dest, size)),
      _ => Ok(size),
    }
  }

  fn get_value(&self, block: usize, element: usize, _value: i64) -> i64 {
    let value = match self.sub_long_values.as_ref() {
      Some(sub) => sub.get_value(block, element, 0),
      None => 0,
    };

    self.values[block].get(element).wrapping_add(value)
  }
  pub fn iterator(&self) -> Result<PackedLongValuesIterator> {
    PackedLongValuesIterator::new(self.clone())
  }
}
impl Accountable for PackedLongValues {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(self.ram_bytes_used)
  }
}
impl LongValues for PackedLongValues {
  fn get(&self, index: usize) -> Result<i64> {
    debug_assert!(index < self.size() as usize);
    let block = index >> self.page_shift;
    let element = index & self.page_mask;
    Ok(self.get_value(block, element, 0))
  }
}

/// Builder for a [`PackedLongValues`] instance.
#[derive(Default)]
pub struct Builder {
  pub(crate) page_shift: i32,
  acceptable_overhead_ratio: f32,
  pending: Vec<i64>,
  pub(crate) size: i64,
  pub(crate) values: Vec<PackedIntsReadEnum>,
  pub(crate) ram_bytes_used: i64,
  finished: bool,
  sub_builder: Option<DeltaPackedLongValuesBuilder>,
}

pub(crate) const INITIAL_PAGE_COUNT: usize = 16;
/// A Builder for a [`PackedLongValues`] instance.
impl Builder {
  // The builder's inline storage is accounted for by its owner.
  const BASE_RAM_BYTES_USED: i64 = 0;
  pub fn new(page_size: i32, acceptable_overhead_ratio: f32) -> Result<Builder> {
    Self::with_sub_builder(page_size, acceptable_overhead_ratio, None)
  }
  pub(crate) fn with_sub_builder(
    page_size: i32,
    acceptable_overhead_ratio: f32,
    sub_packed_long_values_builder: Option<DeltaPackedLongValuesBuilder>,
  ) -> Result<Builder> {
    let page_shift = PackedInts::check_block_size(page_size, MIN_PAGE_SIZE, MAX_PAGE_SIZE)?;
    let values = Vec::new();
    let ram_bytes_used = Self::BASE_RAM_BYTES_USED
      .saturating_add(size_of_vec(&values))
      .saturating_add(
        sub_packed_long_values_builder
          .as_ref()
          .map_or(0, DeltaPackedLongValuesBuilder::base_ram_bytes_used),
      );
    Ok(Self {
      page_shift,
      acceptable_overhead_ratio,
      pending: Vec::new(),
      size: 0,
      values,
      ram_bytes_used,
      finished: false,
      sub_builder: sub_packed_long_values_builder,
    })
  }
  /// Build a [`PackedLongValues`] instance that contains values that have
  /// been added to this builder. This operation is destructive.
  pub fn build(&mut self) -> Result<PackedLongValues> {
    self.finish()?;
    self.finished = true;
    self.pending = Vec::new();

    let values = std::mem::take(&mut self.values);

    let sub = match self.sub_builder.take() {
      Some(sb) => Some(sb.build(values.len())?),
      None => None,
    };

    let page_mask = (1usize << self.page_shift) - 1;
    PackedLongValues::new(self.page_shift, page_mask, values, self.size, sub)
  }
  #[inline]
  pub fn add(&mut self, l: i64) -> Result<&mut Self> {
    if self.finished {
      return Err(LuceneError::illegal_state("Cannot be reused after build()"));
    }
    let page_size = 1usize << self.page_shift;
    if self.pending.len() == page_size {
      let current_value_len = self.values.capacity();
      if current_value_len == self.values.len() {
        // Increase by half of the current length.
        let new_length = (current_value_len + current_value_len / 2).max(INITIAL_PAGE_COUNT);
        debug_assert!(new_length <= i32::MAX as usize);
        self.grow(new_length)?;
      }
      self.pack_impl()?;
    }

    if self.pending.capacity() == 0 {
      self.pending.reserve_exact(page_size);
      self.ram_bytes_used = self
        .ram_bytes_used
        .saturating_add(size_of_vec(&self.pending));
    }
    self.pending.push(l);
    self.size += 1;
    Ok(self)
  }
  pub(crate) fn finish(&mut self) -> Result<()> {
    if !self.pending.is_empty() {
      if self.values.capacity() == self.values.len() {
        self.grow((self.values.len() + 1).max(INITIAL_PAGE_COUNT))?;
      }
      self.pack_impl()?;
    }
    Ok(())
  }
  fn pack_impl(&mut self) -> Result<()> {
    let num_values = self.pending.len();
    let normalized_bits = self
      .sub_builder
      .as_mut()
      .and_then(|builder| builder.pack(self.pending.as_mut(), num_values, self.values.len()));

    let reader = self.pack(num_values, self.acceptable_overhead_ratio, normalized_bits)?;
    self.ram_bytes_used = self.ram_bytes_used.saturating_add(reader.ram_bytes_used()?);
    self.values.push(reader);
    // Reset pending buffer
    self.pending.clear();
    Ok(())
  }
  fn pack(
    &self,
    num_values: usize,
    acceptable_overhead_ratio: f32,
    normalized_bits: Option<i64>,
  ) -> Result<PackedIntsReadEnum> {
    let value_bits = normalized_bits.unwrap_or_else(|| {
      let mut bits = self.pending[0];
      if bits >= 0 {
        for &value in self.pending.iter().take(num_values).skip(1) {
          bits |= value;
        }
      }
      bits
    });

    // The union preserves zero, the sign bit and the highest required bit.
    Ok(if value_bits == 0 {
      PackedIntsReadEnum::NullReader(NullReader::new(num_values))
    } else {
      let bits_required = if value_bits < 0 {
        64
      } else {
        PackedInts::bits_required(value_bits)?
      };

      let mut mutable =
        PackedInts::get_mutable(num_values, bits_required, acceptable_overhead_ratio)?;
      let mut i = 0;
      while i < num_values {
        i += mutable.set_bulk(i, self.pending.as_slice(), i, num_values - i)?;
      }

      match mutable {
        MutablePacked64Enum::P64SingleBlock1(value) => PackedIntsReadEnum::P64SingleBlock1(value),
        MutablePacked64Enum::P64SingleBlock2(value) => PackedIntsReadEnum::P64SingleBlock2(value),
        MutablePacked64Enum::P64SingleBlock3(value) => PackedIntsReadEnum::P64SingleBlock3(value),
        MutablePacked64Enum::P64SingleBlock4(value) => PackedIntsReadEnum::P64SingleBlock4(value),
        MutablePacked64Enum::P64SingleBlock5(value) => PackedIntsReadEnum::P64SingleBlock5(value),
        MutablePacked64Enum::P64SingleBlock6(value) => PackedIntsReadEnum::P64SingleBlock6(value),
        MutablePacked64Enum::P64SingleBlock7(value) => PackedIntsReadEnum::P64SingleBlock7(value),
        MutablePacked64Enum::P64SingleBlock8(value) => PackedIntsReadEnum::P64SingleBlock8(value),
        MutablePacked64Enum::P64SingleBlock9(value) => PackedIntsReadEnum::P64SingleBlock9(value),
        MutablePacked64Enum::P64SingleBlock10(value) => PackedIntsReadEnum::P64SingleBlock10(value),
        MutablePacked64Enum::P64SingleBlock12(value) => PackedIntsReadEnum::P64SingleBlock12(value),
        MutablePacked64Enum::P64SingleBlock16(value) => PackedIntsReadEnum::P64SingleBlock16(value),
        MutablePacked64Enum::P64SingleBlock21(value) => PackedIntsReadEnum::P64SingleBlock21(value),
        MutablePacked64Enum::P64SingleBlock32(value) => PackedIntsReadEnum::P64SingleBlock32(value),
        MutablePacked64Enum::P64(value) => PackedIntsReadEnum::P64(value),
      }
    })
  }

  fn grow(&mut self, new_block_count: usize) -> Result<()> {
    let old_values_size = size_of_vec(&self.values);
    let old_sub_size = self
      .sub_builder
      .as_ref()
      .map_or(0, DeltaPackedLongValuesBuilder::base_ram_bytes_used);
    if let Some(ref mut sub) = self.sub_builder {
      sub.grow(new_block_count)?;
    }
    if self.values.capacity() < new_block_count {
      self
        .values
        .reserve_exact(new_block_count - self.values.len());
    }
    let new_sub_size = self
      .sub_builder
      .as_ref()
      .map_or(0, DeltaPackedLongValuesBuilder::base_ram_bytes_used);
    self.ram_bytes_used = self
      .ram_bytes_used
      .saturating_sub(old_values_size)
      .saturating_sub(old_sub_size)
      .saturating_add(size_of_vec(&self.values))
      .saturating_add(new_sub_size);
    Ok(())
  }
  pub fn size(&self) -> i64 {
    self.size
  }
}
impl Accountable for Builder {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(self.ram_bytes_used)
  }
}

pub struct PackedLongValuesIterator<V = PackedLongValues> {
  packed_long_values: V,
  current_values: smallvec::SmallVec<[i64; 4]>,
  v_off: usize,
  p_off: usize,
  current_count: usize,
}

impl<V: Borrow<PackedLongValues>> PackedLongValuesIterator<V> {
  pub fn new(packed_long_values: V) -> Result<Self> {
    let values = packed_long_values.borrow();
    let current_values =
      smallvec::smallvec![0; values.size.min((values.page_mask + 1) as i64) as usize];
    let mut iterator = PackedLongValuesIterator {
      packed_long_values,
      current_values,
      v_off: 0,
      p_off: 0,
      current_count: 0,
    };
    iterator.fill_block()?;
    Ok(iterator)
  }

  fn fill_block(&mut self) -> Result<()> {
    let values = self.packed_long_values.borrow();
    if (self.v_off) >= values.values.len() {
      self.current_count = 0;
    } else {
      self.current_count = values.decode_block(self.v_off, &mut self.current_values, 0)?;
      debug_assert!(self.current_count > 0);
    }
    Ok(())
  }

  pub fn has_next(&self) -> bool {
    self.p_off < self.current_count
  }

  pub fn next_value(&mut self) -> Result<i64> {
    debug_assert!(self.has_next(), "No more values available");
    let result = self.current_values[self.p_off];
    self.p_off += 1;
    if self.p_off == self.current_count {
      self.v_off += 1;
      self.p_off = 0;
      self.fill_block()?;
    }
    Ok(result)
  }
}
