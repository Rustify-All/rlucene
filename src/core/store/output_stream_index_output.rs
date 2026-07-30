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
use std::fmt::{Display, Formatter};
use std::io::{BufWriter, Write};

use byteorder::{LittleEndian, WriteBytesExt};
use crc32fast::Hasher;

use crate::core::store::data_output::DataOutput;
use crate::core::store::index_output::IndexOutput;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::{LuceneError, Result};

/// Implementation struct for buffered [`IndexOutput`] that writes to an
/// [`OutputStream`](Write).
pub struct OutputStreamIndexOutput<W>
where
  W: Write,
{
  os: Option<XBufferedOutputStream<W>>,
  bytes_written: usize,
  flushed_on_close: bool,
  name: String,
  resource_description: String,
}
impl<W: Write> OutputStreamIndexOutput<W>
where
  W: Write,
{
  /// Creates a new [`OutputStreamIndexOutput`] with the given buffer size.
  ///
  /// # Arguments
  /// * `buffer_size` - The buffer size in bytes used to buffer writes
  ///   internally.
  ///
  /// # Errors
  /// Returns an `IllegalArgumentError` if the given buffer size is less than
  /// [`BitUtil::LONG_BYTES`].
  pub fn new(
    resource_description: &str,
    name: &str,
    inner: W,
    buffer_size: i32,
  ) -> Result<OutputStreamIndexOutput<W>> {
    if (buffer_size as usize) < BitUtil::LONG_BYTES {
      return Err(LuceneError::illegal_argument(format!(
        "Buffer size too small, need: {}, got: {}",
        BitUtil::LONG_BYTES,
        buffer_size
      )));
    }
    let os = XBufferedOutputStream::new(inner, buffer_size);
    Ok(Self {
      os: Some(os),
      bytes_written: 0,
      flushed_on_close: false,
      name: name.to_string(),
      resource_description: resource_description.to_string(),
    })
  }

  fn output_stream(&mut self) -> Result<&mut XBufferedOutputStream<W>> {
    self
      .os
      .as_mut()
      .ok_or_else(|| LuceneError::already_closed("this IndexOutput is closed"))
  }
}

impl<W: Write> DataOutput for OutputStreamIndexOutput<W>
where
  W: Write,
{
  fn write_byte(&mut self, b: u8) -> Result<()> {
    self.bytes_written += 1;
    self.output_stream()?.write_u8(b)
  }

  fn write_bytes_range(&mut self, b: &[u8], offset: usize, length: usize) -> Result<()> {
    let end = offset + length;
    self.bytes_written += length;
    self.output_stream()?.write_bytes(&b[offset..end])
  }

  fn write_int(&mut self, i: i32) -> Result<()> {
    self.bytes_written += 4;
    self.output_stream()?.write_i32(i)
  }

  fn write_short(&mut self, i: i16) -> Result<()> {
    self.bytes_written += 2;
    self.output_stream()?.write_i16(i)
  }

  fn write_long(&mut self, i: i64) -> Result<()> {
    self.bytes_written += 8;
    self.output_stream()?.write_i64(i)
  }
}

impl<W: Write> Display for OutputStreamIndexOutput<W>
where
  W: Write,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", self.resource_description)
  }
}

impl<W: Write> Closeable for OutputStreamIndexOutput<W>
where
  W: Write,
{
  fn close(&mut self) -> Result<()> {
    if let Some(mut output_stream) = self.os.take()
      && !self.flushed_on_close
    {
      self.flushed_on_close = true;
      output_stream.flush()?;
    }
    Ok(())
  }
}

impl<W> IndexOutput for OutputStreamIndexOutput<W>
where
  W: Write + Send + Sync,
{
  fn get_file_pointer(&self) -> Result<usize> {
    Ok(self.bytes_written)
  }

  fn get_checksum(&mut self) -> Result<u64> {
    let output_stream = self.output_stream()?;
    output_stream.checksum = output_stream.hasher.clone().finalize();
    Ok(output_stream.checksum as u64)
  }

  fn get_name(&self) -> &str {
    self.name.as_str()
  }
}

pub struct XBufferedOutputStream<W: Write> {
  inner: BufWriter<W>,
  hasher: Hasher,
  checksum: u32,
}

impl<W: Write> XBufferedOutputStream<W> {
  pub fn new(inner: W, buffer_size: i32) -> Self {
    Self {
      inner: BufWriter::with_capacity(buffer_size as usize, inner),
      hasher: Hasher::new(),
      checksum: 0,
    }
  }

  pub fn checksum(&self) -> u32 {
    self.checksum
  }

  pub fn flush(&mut self) -> Result<()> {
    self.inner.flush()?;
    Ok(())
  }

  //TODO IMPORTANT : If frequent checksum calculations become a bottleneck, we might
  // consider caching a batch of data and then calculating the checksum.
  fn update_checksum(&mut self, buf: &[u8]) {
    self.hasher.update(buf);
  }

  pub fn write_u8(&mut self, value: u8) -> Result<()> {
    self.inner.write_u8(value)?;
    self.update_checksum(&[value]);
    Ok(())
  }

  pub fn write_bytes(&mut self, buf: &[u8]) -> Result<()> {
    debug_assert!(buf.len() <= u32::MAX as usize);
    self.inner.write_all(buf)?;
    self.update_checksum(buf);
    Ok(())
  }

  pub fn write_i16(&mut self, value: i16) -> Result<()> {
    self.inner.write_i16::<LittleEndian>(value)?;
    self.update_checksum(&value.to_le_bytes());
    Ok(())
  }

  pub fn write_i32(&mut self, value: i32) -> Result<()> {
    self.inner.write_i32::<LittleEndian>(value)?;
    self.update_checksum(&value.to_le_bytes());
    Ok(())
  }

  pub fn write_i64(&mut self, value: i64) -> Result<()> {
    self.inner.write_i64::<LittleEndian>(value)?;
    self.update_checksum(&value.to_le_bytes());
    Ok(())
  }
}
