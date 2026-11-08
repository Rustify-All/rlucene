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

use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};

use crate::core::codecs::compression::compression_mode::{
  CompressionModeBase, CompressorEnum, DecompressorEnum,
};
use crate::core::codecs::compression::compressor::Compressor;
use crate::core::codecs::compression::decompressor::Decompressor as LuceneDecompressor;
use crate::core::index::BytesRef;
use crate::core::store::byte_buffers_data_input::ByteBuffersDataInput;
use crate::core::store::{DataInput, DataOutput};
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::{LuceneError, Result};

/// A compression mode that trades speed for compression ratio. Although
/// compression and decompression might be slow, this compression mode should
/// provide a good compression ratio. This mode might be interesting if/when
/// your index size is much bigger than your OS cache.
///
/// # Internal
/// This API is internal and might change in incompatible ways in the next
/// release.
#[derive(Debug)]
pub struct DeflateWithPresetDictCompressionMode;

impl DeflateWithPresetDictCompressionMode {
  // Shoot for 10 sub blocks
  const NUM_SUB_BLOCKS: i32 = 10;
  // And a dictionary whose size is about 6x smaller than sub blocks
  const DICT_SIZE_FACTOR: i32 = 6;
}

impl Display for DeflateWithPresetDictCompressionMode {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "BEST_COMPRESSION")
  }
}

impl Clone for DeflateWithPresetDictCompressionMode {
  fn clone(&self) -> Self {
    DeflateWithPresetDictCompressionMode
  }
}

impl CompressionModeBase for DeflateWithPresetDictCompressionMode {
  fn new_compressor(&self) -> CompressorEnum {
    // notes:
    // 3 is the highest level that doesn't have lazy match evaluation
    // 6 is the default, higher than that is just a waste of cpu
    CompressorEnum::DeflateDict(DeflateWithPresetDictCompressor::new(6))
  }

  fn new_decompressor(&self) -> DecompressorEnum {
    DecompressorEnum::DeflateDict(DeflateWithPresetDictDecompressor::new())
  }
}

pub struct DeflateWithPresetDictDecompressor {
  decoder: Option<Decompress>,
}

impl DeflateWithPresetDictDecompressor {
  fn new() -> Self {
    Self { decoder: None }
  }

  fn do_decompress<DI>(
    input: &mut DI,
    decompressor: &mut Decompress,
    bytes: &mut BytesRef<Vec<u8>>,
  ) -> Result<()>
  where
    DI: DataInput,
  {
    let compressed_length = input.read_vint()?;
    if compressed_length < 0 {
      return Err(LuceneError::corrupt_index(format!(
        "Invalid compressed length: {compressed_length} (resource={input})"
      )));
    }
    let compressed_length = compressed_length as usize;
    if compressed_length == 0 {
      return Ok(());
    }
    // Pad with an extra "dummy byte": see the documentation for using a raw
    // DEFLATE decompressor. We do it for compliance, but it has been
    // unnecessary in zlib for years.
    let padded_length = compressed_length + 1;
    // Preserve the original length error before reading the payload.
    ArrayUtil::oversize(padded_length, 1)?;
    let compressed = input.get_bytes(compressed_length)?;
    // Extra "dummy byte"
    let total_out = decompressor.total_out();
    let total_in = decompressor.total_in();
    let mut status = decompressor
      .decompress(
        &compressed,
        &mut bytes.bytes[bytes.length..],
        FlushDecompress::Finish,
      )
      .map_err(|error| LuceneError::from(std::io::Error::other(error)))?;
    if status != Status::StreamEnd && decompressor.total_in() - total_in == compressed_length as u64
    {
      let produced = (decompressor.total_out() - total_out) as usize;
      status = decompressor
        .decompress(
          &[0],
          &mut bytes.bytes[bytes.length + produced..],
          FlushDecompress::Finish,
        )
        .map_err(|error| LuceneError::from(std::io::Error::other(error)))?;
    }
    bytes.length += (decompressor.total_out() - total_out) as usize;
    if status != Status::StreamEnd {
      return Err(LuceneError::corrupt_index(format!(
        "Invalid decoder state: status={status:?} (resource={input})"
      )));
    }
    Ok(())
  }
}

impl Clone for DeflateWithPresetDictDecompressor {
  fn clone(&self) -> Self {
    Self::new()
  }
}

impl LuceneDecompressor for DeflateWithPresetDictDecompressor {
  fn decompress<DI>(
    &mut self,
    input: &mut DI,
    original_length: i32,
    offset: i32,
    length: i32,
    bytes: &mut BytesRef<Vec<u8>>,
  ) -> Result<()>
  where
    DI: DataInput,
  {
    debug_assert!(offset + length <= original_length);
    if length == 0 {
      bytes.length = 0;
      return Ok(());
    }
    let dict_length = input.read_vint()?;
    let block_length = input.read_vint()?;
    let dict_length_usize = dict_length as usize;
    ArrayUtil::grow_no_copy(&mut bytes.bytes, dict_length_usize, || 0)?;
    bytes.offset = 0;
    bytes.length = 0;

    let decompressor = self.decoder.get_or_insert_with(|| Decompress::new(false));
    decompressor.reset(false);

    // Read the dictionary
    Self::do_decompress(input, decompressor, bytes)?;
    if dict_length_usize != bytes.length {
      return Err(LuceneError::corrupt_index(format!(
        "Unexpected dict length (resource={input})"
      )));
    }

    let mut offset_in_block = dict_length;
    let mut offset_in_bytes_ref = offset;

    // Skip unneeded blocks
    while offset_in_block + block_length < offset {
      let compressed_length = input.read_vint()?;
      input.skip_bytes(compressed_length as i64)?;
      offset_in_block += block_length;
      offset_in_bytes_ref -= block_length;
    }

    // Read blocks that intersect with the interval we need
    while offset_in_block < offset + length {
      ArrayUtil::grow_with(&mut bytes.bytes, bytes.length + block_length as usize, || 0)?;
      decompressor.reset(false);
      decompressor
        .set_dictionary(&bytes.bytes[..dict_length_usize])
        .map_err(|error| LuceneError::from(std::io::Error::other(error)))?;
      Self::do_decompress(input, decompressor, bytes)?;
      offset_in_block += block_length;
    }

    bytes.offset = offset_in_bytes_ref as usize;
    bytes.length = length as usize;
    debug_assert!(bytes.is_valid()?);
    Ok(())
  }
}

pub struct DeflateWithPresetDictCompressor {
  compressor: Option<Compress>,
  compressed: Vec<u8>,
  closed: bool,
  buffer: Vec<u8>,
}

impl DeflateWithPresetDictCompressor {
  fn new(level: u32) -> Self {
    Self {
      compressor: Some(Compress::new(Compression::new(level), false)),
      compressed: Vec::new(),
      closed: false,
      buffer: Vec::new(),
    }
  }

  fn do_compress<DO>(&mut self, off: usize, len: usize, out: &mut DO) -> Result<()>
  where
    DO: DataOutput,
  {
    if len == 0 {
      out.write_vint(0)?;
      return Ok(());
    }
    let compressor = self
      .compressor
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("compressor is closed"))?;
    let initial_total_in = compressor.total_in();
    let initial_total_out = compressor.total_out();
    if self.compressed.is_empty() {
      self.compressed.resize(64, 0);
    }

    let total_count = loop {
      let consumed = (compressor.total_in() - initial_total_in) as usize;
      let total_count = (compressor.total_out() - initial_total_out) as usize;
      let status = compressor
        .compress(
          &self.buffer[off + consumed..off + len],
          &mut self.compressed[total_count..],
          FlushCompress::Finish,
        )
        .map_err(|error| LuceneError::from(std::io::Error::other(error)))?;
      let total_count = (compressor.total_out() - initial_total_out) as usize;
      debug_assert!(total_count <= self.compressed.len());
      if status == Status::StreamEnd {
        break total_count;
      }
      let min_size = self.compressed.len() + 1;
      ArrayUtil::grow_capacity(&mut self.compressed, min_size)?;
      self.compressed.resize(
        self.compressed.capacity().min(ArrayUtil::MAX_ARRAY_LENGTH),
        0,
      );
    };

    out.write_vint(total_count as i32)?;
    out.write_bytes_with_len(&self.compressed, total_count)
  }
}

impl Compressor for DeflateWithPresetDictCompressor {
  fn compress<DO>(
    &mut self,
    buffers_input: &mut ByteBuffersDataInput<&[u8]>,
    out: &mut DO,
  ) -> Result<()>
  where
    DO: DataOutput,
  {
    let len = (buffers_input.length() - buffers_input.position()?) as i32;
    let dict_length = len
      / (DeflateWithPresetDictCompressionMode::NUM_SUB_BLOCKS
        * DeflateWithPresetDictCompressionMode::DICT_SIZE_FACTOR);
    let block_length = (len - dict_length + DeflateWithPresetDictCompressionMode::NUM_SUB_BLOCKS
      - 1)
      / DeflateWithPresetDictCompressionMode::NUM_SUB_BLOCKS;
    out.write_vint(dict_length)?;
    out.write_vint(block_length)?;

    // Compress the dictionary first
    let compressor = self
      .compressor
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("compressor is closed"))?;
    compressor.reset();
    let dict_length_usize = dict_length as usize;
    ArrayUtil::grow_no_copy(
      &mut self.buffer,
      (dict_length + block_length) as usize,
      || 0,
    )?;
    DataInput::read_bytes(buffers_input, &mut self.buffer, 0, dict_length_usize)?;
    self.do_compress(0, dict_length_usize, out)?;

    // And then sub blocks
    let mut start = dict_length;
    while start < len {
      let compressor = self
        .compressor
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("compressor is closed"))?;
      compressor.reset();
      compressor
        .set_dictionary(&self.buffer[..dict_length_usize])
        .map_err(|error| LuceneError::from(std::io::Error::other(error)))?;
      let length = block_length.min(len - start) as usize;
      DataInput::read_bytes(buffers_input, &mut self.buffer, dict_length_usize, length)?;
      self.do_compress(dict_length_usize, length, out)?;
      start += block_length;
    }
    Ok(())
  }
}

impl Closeable for DeflateWithPresetDictCompressor {
  fn close(&mut self) -> Result<()> {
    if !self.closed {
      self.compressor.take();
      self.closed = true;
    }
    Ok(())
  }
}
