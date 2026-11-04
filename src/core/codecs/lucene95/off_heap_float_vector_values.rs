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
use crate::core::codecs::indexed_disi::{
  DocIndexIteratorImpl, IndexedDISIDocIndexIterator, IndexedDISIImpl, get_doc_index_iterator,
};
use crate::core::codecs::lucene95::has_index_slice::HasIndexSlice;
use crate::core::codecs::lucene95::ord_to_doc_disi_reader_configuration::OrdToDocDISIReaderConfiguration;
use crate::core::codecs::off_heap_vector_values::{
  OffHeapVectorValueBits, SparseOffHeapVectorValueBits,
};
use crate::core::index::dummy::dummy_float_vector_values::DummyFloatVectorValues;
use crate::core::index::dummy::dummy_knn_vector_values::DummyKnnVectorsWriter;
use crate::core::index::float_vector_values::FloatVectorValues;
use crate::core::index::knn_vector_values::{
  DenseDocIndexIterator, DocIndexIterator, KnnVectorValues, create_dense_iterator,
};
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::vector_similarity_function::VectorSimilarity;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::{BitSetIteratorAccess, DocIdSetIteratorExtensions};
use crate::core::search::dummy::dummy_vector_scorer::DummyVectorScorer;
use crate::core::search::vector_scorer::VectorScorer;
use crate::core::store::IndexInput;
use crate::core::store::memory_segment_index_input::MappedFloatVectors;
use crate::core::util::TryIntoInt;
use crate::core::util::bits::Bits;
use crate::core::util::clone::TryClone;
use crate::core::util::dummy::dummy_bits::DummyBits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::hnsw::random_vector_scorer::RandomVectorScorer;
use crate::core::util::long_values::LongValues;
use crate::core::util::packed::direct_monotonic_reader::DirectMonotonicReader;
use parking_lot::{Mutex, MutexGuard};
use std::borrow::Cow;
use std::cell::Cell;
use std::sync::Arc;

/// Read the vector values froGm the index input. This supports both iterated and random access.
struct OffHeapFloatVectorValues<I, F> {
  pub(crate) dimension: usize,
  pub(crate) size: usize,
  pub(crate) byte_size: usize,
  pub(crate) similarity_function: VectorSimilarityFunction,
  pub(crate) flat_vectors_scorer: F,
  pub(crate) inner: Mutex<Inner<I>>,
}
pub struct Inner<I> {
  pub(crate) slice: I,
  pub(crate) last_ord: Option<usize>,
  pub(crate) value: Box<[f32]>,
}
impl<I, F> OffHeapFloatVectorValues<I, F> {
  fn new(
    dimension: usize,
    size: usize,
    slice: I,
    byte_size: usize,
    flat_vectors_scorer: F,
    similarity_function: VectorSimilarityFunction,
  ) -> Self {
    let inner = Inner {
      slice,
      last_ord: None,
      value: Box::default(),
    };
    Self {
      dimension,
      size,
      byte_size,
      similarity_function,
      flat_vectors_scorer,
      inner: Mutex::new(inner),
    }
  }
}

impl<I, F> OffHeapFloatVectorValues<I, F>
where
  I: IndexInput,
{
  fn read_value(&self, target_ord: usize, inner: &mut Inner<I>) -> Result<()> {
    let pos = target_ord
      .checked_mul(self.byte_size)
      .ok_or_else(|| LuceneError::illegal_state("seek overflow"))?;
    inner.slice.seek(pos)?;
    if inner.value.is_empty() {
      inner.value = vec![0.0; self.dimension].into_boxed_slice();
    }
    let value = &mut inner.value;
    let len = value.len();
    inner.slice.read_floats(value, 0, len)?;

    Ok(())
  }
}
impl<I, F> HasIndexSlice for OffHeapFloatVectorValues<I, F>
where
  I: IndexInput,
{
  fn seek(&self, pos: usize) -> Result<()> {
    self.inner.lock().slice.seek(pos)
  }

  fn read_bytes(&self, b: &mut [u8], offset: usize, len: usize) -> Result<()> {
    self.inner.lock().slice.read_bytes(b, offset, len)
  }
}

impl<I, F> KnnVectorValues for OffHeapFloatVectorValues<I, F>
where
  I: IndexInput,
{
  fn dimension(&self) -> usize {
    self.dimension
  }

  fn size(&self) -> usize {
    self.size
  }

  type KnnVectorValues = DummyKnnVectorsWriter;

  fn get_encoding(&self) -> VectorEncoding {
    FloatVectorValues::get_encoding(self)
  }

  type Bits<'a, B>
    = DummyBits
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, _accept_docs: Option<B>) -> Option<Self::Bits<'a, B>>
  where
    B: Bits,
  {
    debug_assert!(
      false,
      "should nerver call get_accept_ords on OffHeapFloatVectorValues, should be called on DenseOffHeapVectorValues or SparseOffHeapVectorValues"
    );
    None
  }

  type DocIndexIterator = DenseDocIndexIterator;
}

impl<I, F> FloatVectorValues for OffHeapFloatVectorValues<I, F>
where
  I: IndexInput,
{
  fn mapped_float_vectors(&self) -> Option<MappedFloatVectors> {
    if self.byte_size != self.dimension.checked_mul(4)? {
      return None;
    }
    let inner = self.inner.try_lock()?;
    if inner.last_ord.is_some() {
      return None;
    }
    inner.slice.mapped_float_vectors(self.dimension, self.size)
  }

  #[inline]
  fn compare_mapped_float<S: VectorSimilarity>(
    &self,
    query: &[f32],
    vector: &[f32],
    similarity: S,
  ) -> Option<Result<f32>> {
    if !S::SUPPORTS_MAPPED_FLOAT_INPUT {
      return None;
    }
    let guard = self.inner.lock();
    let result = similarity.compare_f32(query, vector);
    drop(guard);
    Some(result)
  }

  type VectorValue<'v>
    = parking_lot::MappedMutexGuard<'v, [f32]>
  where
    Self: 'v;

  #[inline(always)]
  fn vector_value(&self, target_ord: usize) -> Result<Self::VectorValue<'_>> {
    let mut inner = self.inner.lock();
    let same_ord = matches!(inner.last_ord, Some(last_ord) if last_ord == target_ord);
    if !same_ord {
      self.read_value(target_ord, &mut inner)?;
      inner.last_ord = Some(target_ord);
    }

    Ok(MutexGuard::map(inner, |inner| inner.value.as_mut()))
  }

  type FloatVectorValues = DummyFloatVectorValues;
  type VectorScorer = DummyVectorScorer;
}

pub struct DenseOffHeapVectorValues<I, F> {
  base: OffHeapFloatVectorValues<I, F>,
}

impl<I, F> DenseOffHeapVectorValues<I, F> {
  pub fn new(
    dimension: usize,
    size: usize,
    slice: I,
    byte_size: usize,
    flat_vectors_scorer: F,
    similarity_function: VectorSimilarityFunction,
  ) -> Result<Self> {
    let base = OffHeapFloatVectorValues::new(
      dimension,
      size,
      slice,
      byte_size,
      flat_vectors_scorer,
      similarity_function,
    );
    Ok(Self { base })
  }
}

impl<I, F> TryClone for DenseOffHeapVectorValues<I, F>
where
  I: IndexInput,
  F: FlatVectorsScorer + Clone,
{
  fn try_clone(&self) -> Result<Self>
  where
    Self: Sized,
  {
    let inner = self.base.inner.lock();
    Self::new(
      self.base.dimension,
      self.base.size,
      inner.slice.try_clone()?,
      self.base.byte_size,
      self.base.flat_vectors_scorer.clone(),
      self.base.similarity_function,
    )
  }
}

impl<I, F> KnnVectorValues for DenseOffHeapVectorValues<I, F>
where
  I: IndexInput,
  F: FlatVectorsScorer + Clone,
{
  fn dimension(&self) -> usize {
    self.base.dimension
  }

  fn size(&self) -> usize {
    self.base.size
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    Ok(ord)
  }

  type KnnVectorValues = Self;

  fn copy(&self) -> Result<Self::KnnVectorValues> {
    let inner = self.base.inner.lock();
    Self::new(
      self.base.dimension,
      self.base.size,
      inner.slice.try_clone()?,
      self.base.byte_size,
      self.base.flat_vectors_scorer.clone(),
      self.base.similarity_function,
    )
  }

  fn get_encoding(&self) -> VectorEncoding {
    FloatVectorValues::get_encoding(self)
  }

  type Bits<'a, B>
    = B
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, accept_docs: Option<B>) -> Option<Self::Bits<'a, B>>
  where
    B: Bits,
  {
    accept_docs
  }

  type DocIndexIterator = DenseDocIndexIterator;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    Ok(create_dense_iterator(self.size() as i32))
  }
}
impl<I, F> HasIndexSlice for DenseOffHeapVectorValues<I, F>
where
  I: IndexInput,
{
  fn seek(&self, pos: usize) -> Result<()> {
    self.base.seek(pos)
  }

  fn read_bytes(&self, b: &mut [u8], offset: usize, len: usize) -> Result<()> {
    self.base.read_bytes(b, offset, len)
  }
}

impl<I, F> FloatVectorValues for DenseOffHeapVectorValues<I, F>
where
  I: IndexInput,
  F: FlatVectorsScorer + Clone,
{
  fn mapped_float_vectors(&self) -> Option<MappedFloatVectors> {
    self.base.mapped_float_vectors()
  }

  #[inline]
  fn compare_mapped_float<S: VectorSimilarity>(
    &self,
    query: &[f32],
    vector: &[f32],
    similarity: S,
  ) -> Option<Result<f32>> {
    self.base.compare_mapped_float(query, vector, similarity)
  }

  type VectorValue<'v>
    = parking_lot::MappedMutexGuard<'v, [f32]>
  where
    Self: 'v;

  #[inline(always)]
  fn vector_value(&self, ord: usize) -> Result<Self::VectorValue<'_>> {
    self.base.vector_value(ord)
  }

  type FloatVectorValues = Self;

  fn float_copy(&self) -> Result<Option<Self::FloatVectorValues>> {
    Ok(Some(Self::new(
      self.base.dimension,
      self.base.size,
      self.base.inner.lock().slice.try_clone()?,
      self.base.byte_size,
      self.base.flat_vectors_scorer.clone(),
      self.base.similarity_function,
    )?))
  }

  type VectorScorer = DenseVectorScorer<
    <F as FlatVectorsScorer>::RandomVectorScorerF32<
      DenseOffHeapVectorValues<I, F>,
      VectorSimilarityFunction,
    >,
  >;

  fn scorer<'a, TV>(&self, query: TV) -> Result<Option<Self::VectorScorer>>
  where
    TV: Into<std::borrow::Cow<'a, [f32]>>,
  {
    let query = query.into().into_owned();
    let copy = self.float_copy()?.ok_or_else(|| {
      LuceneError::illegal_state("DenseOffHeapVectorValues should support float_copy()")
    })?;
    let iterator = copy.iterator()?;

    let sf = copy.base.similarity_function;
    let random_vector_scorer = self
      .base
      .flat_vectors_scorer
      .get_random_vector_scorer_f32(sf, copy, query)?;
    Ok(Some(DenseVectorScorer::new(iterator, random_vector_scorer)))
  }
}

pub struct DenseVectorScorer<R> {
  iterator: DenseDocIndexIterator,
  random_vector_scorer: R,
}
impl<R> DenseVectorScorer<R> {
  fn new(iterator: DenseDocIndexIterator, random_vector_scorer: R) -> Self {
    Self {
      iterator,
      random_vector_scorer,
    }
  }
}
impl<R> VectorScorer for DenseVectorScorer<R>
where
  R: RandomVectorScorer,
{
  fn score(&self) -> Result<f32> {
    let doc_id = self.iterator.doc_id().try_convert()?;
    self.random_vector_scorer.score(doc_id)
  }
}

impl<R> DocIdSetIteratorExtensions for DenseVectorScorer<R> where R: RandomVectorScorer {}

impl<R> BitSetIteratorAccess for DenseVectorScorer<R> where R: RandomVectorScorer {}

impl<R> DocIdSetIterator for DenseVectorScorer<R>
where
  R: RandomVectorScorer,
{
  fn doc_id(&self) -> i32 {
    self.iterator.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.iterator.next_doc()
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.iterator.advance(target)
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    self.iterator.slow_advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self.iterator.cost()
  }
}

pub struct SparseOffHeapVectorValues<I, F>
where
  I: IndexInput,
{
  base: OffHeapFloatVectorValues<I::IndexInput, F>,
  ord_to_doc: Box<DirectMonotonicReader<I::RandomAccessSlice>>,
  data_in: I,
  configuration: Arc<OrdToDocDISIReaderConfiguration>,
  disi: Cell<Option<DocIndexIteratorImpl<I>>>,
}

impl<I, F> SparseOffHeapVectorValues<I, F>
where
  I: IndexInput + Clone,
{
  pub fn new(
    configuration: Arc<OrdToDocDISIReaderConfiguration>,
    data_in: I,
    slice: I::IndexInput,
    dimension: usize,
    byte_size: usize,
    flat_vectors_scorer: F,
    similarity_function: VectorSimilarityFunction,
  ) -> Result<Self> {
    let base = OffHeapFloatVectorValues::new(
      dimension,
      configuration.size as usize,
      slice,
      byte_size,
      flat_vectors_scorer,
      similarity_function,
    );
    let addresses_data = data_in.random_access_slice(
      configuration.addresses_offset,
      configuration.addresses_length,
    )?;

    let ord_to_doc = match configuration.meta {
      Some(ref meta) => DirectMonotonicReader::get_instance(meta, addresses_data)?,
      None => return Err(LuceneError::illegal_state("meta is None")),
    };

    let disi = IndexedDISIImpl::new(
      &data_in,
      configuration.docs_with_field_offset.try_convert()?,
      configuration.docs_with_field_length,
      configuration.jump_table_entry_count as i32,
      configuration.dense_rank_power,
      configuration.size as i64,
    )?;
    let disi = Some(get_doc_index_iterator(disi));

    Ok(Self {
      base,
      ord_to_doc: Box::new(ord_to_doc),
      data_in,
      configuration,
      disi: Cell::new(disi),
    })
  }
}
impl<I, F> HasIndexSlice for SparseOffHeapVectorValues<I, F>
where
  I: IndexInput,
{
  fn seek(&self, pos: usize) -> Result<()> {
    self.base.seek(pos)
  }

  fn read_bytes(&self, b: &mut [u8], offset: usize, len: usize) -> Result<()> {
    self.base.read_bytes(b, offset, len)
  }
}
impl<I, F> KnnVectorValues for SparseOffHeapVectorValues<I, F>
where
  I: IndexInput + Clone,
  F: FlatVectorsScorer + Clone,
{
  fn dimension(&self) -> usize {
    self.base.dimension
  }

  fn size(&self) -> usize {
    self.base.size
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    Ok(self.ord_to_doc.get(ord)? as usize)
  }

  type KnnVectorValues = Self;

  fn copy(&self) -> Result<Self::KnnVectorValues> {
    Self::new(
      self.configuration.clone(),
      self.data_in.clone(),
      self.base.inner.lock().slice.try_clone()?,
      self.base.dimension,
      self.base.byte_size,
      self.base.flat_vectors_scorer.clone(),
      self.base.similarity_function,
    )
  }

  fn get_encoding(&self) -> VectorEncoding {
    FloatVectorValues::get_encoding(self)
  }

  type Bits<'a, B>
    = SparseOffHeapVectorValueBits<'a, B, I::RandomAccessSlice>
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, accept_docs: Option<B>) -> Option<Self::Bits<'a, B>>
  where
    B: Bits,
  {
    accept_docs
      .map(|bits| SparseOffHeapVectorValueBits::new(bits, self.base.size, &self.ord_to_doc))
  }

  type DocIndexIterator = DocIndexIteratorImpl<I>;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    match self.disi.take() {
      Some(disi) => Ok(disi),
      None => Err(LuceneError::illegal_state("iterator only called once")),
    }
  }
}

impl<I, F> FloatVectorValues for SparseOffHeapVectorValues<I, F>
where
  I: IndexInput + Clone,
  F: FlatVectorsScorer + Clone,
{
  fn mapped_float_vectors(&self) -> Option<MappedFloatVectors> {
    self.base.mapped_float_vectors()
  }

  #[inline]
  fn compare_mapped_float<S: VectorSimilarity>(
    &self,
    query: &[f32],
    vector: &[f32],
    similarity: S,
  ) -> Option<Result<f32>> {
    self.base.compare_mapped_float(query, vector, similarity)
  }

  type VectorValue<'v>
    = parking_lot::MappedMutexGuard<'v, [f32]>
  where
    Self: 'v;

  #[inline(always)]
  fn vector_value(&self, ord: usize) -> Result<Self::VectorValue<'_>> {
    self.base.vector_value(ord)
  }

  type FloatVectorValues = Self;

  fn float_copy(&self) -> Result<Option<Self::FloatVectorValues>> {
    Ok(Some(Self::new(
      self.configuration.clone(),
      self.data_in.clone(),
      self.base.inner.lock().slice.try_clone()?,
      self.base.dimension,
      self.base.byte_size,
      self.base.flat_vectors_scorer.clone(),
      self.base.similarity_function,
    )?))
  }

  type VectorScorer = SparseVectorScorer<
    I,
    <F as FlatVectorsScorer>::RandomVectorScorerF32<
      SparseOffHeapVectorValues<I, F>,
      VectorSimilarityFunction,
    >,
  >;

  fn scorer<'a, TV>(&self, query: TV) -> Result<Option<Self::VectorScorer>>
  where
    TV: Into<std::borrow::Cow<'a, [f32]>>,
  {
    let query = query.into().into_owned();
    let copy = self.float_copy()?.ok_or_else(|| {
      LuceneError::illegal_state("DenseOffHeapVectorValues should support float_copy()")
    })?;
    let iterator = copy.iterator()?;

    let sf = copy.base.similarity_function;

    let random_vector_scorer = self
      .base
      .flat_vectors_scorer
      .get_random_vector_scorer_f32(sf, copy, query)?;

    Ok(Some(SparseVectorScorer::new(
      iterator,
      random_vector_scorer,
    )))
  }
}
pub struct SparseVectorScorer<I, R>
where
  I: IndexInput,
{
  iterator: DocIndexIteratorImpl<I>,
  random_vector_scorer: R,
}

impl<I, R> SparseVectorScorer<I, R>
where
  I: IndexInput,
{
  fn new(iterator: DocIndexIteratorImpl<I>, random_vector_scorer: R) -> Self {
    Self {
      iterator,
      random_vector_scorer,
    }
  }
}
impl<I, R> VectorScorer for SparseVectorScorer<I, R>
where
  I: IndexInput,
  R: RandomVectorScorer,
{
  fn score(&self) -> Result<f32> {
    let index = self.iterator.index()?;
    self.random_vector_scorer.score(index as usize)
  }
}

impl<I, R> DocIdSetIteratorExtensions for SparseVectorScorer<I, R>
where
  I: IndexInput,
  R: RandomVectorScorer,
{
}

impl<I, R> BitSetIteratorAccess for SparseVectorScorer<I, R>
where
  I: IndexInput,
  R: RandomVectorScorer,
{
}

impl<I, R> DocIdSetIterator for SparseVectorScorer<I, R>
where
  I: IndexInput,
  R: RandomVectorScorer,
{
  fn doc_id(&self) -> i32 {
    self.iterator.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    self.iterator.next_doc()
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    self.iterator.advance(target)
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    self.iterator.slow_advance(target)
  }

  fn cost(&self) -> Result<i64> {
    self.iterator.cost()
  }
}
pub struct EmptyOffHeapVectorValues {
  dimension: usize,
  vectors: Vec<f32>,
}
impl EmptyOffHeapVectorValues {
  fn new(dimension: usize) -> Self {
    let vectors = Vec::new();

    Self { dimension, vectors }
  }
}
impl HasIndexSlice for EmptyOffHeapVectorValues {}
impl KnnVectorValues for EmptyOffHeapVectorValues {
  fn dimension(&self) -> usize {
    self.dimension
  }

  fn size(&self) -> usize {
    0
  }

  type KnnVectorValues = DummyKnnVectorsWriter;

  fn get_encoding(&self) -> VectorEncoding {
    FloatVectorValues::get_encoding(self)
  }

  type Bits<'a, B>
    = DummyBits
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, _accept_docs: Option<B>) -> Option<Self::Bits<'a, B>>
  where
    B: Bits,
  {
    None
  }

  type DocIndexIterator = DenseDocIndexIterator;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    Ok(create_dense_iterator(0))
  }
}

impl FloatVectorValues for EmptyOffHeapVectorValues {
  type VectorValue<'v>
    = parking_lot::MappedMutexGuard<'v, [f32]>
  where
    Self: 'v;

  fn vector_value(&self, _ord: usize) -> Result<Self::VectorValue<'_>> {
    debug_assert!(self.vectors.is_empty());
    Err(LuceneError::unsupported_operation(""))
  }

  type FloatVectorValues = Self;

  fn float_copy(&self) -> Result<Option<Self::FloatVectorValues>> {
    Err(LuceneError::unsupported_operation(""))
  }

  type VectorScorer = DummyVectorScorer;

  fn scorer<'a, TV>(&self, _target: TV) -> Result<Option<Self::VectorScorer>>
  where
    TV: Into<std::borrow::Cow<'a, [f32]>>,
  {
    Ok(None)
  }
}

pub enum OffHeapFloatVectorValuesEnum<I, F>
where
  I: IndexInput,
{
  Empty(EmptyOffHeapVectorValues),
  Dense(DenseOffHeapVectorValues<I::IndexInput, F>),
  Sparse(SparseOffHeapVectorValues<I, F>),
}

pub enum OffHeapFloatVectorValuesCopy<I, F>
where
  I: IndexInput,
{
  Dense(DenseOffHeapVectorValues<I::IndexInput, F>),
  Sparse(SparseOffHeapVectorValues<I, F>),
}

impl<I, F> KnnVectorValues for OffHeapFloatVectorValuesCopy<I, F>
where
  F: FlatVectorsScorer + Clone,
  I: IndexInput + Clone,
{
  fn dimension(&self) -> usize {
    match self {
      Self::Dense(values) => values.dimension(),
      Self::Sparse(values) => values.dimension(),
    }
  }

  fn size(&self) -> usize {
    match self {
      Self::Dense(values) => values.size(),
      Self::Sparse(values) => values.size(),
    }
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    match self {
      Self::Dense(values) => values.ord_to_doc(ord),
      Self::Sparse(values) => values.ord_to_doc(ord),
    }
  }

  type KnnVectorValues = Self;

  fn copy(&self) -> Result<Self::KnnVectorValues> {
    match self {
      Self::Dense(values) => KnnVectorValues::copy(values).map(Self::Dense),
      Self::Sparse(values) => KnnVectorValues::copy(values).map(Self::Sparse),
    }
  }

  fn get_vector_byte_length(&self) -> usize {
    match self {
      Self::Dense(values) => values.get_vector_byte_length(),
      Self::Sparse(values) => values.get_vector_byte_length(),
    }
  }

  fn get_encoding(&self) -> VectorEncoding {
    match self {
      Self::Dense(values) => KnnVectorValues::get_encoding(values),
      Self::Sparse(values) => KnnVectorValues::get_encoding(values),
    }
  }

  type Bits<'a, B>
    = OffHeapVectorValueBits<'a, I::RandomAccessSlice, B>
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, accept_docs: Option<B>) -> Option<Self::Bits<'a, B>>
  where
    B: Bits,
  {
    match self {
      Self::Dense(values) => values
        .get_accept_ords(accept_docs)
        .map(OffHeapVectorValueBits::Dense),
      Self::Sparse(values) => values
        .get_accept_ords(accept_docs)
        .map(OffHeapVectorValueBits::Sparse),
    }
  }

  type DocIndexIterator = IndexedDISIDocIndexIterator<I>;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    match self {
      Self::Dense(values) => values.iterator().map(IndexedDISIDocIndexIterator::Dense),
      Self::Sparse(values) => values.iterator().map(IndexedDISIDocIndexIterator::Sparse),
    }
  }
}

impl<I, F> FloatVectorValues for OffHeapFloatVectorValuesCopy<I, F>
where
  I: IndexInput + Clone,
  F: FlatVectorsScorer + Clone,
{
  fn mapped_float_vectors(&self) -> Option<MappedFloatVectors> {
    match self {
      Self::Dense(values) => values.mapped_float_vectors(),
      Self::Sparse(values) => values.mapped_float_vectors(),
    }
  }

  #[inline]
  fn compare_mapped_float<S: VectorSimilarity>(
    &self,
    query: &[f32],
    vector: &[f32],
    similarity: S,
  ) -> Option<Result<f32>> {
    match self {
      Self::Dense(values) => values.compare_mapped_float(query, vector, similarity),
      Self::Sparse(values) => values.compare_mapped_float(query, vector, similarity),
    }
  }

  type VectorValue<'v>
    = parking_lot::MappedMutexGuard<'v, [f32]>
  where
    Self: 'v;

  fn vector_value(&self, ord: usize) -> Result<Self::VectorValue<'_>> {
    match self {
      Self::Dense(values) => values.vector_value(ord),
      Self::Sparse(values) => values.vector_value(ord),
    }
  }

  type FloatVectorValues = Self;

  fn float_copy(&self) -> Result<Option<Self::FloatVectorValues>> {
    match self {
      Self::Dense(values) => Ok(values.float_copy()?.map(Self::Dense)),
      Self::Sparse(values) => Ok(values.float_copy()?.map(Self::Sparse)),
    }
  }

  type VectorScorer = VectorScorerEnum<I, F>;

  fn scorer<'a, TV>(&self, target: TV) -> Result<Option<Self::VectorScorer>>
  where
    TV: Into<std::borrow::Cow<'a, [f32]>>,
  {
    match self {
      Self::Dense(values) => Ok(values.scorer(target)?.map(VectorScorerEnum::Dense)),
      Self::Sparse(values) => Ok(values.scorer(target)?.map(VectorScorerEnum::Sparse)),
    }
  }

  fn get_encoding(&self) -> VectorEncoding {
    match self {
      Self::Dense(values) => FloatVectorValues::get_encoding(values),
      Self::Sparse(values) => FloatVectorValues::get_encoding(values),
    }
  }

  fn get_vectors_mut(&mut self) -> Result<&mut Vec<Box<[f32]>>> {
    match self {
      Self::Dense(values) => values.get_vectors_mut(),
      Self::Sparse(values) => values.get_vectors_mut(),
    }
  }

  fn get_vectors(&self) -> Result<&[Box<[f32]>]> {
    match self {
      Self::Dense(values) => values.get_vectors(),
      Self::Sparse(values) => values.get_vectors(),
    }
  }
}

impl<I, F> KnnVectorValues for OffHeapFloatVectorValuesEnum<I, F>
where
  F: FlatVectorsScorer + Clone,
  I: IndexInput + Clone,
{
  fn dimension(&self) -> usize {
    match self {
      OffHeapFloatVectorValuesEnum::Empty(e) => e.dimension(),
      OffHeapFloatVectorValuesEnum::Dense(e) => e.dimension(),
      OffHeapFloatVectorValuesEnum::Sparse(e) => e.dimension(),
    }
  }

  fn size(&self) -> usize {
    match self {
      OffHeapFloatVectorValuesEnum::Empty(e) => e.size(),
      OffHeapFloatVectorValuesEnum::Dense(e) => e.size(),
      OffHeapFloatVectorValuesEnum::Sparse(e) => e.size(),
    }
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    match self {
      OffHeapFloatVectorValuesEnum::Empty(e) => e.ord_to_doc(ord),
      OffHeapFloatVectorValuesEnum::Dense(e) => e.ord_to_doc(ord),
      OffHeapFloatVectorValuesEnum::Sparse(e) => e.ord_to_doc(ord),
    }
  }

  type KnnVectorValues = OffHeapFloatVectorValuesCopy<I, F>;

  fn copy(&self) -> Result<Self::KnnVectorValues> {
    match self {
      OffHeapFloatVectorValuesEnum::Empty(_) => Err(LuceneError::unsupported_operation("")),
      OffHeapFloatVectorValuesEnum::Dense(e) => {
        KnnVectorValues::copy(e).map(OffHeapFloatVectorValuesCopy::Dense)
      },
      OffHeapFloatVectorValuesEnum::Sparse(e) => {
        KnnVectorValues::copy(e).map(OffHeapFloatVectorValuesCopy::Sparse)
      },
    }
  }

  fn get_vector_byte_length(&self) -> usize {
    match self {
      OffHeapFloatVectorValuesEnum::Empty(e) => e.get_vector_byte_length(),
      OffHeapFloatVectorValuesEnum::Dense(e) => e.get_vector_byte_length(),
      OffHeapFloatVectorValuesEnum::Sparse(e) => e.get_vector_byte_length(),
    }
  }

  fn get_encoding(&self) -> VectorEncoding {
    match self {
      OffHeapFloatVectorValuesEnum::Empty(e) => FloatVectorValues::get_encoding(e),
      OffHeapFloatVectorValuesEnum::Dense(e) => FloatVectorValues::get_encoding(e),
      OffHeapFloatVectorValuesEnum::Sparse(e) => FloatVectorValues::get_encoding(e),
    }
  }

  type Bits<'a, B>
    = OffHeapVectorValueBits<'a, I::RandomAccessSlice, B>
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, accept_docs: Option<B>) -> Option<Self::Bits<'a, B>>
  where
    B: Bits,
  {
    match self {
      OffHeapFloatVectorValuesEnum::Empty(_) => None,
      OffHeapFloatVectorValuesEnum::Dense(e) => e
        .get_accept_ords(accept_docs)
        .map(OffHeapVectorValueBits::Dense),
      OffHeapFloatVectorValuesEnum::Sparse(e) => e
        .get_accept_ords(accept_docs)
        .map(OffHeapVectorValueBits::Sparse),
    }
  }

  type DocIndexIterator = IndexedDISIDocIndexIterator<I>;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    match self {
      OffHeapFloatVectorValuesEnum::Empty(e) => {
        e.iterator().map(IndexedDISIDocIndexIterator::Dense)
      },
      OffHeapFloatVectorValuesEnum::Dense(e) => {
        e.iterator().map(IndexedDISIDocIndexIterator::Dense)
      },
      OffHeapFloatVectorValuesEnum::Sparse(e) => {
        e.iterator().map(IndexedDISIDocIndexIterator::Sparse)
      },
    }
  }
}

impl<I, F> FloatVectorValues for OffHeapFloatVectorValuesEnum<I, F>
where
  I: IndexInput + Clone,
  F: FlatVectorsScorer + Clone,
{
  fn mapped_float_vectors(&self) -> Option<MappedFloatVectors> {
    match self {
      Self::Dense(values) => values.mapped_float_vectors(),
      Self::Sparse(values) => values.mapped_float_vectors(),
      Self::Empty(_) => None,
    }
  }

  #[inline]
  fn compare_mapped_float<S: VectorSimilarity>(
    &self,
    query: &[f32],
    vector: &[f32],
    similarity: S,
  ) -> Option<Result<f32>> {
    match self {
      Self::Dense(values) => values.compare_mapped_float(query, vector, similarity),
      Self::Sparse(values) => values.compare_mapped_float(query, vector, similarity),
      Self::Empty(_) => None,
    }
  }

  type VectorValue<'v>
    = parking_lot::MappedMutexGuard<'v, [f32]>
  where
    Self: 'v;

  #[inline(always)]
  fn vector_value(&self, ord: usize) -> Result<Self::VectorValue<'_>> {
    match self {
      OffHeapFloatVectorValuesEnum::Empty(e) => e.vector_value(ord),
      OffHeapFloatVectorValuesEnum::Dense(e) => e.vector_value(ord),
      OffHeapFloatVectorValuesEnum::Sparse(e) => e.vector_value(ord),
    }
  }

  type FloatVectorValues = OffHeapFloatVectorValuesCopy<I, F>;

  fn float_copy(&self) -> Result<Option<Self::FloatVectorValues>> {
    match self {
      OffHeapFloatVectorValuesEnum::Empty(_) => Err(LuceneError::unsupported_operation("")),
      OffHeapFloatVectorValuesEnum::Dense(e) => {
        Ok(e.float_copy()?.map(OffHeapFloatVectorValuesCopy::Dense))
      },
      OffHeapFloatVectorValuesEnum::Sparse(e) => {
        Ok(e.float_copy()?.map(OffHeapFloatVectorValuesCopy::Sparse))
      },
    }
  }

  type VectorScorer = VectorScorerEnum<I, F>;

  fn scorer<'a, TV>(&self, target: TV) -> Result<Option<Self::VectorScorer>>
  where
    TV: Into<std::borrow::Cow<'a, [f32]>>,
  {
    match self {
      OffHeapFloatVectorValuesEnum::Empty(_) => {
        let _: std::borrow::Cow<'a, [f32]> = target.into();
        Ok(None)
      },

      OffHeapFloatVectorValuesEnum::Dense(e) => Ok(e.scorer(target)?.map(VectorScorerEnum::Dense)),

      OffHeapFloatVectorValuesEnum::Sparse(e) => {
        Ok(e.scorer(target)?.map(VectorScorerEnum::Sparse))
      },
    }
  }

  fn get_encoding(&self) -> VectorEncoding {
    match self {
      OffHeapFloatVectorValuesEnum::Empty(e) => FloatVectorValues::get_encoding(e),
      OffHeapFloatVectorValuesEnum::Dense(e) => FloatVectorValues::get_encoding(e),
      OffHeapFloatVectorValuesEnum::Sparse(e) => FloatVectorValues::get_encoding(e),
    }
  }

  fn get_vectors_mut(&mut self) -> Result<&mut Vec<Box<[f32]>>> {
    match self {
      OffHeapFloatVectorValuesEnum::Empty(e) => e.get_vectors_mut(),
      OffHeapFloatVectorValuesEnum::Dense(e) => e.get_vectors_mut(),
      OffHeapFloatVectorValuesEnum::Sparse(e) => e.get_vectors_mut(),
    }
  }

  fn get_vectors(&self) -> Result<&[Box<[f32]>]> {
    match self {
      OffHeapFloatVectorValuesEnum::Empty(e) => e.get_vectors(),
      OffHeapFloatVectorValuesEnum::Dense(e) => e.get_vectors(),
      OffHeapFloatVectorValuesEnum::Sparse(e) => e.get_vectors(),
    }
  }
}

pub enum VectorScorerEnum<I, F>
where
  I: IndexInput + Clone,
  F: FlatVectorsScorer + Clone,
{
  Dense(
    DenseVectorScorer<
      F::RandomVectorScorerF32<
        DenseOffHeapVectorValues<I::IndexInput, F>,
        VectorSimilarityFunction,
      >,
    >,
  ),
  Sparse(
    SparseVectorScorer<
      I,
      F::RandomVectorScorerF32<SparseOffHeapVectorValues<I, F>, VectorSimilarityFunction>,
    >,
  ),
}
impl<I, F> VectorScorer for VectorScorerEnum<I, F>
where
  I: IndexInput + Clone,
  F: FlatVectorsScorer + Clone,
{
  fn score(&self) -> Result<f32> {
    match self {
      Self::Dense(scorer) => scorer.score(),
      Self::Sparse(scorer) => scorer.score(),
    }
  }
}

impl<I, F> DocIdSetIteratorExtensions for VectorScorerEnum<I, F>
where
  I: IndexInput + Clone,
  F: FlatVectorsScorer + Clone,
{
}

impl<I, F> BitSetIteratorAccess for VectorScorerEnum<I, F>
where
  I: IndexInput + Clone,
  F: FlatVectorsScorer + Clone,
{
}

impl<I, F> DocIdSetIterator for VectorScorerEnum<I, F>
where
  I: IndexInput + Clone,
  F: FlatVectorsScorer + Clone,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::Dense(scorer) => scorer.doc_id(),
      Self::Sparse(scorer) => scorer.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::Dense(scorer) => scorer.next_doc(),
      Self::Sparse(scorer) => scorer.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Dense(scorer) => scorer.advance(target),
      Self::Sparse(scorer) => scorer.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Dense(scorer) => scorer.slow_advance(target),
      Self::Sparse(scorer) => scorer.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::Dense(scorer) => scorer.cost(),
      Self::Sparse(scorer) => scorer.cost(),
    }
  }
}

impl<I, F> OffHeapFloatVectorValuesEnum<I, F>
where
  I: IndexInput + Clone,
  F: FlatVectorsScorer,
{
  #[allow(clippy::too_many_arguments)]
  pub fn load(
    vector_similarity_function: VectorSimilarityFunction,
    flat_vectors_scorer: F,
    configuration: Arc<OrdToDocDISIReaderConfiguration>,
    vector_encoding: VectorEncoding,
    dimension: usize,
    vector_data_offset: usize,
    vector_data_length: usize,
    vector_data: I,
  ) -> Result<Self> {
    Self::load_with_sources(
      vector_similarity_function,
      flat_vectors_scorer,
      Cow::Owned(configuration),
      vector_encoding,
      dimension,
      vector_data_offset,
      vector_data_length,
      Cow::Owned(vector_data),
    )
  }

  #[allow(clippy::too_many_arguments)]
  pub(crate) fn load_from_reader(
    vector_similarity_function: VectorSimilarityFunction,
    flat_vectors_scorer: F,
    configuration: &Arc<OrdToDocDISIReaderConfiguration>,
    vector_encoding: VectorEncoding,
    dimension: usize,
    vector_data_offset: usize,
    vector_data_length: usize,
    vector_data: &I,
  ) -> Result<Self> {
    Self::load_with_sources(
      vector_similarity_function,
      flat_vectors_scorer,
      Cow::Borrowed(configuration),
      vector_encoding,
      dimension,
      vector_data_offset,
      vector_data_length,
      Cow::Borrowed(vector_data),
    )
  }

  #[allow(clippy::too_many_arguments)]
  fn load_with_sources(
    vector_similarity_function: VectorSimilarityFunction,
    flat_vectors_scorer: F,
    configuration: Cow<'_, Arc<OrdToDocDISIReaderConfiguration>>,
    vector_encoding: VectorEncoding,
    dimension: usize,
    vector_data_offset: usize,
    vector_data_length: usize,
    vector_data: Cow<'_, I>,
  ) -> Result<Self> {
    if configuration.docs_with_field_offset == -2 || vector_encoding != VectorEncoding::FLOAT32(4) {
      return Ok(Self::Empty(EmptyOffHeapVectorValues::new(dimension)));
    }

    let bytes_slice = vector_data.slice("vector-data", vector_data_offset, vector_data_length)?;
    let byte_size = dimension
      .checked_mul(vector_encoding.byte_size())
      .ok_or_else(|| LuceneError::illegal_state("vector byte size overflow"))?;

    if configuration.docs_with_field_offset == -1 {
      Ok(Self::Dense(DenseOffHeapVectorValues::new(
        dimension,
        configuration.size.try_convert()?,
        bytes_slice,
        byte_size,
        flat_vectors_scorer,
        vector_similarity_function,
      )?))
    } else {
      Ok(Self::Sparse(SparseOffHeapVectorValues::new(
        configuration.into_owned(),
        vector_data.into_owned(),
        bytes_slice,
        dimension,
        byte_size,
        flat_vectors_scorer,
        vector_similarity_function,
      )?))
    }
  }
}
