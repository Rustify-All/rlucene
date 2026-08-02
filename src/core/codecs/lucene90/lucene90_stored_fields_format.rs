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
use crate::core::codecs::compressing::lucene90_compressing_stored_fields_format::Lucene90CompressingStoredFieldsFormat;
use crate::core::codecs::compression::compression_mode::{
  CompressionModeEnum, DeflateCompressionMode,
};
use crate::core::codecs::lz4_with_preset_dict_compression_mode::LZ4WithPresetDictCompressionMode;
use crate::core::codecs::stored_fields_format::StoredFieldsFormat;

use crate::core::index::field_infos::FieldInfos;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::store::directory::Directory;
use crate::core::store::{IOContext, IndexInput};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::sync::Arc;

/// Lucene 9.0 stored fields format.
///
/// # Principle
///
/// This [`StoredFieldsFormat`] compresses blocks of documents in order to
/// improve the compression ratio compared to document-level compression. It uses the [LZ4](http://code.google.com/p/lz4/)
/// compression algorithm by default in 8KB blocks and shared dictionaries,
/// which is fast to compress and very fast to decompress data. Although the
/// default compression method that is used ([`Mode::BestSpeed`]) focuses more
/// on speed than on compression ratio, it should provide interesting
/// compression ratios for redundant inputs (such as log files, HTML or plain
/// text). For higher compression, you can choose ([`Mode::BestCompression`]),
/// which uses the [DEFLATE](http://en.wikipedia.org/wiki/DEFLATE) algorithm with 48KB blocks and shared
/// dictionaries for a better ratio at the expense of slower performance. These
/// two options can be configured like this:
///
/// ```java
/// // the default: for high performance
/// indexWriterConfig.setCodec(new Lucene100Codec(Mode.BEST_SPEED));
/// // instead for higher compression (but slower):
/// indexWriterConfig.setCodec(new Lucene100Codec(Mode.BEST_COMPRESSION));
/// ```
///
/// # File formats
///
/// Stored fields are represented by three files:
///
/// 1. A fields data file (extension `.fdt`). This file stores a compact representation of documents
///    in compressed blocks of 8KB or more. When writing a segment, documents are appended to an
///    in-memory `byte[]` buffer. When its size reaches 80KB or more, some metadata about the
///    documents is flushed to disk, immediately followed by a compressed representation of the
///    buffer using the [LZ4 compression format](https://github.com/lz4/lz4).
///
///    **Notes:**
///    - When at least one document in a chunk is large enough so that the chunk
///      is larger than 80KB, the chunk will actually be compressed in several
///      LZ4 blocks of 8KB. This allows
///      [`StoredFieldVisitor`](crate::core::index::stored_field_visitor::StoredFieldVisitor)s
///      which are only interested in the first fields of a document to not have
///      to decompress 10MB of data if the document is 10MB, but only 8-16KB
///      (may cross the block).
///    - Given that the original lengths are written in the metadata of the
///      chunk, the decompressor can leverage this information to stop decoding
///      as soon as enough data has been decompressed.
///    - In case documents are incompressible, the overhead of the compression
///      format is less than 0.5%.
///
/// 2. A fields index file (extension `.fdx`). This file stores two
///    [`DirectMonotonicWriter`](crate::core::util::packed::direct_monotonic_writer::DirectMonotonicWriter)
///    monotonic arrays, one for the first doc IDs of each block of compressed
///    documents, and another one for the corresponding offsets on disk. At
///    search time, the array containing doc IDs is binary-searched in order to
///    find the block that contains the expected doc ID, and the associated
///    offset on disk is retrieved from the second array.
///
/// 3. A fields meta file (extension `.fdm`). This file stores metadata about
///    the monotonic arrays stored in the index file.
///
/// # Known limitations
///
/// This [`StoredFieldsFormat`] does not support individual documents larger
/// than `(2^31 - 2^14)` bytes.
#[derive(Clone)]
pub struct Lucene90StoredFieldsFormat {
  pub mode: Mode,
}
impl Default for Lucene90StoredFieldsFormat {
  fn default() -> Self {
    Self::new()
  }
}

impl Lucene90StoredFieldsFormat {
  /// Attribute key for compression mode.
  const MODE_KEY: &'static str = concat!(module_path!(), "::mode");

  /// Shoot for 10 sub blocks of 48kB each.
  const BEST_COMPRESSION_BLOCK_LENGTH: usize = 10 * 48 * 1024;

  /// Shoot for 10 sub blocks of 8kB each.
  const BEST_SPEED_BLOCK_LENGTH: usize = 10 * 8 * 1024;
  /// Stored fields format with default options.
  pub fn new() -> Self {
    Self::with_mode(Mode::BestSpeed)
  }
  /// Stored fields format with specified mode.
  pub fn with_mode(mode: Mode) -> Self {
    Self { mode }
  }

  fn stored_fields_format_impl(
    &self,
    mode: &Mode,
  ) -> Result<Lucene90CompressingStoredFieldsFormat> {
    match mode {
      Mode::BestSpeed => Lucene90CompressingStoredFieldsFormat::new(
        "Lucene90StoredFieldsFastData",
        BEST_SPEED_MODE.clone(),
        Self::BEST_SPEED_BLOCK_LENGTH as i32,
        1024,
        10,
      ),
      Mode::BestCompression => Lucene90CompressingStoredFieldsFormat::new(
        "Lucene90StoredFieldsHighData",
        BEST_COMPRESSION_MODE.clone(),
        Self::BEST_COMPRESSION_BLOCK_LENGTH as i32,
        4096,
        10,
      ),
    }
  }
}
impl StoredFieldsFormat for Lucene90StoredFieldsFormat {
  type StoredFieldsReader<T: IndexInput> =
    <Lucene90CompressingStoredFieldsFormat as StoredFieldsFormat>::StoredFieldsReader<T>;

  fn fields_reader<D1, D2>(
    &self,
    directory: &D1,
    segment_info: &SegmentInfo<D2>,
    field_infos: Arc<FieldInfos>,
    context: &IOContext,
  ) -> Result<Self::StoredFieldsReader<D1::IndexInput>>
  where
    D1: Directory,
    D2: Directory,
  {
    let value = segment_info.get_attribute(Self::MODE_KEY);
    let mode_name = value.ok_or_else(|| {
      LuceneError::illegal_state(format!(
        "missing value for {} for segment: {}",
        Self::MODE_KEY,
        segment_info.name
      ))
    })?;

    let mode = Mode::from_name(mode_name)?;

    let format = self.stored_fields_format_impl(&mode)?;
    format.fields_reader(directory, segment_info, field_infos, context)
  }

  type StoredFieldsWriter<D: Directory> =
    <Lucene90CompressingStoredFieldsFormat as StoredFieldsFormat>::StoredFieldsWriter<D>;

  fn fields_writer<D1, D2>(
    &self,
    directory: D1,
    segment_info: &mut SegmentInfo<D2>,
    context: &IOContext,
  ) -> Result<Self::StoredFieldsWriter<D1>>
  where
    D1: Directory,
    D2: Directory,
  {
    let previous =
      segment_info.put_attribute(Self::MODE_KEY.to_string(), self.mode.name().to_string());

    if let Some(prev) = previous
      && prev != *self.mode.name()
    {
      return Err(LuceneError::illegal_state(format!(
        "found existing value for {} for segment: {}, old={}, new={}",
        Self::MODE_KEY,
        segment_info.name,
        prev,
        self.mode.name()
      )));
    }
    let format = self.stored_fields_format_impl(&self.mode)?;
    format.fields_writer(directory, segment_info, context)
  }
}
/// Compression mode for [`Mode::BestCompression`].
static BEST_COMPRESSION_MODE: CompressionModeEnum =
  CompressionModeEnum::Deflate(DeflateCompressionMode);
/// Compression mode for [`Mode::BestSpeed`].
static BEST_SPEED_MODE: CompressionModeEnum =
  CompressionModeEnum::LZ4Dict(LZ4WithPresetDictCompressionMode);

/// Configuration option for stored fields.
#[derive(Clone, Copy)]
pub enum Mode {
  /// Trade compression ratio for retrieval speed.
  BestSpeed,
  /// Trade retrieval speed for compression ratio.
  BestCompression,
}
impl Mode {
  fn name(&self) -> &'static str {
    match self {
      Mode::BestSpeed => "BEST_SPEED",
      Mode::BestCompression => "BEST_COMPRESSION",
    }
  }
  fn from_name(name: &str) -> Result<Self> {
    match name {
      "BEST_SPEED" => Ok(Mode::BestSpeed),
      "BEST_COMPRESSION" => Ok(Mode::BestCompression),
      _ => Err(LuceneError::illegal_state("unknown mode name")),
    }
  }
}
