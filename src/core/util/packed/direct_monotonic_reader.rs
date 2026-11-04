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
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::long_values::{LongValues, Zeroes};
use crate::core::util::packed::direct_reader::{
  DirectPackedEnum, DirectReader, FromSlice, FromSliceShared,
};
use crate::core::util::ram_usage_estimator::size_of_vec;

/// Retrieves an instance previously written by
/// [`DirectMonotonicWriter`](crate::core::util::packed::direct_monotonic_writer::DirectMonotonicWriter).
///
///
/// # See also
/// [`DirectMonotonicWriter`](crate::core::util::packed::direct_monotonic_writer::DirectMonotonicWriter)
pub struct DirectMonotonicReader<R> {
  block_shift: i32,
  block_mask: usize,
  readers: Box<[DirectPackedEnum<R>]>,
  has_merge_reader: bool,
  metadata: Box<[(i64, f32, u8)]>,
  slice: R,
}

impl<R> DirectMonotonicReader<R> {
  pub(crate) fn new(
    block_shift: i32,
    readers: Vec<DirectPackedEnum<R>>,
    meta: &Meta,
    slice: R,
  ) -> Result<Self> {
    let readers_len = readers.len();
    if readers_len != meta.mins.len()
      || readers_len != meta.avgs.len()
      || readers_len != meta.bpvs.len()
    {
      return Err(LuceneError::illegal_argument(String::from(
        "Mismatched array lengths",
      )));
    }
    let block_mask = (1 << block_shift) - 1;
    let has_merge_reader = readers
      .iter()
      .any(|reader| matches!(reader, DirectPackedEnum::Merge(_)));
    Ok(DirectMonotonicReader {
      block_shift,
      block_mask,
      readers: readers.into_boxed_slice(),
      has_merge_reader,
      metadata: meta
        .mins
        .iter()
        .zip(&meta.avgs)
        .zip(&meta.bpvs)
        .map(|((&min, &avg), &bpv)| (min, avg, bpv))
        .collect(),
      slice,
    })
  }
}

impl<R> DirectMonotonicReader<R>
where
  R: RandomAccessInput,
{
  /// Get lower/upper bounds for the value at a given index without hitting
  /// the direct reader.
  fn get_bounds(&self, index: usize) -> Result<[i64; 2]> {
    let block = index >> self.block_shift;
    let block_index = index & self.block_mask;
    let lower_bound = self.metadata[block]
      .0
      .wrapping_add((self.metadata[block].1 * (block_index as f32)) as i64);
    let upper_bound = lower_bound
      .wrapping_add(1i64.wrapping_shl(self.metadata[block].2 as u32))
      .wrapping_sub(1);
    if self.metadata[block].2 == 64 || upper_bound < lower_bound {
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
    if self.has_merge_reader {
      return Err(LuceneError::unsupported_operation(
        "binary_search requires a non-merge reader",
      ));
    }
    let mut lo = from_index;
    let mut hi = to_index - 1;
    while lo <= hi {
      let mid = (lo + hi) >> 1;
      // Try to run as many iterations of the binary search as possible
      // without hitting the direct readers, since they might
      // hit a page fault.
      let mid_index = mid as usize;
      let bounds = self.get_bounds(mid_index)?;
      if bounds[1] < key {
        lo = mid + 1;
      } else if bounds[0] > key {
        hi = mid - 1;
      } else {
        let mid_val = self.get(mid_index)?;
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
  pub fn get_instance(meta: &Meta, data: R) -> Result<Self> {
    Self::get_instance_with_merging(meta, data, false)
  }

  /// Retrieves an instance from the specified slice.
  pub fn get_instance_with_merging(meta: &Meta, data: R, merging: bool) -> Result<Self> {
    let mut readers = Vec::with_capacity(meta.num_blocks);
    for i in 0..meta.num_blocks {
      let bpv = meta.bpvs[i];
      if bpv == 0 {
        readers.push(DirectPackedEnum::Zeroes(Zeroes));
      } else if merging
                && i < meta.num_blocks - 1// we only know the number of values for the last block
                && meta.block_shift >= DirectReader::MERGE_BUFFER_SHIFT as i32
      {
        readers.push(DirectReader::get_merge_instance_with_base_offset(
          None,
          bpv as i32,
          meta.offsets[i],
          1 << meta.block_shift,
        ));
      } else {
        readers.push(DirectReader::get_instance_with_offset(
          None,
          bpv as i32,
          meta.offsets[i],
        )?);
      }
    }
    DirectMonotonicReader::new(meta.block_shift, readers, meta, data)
  }
}
impl<R> LongValues for DirectMonotonicReader<R>
where
  R: RandomAccessInput,
{
  fn get_mut(&mut self, index: usize) -> Result<i64> {
    let block = index >> self.block_shift;
    let block_index = index & self.block_mask;
    let delta = self.readers[block].read_from_slice(block_index, Some(&self.slice))?;
    Ok(
      self.metadata[block]
        .0
        .wrapping_add((self.metadata[block].1 * (block_index as f32)) as i64)
        .wrapping_add(delta),
    )
  }

  fn get(&self, index: usize) -> Result<i64> {
    // Merge blocks require get_mut to update their cache.
    let block = index >> self.block_shift;
    let block_index = index & self.block_mask;
    let delta = self.readers[block].read_from_slice_shared(block_index, Some(&self.slice))?;
    Ok(
      self.metadata[block]
        .0
        .wrapping_add((self.metadata[block].1 * (block_index as f32)) as i64)
        .wrapping_add(delta),
    )
  }
}
/// In-memory metadata that needs to be kept around for
/// [`DirectMonotonicReader`] to read data from disk.
#[derive(Clone)]
pub struct Meta {
  pub block_shift: i32,
  pub num_blocks: usize,
  pub mins: Vec<i64>,
  pub avgs: Vec<f32>,
  pub bpvs: Vec<u8>,
  pub offsets: Vec<usize>,
}

impl Meta {
  fn block_count(num_values: i64, block_shift: i32) -> usize {
    let mut num_blocks = (num_values as u64) >> (block_shift as u32);
    if (num_blocks << block_shift) < num_values as u64 {
      num_blocks += 1;
    }
    num_blocks as usize
  }

  pub fn new(num_values: i64, block_shift: i32) -> Self {
    let num_blocks_usize = Self::block_count(num_values, block_shift);
    Meta {
      block_shift,
      num_blocks: num_blocks_usize,
      mins: vec![0; num_blocks_usize],
      avgs: vec![0.0; num_blocks_usize],
      bpvs: vec![0; num_blocks_usize],
      offsets: vec![0; num_blocks_usize],
    }
  }

  /// Returns a new object with identical properties.
  pub fn single_zero_block() -> Self {
    Meta::new(1, 63)
  }
}

impl Accountable for Meta {
  fn ram_bytes_used(&self) -> Result<i64> {
    Ok(
      size_of_vec(&self.mins)
        .saturating_add(size_of_vec(&self.avgs))
        .saturating_add(size_of_vec(&self.bpvs))
        .saturating_add(size_of_vec(&self.offsets)),
    )
  }
}

use crate::core::store::IndexInput;
use crate::core::util::TryIntoInt;

/// Load metadata from the given [`IndexInput`].
///
/// # See also
/// [`DirectMonotonicReader::get_instance`]
pub fn load_meta<II>(meta_in: &mut II, num_values: i64, block_shift: i32) -> Result<Meta>
where
  II: IndexInput,
{
  let mut all_values_zero = true;
  let num_blocks = Meta::block_count(num_values, block_shift);
  let mut meta = Meta {
    block_shift,
    num_blocks,
    mins: Vec::with_capacity(num_blocks),
    avgs: Vec::with_capacity(num_blocks),
    offsets: Vec::with_capacity(num_blocks),
    bpvs: Vec::with_capacity(num_blocks),
  };
  for _ in 0..num_blocks {
    let min = meta_in.read_long()?;
    meta.mins.push(min);
    let avg_int = meta_in.read_int()?;
    meta.avgs.push(f32::from_bits(avg_int as u32));
    meta.offsets.push(meta_in.read_long()?.try_convert()?);
    let bpv = meta_in.read_byte()?;
    meta.bpvs.push(bpv);
    all_values_zero = all_values_zero && (min == 0) && (avg_int == 0) && (bpv == 0);
  }
  if all_values_zero {
    if meta.num_blocks == 1 {
      meta.block_shift = 63;
      meta.offsets[0] = 0;
      Ok(meta)
    } else {
      Ok(Meta::single_zero_block())
    }
  } else {
    Ok(meta)
  }
}
