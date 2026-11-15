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
use crate::core::index::dummy::dummy_doc_index_iterator::DummyDocIndexIterator;
use crate::core::index::knn_vector_values::{BitsImpl, KnnVectorValues};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::vector_similarity_function::VectorSimilarity;
use crate::core::search::dummy::dummy_vector_scorer::DummyVectorScorer;
use crate::core::search::vector_scorer::VectorScorer;
use crate::core::store::memory_segment_index_input::MappedFloatVectors;
use crate::core::util::bits::Bits;
use crate::core::util::clone::TryClone;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use parking_lot::MappedMutexGuard;
use std::borrow::Cow;
use std::ops::Deref;

/// A read-only floating-point slice that keeps its backing buffer borrowed until dropped.
/// Use [`FloatVectorValue::into_owned`] only when an independent snapshot is needed.
#[derive(Debug)]
pub struct FloatVectorValue<'a>(pub(crate) FloatVectorValueStorage<'a>);

#[derive(Debug)]
pub(crate) enum FloatVectorValueStorage<'a> {
  Borrowed(&'a [f32]),
  Owned(Vec<f32>),
  Buffer(MappedMutexGuard<'a, [f32]>),
}

impl Deref for FloatVectorValue<'_> {
  type Target = [f32];

  fn deref(&self) -> &Self::Target {
    match &self.0 {
      FloatVectorValueStorage::Borrowed(value) => value,
      FloatVectorValueStorage::Owned(value) => value.as_slice(),
      FloatVectorValueStorage::Buffer(value) => value,
    }
  }
}

impl AsRef<[f32]> for FloatVectorValue<'_> {
  fn as_ref(&self) -> &[f32] {
    self
  }
}

impl<'a> From<&'a [f32]> for FloatVectorValue<'a> {
  fn from(value: &'a [f32]) -> Self {
    Self(FloatVectorValueStorage::Borrowed(value))
  }
}

impl From<Vec<f32>> for FloatVectorValue<'_> {
  fn from(value: Vec<f32>) -> Self {
    Self(FloatVectorValueStorage::Owned(value))
  }
}

impl<'a> From<parking_lot::MappedMutexGuard<'a, [f32]>> for FloatVectorValue<'a> {
  fn from(value: parking_lot::MappedMutexGuard<'a, [f32]>) -> Self {
    Self(FloatVectorValueStorage::Buffer(value))
  }
}

impl<'a> From<Cow<'a, [f32]>> for FloatVectorValue<'a> {
  fn from(value: Cow<'a, [f32]>) -> Self {
    Self(match value {
      Cow::Borrowed(value) => FloatVectorValueStorage::Borrowed(value),
      Cow::Owned(value) => FloatVectorValueStorage::Owned(value),
    })
  }
}

impl<'a> TryFrom<Cow<'a, VectorValueEnum>> for FloatVectorValue<'a> {
  type Error = LuceneError;

  fn try_from(value: Cow<'a, VectorValueEnum>) -> Result<Self> {
    let value = match value {
      Cow::Borrowed(VectorValueEnum::Float(value)) => Cow::Borrowed(value.as_slice()),
      Cow::Owned(VectorValueEnum::Float(value)) => Cow::Owned(value),
      _ => return Err(LuceneError::unsupported_operation("")),
    };
    Ok(value.into())
  }
}

impl FloatVectorValue<'_> {
  /// Returns an independent snapshot, moving an already-owned vector without copying.
  pub fn into_owned(self) -> Vec<f32> {
    match self.0 {
      FloatVectorValueStorage::Borrowed(value) => value.to_vec(),
      FloatVectorValueStorage::Owned(value) => value,
      FloatVectorValueStorage::Buffer(value) => value.to_vec(),
    }
  }
}

impl From<FloatVectorValue<'_>> for VectorValueEnum {
  fn from(value: FloatVectorValue<'_>) -> Self {
    Self::Float(value.into_owned())
  }
}

/// This trait provides access to per-document floating point vector values indexed as [`KnnFloatVectorField`](crate::core::document::knn_float_vector_field::KnnFloatVectorField).
pub trait FloatVectorValues: KnnVectorValues {
  /// The concrete slice carrier for this provider. Borrowed heap values can return
  /// `&[f32]`; buffered values keep their guard or independent snapshot.
  type VectorValue<'v>: std::ops::Deref<Target = [f32]> + Into<FloatVectorValue<'v>>
  where
    Self: 'v;

  /// Returns the vector value for an ordinal in `0..size()`.
  /// Returns an out-of-bounds error for an invalid ordinal. The returned array may be shared across calls.
  ///
  /// # Returns
  /// A floating-point slice view; vector encoding is already resolved by this interface.
  ///
  /// The returned view may hold the reader's buffer lock. Drop it before reading,
  /// copying or otherwise accessing the same instance again. Use independent
  /// [`Self::float_copy`] instances for simultaneous reads of reusable buffers.
  fn vector_value(&self, ord: usize) -> Result<Self::VectorValue<'_>>;

  /// Optional best-effort heap read hint. Custom and buffered providers default
  /// to no operation, retaining their original reads and failures.
  fn prefetch_vector(&self, _target_ord: usize) {}

  /// Optional fresh owning reader capability. Borrowed, shared and custom
  /// providers do not forward it and retain their original snapshots/guards.
  fn mapped_float_vectors(&self) -> Option<MappedFloatVectors> {
    None
  }

  /// Compares an immutable mapped row while retaining the original buffer lock.
  /// Unsupported providers return `None` and use ordinary vector_value instead.
  fn compare_mapped_float<S: VectorSimilarity>(
    &self,
    _query: &[f32],
    _vector: &[f32],
    _similarity: S,
  ) -> Option<Result<f32>> {
    None
  }

  type FloatVectorValues: FloatVectorValues;
  /// Creates a copy of this [`KnnVectorValues`] when an independent instance is
  /// needed.
  ///
  /// This is useful when multiple vector values need to be accessed at the same
  /// time, in order to avoid overwriting the underlying vector buffer returned by
  /// a single instance.
  ///
  /// Returning `Some(...)` means that a new independent object was created.
  ///
  /// Returning `None` means that no new object was created, and callers should
  /// continue using `self` directly. In this case the instance must support
  /// simultaneous vector views without locking a shared reusable buffer.
  fn float_copy(&self) -> Result<Option<Self::FloatVectorValues>> {
    Err(LuceneError::unsupported_operation(""))
  }

  type VectorScorer: VectorScorer;
  fn scorer<'a, TV>(&self, _target: TV) -> Result<Option<Self::VectorScorer>>
  where
    TV: Into<std::borrow::Cow<'a, [f32]>>,
  {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_encoding(&self) -> VectorEncoding {
    VectorEncoding::FLOAT32(4)
  }

  fn get_vectors_mut(&mut self) -> Result<&mut Vec<Box<[f32]>>> {
    Err(LuceneError::unsupported_operation(""))
  }
  fn get_vectors(&self) -> Result<&[Box<[f32]>]> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_vectors_capacity(&self) -> Result<usize> {
    Ok(self.get_vectors()?.len())
  }
}

/// Checks the Vector Encoding of a field
pub fn check_field<LR>(reader: &LR, field: &str) -> Result<()>
where
  LR: LeafReader,
{
  if let Some(fi) = reader.get_field_infos()?.field_info_by_name(field)?
    && fi.has_vector_values()
    && *fi.get_vector_encoding() != VectorEncoding::FLOAT32(4)
  {
    return Err(LuceneError::illegal_state(format!(
      "Unexpected vector encoding ({:?}) for field {}(expected={:?})",
      fi.get_vector_encoding(),
      field,
      VectorEncoding::FLOAT32(4)
    )));
  }
  Ok(())
}

/// Creates a [`FloatVectorValues`] from a list of float arrays.
///
/// # Arguments
/// * `vectors` - the list of float arrays
/// * `dim` - the dimension of the vectors
///
/// # Returns
/// a [`FloatVectorValues`] instance
pub fn from_floats(dim: usize) -> FloatVectorValuesImpl {
  FloatVectorValuesImpl::new(dim)
}

pub struct FloatVectorValuesImpl {
  vectors: Vec<Box<[f32]>>,
  dim: usize,
}
// TODO IMPORTANT avoid CLone ?
impl TryClone for FloatVectorValuesImpl {
  fn try_clone(&self) -> Result<Self>
  where
    Self: Sized,
  {
    Ok(Self {
      vectors: self.vectors.clone(),
      dim: self.dim,
    })
  }
}
impl FloatVectorValuesImpl {
  pub(crate) fn new(dim: usize) -> Self {
    Self {
      vectors: Vec::new(),
      dim,
    }
  }
}

impl KnnVectorValues for FloatVectorValuesImpl {
  fn dimension(&self) -> usize {
    self.dim
  }

  fn size(&self) -> usize {
    self.vectors.len()
  }

  type KnnVectorValues = FloatVectorValuesImpl;

  fn get_encoding(&self) -> VectorEncoding {
    FloatVectorValues::get_encoding(self)
  }

  type Bits<'a, B>
    = BitsImpl<B, &'a Self>
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, accept_docs: Option<B>) -> Option<Self::Bits<'a, B>>
  where
    B: Bits,
  {
    self.default_get_accept_ords(accept_docs)
  }

  type DocIndexIterator = DummyDocIndexIterator;
}

impl FloatVectorValues for FloatVectorValuesImpl {
  #[inline]
  fn prefetch_vector(&self, target_ord: usize) {
    if let Some(row) = self.vectors.get(target_ord)
      && let Some(value) = row.first()
    {
      // A safe demand read, not a hardware prefetch instruction. Missing rows
      // remain untouched so the ordinary score retains its original failure.
      std::hint::black_box(*value);
    }
  }

  type VectorValue<'v>
    = &'v [f32]
  where
    Self: 'v;

  #[inline]
  fn vector_value(&self, target_ord: usize) -> Result<Self::VectorValue<'_>> {
    Ok(self.vectors[target_ord].as_ref())
  }

  type FloatVectorValues = crate::core::index::knn_vector_values::NoVectorValuesCopy;

  fn float_copy(&self) -> Result<Option<Self::FloatVectorValues>> {
    Ok(None)
  }

  type VectorScorer = DummyVectorScorer;

  fn get_vectors_mut(&mut self) -> Result<&mut Vec<Box<[f32]>>> {
    Ok(&mut self.vectors)
  }

  fn get_vectors(&self) -> Result<&[Box<[f32]>]> {
    Ok(&self.vectors)
  }

  fn get_vectors_capacity(&self) -> Result<usize> {
    Ok(self.vectors.capacity())
  }
}

use crate::core::codecs::knn_field_vectors_writer::VectorValueEnum;
