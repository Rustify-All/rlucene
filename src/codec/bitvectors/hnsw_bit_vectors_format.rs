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
use crate::codec::bitvectors::flat_bit_vectors_scorer::FlatBitVectorsScorer;
use crate::core::codecs::hnsw::flat_vectors_format::FlatVectorsFormat;
use crate::core::codecs::knn_field_vectors_writer::VectorValueEnum;
use crate::core::codecs::knn_vectors_format::KnnVectorsFormat;
use crate::core::codecs::knn_vectors_writer::KnnVectorsWriter;
use crate::core::codecs::lucene99::lucene99_flat_vectors_format::Lucene99FlatVectorsFormat;
use crate::core::codecs::lucene99::lucene99_flat_vectors_reader::Lucene99FlatVectorsReader;
use crate::core::codecs::lucene99::lucene99_flat_vectors_writer::Lucene99FlatVectorsWriter;
use crate::core::codecs::lucene99::lucene99_hnsw_vectors_format::{
  DEFAULT_BEAM_WIDTH, DEFAULT_MAX_CONN, DEFAULT_NUM_MERGE_WORKER, MAXIMUM_BEAM_WIDTH,
  MAXIMUM_MAX_CONN,
};
use crate::core::codecs::lucene99::lucene99_hnsw_vectors_reader::Lucene99HnswVectorsReader;
use crate::core::codecs::lucene99::lucene99_hnsw_vectors_writer::Lucene99HnswVectorsWriter;
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::index_reader::Identity;
use crate::core::index::merge_state::MergeState;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorter::DocMap;
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::store::directory::Directory;
use crate::core::store::{IndexInput, IndexOutput};
use crate::core::util::HasIdentity;
use crate::core::util::accountable::Accountable;
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::fmt::{Display, Formatter};
use std::sync::{Arc, OnceLock};

/// Encodes bit vector values into an associated graph connecting the documents having values. The
/// graph is used to power HNSW search. The format consists of two files, and uses
/// [`Lucene99FlatVectorsFormat`] to store the actual vectors, but with a custom scorer
/// implementation: For details on graph storage and file extensions, see
/// `Lucene99HnswVectorsFormat`.
pub struct HnswBitVectorsFormat {
  max_conn: usize,
  beam_width: usize,
  /// The format for storing, reading, merging vectors on disk
  flat_vectors_format: Lucene99FlatVectorsFormat<FlatBitVectorsScorer>,
  num_merge_workers: usize,
  identity: Identity,
}

pub const NAME: &str = "HnswBitVectorsFormat";

impl HnswBitVectorsFormat {
  /// Constructs a format using default graph construction parameters
  pub fn new() -> Result<Self> {
    Self::with_graph_para_with_threads(
      DEFAULT_MAX_CONN,
      DEFAULT_BEAM_WIDTH,
      DEFAULT_NUM_MERGE_WORKER,
    )
  }

  /// Constructs a format using the given graph construction parameters.
  ///
  /// - `max_conn`: the maximum number of connections to a node in the HNSW graph
  /// - `beam_width`: the size of the queue maintained during graph construction.
  pub fn with_graph_para(max_conn: usize, beam_width: usize) -> Result<Self> {
    Self::with_graph_para_with_threads(max_conn, beam_width, DEFAULT_NUM_MERGE_WORKER)
  }

  /// Constructs a format using the given graph construction parameters and scalar quantization.
  ///
  /// - `max_conn`: the maximum number of connections to a node in the HNSW graph
  /// - `beam_width`: the size of the queue maintained during graph construction.
  /// - `num_merge_workers`: number of workers (threads) that will be used when doing merge.
  pub fn with_graph_para_with_threads(
    max_conn: usize,
    beam_width: usize,
    num_merge_workers: usize,
  ) -> Result<Self> {
    if max_conn == 0 || max_conn > MAXIMUM_MAX_CONN {
      return Err(LuceneError::illegal_argument(format!(
        "maxConn must be positive and less than or equal to {}; maxConn={}",
        MAXIMUM_MAX_CONN, max_conn
      )));
    }
    if beam_width == 0 || beam_width > MAXIMUM_BEAM_WIDTH {
      return Err(LuceneError::illegal_argument(format!(
        "beamWidth must be positive and less than or equal to {}; beamWidth={}",
        MAXIMUM_BEAM_WIDTH, beam_width
      )));
    }
    Ok(Self {
      max_conn,
      beam_width,
      num_merge_workers,
      flat_vectors_format: Lucene99FlatVectorsFormat::new(FlatBitVectorsScorer),
      identity: Identity::new(),
    })
  }
}

impl HasIdentity for HnswBitVectorsFormat {
  fn identity(&self) -> &Identity {
    &self.identity
  }
}

impl KnnVectorsFormat for HnswBitVectorsFormat {
  fn get_name(&self) -> &str {
    NAME
  }

  type KnnVectorsWriter<T: IndexOutput> = FlatBitVectorsWriter<
    Lucene99HnswVectorsWriter<Lucene99FlatVectorsWriter<T, FlatBitVectorsScorer>, T>,
  >;

  fn fields_writer<D1, D2>(
    &self,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::KnnVectorsWriter<D1::IndexOutput>>
  where
    D1: Directory,
    D2: Directory,
  {
    Ok(FlatBitVectorsWriter::new(Lucene99HnswVectorsWriter::new(
      state,
      self.max_conn,
      self.beam_width,
      FlatVectorsFormat::fields_writer(&self.flat_vectors_format, state, segment_info)?,
      self.num_merge_workers,
      segment_info,
    )?))
  }

  type KnnVectorsReader<T: IndexInput> =
    Lucene99HnswVectorsReader<Lucene99FlatVectorsReader<T, FlatBitVectorsScorer>, T>;

  fn fields_reader<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::KnnVectorsReader<D1::IndexInput>>
  where
    D1: Directory,
    D2: Directory,
  {
    Lucene99HnswVectorsReader::new(
      state,
      FlatVectorsFormat::fields_reader(&self.flat_vectors_format, state, segment_info)?,
      segment_info,
    )
  }

  fn get_max_dimensions(&self, _field_name: &str) -> Result<usize> {
    Ok(1024)
  }

  fn for_name(name: &str) -> Result<Arc<Self>> {
    static FORMAT: OnceLock<Arc<HnswBitVectorsFormat>> = OnceLock::new();

    match name {
      NAME => {
        if let Some(format) = FORMAT.get() {
          return Ok(Arc::clone(format));
        }
        let format = Arc::new(Self::new()?);
        if FORMAT.set(Arc::clone(&format)).is_ok() {
          Ok(format)
        } else {
          FORMAT.get().map(Arc::clone).ok_or_else(|| {
            LuceneError::illegal_state(format!(
              "failed to initialize vectors format named \"{NAME}\""
            ))
          })
        }
      },
      _ => Err(LuceneError::illegal_argument(format!(
        "Could not load vectors format named \"{name}\""
      ))),
    }
  }
}

impl Display for HnswBitVectorsFormat {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "{}(name={}, maxConn={}, beamWidth={}, flatVectorFormat={})",
      NAME, NAME, self.max_conn, self.beam_width, self.flat_vectors_format
    )
  }
}

pub struct FlatBitVectorsWriter<W>
where
  W: KnnVectorsWriter,
{
  delegate: W,
}

impl<W> FlatBitVectorsWriter<W>
where
  W: KnnVectorsWriter,
{
  pub fn new(delegate: W) -> Self {
    Self { delegate }
  }
}

impl<W> KnnVectorsWriter for FlatBitVectorsWriter<W>
where
  W: KnnVectorsWriter,
{
  type IndexOutput = W::IndexOutput;

  fn merge_one_field<D1, D2, CR>(
    &mut self,
    field_info: &Arc<FieldInfo>,
    merge_state: &MergeState<'_, D1, CR>,
    segment_write_state: &SegmentWriteState<&D2>,
  ) -> Result<()>
  where
    D1: Directory,
    D2: Directory<IndexOutput = Self::IndexOutput>,
    CR: CodecReader,
  {
    self
      .delegate
      .merge_one_field(field_info, merge_state, segment_write_state)
  }

  fn finish(&mut self) -> Result<()> {
    self.delegate.finish()
  }

  fn add_field<D1, D2>(
    &mut self,
    write_state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
    field_info: Arc<FieldInfo>,
  ) -> Result<usize>
  where
    D1: Directory<IndexOutput = Self::IndexOutput>,
    D2: Directory,
  {
    if !matches!(field_info.get_vector_encoding(), VectorEncoding::BYTE(_)) {
      return Err(LuceneError::illegal_argument(
        "HnswBitVectorsFormat only supports BYTE encoding",
      ));
    }
    self
      .delegate
      .add_field(write_state, segment_info, field_info)
  }

  fn flush<DM>(&mut self, max_doc: i32, sort_map: Option<&DM>) -> Result<()>
  where
    DM: DocMap,
  {
    self.delegate.flush(max_doc, sort_map)
  }

  fn add_value(
    &mut self,
    doc_id: i32,
    vector_value: &VectorValueEnum,
    field_vectors_writers_idx: usize,
  ) -> Result<()> {
    self
      .delegate
      .add_value(doc_id, vector_value, field_vectors_writers_idx)
  }
}

impl<W> Closeable for FlatBitVectorsWriter<W>
where
  W: KnnVectorsWriter,
{
  fn close(&mut self) -> Result<()> {
    self.delegate.close()
  }
}

impl<W> Accountable for FlatBitVectorsWriter<W>
where
  W: KnnVectorsWriter,
{
  fn ram_bytes_used(&self) -> Result<i64> {
    self.delegate.ram_bytes_used()
  }
}
