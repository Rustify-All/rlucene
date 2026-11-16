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

use crate::core::codecs::compression::compression_mode::{
  CompressionModeBase, CompressorEnum, DecompressorEnum,
};
use crate::core::codecs::compression::compressor::Compressor;
use crate::core::codecs::compression::decompressor::Decompressor;
use crate::core::index::BytesRef;
use crate::core::store::byte_buffers_data_input::ByteBuffersDataInput;
use crate::core::store::{ByteBuffersDataOutput, DataInput, DataOutput};
use crate::core::util::TryIntoInt;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::close::Closeable;
use crate::core::util::compress::lz4::{FastCompressionHashTable, HashTableEnum, LZ4};
use crate::core::util::error::lucene_error::{LuceneError, Result};
#[derive(Debug)]
pub struct LZ4WithPresetDictCompressionMode;
impl LZ4WithPresetDictCompressionMode {
  // Shoot for 10 sub blocks
  const NUM_SUB_BLOCKS: i32 = 10;
  // And a dictionary whose size is about 2x smaller than sub blocks
  const DICT_SIZE_FACTOR: i32 = 2;
}

impl Display for LZ4WithPresetDictCompressionMode {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "BEST_SPEED")
  }
}

impl Clone for LZ4WithPresetDictCompressionMode {
  fn clone(&self) -> Self {
    LZ4WithPresetDictCompressionMode
  }
}

impl CompressionModeBase for LZ4WithPresetDictCompressionMode {
  fn new_compressor(&self) -> CompressorEnum {
    CompressorEnum::LZ4Dict(LZ4WithPresetDictCompressor::new())
  }

  fn new_decompressor(&self) -> DecompressorEnum {
    DecompressorEnum::LZ4Dict(LZ4WithPresetDictDecompressor::new())
  }
}

pub struct LZ4WithPresetDictDecompressor {
  compressed_lengths: Vec<i32>,
  buffer: Vec<u8>,
}

impl Default for LZ4WithPresetDictDecompressor {
  fn default() -> Self {
    Self::new()
  }
}

impl LZ4WithPresetDictDecompressor {
  pub fn new() -> Self {
    LZ4WithPresetDictDecompressor {
      compressed_lengths: Vec::new(),
      buffer: Vec::new(),
    }
  }

  fn read_compressed_lengths<DI>(
    &mut self,
    input: &mut DI,
    original_length: i32,
    dict_length: i32,
    block_length: i32,
  ) -> Result<usize>
  where
    DI: DataInput,
  {
    input.read_vint()?; // Compressed length of the dictionary, unused
    let mut total_length = dict_length;
    let mut i = 0;
    ArrayUtil::grow_no_copy(
      &mut self.compressed_lengths,
      (original_length / block_length + 1) as usize,
      || 0,
    )?;
    while total_length < original_length {
      self.compressed_lengths[i] = input.read_vint()?;
      total_length += block_length;
      i += 1;
    }
    Ok(i)
  }
}

impl Clone for LZ4WithPresetDictDecompressor {
  fn clone(&self) -> Self {
    LZ4WithPresetDictDecompressor::new()
  }
}

impl Decompressor for LZ4WithPresetDictDecompressor {
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
    let dict_length = input.read_vint()?; // Read dictionary length
    let block_length = input.read_vint()?; // Read block length

    let num_blocks =
      self.read_compressed_lengths(input, original_length, dict_length, block_length)?;

    // Grow the buffer to fit the dictionary and block length
    ArrayUtil::grow_no_copy(
      &mut self.buffer,
      (dict_length + block_length) as usize,
      || 0,
    )?;
    bytes.length = 0;
    let dict_length_usize = dict_length as usize;

    // Read the dictionary
    if LZ4::decompress(input, dict_length, &mut self.buffer, 0)? != dict_length {
      return Err(LuceneError::corrupt_index(format!(
        "Illegal dict length  (resource={input})"
      )));
    }

    let mut offset_in_block = dict_length;
    let mut offset_in_bytes_ref = offset;

    if offset >= dict_length {
      offset_in_bytes_ref -= dict_length;

      // Skip unneeded blocks
      let mut num_bytes_to_skip = 0;
      for i in 0..num_blocks {
        if offset_in_block + block_length < offset {
          let compressed_block_length = self.compressed_lengths[i];
          num_bytes_to_skip += compressed_block_length;
          offset_in_block += block_length;
          offset_in_bytes_ref -= block_length;
        } else {
          break;
        }
      }
      input.skip_bytes(num_bytes_to_skip as i64)?;
    } else {
      // The dictionary contains some bytes we need, copy its content to
      // the BytesRef
      ArrayUtil::grow_no_copy(&mut bytes.bytes, dict_length_usize, || 0)?;
      let copy_source = &self.buffer[0..dict_length_usize];
      bytes.bytes[..copy_source.len()].copy_from_slice(copy_source);
      bytes.length = dict_length_usize;
    }

    // Read blocks that intersect with the interval we need
    if offset_in_block < offset + length {
      ArrayUtil::grow_with(
        &mut bytes.bytes,
        bytes.length + (offset + length - offset_in_block) as usize,
        || 0,
      )?;
    }

    while offset_in_block < offset + length {
      let bytes_to_decompress = (offset + length - offset_in_block).min(block_length);
      LZ4::decompress(input, bytes_to_decompress, &mut self.buffer, dict_length)?;
      let copy_source =
        &self.buffer[dict_length_usize..(dict_length + bytes_to_decompress) as usize];
      bytes.bytes[bytes.length..][..copy_source.len()].copy_from_slice(copy_source);
      bytes.length += bytes_to_decompress as usize;
      offset_in_block += block_length;
    }
    bytes.offset = offset_in_bytes_ref as usize;
    bytes.length = length as usize;
    debug_assert!(bytes.is_valid()?);
    Ok(())
  }
}

pub struct LZ4WithPresetDictCompressor {
  compressed: ByteBuffersDataOutput,
  hash_table: HashTableEnum,
  buffer: Vec<u8>,
}

impl LZ4WithPresetDictCompressor {
  fn new() -> Self {
    LZ4WithPresetDictCompressor {
      compressed: ByteBuffersDataOutput::new_resettable_instance(),
      hash_table: HashTableEnum::Fast(FastCompressionHashTable::new()),
      buffer: Vec::new(),
    }
  }
  fn do_compress<DO>(&mut self, dict_len: i32, len: i32, out: &mut DO) -> Result<()>
  where
    DO: DataOutput,
  {
    let prev_compressed_size = self.compressed.size();
    LZ4::compress_with_dictionary(
      self.buffer.as_slice(),
      0,
      dict_len,
      len,
      &mut self.compressed,
      &mut self.hash_table,
    )?;
    // Write the number of compressed bytes
    out.write_vint((self.compressed.size() - prev_compressed_size).try_convert()?)
  }
}
impl Compressor for LZ4WithPresetDictCompressor {
  fn compress<DO>(
    &mut self,
    buffers_input: &mut ByteBuffersDataInput<&[u8]>,
    out: &mut DO,
  ) -> Result<()>
  where
    DO: DataOutput,
  {
    let len = (buffers_input.length() - buffers_input.position()?) as i32;
    let dict_length = (len
      / (LZ4WithPresetDictCompressionMode::NUM_SUB_BLOCKS
        * LZ4WithPresetDictCompressionMode::DICT_SIZE_FACTOR))
      .min(LZ4::MAX_DISTANCE);
    let block_length = (len - dict_length + LZ4WithPresetDictCompressionMode::NUM_SUB_BLOCKS - 1)
      / LZ4WithPresetDictCompressionMode::NUM_SUB_BLOCKS;

    ArrayUtil::grow_no_copy(
      &mut self.buffer,
      (dict_length + block_length) as usize,
      || 0,
    )?;

    out.write_vint(dict_length)?;
    out.write_vint(block_length)?;

    self.compressed.reset();
    let dict_length_usize = dict_length as usize;
    // Compress the dictionary first
    DataInput::read_bytes(buffers_input, &mut self.buffer, 0, dict_length_usize)?;
    self.do_compress(0, dict_length, out)?;

    // And then sub blocks
    let mut start = dict_length;
    while start < len {
      let l = (len - start).min(block_length);
      DataInput::read_bytes(
        buffers_input,
        &mut self.buffer,
        dict_length_usize,
        l as usize,
      )?;
      self.do_compress(dict_length, l, out)?;
      start += block_length;
    }
    // We only wrote lengths so far, now write compressed data
    self.compressed.copy_to(out)?;
    Ok(())
  }
}

impl Closeable for LZ4WithPresetDictCompressor {}
