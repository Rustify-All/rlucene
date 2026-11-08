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
use crate::core::document::field::{Field, FieldBase, FieldDataEnum};
use crate::core::document::field_type::FieldType;
use crate::core::document::invertable_field::InvertableType;
use crate::core::index::BinaryValueEnum;
use crate::core::index::BytesRef;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::indexable_field::{
  IndexableField, IndexingTokenStream, ReusedIndexingTokenStream,
};
use crate::core::util::error::lucene_error::Result;
use crate::core::util::number::Number;
use std::borrow::Cow;
use std::fmt::{Display, Formatter};
use std::sync::LazyLock;

/// Type for straight bytes DocValues.
static TYPE: LazyLock<FieldType> = LazyLock::new(|| {
  let mut ft = FieldType::new();
  expect_invariant!(
    ft.set_doc_values_type(DocValuesType::Binary),
    "the built-in binary doc-values field type is mutable and unfrozen"
  );
  ft.freeze();
  ft
});

/// Field that stores a per-document [`BytesRef`] value.
///
/// The values are stored directly with no sharing, which is a good fit when the fields don't
/// share many values, such as a title field. If values may be shared and sorted it's better to use
/// [`SortedDocValuesField`](crate::core::document::sorted_doc_values_field::SortedDocValuesField).
///
/// If you also need to store the value, you should add a separate
/// [`StoredField`](crate::core::document::stored_field::StoredField) instance.
///
/// See also [`BinaryDocValues`](crate::core::index::binary_doc_values::BinaryDocValues).
pub struct BinaryDocValuesField {
  pub(crate) parent_field: Field,
}
impl BinaryDocValuesField {
  /// Create a new binary DocValues field.
  ///
  /// # Arguments
  ///
  /// * `name` - Field name.
  /// * `value` - Binary content.
  pub fn new<T, B>(name: T, value: B) -> Self
  where
    T: Into<String>,
    B: Into<BytesRef<Vec<u8>>>,
  {
    let value = value.into();
    let parent_field = Field::new(name, value, TYPE.clone());
    Self { parent_field }
  }
}

impl Display for BinaryDocValuesField {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    self.parent_field.fmt(f)
  }
}
impl FieldBase for BinaryDocValuesField {
  fn set_bytes_value<B>(&mut self, value: B) -> Result<()>
  where
    B: Into<BytesRef<Vec<u8>>>,
  {
    self.parent_field.set_bytes_value(value)
  }
}

impl IndexableField for BinaryDocValuesField {
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

  fn string_value(&self) -> Result<Option<Cow<'_, String>>> {
    self.parent_field.string_value()
  }

  fn take_string_value(&mut self) -> Result<Option<String>> {
    self.parent_field.take_string_value()
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
