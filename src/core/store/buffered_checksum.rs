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
use crate::core::store::Checksum;
use crate::core::util::SliceCopyOps;

/// Wraps another [`Checksum`] with an internal buffer to speed up checksum
/// calculations.
pub struct BufferedChecksum<T> {
  buffer: Box<[u8]>,
  upto: usize,
  checksum: T,
}

impl<T: Checksum> BufferedChecksum<T> {
  /// Default buffer size: 1024
  pub const DEFAULT_BUFFER_SIZE: usize = 1024;
  pub fn new(checksum: T) -> Self {
    Self {
      buffer: vec![0; Self::DEFAULT_BUFFER_SIZE].into_boxed_slice(),
      upto: 0,
      checksum,
    }
  }
  /// Creates a new [`BufferedChecksum`] with the specified `buffer_size`.
  pub fn with_buffer_size(checksum: T, buffer_size: usize) -> Self {
    Self {
      buffer: vec![0; buffer_size].into_boxed_slice(),
      upto: 0,
      checksum,
    }
  }
  fn flush(&mut self) {
    if self.upto > 0 {
      self.checksum.update_bytes(&self.buffer, 0, self.upto);
      self.upto = 0;
    }
  }

  pub(crate) fn update_short(&mut self, value: i16) {
    let bytes = value.to_le_bytes();
    self.update_bytes(&bytes, 0, bytes.len());
  }

  pub(crate) fn update_int(&mut self, value: i32) {
    let bytes = value.to_le_bytes();
    self.update_bytes(&bytes, 0, bytes.len());
  }

  pub(crate) fn update_long(&mut self, value: i64) {
    let bytes = value.to_le_bytes();
    self.update_bytes(&bytes, 0, bytes.len());
  }

  pub(crate) fn update_longs(&mut self, values: &[i64], mut offset: usize, mut len: usize) {
    // Validate the same source slice before changing the checksum buffer.
    let _ = &values[offset..offset + len];
    // Preserve the existing direct-update path for buffers smaller than a long.
    if self.buffer.len() < size_of::<i64>() {
      for value in &values[offset..offset + len] {
        self.update_long(*value);
      }
      return;
    }
    if self.upto > 0 {
      let remaining = ((self.buffer.len() - self.upto) / size_of::<i64>()).min(len);
      for _ in 0..remaining {
        self.update_long(values[offset]);
        offset += 1;
        len -= 1;
      }
      if len == 0 {
        return;
      }
    }
    let capacity = self.buffer.len() / size_of::<i64>();
    while len > 0 {
      self.flush();
      let count = capacity.min(len);
      for (bytes, value) in self.buffer[..count * size_of::<i64>()]
        .as_chunks_mut::<{ size_of::<i64>() }>()
        .0
        .iter_mut()
        .zip(&values[offset..offset + count])
      {
        bytes.copy_from_slice(&value.to_le_bytes());
      }
      self.upto += count * size_of::<i64>();
      offset += count;
      len -= count;
    }
  }
}
impl<T: Checksum> Checksum for BufferedChecksum<T> {
  fn update(&mut self, b: u8) {
    debug_assert!(self.buffer.len() <= i32::MAX as usize);
    if self.upto == self.buffer.len() {
      self.flush();
    }
    self.buffer[self.upto] = b;
    self.upto += 1;
  }

  fn update_bytes(&mut self, bytes: &[u8], offset: usize, len: usize) {
    if len >= self.buffer.len() {
      self.flush();
      self
        .checksum
        .update_bytes(&bytes[offset..offset + len], 0, len);
    } else {
      if self.upto + len > self.buffer.len() {
        self.flush();
      }
      self
        .buffer
        .copy_from(&bytes[offset..offset + len], self.upto);
      self.upto += len;
    }
  }

  fn get_value(&mut self) -> i64 {
    self.flush();
    self.checksum.get_value()
  }

  fn reset(&mut self) {
    self.checksum.reset();
    self.upto = 0;
  }
}
