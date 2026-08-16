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
use crate::core::codecs::compound_directory::CompoundDirectory;
use crate::core::codecs::lucene90::lucene90_compound_reader::Lucene90CompoundReader;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::store::IOContext;
use crate::core::store::directory::Directory;
use crate::core::util::error::lucene_error::Result;

/// Encodes/decodes compound files
pub trait CompoundFormat {
  /// Returns a read-only view of the compound files in this segment.
  type Directory<D>: CompoundDirectory
  where
    D: Directory;
  fn get_compound_reader<D>(&self, dir: &D, si: &SegmentInfo<D>) -> Result<Self::Directory<D>>
  where
    D: Directory;

  /// Packs the provided segment's files into a compound format.
  ///
  /// All files referenced by the provided [`SegmentInfo`]
  /// must have their headers and footers
  /// written using
  /// [`CodecUtil::write_index_header`](crate::core::codecs::codec_util::CodecUtil::write_index_header)
  /// and [`CodecUtil::write_footer`](crate::core::codecs::codec_util::CodecUtil::write_footer).
  fn write<D>(&self, dir: &impl Directory, si: &SegmentInfo<D>, context: &IOContext) -> Result<()>;
}
pub type DefaultCompoundReaderImpl<I> = Lucene90CompoundReader<I>;
pub type DefaultCompoundReader<D> =
  DefaultCompoundReaderImpl<<D as Directory>::IndexInput>;
