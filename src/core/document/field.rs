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
use crate::core::analysis::token_attributes::packed_token_and_binary::BinaryTokenStreamAttributeImpl;
use crate::core::analysis::token_stream::{TokenStream, TokenStreamBase};
use crate::core::codecs::knn_field_vectors_writer::VectorValueEnum;
use crate::core::document::field_type::FieldType;
use crate::core::document::fields::FieldTokenStreamEnum;
use crate::core::document::invertable_field::InvertableType;
use crate::core::index::BinaryValueEnum;
use crate::core::index::BytesRef;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::indexable_field::{
  IndexableField, IndexingTokenStream, ReusedIndexingTokenStream,
};
use crate::core::index::indexable_field_type::IndexableFieldType;
use crate::core::util::attribute_source::{AttributeSource, Attributes};
use crate::core::util::close::Closeable;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::number::Number;
use crate::{either_token_stream, impl_from_for_enum};
use std::borrow::Cow;
use std::fmt;
use std::fmt::{Debug, Display};

/// Expert: directly creates a field for a document. Most users should use one
/// of the convenience implementations:
///
/// - [`TextField`](crate::core::document::text_field::TextField):
///   [`Reader`](std::io::Read) or `String` indexed for full-text search.
/// - [`StringField`](crate::core::document::string_field::StringField): `String`
///   indexed verbatim as a single token.
/// - [`IntField`](crate::core::document::int_field::IntField): `i32` indexed for
///   exact/range queries.
/// - [`LongField`](crate::core::document::long_field::LongField): `i64` indexed for
///   exact/range queries.
/// - [`FloatField`](crate::core::document::float_field::FloatField): `f32` indexed
///   for exact/range queries.
/// - [`DoubleField`](crate::core::document::double_field::DoubleField): `f64` indexed
///   for exact/range queries.
/// - [`SortedDocValuesField`](crate::core::document::sorted_doc_values_field::SortedDocValuesField): `&[u8]` indexed column-wise for sorting/faceting.
/// - [`SortedSetDocValuesField`](crate::core::document::sorted_set_doc_values_field::SortedSetDocValuesField): `SortedSet<&[u8]>` indexed column-wise for sorting/faceting.
/// - [`NumericDocValuesField`](crate::core::document::numeric_doc_values_field::NumericDocValuesField): `i64` indexed column-wise for sorting/faceting.
/// - [`SortedNumericDocValuesField`](crate::core::document::sorted_numeric_doc_values_field::SortedNumericDocValuesField): `SortedSet<i64>` indexed column-wise for sorting/faceting.
/// - [`StoredField`](crate::core::document::stored_field::StoredField): Stored-only
///   value for retrieving in summary results.
///
/// A field is a section of a document. Each field has three parts: name, type,
/// and value. Values may be text (`String`, [`ReaderEnum`], or a pre-analyzed
/// [`TokenStream`]), binary (`&[u8]`), or numeric ([`Number`]). Fields are
/// optionally stored in the index so they can be returned with document hits.
///
/// # Note
/// The field type is an [`IndexableFieldType`]. Modifying the state of the
/// [`IndexableFieldType`] will affect any field using it. It is strongly
/// recommended not to make changes after field instantiation.
pub struct Field {
  /// Field's type.
  indexable_field_type: FieldType,
  /// Field's name.
  pub(crate) name: Box<str>,
  /// Field's value.
  pub(crate) fields_data: FieldDataEnum,
}
impl Field {
  /// Expert: creates a field with no initial value. This is intended to be
  /// used by custom [`Field`] implementations with preconfigured
  /// [`IndexableFieldType`].
  ///
  /// # Parameters
  /// - `name`: Field name.
  /// - `field_type`: Field type.
  ///
  pub fn new<T, FD>(name: T, fields_data: FD, indexable_field_type: FieldType) -> Self
  where
    T: Into<String>,
    FD: Into<FieldDataEnum>,
  {
    Self {
      name: String::into_boxed_str(name.into()),
      fields_data: fields_data.into(),
      indexable_field_type,
    }
  }
  /// Creates a field with a [`ReaderEnum`] value.
  ///
  /// # Parameters
  /// - `name`: Field name.
  /// - `reader`: Reader value.
  /// - `field_type`: Field type.
  ///
  /// # Errors
  /// - Returns an error if the field's type is `stored()`, or if
  ///   `tokenized()` is `false`.
  pub fn from_reader<T, R>(name: T, reader: R, indexable_field_type: FieldType) -> Result<Self>
  where
    T: Into<String>,
    R: Into<ReaderEnum>,
  {
    if indexable_field_type.stored() {
      return Err(LuceneError::illegal_argument(
        "fields with a Reader value cannot be stored",
      ));
    }
    if !indexable_field_type.tokenized() {
      return Err(LuceneError::illegal_argument(
        "non-tokenized fields must use String values",
      ));
    }
    Ok(Field {
      indexable_field_type,
      name: String::into_boxed_str(name.into()),
      fields_data: reader.into().into(),
    })
  }
  /// Creates a field with a [`TokenStream`] value.
  ///
  /// # Parameters
  /// - `name`: Field name.
  /// - `token_stream`: [`TokenStream`] value.
  /// - `field_type`: Field type.
  ///
  /// # Errors
  /// - Returns an error if the field's type is `stored()`, `tokenized()` is
  ///   `false`, or `indexed()` is `false`.
  pub fn from_token_stream<T, V>(
    name: T,
    token_stream: V,
    indexable_field_type: FieldType,
  ) -> Result<Self>
  where
    T: Into<String>,
    V: Into<FieldTokenStreamEnum>,
  {
    if !indexable_field_type.tokenized()
      || indexable_field_type.index_options() == &IndexOptions::None
    {
      return Err(LuceneError::illegal_argument(
        "TokenStream fields must be indexed and tokenized",
      ));
    }
    if indexable_field_type.stored() {
      return Err(LuceneError::illegal_argument(
        "TokenStream fields cannot be stored",
      ));
    }
    let ts = token_stream.into();
    Ok(Field {
      indexable_field_type,
      name: String::into_boxed_str(name.into()),
      fields_data: ts.into(),
    })
  }
  /// Creates a field with a binary value.
  ///
  /// # Note
  /// An owned `Vec<u8>` is moved into the field without copying its contents.
  /// Borrowed slices are copied; arrays are moved into a new vector.
  ///
  /// # Parameters
  /// - `name`: Field name.
  /// - `value`: Binary content from a vector, array or slice.
  /// - `field_type`: Field type.
  ///
  /// # Errors
  /// - Returns an error if the field's type is `indexed()`.
  pub fn from_binary<T, V>(name: T, value: V, indexable_field_type: FieldType) -> Result<Self>
  where
    T: Into<String>,
    V: Into<Vec<u8>>,
  {
    let value = value.into();
    let len = value.len();
    Self::from_binary_range(name, value, 0, len, indexable_field_type)
  }
  /// Creates a field with a binary value.
  ///
  /// # Note
  /// An owned `Vec<u8>` is moved into the field without copying its contents.
  /// Borrowed slices are copied; arrays are moved into a new vector.
  ///
  /// # Parameters
  /// - `name`: Field name.
  /// - `value`: Binary content from a vector, array or slice.
  /// - `offset`: Starting position in the byte array.
  /// - `length`: Valid length of the byte array.
  /// - `field_type`: Field type.
  ///
  /// # Errors
  /// - Returns an error if the field's type is `indexed()`.
  pub fn from_binary_range<T, V>(
    name: T,
    value: V,
    offset: usize,
    length: usize,
    indexable_field_type: FieldType,
  ) -> Result<Self>
  where
    T: Into<String>,
    V: Into<Vec<u8>>,
  {
    let value = value.into();
    let value = BytesRef::from_slice(value, offset, length);
    Self::from_bytes_ref(name, value, indexable_field_type)
  }
  /// Creates a field with a binary value.
  ///
  /// # Note
  /// Owned bytes and `BytesRef` values are moved without cloning their buffers.
  /// Borrowed bytes are copied; existing `BytesRef` offsets are preserved.
  ///
  /// # Parameters
  /// - `name`: Field name.
  /// - `bytes`: Binary content convertible to `BytesRef<Vec<u8>>`.
  /// - `field_type`: Field type.
  ///
  /// # Errors
  /// - Returns an error if the field's type is `indexed()`.
  pub fn from_bytes_ref<T, B>(name: T, bytes: B, indexable_field_type: FieldType) -> Result<Self>
  where
    T: Into<String>,
    B: Into<BytesRef<Vec<u8>>>,
  {
    if indexable_field_type
      .index_options()
      .cmp(&IndexOptions::DocsAndFreqsAndPositionsAndOffsets)
      != std::cmp::Ordering::Less
      || indexable_field_type.store_term_vector_offsets()
    {
      return Err(LuceneError::illegal_argument(
        "It doesn't make sense to index offsets on binary fields",
      ));
    }
    if indexable_field_type.index_options() != &IndexOptions::None
      && indexable_field_type.tokenized()
    {
      return Err(LuceneError::illegal_argument(
        "cannot set a BytesRef value on a tokenized field",
      ));
    }
    if indexable_field_type.index_options() == &IndexOptions::None
      && indexable_field_type.point_dimension_count() == 0
      && indexable_field_type.doc_values_type() == &DocValuesType::None
      && !indexable_field_type.stored()
    {
      return Err(LuceneError::illegal_argument(
        "it doesn't make sense to have a field that is neither indexed, nor doc-valued, nor stored",
      ));
    }
    Ok(Field {
      indexable_field_type,
      name: String::into_boxed_str(name.into()),
      fields_data: FieldDataEnum::Binary(bytes.into()),
    })
  }
  /// Creates a field with a `String` value.
  ///
  /// # Parameters
  /// - `name`: Field name.
  /// - `value`: String value.
  /// - `field_type`: Field type.
  ///
  /// # Errors
  /// - Returns an error if the field's type is neither `indexed()` nor
  ///   `stored()`.
  /// - Returns an error if `indexed()` is `false` but `store_term_vectors()`
  ///   is `true`.
  pub fn from_string<T1, T2>(name: T1, value: T2, indexable_field_type: FieldType) -> Result<Self>
  where
    T1: Into<String>,
    T2: Into<String>,
  {
    if !indexable_field_type.stored() && *indexable_field_type.index_options() == IndexOptions::None
    {
      return Err(LuceneError::illegal_argument(
        "it doesn't make sense to have a field that is neither indexed nor stored",
      ));
    }
    Ok(Field {
      indexable_field_type,
      name: String::into_boxed_str(name.into()),
      fields_data: FieldDataEnum::String(value.into()),
    })
  }
  /// Returns the [`TokenStream`] for this field to be used when indexing, or
  /// `None` if not set. If `None`, the [`ReaderEnum`] value or `String` value
  /// is analyzed to produce the indexed tokens.
  pub fn token_stream_value(&mut self) -> Result<Option<&mut FieldTokenStreamEnum>> {
    match self.fields_data {
      FieldDataEnum::TokenStream(ref mut token_stream) => Ok(Some(token_stream)),
      _ => Ok(None),
    }
  }
  /// Expert: changes the value of this field. This can be used during
  /// indexing to re-use a single [`Field`] instance to improve indexing
  /// speed by reducing GC overhead from creating and reclaiming
  /// [`Field`] instances. Typically, a single [`Document`](crate::core::document::document::Document) instance is also
  /// re-used, which is especially beneficial for small documents.
  ///
  /// # Note
  /// Each [`Field`] instance should only be used once within a single
  /// [`Document`](crate::core::document::document::Document) instance. See [ImproveIndexingSpeed](http://wiki.apache.org/lucene-java/ImproveIndexingSpeed) for details.
  ///
  /// Borrowed text reuses the current string's allocation. Owned strings are
  /// moved into the field. Custom inputs must convert to [`Cow<'a, str>`](Cow).
  pub fn set_string_value<'a, T>(&mut self, value: T) -> Result<()>
  where
    T: Into<Cow<'a, str>>,
  {
    match &mut self.fields_data {
      FieldDataEnum::String(current) => match value.into() {
        Cow::Borrowed(value) => value.clone_into(current),
        Cow::Owned(value) => *current = value,
      },
      _ => {
        return Err(LuceneError::illegal_argument(format!(
          "cannot change value type from {:?} to String",
          self.fields_data
        )));
      },
    }
    Ok(())
  }
  /// Expert: changes the value of this field. See
  /// [`set_string_value`](Field::set_string_value).
  pub fn set_reader_value<R>(&mut self, value: R) -> Result<()>
  where
    R: Into<ReaderEnum>,
  {
    match &self.fields_data {
      FieldDataEnum::Reader(_) => {},
      _ => {
        return Err(LuceneError::illegal_argument(format!(
          "cannot change value type from {:?} to Reader",
          self.fields_data
        )));
      },
    }

    self.fields_data = FieldDataEnum::Reader(value.into());
    Ok(())
  }
  pub fn set_vec_value<V>(&mut self, value: V) -> Result<()>
  where
    V: Into<Vec<u8>>,
  {
    self.set_bytes_value(BytesRef::from_bytes(value.into()))
  }
  /// Expert: changes the value of this field. See
  /// [`set_string_value`](Field::set_string_value).
  ///
  /// Accepts text, byte arrays, vectors, slices, and `BytesRef` values.
  /// Owned buffers are moved; borrowed inputs are copied. `BytesRef` offsets
  /// and lengths are preserved.
  pub fn set_bytes_value<B>(&mut self, value: B) -> Result<()>
  where
    B: Into<BytesRef<Vec<u8>>>,
  {
    match &mut self.fields_data {
      FieldDataEnum::Binary(current) => *current = value.into(),
      _ => {
        return Err(LuceneError::illegal_argument(format!(
          "cannot change value type from {:?} to BytesRef",
          self.fields_data
        )));
      },
    }
    Ok(())
  }
  /// Expert: changes the value of this field. See
  /// [`set_string_value`](Field::set_string_value).
  pub fn set_byte_value(&mut self, value: u8) -> Result<()> {
    match &self.fields_data {
      FieldDataEnum::Number(Number::U8(_)) => {},
      _ => {
        return Err(LuceneError::illegal_argument(format!(
          "cannot change value type from {:?} to Byte",
          self.fields_data
        )));
      },
    }
    self.fields_data = value.into();
    Ok(())
  }
  /// Expert: changes the value of this field. See
  /// [`set_string_value`](Field::set_string_value).
  pub fn set_short_value(&mut self, value: i16) -> Result<()> {
    match &self.fields_data {
      FieldDataEnum::Number(Number::I16(_)) => {},
      _ => {
        return Err(LuceneError::illegal_argument(format!(
          "cannot change value type from {:?} to Short",
          self.fields_data
        )));
      },
    }
    self.fields_data = FieldDataEnum::Number(Number::I16(value));
    Ok(())
  }
  /// Expert: changes the value of this field. See
  /// [`set_string_value`](Field::set_string_value).
  pub fn set_int_value(&mut self, value: i32) -> Result<()> {
    match &self.fields_data {
      FieldDataEnum::Number(Number::I32(_)) => {},
      _ => {
        return Err(LuceneError::illegal_argument(format!(
          "cannot change value type from {:?} to Integer",
          self.fields_data
        )));
      },
    }

    self.fields_data = FieldDataEnum::Number(Number::I32(value));
    Ok(())
  }
  /// Expert: changes the value of this field. See
  /// [`set_string_value`](Field::set_string_value).
  pub fn set_long_value(&mut self, value: i64) -> Result<()> {
    match &mut self.fields_data {
      FieldDataEnum::Number(Number::I64(current)) => *current = value,
      _ => {
        return Err(LuceneError::illegal_argument(format!(
          "cannot change value type from {:?} to Long",
          self.fields_data
        )));
      },
    }
    Ok(())
  }
  /// Expert: changes the value of this field. See
  /// [`set_string_value`](Field::set_string_value).
  pub fn set_float_value(&mut self, value: f32) -> Result<()> {
    match &self.fields_data {
      FieldDataEnum::Number(Number::F32(_)) => {},
      _ => {
        return Err(LuceneError::illegal_argument(format!(
          "cannot change value type from {:?} to Float",
          self.fields_data
        )));
      },
    }

    self.fields_data = FieldDataEnum::Number(Number::F32(value));
    Ok(())
  }
  /// Expert: changes the value of this field. See
  /// [`set_string_value`](Field::set_string_value).
  pub fn set_double_value(&mut self, value: f64) -> Result<()> {
    match &self.fields_data {
      FieldDataEnum::Number(Number::F64(_)) => {},
      _ => {
        return Err(LuceneError::illegal_argument(format!(
          "cannot change value type from {:?} to Double",
          self.fields_data
        )));
      },
    }

    self.fields_data = FieldDataEnum::Number(Number::F64(value));
    Ok(())
  }
  /// Expert: sets the token stream to be used for indexing.
  pub fn set_token_stream<S>(&mut self, token_stream: S) -> Result<()>
  where
    S: Into<FieldTokenStreamEnum>,
  {
    match &self.fields_data {
      FieldDataEnum::TokenStream(_) => {},
      _ => {
        return Err(LuceneError::illegal_argument(format!(
          "cannot change value type from {:?} to TokenStream",
          self.fields_data
        )));
      },
    }

    self.fields_data = FieldDataEnum::TokenStream(token_stream.into());
    Ok(())
  }
}
impl IndexableField for Field {
  fn name(&self) -> &str {
    &self.name
  }

  type FieldType<'a>
    = &'a FieldType
  where
    Self: 'a;

  /// Returns the [`FieldType`] for this field.
  fn field_type(&self) -> Self::FieldType<'_> {
    &self.indexable_field_type
  }
  fn token_stream<'a, A>(
    &'a mut self,
    analyzer: &'a A,
    reuse_token_stream: &'a mut Option<ReusedIndexingTokenStream>,
  ) -> Result<IndexingTokenStream<'a>>
  where
    A: Analyzer,
  {
    if *self.field_type().index_options() == IndexOptions::None {
      return Ok(None);
    }

    if !self.field_type().tokenized() {
      if let Some(string_value) = self.string_value()? {
        if !matches!(
          reuse_token_stream.as_ref(),
          Some(ReusedIndexingTokenStream::B(_))
        ) {
          *reuse_token_stream = Some(ReusedIndexingTokenStream::B(StringTokenStream::new()));
        }

        let stream = reuse_token_stream
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("should StringTokenStream here"))?;
        match stream {
          ReusedIndexingTokenStream::B(s) => s.set_value(string_value.as_str())?,
          ReusedIndexingTokenStream::A(_) => {
            return Err(LuceneError::illegal_state("should StringTokenStream here"));
          },
        }

        return Ok(Some(IndexingTokenStreamEnum3::Reused(stream)));
      }

      if let Some(binary_value) = self.binary_value()?.map(|v| v.into_owned()) {
        if !matches!(
          reuse_token_stream.as_ref(),
          Some(ReusedIndexingTokenStream::A(_))
        ) {
          *reuse_token_stream = Some(ReusedIndexingTokenStream::A(BinaryTokenStream::new()?));
        }
        let stream = reuse_token_stream
          .as_mut()
          .ok_or_else(|| LuceneError::illegal_state("should BinaryTokenStream here"))?;
        match stream {
          ReusedIndexingTokenStream::A(s) => s.set_value(binary_value),
          ReusedIndexingTokenStream::B(_) => {
            return Err(LuceneError::illegal_state("should BinaryTokenStream here"));
          },
        }
        return Ok(Some(IndexingTokenStreamEnum3::Reused(stream)));
      }
      debug_assert!(reuse_token_stream.is_none());
    }

    debug_assert!(reuse_token_stream.is_none());
    if let FieldDataEnum::TokenStream(ref mut token_stream) = self.fields_data {
      return Ok(Some(IndexingTokenStreamEnum3::FieldTokenStream(
        token_stream,
      )));
    }

    if let Some(reader) = self.take_reader_value()? {
      Ok(Some(IndexingTokenStreamEnum3::AnalyzerTokenStream(
        analyzer.token_stream(self.name(), reader)?,
      )))
    } else if let Some(v) = self.string_value()? {
      let token_stream = match v {
        Cow::Borrowed(text) => analyzer.token_stream_from_text(self.name(), text)?,
        Cow::Owned(text) => analyzer.token_stream(self.name(), ReaderEnum::from(text))?,
      };
      Ok(Some(IndexingTokenStreamEnum3::AnalyzerTokenStream(
        token_stream,
      )))
    } else {
      Err(LuceneError::illegal_state(format!(
        "Field must have either TokenStream, String, Reader or Number value; got {}",
        self
      )))
    }
  }

  fn binary_value(&self) -> Result<Option<BinaryValueEnum<'_>>> {
    if let FieldDataEnum::Binary(bytes) = &self.fields_data {
      Ok(Some(BinaryValueEnum::Borrowed(bytes)))
    } else {
      Ok(None)
    }
  }

  fn take_binary_value(&mut self) -> Result<Option<BytesRef<Vec<u8>>>> {
    if !matches!(self.fields_data, FieldDataEnum::Binary(_)) {
      return Ok(None);
    }
    if let FieldDataEnum::Binary(binary) =
      std::mem::replace(&mut self.fields_data, FieldDataEnum::Dummy(()))
    {
      Ok(Some(binary))
    } else {
      Ok(None)
    }
  }

  /// Returns the value of the field as a `String`, or `None` if not set.
  /// If `None`, the [`ReaderEnum`] value or binary value is used.
  ///
  /// Exactly one of `string_value()`, `reader_value()`, or `binary_value()`
  /// must be set.
  fn string_value(&self) -> Result<Option<Cow<'_, String>>> {
    match &self.fields_data {
      FieldDataEnum::String(s) => Ok(Some(Cow::Borrowed(s))),
      FieldDataEnum::Number(n) => Ok(Some(Cow::Owned(n.to_string()))),
      _ => Ok(None),
    }
  }

  fn take_string_value(&mut self) -> Result<Option<String>> {
    if !matches!(self.fields_data, FieldDataEnum::String(_))
      && !matches!(self.fields_data, FieldDataEnum::Number(_))
    {
      return Ok(None);
    }
    match std::mem::replace(&mut self.fields_data, FieldDataEnum::Dummy(())) {
      FieldDataEnum::String(s) => Ok(Some(s)),
      FieldDataEnum::Number(n) => Ok(Some(n.to_string())),
      _ => Ok(None),
    }
  }

  fn get_char_sequence_value(&self) -> Result<Option<Cow<'_, String>>> {
    if let FieldDataEnum::String(s) = &self.fields_data {
      Ok(Some(Cow::Borrowed(s)))
    } else {
      self.string_value()
    }
  }

  /// Returns the value of the field as a [`ReaderEnum`], or `None` if not set.
  /// If `None`, the `String` value or binary value is used.
  ///
  /// Exactly one of `string_value()`, `reader_value()`, or `binary_value()`
  /// must be set.
  fn take_reader_value(&mut self) -> Result<Option<ReaderEnum>> {
    if !matches!(self.fields_data, FieldDataEnum::Reader(_)) {
      return Ok(None);
    }
    if let FieldDataEnum::Reader(reader) =
      std::mem::replace(&mut self.fields_data, FieldDataEnum::Dummy(()))
    {
      Ok(Some(reader))
    } else {
      Ok(None)
    }
  }

  fn numeric_value(&self) -> Result<Option<Number>> {
    if let FieldDataEnum::Number(n) = &self.fields_data {
      Ok(Some(n.clone()))
    } else {
      Ok(None)
    }
  }

  type StoredValue<'a>
    = &'a FieldDataEnum
  where
    Self: 'a;

  fn stored_value(&self) -> Result<Option<Self::StoredValue<'_>>> {
    if !self.indexable_field_type.stored() {
      return Ok(None);
    }

    match &self.fields_data {
      FieldDataEnum::Number(_) | FieldDataEnum::Binary(_) | FieldDataEnum::String(_) => {
        Ok(Some(&self.fields_data))
      },
      _ => Err(LuceneError::illegal_state(
        "stored field data must be numeric, binary, or a string",
      )),
    }
  }

  fn invertable_type(&self) -> &InvertableType {
    &InvertableType::TokenStream
  }
  fn float_vector_value(&self) -> Result<&[f32]> {
    if let FieldDataEnum::VectorValue(v) = &self.fields_data {
      v.as_floats()
    } else {
      Err(LuceneError::unsupported_operation(""))
    }
  }
  fn byte_vector_value(&self) -> Result<&[u8]> {
    if let FieldDataEnum::VectorValue(v) = &self.fields_data {
      v.as_bytes()
    } else {
      Err(LuceneError::unsupported_operation(""))
    }
  }
}
impl FieldBase for Field {}
impl Display for Field {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    write!(f, "{}<{}:", self.indexable_field_type, self.name)?;

    write!(f, "{}", self.fields_data)?;

    write!(f, ">")
  }
}

pub trait FieldBase {
  /// Updates binary content from text, byte arrays, vectors, slices, or `BytesRef`.
  /// Supporting fields move owned buffers and copy borrowed inputs.
  fn set_bytes_value<B>(&mut self, _value: B) -> Result<()>
  where
    B: Into<BytesRef<Vec<u8>>>,
  {
    Err(LuceneError::not_implemented(
      "set_bytes_value not implement",
    ))
  }
  fn set_byte_value(&mut self, _value: u8) -> Result<()> {
    Err(LuceneError::not_implemented("set_byte_value not implement"))
  }
  fn set_short_value(&mut self, _value: i16) -> Result<()> {
    Err(LuceneError::not_implemented(
      "set_short_value not implement",
    ))
  }
  fn set_int_value(&mut self, _value: i32) -> Result<()> {
    Err(LuceneError::not_implemented("set_int_value not implement"))
  }
  fn set_long_value(&mut self, _value: i64) -> Result<()> {
    Err(LuceneError::not_implemented("set_long_value not implement"))
  }
  fn set_float_value(&mut self, _value: f32) -> Result<()> {
    Err(LuceneError::not_implemented(
      "set_float_value not implement",
    ))
  }
  fn set_double_value(&mut self, _value: f64) -> Result<()> {
    Err(LuceneError::not_implemented(
      "set_double_value not implement",
    ))
  }
  fn set_token_stream<S>(&mut self, _token_stream: S) -> Result<()>
  where
    S: Into<FieldTokenStreamEnum>,
  {
    Err(LuceneError::not_implemented(
      "set_token_stream not implement",
    ))
  }
  fn set_string_value<'a, T>(&mut self, _value: T) -> Result<()>
  where
    T: Into<Cow<'a, str>>,
  {
    Err(LuceneError::not_implemented(
      "set_string_value not implement",
    ))
  }
  /// Updates reader content from a concrete reader or another `ReaderEnum` input.
  fn set_reader_value<R>(&mut self, _value: R) -> Result<()>
  where
    R: Into<ReaderEnum>,
  {
    Err(LuceneError::not_implemented(
      "set_reader_value not implement",
    ))
  }
}
/// Specifies whether and how a field should be stored.
pub enum Store {
  /// Store the original field value in the index. This is useful for short
  /// texts like a document's title which should be displayed with the
  /// results. The value is stored in its original form, i.e. no analyzer
  /// is used before it is stored.
  Yes,

  /// Do not store the field value in the index.
  No,
}
impl From<Store> for bool {
  fn from(store: Store) -> bool {
    matches!(store, Store::Yes)
  }
}

#[derive(Debug)]
pub enum FieldDataEnum {
  Number(Number),
  Binary(BytesRef<Vec<u8>>),
  String(String),
  Reader(ReaderEnum),
  TokenStream(FieldTokenStreamEnum),
  // used to std::mem::replace(FieldDataEnum)
  Dummy(()),
  VectorValue(VectorValueEnum),
}
impl FieldDataEnum {
  /// Clones field data when its value supports independent ownership.
  ///
  /// Token streams carry mutable iteration state and cannot be cloned.
  pub fn try_clone(&self) -> Result<Self> {
    match self {
      Self::Number(n) => Ok(Self::Number(n.clone())),
      Self::Binary(b) => Ok(Self::Binary(b.clone())),
      Self::String(s) => Ok(Self::String(s.clone())),
      Self::Reader(r) => Ok(Self::Reader(r.clone())),
      Self::TokenStream(_) => Err(LuceneError::illegal_state("token stream cannot be cloned")),
      Self::Dummy(d) => {
        let _: () = *d;
        Ok(Self::Dummy(()))
      },
      Self::VectorValue(v) => Ok(Self::VectorValue(v.clone())),
    }
  }
}

impl From<i32> for FieldDataEnum {
  fn from(v: i32) -> Self {
    FieldDataEnum::Number(Number::I32(v))
  }
}

impl From<i64> for FieldDataEnum {
  fn from(v: i64) -> Self {
    FieldDataEnum::Number(Number::I64(v))
  }
}

impl From<u8> for FieldDataEnum {
  fn from(v: u8) -> Self {
    FieldDataEnum::Number(Number::U8(v))
  }
}

impl From<i16> for FieldDataEnum {
  fn from(v: i16) -> Self {
    FieldDataEnum::Number(Number::I16(v))
  }
}

impl From<f32> for FieldDataEnum {
  fn from(v: f32) -> Self {
    FieldDataEnum::Number(Number::F32(v))
  }
}

impl From<f64> for FieldDataEnum {
  fn from(v: f64) -> Self {
    FieldDataEnum::Number(Number::F64(v))
  }
}
impl_from_for_enum!(
    FieldDataEnum,
    BytesRef<Vec<u8>> => Binary,
    String => String,
    ReaderEnum => Reader,
    FieldTokenStreamEnum => TokenStream,
     VectorValueEnum => VectorValue,
);

impl From<&str> for FieldDataEnum {
  fn from(s: &str) -> Self {
    FieldDataEnum::String(s.to_string())
  }
}

impl Display for FieldDataEnum {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    match self {
      FieldDataEnum::Number(n) => write!(f, "{}", n),
      FieldDataEnum::Binary(b) => write!(f, "{}", b),
      FieldDataEnum::String(s) => write!(f, "{}", s),
      FieldDataEnum::Reader(r) => write!(f, "{:?}", r),
      FieldDataEnum::TokenStream(t) => write!(f, "{:?}", t),
      FieldDataEnum::Dummy(s) => write!(f, "{:?}", s),
      FieldDataEnum::VectorValue(v) => write!(f, "{:?}", v),
    }
  }
}
/// Creates a new TokenStream that returns a BytesRef as single token
pub struct BinaryTokenStream {
  used: bool,
  value: Option<BytesRef<Vec<u8>>>,
  token_stream_base: TokenStreamBase,
}

impl BinaryTokenStream {
  /// Creates a new TokenStream that returns a BytesRef as single token.
  pub(crate) fn new() -> Result<Self> {
    Ok(Self {
      used: false,
      value: None,
      token_stream_base: TokenStreamBase::new(BinaryTokenStreamAttributeImpl::new()?.into()),
    })
  }

  /// Sets the bytes value.
  pub(crate) fn set_value(&mut self, value: BytesRef<Vec<u8>>) {
    self.value = Some(value);
  }
}

impl Drop for BinaryTokenStream {
  fn drop(&mut self) {
    let _ = self.close();
  }
}

impl Closeable for BinaryTokenStream {
  fn close(&mut self) -> Result<()> {
    let _ = self.value.take();
    Ok(())
  }
}

impl TokenStream for BinaryTokenStream {
  fn increment_token(&mut self) -> Result<bool> {
    if self.used {
      return Ok(false);
    }
    self.token_stream_base.att.clear_attributes()?;
    let value = self.value.take();
    self.token_stream_base.att.set_bytes_ref(value)?;
    self.used = true;
    Ok(true)
  }

  fn end(&mut self) -> Result<()> {
    self.default_end()
  }

  fn reset(&mut self) -> Result<()> {
    self.used = false;
    Ok(())
  }

  fn get_attribute_source(&self) -> &Attributes {
    &self.token_stream_base.att
  }

  fn get_attribute_source_mut(&mut self) -> &mut Attributes {
    &mut self.token_stream_base.att
  }
}

pub struct StringTokenStream {
  used: bool,
  value: String,
  has_value: bool,
  end_offset: i32,
  token_stream_base: TokenStreamBase,
}
impl StringTokenStream {
  /// Creates a new TokenStream that returns a String as single token.
  pub(crate) fn new() -> Self {
    Self {
      used: false,
      value: String::new(),
      has_value: false,
      end_offset: 0,
      token_stream_base: TokenStreamBase::new(Attributes::default()),
    }
  }
  pub(crate) fn set_value(&mut self, value: &str) -> Result<()> {
    let utf16_length = if value.is_ascii() {
      value.len()
    } else if value.len() <= char::MAX.len_utf8() {
      value.chars().map(char::len_utf16).sum()
    } else {
      // Rust strings already contain valid scalar values. Four-byte UTF-8
      // sequences contribute one extra unit to Java-compatible offsets.
      value.chars().count() + value.bytes().filter(|byte| *byte >= 0xF0).count()
    };
    self.end_offset = i32::try_from(utf16_length)
      .map_err(|_| LuceneError::illegal_argument("string UTF-16 length exceeds i32::MAX"))?;
    self.value.clear();
    self.value.push_str(value);
    self.has_value = true;
    Ok(())
  }
}

impl Drop for StringTokenStream {
  fn drop(&mut self) {
    let _ = self.close();
  }
}

impl Closeable for StringTokenStream {
  fn close(&mut self) -> Result<()> {
    self.has_value = false;
    Ok(())
  }
}

impl TokenStream for StringTokenStream {
  fn increment_token(&mut self) -> Result<bool> {
    if self.used {
      return Ok(false);
    }
    self.token_stream_base.att.clear_attributes()?;
    if !self.has_value {
      return Err(LuceneError::illegal_argument("set_value() not call?"));
    }
    self.token_stream_base.att.append_str(Some(&self.value))?;
    self.token_stream_base.att.set_offset(0, self.end_offset)?;
    self.used = true;
    Ok(true)
  }

  fn end(&mut self) -> Result<()> {
    self.default_end()?;
    if !self.has_value {
      return Err(LuceneError::illegal_state(
        "StringTokenStream value is not set",
      ));
    }
    let final_offset = self.end_offset;
    self
      .token_stream_base
      .att
      .set_offset(final_offset, final_offset)
  }

  fn reset(&mut self) -> Result<()> {
    self.used = false;
    Ok(())
  }

  fn get_attribute_source(&self) -> &Attributes {
    &self.token_stream_base.att
  }

  fn get_attribute_source_mut(&mut self) -> &mut Attributes {
    &mut self.token_stream_base.att
  }
}
either_token_stream!(pub IndexingTokenStreamEnum3 { AnalyzerTokenStream: A, Reused: B,FieldTokenStream:C });
