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
use std::sync::LazyLock;

use crate::core::analysis::analyzer::Analyzer;
use crate::core::analysis::reader::ReaderEnum;
use crate::core::document::field::{Field, FieldBase, FieldDataEnum, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::invertable_field::InvertableType;
use crate::core::index::BinaryValueEnum;
use crate::core::index::BytesRef;
use crate::core::index::BytesRefValue;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::indexable_field::{
  IndexableField, IndexingTokenStream, ReusedIndexingTokenStream,
};
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::number::Number;

/// Indexed, not tokenized, omits norms, indexes DOCS_ONLY, not stored.
pub(crate) static TYPE_NOT_STORED: LazyLock<FieldType> = LazyLock::new(|| {
  let mut ft = FieldType::new();
  expect_invariant!(
    ft.set_omit_norms(true),
    "the built-in non-stored string field type is mutable and unfrozen"
  );
  expect_invariant!(
    ft.set_index_options(IndexOptions::Docs),
    "the built-in string field index options are valid"
  );
  expect_invariant!(
    ft.set_tokenized(false),
    "the built-in non-stored string field type is mutable and unfrozen"
  );
  ft.freeze();
  ft
});
/// Indexed, not tokenized, omits norms, indexes DOCS_ONLY, stored.
pub(crate) static TYPE_STORED: LazyLock<FieldType> = LazyLock::new(|| {
  let mut ft = FieldType::new();
  expect_invariant!(
    ft.set_omit_norms(true),
    "the built-in stored string field type is mutable and unfrozen"
  );
  expect_invariant!(
    ft.set_index_options(IndexOptions::Docs),
    "the built-in stored string field index options are valid"
  );
  expect_invariant!(
    ft.set_stored(true),
    "the built-in stored string field type is mutable and unfrozen"
  );
  expect_invariant!(
    ft.set_tokenized(false),
    "the built-in stored string field type is mutable and unfrozen"
  );
  ft.freeze();
  ft
});
/// A field that is indexed but not tokenized: the entire string value is
/// indexed as a single token. For example, this might be used for a `country`
/// field or an `id` field. If sorting on this field is required, add a
/// [`SortedDocValuesField`](crate::core::document::sorted_doc_values_field::SortedDocValuesField)
/// separately to the document.
pub struct StringField {
  parent_field: Field,
}

impl StringField {
  /// Creates a new textual [`StringField`], indexing the provided string value
  /// as a single token.
  ///
  /// # Parameters
  /// - `name`: Field name.
  /// - `value`: String value.
  /// - `stored`: [`Store::Yes`] if the content should also be stored.
  pub fn from_string<T1, T2>(name: T1, value: T2, store: Store) -> Result<Self>
  where
    T1: Into<String>,
    T2: Into<String>,
  {
    let store = store.into();
    let field_type = if store {
      TYPE_STORED.clone()
    } else {
      TYPE_NOT_STORED.clone()
    };
    let value_str = value.into();
    let parent_field = Field::from_string(name, value_str, field_type)?;

    Ok(Self { parent_field })
  }
  /// Creates a new binary [`StringField`], indexing the provided binary
  /// ([`BytesRef`]) value as a single token.
  ///
  /// # Parameters
  /// - `name`: Field name.
  /// - `value`: Binary content convertible to `BytesRef<Vec<u8>>`. Owned buffers
  ///   are moved, borrowed bytes are copied, and `BytesRef` offsets are preserved.
  /// - `stored`: [`Store::Yes`] if the content should also be stored.
  pub fn from_bytes_ref<T, B>(name: T, value: B, store: Store) -> Result<Self>
  where
    T: Into<String>,
    B: Into<BytesRef<Vec<u8>>>,
  {
    let store = store.into();
    let field_type = if store {
      TYPE_STORED.clone()
    } else {
      TYPE_NOT_STORED.clone()
    };
    let parent_field = Field::from_bytes_ref(name, value, field_type)?;
    Ok(Self { parent_field })
  }
}

impl FieldBase for StringField {
  fn set_bytes_value<B>(&mut self, value: B) -> Result<()>
  where
    B: Into<BytesRef<Vec<u8>>>,
  {
    self.parent_field.set_bytes_value(value)
  }

  fn set_string_value<T>(&mut self, value: T) -> Result<()>
  where
    T: Into<String>,
  {
    self.parent_field.set_string_value(value)
  }
}

impl Display for StringField {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    self.parent_field.fmt(f)
  }
}

impl IndexableField for StringField {
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
    match &self.parent_field.fields_data {
      FieldDataEnum::String(value) => Ok(Some(BinaryValueEnum::Slice(BytesRef {
        bytes: value.as_bytes(),
        offset: 0,
        length: value.len(),
      }))),
      FieldDataEnum::Binary(value) => Ok(Some(BinaryValueEnum::Slice(value.as_bytes_ref()))),
      _ => Err(LuceneError::illegal_state(
        "StringField contains neither a string nor a binary value",
      )),
    }
  }

  fn take_binary_value(&mut self) -> Result<Option<BytesRef<Vec<u8>>>> {
    match &self.parent_field.fields_data {
      FieldDataEnum::String(value) => Ok(Some(BytesRef::from_string(value))),
      _ => self.parent_field.take_binary_value(),
    }
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
    &InvertableType::BINARY
  }
}
