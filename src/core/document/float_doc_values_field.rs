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
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::index::BinaryValueEnum;
use crate::core::index::BytesRef;
use crate::core::index::indexable_field::{
  IndexableField, IndexingTokenStream, ReusedIndexingTokenStream,
};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::number::Number;
use std::borrow::Cow;

/// Syntactic sugar for encoding floats as NumericDocValues via [`f32::to_bits`].
///
/// Per-document floating point values can be retrieved via
/// [`LeafReader::get_numeric_doc_values`](crate::core::index::leaf_reader::LeafReader::get_numeric_doc_values).
///
/// Note: in most all cases this will be rather inefficient, requiring four bytes per document.
/// Consider encoding floating point values yourself with only as much precision as you require.
pub struct FloatDocValuesField {
  parent_field: NumericDocValuesField,
}

impl FloatDocValuesField {
  /// Creates a new doc-values field with the specified `f32` value.
  ///
  /// # Arguments
  ///
  /// * `name` - Field name.
  /// * `value` - An `f32` value.
  pub fn new<T>(name: T, value: f32) -> Self
  where
    T: Into<Cow<'static, str>>,
  {
    let int_value = value.to_bits() as i32 as i64;
    let parent_field = NumericDocValuesField::new(name, int_value);
    FloatDocValuesField { parent_field }
  }
}

impl std::fmt::Display for FloatDocValuesField {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    self.parent_field.parent_field.fmt(f)
  }
}

impl IndexableField for FloatDocValuesField {
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

  fn is_reserved(&self) -> bool {
    self.parent_field.is_reserved()
  }
}

impl FieldBase for FloatDocValuesField {
  fn set_long_value(&mut self, _value: i64) -> Result<()> {
    Err(LuceneError::illegal_argument(
      "cannot change value type from Float to Long",
    ))
  }

  fn set_float_value(&mut self, value: f32) -> Result<()> {
    let value = value.to_bits() as i32 as i64;
    self.parent_field.parent_field.set_long_value(value)
  }
}
