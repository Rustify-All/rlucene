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
use crate::core::store::IndexOutput;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::packed::direct_writer::{DirectWriter, unsigned_bits_required};
/// Write monotonically-increasing sequences of integers. This writer splits
/// data into blocks and then for each block, computes the average slope, the
/// minimum value, and encodes only the delta from the expected value using a
/// [`DirectWriter`].
///
/// # See also
/// [`DirectMonotonicReader`](crate::core::util::packed::direct_monotonic_reader::DirectMonotonicReader)
///
/// # Internal
pub struct DirectMonotonicWriter<'a, I1, I2> {
  meta: &'a mut I1,
  data: &'a mut I2,
  num_values: i64,
  base_data_pointer: usize,
  buffer: smallvec::SmallVec<[i64; 4]>,
  buffer_size: usize,
  count: i64,
  finished: bool,
  previous: i64,
}

impl<'a, I1, I2> DirectMonotonicWriter<'a, I1, I2>
where
  I1: IndexOutput,
  I2: IndexOutput,
{
  fn new(
    meta_out: &'a mut I1,
    data_out: &'a mut I2,
    num_values: i64,
    block_shift: i32,
  ) -> Result<Self> {
    if !(MIN_BLOCK_SHIFT..=MAX_BLOCK_SHIFT).contains(&block_shift) {
      return Err(LuceneError::illegal_argument(format!(
        "blockShift must be in [{}-{}], got {}",
        MIN_BLOCK_SHIFT, MAX_BLOCK_SHIFT, block_shift
      )));
    }

    if num_values < 0 {
      return Err(LuceneError::illegal_argument(format!(
        "numValues can't be negative, got {num_values}"
      )));
    }

    let num_blocks = if num_values == 0 {
      0
    } else {
      ((num_values - 1) >> block_shift) + 1
    };

    if num_blocks > ArrayUtil::MAX_ARRAY_LENGTH as i64 {
      return Err(LuceneError::illegal_argument(format!(
        "blockShift is too low for the provided number of values: blockShift={}, numValues={}, MAX_ARRAY_LENGTH={}",
        block_shift,
        num_values,
        ArrayUtil::MAX_ARRAY_LENGTH
      )));
    }

    let block_size = 1i64 << block_shift;
    let buffer_len = std::cmp::min(num_values, block_size) as usize;
    let buffer = smallvec::smallvec![0i64; buffer_len];
    let base_data_pointer = data_out.get_file_pointer()?;

    Ok(DirectMonotonicWriter {
      meta: meta_out,
      data: data_out,
      num_values,
      base_data_pointer,
      buffer,
      buffer_size: 0,
      count: 0,
      finished: false,
      previous: i64::MIN,
    })
  }
  fn flush(&mut self) -> Result<()> {
    debug_assert!(self.buffer_size != 0);

    let avg_inc = {
      let numerator = self.buffer[self.buffer_size - 1].wrapping_sub(self.buffer[0]);
      let denominator = std::cmp::max(1, self.buffer_size - 1) as f64;
      (numerator as f64 / denominator) as f32
    };

    let mut min = i64::MAX;
    for i in 0..self.buffer_size {
      let expected = (avg_inc * (i as f32)) as i64;
      self.buffer[i] = self.buffer[i].wrapping_sub(expected);
      min = std::cmp::min(self.buffer[i], min);
    }

    let mut max_delta = 0;
    for i in 0..self.buffer_size {
      self.buffer[i] = self.buffer[i].wrapping_sub(min);
      // use | will change nothing when it comes to computing required
      // bits but has the benefit of working fine with
      // negative values too (in case of overflow)
      max_delta |= self.buffer[i];
    }

    self.meta.write_long(min)?;
    self.meta.write_int(BitUtil::float_to_int_bits(avg_inc))?;
    self
      .meta
      .write_long((self.data.get_file_pointer()? - self.base_data_pointer) as i64)?;
    if max_delta == 0 {
      self.meta.write_byte(0)?;
    } else {
      let bits_required = unsigned_bits_required(max_delta);
      let mut writer =
        DirectWriter::get_instance(self.data, self.buffer_size as i64, bits_required)?;
      for i in 0..self.buffer_size {
        writer.add(self.buffer[i])?;
      }
      writer.finish()?;
      self.meta.write_byte(bits_required as u8)?;
    }
    self.buffer_size = 0;
    Ok(())
  }
  /// Write a new value. Note that data might not be stored until
  /// [`finish()`](DirectMonotonicWriter::finish) is called.
  ///
  /// # Errors
  /// - Returns an error if values are not provided in order.
  #[inline]
  pub fn add(&mut self, v: i64) -> Result<()> {
    if v < self.previous {
      return Err(LuceneError::illegal_argument(format!(
        "Values do not come in order: {}, {}",
        self.previous, v
      )));
    }
    if self.buffer_size == self.buffer.len() {
      self.flush()?;
    }
    self.buffer[self.buffer_size] = v;
    self.buffer_size += 1;
    self.previous = v;
    self.count += 1;
    Ok(())
  }
  /// This must be called exactly once after all values have been added using
  /// [`add(i64)`](DirectMonotonicWriter::add).
  pub fn finish(&mut self) -> Result<()> {
    if self.count != self.num_values {
      return Err(LuceneError::illegal_state(format!(
        "Wrong number of values added, expected: {}, got: {}",
        self.num_values, self.count
      )));
    }
    if self.finished {
      return Err(LuceneError::illegal_state(String::from(
        "#finish has been called already",
      )));
    }
    if self.buffer_size > 0 {
      self.flush()?;
    }
    self.finished = true;
    Ok(())
  }
  /// Returns an instance suitable for encoding `num_values` into monotonic
  /// blocks of 2<sup>`block_shift`</sup> values. Metadata will be written
  /// to `meta_out` and actual data to `data_out`.
  pub fn get_instance(
    meta_out: &'a mut I1,
    data_out: &'a mut I2,
    num_values: i64,
    block_shift: i32,
  ) -> Result<Self> {
    Self::new(meta_out, data_out, num_values, block_shift)
  }
}

pub const MIN_BLOCK_SHIFT: i32 = 2;
pub const MAX_BLOCK_SHIFT: i32 = 22;
