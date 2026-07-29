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
use crate::core::codecs::DefaultKnnVectorsFormat;
use crate::core::codecs::knn_vectors_reader::KnnVectorsReader;
use crate::core::codecs::knn_vectors_writer::KnnVectorsWriter;
use crate::core::index::segment_info::SegmentInfo;
use crate::core::index::segment_read_state::SegmentReadState;
use crate::core::index::segment_write_state::SegmentWriteState;
use crate::core::store::directory::Directory;
use crate::core::store::{IndexInput, IndexOutput};
use crate::core::util::HasIdentity;
use crate::core::util::error::lucene_error::Result;
use std::fmt::Display;
use std::sync::Arc;

/// The maximum number of vector dimensions.
pub const DEFAULT_MAX_DIMENSIONS: usize = 1024;

pub trait KnnVectorsFormat: Display + HasIdentity {
  /// Returns this vectors format's name.
  fn get_name(&self) -> &str;

  type KnnVectorsWriter<T: IndexOutput>: KnnVectorsWriter<IndexOutput = T>;
  /// Returns a [`KnnVectorsWriter`] to write the vectors to the index.
  fn fields_writer<D1, D2>(
    &self,
    state: &SegmentWriteState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::KnnVectorsWriter<D1::IndexOutput>>
  where
    D1: Directory,
    D2: Directory;
  type KnnVectorsReader<T: IndexInput>: KnnVectorsReader;
  /// Returns a [`KnnVectorsReader`] to write the vectors to the index.
  fn fields_reader<D1, D2>(
    &self,
    state: &SegmentReadState<D1>,
    segment_info: &SegmentInfo<D2>,
  ) -> Result<Self::KnnVectorsReader<D1::IndexInput>>
  where
    D1: Directory,
    D2: Directory;
  /// Returns the maximum number of vector dimensions supported by this codec for the given field
  /// name
  ///
  /// Codecs implement this method to specify the maximum number of dimensions they support.
  ///
  /// # Arguments
  /// * `field_name` - the field name
  ///
  /// # Returns
  /// the maximum number of vector dimensions.
  fn get_max_dimensions(&self, field_name: &str) -> Result<usize>;

  /// Looks up a format by name.
  fn for_name(name: &str) -> Result<Arc<Self>>
  where
    Self: Sized;
}

pub type DefaultKnnVectorsWriter<T> =
  <DefaultKnnVectorsFormat as KnnVectorsFormat>::KnnVectorsWriter<T>;
