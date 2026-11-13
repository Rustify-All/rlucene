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
use std::borrow::Cow;
use std::fmt::{Display, Formatter};

use crate::core::analysis::analyzer::Analyzer;
use crate::core::analysis::reader::ReaderEnum;
use crate::core::document::field::{Field, FieldBase, FieldDataEnum};
use crate::core::document::field_type::FieldType;
use crate::core::document::invertable_field::InvertableType;
use crate::core::index::BinaryValueEnum;
use crate::core::index::BytesRef;
use crate::core::index::indexable_field::{
  IndexableField, IndexingTokenStream, ReusedIndexingTokenStream,
};
use crate::core::util::error::lucene_error::Result;
use crate::core::util::number::Number;

/// Type for a stored-only field.
pub mod stored_field_type {
  use crate::core::document::field_type::FieldType;
  use std::sync::LazyLock;
  pub static TYPE: LazyLock<FieldType> = LazyLock::new(|| {
    let mut ft = FieldType::new();
    expect_invariant!(
      ft.set_stored(true),
      "the built-in stored field type is mutable and unfrozen"
    );
    ft.freeze();
    ft
  });
}

/// A field whose value is stored so that
/// [`IndexSearcher::stored_fields`](crate::core::search::index_searcher::IndexSearcher::stored_fields)
/// and [`IndexReader::stored_fields`](crate::core::search::index_searcher::IndexSearcher::stored_fields)
/// will return the field and its value.
pub struct StoredField {
  parent_field: Field,
}

impl StoredField {
  /// Expert: allows you to customize the [`FieldType`].
  ///
  /// # Note
  /// Owned buffers are moved without cloning; borrowed bytes are copied.
  /// Existing `BytesRef` offsets and lengths are preserved.
  ///
  /// # Parameters
  /// - `name`: Field name.
  /// - `bytes`: Binary content convertible to `BytesRef<Vec<u8>>`.
  /// - `field_type`: Custom [`FieldType`] for this field.
  pub fn from_bytes_ref_and_type<T, B>(name: T, bytes: B, file_type: FieldType) -> Result<Self>
  where
    T: Into<Cow<'static, str>>,
    B: Into<BytesRef<Vec<u8>>>,
  {
    let parent_field = Field::from_bytes_ref(name, bytes, file_type)?;
    Ok(Self { parent_field })
  }
  /// Creates a stored-only field with the given binary value.
  ///
  /// # Note
  /// An owned `Vec<u8>` is moved into the field without copying its contents.
  /// Borrowed slices are copied; arrays are moved into a new vector.
  ///
  /// # Parameters
  /// - `name`: Field name.
  /// - `value`: Byte array pointing to binary content.
  pub fn from_binary<T, V>(name: T, value: V) -> Result<Self>
  where
    T: Into<Cow<'static, str>>,
    V: Into<Vec<u8>>,
  {
    let value = value.into();
    let len = value.len();
    debug_assert!(len <= i32::MAX as usize);
    let bytes_ref = BytesRef::from_slice(value, 0, len);
    let parent_field = Field::from_bytes_ref(name, bytes_ref, stored_field_type::TYPE.clone())?;
    Ok(Self { parent_field })
  }
  /// Creates a stored-only field with the given binary value.
  ///
  /// # Note
  /// An owned `Vec<u8>` is moved into the field without copying its contents.
  /// Borrowed slices are copied; arrays are moved into a new vector.
  ///
  /// # Parameters
  /// - `name`: Field name.
  /// - `value`: Byte array pointing to binary content .
  /// - `offset`: Starting position in the byte array.
  /// - `length`: Valid length of the byte array.
  pub fn from_binary_with_range<T, V>(
    name: T,
    value: V,
    offset: usize,
    length: usize,
  ) -> Result<Self>
  where
    T: Into<Cow<'static, str>>,
    V: Into<Vec<u8>>,
  {
    let value = value.into();
    let bytes_ref = BytesRef::from_slice(value, offset, length);
    let parent_field = Field::from_bytes_ref(name, bytes_ref, stored_field_type::TYPE.clone())?;
    Ok(Self { parent_field })
  }
  /// Creates a stored-only field with the given binary value.
  ///
  /// # Note
  /// Owned bytes and `BytesRef` values are moved without cloning their buffers.
  /// Borrowed bytes are copied; existing `BytesRef` offsets are preserved.
  ///
  /// # Parameters
  /// - `name`: Field name.
  /// - `value`: Binary content convertible to `BytesRef<Vec<u8>>`.
  pub fn from_bytes_ref<T, B>(name: T, value: B) -> Result<Self>
  where
    T: Into<Cow<'static, str>>,
    B: Into<BytesRef<Vec<u8>>>,
  {
    let parent_field = Field::from_bytes_ref(name, value, stored_field_type::TYPE.clone())?;
    Ok(Self { parent_field })
  }
  /// Creates a stored-only field with the given string value.
  ///
  /// # Parameters
  /// - `name`: Field name.
  /// - `value`: String value.
  pub fn from_string<'a, T1, T2>(name: T1, value: T2) -> Result<Self>
  where
    T1: Into<Cow<'static, str>>,
    T2: Into<Cow<'a, str>>,
  {
    let parent_field = Field::from_string(name, value, stored_field_type::TYPE.clone())?;
    Ok(Self { parent_field })
  }
  /// Expert: allows customization of the [`FieldType`].
  ///
  /// # Parameters
  /// - `name`: Field name.
  /// - `value`: String value.
  /// - `field_type`: Custom [`FieldType`] for this field.
  pub fn from_string_and_type<'a, T1, T2>(name: T1, value: T2, file_type: FieldType) -> Result<Self>
  where
    T1: Into<Cow<'static, str>>,
    T2: Into<Cow<'a, str>>,
  {
    let parent_field = Field::from_string(name, value, file_type)?;
    Ok(Self { parent_field })
  }
  /// Creates a stored-only field with the given i32 value.
  ///
  /// # Parameters
  /// - `name`: Field name.
  /// - `value`: i32 value.
  pub fn from_i32<T>(name: T, value: i32) -> Result<Self>
  where
    T: Into<Cow<'static, str>>,
  {
    let parent_field = Field::new(name, value, stored_field_type::TYPE.clone());
    Ok(Self { parent_field })
  }
  /// Creates a stored-only field with the given `i64` value.
  ///
  /// # Parameters
  /// - `name`: Field name.
  /// - `value`: Long value.
  pub fn from_i64<T>(name: T, value: i64) -> Result<Self>
  where
    T: Into<Cow<'static, str>>,
  {
    let parent_field = Field::new(name, value, stored_field_type::TYPE.clone());
    Ok(Self { parent_field })
  }
  /// Creates a stored-only field with the given f32 value.
  ///
  /// # Parameters
  /// - `name`: Field name.
  /// - `value`: f32 value.
  pub fn from_f32<T>(name: T, value: f32) -> Result<Self>
  where
    T: Into<Cow<'static, str>>,
  {
    let parent_field = Field::new(name, value, stored_field_type::TYPE.clone());
    Ok(Self { parent_field })
  }
  /// Creates a stored-only field with the given f64 value.
  ///
  /// # Parameters
  /// - `name`: Field name.
  /// - `value`: f64 value.
  pub fn from_f64<T>(name: T, value: f64) -> Result<Self>
  where
    T: Into<Cow<'static, str>>,
  {
    let parent_field = Field::new(name, value, stored_field_type::TYPE.clone());
    Ok(Self { parent_field })
  }
}
impl FieldBase for StoredField {
  fn set_bytes_value<'a, B>(&mut self, value: B) -> Result<()>
  where
    B: Into<BinaryValueEnum<'a>>,
  {
    self.parent_field.set_bytes_value(value)
  }

  fn set_int_value(&mut self, value: i32) -> Result<()> {
    self.parent_field.set_int_value(value)
  }

  fn set_long_value(&mut self, value: i64) -> Result<()> {
    self.parent_field.set_long_value(value)
  }

  fn set_float_value(&mut self, value: f32) -> Result<()> {
    self.parent_field.set_float_value(value)
  }

  fn set_double_value(&mut self, value: f64) -> Result<()> {
    self.parent_field.set_double_value(value)
  }

  fn set_string_value<'a, T>(&mut self, value: T) -> Result<()>
  where
    T: Into<Cow<'a, str>>,
  {
    self.parent_field.set_string_value(value)
  }
}

impl Display for StoredField {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    self.parent_field.fmt(f)
  }
}

impl IndexableField for StoredField {
  fn name(&self) -> &str {
    self.parent_field.name()
  }

  type FieldType<'a>
    = &'a FieldType
  where
    Self: 'a;

  fn field_type(&self) -> Self::FieldType<'_> {
    self.parent_field.field_type()
  }
  fn token_stream<'a, A>(
    &'a mut self,
    analyzer: &'a A,
    reuse_token_stream: &'a mut Option<ReusedIndexingTokenStream>,
  ) -> Result<IndexingTokenStream<'a>>
  where
    A: Analyzer,
  {
    self.parent_field.token_stream(analyzer, reuse_token_stream)
  }

  fn binary_value(&self) -> Result<Option<BinaryValueEnum<'_>>> {
    self.parent_field.binary_value()
  }

  fn take_binary_value(&mut self) -> Result<Option<BytesRef<Vec<u8>>>> {
    self.parent_field.take_binary_value()
  }

  fn string_value(&self) -> Result<Option<Cow<'_, str>>> {
    self.parent_field.string_value()
  }

  fn take_string_value(&mut self) -> Result<Option<String>> {
    self.parent_field.take_string_value()
  }

  fn get_char_sequence_value(&self) -> Result<Option<Cow<'_, str>>> {
    self.parent_field.get_char_sequence_value()
  }

  fn take_reader_value(&mut self) -> Result<Option<ReaderEnum>> {
    self.parent_field.take_reader_value()
  }

  fn numeric_value(&self) -> Result<Option<Number>> {
    self.parent_field.numeric_value()
  }

  type StoredValue<'a>
    = &'a FieldDataEnum
  where
    Self: 'a;

  fn stored_value(&self) -> Result<Option<Self::StoredValue<'_>>> {
    self.parent_field.stored_value()
  }

  fn invertable_type(&self) -> &InvertableType {
    self.parent_field.invertable_type()
  }
}
