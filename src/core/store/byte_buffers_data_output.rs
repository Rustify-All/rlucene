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
use std::collections::VecDeque;
use std::io::Cursor;
use std::sync::atomic::{AtomicI64, Ordering};

use byteorder::WriteBytesExt;

use crate::core::store::DataInput;
use crate::core::store::byte_buffers_data_input::{
  ByteBuffersDataInput, ByteBuffersDataInputOwned,
};
use crate::core::store::data_output::DataOutput;
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::ram_usage_estimator::{size_of_vec, size_of_vec_deque};
use crate::core::util::{ReadableCursorExt, TryIntoInt, WritableCursorExt};

/// A [`DataOutput`] storing data in a list of [`Cursor<Vec<u8>>`](Cursor).
pub struct ByteBuffersDataOutput {
  // All data within each block is considered valid.
  blocks: VecDeque<Cursor<Vec<u8>>>,
  max_bits_per_block: i32,
  block_bits: i32,
  // it is necessary when we want to reuse the data output
  current_block_index: usize,
  reuse: bool,
  cached_ram_bytes: AtomicI64,
}

impl Default for ByteBuffersDataOutput {
  fn default() -> Self {
    Self::new()
  }
}

impl ByteBuffersDataOutput {
  /// Smallest `minBitsPerBlock` allowed
  pub const LIMIT_MIN_BITS_PER_BLOCK: i32 = 1;
  /// Largest `maxBitsPerBlock` allowed
  pub const LIMIT_MAX_BITS_PER_BLOCK: i32 = 31;
  ///Maximum number of blocks at the current `blockBits` block size before we
  /// increase the block size (and thus decrease the number of blocks).
  pub const MAX_BLOCKS_BEFORE_BLOCK_EXPANSION: usize = 100;
  ///Default `maxBitsPerBlock`
  pub const DEFAULT_MAX_BITS_PER_BLOCK: i32 = 26;
  /// Default `minBitsPerBlock`
  pub const DEFAULT_MIN_BITS_PER_BLOCK: i32 = 10;

  pub fn new() -> Self {
    Self::with_validated_bits(
      Self::DEFAULT_MIN_BITS_PER_BLOCK,
      Self::DEFAULT_MAX_BITS_PER_BLOCK,
      false,
    )
  }
  ///Creates a new output with all defaults.
  pub fn new_resettable_instance() -> Self {
    Self::with_validated_bits(
      Self::DEFAULT_MIN_BITS_PER_BLOCK,
      Self::DEFAULT_MAX_BITS_PER_BLOCK,
      true,
    )
  }
  /// Expert: Creates a new output with custom parameters.
  ///
  /// # Arguments
  /// * `min_bits_per_block` - Minimum bits per block.
  /// * `max_bits_per_block` - Maximum bits per block.
  /// * `reuse` - Reuse this Instance.
  pub fn with_reuse(min_bits_per_block: i32, max_bits_per_block: i32, reuse: bool) -> Result<Self> {
    if min_bits_per_block < Self::LIMIT_MIN_BITS_PER_BLOCK {
      return Err(LuceneError::illegal_argument(format!(
        "minBitsPerBlock ({}) too small, must be at least {}",
        min_bits_per_block,
        Self::LIMIT_MIN_BITS_PER_BLOCK
      )));
    }
    if max_bits_per_block > Self::LIMIT_MAX_BITS_PER_BLOCK {
      return Err(LuceneError::illegal_argument(format!(
        "maxBitsPerBlock ({}) too large, must not exceed {}",
        max_bits_per_block,
        Self::LIMIT_MAX_BITS_PER_BLOCK
      )));
    }
    if min_bits_per_block > max_bits_per_block {
      return Err(LuceneError::illegal_argument(format!(
        "minBitsPerBlock ({min_bits_per_block}) cannot exceed maxBitsPerBlock ({max_bits_per_block})"
      )));
    }
    Ok(Self::with_validated_bits(
      min_bits_per_block,
      max_bits_per_block,
      reuse,
    ))
  }

  fn with_validated_bits(min_bits_per_block: i32, max_bits_per_block: i32, reuse: bool) -> Self {
    Self {
      max_bits_per_block,
      block_bits: min_bits_per_block,
      blocks: VecDeque::new(),
      current_block_index: 0,
      reuse,
      cached_ram_bytes: AtomicI64::new(-1),
    }
  }
  /// Creates a new output, suitable for writing a file of approximately
  /// `expected_size` bytes.
  ///
  /// Memory allocation will be optimized based on the `expected_size` hint to
  /// reduce overhead for larger files.
  ///
  /// # Arguments
  /// * `expected_size` - Estimated size of the output file.
  pub fn with_size(expected_size: i64) -> Result<Self> {
    let block_bits = compute_block_size_bits_for(expected_size);
    Self::with_reuse(block_bits, Self::DEFAULT_MAX_BITS_PER_BLOCK, false)
  }

  fn append_block(&mut self) -> Result<()> {
    self.cached_ram_bytes.store(-1, Ordering::Relaxed);
    if self.blocks.len() > Self::MAX_BLOCKS_BEFORE_BLOCK_EXPANSION
      && self.block_bits < self.max_bits_per_block
    {
      self.rewrite_to_block_size(self.block_bits + 1)?;
      let block_count = self.blocks.len();
      let current_block = self
        .blocks
        .get_mut(self.current_block_index)
        .ok_or_else(|| {
          LuceneError::illegal_state(format!(
            "current block index {} is out of bounds for {} blocks",
            self.current_block_index, block_count
          ))
        })?;
      if current_block.remain()? > 0 {
        return Ok(());
      }
    }
    let required_block_size = 1 << self.block_bits;
    self
      .blocks
      .push_back(Cursor::new(vec![0u8; required_block_size]));
    self.current_block_index += 1;
    Ok(())
  }
  fn rewrite_to_block_size(&mut self, target_block_bits: i32) -> Result<()> {
    debug_assert!(target_block_bits <= self.max_bits_per_block);
    self.rewrite_blocks(target_block_bits)?;
    Ok(())
  }
  // create larger blocks and copy data from smaller blocks
  // TODO: the first old_block's data could be reused ,first do expansion by
  // `push_back` and then move to tail and continue copy the second
  // old_block's data to it
  pub fn rewrite_blocks(&mut self, target_block_bits: i32) -> Result<()> {
    self.cached_ram_bytes.store(-1, Ordering::Relaxed);
    debug_assert!(target_block_bits > self.block_bits);
    self.block_bits = target_block_bits;
    let block_size = 1 << self.block_bits;
    let mut new_block = Cursor::new(vec![0; block_size]);
    let mut old_block_count = self.blocks.len();
    while let Some(mut old_block) = self.blocks.pop_front() {
      // read from head
      old_block.set_position(0);
      if new_block.remain()? == 0 && old_block.get_ref().len() <= block_size {
        self.blocks.push_back(new_block);
        // This block has a unique owner. Grow it into the next destination
        // instead of copying its existing bytes into a newly allocated block.
        let old_length = old_block.get_ref().len();
        let mut bytes = old_block.into_inner();
        bytes.reserve_exact(block_size - old_length);
        // All source blocks have the same physical size. Append whole blocks
        // before initializing the unused tail of the final destination.
        while old_block_count > 1 && bytes.len() + old_length <= block_size {
          let following = self
            .blocks
            .pop_front()
            .ok_or_else(|| LuceneError::illegal_state("rewrite source block is missing"))?;
          bytes.extend_from_slice(following.get_ref());
          old_block_count -= 1;
        }
        let position = bytes.len();
        bytes.resize(block_size, 0);
        new_block = Cursor::new(bytes);
        new_block.set_position(position as u64);
      } else {
        while old_block.remain()? > 0 {
          let mut available_space = new_block.remain()?;
          if available_space == 0 {
            self.blocks.push_back(new_block);
            new_block = Cursor::new(vec![0; block_size]);
            available_space = 1 << self.block_bits;
          }
          let bytes_to_copy = available_space.min(old_block.remain()?);
          let old_position = old_block.position() as usize;
          let old_data = &old_block.get_ref()[old_position..old_position + bytes_to_copy];
          debug_assert!(
            new_block.remain()? >= bytes_to_copy,
            "Insufficient space in new_block: remaining={}, required={}",
            new_block.remain()?,
            bytes_to_copy
          );
          new_block.write_from_slice(old_data)?;
          old_block.set_position((old_position + bytes_to_copy) as u64);
        }
      }
      old_block_count -= 1;
      if old_block_count == 0 {
        break;
      }
    }
    if new_block.position() > 0 {
      self.blocks.push_back(new_block);
    }
    self.current_block_index = self.blocks.len() - 1;
    Ok(())
  }
  /// Copies the current content of this object into another [`DataOutput`].
  pub(crate) fn copy_to<DA>(&self, output: &mut DA) -> Result<()>
  where
    DA: DataOutput,
  {
    for (index, block) in self
      .blocks
      .iter()
      .take(self.current_block_index + 1)
      .enumerate()
    {
      if index == self.current_block_index {
        let end = block.position() as usize;
        output.write_bytes_range(block.get_ref(), 0, end)?;
      } else {
        // this block is reused
        if block.position() == 0 {
          continue;
        }
        let len = block.get_ref().len();
        debug_assert!(len == 1 << self.block_bits);
        output.write_bytes_with_len(block.get_ref(), len)?;
      }
    }
    Ok(())
  }
  /// The number of bytes written to this output so far.
  #[inline]
  pub fn size(&self) -> usize {
    if self.blocks.is_empty() {
      return 0;
    }
    let block_count = self.current_block_index + 1;
    let full_block_size = (block_count - 1) * self.block_size();
    let last_block = expect_invariant!(
      self.blocks.get(self.current_block_index),
      "a non-empty byte buffers output keeps its current block index within the allocated blocks",
    );
    full_block_size + last_block.position() as usize
  }
  fn block_size(&self) -> usize {
    1 << self.block_bits
  }
  /// Resets this object to a clean (zero-size) state and publishes any
  /// currently allocated buffers for reuse according to the reuse
  /// strategy provided when this value was created.
  ///
  /// # Warning
  /// Sharing byte buffers for reads and writes is dangerous and may lead to
  /// hard-to-debug issues. Use with great caution.
  #[inline]
  pub fn reset(&mut self) {
    self.cached_ram_bytes.store(-1, Ordering::Relaxed);
    if self.reuse {
      for block in self.blocks.iter_mut().take(self.current_block_index + 1) {
        block.set_position(0);
      }
    } else {
      self.blocks = VecDeque::new();
    }
    self.current_block_index = 0;
  }

  pub(crate) fn buffer_slices(&self) -> impl Iterator<Item = &[u8]> {
    self.blocks.iter().map(|block| block.get_ref().as_slice())
  }

  /// Returns a list of read-only views of [`Cursor<Vec<u8>>`](Cursor) blocks
  /// over the current content written to the output.
  pub fn to_buffer_list_ref(&self) -> (usize, Vec<Cursor<&[u8]>>) {
    let mut data: Vec<_> = self
      .blocks
      .iter()
      .map(|cursor| {
        let slice: &[u8] = cursor.get_ref().as_slice();
        let mut new_cursor = Cursor::new(slice);
        new_cursor.set_position(0);
        new_cursor
      })
      .collect();
    if data.is_empty() {
      data.push(Cursor::new(&[]));
    }
    (self.size(), data)
  }
  /// Moves the blocks out of the current object, transferring ownership.
  /// # Parameters
  /// - `init_blocks`: If init_blocks is true, then after taking ownership of blocks, we pre-allocate the space so it can be reused.
  pub fn to_buffer_list_owner(&mut self, init_blocks: bool) -> (usize, Vec<Cursor<Vec<u8>>>) {
    self.cached_ram_bytes.store(-1, Ordering::Relaxed);
    let size = self.size();

    let old_blocks = {
      if init_blocks {
        let cap = self.blocks.capacity();
        let len = self.blocks.len();

        let mut new_blocks = VecDeque::with_capacity(cap);
        for _ in 0..len {
          new_blocks.push_back(Cursor::new(vec![0u8; 1 << self.block_bits]));
        }
        std::mem::replace(&mut self.blocks, new_blocks)
      } else {
        std::mem::take(&mut self.blocks)
      }
    };

    let mut data: Vec<_> = old_blocks.into();
    if data.is_empty() {
      data.push(Cursor::new(Vec::new()));
    }
    for cursor in &mut data {
      cursor.set_position(0);
    }

    (size, data)
  }
  pub(crate) fn take_raw_blocks(&mut self) -> impl ExactSizeIterator<Item = Vec<u8>> + use<> {
    self.cached_ram_bytes.store(-1, Ordering::Relaxed);
    std::mem::take(&mut self.blocks)
      .into_iter()
      .map(Cursor::into_inner)
  }

  pub fn get_writeable_buffer_list(&mut self) -> Vec<&mut Cursor<Vec<u8>>> {
    todo!()
  }
  /// Returns a contiguous array containing the current content written to the
  /// output. The returned array is always a copy and can be safely
  /// mutated. # Note
  /// If reset is called immediately after get_array_copy,
  /// or if ByteBuffersDataOutput will no longer be used,
  /// then [`try_get_array_ownership`](Self::try_get_array_ownership) should
  /// be used instead. If the number of blocks is 1, we take ownership to
  /// avoid copying. See
  /// [`try_get_array_ownership`](Self::try_get_array_ownership)
  pub fn get_array_copy(&self) -> Vec<u8> {
    let mut buffer = Vec::with_capacity(self.size());
    for block in &self.blocks {
      let end = block.position() as usize;
      buffer.extend_from_slice(&block.get_ref()[..end]);
    }
    buffer
  }
  /// Transfers a sole block from a finished output, or gathers multiple blocks.
  pub(crate) fn into_array(mut self) -> Vec<u8> {
    if self.blocks.len() == 1
      && let Some(block) = self.blocks.pop_front()
    {
      let end = block.position() as usize;
      let mut bytes = block.into_inner();
      bytes.truncate(end);
      return bytes;
    }
    self.get_array_copy()
  }

  /// See [`get_array_copy`](Self::get_array_copy) Before use this method.
  pub fn try_get_array_ownership(&mut self) -> Vec<u8> {
    self.cached_ram_bytes.store(-1, Ordering::Relaxed);
    match self.blocks.len() {
      0 => Vec::new(),
      // If the number of blocks is 1, take ownership to avoid copying.
      1 => {
        let cursor = expect_invariant!(
          self.blocks.front_mut(),
          "a byte buffers output with exactly one allocated block has a front block",
        );
        let end = cursor.position() as usize;

        let old_vec = std::mem::replace(cursor.get_mut(), vec![0u8; 1 << self.block_bits]);

        old_vec.into_iter().take(end).collect()
      },
      _ => {
        let mut buffer = Vec::with_capacity(self.size());
        for block in &self.blocks {
          let end = block.position() as usize;
          buffer.extend_from_slice(&block.get_ref()[..end]);
        }
        buffer
      },
    }
  }

  /// Returns a [`ByteBuffersDataInput`] backed by references to internal
  /// buffers.
  ///
  /// This method borrows the internal buffer data as `&[u8]`,
  /// and constructs a read-only view over the current written content.
  ///
  /// The returned input is only valid as long as `self` is not mutated.
  pub fn get_data_input_ref(&self) -> Result<ByteBuffersDataInput<&[u8]>> {
    let (length, data) = self.to_buffer_list_ref();
    ByteBuffersDataInput::new(data, length)
  }

  /// Returns a [`ByteBuffersDataInput`] that owns its internal buffers.
  ///
  /// This method consumes the written buffer content into owned `[u8]`
  /// vectors, and constructs a self-contained input stream that can
  /// outlive `self`.
  ///
  /// Use this when the data needs to be retained or passed independently.
  /// # Parameters
  /// - `init_blocks`: If init_blocks is true, then after taking ownership of blocks, we pre-allocate the space so it can be reused.
  pub fn get_data_input_owner(&mut self, init_blocks: bool) -> Result<ByteBuffersDataInputOwned> {
    let (length, data) = self.to_buffer_list_owner(init_blocks);
    ByteBuffersDataInput::new(data, length)
  }

  fn append_block_if_needed(&mut self) -> Result<usize> {
    if self.blocks.is_empty() {
      self.cached_ram_bytes.store(-1, Ordering::Relaxed);
      self.blocks.reserve_exact(1);
      self
        .blocks
        .push_back(Cursor::new(vec![0u8; 1 << self.block_bits]));
      self.current_block_index = 0;
    }
    let mut last_block = self
      .blocks
      .get_mut(self.current_block_index)
      .ok_or_else(|| {
        LuceneError::illegal_state(format!(
          "current block index {} is out of bounds",
          self.current_block_index
        ))
      })?;
    if last_block.remain()? == 0 {
      if self.reuse && self.current_block_index < self.blocks.len() - 1 {
        self.current_block_index += 1;
        last_block = self
          .blocks
          .get_mut(self.current_block_index)
          .ok_or_else(|| {
            LuceneError::illegal_state(format!(
              "reused block index {} is out of bounds",
              self.current_block_index
            ))
          })?;
      } else {
        self.append_block()?;
        // it is safe to get by `back_mut` because blocks are not reused
        last_block = self
          .blocks
          .back_mut()
          .ok_or_else(|| LuceneError::illegal_state("appended block is missing"))?;
      }
    }
    last_block.remain()
  }
  pub fn write_bytes(&mut self, b: &[u8]) -> Result<()> {
    debug_assert!(b.len() <= u32::MAX as usize);
    self.write_bytes_range(b, 0, b.len())
  }

  pub fn write_byte(&mut self, b: u8) -> Result<()> {
    self.write_bytes_range(&[b], 0, 1)
  }
}

impl DataOutput for ByteBuffersDataOutput {
  #[inline]
  fn write_byte(&mut self, b: u8) -> Result<()> {
    self.append_block_if_needed()?;
    let last_block = self
      .blocks
      .get_mut(self.current_block_index)
      .ok_or_else(|| LuceneError::illegal_state("current write block is missing"))?;
    Ok(last_block.write_u8(b)?)
  }

  #[inline]
  fn write_vint(&mut self, i: i32) -> Result<()> {
    let mut value = i as u32;
    if value < 0x80 {
      return <Self as DataOutput>::write_byte(self, value as u8);
    }
    let mut bytes = [0u8; 5];
    let mut length = 0;
    while (value & !0x7F) != 0 {
      bytes[length] = ((value & 0x7F) | 0x80) as u8;
      length += 1;
      value >>= 7;
    }
    bytes[length] = value as u8;
    self.write_bytes_range(&bytes, 0, length + 1)
  }

  fn write_signed_vlong(&mut self, i: i64) -> Result<()> {
    let mut value = i as u64;
    if (value & !0x7F) == 0 {
      return <Self as DataOutput>::write_byte(self, value as u8);
    }
    let mut bytes = [0u8; 10];
    let mut length = 0;
    while (value & !0x7F) != 0 {
      bytes[length] = ((value & 0x7F) | 0x80) as u8;
      length += 1;
      value >>= 7;
    }
    bytes[length] = value as u8;
    self.write_bytes_range(&bytes, 0, length + 1)
  }

  #[inline]
  fn write_bytes_with_len(&mut self, b: &[u8], len: usize) -> Result<()> {
    self.write_bytes_range(b, 0, len)
  }

  #[inline]
  fn write_bytes_range(&mut self, b: &[u8], mut offset: usize, mut length: usize) -> Result<()> {
    while length > 0 {
      let available_space = self.append_block_if_needed()?;
      let last_block = self
        .blocks
        .get_mut(self.current_block_index)
        .ok_or_else(|| LuceneError::illegal_state("current write block is missing"))?;
      let chunk = available_space.min(length);
      last_block.write_from(b, offset, chunk)?;
      length -= chunk;
      offset += chunk;
    }
    Ok(())
  }

  #[inline]
  fn write_int(&mut self, i: i32) -> Result<()> {
    let value = i.to_le_bytes();
    self.write_bytes_range(&value, 0, 4)
  }

  #[inline(always)]
  fn write_ints(&mut self, values: &[i32]) -> Result<()> {
    if values.len() < 4 {
      for &value in values {
        self.write_int(value)?;
      }
      return Ok(());
    }
    let (chunks, remainder) = values.as_chunks::<4>();
    for chunk in chunks {
      if let Some(block) = self.blocks.get_mut(self.current_block_index)
        && block.remain()? >= 16
      {
        let mut bytes = [0u8; 16];
        for (encoded, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(chunk) {
          encoded.copy_from_slice(&value.to_le_bytes());
        }
        let position = block.position() as usize;
        let end = position + bytes.len();
        block.get_mut()[position..end].copy_from_slice(&bytes);
        block.set_position(end as u64);
      } else {
        for &value in chunk {
          self.write_int(value)?;
        }
      }
    }
    for &value in remainder {
      self.write_int(value)?;
    }
    Ok(())
  }

  fn write_short(&mut self, i: i16) -> Result<()> {
    let value = i.to_le_bytes();
    self.write_bytes_range(&value, 0, 2)
  }

  #[inline]
  fn write_long(&mut self, i: i64) -> Result<()> {
    let value = i.to_le_bytes();
    self.write_bytes_range(&value, 0, 8)
  }

  fn write_string(&mut self, s: &str) -> Result<()> {
    let bytes = s.as_bytes();
    let length = bytes.len();
    self.write_vint(length.try_convert()?)?;
    self.write_bytes_range(bytes, 0, length)
  }

  fn copy_bytes<I>(&mut self, input: &mut I, mut num_bytes: usize) -> Result<()>
  where
    I: DataInput + ?Sized,
  {
    while num_bytes > 0 {
      let available_space = self.append_block_if_needed()?;
      let last_block = self
        .blocks
        .get_mut(self.current_block_index)
        .ok_or_else(|| LuceneError::illegal_state("current write block is missing"))?;
      let bytes_to_copy = available_space.min(num_bytes);

      let current_pos = last_block.position().try_convert()?;
      let current_block_mut = last_block.get_mut();
      input.read_bytes(current_block_mut, current_pos, bytes_to_copy)?;
      last_block.set_position((current_pos + bytes_to_copy).try_convert()?);
      num_bytes -= bytes_to_copy;
    }
    Ok(())
  }
}

impl Accountable for ByteBuffersDataOutput {
  fn ram_bytes_used(&self) -> Result<i64> {
    let cached = self.cached_ram_bytes.load(Ordering::Relaxed);
    if cached >= 0 {
      return Ok(cached);
    }
    let mut size = size_of_vec_deque(&self.blocks);
    for block in &self.blocks {
      size = size.saturating_add(size_of_vec(block.get_ref()));
    }
    self.cached_ram_bytes.store(size, Ordering::Relaxed);
    Ok(size)
  }
}

fn compute_block_size_bits_for(bytes: i64) -> i32 {
  let avg_block_size =
    (bytes / ByteBuffersDataOutput::MAX_BLOCKS_BEFORE_BLOCK_EXPANSION as i64) as u64;
  let power_of_two = avg_block_size.next_power_of_two();
  if power_of_two == 0 {
    return ByteBuffersDataOutput::DEFAULT_MIN_BITS_PER_BLOCK;
  }
  let mut block_bits = power_of_two.trailing_zeros() as i32;
  block_bits = block_bits.min(ByteBuffersDataOutput::DEFAULT_MAX_BITS_PER_BLOCK);
  block_bits = block_bits.max(ByteBuffersDataOutput::DEFAULT_MIN_BITS_PER_BLOCK);
  block_bits
}
