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

use crate::core::store::data_input::DataInput;
use crate::core::store::data_output::{DataOutput, copy_bytes_impl};
use crate::core::store::index_output::IndexOutput;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::close::{Closeable, CloseableWrite};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::io_utils::IOUtils;

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
impl<W: CloseableWrite> OutputStreamIndexOutput<W>
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
  /// Returns an [`IllegalArgumentError`](crate::core::util::error::IllegalArgumentError) if the given buffer size is less than
  /// [`BitUtil::LONG_BYTES`].
  pub fn new<R: Into<String>, N: Into<String>>(
    resource_description: R,
    name: N,
    inner: W,
    buffer_size: usize,
  ) -> Result<OutputStreamIndexOutput<W>> {
    if buffer_size < BitUtil::LONG_BYTES {
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
      name: name.into(),
      resource_description: resource_description.into(),
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
  W: CloseableWrite,
{
  fn copy_bytes<I>(&mut self, input: &mut I, num_bytes: usize) -> Result<()>
  where
    I: DataInput + ?Sized,
  {
    copy_bytes_impl(self, input, num_bytes)
  }

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

  fn write_ints(&mut self, values: &[i32]) -> Result<()> {
    if let Some(byte_count) = values.len().checked_mul(BitUtil::INT_BYTES)
      && let Some(bytes_written) = self.bytes_written.checked_add(byte_count)
      && let Some(os) = self.os.as_mut()
      && byte_count < os.inner.capacity() - os.inner.buffer().len()
    {
      // Strict spare capacity keeps every write in the existing buffer. No inner
      // Write callback can observe different primitive accounting or CRC updates.
      #[cfg(target_endian = "big")]
      let mut encoded = [0u8; 128];
      for chunk in values.chunks(32) {
        #[cfg(target_endian = "little")]
        let bytes = wide::bytemuck::cast_slice(chunk);
        #[cfg(target_endian = "big")]
        let bytes = {
          for (value, bytes) in chunk.iter().zip(encoded.as_chunks_mut::<4>().0) {
            *bytes = value.to_le_bytes();
          }
          &encoded[..chunk.len() * 4]
        };
        os.write_bytes(bytes)?;
      }
      self.bytes_written = bytes_written;
      return Ok(());
    }
    for &value in values {
      self.write_int(value)?;
    }
    Ok(())
  }

  fn write_short(&mut self, i: i16) -> Result<()> {
    self.bytes_written += 2;
    self.output_stream()?.write_i16(i)
  }

  fn write_vint(&mut self, i: i32) -> Result<()> {
    let mut value = i as u32;
    if value < 0x80 {
      return self.write_byte(value as u8);
    }
    let mut encoded = [0u8; 5];
    let mut len = 0;
    for byte in &mut encoded {
      len += 1;
      *byte = (value & 0x7F) as u8;
      value >>= 7;
      if value == 0 {
        break;
      }
      *byte |= 0x80;
    }
    if let Some(bytes_written) = self.bytes_written.checked_add(len)
      && let Some(os) = self.os.as_mut()
      && len < os.inner.capacity() - os.inner.buffer().len()
    {
      // Buffered-only writes cannot expose different callbacks or error prefixes.
      os.write_bytes(&encoded[..len])?;
      self.bytes_written = bytes_written;
      return Ok(());
    }
    for &byte in &encoded[..len] {
      self.write_byte(byte)?;
    }
    Ok(())
  }

  fn write_long(&mut self, i: i64) -> Result<()> {
    self.bytes_written += 8;
    self.output_stream()?.write_i64(i)
  }

  fn write_signed_vlong(&mut self, i: i64) -> Result<()> {
    let mut value = i as u64;
    if value < 0x80 {
      return self.write_byte(value as u8);
    }
    let mut encoded = [0u8; 10];
    let mut len = 0;
    for byte in &mut encoded {
      len += 1;
      *byte = (value & 0x7F) as u8;
      value >>= 7;
      if value == 0 {
        break;
      }
      *byte |= 0x80;
    }
    if let Some(bytes_written) = self.bytes_written.checked_add(len)
      && let Some(os) = self.os.as_mut()
      && len < os.inner.capacity() - os.inner.buffer().len()
    {
      // Buffered-only writes cannot expose different callbacks or error prefixes.
      os.write_bytes(&encoded[..len])?;
      self.bytes_written = bytes_written;
      return Ok(());
    }
    for &byte in &encoded[..len] {
      self.write_byte(byte)?;
    }
    Ok(())
  }
}

impl<W: Write> Display for OutputStreamIndexOutput<W> {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", self.resource_description)
  }
}

impl<W: Write> Closeable for OutputStreamIndexOutput<W>
where
  W: CloseableWrite,
{
  fn close(&mut self) -> Result<()> {
    if let Some(mut output_stream) = self.os.take()
      && !self.flushed_on_close
    {
      self.flushed_on_close = true;
      let flush_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| output_stream.flush()));
      let close_result =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| output_stream.close()));
      IOUtils::use_or_suppress_caught_result(flush_result, close_result)?;
    }
    Ok(())
  }
}

impl<W> IndexOutput for OutputStreamIndexOutput<W>
where
  W: CloseableWrite + Send + Sync,
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

impl<W: CloseableWrite> XBufferedOutputStream<W> {
  pub fn new(inner: W, buffer_size: usize) -> Self {
    Self {
      inner: BufWriter::with_capacity(buffer_size, inner),
      hasher: Hasher::new(),
      checksum: 0,
    }
  }

  fn close(self) -> Result<()> {
    let (inner, _) = self.inner.into_parts();
    inner.close()
  }

  pub fn checksum(&self) -> u32 {
    self.checksum
  }

  pub fn flush(&mut self) -> Result<()> {
    self.inner.flush()?;
    Ok(())
  }

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
