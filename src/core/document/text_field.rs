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
use crate::core::document::field::{Field, FieldBase, FieldDataEnum, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::fields::FieldTokenStreamEnum;
use crate::core::document::invertable_field::InvertableType;
use crate::core::index::BinaryValueEnum;
use crate::core::index::BytesRef;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::indexable_field::{
  IndexableField, IndexingTokenStream, ReusedIndexingTokenStream,
};
use crate::core::util::error::lucene_error::Result;
use crate::core::util::number::Number;
use std::borrow::Cow;
use std::fmt;
use std::fmt::Formatter;
use std::sync::LazyLock;

/// Indexed, tokenized, not stored.
pub(crate) static TYPE_NOT_STORED: LazyLock<FieldType> = LazyLock::new(|| {
  let mut ft = FieldType::new();
  expect_invariant!(
    ft.set_index_options(IndexOptions::DocsAndFreqsAndPositions),
    "the built-in non-stored text field index options are valid"
  );
  expect_invariant!(
    ft.set_tokenized(true),
    "the built-in non-stored text field type is mutable and unfrozen"
  );
  ft.freeze();
  ft
});
/// Indexed, tokenized, stored.
pub(crate) static TYPE_STORED: LazyLock<FieldType> = LazyLock::new(|| {
  let mut ft = FieldType::new();
  expect_invariant!(
    ft.set_index_options(IndexOptions::DocsAndFreqsAndPositions),
    "the built-in stored text field index options are valid"
  );
  expect_invariant!(
    ft.set_tokenized(true),
    "the built-in stored text field type is mutable and unfrozen"
  );
  expect_invariant!(
    ft.set_stored(true),
    "the built-in stored text field type is mutable and unfrozen"
  );
  ft.freeze();
  ft
});

/// A field that is indexed and tokenized, without term vectors.
/// For example, this would be used on a `body` field that contains the bulk of
/// a document's text.
pub struct TextField {
  parent_field: Field,
  has_stored_value: bool,
}
impl TextField {
  /// Creates a new un-stored [`TextField`] with a [`ReaderEnum`] value.
  ///
  /// # Parameters
  /// - `name`: Field name.
  /// - `reader`: [`ReaderEnum`] value.
  pub fn from_reader<T, R>(name: T, reader: R) -> Result<Self>
  where
    T: Into<Cow<'static, str>>,
    R: Into<ReaderEnum>,
  {
    let parent_field = Field::from_reader(name, reader, TYPE_NOT_STORED.clone())?;
    Ok(Self {
      parent_field,
      has_stored_value: false,
    })
  }
  /// Creates a new [`TextField`] with a string value.
  ///
  /// # Parameters
  /// - `name`: Field name.
  /// - `value`: String value.
  /// - `store`: [`Store::Yes`] if the content should also be stored.
  pub fn from_string<T1, T2>(name: T1, value: T2, store: Store) -> Result<Self>
  where
    T1: Into<Cow<'static, str>>,
    T2: Into<String>,
  {
    let store = store.into();
    let (field_type, has_stored_value) = if store {
      (TYPE_STORED.clone(), true)
    } else {
      (TYPE_NOT_STORED.clone(), false)
    };
    let parent_field = Field::from_string(name, value, field_type)?;
    Ok(Self {
      parent_field,
      has_stored_value,
    })
  }
  /// Creates a new un-stored [`TextField`] with a [`TokenStreamEnum`](crate::core::analysis::token_stream::TokenStreamEnum) value.
  ///
  /// # Parameters
  /// - `name`: Field name.
  /// - `stream`: [`TokenStream`](crate::core::analysis::token_stream::TokenStream) value.
  pub fn from_token_stream<T, V>(name: T, stream: V) -> Result<Self>
  where
    T: Into<Cow<'static, str>>,
    V: Into<FieldTokenStreamEnum>,
  {
    let parent_field = Field::from_token_stream(name, stream, TYPE_NOT_STORED.clone())?;
    Ok(Self {
      parent_field,
      has_stored_value: false,
    })
  }
}
impl FieldBase for TextField {
  fn set_string_value<'a, T>(&mut self, value: T) -> Result<()>
  where
    T: Into<Cow<'a, str>>,
  {
    self.parent_field.set_string_value(value)?;
    self.has_stored_value = true;
    Ok(())
  }

  fn set_reader_value<R>(&mut self, value: R) -> Result<()>
  where
    R: Into<ReaderEnum>,
  {
    self.parent_field.set_reader_value(value)
  }
}
impl IndexableField for TextField {
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
    if self.has_stored_value {
      self.parent_field.stored_value()
    } else {
      Ok(None)
    }
  }

  fn invertable_type(&self) -> &InvertableType {
    self.parent_field.invertable_type()
  }
}

impl fmt::Display for TextField {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    self.parent_field.fmt(f)
  }
}
