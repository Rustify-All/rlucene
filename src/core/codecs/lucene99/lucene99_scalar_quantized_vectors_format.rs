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
use crate::core::codecs::hnsw::flat_vectors_format::FlatVectorsFormat;
use crate::core::codecs::knn_vectors_format::KnnVectorsFormat;
use crate::core::codecs::lucene99::lucene99_flat_vectors_format::Lucene99FlatVectorsFormat;
use crate::core::codecs::lucene99::lucene99_flat_vectors_reader::Lucene99FlatVectorsReader;
use crate::core::codecs::lucene99::lucene99_flat_vectors_writer::Lucene99FlatVectorsWriter;
use crate::core::codecs::lucene99::lucene99_scalar_quantized_vector_scorer::Lucene99ScalarQuantizedVectorScorer;
use crate::core::codecs::lucene99::lucene99_scalar_quantized_vectors_reader::Lucene99ScalarQuantizedVectorsReader;
use crate::core::codecs::lucene99::lucene99_scalar_quantized_vectors_writer::Lucene99ScalarQuantizedVectorsWriter;
use crate::core::index::index_reader::Identity;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::store::directory::Directory;
use crate::core::store::{IndexInput, IndexOutput};
use crate::core::util::HasIdentity;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::fmt::{Display, Formatter};
use std::sync::{Arc, LazyLock, OnceLock};

// The bits that are allowed for scalar quantization
// We only allow signed byte (7), and half-byte (4)
// NOTE: we used to allow 8 bits as well, but it was broken so we removed it
// (https://github.com/apache/lucene/issues/13519)
const ALLOWED_BITS: i32 = (1 << 7) | (1 << 4);
pub const QUANTIZED_VECTOR_COMPONENT: &str = "QVEC";

pub const NAME: &str = "Lucene99ScalarQuantizedVectorsFormat";

pub(crate) const VERSION_START: i32 = 0;
pub(crate) const VERSION_ADD_BITS: i32 = 1;
pub(crate) const VERSION_CURRENT: i32 = VERSION_ADD_BITS;
pub(crate) const META_CODEC_NAME: &str = "Lucene99ScalarQuantizedVectorsFormatMeta";
pub(crate) const VECTOR_DATA_CODEC_NAME: &str = "Lucene99ScalarQuantizedVectorsFormatData";
pub(crate) const META_EXTENSION: &str = "vemq";
pub(crate) const VECTOR_DATA_EXTENSION: &str = "veq";

static RAW_VECTOR_FORMAT: LazyLock<Lucene99FlatVectorsFormat<DefaultFlatVectorScorer>> =
  LazyLock::new(|| {
    let scorer = GET_LUCENE99_FLAT_VECTORS_SCORER.clone();
    Lucene99FlatVectorsFormat::new(scorer)
  });

/// The minimum confidence interval
const MINIMUM_CONFIDENCE_INTERVAL: f32 = 0.9;

/// The maximum confidence interval
const MAXIMUM_CONFIDENCE_INTERVAL: f32 = 1.0;

/// Dynamic confidence interval
pub const DYNAMIC_CONFIDENCE_INTERVAL: f32 = 0.0;

/// Format supporting vector quantization, storage, and retrieval.
pub struct Lucene99ScalarQuantizedVectorsFormat {
  /// Controls the confidence interval used to scalar quantize the vectors the default value is
  /// calculated as `1-1/(vector_dimensions + 1)`
  confidence_interval: Option<f32>,
  bits: u8,
  compress: bool,
  flat_vector_scorer: Lucene99ScalarQuantizedVectorScorer<DefaultFlatVectorScorer>,
  identity: Identity,
}

impl Lucene99ScalarQuantizedVectorsFormat {
  /// Constructs a format using default graph construction parameters
  pub fn new() -> Result<Self> {
    Self::with_confidence_interval_bits_compress(None, 7, false)
  }

  /// Constructs a format using the given graph construction parameters.
  ///
  /// # Arguments
  ///
  /// - `confidence_interval`: the confidenceInterval for scalar quantizing the vectors, when
  ///   `None` it is calculated based on the vector dimension. When `0`, the quantiles are
  ///   dynamically determined by sampling many confidence intervals and determining the most
  ///   accurate pair.
  /// - `bits`: the number of bits to use for scalar quantization (must be between 1 and 8,
  ///   inclusive)
  /// - `compress`: whether to compress the quantized vectors by another 50% when bits=4. If
  ///   `true`, pairs of (4 bit quantized) dimensions are packed into a single byte. This must be
  ///   `false` when bits=7. This provides a trade-off of 50% reduction in hot vector memory usage
  ///   during searching, at some decode speed penalty.
  pub fn with_confidence_interval_bits_compress(
    confidence_interval: Option<f32>,
    bits: i32,
    compress: bool,
  ) -> Result<Self> {
    if let Some(confidence_interval) = confidence_interval
      && confidence_interval != DYNAMIC_CONFIDENCE_INTERVAL
      && !(MINIMUM_CONFIDENCE_INTERVAL..=MAXIMUM_CONFIDENCE_INTERVAL).contains(&confidence_interval)
    {
      return Err(LuceneError::illegal_argument(format!(
        "confidenceInterval must be between {} and {} or 0; confidenceInterval={}",
        MINIMUM_CONFIDENCE_INTERVAL, MAXIMUM_CONFIDENCE_INTERVAL, confidence_interval
      )));
    }

    if !(1..=8).contains(&bits) || (ALLOWED_BITS & (1 << bits)) == 0 {
      return Err(LuceneError::illegal_argument(format!(
        "bits must be one of: 4, 7; bits={}",
        bits
      )));
    }

    if bits > 4 && compress {
      // compress=true otherwise silently does nothing when bits=7?
      return Err(LuceneError::illegal_argument(
        "compress=true only applies when bits=4",
      ));
    }

    Ok(Self {
      bits: bits as u8,
      confidence_interval,
      compress,
      flat_vector_scorer: Lucene99ScalarQuantizedVectorScorer::new(DefaultFlatVectorScorer),
      identity: Identity::new(),
    })
  }

  pub fn calculate_default_confidence_interval(vector_dimension: usize) -> f32 {
    MINIMUM_CONFIDENCE_INTERVAL.max(1.0 - (1.0 / (vector_dimension as f32 + 1.0)))
  }
}

impl Display for Lucene99ScalarQuantizedVectorsFormat {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    let confidence_interval = self
      .confidence_interval
      .map(|value| value.to_string())
      .unwrap_or_else(|| "null".to_string());
    write!(
      f,
      "{}(name={}, confidenceInterval={}, bits={}, compress={}, flatVectorScorer={}, rawVectorFormat={})",
      NAME,
      NAME,
      confidence_interval,
      self.bits,
      self.compress,
      self.flat_vector_scorer,
      *RAW_VECTOR_FORMAT
    )
  }
}

impl HasIdentity for Lucene99ScalarQuantizedVectorsFormat {
  fn identity(&self) -> &Identity {
    &self.identity
  }
}

impl KnnVectorsFormat for Lucene99ScalarQuantizedVectorsFormat {
  fn get_name(&self) -> &str {
    NAME
  }

  type KnnVectorsWriter<T: IndexOutput> = Lucene99ScalarQuantizedVectorsWriter<
    T,
    crate::core::codecs::lucene99::lucene99_flat_vectors_writer::Lucene99FlatVectorsWriter<
      T,
      DefaultFlatVectorScorer,
    >,
    Lucene99ScalarQuantizedVectorScorer<DefaultFlatVectorScorer>,
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
    FlatVectorsFormat::fields_writer(self, state, segment_info)
  }

  type KnnVectorsReader<T: IndexInput> = Lucene99ScalarQuantizedVectorsReader<
    T,
    Lucene99FlatVectorsReader<T, DefaultFlatVectorScorer>,
    Lucene99ScalarQuantizedVectorScorer<DefaultFlatVectorScorer>,
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
    FlatVectorsFormat::fields_reader(self, state, segment_info)
  }

  fn get_max_dimensions(&self, field_name: &str) -> Result<usize> {
    Ok(FlatVectorsFormat::get_max_dimensions(self, field_name))
  }

  fn for_name(name: &str) -> Result<Arc<Self>> {
    static FORMAT: OnceLock<Arc<Lucene99ScalarQuantizedVectorsFormat>> = OnceLock::new();

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

impl FlatVectorsFormat for Lucene99ScalarQuantizedVectorsFormat {
  type FlatVectorsWriter<T: IndexOutput> = Lucene99ScalarQuantizedVectorsWriter<
    T,
    Lucene99FlatVectorsWriter<T, DefaultFlatVectorScorer>,
    Lucene99ScalarQuantizedVectorScorer<DefaultFlatVectorScorer>,
  >;

  fn fields_writer<D1, D2>(
    &self,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::FlatVectorsWriter<D1::IndexOutput>>
  where
    D1: Directory,
    D2: Directory,
  {
    let raw_vector_delegate =
      FlatVectorsFormat::fields_writer(&*RAW_VECTOR_FORMAT, state, segment_info)?;
    Lucene99ScalarQuantizedVectorsWriter::new(
      state,
      self.confidence_interval,
      self.bits,
      self.compress,
      raw_vector_delegate,
      self.flat_vector_scorer.clone(),
      segment_info,
    )
  }

  type FlatVectorsReader<T: IndexInput> = Lucene99ScalarQuantizedVectorsReader<
    T,
    Lucene99FlatVectorsReader<T, DefaultFlatVectorScorer>,
    Lucene99ScalarQuantizedVectorScorer<DefaultFlatVectorScorer>,
  >;

  fn fields_reader<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::FlatVectorsReader<D1::IndexInput>>
  where
    D1: Directory,
    D2: Directory,
  {
    let raw_vectors_reader =
      FlatVectorsFormat::fields_reader(&*RAW_VECTOR_FORMAT, state, segment_info)?;
    Lucene99ScalarQuantizedVectorsReader::new(
      state,
      raw_vectors_reader,
      self.flat_vector_scorer.clone(),
      segment_info,
    )
  }
}
