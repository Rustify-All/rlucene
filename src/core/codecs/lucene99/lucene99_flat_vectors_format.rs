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
use crate::core::codecs::hnsw::flat_vectors_scorer::FlatVectorsScorer;
use crate::core::codecs::knn_vectors_format::KnnVectorsFormat;
use crate::core::codecs::lucene99::lucene99_flat_vectors_reader::Lucene99FlatVectorsReader;
use crate::core::codecs::lucene99::lucene99_flat_vectors_writer::Lucene99FlatVectorsWriter;
use crate::core::index::index_reader::Identity;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::store::directory::Directory;
use crate::core::store::{IndexInput, IndexOutput};
use crate::core::util::HasIdentity;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub(crate) const NAME: &str = "Lucene99FlatVectorsFormat";
pub(crate) const META_CODEC_NAME: &str = "Lucene99FlatVectorsFormatMeta";
pub(crate) const VECTOR_DATA_CODEC_NAME: &str = "Lucene99FlatVectorsFormatData";
pub(crate) const META_EXTENSION: &str = "vemf";
pub(crate) const VECTOR_DATA_EXTENSION: &str = "vec";

pub(crate) const VERSION_START: i32 = 0;
pub(crate) const VERSION_CURRENT: i32 = VERSION_START;

pub(crate) const DIRECT_MONOTONIC_BLOCK_SHIFT: i32 = 16;
/// Lucene 9.9 flat vector format, which encodes numeric vector values
///
/// ## .vec (vector data) file
///
/// For each field:
///
/// - Vector data ordered by field, document ordinal, and vector dimension. When the
///   vectorEncoding is BYTE, each sample is stored as a single byte. When it is FLOAT32, each
///   sample is stored as an IEEE float in little-endian byte order.
/// - DocIds encoded by `IndexedDISI::write_bit_set`
///   note that only in sparse case
/// - OrdToDoc was encoded by `DirectMonotonicWriter`, note
///   that only in sparse case
///
/// ## .vemf (vector metadata) file
///
/// For each field:
///
/// - **`int32`** field number
/// - **`int32`** vector similarity function ordinal
/// - **`vlong`** offset to this field's vectors in the .vec file
/// - **`vlong`** length of this field's vectors, in bytes
/// - **`vint`** dimension of this field's vectors
/// - **`int`** the number of documents having values for this field
/// - **`int8`** if equals to -2, empty - no vector values. If equals to -1, dense – all
///   documents have values for a field. If equals to 0, sparse – some documents missing values.
/// - DocIds were encoded by `IndexedDISI::write_bit_set`
/// - OrdToDoc was encoded by `DirectMonotonicWriter`, note
///   that only in sparse case
#[derive(Debug)]
pub struct Lucene99FlatVectorsFormat<F>
where
  F: FlatVectorsScorer,
{
  vectors_scorer: F,
  identity: Identity,
}
impl<F> Lucene99FlatVectorsFormat<F>
where
  F: FlatVectorsScorer + Clone,
{
  pub fn new(vectors_scorer: F) -> Self {
    Self {
      vectors_scorer,
      identity: Identity::new(),
    }
  }
}

impl<F> Display for Lucene99FlatVectorsFormat<F>
where
  F: Clone + FlatVectorsScorer,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "Lucene99FlatVectorsFormat(vectorsScorer={})",
      self.vectors_scorer
    )
  }
}

impl<F> HasIdentity for Lucene99FlatVectorsFormat<F>
where
  F: FlatVectorsScorer,
{
  fn identity(&self) -> &Identity {
    &self.identity
  }
}

impl<F> KnnVectorsFormat for Lucene99FlatVectorsFormat<F>
where
  F: Clone + FlatVectorsScorer,
{
  fn get_name(&self) -> &str {
    NAME
  }

  type KnnVectorsWriter<T: IndexOutput> = Lucene99FlatVectorsWriter<T, F>;

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

  type KnnVectorsReader<T: IndexInput> = Lucene99FlatVectorsReader<T, F>;

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
    Err(LuceneError::illegal_argument(format!(
      "Could not load vectors format named \"{name}\""
    )))
  }
}

impl<F> FlatVectorsFormat for Lucene99FlatVectorsFormat<F>
where
  F: FlatVectorsScorer + Clone,
{
  type FlatVectorsWriter<T: IndexOutput> = Lucene99FlatVectorsWriter<T, F>;

  fn fields_writer<D1, D2>(
    &self,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::FlatVectorsWriter<D1::IndexOutput>>
  where
    D1: Directory,
    D2: Directory,
  {
    Lucene99FlatVectorsWriter::new(state, self.vectors_scorer.clone(), segment_info)
  }

  type FlatVectorsReader<T: IndexInput> = Lucene99FlatVectorsReader<T, F>;

  fn fields_reader<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::FlatVectorsReader<D1::IndexInput>>
  where
    D1: Directory,
    D2: Directory,
  {
    Lucene99FlatVectorsReader::new(state, self.vectors_scorer.clone(), segment_info)
  }
}
