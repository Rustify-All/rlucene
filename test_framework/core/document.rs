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
use crate::core::analysis::reader::{ReaderEnum, StringReader};
use crate::core::document::field::{Field, FieldBase, FieldDataEnum, IndexingTokenStreamEnum3};
use crate::core::document::field_type::FieldType;
use crate::core::document::invertable_field::InvertableType;
use crate::core::document::stored_field::stored_field_type;
use crate::core::index::BinaryValueEnum;
use crate::core::index::BytesRef;
use crate::core::index::doc_values_skip_index_type::DocValuesSkipIndexType;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::indexable_field::{
  IndexableField, IndexingTokenStream, ReusedIndexingTokenStream,
};
use crate::core::index::indexable_field_type::{IndexableFieldType, IndexableFieldTypeEnum};
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::vector_similarity_function::VectorSimilarityFunction;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::number::Number;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};

pub struct FieldImpl {
  parent_field: Field,
}

impl FieldImpl {
  pub fn new(name: &str, value: BytesRef<Vec<u8>>, field_type: FieldType) -> Self {
    let parent_field = Field::new(name.to_owned(), value, field_type);
    FieldImpl { parent_field }
  }
}

impl FieldBase for FieldImpl {}

impl Display for FieldImpl {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{}", self.parent_field)
  }
}

impl IndexableField for FieldImpl {
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
    _analyzer: &'a A,
    _reuse_token_stream: &'a mut Option<ReusedIndexingTokenStream>,
  ) -> Result<IndexingTokenStream<'a>>
  where
    A: Analyzer,
  {
    Err(LuceneError::unsupported_operation(""))
  }

  fn binary_value(&self) -> Result<Option<BinaryValueEnum<'_>>> {
    self.parent_field.binary_value()
  }

  fn take_binary_value(&mut self) -> Result<Option<BytesRef<Vec<u8>>>> {
    self.parent_field.take_binary_value()
  }

  fn string_value(&self) -> Result<Option<Cow<'_, String>>> {
    self.parent_field.string_value()
  }

  fn take_string_value(&mut self) -> Result<Option<String>> {
    self.parent_field.take_string_value()
  }

  fn get_char_sequence_value(&self) -> Result<Option<Cow<'_, String>>> {
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

pub struct MockIndexableField {
  field: String,
  value: Option<BytesRef<Vec<u8>>>,
  field_type: FieldType,
}

impl MockIndexableField {
  pub(crate) fn new(field: &str, value: Option<BytesRef<Vec<u8>>>, field_type: FieldType) -> Self {
    Self {
      field: field.to_string(),
      value,
      field_type,
    }
  }
}

impl Display for MockIndexableField {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "MockIndexableField<{}>", self.field)
  }
}

impl IndexableField for MockIndexableField {
  fn name(&self) -> &str {
    &self.field
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
    _reuse_token_stream: &'a mut Option<ReusedIndexingTokenStream>,
  ) -> Result<IndexingTokenStream<'a>>
  where
    A: Analyzer,
  {
    Ok(None)
  }

  fn binary_value(&self) -> Result<Option<BinaryValueEnum<'_>>> {
    Ok(self.value.as_ref().map(BinaryValueEnum::Borrowed))
  }

  fn take_binary_value(&mut self) -> Result<Option<BytesRef<Vec<u8>>>> {
    Ok(self.value.take())
  }

  fn string_value(&self) -> Result<Option<Cow<'_, String>>> {
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
    = FieldDataEnum
  where
    Self: 'a;

  fn stored_value(&self) -> Result<Option<Self::StoredValue<'_>>> {
    if let Some(string_value) = self.string_value()? {
      Ok(Some(FieldDataEnum::String(string_value.into_owned())))
    } else {
      Ok(
        self
          .binary_value()?
          .map(|binary_value| FieldDataEnum::Binary(binary_value.into_owned())),
      )
    }
  }

  fn invertable_type(&self) -> &InvertableType {
    &InvertableType::BINARY
  }
}

pub struct MyField {
  counter: usize,
  name: String,
  field_type: MyFieldType,
}

pub struct MyFieldType {
  counter: usize,
}

impl IndexableFieldType for MyFieldType {
  fn stored(&self) -> bool {
    (self.counter & 1) == 0 || (self.counter % 10) == 3
  }

  fn tokenized(&self) -> bool {
    true
  }

  fn store_term_vectors(&self) -> bool {
    self.index_options() != &IndexOptions::None && self.counter % 2 == 1 && self.counter % 10 != 9
  }

  fn store_term_vector_offsets(&self) -> bool {
    self.store_term_vectors() && self.counter % 10 != 9
  }

  fn store_term_vector_positions(&self) -> bool {
    self.store_term_vectors() && self.counter % 10 != 9
  }

  fn store_term_vector_payloads(&self) -> bool {
    self.store_term_vectors() && self.counter % 10 != 9
  }

  fn omit_norms(&self) -> bool {
    false
  }

  fn index_options(&self) -> &IndexOptions {
    if self.counter % 10 == 3 {
      &IndexOptions::None
    } else {
      &IndexOptions::DocsAndFreqsAndPositions
    }
  }

  fn doc_values_type(&self) -> &DocValuesType {
    &DocValuesType::None
  }

  fn doc_values_skip_index_type(&self) -> &DocValuesSkipIndexType {
    &DocValuesSkipIndexType::None
  }

  fn point_dimension_count(&self) -> usize {
    0
  }

  fn point_index_dimension_count(&self) -> usize {
    0
  }

  fn point_num_bytes(&self) -> usize {
    0
  }

  fn vector_dimension(&self) -> i32 {
    0
  }

  fn vector_encoding(&self) -> &VectorEncoding {
    &VectorEncoding::FLOAT32(4)
  }

  fn vector_similarity_function(&self) -> &VectorSimilarityFunction {
    &VectorSimilarityFunction::Euclidean
  }

  fn get_attributes(&self) -> Option<&HashMap<String, String>> {
    None
  }
}

impl<'a> From<&'a MyFieldType> for IndexableFieldTypeEnum<'a> {
  fn from(field_type: &'a MyFieldType) -> Self {
    Self::Custom(field_type)
  }
}

impl MyField {
  pub fn new(counter: usize) -> Result<Self> {
    Ok(Self {
      counter,
      name: format!("f{counter}"),
      field_type: MyFieldType { counter },
    })
  }

  fn reader_value(&self) -> Option<ReaderEnum> {
    if self.counter % 10 == 7 {
      Some(ReaderEnum::from(StringReader::new(format!(
        "text {}",
        self.counter
      ))))
    } else {
      None
    }
  }
}

impl Display for MyField {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "MyField<{}>", self.counter)
  }
}

impl IndexableField for MyField {
  fn name(&self) -> &str {
    &self.name
  }

  fn binary_value(&self) -> Result<Option<BinaryValueEnum<'_>>> {
    if (self.counter % 10) == 3 {
      let mut bytes = vec![0u8; 10];
      for (idx, byte) in bytes.iter_mut().enumerate() {
        *byte = self.counter.wrapping_add(idx) as u8;
      }
      let length = bytes.len();
      Ok(Some(BinaryValueEnum::Owned(BytesRef::from_slice(
        bytes, 0, length,
      ))))
    } else {
      Ok(None)
    }
  }

  fn take_binary_value(&mut self) -> Result<Option<BytesRef<Vec<u8>>>> {
    Ok(self.binary_value()?.map(|value| value.into_owned()))
  }

  fn string_value(&self) -> Result<Option<Cow<'_, String>>> {
    let field_id = self.counter % 10;
    if field_id != 3 && field_id != 7 {
      Ok(Some(Cow::Owned(format!("text {}", self.counter))))
    } else {
      Ok(None)
    }
  }

  fn take_string_value(&mut self) -> Result<Option<String>> {
    Ok(self.string_value()?.map(|value| value.into_owned()))
  }

  fn get_char_sequence_value(&self) -> Result<Option<Cow<'_, String>>> {
    self.string_value()
  }

  fn take_reader_value(&mut self) -> Result<Option<ReaderEnum>> {
    Ok(self.reader_value())
  }

  fn numeric_value(&self) -> Result<Option<Number>> {
    Ok(None)
  }

  type FieldType<'a>
    = &'a MyFieldType
  where
    Self: 'a;

  fn field_type(&self) -> Self::FieldType<'_> {
    &self.field_type
  }

  fn token_stream<'a, A>(
    &'a mut self,
    analyzer: &'a A,
    _reuse_token_stream: &'a mut Option<ReusedIndexingTokenStream>,
  ) -> Result<IndexingTokenStream<'a>>
  where
    A: Analyzer,
  {
    if let Some(reader) = self.reader_value() {
      Ok(Some(IndexingTokenStreamEnum3::AnalyzerTokenStream(
        analyzer.token_stream(self.name(), reader)?,
      )))
    } else if let Some(string_value) = self.string_value()?.map(|value| value.into_owned()) {
      Ok(Some(IndexingTokenStreamEnum3::AnalyzerTokenStream(
        analyzer.token_stream(
          self.name(),
          ReaderEnum::from(StringReader::new(string_value)),
        )?,
      )))
    } else {
      Err(LuceneError::illegal_state(format!(
        "Field must have either TokenStream, String, Reader or Number value; got {}",
        self
      )))
    }
  }

  type StoredValue<'a>
    = FieldDataEnum
  where
    Self: 'a;

  fn stored_value(&self) -> Result<Option<Self::StoredValue<'_>>> {
    if let Some(string_value) = self.string_value()? {
      Ok(Some(FieldDataEnum::String(string_value.into_owned())))
    } else {
      Ok(
        self
          .binary_value()?
          .map(|binary_value| FieldDataEnum::Binary(binary_value.into_owned())),
      )
    }
  }

  fn invertable_type(&self) -> &InvertableType {
    &InvertableType::TokenStream
  }
}

pub struct CustomField {
  field_type: FieldType,
}

impl CustomField {
  pub fn new() -> Result<Self> {
    let mut field_type = FieldType::from_ref(&*stored_field_type::TYPE)?;
    field_type.set_store_term_vectors(true)?;
    field_type.freeze();
    Ok(Self { field_type })
  }
}

impl Display for CustomField {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "CustomField")
  }
}

impl IndexableField for CustomField {
  fn name(&self) -> &str {
    "field"
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
    _reuse_token_stream: &'a mut Option<ReusedIndexingTokenStream>,
  ) -> Result<IndexingTokenStream<'a>>
  where
    A: Analyzer,
  {
    Ok(None)
  }

  fn binary_value(&self) -> Result<Option<BinaryValueEnum<'_>>> {
    Ok(None)
  }

  fn take_binary_value(&mut self) -> Result<Option<BytesRef<Vec<u8>>>> {
    Ok(None)
  }

  fn string_value(&self) -> Result<Option<Cow<'_, String>>> {
    Ok(Some(Cow::Owned("foobar".to_string())))
  }

  fn take_string_value(&mut self) -> Result<Option<String>> {
    Ok(Some("foobar".to_string()))
  }

  fn take_reader_value(&mut self) -> Result<Option<ReaderEnum>> {
    Ok(None)
  }

  fn numeric_value(&self) -> Result<Option<Number>> {
    Ok(None)
  }

  type StoredValue<'a>
    = FieldDataEnum
  where
    Self: 'a;

  fn stored_value(&self) -> Result<Option<Self::StoredValue<'_>>> {
    Ok(None)
  }

  fn invertable_type(&self) -> &InvertableType {
    &InvertableType::TokenStream
  }
}
