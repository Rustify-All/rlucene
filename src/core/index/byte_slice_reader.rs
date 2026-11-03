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

use crate::core::index::byte_slice_pool::ByteSlicePool;
use crate::core::store::data_input_ext::DataInputExt;
use crate::core::store::{DataInput, DataOutput};
use crate::core::util::bit_util::BitUtil;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::group_vint_util::GroupVIntUtil;
use crate::core::util::{
  BYTE_BLOCK_MASK, BYTE_BLOCK_SIZE, ByteBlockPool, SliceCopyOps, TryIntoInt,
};
use std::fmt::{Display, Formatter};

/// IndexInput that knows how to read the byte slices written by Posting and PostingVector.
/// We read the bytes in each slice until we hit the end of that slice at which point we read the forwarding address of the next slice and then jump to it.
pub(crate) struct ByteSliceReader<'a> {
  pool: &'a ByteBlockPool,
  buffer_upto: usize,
  buffer: &'a [u8],
  upto: usize,
  limit: usize,
  level: usize,
  buffer_offset: usize,
  end_index: usize,
}

impl<'a> ByteSliceReader<'a> {
  pub(crate) fn new(pool: &'a ByteBlockPool) -> Self {
    Self {
      pool,
      buffer_upto: 0,
      buffer: &[],
      upto: 0,
      limit: 0,
      level: 0,
      buffer_offset: 0,
      end_index: 0,
    }
  }

  pub(crate) fn eof(&self) -> bool {
    debug_assert!(self.upto + self.buffer_offset <= self.end_index);
    self.upto + self.buffer_offset == self.end_index
  }
}

impl<'a> ByteSliceReader<'a> {
  pub(crate) fn init(&mut self, start_index: usize, end_index: usize) {
    debug_assert!(end_index >= start_index);

    self.end_index = end_index;
    self.level = 0;

    self.buffer_upto = start_index / BYTE_BLOCK_SIZE as usize;
    self.buffer_offset = self.buffer_upto * BYTE_BLOCK_SIZE as usize;
    // Like Java's nullable buffer, an unallocated block fails when read.
    self.buffer = self.pool.get_buffer_or_empty(self.buffer_upto);
    self.upto = start_index & BYTE_BLOCK_MASK as usize;

    let first_size = ByteSlicePool::LEVEL_SIZE_ARRAY[0] as usize;

    if start_index + first_size >= end_index {
      // Only one slice
      self.limit = end_index & BYTE_BLOCK_MASK as usize;
    } else {
      self.limit = self.upto + first_size - 4;
    }
  }

  #[allow(dead_code)]
  pub(crate) fn write<DO>(&mut self, out: &mut DO) -> Result<usize>
  where
    DO: DataOutput,
  {
    let mut size = 0;
    loop {
      let buffer = if self.buffer.is_empty() {
        self.pool.get_buffer(self.buffer_upto).as_slice()
      } else {
        self.buffer
      };
      if self.limit + self.buffer_offset == self.end_index {
        debug_assert!(self.end_index - self.buffer_offset >= self.upto);
        out.write_bytes_range(buffer, self.upto, self.limit - self.upto)?;
        size += self.limit - self.upto;
        break;
      } else {
        out.write_bytes_range(buffer, self.upto, self.limit - self.upto)?;
        size += self.limit - self.upto;
        self.next_slice()?;
      }
    }
    Ok(size)
  }

  pub(crate) fn next_slice(&mut self) -> Result<()> {
    let next_index = BitUtil::get_i32_le(self.buffer, self.limit).try_convert()?;

    self.level = ByteSlicePool::NEXT_LEVEL_ARRAY[self.level];
    let new_size = ByteSlicePool::LEVEL_SIZE_ARRAY[self.level] as usize;

    self.buffer_upto = next_index / BYTE_BLOCK_SIZE as usize;
    self.buffer_offset = self.buffer_upto * BYTE_BLOCK_SIZE as usize;
    // Like Java's nullable buffer, an unallocated block fails when read.
    self.buffer = self.pool.get_buffer_or_empty(self.buffer_upto);
    self.upto = next_index & BYTE_BLOCK_MASK as usize;

    if next_index + new_size >= self.end_index {
      // Final slice
      debug_assert!(self.end_index - next_index > 0);
      self.limit = self.end_index - self.buffer_offset;
    } else {
      // Intermediate slice (reserve 4 bytes for forwarding address)
      self.limit = self.upto + new_size - 4;
    }
    Ok(())
  }
}

impl crate::core::util::close::Closeable for ByteSliceReader<'_> {}

impl DataInput for ByteSliceReader<'_> {
  fn read_byte(&mut self) -> Result<u8> {
    debug_assert!(!self.eof());
    debug_assert!(self.upto <= self.limit);

    if self.upto == self.limit {
      self.next_slice()?;
    }

    let byte = self.buffer[self.upto];
    self.upto += 1;
    Ok(byte)
  }

  fn get_bytes(&mut self, len: usize) -> Result<std::borrow::Cow<'_, [u8]>> {
    let mut bytes = vec![0; len];
    self.read_bytes(&mut bytes, 0, len)?;
    Ok(std::borrow::Cow::Owned(bytes))
  }

  fn read_bytes(&mut self, b: &mut [u8], mut offset: usize, mut len: usize) -> Result<()> {
    while len > 0 {
      let num_left = self.limit - self.upto;
      if num_left < len {
        {
          let buffer = self.buffer;
          b.copy_from(&buffer[self.upto..self.upto + num_left], offset);
        }
        offset += num_left;
        len -= num_left;
        self.next_slice()?;
      } else {
        let buffer = self.buffer;
        b.copy_from(&buffer[self.upto..(self.upto + len)], offset);
        self.upto += len;
        break;
      }
    }
    Ok(())
  }

  fn read_group_vint(&mut self, dst: &mut [i32], offset: usize) -> Result<()> {
    GroupVIntUtil::read_group_vint_i32(self, dst, offset)
  }

  fn skip_bytes(&mut self, num_bytes: i64) -> Result<()> {
    if num_bytes < 0 {
      return Err(LuceneError::illegal_argument(format!(
        "num_bytes must be >= 0, got {num_bytes}"
      )));
    }
    let mut num_bytes = num_bytes.try_convert()?;
    while num_bytes > 0 {
      let num_left = self.limit - self.upto;
      if num_left < num_bytes {
        num_bytes -= num_left;
        self.next_slice()?;
      } else {
        self.upto += num_bytes;
        break;
      }
    }
    Ok(())
  }
}

impl DataInputExt for ByteSliceReader<'_> {}

impl Display for ByteSliceReader<'_> {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", std::any::type_name::<Self>())
  }
}
