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

use crate::core::store::data_input_ext::DataInputExt;
use std::fmt::{Display, Formatter};
use std::rc::Rc;

use crate::core::store::{ByteBuffersDataOutput, DataInput, DataOutput};
use crate::core::util::accountable::Accountable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fst_impl::fst::{BytesReader, BytesReaderEnum2};
use crate::core::util::fst_impl::fst_reader::FstReader;
use crate::core::util::fst_impl::reverse_bytes_reader::ReverseBytesReader;
use crate::core::util::group_vint_util::GroupVIntUtil;
use crate::core::util::ram_usage_estimator::size_of_vec;

/// An adapter struct to use [`ByteBuffersDataOutput`] as a
/// [`FSTReader`](FstReader). It allows the FST to be readable immediately after
/// freezing the written bytes.
pub struct ReadWriteDataOutput {
  pub data_output: ByteBuffersDataOutput,
  pub block_bits: i32,
  pub block_size: i32,
  pub block_mask: i32,
  pub byte_buffers: Option<Rc<Vec<Vec<u8>>>>,
  pub byte_buffer: Option<Rc<Vec<u8>>>,
  pub frozen: bool,
  pub contiguous_reader: bool,
  /// Indicates whether the byte_buffer/byte_buffers have been initialized.
  pub finish: bool,
  // The written length remains valid after the reader takes the buffer owner.
  num_bytes: usize,
}
/// # Warning
/// Padding Implement for std::mem::replace
impl Default for ReadWriteDataOutput {
  fn default() -> Self {
    Self {
      data_output: ByteBuffersDataOutput::new(),
      block_bits: 0,
      block_size: 0,
      block_mask: 0,
      byte_buffers: None,
      byte_buffer: None,
      frozen: true,
      contiguous_reader: false,
      finish: true,
      num_bytes: 0,
    }
  }
}

impl ReadWriteDataOutput {
  pub(crate) fn new(block_bits: i32, contiguous_reader: bool) -> Result<Self> {
    let block_size = 1 << block_bits;
    let block_mask = block_size - 1;
    let data_output = ByteBuffersDataOutput::with_reuse(block_bits, block_bits, false)?;
    Ok(Self {
      data_output,
      block_bits,
      block_size,
      block_mask,
      byte_buffers: None,
      byte_buffer: None,
      frozen: false,
      contiguous_reader,
      finish: false,
      num_bytes: 0,
    })
  }

  pub fn freeze(&mut self) -> Result<()> {
    self.frozen = true;
    // Prepare the immutable backing once, at the same boundary as Java freeze().
    self.finish = true;
    if self.byte_buffer.is_none() && self.byte_buffers.is_none() {
      self.num_bytes = self.data_output.size();
      if self.contiguous_reader {
        let output = std::mem::take(&mut self.data_output);
        self.byte_buffer = Some(Rc::new(output.into_array()));
      } else {
        let mut blocks = self.data_output.take_raw_blocks();
        if blocks.len() <= 1 {
          self.byte_buffer = Some(Rc::new(blocks.next().unwrap_or_default()));
        } else {
          self.byte_buffers = Some(Rc::new(blocks.collect()));
        }
      }
    }
    Ok(())
  }
}

impl Accountable for ReadWriteDataOutput {
  fn ram_bytes_used(&self) -> Result<i64> {
    if let Some(byte_buffer) = &self.byte_buffer {
      return Ok(
        (std::mem::size_of_val(byte_buffer.as_ref()) as i64)
          .saturating_add(size_of_vec(byte_buffer.as_ref())),
      );
    }
    if let Some(byte_buffers) = &self.byte_buffers {
      let mut size = (std::mem::size_of_val(byte_buffers.as_ref()) as i64)
        .saturating_add(size_of_vec(byte_buffers.as_ref()));
      for byte_buffer in byte_buffers.iter() {
        size = size.saturating_add(size_of_vec(byte_buffer));
      }
      return Ok(size);
    }
    self.data_output.ram_bytes_used()
  }
}

impl FstReader for ReadWriteDataOutput {
  type FstBytesReader = BytesReaderEnum2<BytesReaderImpl, ReverseBytesReader>;

  fn get_reverse_bytes_reader(&self) -> Result<Self::FstBytesReader> {
    if !self.finish {
      return Err(LuceneError::illegal_state(
        "Call ReadWriteDataOutput#freeze before Call ReadWriteDataOutput#get_reverse_bytes_reader",
      ));
    }
    match (self.byte_buffers.as_ref(), self.byte_buffer.as_ref()) {
      (Some(byte_buffers), None) => Ok(BytesReaderEnum2::A(BytesReaderImpl::new(
        byte_buffers.clone(),
        self.block_bits,
        self.block_size,
        self.block_mask,
      ))),
      (None, Some(byte_buffer)) => Ok(BytesReaderEnum2::B(ReverseBytesReader::new(
        byte_buffer.clone(),
      ))),
      _ => Err(LuceneError::illegal_state("Only one buffer is some")),
    }
  }

  fn write_to<DO>(&self, out: &mut DO) -> Result<()>
  where
    DO: DataOutput,
  {
    if let Some(buffer) = &self.byte_buffer {
      let mut remaining = self.num_bytes;
      for block in buffer[..self.num_bytes].chunks(self.block_size as usize) {
        if remaining <= self.block_size as usize {
          out.write_bytes_range(block, 0, remaining)?;
        } else {
          out.write_bytes_with_len(block, block.len())?;
        }
        remaining -= block.len();
      }
      Ok(())
    } else if let Some(buffers) = &self.byte_buffers {
      let mut remaining = self.num_bytes;
      for block in buffers.iter() {
        if remaining <= self.block_size as usize {
          out.write_bytes_range(block, 0, remaining)?;
        } else {
          out.write_bytes_with_len(block, block.len())?;
        }
        remaining -= remaining.min(block.len());
      }
      Ok(())
    } else {
      self.data_output.copy_to(out)
    }
  }
}

impl DataOutput for ReadWriteDataOutput {
  #[inline]
  fn write_byte(&mut self, b: u8) -> Result<()> {
    debug_assert!(!self.frozen);
    DataOutput::write_byte(&mut self.data_output, b)
  }

  #[inline]
  fn write_bytes_range(&mut self, b: &[u8], offset: usize, length: usize) -> Result<()> {
    debug_assert!(!self.frozen);
    self.data_output.write_bytes_range(b, offset, length)
  }
}

pub struct BytesReaderImpl {
  byte_buffers: Rc<Vec<Vec<u8>>>,
  next_buffer: Option<usize>,
  next_read: Option<usize>,
  current: usize,
  block_size: i32,
  block_bits: i32,
  block_mask: i32,
}

impl BytesReaderImpl {
  pub fn new(
    byte_buffers: Rc<Vec<Vec<u8>>>,
    block_bits: i32,
    block_size: i32,
    block_mask: i32,
  ) -> Self {
    Self {
      byte_buffers,
      next_buffer: None,
      next_read: Some(0),
      current: 0,
      block_size,
      block_bits,
      block_mask,
    }
  }
}

impl crate::core::util::close::Closeable for BytesReaderImpl {}

impl DataInput for BytesReaderImpl {
  fn read_byte(&mut self) -> Result<u8> {
    if self.next_read.is_none() {
      let next_buffer = self
        .next_buffer
        .ok_or_else(|| LuceneError::illegal_state("reverse reader has no preceding buffer"))?;
      self.current = next_buffer;
      self.next_buffer = next_buffer.checked_sub(1);
      self.next_read = Some((self.block_size - 1) as usize);
    }
    let next_read = self
      .next_read
      .ok_or_else(|| LuceneError::illegal_state("reverse reader position is not initialized"))?;
    let byte = &self.byte_buffers[self.current][next_read];
    self.next_read = next_read.checked_sub(1);
    Ok(*byte)
  }

  fn get_bytes(&mut self, len: usize) -> Result<std::borrow::Cow<'_, [u8]>> {
    // A complete physical block needs only one reverse copy into the final owner.
    // Requests that may fail part-way use the fallback below.
    if len > 1 && self.block_size > 0 {
      let (current, next_read) = match self.next_read {
        Some(next_read) => (Some(self.current), next_read),
        None => (self.next_buffer, (self.block_size - 1) as usize),
      };
      if let Some(end) = next_read.checked_add(1)
        && len <= end
        && let Some(current) = current
        && let Some(buffer) = self.byte_buffers.get(current)
        && end <= buffer.len()
      {
        let bytes = buffer[end - len..end]
          .iter()
          .rev()
          .copied()
          .collect::<Vec<u8>>();
        if self.next_read.is_none() {
          self.current = current;
          self.next_buffer = current.checked_sub(1);
        }
        self.next_read = next_read.checked_sub(len);
        return Ok(std::borrow::Cow::Owned(bytes));
      }
    }
    let mut bytes = Vec::with_capacity(len);
    while bytes.len() < len {
      if let Some(next_read) = self.next_read
        && let Some(buffer) = self.byte_buffers.get(self.current)
        && let Some(end) = next_read.checked_add(1)
        && end <= buffer.len()
      {
        let count = (len - bytes.len()).min(end);
        bytes.extend(buffer[end - count..end].iter().rev().copied());
        self.next_read = next_read.checked_sub(count);
      } else {
        bytes.push(self.read_byte()?);
      }
    }
    Ok(std::borrow::Cow::Owned(bytes))
  }

  fn read_bytes(&mut self, b: &mut [u8], offset: usize, len: usize) -> Result<()> {
    for i in 0..len {
      b[offset + i] = self.read_byte()?;
    }
    Ok(())
  }

  fn read_group_vint(&mut self, dst: &mut [i32], offset: usize) -> Result<()> {
    GroupVIntUtil::read_group_vint_i32(self, dst, offset)
  }

  fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
    self.set_position(self.get_position() - num_bytes);
    Ok(())
  }
}

impl DataInputExt for BytesReaderImpl {}

impl Display for BytesReaderImpl {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}({})", std::any::type_name::<Self>(), self.block_bits)
  }
}

impl BytesReader for BytesReaderImpl {
  fn get_position(&self) -> i64 {
    self
      .next_buffer
      .map_or(0, |next_buffer| next_buffer as i64 + 1)
      * i64::from(self.block_size)
      + self.next_read.map_or(-1, |next_read| next_read as i64)
  }

  fn set_position(&mut self, pos: i64) {
    let buffer_index = (pos >> self.block_bits) as usize;
    let next_buffer = buffer_index.checked_sub(1);
    if self.next_buffer != next_buffer {
      self.next_buffer = next_buffer;
      self.current = buffer_index;
    }
    self.next_read = Some((pos & self.block_mask as i64) as usize);
    debug_assert_eq!(
      self.get_position(),
      pos,
      "pos={} get_pos={}",
      pos,
      self.get_position()
    );
  }
}
