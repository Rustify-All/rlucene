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
use crate::core::store::random_access_input::RandomAccessInput;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::long_values::{LongValues, Zeroes};
use crate::core::util::packed::direct_monotonic_reader::direct_monotonic::Meta;
use crate::core::util::packed::direct_reader::{DirectPackedEnum, DirectReader};
use parking_lot::Mutex;
use std::sync::Arc;

/// Retrieves an instance previously written by
/// [`DirectMonotonicWriter`](crate::core::util::packed::direct_monotonic_writer::DirectMonotonicWriter).
///
///
/// # See also
/// `DirectMonotonicWriter`
pub struct DirectMonotonicReader<R>
where
    R: RandomAccessInput,
{
    block_shift: i32,
    block_mask: i64,
    readers: Vec<DirectPackedEnum<R>>,
    mins: Vec<i64>,
    avgs: Vec<f32>,
    bpvs: Vec<u8>,
}

impl<R> DirectMonotonicReader<R>
where
    R: RandomAccessInput,
{
    pub(crate) fn new(
        block_shift: i32,
        readers: Vec<DirectPackedEnum<R>>,
        mins: Vec<i64>,
        avgs: Vec<f32>,
        bpvs: Vec<u8>,
    ) -> Result<Self> {
        let readers_len = readers.len();
        if readers_len != mins.len() || readers_len != avgs.len() || readers_len != bpvs.len() {
            return Err(LuceneError::illegal_argument(String::from(
                "Mismatched array lengths",
            )));
        }
        let block_mask = (1i64 << block_shift) - 1;
        Ok(DirectMonotonicReader {
            block_shift,
            block_mask,
            readers,
            mins,
            avgs,
            bpvs,
        })
    }

    /// Get lower/upper bounds for the value at a given index without hitting
    /// the direct reader.
    fn get_bounds(&self, index: i64) -> Result<[i64; 2]> {
        let block: i32 = (((index as u64) >> self.block_shift) as i64).try_into()?;
        let block = block as usize;
        let block_index = index & self.block_mask;
        let lower_bound = self.mins[block] + ((self.avgs[block] * (block_index as f32)) as i64);
        let upper_bound = lower_bound + ((1i64 << (self.bpvs[block] as u32)) - 1);
        if self.bpvs[block] == 64 || upper_bound < lower_bound {
            Ok([i64::MIN, i64::MAX])
        } else {
            Ok([lower_bound, upper_bound])
        }
    }

    pub fn binary_search(&self, from_index: i64, to_index: i64, key: i64) -> Result<i64> {
        if from_index < 0 || from_index > to_index {
            return Err(LuceneError::illegal_argument(format!(
                "fromIndex={from_index}, toIndex={to_index}"
            )));
        }
        let mut lo = from_index;
        let mut hi = to_index - 1;
        while lo <= hi {
            let mid = (lo + hi) >> 1;
            // Try to run as many iterations of the binary search as possible
            // without hitting the direct readers, since they might
            // hit a page fault.
            let bounds = self.get_bounds(mid)?;
            if bounds[1] < key {
                lo = mid + 1;
            } else if bounds[0] > key {
                hi = mid - 1;
            } else {
                let mid_val = self.get(mid)?;
                match mid_val.cmp(&key) {
                    std::cmp::Ordering::Less => lo = mid + 1,
                    std::cmp::Ordering::Greater => hi = mid - 1,
                    std::cmp::Ordering::Equal => return Ok(mid),
                }
            }
        }
        Ok(-1 - lo)
    }
    /// Retrieves a non-merging instance from the specified slice.
    pub fn get_instance(meta: &Meta, data: Arc<Mutex<R>>) -> Result<Self> {
        Self::get_instance_with_merging(meta, data, false)
    }

    /// Retrieves an instance from the specified slice.
    pub fn get_instance_with_merging(
        meta: &Meta,
        data: Arc<Mutex<R>>,
        merging: bool,
    ) -> Result<Self> {
        let mut readers = Vec::with_capacity(meta.num_blocks);
        for i in 0..meta.num_blocks {
            let bpv = meta.bpvs[i];
            if bpv == 0 {
                readers.push(DirectPackedEnum::P(Zeroes));
            } else if merging
                && i < meta.num_blocks - 1// we only know the number of values for the last block
                && meta.block_shift >= DirectReader::MERGE_BUFFER_SHIFT
            {
                readers.push(DirectReader::get_merge_instance_with_base_offset(
                    data.clone(),
                    bpv as i32,
                    meta.offsets[i],
                    1i64 << meta.block_shift,
                ));
            } else {
                readers.push(DirectReader::get_instance_with_offset(
                    data.clone(),
                    bpv as i32,
                    meta.offsets[i],
                ));
            }
        }
        DirectMonotonicReader::new(
            meta.block_shift,
            readers,
            meta.mins.clone(),
            meta.avgs.clone(),
            meta.bpvs.clone(),
        )
    }
}
impl<R> LongValues for DirectMonotonicReader<R>
where
    R: RandomAccessInput,
{
    fn get(&self, index: i64) -> Result<i64> {
        let block = ((index as u64) >> self.block_shift) as usize;
        let block_index = index & self.block_mask;
        let delta = self.readers[block].get(block_index)?;
        Ok(self.mins[block] + ((self.avgs[block] * (block_index as f32)) as i64) + delta)
    }
}
pub mod direct_monotonic {

    /// In-memory metadata that needs to be kept around for
    /// [`DirectMonotonicReader`](crate::core::util::packed::direct_monotonic_reader::DirectMonotonicReader)
    /// to read data from disk.
    #[derive(Clone)]
    pub struct Meta {
        pub block_shift: i32,
        pub num_blocks: usize,
        pub mins: Vec<i64>,
        pub avgs: Vec<f32>,
        pub bpvs: Vec<u8>,
        pub offsets: Vec<i64>,
    }

    impl Meta {
        pub fn new(num_values: i64, block_shift: i32) -> Self {
            let mut num_blocks = (num_values as u64) >> (block_shift as u32);
            if (num_blocks << block_shift) < num_values as u64 {
                num_blocks += 1;
            }
            let num_blocks_usize = num_blocks as usize;
            Meta {
                block_shift,
                num_blocks: num_blocks_usize,
                mins: vec![0; num_blocks_usize],
                avgs: vec![0.0; num_blocks_usize],
                bpvs: vec![0; num_blocks_usize],
                offsets: vec![0; num_blocks_usize],
            }
        }

        /// Unlike Java Lucene, here we return a new object with identical
        /// properties.
        pub fn single_zero_block() -> Self {
            Meta::new(1, 63)
        }
    }
}

use crate::core::store::IndexInput;

/// Load metadata from the given [`IndexInput`].
///
/// # See also
/// `DirectMonotonicReader::getInstance(Meta, RandomAccessInput)`
pub fn load_meta(meta_in: &mut impl IndexInput, num_values: i64, block_shift: i32) -> Result<Meta> {
    let mut all_values_zero = true;
    let mut meta = Meta::new(num_values, block_shift);
    for i in 0..meta.num_blocks {
        let min = meta_in.read_long()?;
        meta.mins[i] = min;
        let avg_int = meta_in.read_int()?;
        meta.avgs[i] = f32::from_bits(avg_int as u32);
        meta.offsets[i] = meta_in.read_long()?;
        let bpv = meta_in.read_byte()?;
        meta.bpvs[i] = bpv;
        all_values_zero = all_values_zero && (min == 0) && (avg_int == 0) && (bpv == 0);
    }
    if all_values_zero {
        Ok(Meta::single_zero_block())
    } else {
        Ok(meta)
    }
}
