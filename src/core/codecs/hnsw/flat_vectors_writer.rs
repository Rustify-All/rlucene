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
use crate::core::codecs::hnsw::flat_field_vectors_writer::FlatFieldVectorsWriter;
use crate::core::codecs::hnsw::flat_vectors_scorer::FlatVectorsScorer;
use crate::core::codecs::knn_vectors_writer::KnnVectorsWriter;
use crate::core::codecs::lucene99::lucene99_hnsw_vectors_writer::{
  DefaultRandomVectorScorerSupplier, FieldWriter,
};
use crate::core::index::codec_reader::CodecReader;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::merge_state::MergeState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::index::sorter::DocMap;
use crate::core::store::directory::Directory;
use crate::core::store::index_output::IndexOutput;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::hnsw::closeable_random_vector_scorer_supplier::CloseableRandomVectorScorerSupplier;
use std::sync::Arc;

/// Vectors' writer for a field that allows additional indexing logic to be implemented by the caller
pub trait FlatVectorsWriter: KnnVectorsWriter<Self::IndexOutput> {
  type IndexOutput: IndexOutput;
  type FlatVectorsScorer: FlatVectorsScorer;
  fn get_flat_vector_scorer(&self) -> &Self::FlatVectorsScorer;
  /// Add a new field for indexing
  ///
  /// # Arguments
  ///
  /// * `field_info` - fieldInfo of the field to add
  ///
  /// # Returns
  ///
  /// A writer for the field.
  ///
  /// # Errors
  ///
  /// Returns an error if an I/O error occurs when adding the field.
  fn flat_add_field(&mut self, field_info: Arc<FieldInfo>) -> Result<usize>;

  /// Flushes all buffered data on disk.
  fn flat_flush<DM, F>(
    &mut self,
    max_doc: i32,
    sort_map: Option<&DM>,
    fields: &[FieldWriter<DefaultRandomVectorScorerSupplier<F>>],
  ) -> Result<()>
  where
    DM: DocMap,
    F: FlatVectorsWriter;

  type FlatFieldVectorsWriter: FlatFieldVectorsWriter;
  fn get_fields_mut(&mut self) -> &mut [Self::FlatFieldVectorsWriter];

  type CloseableRandomVectorScorerSupplier<'a, D>: CloseableRandomVectorScorerSupplier
  where
    D: Directory,
    Self: 'a,
    D: 'a;
  /// Write the field for merging, providing a scorer over the newly merged flat vectors. This way
  /// callers may implement any additional merging logic.
  ///
  /// # Arguments
  ///
  /// * `field_info` - fieldInfo of the field to merge
  /// * `merge_state` - mergeState of the segments to merge
  ///
  /// # Returns
  ///
  /// A scorer over the newly merged flat vectors, which should be closed as it holds a temporary
  /// file handle to read over the newly merged vectors.
  ///
  /// # Errors
  ///
  /// Returns an error if an I/O error occurs when merging.
  fn merge_one_field_to_index<'a, D1, D2, CR>(
    &'a mut self,
    _field_info: &FieldInfo,
    _merge_state: &MergeState<'_, D1, CR>,
    _segment_write_state: &SegmentWriteState<'a, &D2>,
  ) -> Result<Self::CloseableRandomVectorScorerSupplier<'a, D2>>
  where
    D2: Directory<IndexOutput = Self::IndexOutput>,
    CR: CodecReader,
  {
    Err(crate::core::util::error::lucene_error::LuceneError::unsupported_operation(""))
  }
}

pub type FlatVectorsWriterSs<F, BV, FV> =
  <<F as FlatVectorsWriter>::FlatVectorsScorer as FlatVectorsScorer>::RandomVectorScorerSupplier<
    BV,
    FV,
  >;
