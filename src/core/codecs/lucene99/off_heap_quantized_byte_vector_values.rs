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
  DocIndexIteratorImpl, IndexedDISIImpl, get_doc_index_iterator,
};
use crate::core::codecs::knn_field_vectors_writer::VectorValueEnum;
use crate::core::codecs::lucene95::has_index_slice::HasIndexSlice;
use crate::core::codecs::lucene95::ord_to_doc_disi_reader_configuration::OrdToDocDISIReaderConfiguration;
use crate::core::codecs::lucene99::lucene99_scalar_quantized_vector_scorer::{
  Lucene99ScalarQuantizedVectorScorer, ScalarQuantizedRandomVectorScorerEnum,
};
use crate::core::index::byte_vector_values::ByteVectorValues;
use crate::core::index::dummy::dummy_byte_vector_values::DummyByteVectorValues;
use crate::core::index::dummy::dummy_knn_vector_values::DummyKnnVectorsWriter;
use crate::core::index::index_reader::Identity;
use crate::core::index::knn_vector_values::{
  DenseDocIndexIterator, DocIndexIterator, KnnVectorValues, create_dense_iterator,
};
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, DocIdSetIteratorEnum2};
use crate::core::search::dummy::dummy_vector_scorer::DummyVectorScorer;
use crate::core::search::vector_scorer::VectorScorer;
use crate::core::store::IndexInput;
use crate::core::store::random_access_input::RandomAccessInput;
use crate::core::util::bit_util::BitUtil;
use crate::core::util::bits::Bits;
use crate::core::util::clone::TryClone;
use crate::core::util::dummy::dummy_bits::DummyBits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::hnsw::random_vector_scorer::RandomVectorScorer;
use crate::core::util::long_values::LongValues;
use crate::core::util::packed::direct_monotonic_reader::DirectMonotonicReader;
use crate::core::util::quantization::quantized_byte_vector_values::QuantizedByteVectorValues;
use crate::core::util::quantization::scalar_quantizer::ScalarQuantizer;
use crate::core::util::{HasIdentity, TryIntoInt};
use parking_lot::Mutex;
use std::borrow::Cow;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

/// Read the quantized vector values and their score correction values from the index input. This
/// supports both iterated and random access.
struct OffHeapQuantizedByteVectorValues<I, F> {
  dimension: usize,
  size: usize,
  num_bytes: usize,
  scalar_quantizer: ScalarQuantizer,
  similarity_function: VectorSimilarityFunction,
  vectors_scorer: F,
  compress: bool,
  byte_size: usize,
  inner: Mutex<Inner<I>>,
}

struct Inner<I> {
  slice: I,
  last_ord: Option<usize>,
  binary_value: Vec<u8>,
  score_correction_constant: [f32; 1],
}

impl<I, F> OffHeapQuantizedByteVectorValues<I, F> {
  fn new(
    dimension: usize,
    size: usize,
    scalar_quantizer: ScalarQuantizer,
    similarity_function: VectorSimilarityFunction,
    vectors_scorer: F,
    compress: bool,
    slice: I,
  ) -> Self {
    let num_bytes = if scalar_quantizer.get_bits() <= 4 && compress {
      (dimension + 1) >> 1
    } else {
      dimension
    };
    let byte_size = num_bytes + BitUtil::FLOAT_BYTES;
    let inner = Mutex::new(Inner {
      slice,
      last_ord: None,
      binary_value: vec![0; dimension],
      score_correction_constant: [0.0],
    });
    Self {
      dimension,
      size,
      num_bytes,
      scalar_quantizer,
      similarity_function,
      vectors_scorer,
      compress,
      byte_size,
      inner,
    }
  }
}

impl<I, F> OffHeapQuantizedByteVectorValues<I, F>
where
  I: IndexInput,
{
  fn read_value(&self, target_ord: usize, inner: &mut Inner<I>) -> Result<()> {
    let pos = target_ord
      .checked_mul(self.byte_size)
      .ok_or_else(|| LuceneError::illegal_state("seek overflow"))?;
    inner.slice.seek(pos)?;
    inner
      .slice
      .read_bytes(&mut inner.binary_value, 0, self.num_bytes)?;
    let mut score_correction_constant = [0.0];
    inner
      .slice
      .read_floats(&mut score_correction_constant, 0, 1)?;
    inner.score_correction_constant = score_correction_constant;
    decompress_bytes(&mut inner.binary_value, self.num_bytes)?;
    inner.last_ord = Some(target_ord);
    Ok(())
  }

  fn get_score_correction_constant(&self, target_ord: usize) -> Result<f32> {
    let mut inner = self.inner.lock();
    if matches!(inner.last_ord, Some(last_ord) if last_ord == target_ord) {
      return Ok(inner.score_correction_constant[0]);
    }
    let pos = target_ord
      .checked_mul(self.byte_size)
      .and_then(|pos| pos.checked_add(self.num_bytes))
      .ok_or_else(|| LuceneError::illegal_state("seek overflow"))?;
    inner.slice.seek(pos)?;
    let mut score_correction_constant = [0.0];
    inner
      .slice
      .read_floats(&mut score_correction_constant, 0, 1)?;
    inner.score_correction_constant = score_correction_constant;
    Ok(inner.score_correction_constant[0])
  }
}

impl<I, F> HasIndexSlice for OffHeapQuantizedByteVectorValues<I, F>
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

impl<I, F> KnnVectorValues for OffHeapQuantizedByteVectorValues<I, F>
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

  fn get_vector_byte_length(&self) -> usize {
    self.num_bytes
  }

  fn get_encoding(&self) -> VectorEncoding {
    ByteVectorValues::get_encoding(self)
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
      "should never call get_accept_ords on OffHeapQuantizedByteVectorValuesBase, should be called on DenseOffHeapVectorValues or SparseOffHeapVectorValues"
    );
    None
  }

  type DocIndexIterator = DenseDocIndexIterator;
}

impl<I, F> ByteVectorValues for OffHeapQuantizedByteVectorValues<I, F>
where
  I: IndexInput,
{
  fn vector_value(&self, target_ord: usize) -> Result<Cow<'_, VectorValueEnum>> {
    let mut inner = self.inner.lock();
    let same_ord = matches!(inner.last_ord, Some(last_ord) if last_ord == target_ord);
    if !same_ord {
      self.read_value(target_ord, &mut inner)?;
    }
    Ok(Cow::Owned(VectorValueEnum::Byte(
      inner.binary_value.clone(),
    )))
  }

  type ByteVectorValues = DummyByteVectorValues;
  type VectorScorer = DummyVectorScorer;
}

pub(crate) fn decompress_bytes(compressed: &mut [u8], num_bytes: usize) -> Result<()> {
  if num_bytes == compressed.len() {
    return Ok(());
  }
  if num_bytes << 1 != compressed.len() {
    return Err(LuceneError::illegal_argument(format!(
      "numBytes: {} does not match compressed length: {}",
      num_bytes,
      compressed.len()
    )));
  }
  for i in 0..num_bytes {
    compressed[num_bytes + i] = compressed[i] & 0x0F;
    compressed[i] >>= 4;
  }
  Ok(())
}

pub(crate) fn compressed_array(dimension: usize, bits: u8) -> Option<Vec<u8>> {
  if bits <= 4 {
    Some(vec![0; (dimension + 1) >> 1])
  } else {
    None
  }
}

pub(crate) fn compress_bytes(raw: &[u8], compressed: &mut [u8]) -> Result<()> {
  if compressed.len() != ((raw.len() + 1) >> 1) {
    return Err(LuceneError::illegal_argument(format!(
      "compressed length: {} does not match raw length: {}",
      compressed.len(),
      raw.len()
    )));
  }
  if compressed.len() << 1 != raw.len() {
    return Err(LuceneError::illegal_argument(format!(
      "raw length: {} must be twice compressed length: {}",
      raw.len(),
      compressed.len()
    )));
  }
  for i in 0..compressed.len() {
    let v = (raw[i] << 4) | raw[compressed.len() + i];
    compressed[i] = v;
  }
  Ok(())
}

/// Dense vector values that are stored off-heap. This is the most common case when every doc has a
/// vector.
pub struct DenseOffHeapVectorValues<I, F> {
  base: OffHeapQuantizedByteVectorValues<I, F>,
}

impl<I, F> DenseOffHeapVectorValues<I, F> {
  pub fn new(
    dimension: usize,
    size: usize,
    scalar_quantizer: ScalarQuantizer,
    compress: bool,
    similarity_function: VectorSimilarityFunction,
    vectors_scorer: F,
    slice: I,
  ) -> Self {
    Self {
      base: OffHeapQuantizedByteVectorValues::new(
        dimension,
        size,
        scalar_quantizer,
        similarity_function,
        vectors_scorer,
        compress,
        slice,
      ),
    }
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
    Ok(Self::new(
      self.base.dimension,
      self.base.size,
      self.base.scalar_quantizer.clone(),
      self.base.compress,
      self.base.similarity_function,
      self.base.vectors_scorer.clone(),
      inner.slice.try_clone()?,
    ))
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

  type KnnVectorValues = Self;

  fn copy(&self) -> Result<Self::KnnVectorValues> {
    self.try_clone()
  }

  fn get_vector_byte_length(&self) -> usize {
    self.base.num_bytes
  }

  fn get_encoding(&self) -> VectorEncoding {
    ByteVectorValues::get_encoding(self)
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

impl<I, F> ByteVectorValues for DenseOffHeapVectorValues<I, F>
where
  I: IndexInput,
  F: FlatVectorsScorer + Clone,
{
  fn vector_value(&self, ord: usize) -> Result<Cow<'_, VectorValueEnum>> {
    self.base.vector_value(ord)
  }

  type ByteVectorValues = Self;

  fn byte_copy(&self) -> Result<Option<Self::ByteVectorValues>> {
    Ok(Some(self.try_clone()?))
  }

  type VectorScorer = DummyVectorScorer;
}

impl<I, F> QuantizedByteVectorValues
  for DenseOffHeapVectorValues<I, Lucene99ScalarQuantizedVectorScorer<F>>
where
  I: IndexInput,
  F: FlatVectorsScorer + Clone,
{
  type QuantizedVectorScorer = DenseVectorScorer<ScalarQuantizedRandomVectorScorerEnum<Self>>;

  fn get_scalar_quantizer(&self) -> Result<ScalarQuantizer> {
    Ok(self.base.scalar_quantizer.clone())
  }

  fn get_score_correction_constant(&self, ord: usize) -> Result<f32> {
    self.base.get_score_correction_constant(ord)
  }

  fn scorer(&self, target: &[f32]) -> Result<Option<Self::QuantizedVectorScorer>> {
    let copy = QuantizedByteVectorValues::copy(self)?;
    let iterator = copy.iterator()?;
    let similarity_function = copy.base.similarity_function;
    let random_vector_scorer = self.base.vectors_scorer.get_random_vector_scorer_f32(
      similarity_function,
      copy,
      target.to_vec(),
    )?;
    Ok(Some(DenseVectorScorer::new(iterator, random_vector_scorer)))
  }

  type QuantizedByteVectorValues = Self;

  fn copy(&self) -> Result<Self::QuantizedByteVectorValues> {
    self.try_clone()
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
    let index = self.iterator.index()?.try_convert()?;
    self.random_vector_scorer.score(index)
  }

  type DocIdSetIteratorRef<'a>
    = &'a DenseDocIndexIterator
  where
    Self: 'a;

  fn iterator(&self) -> Self::DocIdSetIteratorRef<'_> {
    &self.iterator
  }

  type DocIdSetIteratorMut<'a>
    = &'a mut DenseDocIndexIterator
  where
    Self: 'a;

  fn iterator_mut(&mut self) -> Self::DocIdSetIteratorMut<'_> {
    &mut self.iterator
  }
}

pub struct SparseOffHeapVectorValues<I, F>
where
  I: IndexInput,
{
  base: OffHeapQuantizedByteVectorValues<I::IndexInput, F>,
  ord_to_doc: Rc<RefCell<DirectMonotonicReader<I::RandomAccessSlice>>>,
  data_in: I,
  configuration: Arc<OrdToDocDISIReaderConfiguration>,
  disi: RefCell<Option<DocIndexIteratorImpl<I>>>,
}

impl<I, F> SparseOffHeapVectorValues<I, F>
where
  I: IndexInput + Clone,
{
  #[allow(clippy::too_many_arguments)]
  pub fn new(
    configuration: Arc<OrdToDocDISIReaderConfiguration>,
    dimension: usize,
    size: usize,
    scalar_quantizer: ScalarQuantizer,
    compress: bool,
    data_in: I,
    similarity_function: VectorSimilarityFunction,
    vectors_scorer: F,
    slice: I::IndexInput,
  ) -> Result<Self> {
    let base = OffHeapQuantizedByteVectorValues::new(
      dimension,
      size,
      scalar_quantizer,
      similarity_function,
      vectors_scorer,
      compress,
      slice,
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
      ord_to_doc: Rc::new(RefCell::new(ord_to_doc)),
      data_in,
      configuration,
      disi: RefCell::new(disi),
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
    Ok(self.ord_to_doc.borrow_mut().get_mut(ord)? as usize)
  }

  type KnnVectorValues = Self;

  fn copy(&self) -> Result<Self::KnnVectorValues> {
    Self::new(
      self.configuration.clone(),
      self.base.dimension,
      self.base.size,
      self.base.scalar_quantizer.clone(),
      self.base.compress,
      self.data_in.clone(),
      self.base.similarity_function,
      self.base.vectors_scorer.clone(),
      self.base.inner.lock().slice.try_clone()?,
    )
  }

  fn get_vector_byte_length(&self) -> usize {
    self.base.num_bytes
  }

  fn get_encoding(&self) -> VectorEncoding {
    ByteVectorValues::get_encoding(self)
  }

  type Bits<'a, B>
    = SparseBits<B, I::RandomAccessSlice>
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, accept_docs: Option<B>) -> Option<Self::Bits<'a, B>>
  where
    B: Bits,
  {
    accept_docs.map(|bits| SparseBits::new(bits, self.base.size, self.ord_to_doc.clone()))
  }

  type DocIndexIterator = DocIndexIteratorImpl<I>;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    match self.disi.borrow_mut().take() {
      Some(disi) => Ok(disi),
      None => Err(LuceneError::illegal_state("iterator only called once")),
    }
  }
}

impl<I, F> ByteVectorValues for SparseOffHeapVectorValues<I, F>
where
  I: IndexInput + Clone,
  F: FlatVectorsScorer + Clone,
{
  fn vector_value(&self, ord: usize) -> Result<Cow<'_, VectorValueEnum>> {
    self.base.vector_value(ord)
  }

  type ByteVectorValues = Self;

  fn byte_copy(&self) -> Result<Option<Self::ByteVectorValues>> {
    Ok(Some(Self::new(
      self.configuration.clone(),
      self.base.dimension,
      self.base.size,
      self.base.scalar_quantizer.clone(),
      self.base.compress,
      self.data_in.clone(),
      self.base.similarity_function,
      self.base.vectors_scorer.clone(),
      self.base.inner.lock().slice.try_clone()?,
    )?))
  }

  type VectorScorer = DummyVectorScorer;
}

impl<I, F> QuantizedByteVectorValues
  for SparseOffHeapVectorValues<I, Lucene99ScalarQuantizedVectorScorer<F>>
where
  I: IndexInput + Clone,
  F: FlatVectorsScorer + Clone,
{
  type QuantizedVectorScorer = SparseVectorScorer<I, ScalarQuantizedRandomVectorScorerEnum<Self>>;

  fn get_scalar_quantizer(&self) -> Result<ScalarQuantizer> {
    Ok(self.base.scalar_quantizer.clone())
  }

  fn get_score_correction_constant(&self, ord: usize) -> Result<f32> {
    self.base.get_score_correction_constant(ord)
  }

  fn scorer(&self, target: &[f32]) -> Result<Option<Self::QuantizedVectorScorer>> {
    let copy = QuantizedByteVectorValues::copy(self)?;
    let iterator = copy.iterator()?;
    let similarity_function = copy.base.similarity_function;
    let random_vector_scorer = self.base.vectors_scorer.get_random_vector_scorer_f32(
      similarity_function,
      copy,
      target.to_vec(),
    )?;
    Ok(Some(SparseVectorScorer::new(
      iterator,
      random_vector_scorer,
    )))
  }

  type QuantizedByteVectorValues = Self;

  fn copy(&self) -> Result<Self::QuantizedByteVectorValues> {
    Self::new(
      self.configuration.clone(),
      self.base.dimension,
      self.base.size,
      self.base.scalar_quantizer.clone(),
      self.base.compress,
      self.data_in.clone(),
      self.base.similarity_function,
      self.base.vectors_scorer.clone(),
      self.base.inner.lock().slice.try_clone()?,
    )
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
    let index = self.iterator.index()?.try_convert()?;
    self.random_vector_scorer.score(index)
  }

  type DocIdSetIteratorRef<'a>
    = &'a DocIndexIteratorImpl<I>
  where
    Self: 'a;

  fn iterator(&self) -> Self::DocIdSetIteratorRef<'_> {
    &self.iterator
  }

  type DocIdSetIteratorMut<'a>
    = &'a mut DocIndexIteratorImpl<I>
  where
    Self: 'a;

  fn iterator_mut(&mut self) -> Self::DocIdSetIteratorMut<'_> {
    &mut self.iterator
  }
}

pub struct SparseBits<B, R> {
  accept_docs: B,
  size: usize,
  map: Rc<RefCell<DirectMonotonicReader<R>>>,
  id: Identity,
}

impl<B, R> SparseBits<B, R> {
  fn new(accept_docs: B, size: usize, map: Rc<RefCell<DirectMonotonicReader<R>>>) -> Self {
    Self {
      accept_docs,
      size,
      map,
      id: Identity::new(),
    }
  }
}

impl<B, R> HasIdentity for SparseBits<B, R> {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl<B, R> Bits for SparseBits<B, R>
where
  B: Bits,
  R: RandomAccessInput,
{
  fn get(&self, index: usize) -> Result<bool> {
    let index = self.map.borrow_mut().get_mut(index)? as usize;
    self.accept_docs.get(index)
  }

  fn length(&self) -> usize {
    self.size
  }
}

pub struct EmptyOffHeapVectorValues {
  dimension: usize,
}

impl EmptyOffHeapVectorValues {
  fn new(dimension: usize) -> Self {
    Self { dimension }
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
    ByteVectorValues::get_encoding(self)
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

impl ByteVectorValues for EmptyOffHeapVectorValues {
  fn vector_value(&self, _ord: usize) -> Result<Cow<'_, VectorValueEnum>> {
    Err(LuceneError::unsupported_operation(""))
  }

  type ByteVectorValues = Self;

  fn byte_copy(&self) -> Result<Option<Self::ByteVectorValues>> {
    Err(LuceneError::unsupported_operation(""))
  }

  type VectorScorer = DummyVectorScorer;

  fn scorer(&self, _query: Vec<u8>) -> Result<Option<Self::VectorScorer>> {
    Ok(None)
  }
}

impl QuantizedByteVectorValues for EmptyOffHeapVectorValues {
  type QuantizedVectorScorer = DummyVectorScorer;

  fn get_scalar_quantizer(&self) -> Result<ScalarQuantizer> {
    ScalarQuantizer::new(-1.0, 1.0, 7)
  }

  fn get_score_correction_constant(&self, _ord: usize) -> Result<f32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn scorer(&self, _query: &[f32]) -> Result<Option<Self::QuantizedVectorScorer>> {
    Ok(None)
  }

  type QuantizedByteVectorValues = Self;

  fn copy(&self) -> Result<Self::QuantizedByteVectorValues> {
    Err(LuceneError::unsupported_operation(""))
  }
}

pub enum OffHeapQuantizedByteVectorValuesEnum<I, F>
where
  I: IndexInput,
{
  Empty(EmptyOffHeapVectorValues),
  Dense(DenseOffHeapVectorValues<I::IndexInput, F>),
  Sparse(SparseOffHeapVectorValues<I, F>),
}

impl<I, F> HasIndexSlice for OffHeapQuantizedByteVectorValuesEnum<I, F>
where
  I: IndexInput,
{
  fn seek(&self, pos: usize) -> Result<()> {
    match self {
      Self::Empty(e) => e.seek(pos),
      Self::Dense(e) => e.seek(pos),
      Self::Sparse(e) => e.seek(pos),
    }
  }

  fn read_bytes(&self, b: &mut [u8], offset: usize, len: usize) -> Result<()> {
    match self {
      Self::Empty(e) => e.read_bytes(b, offset, len),
      Self::Dense(e) => e.read_bytes(b, offset, len),
      Self::Sparse(e) => e.read_bytes(b, offset, len),
    }
  }
}

impl<I, F> KnnVectorValues for OffHeapQuantizedByteVectorValuesEnum<I, F>
where
  I: IndexInput + Clone,
  F: FlatVectorsScorer + Clone,
{
  fn dimension(&self) -> usize {
    match self {
      Self::Empty(e) => e.dimension(),
      Self::Dense(e) => e.dimension(),
      Self::Sparse(e) => e.dimension(),
    }
  }

  fn size(&self) -> usize {
    match self {
      Self::Empty(e) => e.size(),
      Self::Dense(e) => e.size(),
      Self::Sparse(e) => e.size(),
    }
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    match self {
      Self::Empty(e) => e.ord_to_doc(ord),
      Self::Dense(e) => e.ord_to_doc(ord),
      Self::Sparse(e) => e.ord_to_doc(ord),
    }
  }

  type KnnVectorValues = Self;

  fn copy(&self) -> Result<Self::KnnVectorValues> {
    match self {
      Self::Empty(_) => Err(LuceneError::unsupported_operation("")),
      Self::Dense(e) => KnnVectorValues::copy(e).map(Self::Dense),
      Self::Sparse(e) => KnnVectorValues::copy(e).map(Self::Sparse),
    }
  }

  fn get_vector_byte_length(&self) -> usize {
    match self {
      Self::Empty(e) => e.get_vector_byte_length(),
      Self::Dense(e) => e.get_vector_byte_length(),
      Self::Sparse(e) => e.get_vector_byte_length(),
    }
  }

  fn get_encoding(&self) -> VectorEncoding {
    match self {
      Self::Empty(e) => ByteVectorValues::get_encoding(e),
      Self::Dense(e) => ByteVectorValues::get_encoding(e),
      Self::Sparse(e) => ByteVectorValues::get_encoding(e),
    }
  }

  type Bits<'a, B>
    = OffHeapQuantizedByteVectorValueBitsEnum<I::RandomAccessSlice, B>
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, accept_docs: Option<B>) -> Option<Self::Bits<'a, B>>
  where
    B: Bits,
  {
    match self {
      Self::Empty(_) => None,
      Self::Dense(e) => e
        .get_accept_ords(accept_docs)
        .map(OffHeapQuantizedByteVectorValueBitsEnum::Dense),
      Self::Sparse(e) => e
        .get_accept_ords(accept_docs)
        .map(OffHeapQuantizedByteVectorValueBitsEnum::Sparse),
    }
  }

  type DocIndexIterator = IterEnum<I>;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    match self {
      Self::Empty(e) => e.iterator().map(IterEnum::Dense),
      Self::Dense(e) => e.iterator().map(IterEnum::Dense),
      Self::Sparse(e) => e.iterator().map(IterEnum::Sparse),
    }
  }
}

impl<I, F> ByteVectorValues for OffHeapQuantizedByteVectorValuesEnum<I, F>
where
  I: IndexInput + Clone,
  F: FlatVectorsScorer + Clone,
{
  fn vector_value(&self, ord: usize) -> Result<Cow<'_, VectorValueEnum>> {
    match self {
      Self::Empty(e) => e.vector_value(ord),
      Self::Dense(e) => e.vector_value(ord),
      Self::Sparse(e) => e.vector_value(ord),
    }
  }

  type ByteVectorValues = Self;

  fn byte_copy(&self) -> Result<Option<Self::ByteVectorValues>> {
    match self {
      Self::Empty(e) => Ok(e.byte_copy()?.map(Self::Empty)),
      Self::Dense(e) => Ok(e.byte_copy()?.map(Self::Dense)),
      Self::Sparse(e) => Ok(e.byte_copy()?.map(Self::Sparse)),
    }
  }

  type VectorScorer = DummyVectorScorer;
}

impl<I, F> QuantizedByteVectorValues
  for OffHeapQuantizedByteVectorValuesEnum<I, Lucene99ScalarQuantizedVectorScorer<F>>
where
  I: IndexInput + Clone,
  F: FlatVectorsScorer + Clone,
{
  type QuantizedVectorScorer = VectorScorerEnum<I, F>;

  fn get_scalar_quantizer(&self) -> Result<ScalarQuantizer> {
    match self {
      Self::Empty(e) => e.get_scalar_quantizer(),
      Self::Dense(e) => e.get_scalar_quantizer(),
      Self::Sparse(e) => e.get_scalar_quantizer(),
    }
  }

  fn get_score_correction_constant(&self, ord: usize) -> Result<f32> {
    match self {
      Self::Empty(e) => e.get_score_correction_constant(ord),
      Self::Dense(e) => e.get_score_correction_constant(ord),
      Self::Sparse(e) => e.get_score_correction_constant(ord),
    }
  }

  fn scorer(&self, target: &[f32]) -> Result<Option<Self::QuantizedVectorScorer>> {
    match self {
      Self::Empty(_) => Ok(None),
      Self::Dense(e) => Ok(
        QuantizedByteVectorValues::scorer(e, target)?
          .map(VectorScorerEnum::Dense),
      ),
      Self::Sparse(e) => Ok(
        QuantizedByteVectorValues::scorer(e, target)?
          .map(VectorScorerEnum::Sparse),
      ),
    }
  }

  type QuantizedByteVectorValues = Self;

  fn copy(&self) -> Result<Self::QuantizedByteVectorValues> {
    match self {
      Self::Empty(e) => QuantizedByteVectorValues::copy(e).map(Self::Empty),
      Self::Dense(e) => QuantizedByteVectorValues::copy(e).map(Self::Dense),
      Self::Sparse(e) => QuantizedByteVectorValues::copy(e).map(Self::Sparse),
    }
  }
}

pub enum OffHeapQuantizedByteVectorValueBitsEnum<R, B> {
  Dense(B),
  Sparse(SparseBits<B, R>),
}

impl<R, B> HasIdentity for OffHeapQuantizedByteVectorValueBitsEnum<R, B>
where
  B: HasIdentity,
{
  fn identity(&self) -> &Identity {
    match self {
      Self::Dense(e) => e.identity(),
      Self::Sparse(e) => e.identity(),
    }
  }
}

impl<R, B> Bits for OffHeapQuantizedByteVectorValueBitsEnum<R, B>
where
  R: RandomAccessInput,
  B: Bits,
{
  fn get(&self, index: usize) -> Result<bool> {
    match self {
      Self::Dense(e) => e.get(index),
      Self::Sparse(e) => e.get(index),
    }
  }

  fn length(&self) -> usize {
    match self {
      Self::Dense(e) => e.length(),
      Self::Sparse(e) => e.length(),
    }
  }

  fn copy_of(&self) -> Result<FixedBitSet> {
    match self {
      Self::Dense(e) => e.copy_of(),
      Self::Sparse(e) => e.copy_of(),
    }
  }

  fn to_string(&self) -> String {
    match self {
      Self::Dense(e) => e.to_string(),
      Self::Sparse(e) => e.to_string(),
    }
  }
}

pub enum IterEnum<I>
where
  I: IndexInput,
{
  Dense(DenseDocIndexIterator),
  Sparse(DocIndexIteratorImpl<I>),
}

impl<I> DocIdSetIterator for IterEnum<I>
where
  I: IndexInput,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::Dense(e) => e.doc_id(),
      Self::Sparse(e) => e.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::Dense(e) => e.next_doc(),
      Self::Sparse(e) => e.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Dense(e) => e.advance(target),
      Self::Sparse(e) => e.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Dense(e) => e.slow_advance(target),
      Self::Sparse(e) => e.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::Dense(e) => e.cost(),
      Self::Sparse(e) => e.cost(),
    }
  }
}

impl<I> DocIndexIterator for IterEnum<I>
where
  I: IndexInput,
{
  fn index(&self) -> Result<i32> {
    match self {
      Self::Dense(e) => e.index(),
      Self::Sparse(e) => e.index(),
    }
  }
}

#[allow(clippy::large_enum_variant)] // Keep vector scoring allocation-free.
pub enum VectorScorerEnum<I, F>
where
  I: IndexInput + Clone,
  F: FlatVectorsScorer + Clone,
{
  Dense(
    DenseVectorScorer<ScalarQuantizedRandomVectorScorerEnum<
      DenseOffHeapVectorValues<I::IndexInput, Lucene99ScalarQuantizedVectorScorer<F>>,
    >>,
  ),
  Sparse(
    SparseVectorScorer<
      I,
      ScalarQuantizedRandomVectorScorerEnum<
        SparseOffHeapVectorValues<I, Lucene99ScalarQuantizedVectorScorer<F>>,
      >,
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

  type DocIdSetIteratorRef<'a>
    = DocIdSetIteratorEnum2<&'a DenseDocIndexIterator, &'a DocIndexIteratorImpl<I>>
  where
    Self: 'a;

  fn iterator(&self) -> Self::DocIdSetIteratorRef<'_> {
    match self {
      Self::Dense(scorer) => DocIdSetIteratorEnum2::A(scorer.iterator()),
      Self::Sparse(scorer) => DocIdSetIteratorEnum2::B(scorer.iterator()),
    }
  }

  type DocIdSetIteratorMut<'a>
    = DocIdSetIteratorEnum2<&'a mut DenseDocIndexIterator, &'a mut DocIndexIteratorImpl<I>>
  where
    Self: 'a;

  fn iterator_mut(&mut self) -> Self::DocIdSetIteratorMut<'_> {
    match self {
      Self::Dense(scorer) => DocIdSetIteratorEnum2::A(scorer.iterator_mut()),
      Self::Sparse(scorer) => DocIdSetIteratorEnum2::B(scorer.iterator_mut()),
    }
  }
}

impl<I, F> OffHeapQuantizedByteVectorValuesEnum<I, F>
where
  I: IndexInput + Clone,
  F: FlatVectorsScorer,
{
  #[allow(clippy::too_many_arguments)]
  pub fn load<S>(
    configuration: Arc<OrdToDocDISIReaderConfiguration>,
    dimension: usize,
    size: usize,
    scalar_quantizer: S,
    similarity_function: VectorSimilarityFunction,
    vectors_scorer: F,
    compress: bool,
    quantized_vector_data_offset: usize,
    quantized_vector_data_length: usize,
    vector_data: I,
  ) -> Result<Self>
  where
    S: Into<Option<ScalarQuantizer>>,
  {
    if configuration.is_empty() {
      return Ok(Self::Empty(EmptyOffHeapVectorValues::new(dimension)));
    }
    let scalar_quantizer = scalar_quantizer
      .into()
      .ok_or_else(|| LuceneError::illegal_state("Missing scalar quantizer"))?;
    let bytes_slice = vector_data.slice(
      "quantized-vector-data",
      quantized_vector_data_offset,
      quantized_vector_data_length,
    )?;
    if configuration.is_dense() {
      Ok(Self::Dense(DenseOffHeapVectorValues::new(
        dimension,
        size,
        scalar_quantizer,
        compress,
        similarity_function,
        vectors_scorer,
        bytes_slice,
      )))
    } else {
      Ok(Self::Sparse(SparseOffHeapVectorValues::new(
        configuration,
        dimension,
        size,
        scalar_quantizer,
        compress,
        vector_data,
        similarity_function,
        vectors_scorer,
        bytes_slice,
      )?))
    }
  }
}
