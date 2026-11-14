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
use crate::core::analysis::analyzer::Analyzer;
use crate::core::analysis::reader::ReaderEnum;
use crate::core::document::field::{FieldBase, FieldDataEnum};
use crate::core::document::field_type::FieldType;
use crate::core::document::invertable_field::InvertableType;
use crate::core::index::BinaryValueEnum;
use crate::core::index::BytesRef;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::indexable_field::{
  IndexableField, IndexingTokenStream, ReusedIndexingTokenStream,
};
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::number::Number;
use crate::core::util::vector_util::VectorUtil;
use std::borrow::Cow;
use std::fmt::{Display, Formatter};

/// A field that contains a single floating-point numeric vector (or none) for each document. Vectors
/// are dense - that is, every dimension of a vector contains an explicit value, stored packed into
/// an `f32` slice whose length is the vector dimension. Values can be retrieved using
/// [`FloatVectorValues`](crate::core::index::float_vector_values::FloatVectorValues), which is a forward-only docID-based iterator and also offers
/// random-access by dense ordinal (not docId). [`VectorSimilarityFunction`] may be used to
/// compare vectors at query time (for example as part of result ranking). A
/// [`KnnFloatVectorField`] may be associated with a search similarity function defining the metric used
/// for nearest-neighbor search among vectors of that field.
pub struct KnnFloatVectorField {
  name: String,
  field_type: FieldType,
  vector: Vec<f32>,
}

impl KnnFloatVectorField {
  /// A field that contains a single floating-point numeric vector (or none) for each document. Vectors
  /// are dense - that is, every dimension of a vector contains an explicit value, stored packed into
  /// an `f32` slice whose length is the vector dimension. Values can be retrieved using
  /// [`FloatVectorValues`](crate::core::index::float_vector_values::FloatVectorValues), which is a forward-only docID-based iterator and also offers
  /// random-access by dense ordinal (not docId). [`VectorSimilarityFunction`] may be used to
  /// compare vectors at query time (for example as part of result ranking). A
  /// [`KnnFloatVectorField`] may be associated with a search similarity function defining the metric used
  /// for nearest-neighbor search among vectors of that field.
  pub fn create_type(
    v: &[f32],
    similarity_function: VectorSimilarityFunction,
  ) -> Result<FieldType> {
    if v.is_empty() {
      return Err(LuceneError::illegal_argument(
        "cannot index an empty vector",
      ));
    }

    let dimension = v.len() as i32;

    let mut field_type = FieldType::new();

    field_type.set_vector_attributes(dimension, VectorEncoding::FLOAT32(4), similarity_function)?;
    field_type.freeze();
    Ok(field_type)
  }
  /// A convenience method for creating a vector field type.
  ///
  /// # Arguments
  ///
  /// * `dimension` - dimension of vectors
  /// * `similarity_function` - a function defining vector proximity.
  ///
  /// # Errors
  ///
  /// returns [`LuceneErrorKind::IllegalArgument`](crate::core::util::error::lucene_error::LuceneErrorKind::IllegalArgument) if has dimension > 1024.
  pub fn create_field_type(
    dimension: i32,
    similarity_function: VectorSimilarityFunction,
  ) -> Result<FieldType> {
    let mut field_type = FieldType::new();

    field_type.set_vector_attributes(dimension, VectorEncoding::FLOAT32(4), similarity_function)?;

    field_type.freeze();

    Ok(field_type)
  }

  /// Creates a numeric vector field. Fields are single-valued: each document has either one value or
  /// no value. Vectors of a single field share the same dimension and similarity function. Note that
  /// some vector similarities (like [`VectorSimilarityFunction::DotProduct`]) require values to
  /// be unit-length, which can be enforced using [`VectorUtil::l2normalize`].
  ///
  /// # Arguments
  ///
  /// * `name` - field name
  /// * `vector` - Values from a vector, array or slice. Owned vectors are moved; borrowed slices
  ///   are copied.
  /// * `similarity_function` - a function defining vector proximity.
  ///
  /// # Errors
  ///
  /// returns [`LuceneErrorKind::IllegalArgument`](crate::core::util::error::lucene_error::LuceneErrorKind::IllegalArgument) if the vector is empty or has
  /// dimension > 1024.
  pub fn with_similarity_function<N, V>(
    name: N,
    vector: V,
    similarity_function: VectorSimilarityFunction,
  ) -> Result<Self>
  where
    N: Into<String>,
    V: Into<Vec<f32>>,
  {
    let vector = vector.into();
    let field_type = Self::create_type(vector.as_ref(), similarity_function)?;
    VectorUtil::check_finite(vector.as_ref())?;

    Ok(Self {
      name: name.into(),
      field_type,
      vector,
    })
  }

  /// Creates a numeric vector field with the default EUCLIDEAN_HNSW (L2) similarity. Fields are
  /// single-valued: each document has either one value or no value. Vectors of a single field share
  /// the same dimension and similarity function.
  ///
  /// # Arguments
  ///
  /// * `name` - field name
  /// * `vector` - Values from a vector, array or slice. Owned vectors are moved; borrowed slices
  ///   are copied.
  ///
  /// # Errors
  ///
  /// returns [`LuceneErrorKind::IllegalArgument`](crate::core::util::error::lucene_error::LuceneErrorKind::IllegalArgument) if the vector is empty or has
  /// dimension > 1024.
  pub fn new<N, V>(name: N, vector: V) -> Result<Self>
  where
    N: Into<String>,
    V: Into<Vec<f32>>,
  {
    Self::with_similarity_function(name, vector, VectorSimilarityFunction::Euclidean)
  }

  /// Creates a numeric vector field. Fields are single-valued: each document has either one value or
  /// no value. Vectors of a single field share the same dimension and similarity function.
  ///
  /// # Arguments
  ///
  /// * `name` - field name
  /// * `vector` - Values from a vector, array or slice. Owned vectors are moved; borrowed slices
  ///   are copied.
  /// * `field_type` - field type
  ///
  /// # Errors
  ///
  /// returns [`LuceneErrorKind::IllegalArgument`](crate::core::util::error::lucene_error::LuceneErrorKind::IllegalArgument) if the vector is empty or has
  /// dimension > 1024.
  pub fn with_type<N, V>(name: N, vector: V, field_type: FieldType) -> Result<Self>
  where
    N: Into<String>,
    V: Into<Vec<f32>>,
  {
    let name = name.into();
    let vector = vector.into();
    if *field_type.vector_encoding() != VectorEncoding::FLOAT32(4) {
      return Err(LuceneError::illegal_argument(format!(
        "Attempt to create a vector for field {} using f32 values but the field encoding is {:?}",
        name,
        field_type.vector_encoding()
      )));
    }

    if vector.len() as i32 != field_type.vector_dimension() {
      return Err(LuceneError::illegal_argument(
        "The number of vector dimensions does not match the field type",
      ));
    }

    VectorUtil::check_finite(vector.as_ref())?;

    Ok(Self {
      name,
      field_type,
      vector,
    })
  }

  /// Returns the concrete vector slice owned by this field.
  pub fn vector_value(&self) -> &[f32] {
    &self.vector
  }

  /// Set the vector value of this field
  ///
  /// # Arguments
  ///
  /// * `value` - value to set; its length must match the field type.
  ///
  /// # Errors
  ///
  /// returns [`LuceneErrorKind::IllegalArgument`](crate::core::util::error::lucene_error::LuceneErrorKind::IllegalArgument) if value is invalid
  pub fn set_vector_value<V>(&mut self, value: V) -> Result<()>
  where
    V: Into<Vec<f32>>,
  {
    let value = value.into();
    if value.len() != self.field_type.vector_dimension() as usize {
      return Err(LuceneError::illegal_argument(format!(
        "value length {} must match field dimension {}",
        value.len(),
        self.field_type.vector_dimension()
      )));
    }
    self.vector = value;
    Ok(())
  }
}

impl FieldBase for KnnFloatVectorField {}

impl Display for KnnFloatVectorField {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "{}<{}:Float({:?})>",
      self.field_type, self.name, self.vector
    )
  }
}

impl IndexableField for KnnFloatVectorField {
  fn name(&self) -> &str {
    &self.name
  }

  type FieldType<'a>
    = &'a FieldType
  where
    Self: 'a;

  fn field_type(&self) -> Self::FieldType<'_> {
    &self.field_type
  }
  fn token_stream<'a, A>(
    &'a mut self,
    _analyzer: &'a A,
    reuse_token_stream: &'a mut Option<ReusedIndexingTokenStream>,
  ) -> Result<IndexingTokenStream<'a>>
  where
    A: Analyzer,
  {
    if *self.field_type.index_options() == IndexOptions::None {
      return Ok(None);
    }
    debug_assert!(reuse_token_stream.is_none());
    Err(LuceneError::illegal_state(format!(
      "Field must have either TokenStream, String, Reader or Number value; got {}",
      self
    )))
  }

  fn binary_value(&self) -> Result<Option<BinaryValueEnum<'_>>> {
    Ok(None)
  }

  fn take_binary_value(&mut self) -> Result<Option<BytesRef<Vec<u8>>>> {
    self.binary_value().map(|v| v.map(|c| c.into_owned()))
  }

  fn string_value(&self) -> Result<Option<Cow<'_, str>>> {
    Ok(None)
  }

  fn take_string_value(&mut self) -> Result<Option<String>> {
    Ok(None)
  }

  fn take_reader_value(&mut self) -> Result<Option<ReaderEnum>> {
    Ok(None)
  }

  fn numeric_value(&self) -> Result<Option<Number>> {
    Ok(None)
  }

  type StoredValue<'a>
    = &'a FieldDataEnum
  where
    Self: 'a;

  fn stored_value(&self) -> Result<Option<Self::StoredValue<'_>>> {
    if self.field_type.stored() {
      Err(LuceneError::illegal_state(
        "stored field data must be numeric, binary, or a string",
      ))
    } else {
      Ok(None)
    }
  }

  fn invertable_type(&self) -> &InvertableType {
    &InvertableType::TokenStream
  }
  fn float_vector_value(&self) -> Result<&[f32]> {
    Ok(&self.vector)
  }
}
