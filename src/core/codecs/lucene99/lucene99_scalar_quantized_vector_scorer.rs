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
use crate::core::index::vector_similarity_function::{VectorSimilarity, VectorSimilarityFunction};
use crate::core::util::CoreHelper;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bits::Bits;
use crate::core::util::clone::TryClone;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::hnsw::random_vector_scorer::RandomVectorScorer;
use crate::core::util::hnsw::random_vector_scorer_supplier::RandomVectorScorerSupplier;
use crate::core::util::quantization::quantized_byte_vector_values::QuantizedByteVectorValues;
use crate::core::util::quantization::scalar_quantizer::ScalarQuantizer;
use crate::core::util::vector_util::{VECTOR_UTIL, VectorUtil};
use std::borrow::Cow;
use std::fmt::{Display, Formatter};

/// Optimized scalar quantized implementation of [`FlatVectorsScorer`] for quantized vectors
/// stored in the Lucene99 format.
#[derive(Clone, Debug)]
pub struct Lucene99ScalarQuantizedVectorScorer<F> {
  non_quantized_delegate: F,
}

impl<F> Lucene99ScalarQuantizedVectorScorer<F> {
  pub fn new(flat_vectors_scorer: F) -> Self {
    Self {
      non_quantized_delegate: flat_vectors_scorer,
    }
  }
}

impl<F> Lucene99ScalarQuantizedVectorScorer<F>
where
  F: FlatVectorsScorer,
{
  pub(crate) fn get_random_vector_scorer_supplier<V>(
    &self,
    similarity_function: VectorSimilarityFunction,
    vector_values: V,
  ) -> Result<ScalarQuantizedRandomVectorScorerSupplier<V>>
  where
    V: QuantizedByteVectorValues<QuantizedByteVectorValues = V> + Send,
  {
    ScalarQuantizedRandomVectorScorerSupplier::new(vector_values, similarity_function)
  }

  pub(crate) fn get_random_vector_scorer_f32<'a, V, TV>(
    &self,
    similarity_function: VectorSimilarityFunction,
    vector_values: V,
    target: TV,
  ) -> Result<ScalarQuantizedRandomVectorScorerEnum<V>>
  where
    TV: Into<Cow<'a, [f32]>>,
    V: QuantizedByteVectorValues,
  {
    let target = target.into();
    let scalar_quantizer = vector_values.get_scalar_quantizer()?;
    let mut target_bytes = vec![0; target.len()];
    let offset_correction = quantize_query(
      target,
      &mut target_bytes,
      similarity_function,
      &scalar_quantizer,
    )?;
    from_vector_similarity(
      target_bytes,
      offset_correction,
      similarity_function,
      scalar_quantizer.get_constant_multiplier(),
      vector_values,
    )
  }
}

pub trait ScalarQuantizedVectorsScorer: FlatVectorsScorer + Clone {
  type QuantizedRandomVectorScorerSupplier<V>: RandomVectorScorerSupplier
  where
    V: QuantizedByteVectorValues<QuantizedByteVectorValues = V> + Send;

  fn get_random_vector_scorer_supplier_quantized<V>(
    &self,
    similarity_function: VectorSimilarityFunction,
    vector_values: V,
  ) -> Result<Self::QuantizedRandomVectorScorerSupplier<V>>
  where
    V: QuantizedByteVectorValues<QuantizedByteVectorValues = V> + Send;

  type QuantizedRandomVectorScorer<V>: RandomVectorScorer
  where
    V: QuantizedByteVectorValues;

  fn get_random_vector_scorer_f32_quantized<V, TV>(
    &self,
    similarity_function: VectorSimilarityFunction,
    vector_values: V,
    target: TV,
  ) -> Result<Self::QuantizedRandomVectorScorer<V>>
  where
    TV: Into<Vec<f32>>,
    V: QuantizedByteVectorValues;
}

impl<F> ScalarQuantizedVectorsScorer for Lucene99ScalarQuantizedVectorScorer<F>
where
  F: FlatVectorsScorer + Clone,
{
  type QuantizedRandomVectorScorerSupplier<V>
    = ScalarQuantizedRandomVectorScorerSupplier<V>
  where
    V: QuantizedByteVectorValues<QuantizedByteVectorValues = V> + Send;

  fn get_random_vector_scorer_supplier_quantized<V>(
    &self,
    similarity_function: VectorSimilarityFunction,
    vector_values: V,
  ) -> Result<Self::QuantizedRandomVectorScorerSupplier<V>>
  where
    V: QuantizedByteVectorValues<QuantizedByteVectorValues = V> + Send,
  {
    self.get_random_vector_scorer_supplier(similarity_function, vector_values)
  }

  type QuantizedRandomVectorScorer<V>
    = ScalarQuantizedRandomVectorScorerEnum<V>
  where
    V: QuantizedByteVectorValues;

  fn get_random_vector_scorer_f32_quantized<V, TV>(
    &self,
    similarity_function: VectorSimilarityFunction,
    vector_values: V,
    target: TV,
  ) -> Result<Self::QuantizedRandomVectorScorer<V>>
  where
    TV: Into<Vec<f32>>,
    V: QuantizedByteVectorValues,
  {
    let target = target.into();
    self.get_random_vector_scorer_f32(similarity_function, vector_values, target)
  }
}

impl<F> Display for Lucene99ScalarQuantizedVectorScorer<F>
where
  F: Display,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "ScalarQuantizedVectorScorer(nonQuantizedDelegate={})",
      self.non_quantized_delegate
    )
  }
}

impl<F> FlatVectorsScorer for Lucene99ScalarQuantizedVectorScorer<F>
where
  F: FlatVectorsScorer,
{
  type ByteScorerSupplier<B, Sim: VectorSimilarity>
    = F::ByteScorerSupplier<B, Sim>
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
    self
      .non_quantized_delegate
      .get_byte_scorer_supplier(similarity_function, vector_values)
  }

  type FloatScorerSupplier<FV, Sim: VectorSimilarity>
    = F::FloatScorerSupplier<FV, Sim>
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
    self
      .non_quantized_delegate
      .get_float_scorer_supplier(similarity_function, vector_values)
  }

  type RandomVectorScorerF32<T, Sim: VectorSimilarity>
    = F::RandomVectorScorerF32<T, Sim>
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
    // It is possible to get to this branch during initial indexing and flush.
    self.non_quantized_delegate.get_random_vector_scorer_f32(
      similarity_function,
      vector_values,
      target,
    )
  }

  type RandomVectorScorerU8<T, Sim: VectorSimilarity>
    = F::RandomVectorScorerU8<T, Sim>
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
    self.non_quantized_delegate.get_random_vector_scorer_u8(
      similarity_function,
      vector_values,
      target,
    )
  }
}

fn quantize_query(
  mut query: Cow<'_, [f32]>,
  quantized_query: &mut [u8],
  similarity_function: VectorSimilarityFunction,
  scalar_quantizer: &ScalarQuantizer,
) -> Result<f32> {
  if similarity_function == VectorSimilarityFunction::Cosine {
    VectorUtil::l2normalize(query.to_mut())?;
  }
  Ok(scalar_quantizer.quantize(&query, quantized_query, similarity_function))
}

fn from_vector_similarity<V>(
  target_bytes: Vec<u8>,
  offset_correction: f32,
  sim: VectorSimilarityFunction,
  const_multiplier: f32,
  values: V,
) -> Result<ScalarQuantizedRandomVectorScorerEnum<V>>
where
  V: QuantizedByteVectorValues,
{
  match sim {
    VectorSimilarityFunction::Euclidean => Ok(ScalarQuantizedRandomVectorScorerEnum::Euclidean(
      Euclidean::new(values, const_multiplier, target_bytes),
    )),
    VectorSimilarityFunction::Cosine | VectorSimilarityFunction::DotProduct => dot_product_factory(
      target_bytes,
      offset_correction,
      const_multiplier,
      values,
      ScoreAdjustmentFunction::DotProduct,
    ),
    VectorSimilarityFunction::MaximumInnerProduct => dot_product_factory(
      target_bytes,
      offset_correction,
      const_multiplier,
      values,
      ScoreAdjustmentFunction::MaximumInnerProduct,
    ),
  }
}

fn dot_product_factory<V>(
  target_bytes: Vec<u8>,
  offset_correction: f32,
  const_multiplier: f32,
  values: V,
  score_adjustment_function: ScoreAdjustmentFunction,
) -> Result<ScalarQuantizedRandomVectorScorerEnum<V>>
where
  V: QuantizedByteVectorValues,
{
  if values.get_scalar_quantizer()?.get_bits() <= 4 {
    if values.get_vector_byte_length() != values.dimension() {
      return Ok(
        ScalarQuantizedRandomVectorScorerEnum::CompressedInt4DotProduct(
          CompressedInt4DotProduct::new(
            values,
            const_multiplier,
            target_bytes,
            offset_correction,
            score_adjustment_function,
          ),
        ),
      );
    }
    return Ok(ScalarQuantizedRandomVectorScorerEnum::Int4DotProduct(
      Int4DotProduct::new(
        values,
        const_multiplier,
        target_bytes,
        offset_correction,
        score_adjustment_function,
      ),
    ));
  }
  Ok(ScalarQuantizedRandomVectorScorerEnum::DotProduct(
    DotProduct::new(
      values,
      const_multiplier,
      target_bytes,
      offset_correction,
      score_adjustment_function,
    ),
  ))
}

pub enum ScalarQuantizedRandomVectorScorerEnum<V> {
  Euclidean(Euclidean<V>),
  DotProduct(DotProduct<V>),
  CompressedInt4DotProduct(CompressedInt4DotProduct<V>),
  Int4DotProduct(Int4DotProduct<V>),
}

impl<V> RandomVectorScorer for ScalarQuantizedRandomVectorScorerEnum<V>
where
  V: QuantizedByteVectorValues,
{
  fn score(&self, node: usize) -> Result<f32> {
    match self {
      Self::Euclidean(inner) => inner.score(node),
      Self::DotProduct(inner) => inner.score(node),
      Self::CompressedInt4DotProduct(inner) => inner.score(node),
      Self::Int4DotProduct(inner) => inner.score(node),
    }
  }

  fn max_ord(&self) -> usize {
    match self {
      Self::Euclidean(inner) => inner.max_ord(),
      Self::DotProduct(inner) => inner.max_ord(),
      Self::CompressedInt4DotProduct(inner) => inner.max_ord(),
      Self::Int4DotProduct(inner) => inner.max_ord(),
    }
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    match self {
      Self::Euclidean(inner) => inner.ord_to_doc(ord),
      Self::DotProduct(inner) => inner.ord_to_doc(ord),
      Self::CompressedInt4DotProduct(inner) => inner.ord_to_doc(ord),
      Self::Int4DotProduct(inner) => inner.ord_to_doc(ord),
    }
  }

  type Bits<'a, B>
    = <V as crate::core::index::knn_vector_values::KnnVectorValues>::Bits<'a, B>
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, accept_docs: Option<B>) -> Result<Option<Self::Bits<'a, B>>>
  where
    B: Bits,
  {
    match self {
      Self::Euclidean(inner) => inner.get_accept_ords(accept_docs),
      Self::DotProduct(inner) => inner.get_accept_ords(accept_docs),
      Self::CompressedInt4DotProduct(inner) => inner.get_accept_ords(accept_docs),
      Self::Int4DotProduct(inner) => inner.get_accept_ords(accept_docs),
    }
  }
}

pub struct Euclidean<V> {
  values: V,
  const_multiplier: f32,
  target_bytes: Box<[u8]>,
}

impl<V> Euclidean<V> {
  fn new(values: V, const_multiplier: f32, target_bytes: Vec<u8>) -> Self {
    Self {
      values,
      const_multiplier,
      target_bytes: target_bytes.into_boxed_slice(),
    }
  }
}

impl<V> RandomVectorScorer for Euclidean<V>
where
  V: QuantizedByteVectorValues,
{
  fn score(&self, node: usize) -> Result<f32> {
    let node_vector = self.values.vector_value(node)?;
    let square_distance =
      VECTOR_UTIL.square_distance_u8(node_vector.as_ref(), &self.target_bytes)?;
    let adjusted_distance = square_distance as f32 * self.const_multiplier;
    Ok(1.0 / (1.0 + adjusted_distance))
  }

  fn max_ord(&self) -> usize {
    self.values.size()
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    self.values.ord_to_doc(ord)
  }

  type Bits<'a, B>
    = <V as crate::core::index::knn_vector_values::KnnVectorValues>::Bits<'a, B>
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

/// Calculates dot product on quantized vectors, applying the appropriate corrections
pub struct DotProduct<V> {
  values: V,
  const_multiplier: f32,
  target_bytes: Box<[u8]>,
  offset_correction: f32,
  score_adjustment_function: ScoreAdjustmentFunction,
}

impl<V> DotProduct<V> {
  fn new(
    values: V,
    const_multiplier: f32,
    target_bytes: Vec<u8>,
    offset_correction: f32,
    score_adjustment_function: ScoreAdjustmentFunction,
  ) -> Self {
    Self {
      values,
      const_multiplier,
      target_bytes: target_bytes.into_boxed_slice(),
      offset_correction,
      score_adjustment_function,
    }
  }
}

impl<V> RandomVectorScorer for DotProduct<V>
where
  V: QuantizedByteVectorValues,
{
  fn score(&self, vector_ordinal: usize) -> Result<f32> {
    let stored_vector = self.values.vector_value(vector_ordinal)?;
    let vector_offset = self.values.get_score_correction_constant(vector_ordinal)?;
    let dot_product = VECTOR_UTIL.dot_product_u8(stored_vector.as_ref(), &self.target_bytes)?;
    debug_assert!(dot_product >= 0);
    let adjusted_distance =
      dot_product as f32 * self.const_multiplier + self.offset_correction + vector_offset;
    Ok(self.score_adjustment_function.apply(adjusted_distance))
  }

  fn max_ord(&self) -> usize {
    self.values.size()
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    self.values.ord_to_doc(ord)
  }

  type Bits<'a, B>
    = <V as crate::core::index::knn_vector_values::KnnVectorValues>::Bits<'a, B>
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

pub struct CompressedInt4DotProduct<V> {
  values: V,
  const_multiplier: f32,
  target_bytes: Box<[u8]>,
  offset_correction: f32,
  score_adjustment_function: ScoreAdjustmentFunction,
}

impl<V> CompressedInt4DotProduct<V> {
  fn new(
    values: V,
    const_multiplier: f32,
    target_bytes: Vec<u8>,
    offset_correction: f32,
    score_adjustment_function: ScoreAdjustmentFunction,
  ) -> Self {
    Self {
      values,
      const_multiplier,
      target_bytes: target_bytes.into_boxed_slice(),
      offset_correction,
      score_adjustment_function,
    }
  }
}

impl<V> RandomVectorScorer for CompressedInt4DotProduct<V>
where
  V: QuantizedByteVectorValues,
{
  fn score(&self, vector_ordinal: usize) -> Result<f32> {
    let compressed_vector_len = self.values.get_vector_byte_length();
    stack_or_heap_buffer!(compressed_vector, u8, compressed_vector_len, 128, 0);
    self
      .values
      .seek(vector_ordinal * (self.values.get_vector_byte_length() + BitUtil::FLOAT_BYTES))?;
    self
      .values
      .read_bytes(compressed_vector, 0, compressed_vector_len)?;
    let vector_offset = self.values.get_score_correction_constant(vector_ordinal)?;
    let dot_product = VECTOR_UTIL.int4_dot_product_packed(&self.target_bytes, compressed_vector)?;
    debug_assert!(dot_product >= 0);
    let adjusted_distance =
      dot_product as f32 * self.const_multiplier + self.offset_correction + vector_offset;
    Ok(self.score_adjustment_function.apply(adjusted_distance))
  }

  fn max_ord(&self) -> usize {
    self.values.size()
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    self.values.ord_to_doc(ord)
  }

  type Bits<'a, B>
    = <V as crate::core::index::knn_vector_values::KnnVectorValues>::Bits<'a, B>
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

pub struct Int4DotProduct<V> {
  values: V,
  const_multiplier: f32,
  target_bytes: Box<[u8]>,
  offset_correction: f32,
  score_adjustment_function: ScoreAdjustmentFunction,
}

impl<V> Int4DotProduct<V> {
  fn new(
    values: V,
    const_multiplier: f32,
    target_bytes: Vec<u8>,
    offset_correction: f32,
    score_adjustment_function: ScoreAdjustmentFunction,
  ) -> Self {
    Self {
      values,
      const_multiplier,
      target_bytes: target_bytes.into_boxed_slice(),
      offset_correction,
      score_adjustment_function,
    }
  }
}

impl<V> RandomVectorScorer for Int4DotProduct<V>
where
  V: QuantizedByteVectorValues,
{
  fn score(&self, vector_ordinal: usize) -> Result<f32> {
    let stored_vector = self.values.vector_value(vector_ordinal)?;
    let vector_offset = self.values.get_score_correction_constant(vector_ordinal)?;
    let dot_product = VECTOR_UTIL.int4_dot_product(stored_vector.as_ref(), &self.target_bytes)?;
    debug_assert!(dot_product >= 0);
    let adjusted_distance =
      dot_product as f32 * self.const_multiplier + self.offset_correction + vector_offset;
    Ok(self.score_adjustment_function.apply(adjusted_distance))
  }

  fn max_ord(&self) -> usize {
    self.values.size()
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    self.values.ord_to_doc(ord)
  }

  type Bits<'a, B>
    = <V as crate::core::index::knn_vector_values::KnnVectorValues>::Bits<'a, B>
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

#[derive(Clone, Copy)]
enum ScoreAdjustmentFunction {
  DotProduct,
  MaximumInnerProduct,
}

impl ScoreAdjustmentFunction {
  fn apply(self, f: f32) -> f32 {
    match self {
      Self::DotProduct => CoreHelper::max_f32((1.0 + f) / 2.0, 0.0),
      Self::MaximumInnerProduct => VectorUtil::scale_max_inner_product_score(f),
    }
  }
}

pub struct ScalarQuantizedRandomVectorScorerSupplier<V> {
  vector_similarity_function: VectorSimilarityFunction,
  values: V,
  values1: V,
  values2: V,
}

impl<V> ScalarQuantizedRandomVectorScorerSupplier<V>
where
  V: QuantizedByteVectorValues<QuantizedByteVectorValues = V>,
{
  pub(crate) fn new(
    values: V,
    vector_similarity_function: VectorSimilarityFunction,
  ) -> Result<Self> {
    let values1 = QuantizedByteVectorValues::copy(&values)?;
    let values2 = QuantizedByteVectorValues::copy(&values)?;
    Ok(Self {
      vector_similarity_function,
      values,
      values1,
      values2,
    })
  }
}

impl<V> RandomVectorScorerSupplier for ScalarQuantizedRandomVectorScorerSupplier<V>
where
  V: QuantizedByteVectorValues<QuantizedByteVectorValues = V> + Send,
{
  type Scorer<'a>
    = ScalarQuantizedRandomVectorScorerEnum<V>
  where
    Self: 'a;

  type RandomVectorScorerSupplier = Self;

  fn scorer(&self, ord: usize) -> Result<Self::Scorer<'_>> {
    let vector_value = self.values1.vector_value(ord)?;
    let vector_value = Cow::<[u8]>::from(Into::<
      crate::core::index::byte_vector_values::ByteVectorValue<'_>,
    >::into(vector_value))
    .into_owned();
    let offset_correction = self.values1.get_score_correction_constant(ord)?;
    from_vector_similarity(
      vector_value,
      offset_correction,
      self.vector_similarity_function,
      self
        .values
        .get_scalar_quantizer()?
        .get_constant_multiplier(),
      QuantizedByteVectorValues::copy(&self.values2)?,
    )
  }

  fn copy(&self) -> Result<Self::RandomVectorScorerSupplier>
  where
    Self: Sized,
  {
    Self::new(
      QuantizedByteVectorValues::copy(&self.values)?,
      self.vector_similarity_function,
    )
  }
}

impl<V> Display for ScalarQuantizedRandomVectorScorerSupplier<V> {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "ScalarQuantizedRandomVectorScorerSupplier(vectorSimilarityFunction={})",
      self.vector_similarity_function
    )
  }
}
