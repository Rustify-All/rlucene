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
use crate::core::codecs::hnsw::flat_vectors_format::FlatVectorsFormat;
use crate::core::codecs::knn_vectors_format::KnnVectorsFormat;
use crate::core::codecs::lucene99::lucene99_hnsw_vectors_format::{
  DEFAULT_BEAM_WIDTH, DEFAULT_MAX_CONN, DEFAULT_NUM_MERGE_WORKER, MAXIMUM_BEAM_WIDTH,
  MAXIMUM_MAX_CONN,
};
use crate::core::codecs::lucene99::lucene99_hnsw_vectors_reader::Lucene99HnswVectorsReader;
use crate::core::codecs::lucene99::lucene99_hnsw_vectors_writer::Lucene99HnswVectorsWriter;
use crate::core::codecs::lucene99::lucene99_scalar_quantized_vectors_format::Lucene99ScalarQuantizedVectorsFormat;
use crate::core::index::index_reader::Identity;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::store::directory::Directory;
use crate::core::store::{IndexInput, IndexOutput};
use crate::core::util::HasIdentity;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::fmt::{Display, Formatter};
use std::sync::{Arc, OnceLock};

/// Lucene 9.9 vector format, which encodes numeric vector values into an associated graph connecting
/// the documents having values. The graph is used to power HNSW search. The format consists of two
/// files, and uses [`Lucene99ScalarQuantizedVectorsFormat`] to store the actual vectors: For
/// details on graph storage and file extensions, see `Lucene99HnswVectorsFormat`.
pub struct Lucene99HnswScalarQuantizedVectorsFormat {
  max_conn: usize,
  beam_width: usize,
  /// The format for storing, reading, merging vectors on disk
  flat_vectors_format: Lucene99ScalarQuantizedVectorsFormat,
  num_merge_workers: usize,
  identity: Identity,
}

pub const NAME: &str = "Lucene99HnswScalarQuantizedVectorsFormat";

impl Lucene99HnswScalarQuantizedVectorsFormat {
  /// Constructs a format using default graph construction parameters with 7 bit quantization
  pub fn new() -> Result<Self> {
    Self::with_graph_para_with_threads_bits_compress_confidence_interval(
      DEFAULT_MAX_CONN,
      DEFAULT_BEAM_WIDTH,
      DEFAULT_NUM_MERGE_WORKER,
      7,
      false,
      None,
    )
  }

  /// Constructs a format using the given graph construction parameters with 7 bit quantization
  ///
  /// - `max_conn`: the maximum number of connections to a node in the HNSW graph
  /// - `beam_width`: the size of the queue maintained during graph construction.
  pub fn with_graph_para(max_conn: usize, beam_width: usize) -> Result<Self> {
    Self::with_graph_para_with_threads_bits_compress_confidence_interval(
      max_conn,
      beam_width,
      DEFAULT_NUM_MERGE_WORKER,
      7,
      false,
      None,
    )
  }

  /// Constructs a format using the given graph construction parameters and scalar quantization.
  ///
  /// - `max_conn`: the maximum number of connections to a node in the HNSW graph
  /// - `beam_width`: the size of the queue maintained during graph construction.
  /// - `num_merge_workers`: number of workers (threads) that will be used when doing merge.
  /// - `bits`: the number of bits to use for scalar quantization (must be 4 or 7)
  /// - `compress`: whether to compress the quantized vectors by another 50% when bits=4. If
  ///   `true`, pairs of (4 bit quantized) dimensions are packed into a single byte. This must be
  ///   `false` when bits=7. This provides a trade-off of 50% reduction in hot vector memory usage
  ///   during searching, at some decode speed penalty.
  /// - `confidence_interval`: the confidenceInterval for scalar quantizing the vectors, when `None`
  ///   it is calculated based on the vector field dimensions. When `0`, the quantiles are
  ///   dynamically determined by sampling many confidence intervals and determining the most
  ///   accurate pair.
  pub fn with_graph_para_with_threads_bits_compress_confidence_interval(
    max_conn: usize,
    beam_width: usize,
    num_merge_workers: usize,
    bits: i32,
    compress: bool,
    confidence_interval: Option<f32>,
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
      flat_vectors_format:
        Lucene99ScalarQuantizedVectorsFormat::with_confidence_interval_bits_compress(
          confidence_interval,
          bits,
          compress,
        )?,
      num_merge_workers,
      identity: Identity::new(),
    })
  }
}

impl HasIdentity for Lucene99HnswScalarQuantizedVectorsFormat {
  fn identity(&self) -> &Identity {
    &self.identity
  }
}

impl KnnVectorsFormat for Lucene99HnswScalarQuantizedVectorsFormat {
  fn get_name(&self) -> &str {
    NAME
  }

  type KnnVectorsWriter<T: IndexOutput> = Lucene99HnswVectorsWriter<
    <Lucene99ScalarQuantizedVectorsFormat as FlatVectorsFormat>::FlatVectorsWriter<T>,
    T,
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
    Lucene99HnswVectorsWriter::new(
      state,
      self.max_conn,
      self.beam_width,
      FlatVectorsFormat::fields_writer(&self.flat_vectors_format, state, segment_info)?,
      self.num_merge_workers,
      segment_info,
    )
  }

  type KnnVectorsReader<T: IndexInput> = Lucene99HnswVectorsReader<
    <Lucene99ScalarQuantizedVectorsFormat as FlatVectorsFormat>::FlatVectorsReader<T>,
    T,
  >;

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
    static FORMAT: OnceLock<Arc<Lucene99HnswScalarQuantizedVectorsFormat>> = OnceLock::new();

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

impl Display for Lucene99HnswScalarQuantizedVectorsFormat {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "{}(name={}, maxConn={}, beamWidth={}, flatVectorFormat={})",
      NAME, NAME, self.max_conn, self.beam_width, self.flat_vectors_format
    )
  }
}
