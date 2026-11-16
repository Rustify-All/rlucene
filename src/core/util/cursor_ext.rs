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
use std::io::Cursor;

use crate::core::util::TryIntoInt;
use crate::core::util::error::lucene_error::{LuceneError, Result};

pub trait ReadableCursorExt {
  /// Returns the remaining bytes in the buffer from the current position.
  fn remain(&self) -> Result<usize>;

  /// Returns the remaining bytes between a specified position and a limit.
  ///
  /// # Arguments
  /// * `position` - The current position in the buffer.
  /// * `limit` - The effective limit up to which remaining bytes are
  ///   calculated.
  fn remain_between(&self, position: usize, limit: usize) -> usize;

  /// Reads data from the cursor's buffer to the destination slice, starting
  /// at the current position.
  fn read_to(&mut self, dest: &mut [u8], offset: usize, len: usize) -> Result<()>;

  /// Reads data from a specific position in the cursor into the destination
  /// buffer.
  fn read_to_buffer(
    &self,
    dest: &mut [u8],
    offset: usize,
    position: usize,
    len: usize,
  ) -> Result<()>;
}

pub trait WritableCursorExt: ReadableCursorExt {
  /// Writes the entire slice of data into the cursor's buffer.
  fn write_from_slice(&mut self, src: &[u8]) -> Result<()>;

  /// Writes data from the source slice into the cursor's buffer, starting
  /// from the given offset.
  fn write_from(&mut self, src: &[u8], offset: usize, len: usize) -> Result<()>;
}
impl<T> ReadableCursorExt for Cursor<T>
where
  T: AsRef<[u8]>,
{
  fn remain(&self) -> Result<usize> {
    let p: usize = self.position().try_convert()?;
    Ok(self.remain_between(p, self.get_ref().as_ref().len()))
  }

  fn remain_between(&self, position: usize, limit: usize) -> usize {
    if limit == 0 {
      return 0;
    }
    debug_assert!(
      position <= limit,
      "Position ({position}) exceeds specified limit ({limit})"
    );
    limit.saturating_sub(position)
  }

  fn read_to(&mut self, dest: &mut [u8], offset: usize, len: usize) -> Result<()> {
    let position = self.position().try_convert()?;
    perform_read(self.get_ref().as_ref(), dest, offset, position, len)?;
    self.set_position((position + len).try_convert()?);
    Ok(())
  }

  fn read_to_buffer(
    &self,
    dest: &mut [u8],
    offset: usize,
    position: usize,
    len: usize,
  ) -> Result<()> {
    perform_read(self.get_ref().as_ref(), dest, offset, position, len)
  }
}

impl<T> WritableCursorExt for Cursor<T>
where
  T: AsRef<[u8]> + AsMut<[u8]>,
{
  fn write_from_slice(&mut self, src: &[u8]) -> Result<()> {
    let position: usize = self.position().try_convert()?;
    let len = src.len();
    let destination = self.get_mut().as_mut();

    if position > destination.len() || len > destination.len() - position {
      return Err(LuceneError::illegal_argument(format!(
        "Buffer out of bounds: position={}, len={}, total={}",
        position,
        len,
        destination.len()
      )));
    }

    destination[position..position + len].copy_from_slice(src);
    self.set_position((position + len) as u64);
    Ok(())
  }

  fn write_from(&mut self, src: &[u8], offset: usize, len: usize) -> Result<()> {
    let src_slice = &src[offset..(offset + len)];
    self.write_from_slice(src_slice)
  }
}
fn perform_read(
  source: &[u8],
  dest: &mut [u8],
  offset: usize,
  position: usize,
  len: usize,
) -> Result<()> {
  let total = source.len();

  if position > total || len > total - position {
    return Err(LuceneError::illegal_argument(format!(
      "Read out of bounds: position={position}, len={len}, total={total}"
    )));
  }
  if offset > dest.len() || len > dest.len() - offset {
    return Err(LuceneError::illegal_argument(format!(
      "Destination buffer out of bounds: offset={}, len={}, total={}",
      offset,
      len,
      dest.len()
    )));
  }

  dest[offset..offset + len].copy_from_slice(&source[position..position + len]);

  Ok(())
}
