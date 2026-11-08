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
use crate::core::store::DataOutput;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::packed::Format::Packed;
use crate::core::util::packed::block_packed_writer::BlockPackedWriter;
use crate::core::util::packed::{Encoder, FormatBehavior, PackedImpl, PackedInts};

pub(crate) const MIN_BLOCK_SIZE: i32 = 64;
pub(crate) const MAX_BLOCK_SIZE: i32 = 1 << (30 - 3);
pub(crate) const MIN_VALUE_EQUALS_0: i32 = 1 << 0;
pub(crate) const BPV_SHIFT: i32 = 1;
pub(crate) struct AbstractBlockPackedWriter<D> {
  block_size: usize,
  values: Vec<i64>,
  blocks: Vec<u8>,
  off: usize,
  ord: usize,
  finished: bool,
  sub_writer: D,
}

impl<D: AbstractBlockPackedWriterBase> AbstractBlockPackedWriter<D> {
  /// Constructs a new `AbstractBlockPackedWriter`.
  ///
  /// # Arguments
  ///
  /// * `out` - The output stream.
  /// * `block_size` - The number of values in a single block, must be a
  ///   multiple of 64.
  ///
  /// # Errors
  ///
  /// Returns an error if `block_size` is not valid.
  pub fn new(block_size: usize, sub_writer: D) -> Result<Self> {
    PackedInts::check_block_size(
      i32::try_from(block_size).map_err(|_| {
        LuceneError::illegal_argument(format!(
          "block_size must be >= {MIN_BLOCK_SIZE} and <= {MAX_BLOCK_SIZE}, got {block_size}"
        ))
      })?,
      MIN_BLOCK_SIZE,
      MAX_BLOCK_SIZE,
    )?;

    Ok(Self {
      block_size,
      values: Vec::new(),
      blocks: Vec::new(),
      off: 0,
      ord: 0,
      finished: false,
      sub_writer,
    })
  }

  /// Resets the writer with a new output.
  ///
  /// # Arguments
  ///
  /// * `out` - The new output stream.
  pub fn reset(&mut self) {
    self.off = 0;
    self.ord = 0;
    self.finished = false;
  }
  fn ensure_values(&mut self) {
    if self.values.is_empty() {
      self.values.resize(self.block_size, 0);
    }
  }

  fn check_not_finished(&self) -> Result<()> {
    if self.finished {
      return Err(LuceneError::illegal_state("Already finished"));
    }
    Ok(())
  }

  /// Appends a new `i64` value to the writer.
  ///
  /// # Arguments
  ///
  /// * `value` - The value to append.
  ///
  /// # Errors
  ///
  /// Returns an error if the writer has already finished or if flushing
  /// fails.
  pub fn add<DO>(&mut self, value: i64, out: &mut DO) -> Result<()>
  where
    DO: DataOutput,
  {
    self.sub_writer.add(value);
    self.check_not_finished()?;
    self.ensure_values();
    if self.off == self.values.len() {
      self
        .sub_writer
        .flush(out, &mut self.off, &mut self.values, &mut self.blocks)?;
    }
    self.values[self.off] = value;
    self.off += 1;
    self.ord += 1;
    Ok(())
  }
  /// Adds a block of zeros to the writer (for testing only).
  ///
  /// # Errors
  ///
  /// Returns an error if the writer has already finished or the offset is
  /// invalid.
  #[cfg(test)]
  #[cfg_attr(not(feature = "nightly"), allow(dead_code))]
  pub(crate) fn add_block_of_zeros<DO>(&mut self, out: &mut DO) -> Result<()>
  where
    DO: DataOutput,
  {
    self.check_not_finished()?;
    self.ensure_values();
    if self.off != 0 && self.off != self.values.len() {
      return Err(LuceneError::illegal_state(format!("{}", self.off)));
    }
    if self.off == self.values.len() {
      self
        .sub_writer
        .flush(out, &mut self.off, &mut self.values, &mut self.blocks)?;
    }
    self.values.fill(0);
    debug_assert!(self.values.len() <= i32::MAX as usize);
    self.off = self.values.len();
    self.ord += self.values.len();
    Ok(())
  }
  /// Flushes all buffered data to the output stream. After calling this
  /// method, this instance is no longer usable until `reset` is called.
  ///
  /// # Errors
  ///
  /// Returns an error if the writer has already finished or if flushing
  /// fails.
  pub fn finish<DO>(&mut self, out: &mut DO) -> Result<()>
  where
    DO: DataOutput,
  {
    self.check_not_finished()?;
    if self.off > 0 {
      self
        .sub_writer
        .flush(out, &mut self.off, &mut self.values, &mut self.blocks)?;
    }
    self.finished = true;
    Ok(())
  }
  /// Returns the number of values that have been added.
  #[cfg(test)]
  pub fn ord(&self) -> usize {
    self.ord
  }
}
impl AbstractBlockPackedWriter<BlockPackedWriter> {
  pub(crate) fn add_position_deltas<DO>(&mut self, positions: &[i32], out: &mut DO) -> Result<()>
  where
    DO: DataOutput,
  {
    if let Some(&first) = positions.first() {
      self.add(i64::from(first), out)?;
      let mut positions = positions;
      while positions.len() > 1 {
        if self.off == self.values.len() {
          self
            .sub_writer
            .flush(out, &mut self.off, &mut self.values, &mut self.blocks)?;
        }

        let count = (positions.len() - 1).min(self.values.len() - self.off);
        let end = self.off + count;
        for (value, pair) in self.values[self.off..end]
          .iter_mut()
          .zip(positions[..=count].windows(2))
        {
          *value = i64::from(pair[1].wrapping_sub(pair[0]));
        }
        self.off = end;
        self.ord += count;
        positions = &positions[count..];
      }
    }
    Ok(())
  }

  pub(crate) fn add_start_offset_residuals<DO>(
    &mut self,
    positions: &[i32],
    offsets: &[[i32; 2]],
    chars_per_term: f32,
    out: &mut DO,
  ) -> Result<()>
  where
    DO: DataOutput,
  {
    if let Some(&first_position) = positions.first() {
      let predicted = (chars_per_term * first_position as f32) as i32;
      self.add(i64::from(offsets[0][0].wrapping_sub(predicted)), out)?;

      let mut positions = positions;
      let mut offsets = offsets;
      while positions.len() > 1 {
        if self.off == self.values.len() {
          self
            .sub_writer
            .flush(out, &mut self.off, &mut self.values, &mut self.blocks)?;
        }

        let count = (positions.len() - 1).min(self.values.len() - self.off);
        let end = self.off + count;
        for ((value, positions), offsets) in self.values[self.off..end]
          .iter_mut()
          .zip(positions[..=count].windows(2))
          .zip(offsets[..=count].windows(2))
        {
          let position_delta = positions[1].wrapping_sub(positions[0]);
          let predicted = (chars_per_term * position_delta as f32) as i32;
          let offset_delta = offsets[1][0].wrapping_sub(offsets[0][0]);
          *value = i64::from(offset_delta.wrapping_sub(predicted));
        }
        self.off = end;
        self.ord += count;
        positions = &positions[count..];
        offsets = &offsets[count..];
      }
    }
    Ok(())
  }

  pub(crate) fn add_term_freqs<DO>(&mut self, freqs: &[i32], out: &mut DO) -> Result<()>
  where
    DO: DataOutput,
  {
    if freqs.is_empty() {
      return Ok(());
    }
    self.check_not_finished()?;

    let mut freqs = freqs;
    while !freqs.is_empty() {
      if self.off == self.values.len() {
        self
          .sub_writer
          .flush(out, &mut self.off, &mut self.values, &mut self.blocks)?;
      }

      let count = freqs.len().min(self.values.len() - self.off);
      let (input, remaining) = freqs.split_at(count);
      let end = self.off + count;
      for (value, &freq) in self.values[self.off..end].iter_mut().zip(input) {
        *value = i64::from(freq.wrapping_sub(1));
      }
      self.off = end;
      self.ord += count;
      freqs = remaining;
    }
    Ok(())
  }

  pub(crate) fn add_term_lengths<DO>(&mut self, lengths: &[usize], out: &mut DO) -> Result<()>
  where
    DO: DataOutput,
  {
    if lengths.is_empty() {
      return Ok(());
    }
    self.check_not_finished()?;

    let mut lengths = lengths;
    while !lengths.is_empty() {
      if self.off == self.values.len() {
        self
          .sub_writer
          .flush(out, &mut self.off, &mut self.values, &mut self.blocks)?;
      }

      let count = lengths.len().min(self.values.len() - self.off);
      let (input, remaining) = lengths.split_at(count);
      let end = self.off + count;
      for (value, &length) in self.values[self.off..end].iter_mut().zip(input) {
        *value = length as i64;
      }
      self.off = end;
      self.ord += count;
      lengths = remaining;
    }
    Ok(())
  }
}

/// Encodes and writes the current values to the output stream.
///
/// # Arguments
///
/// * `bits_required` - The number of bits required for encoding the values.
///
/// # Errors
///
/// Returns an error if writing to the output stream fails.
#[inline(never)]
pub(crate) fn write_values<DO>(
  bits_required: i32,
  out: &mut DO,
  blocks: &mut Vec<u8>,
  values: &mut [i64],
  off: usize,
) -> Result<()>
where
  DO: DataOutput,
{
  let encoder = PackedInts::get_encoder(
    Packed(PackedImpl::new(0)),
    PackedInts::VERSION_CURRENT,
    bits_required,
  )?;
  let iterations = values.len() / Encoder::byte_value_count(encoder);
  let block_size = Encoder::byte_block_count(encoder) * iterations;
  ArrayUtil::grow_no_copy(blocks, block_size, || 0)?;
  if off < values.len() {
    for value in values.iter_mut().skip(off) {
      *value = 0;
    }
  }
  debug_assert!(iterations <= i32::MAX as usize);
  encoder.encode_i64_to_u8(values, 0, blocks, 0, iterations);
  let block_count =
    Packed(PackedImpl::new(0)).byte_count(PackedInts::VERSION_CURRENT, off, bits_required);
  out.write_bytes_with_len(blocks, block_count)?;
  Ok(())
}
/// Same as DataOutput::writeVLong but accepts negative values.
pub(crate) fn write_vlong<DO>(out: &mut DO, mut i: i64) -> Result<()>
where
  DO: DataOutput,
{
  let mut k = 0;
  while (i & !0x7F) != 0 && k < 8 {
    out.write_byte(((i & 0x7F) | 0x80) as u8)?;
    i >>= 7;
    k += 1;
  }
  out.write_byte(i as u8)?;
  Ok(())
}
pub(crate) trait AbstractBlockPackedWriterBase {
  fn flush<DO>(
    &mut self,
    out: &mut DO,
    off: &mut usize,
    values: &mut [i64],
    blocks: &mut Vec<u8>,
  ) -> Result<()>
  where
    DO: DataOutput;
  fn add(&mut self, _value: i64) {}
}
