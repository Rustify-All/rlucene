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
use crate::core::codecs::hnsw::default_flat_vector_scorer::DefaultFlatVectorScorer;
use crate::core::codecs::hnsw::flat_vector_scorer_util::GET_LUCENE99_FLAT_VECTORS_SCORER;
use crate::core::codecs::knn_vectors_format::KnnVectorsFormat;
use crate::core::codecs::lucene99::lucene99_flat_vectors_format::Lucene99FlatVectorsFormat;
use crate::core::codecs::lucene99::lucene99_flat_vectors_reader::Lucene99FlatVectorsReader;
use crate::core::codecs::lucene99::lucene99_flat_vectors_writer::Lucene99FlatVectorsWriter;
use crate::core::codecs::lucene99::lucene99_hnsw_vectors_reader::Lucene99HnswVectorsReader;
use crate::core::codecs::lucene99::lucene99_hnsw_vectors_writer::Lucene99HnswVectorsWriter;
use crate::core::index::index_reader::Identity;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::store::directory::Directory;
use crate::core::store::{IndexInput, IndexOutput};
use crate::core::util::HasIdentity;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::hnsw::hnsw_graph_builder::DEFAULT_MAX_CONN as OtherDEFAULT_MAX_CONN;
use std::fmt::{Display, Formatter};
use std::sync::{Arc, LazyLock, OnceLock};

pub(crate) const META_CODEC_NAME: &str = "Lucene99HnswVectorsFormatMeta";
pub(crate) const VECTOR_INDEX_CODEC_NAME: &str = "Lucene99HnswVectorsFormatIndex";
pub(crate) const META_EXTENSION: &str = "vem";
pub(crate) const VECTOR_INDEX_EXTENSION: &str = "vex";

pub const VERSION_START: i32 = 0;
pub const VERSION_CURRENT: i32 = VERSION_START;

/// A maximum configurable maximum max conn.
///
/// NOTE: We eagerly populate `float[MAX_CONN*2]` and `int[MAX_CONN*2]`, so exceptionally large
/// numbers here will use an inordinate amount of heap
pub const MAXIMUM_MAX_CONN: usize = 512;

/// Default number of maximum connections per node
pub const DEFAULT_MAX_CONN: usize = OtherDEFAULT_MAX_CONN;

/// The maximum size of the queue to maintain while searching during graph construction. This
/// maximum value preserves the ratio of the `DEFAULT_BEAM_WIDTH`/`DEFAULT_MAX_CONN` (i.e. `6.25 * 16 = 3200`).
pub const MAXIMUM_BEAM_WIDTH: usize = 3200;

/// Default number of the size of the queue maintained while searching during a graph construction.
pub const DEFAULT_BEAM_WIDTH: usize = DEFAULT_MAX_CONN;

/// Default to use single thread merge
pub const DEFAULT_NUM_MERGE_WORKER: usize = 1;

pub static FLAT_VECTORS_FORMAT: LazyLock<Lucene99FlatVectorsFormat<DefaultFlatVectorScorer>> =
  LazyLock::new(|| {
    let scorer = GET_LUCENE99_FLAT_VECTORS_SCORER.clone();
    Lucene99FlatVectorsFormat::new(scorer)
  });

pub(crate) const DIRECT_MONOTONIC_BLOCK_SHIFT: i32 = 16;
/// Lucene 9.9 vector format, which encodes numeric vector values into an associated graph connecting
/// the documents having values. The graph is used to power HNSW search. The format consists of two
/// files, and requires a `FlatVectorsFormat` to store the actual vectors:
///
/// ## .vex (vector index)
///
/// Stores graphs connecting the documents for each field organized as a list of nodes' neighbours
/// as following:
///
/// - For each level:
///   - For each node:
///     - **`vint`** the number of neighbor nodes
///     - **``array`vint```** the delta encoded neighbor ordinals
/// - After all levels are encoded, memory offsets for each node's neighbor nodes are appended to
///   the end of the file. The offsets are encoded by `DirectMonotonicWriter`.
///
/// ## .vem (vector metadata) file
///
/// For each field:
///
/// - **`int32`** field number
/// - **`int32`** vector similarity function ordinal
/// - **`vlong`** offset to this field's index in the .vex file
/// - **`vlong`** length of this field's index data, in bytes
/// - **`vint`** dimension of this field's vectors
/// - **`int`** the number of documents having values for this field
/// - **`vint`** the maximum number of connections (neighbours) that each node can have
/// - **`vint`** number of levels in the graph
/// - Graph nodes by level. For each level:
///   - **`vint`** the number of nodes on this level
///   - **``array`vint```** for levels greater than 0 list of nodes on this level, stored as
///     the level 0th delta encoded nodes' ordinals.
pub struct Lucene99HnswVectorsFormat {
  max_conn: usize,
  beam_width: usize,
  num_merge_workers: usize,
  identity: Identity,
}
impl Lucene99HnswVectorsFormat {
  /// Constructs a format using default graph construction parameters
  pub fn new() -> Result<Self> {
    Self::with_graph_para_with_threads(DEFAULT_MAX_CONN, DEFAULT_BEAM_WIDTH, DEFAULT_BEAM_WIDTH)
  }
  pub fn with_graph_para(max_conn: usize, beam_width: usize) -> Result<Self> {
    Self::with_graph_para_with_threads(max_conn, beam_width, DEFAULT_NUM_MERGE_WORKER)
  }
  /// Constructs a format using the given graph construction parameters and scalar quantization.
  ///
  /// # Arguments
  ///
  /// - `max_conn`: the maximum number of connections to a node in the HNSW graph
  /// - `beam_width`: the size of the queue maintained during graph construction.
  /// - `num_merge_workers`: number of workers (threads) that will be used when doing merge. If
  ///   larger than 1, a present `ExecutorService` must be passed as `merge_exec`
  ///
  /// # Errors
  ///
  /// Returns an error if the parameters are invalid.
  pub fn with_graph_para_with_threads(
    max_conn: usize,
    beam_width: usize,
    num_merge_workers: usize,
    // TODO IMPORTANT 多线程不支持
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
      identity: Identity::new(),
    })
  }
}
impl Display for Lucene99HnswVectorsFormat {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "Lucene99HnswVectorsFormat(name=Lucene99HnswVectorsFormat, maxConn={}, beamWidth={}, flatVectorFormat={})",
      self.max_conn, self.beam_width, *FLAT_VECTORS_FORMAT,
    )
  }
}

impl HasIdentity for Lucene99HnswVectorsFormat {
  fn identity(&self) -> &Identity {
    &self.identity
  }
}

impl KnnVectorsFormat for Lucene99HnswVectorsFormat {
  fn get_name(&self) -> &str {
    "Lucene99HnswVectorsFormat"
  }

  type KnnVectorsWriter<T: IndexOutput> =
    Lucene99HnswVectorsWriter<Lucene99FlatVectorsWriter<T, DefaultFlatVectorScorer>, T>;

  fn fields_writer<D1, D2>(
    &self,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::KnnVectorsWriter<D1::IndexOutput>>
  where
    D1: Directory,
    D2: Directory,
  {
    let flat_writer = FLAT_VECTORS_FORMAT.fields_writer(state, segment_info)?;
    Lucene99HnswVectorsWriter::new(
      state,
      self.max_conn,
      self.beam_width,
      flat_writer,
      self.num_merge_workers,
      segment_info,
    )
  }

  type KnnVectorsReader<T: IndexInput> =
    Lucene99HnswVectorsReader<Lucene99FlatVectorsReader<T, DefaultFlatVectorScorer>, T>;

  fn fields_reader<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::KnnVectorsReader<D1::IndexInput>>
  where
    D1: Directory,
    D2: Directory,
  {
    let flat_reader = FLAT_VECTORS_FORMAT.fields_reader(state, segment_info)?;
    Lucene99HnswVectorsReader::new(state, flat_reader, segment_info)
  }

  fn get_max_dimensions(&self, _field_name: &str) -> Result<usize> {
    Ok(1024)
  }

  fn for_name(name: &str) -> Result<Arc<Self>> {
    static FORMAT: OnceLock<Arc<Lucene99HnswVectorsFormat>> = OnceLock::new();

    match name {
      "Lucene99HnswVectorsFormat" => {
        if let Some(format) = FORMAT.get() {
          return Ok(Arc::clone(format));
        }
        let format = Arc::new(Self::new()?);
        if FORMAT.set(Arc::clone(&format)).is_ok() {
          Ok(format)
        } else {
          FORMAT.get().map(Arc::clone).ok_or_else(|| {
            LuceneError::illegal_state(
              "failed to initialize vectors format named \"Lucene99HnswVectorsFormat\"",
            )
          })
        }
      },
      _ => Err(LuceneError::illegal_argument(format!(
        "Could not load vectors format named \"{name}\""
      ))),
    }
  }
}
