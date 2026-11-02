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
use crate::core::codecs::hnsw::flat_vectors_scorer::FlatVectorsScorer;
use crate::core::index::byte_vector_values::ByteVectorValues;
use crate::core::index::float_vector_values::FloatVectorValues;
use crate::core::index::knn_vector_values::KnnVectorValues;
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::vector_similarity_function::{VectorSimilarity, VectorSimilarityFunction};
use crate::core::store::memory_segment_index_input::MappedFloatVectors;
use crate::core::util::bits::Bits;
use crate::core::util::clone::TryClone;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::hnsw::random_vector_scorer::RandomVectorScorer;
use crate::core::util::hnsw::random_vector_scorer_supplier::{
  RandomVectorScorerSupplier, vector_values_ram_bytes_used,
};
use std::fmt::{Display, Formatter};

/// Default implementation of [`FlatVectorsScorer`].
#[derive(Default, Clone, Debug)]
pub struct DefaultFlatVectorScorer;
impl TryClone for DefaultFlatVectorScorer {
  fn try_clone(&self) -> Result<Self> {
    Ok(self.clone())
  }
}

impl Display for DefaultFlatVectorScorer {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "DefaultFlatVectorScorer()")
  }
}

impl FlatVectorsScorer for DefaultFlatVectorScorer {
  type ByteScorerSupplier<B, Sim: VectorSimilarity>
    = ByteScoringSupplier<B, Sim>
  where
    B: ByteVectorValues + TryClone + Send,
    B::ByteVectorValues: Send;
  fn get_byte_scorer_supplier<B, Sim: VectorSimilarity>(
    &self,
    similarity_function: Sim,
    vector_values: B,
  ) -> Result<Self::ByteScorerSupplier<B, Sim>>
  where
    B: ByteVectorValues + TryClone + Send,
    B::ByteVectorValues: Send,
  {
    debug_assert!(KnnVectorValues::get_encoding(&vector_values) == VectorEncoding::BYTE(1));
    ByteScoringSupplier::new(vector_values, similarity_function)
  }

  type FloatScorerSupplier<FV, Sim: VectorSimilarity>
    = FloatScoringSupplier<FV, Sim>
  where
    FV: FloatVectorValues + TryClone + Send,
    FV::FloatVectorValues: Send;
  fn get_float_scorer_supplier<FV, Sim: VectorSimilarity>(
    &self,
    similarity_function: Sim,
    vector_values: FV,
  ) -> Result<Self::FloatScorerSupplier<FV, Sim>>
  where
    FV: FloatVectorValues + TryClone + Send,
    FV::FloatVectorValues: Send,
  {
    debug_assert!(KnnVectorValues::get_encoding(&vector_values) == VectorEncoding::FLOAT32(4));
    FloatScoringSupplier::new(vector_values, similarity_function)
  }

  type RandomVectorScorerF32<T, Sim: VectorSimilarity>
    = FloatVectorScorer<T, Sim>
  where
    T: FloatVectorValues;

  fn get_random_vector_scorer_f32<K, TV, Sim: VectorSimilarity>(
    &self,
    similarity_function: Sim,
    vector_values: K,
    target: TV,
  ) -> Result<Self::RandomVectorScorerF32<K, Sim>>
  where
    TV: Into<Vec<f32>>,
    K: FloatVectorValues,
  {
    let target = target.into();
    if target.len() != vector_values.dimension() {
      return Err(LuceneError::illegal_argument(format!(
        "vector query dimension: {} differs from field dimension: {}",
        target.len(),
        vector_values.dimension()
      )));
    }

    Ok(FloatVectorScorer::new(
      vector_values,
      target,
      similarity_function,
    ))
  }

  type RandomVectorScorerU8<T, Sim: VectorSimilarity>
    = ByteVectorScorer<T, Sim>
  where
    T: ByteVectorValues;

  fn get_random_vector_scorer_u8<K, TV, Sim: VectorSimilarity>(
    &self,
    similarity_function: Sim,
    vector_values: K,
    target: TV,
  ) -> Result<Self::RandomVectorScorerU8<K, Sim>>
  where
    TV: Into<Vec<u8>>,
    K: ByteVectorValues,
  {
    let target = target.into();
    if target.len() != vector_values.dimension() {
      return Err(LuceneError::illegal_argument(format!(
        "vector query dimension: {} differs from field dimension: {}",
        target.len(),
        vector_values.dimension()
      )));
    }

    Ok(ByteVectorScorer::new(
      vector_values,
      target,
      similarity_function,
    ))
  }
}
/// RandomVectorScorerSupplier for bytes vector
pub struct ByteScoringSupplier<BV, Sim: VectorSimilarity = VectorSimilarityFunction>
where
  BV: ByteVectorValues,
{
  vectors: BV,
  vectors1: Option<<BV as ByteVectorValues>::ByteVectorValues>,
  vectors2: Option<<BV as ByteVectorValues>::ByteVectorValues>,
  similarity_function: Sim,
}

impl<BV, Sim: VectorSimilarity> ByteScoringSupplier<BV, Sim>
where
  BV: ByteVectorValues,
{
  pub(crate) fn new(vectors: BV, similarity_function: Sim) -> Result<Self> {
    let vectors1 = ByteVectorValues::byte_copy(&vectors)?;
    let vectors2 = ByteVectorValues::byte_copy(&vectors)?;
    debug_assert_eq!(vectors1.is_some(), vectors2.is_some());
    Ok(Self {
      vectors,
      vectors1,
      vectors2,
      similarity_function,
    })
  }
}

impl<BV, Sim: VectorSimilarity> RandomVectorScorerSupplier for ByteScoringSupplier<BV, Sim>
where
  BV: ByteVectorValues + TryClone + Send,
  BV::ByteVectorValues: Send,
{
  type Scorer<'a>
    = RandomVectorScorerByteImpl<'a, BV, Sim>
  where
    Self: 'a;

  type RandomVectorScorerSupplier = Self;

  fn scorer(&self, ord: usize) -> Result<Self::Scorer<'_>> {
    Ok(RandomVectorScorerByteImpl::new(
      &self.vectors,
      &self.vectors1,
      &self.vectors2,
      self.similarity_function,
      ord,
    ))
  }

  fn copy(&self) -> Result<Self>
  where
    Self: Sized,
  {
    ByteScoringSupplier::new(self.vectors.try_clone()?, self.similarity_function)
  }

  fn ram_bytes_used(&self) -> Result<i64> {
    let Ok(vectors) = self.vectors.get_vectors() else {
      return Ok(0);
    };
    let capacity = self.vectors.get_vectors_capacity().unwrap_or(vectors.len());
    vector_values_ram_bytes_used(vectors, capacity)
  }
}

pub struct RandomVectorScorerByteImpl<'a, BV, Sim: VectorSimilarity = VectorSimilarityFunction>
where
  BV: ByteVectorValues,
{
  vectors: &'a BV,
  vectors1: &'a Option<<BV as ByteVectorValues>::ByteVectorValues>,
  vectors2: &'a Option<<BV as ByteVectorValues>::ByteVectorValues>,
  similarity_function: Sim,
  ord: usize,
}

impl<'a, BV, Sim: VectorSimilarity> RandomVectorScorerByteImpl<'a, BV, Sim>
where
  BV: ByteVectorValues,
{
  pub(crate) fn new(
    vectors: &'a BV,
    vectors1: &'a Option<<BV as ByteVectorValues>::ByteVectorValues>,
    vectors2: &'a Option<<BV as ByteVectorValues>::ByteVectorValues>,
    similarity_function: Sim,
    ord: usize,
  ) -> Self {
    debug_assert_eq!(vectors1.is_some(), vectors2.is_some());
    Self {
      vectors,
      vectors1,
      vectors2,
      similarity_function,
      ord,
    }
  }
}

impl<BV, Sim: VectorSimilarity> RandomVectorScorer for RandomVectorScorerByteImpl<'_, BV, Sim>
where
  BV: ByteVectorValues,
{
  #[inline]
  fn score(&self, node: usize) -> Result<f32> {
    debug_assert_eq!(self.vectors1.is_some(), self.vectors2.is_some());

    match (self.vectors1.as_ref(), self.vectors2.as_ref()) {
      (Some(v1), Some(v2)) => score_byte_vectors(v1, v2, self.similarity_function, self.ord, node),
      (None, None) => score_byte_vectors(
        self.vectors,
        self.vectors,
        self.similarity_function,
        self.ord,
        node,
      ),
      _ => Err(LuceneError::illegal_state("should not here")),
    }
  }

  fn max_ord(&self) -> usize {
    self.vectors.size()
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    self.vectors.ord_to_doc(ord)
  }

  type Bits<'a, B>
    = <BV as KnnVectorValues>::Bits<'a, B>
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, accept_docs: Option<B>) -> Result<Option<Self::Bits<'a, B>>>
  where
    B: Bits,
  {
    Ok(self.vectors.get_accept_ords(accept_docs))
  }
}
/// RandomVectorScorerSupplier for Float vector
pub struct FloatScoringSupplier<FV, Sim: VectorSimilarity = VectorSimilarityFunction>
where
  FV: FloatVectorValues,
{
  vectors: FV,
  vectors1: Option<<FV as FloatVectorValues>::FloatVectorValues>,
  vectors2: Option<<FV as FloatVectorValues>::FloatVectorValues>,
  similarity_function: Sim,
}
impl<FV, Sim: VectorSimilarity> FloatScoringSupplier<FV, Sim>
where
  FV: FloatVectorValues + TryClone,
{
  pub(crate) fn new(vectors: FV, similarity_function: Sim) -> Result<Self> {
    let vectors1 = FloatVectorValues::float_copy(&vectors)?;
    let vectors2 = FloatVectorValues::float_copy(&vectors)?;
    Ok(Self {
      vectors,
      vectors1,
      vectors2,
      similarity_function,
    })
  }
}
impl<FV, Sim: VectorSimilarity> RandomVectorScorerSupplier for FloatScoringSupplier<FV, Sim>
where
  FV: FloatVectorValues + TryClone + Send,
  FV::FloatVectorValues: Send,
{
  type Scorer<'a>
    = RandomVectorScorerF32Impl<'a, FV, Sim>
  where
    Self: 'a;

  type RandomVectorScorerSupplier = Self;

  fn scorer(&self, ord: usize) -> Result<Self::Scorer<'_>> {
    Ok(RandomVectorScorerF32Impl::new(
      &self.vectors,
      &self.vectors1,
      &self.vectors2,
      self.similarity_function,
      ord,
    ))
  }

  fn copy(&self) -> Result<Self>
  where
    Self: Sized,
  {
    FloatScoringSupplier::new(self.vectors.try_clone()?, self.similarity_function)
  }

  fn ram_bytes_used(&self) -> Result<i64> {
    let Ok(vectors) = self.vectors.get_vectors() else {
      return Ok(0);
    };
    let capacity = self.vectors.get_vectors_capacity().unwrap_or(vectors.len());
    vector_values_ram_bytes_used(vectors, capacity)
  }
}
pub struct RandomVectorScorerF32Impl<'a, FV, Sim: VectorSimilarity = VectorSimilarityFunction>
where
  FV: FloatVectorValues,
{
  vectors: &'a FV,
  vectors1: &'a Option<<FV as FloatVectorValues>::FloatVectorValues>,
  vectors2: &'a Option<<FV as FloatVectorValues>::FloatVectorValues>,
  similarity_function: Sim,
  ord: usize,
}
impl<'a, FV, Sim: VectorSimilarity> RandomVectorScorerF32Impl<'a, FV, Sim>
where
  FV: FloatVectorValues,
{
  pub(crate) fn new(
    vectors: &'a FV,
    vectors1: &'a Option<<FV as FloatVectorValues>::FloatVectorValues>,
    vectors2: &'a Option<<FV as FloatVectorValues>::FloatVectorValues>,
    similarity_function: Sim,
    ord: usize,
  ) -> Self {
    Self {
      vectors,
      vectors1,
      vectors2,
      similarity_function,
      ord,
    }
  }
}
impl<FV, Sim: VectorSimilarity> RandomVectorScorer for RandomVectorScorerF32Impl<'_, FV, Sim>
where
  FV: FloatVectorValues,
{
  #[inline]
  fn score(&self, node: usize) -> Result<f32> {
    debug_assert_eq!(self.vectors1.is_some(), self.vectors2.is_some());

    match (self.vectors1.as_ref(), self.vectors2.as_ref()) {
      (Some(v1), Some(v2)) => score_float_vectors(v1, v2, self.similarity_function, self.ord, node),
      (None, None) => score_float_vectors(
        self.vectors,
        self.vectors,
        self.similarity_function,
        self.ord,
        node,
      ),
      _ => Err(LuceneError::illegal_state("should not here")),
    }
  }

  fn max_ord(&self) -> usize {
    self.vectors.size()
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    self.vectors.ord_to_doc(ord)
  }

  type Bits<'a, B>
    = <FV as KnnVectorValues>::Bits<'a, B>
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, accept_docs: Option<B>) -> Result<Option<Self::Bits<'a, B>>>
  where
    B: Bits,
  {
    Ok(self.vectors.get_accept_ords(accept_docs))
  }
}

#[inline]
fn score_float_vectors<FV: FloatVectorValues, Sim: VectorSimilarity>(
  vectors1: &FV,
  vectors2: &FV,
  similarity: Sim,
  ord: usize,
  node: usize,
) -> Result<f32> {
  let ord_vector = vectors1.vector_value(ord)?;
  let node_vector = vectors2.vector_value(node)?;
  similarity.compare_f32(ord_vector.as_ref(), node_vector.as_ref())
}

#[inline]
fn score_byte_vectors<BV: ByteVectorValues, Sim: VectorSimilarity>(
  vectors1: &BV,
  vectors2: &BV,
  similarity: Sim,
  ord: usize,
  node: usize,
) -> Result<f32> {
  let ord_vector = vectors1.vector_value(ord)?;
  let node_vector = vectors2.vector_value(node)?;
  similarity.compare_u8(ord_vector.as_ref(), node_vector.as_ref())
}

pub struct FloatVectorScorer<FV, Sim: VectorSimilarity = VectorSimilarityFunction> {
  values: FV,
  query: Box<[f32]>,
  similarity_function: Sim,
}

impl<FV, Sim: VectorSimilarity> FloatVectorScorer<FV, Sim> {
  pub(crate) fn new(values: FV, query: Vec<f32>, similarity_function: Sim) -> Self {
    Self {
      values,
      query: query.into_boxed_slice(),
      similarity_function,
    }
  }
}

impl<FV, Sim: VectorSimilarity> RandomVectorScorer for FloatVectorScorer<FV, Sim>
where
  FV: FloatVectorValues,
{
  fn mapped_float_vectors(&self) -> Option<MappedFloatVectors> {
    if !Sim::SUPPORTS_MAPPED_FLOAT_INPUT {
      return None;
    }
    let vectors = self.values.mapped_float_vectors()?;
    (vectors.dimension() == self.query.len()).then_some(vectors)
  }

  #[inline]
  fn score_mapped_float(&self, node: usize, vector: &[f32]) -> Result<f32> {
    match self
      .values
      .compare_mapped_float(&self.query, vector, self.similarity_function)
    {
      Some(result) => result,
      None => self.score(node),
    }
  }

  #[inline]
  fn score(&self, node: usize) -> Result<f32> {
    let value = self.values.vector_value(node)?;

    self
      .similarity_function
      .compare_f32(&self.query, value.as_ref())
  }

  fn max_ord(&self) -> usize {
    self.values.size()
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    self.values.ord_to_doc(ord)
  }

  type Bits<'a, B>
    = <FV as KnnVectorValues>::Bits<'a, B>
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, accept_docs: Option<B>) -> Result<Option<Self::Bits<'a, B>>>
  where
    B: Bits,
  {
    Ok(self.values.get_accept_ords(accept_docs))
  }
}
pub struct ByteVectorScorer<BV, Sim: VectorSimilarity = VectorSimilarityFunction> {
  values: BV,
  query: Box<[u8]>,
  similarity_function: Sim,
}
impl<BV, Sim: VectorSimilarity> ByteVectorScorer<BV, Sim> {
  pub(crate) fn new(values: BV, query: Vec<u8>, similarity_function: Sim) -> Self {
    Self {
      values,
      query: query.into_boxed_slice(),
      similarity_function,
    }
  }
}
impl<BV, Sim: VectorSimilarity> RandomVectorScorer for ByteVectorScorer<BV, Sim>
where
  BV: ByteVectorValues,
{
  #[inline]
  fn score(&self, node: usize) -> Result<f32> {
    let value = self.values.vector_value(node)?;
    self
      .similarity_function
      .compare_u8(&self.query, value.as_ref())
  }

  fn max_ord(&self) -> usize {
    self.values.size()
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    self.values.ord_to_doc(ord)
  }

  type Bits<'a, B>
    = <BV as KnnVectorValues>::Bits<'a, B>
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, accept_docs: Option<B>) -> Result<Option<Self::Bits<'a, B>>>
  where
    B: Bits,
  {
    Ok(self.values.get_accept_ords(accept_docs))
  }
}

use crate::core::util::hnsw::random_vector_scorer_supplier::BufferedVectorScorerSupplier;

impl<BV, Sim: VectorSimilarity> BufferedVectorScorerSupplier for ByteScoringSupplier<BV, Sim>
where
  BV: ByteVectorValues + TryClone + Send,
  BV::ByteVectorValues: Send,
{
  type Element = u8;
  fn get_vector(&self) -> Result<&[Box<[u8]>]> {
    self.vectors.get_vectors()
  }
  fn get_vector_mut(&mut self) -> Result<&mut Vec<Box<[u8]>>> {
    self.vectors.get_vectors_mut()
  }
}

impl<FV, Sim: VectorSimilarity> BufferedVectorScorerSupplier for FloatScoringSupplier<FV, Sim>
where
  FV: FloatVectorValues + TryClone + Send,
  FV::FloatVectorValues: Send,
{
  type Element = f32;
  fn get_vector(&self) -> Result<&[Box<[f32]>]> {
    self.vectors.get_vectors()
  }
  fn get_vector_mut(&mut self) -> Result<&mut Vec<Box<[f32]>>> {
    self.vectors.get_vectors_mut()
  }
}
