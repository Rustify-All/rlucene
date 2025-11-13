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
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::long_values::LongValues;
use crate::core::util::packed::delta_packed_long_values::{
    DeltaPackedLongValues, DeltaPackedLongValuesBuilder,
};
use crate::core::util::packed::monotonic_long_values::MonotonicLongValuesBuilder;
use crate::core::util::packed::read_enum::PackedIntsReadEnum;
use crate::core::util::packed::{Mutable, NullReader, PackedInts, Reader};
use std::sync::Arc;

/// Utility struct to compress integers into a [`LongValues`] instance.
#[derive(Clone)]
pub struct PackedLongValues {
    page_shift: i32,
    pub(crate) page_mask: i32,
    pub(crate) values: Vec<Arc<PackedIntsReadEnum>>,
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
    /// Return a new [`PackedLongValuesBuilder`] that will compress efficiently
    /// positive integers.
    pub fn packed_long_values_builder(
        page_size: i32,
        acceptable_overhead_ratio: f32,
    ) -> Result<PackedLongValuesBuilder> {
        PackedLongValuesBuilder::new(page_size, acceptable_overhead_ratio)
    }
    /// See [`PackedLongValuesBuilder`].
    pub fn packed_long_values_builder_default(
        acceptable_overhead_ratio: f32,
    ) -> Result<PackedLongValuesBuilder> {
        Self::packed_long_values_builder(Self::DEFAULT_PAGE_SIZE, acceptable_overhead_ratio)
    }

    /// Return a new `DeltaPackedLongValuesBuilder` that will compress
    /// efficiently integers that are close to each other.
    pub fn delta_packed_long_values_builder(
        page_size: i32,
        acceptable_overhead_ratio: f32,
    ) -> Result<PackedLongValuesBuilder> {
        let sub_builder = DeltaPackedLongValuesBuilder::new();
        PackedLongValuesBuilder::with_sub_builder(
            page_size,
            acceptable_overhead_ratio,
            Some(sub_builder),
        )
    }

    /// See [`delta_packed_long_values_builder`](DeltaPackedLongValuesBuilder).
    pub fn delta_packed_long_values_builder_default(
        acceptable_overhead_ratio: f32,
    ) -> Result<PackedLongValuesBuilder> {
        Self::delta_packed_long_values_builder(Self::DEFAULT_PAGE_SIZE, acceptable_overhead_ratio)
    }

    /// Return a new [`MonotonicLongValuesBuilder`] that will compress
    /// efficiently integers that would be a monotonic function of their index.
    pub fn monotonic_long_values_builder(
        page_size: i32,
        acceptable_overhead_ratio: f32,
    ) -> Result<PackedLongValuesBuilder> {
        let sub_builder = MonotonicLongValuesBuilder::new();
        let sub_delta_builder = DeltaPackedLongValuesBuilder::with_sub_builder(Some(sub_builder));
        PackedLongValuesBuilder::with_sub_builder(
            page_size,
            acceptable_overhead_ratio,
            Some(sub_delta_builder),
        )
    }

    /// See [`monotonic_long_values_builder`](MonotonicLongValuesBuilder).
    pub fn monotonic_long_values_builder_default(
        acceptable_overhead_ratio: f32,
    ) -> Result<PackedLongValuesBuilder> {
        PackedLongValues::monotonic_long_values_builder(
            Self::DEFAULT_PAGE_SIZE,
            acceptable_overhead_ratio,
        )
    }
    fn new(
        page_shift: i32,
        page_mask: i32,
        values: Vec<PackedIntsReadEnum>,
        size: i64,
        ram_bytes_used: i64,
        sub_packed_long_values: Option<DeltaPackedLongValues>,
    ) -> Self {
        let sub_long_values = sub_packed_long_values.map(Arc::new);
        let values: Vec<Arc<PackedIntsReadEnum>> = values.into_iter().map(Arc::new).collect();
        Self {
            page_shift,
            page_mask,
            values,
            size,
            ram_bytes_used,
            sub_long_values,
        }
    }
    pub fn size(&self) -> i64 {
        self.size
    }

    fn decode_block(&self, block: i32, dest: &mut [i64], _count: i32) -> i32 {
        let vals = &self.values[block as usize];
        let size = vals.size();
        let mut k = 0;
        while k < size {
            k += vals.get_bulk(k, dest, k, size - k);
        }
        match self.sub_long_values {
            Some(ref sub) => sub.decode_block(block, dest, size),
            _ => size,
        }
    }

    fn get_value(&self, block: i32, element: i32, _value: i64) -> i64 {
        let value = if self.sub_long_values.is_some() {
            self.sub_long_values
                .as_ref()
                .unwrap()
                .get_value(block, element, 0)
        } else {
            0
        };
        self.values[block as usize].get(element) + value
    }
    pub fn iterator(&self) -> PackedLongValuesIterator {
        PackedLongValuesIterator::new(self.clone())
    }
}
impl Accountable for PackedLongValues {
    fn ram_bytes_used(&self) -> Result<i64> {
        //TODO
        Ok(0)
    }
}
impl LongValues for PackedLongValues {
    fn get(&self, index: i64) -> Result<i64> {
        debug_assert!(index >= 0);
        debug_assert!(index < self.size());
        let block = (index >> self.page_shift) as i32;
        let element = (index & self.page_mask as i64) as i32;
        Ok(self.get_value(block, element, 0))
    }
}

/// A Builder for a {@link PackedLongValues} instance.
#[derive(Default)]
pub struct PackedLongValuesBuilder {
    pub(crate) page_shift: i32,
    pub(crate) page_mask: i32,
    page_size: i32,
    acceptable_overhead_ratio: f32,
    pending: Option<Vec<i64>>,
    pub(crate) size: i64,
    pub(crate) values: Vec<PackedIntsReadEnum>,
    pub(crate) ram_bytes_used: i64,
    pub(crate) values_off: i32,
    pending_off: i32,
    sub_builder: Option<DeltaPackedLongValuesBuilder>,
}

pub(crate) const INITIAL_PAGE_COUNT: i32 = 16;
/// A Builder for a [`PackedLongValues`] instance.
impl PackedLongValuesBuilder {
    // TODO
    const BASE_RAM_BYTES_USED: i64 = 0;
    pub fn new(page_size: i32, acceptable_overhead_ratio: f32) -> Result<PackedLongValuesBuilder> {
        Self::with_sub_builder(page_size, acceptable_overhead_ratio, None)
    }
    pub(crate) fn with_sub_builder(
        page_size: i32,
        acceptable_overhead_ratio: f32,
        sub_packed_long_values_builder: Option<DeltaPackedLongValuesBuilder>,
    ) -> Result<PackedLongValuesBuilder> {
        let page_shift = PackedInts::check_block_size(page_size, MIN_PAGE_SIZE, MAX_PAGE_SIZE)?;
        let page_mask = page_size - 1;
        let pending = Some(vec![0; page_size as usize]);
        let mut values = Vec::new();
        // TODO: maybe we should impl `Clone` for `PackedIntsReadEnum`
        for _ in 0..INITIAL_PAGE_COUNT {
            values.push(PackedIntsReadEnum::NullReader(NullReader::new(0)));
        }
        Ok(Self {
            page_shift,
            page_mask,
            acceptable_overhead_ratio,
            page_size,
            pending,
            size: 0,
            values,
            ram_bytes_used: 0, // TODO
            values_off: 0,
            pending_off: 0,
            sub_builder: sub_packed_long_values_builder,
        })
    }
    /// Build a [`PackedLongValues`] instance that contains values that have
    /// been added to this builder. This operation is destructive.
    pub fn build(&mut self) -> Result<PackedLongValues> {
        self.finish()?;
        // TODO
        let ram_bytes_used = 0;
        let mut values = std::mem::take(&mut self.values);
        let _ = values.split_off(self.values_off as usize);
        if self.sub_builder.is_some() {
            let sub = self.sub_builder.take().unwrap().build(self.values_off)?;
            return Ok(PackedLongValues::new(
                self.page_shift,
                self.page_mask,
                std::mem::take(&mut values),
                self.size,
                ram_bytes_used,
                Some(sub),
            ));
        };
        Ok(PackedLongValues::new(
            self.page_shift,
            self.page_mask,
            values,
            self.size,
            ram_bytes_used,
            None,
        ))
    }
    pub fn add(&mut self, l: i64) -> Result<&mut Self> {
        if self.pending.is_none() {
            return Err(LuceneError::illegal_state("Cannot be reused after build()"));
        }

        if self.pending_off as usize == self.pending.as_ref().unwrap().len() {
            let current_value_len = self.values.len();
            if current_value_len == self.values_off as usize {
                // Not consistent with the Java version implementation, we
                // increase by half of the current length
                let new_length = current_value_len + current_value_len / 2;
                debug_assert!(new_length <= i32::MAX as usize);
                self.grow(new_length as i32)?;
            }
            self.pack_impl()?;
            debug_assert!(
                self.pending.is_none(),
                "pending should be None after pack_impl"
            );
            self.pending = Some(vec![0; self.page_size as usize])
        }

        self.pending.as_mut().unwrap()[self.pending_off as usize] = l;
        self.pending_off += 1;
        self.size += 1;
        Ok(self)
    }
    pub(crate) fn finish(&mut self) -> Result<()> {
        if self.pending_off > 0 {
            if self.values.len() == self.values_off as usize {
                self.grow(self.values_off + 1)?;
            }
            self.pack_impl()?;
        }
        Ok(())
    }
    fn pack_impl(&mut self) -> Result<()> {
        let mut pending = self.pending.take().unwrap();
        if self.sub_builder.is_some() {
            self.sub_builder.as_mut().unwrap().pack(
                &mut pending,
                self.pending_off,
                self.values_off,
            );
        }
        self.pack(
            &mut pending,
            self.pending_off,
            self.values_off,
            self.acceptable_overhead_ratio,
        )?;

        // TODO
        self.ram_bytes_used = 0;
        self.values_off += 1;
        // Reset pending buffer
        self.pending_off = 0;
        Ok(())
    }
    fn base_ram_bytes_used(&self) -> i64 {
        // TODO
        todo!()
    }

    fn pack(
        &mut self,
        values: &mut [i64],
        num_values: i32,
        block: i32,
        acceptable_overhead_ratio: f32,
    ) -> Result<()> {
        let mut min_value = values[0];
        let mut max_value = values[0];

        for &value in values.iter().take(num_values as usize).skip(1) {
            min_value = min_value.min(value);
            max_value = max_value.max(value);
        }

        // Build a new packed reader
        if min_value == 0 && max_value == 0 {
            let reader = NullReader::new(num_values);
            self.values[block as usize] = PackedIntsReadEnum::NullReader(reader);
            Ok(())
        } else {
            let bits_required = if min_value < 0 {
                64
            } else {
                PackedInts::bits_required(max_value)?
            };

            let mut mutable =
                PackedInts::get_mutable(num_values, bits_required, acceptable_overhead_ratio);
            let mut i = 0;
            while i < num_values {
                i += mutable.set_bulk(i, values, i, num_values - i);
            }

            self.values[block as usize] = PackedIntsReadEnum::PackedReader(mutable);
            Ok(())
        }
    }

    fn grow(&mut self, new_block_count: i32) -> Result<()> {
        if let Some(ref mut sub) = self.sub_builder {
            sub.grow(new_block_count)?;
        }
        // TODO
        self.ram_bytes_used = 0;
        ArrayUtil::grow_exact(&mut self.values, new_block_count as usize)?;
        Ok(())
    }
    pub fn size(&self) -> i64 {
        self.size
    }
}
impl Accountable for PackedLongValuesBuilder {
    fn ram_bytes_used(&self) -> Result<i64> {
        Ok(self.ram_bytes_used)
    }
}

pub struct PackedLongValuesIterator {
    packed_long_values: PackedLongValues,
    current_values: Vec<i64>,
    v_off: i32,
    p_off: i32,
    current_count: i32,
}

impl PackedLongValuesIterator {
    pub fn new(packed_long_values: PackedLongValues) -> Self {
        let current_values = vec![
            0;
            packed_long_values
                .size
                .min((packed_long_values.page_mask + 1) as i64)
                as usize
        ];
        let mut iterator = PackedLongValuesIterator {
            packed_long_values,
            current_values,
            v_off: 0,
            p_off: 0,
            current_count: 0,
        };
        iterator.fill_block();
        iterator
    }

    fn fill_block(&mut self) {
        if (self.v_off as usize) >= self.packed_long_values.values.len() {
            self.current_count = 0;
        } else {
            self.current_count =
                self.packed_long_values
                    .decode_block(self.v_off, &mut self.current_values, 0);
            debug_assert!(self.current_count > 0);
        }
    }

    pub fn has_next(&self) -> bool {
        self.p_off < self.current_count
    }

    pub fn next_value(&mut self) -> i64 {
        debug_assert!(self.has_next(), "No more values available");
        let result = self.current_values[self.p_off as usize];
        self.p_off += 1;
        if self.p_off == self.current_count {
            self.v_off += 1;
            self.p_off = 0;
            self.fill_block();
        }
        result
    }
}
