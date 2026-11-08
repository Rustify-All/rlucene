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
use crate::core::codecs::lucene95::has_index_slice::HasIndexSlice;
use crate::core::index::byte_vector_values::ByteVectorValues;
use crate::core::search::vector_scorer::VectorScorer;
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::quantization::scalar_quantizer::ScalarQuantizer;

/// A version of [`ByteVectorValues`], but additionally retrieving score correction offset for
/// Scalar quantization scores.
pub trait QuantizedByteVectorValues: ByteVectorValues + HasIndexSlice {
  fn get_scalar_quantizer(&self) -> Result<ScalarQuantizer> {
    Err(LuceneError::unsupported_operation(""))
  }

  /// Lends the quantized bytes for the duration of a single operation.
  fn with_quantized_value<R, UseValue>(&self, ord: usize, use_value: UseValue) -> Result<R>
  where
    UseValue: FnOnce(&[u8]) -> Result<R>,
  {
    use_value(self.vector_value(ord)?.as_ref())
  }

  fn get_score_correction_constant(&self, ord: usize) -> Result<f32>;

  /// Return a `VectorScorer` for the given query vector.
  ///
  /// # Arguments
  /// * `query` - the query vector
  ///
  /// # Returns
  /// a `VectorScorer` instance or None
  type QuantizedVectorScorer: VectorScorer;
  fn scorer(&self, _query: &[f32]) -> Result<Option<Self::QuantizedVectorScorer>> {
    Err(LuceneError::unsupported_operation(""))
  }

  type QuantizedByteVectorValues: QuantizedByteVectorValues;

  fn copy(&self) -> Result<Self::QuantizedByteVectorValues>;
}
