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
use crate::core::document::field::{Field, FieldBase, FieldDataEnum, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::invertable_field::InvertableType;
use crate::core::index::BinaryValueEnum;
use crate::core::index::BytesRef;
use crate::core::index::BytesRefValue;
use crate::core::index::indexable_field::{
  IndexableField, IndexingTokenStream, ReusedIndexingTokenStream,
};
use crate::core::search::index_or_doc_values_query::IndexOrDocValuesQuery;
use crate::core::search::multi_term_query::{DOC_VALUES_REWRITE, MultiTermQuerySet};
use crate::core::search::sort_field_enum::SortFieldEnum;
use crate::core::search::sorted_set_selector::SortedSetSelectorType;
use crate::core::search::sorted_set_sort_field::SortedSetSortField;
use crate::core::search::term_in_set_query::TermInSetQuery;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::number::Number;
pub mod keyword {
  use crate::core::document::field_type::FieldType;
  use crate::core::index::doc_values_type::DocValuesType;
  use crate::core::index::index_options::IndexOptions;
  use std::sync::LazyLock;

  pub(crate) static FIELD_TYPE: LazyLock<FieldType> = LazyLock::new(|| {
    let mut ft = FieldType::new();
    expect_invariant!(
      ft.set_index_options(IndexOptions::Docs),
      "the built-in keyword field index options are valid"
    );
    expect_invariant!(
      ft.set_omit_norms(true),
      "the built-in keyword field type is mutable and unfrozen"
    );
    expect_invariant!(
      ft.set_tokenized(false),
      "the built-in keyword field type is mutable and unfrozen"
    );
    expect_invariant!(
      ft.set_doc_values_type(DocValuesType::SortedSet),
      "the built-in keyword field doc-values type is valid"
    );
    ft.freeze();
    ft
  });

  pub(crate) static FIELD_TYPE_STORED: LazyLock<FieldType> = LazyLock::new(|| {
    let mut ft = expect_invariant!(
      FieldType::from_ref(&*FIELD_TYPE),
      "the built-in keyword field type can be copied"
    );
    expect_invariant!(
      ft.set_stored(true),
      "the copied keyword field type is mutable and unfrozen"
    );
    ft.freeze();
    ft
  });
}

pub struct KeywordField {
  parent_field: Field,
  has_stored_value: bool,
}

impl KeywordField {
  /// Accepts text, owned or borrowed bytes, and `BytesRef` values.
  /// Owned buffers are moved; borrowed inputs are copied.
  pub fn from_bytes_ref<T, B>(name: T, value: B, store: Store) -> Result<Self>
  where
    T: Into<String>,
    B: Into<BytesRef<Vec<u8>>>,
  {
    let store = store.into();
    let (ft, has_stored_value) = if store {
      (keyword::FIELD_TYPE_STORED.clone(), true)
    } else {
      (keyword::FIELD_TYPE.clone(), false)
    };

    let parent_field = Field::from_bytes_ref(name, value, ft)?;

    Ok(Self {
      parent_field,
      has_stored_value,
    })
  }

  pub fn from_string<T1, T2>(name: T1, value: T2, store: Store) -> Result<Self>
  where
    T1: Into<String>,
    T2: Into<String>,
  {
    let store = store.into();
    let (ft, has_stored_value) = if store {
      (keyword::FIELD_TYPE_STORED.clone(), true)
    } else {
      (keyword::FIELD_TYPE.clone(), false)
    };

    let v = value.into();
    let parent_field = Field::from_string(name, v, ft)?;

    Ok(Self {
      parent_field,
      has_stored_value,
    })
  }
  /// Create a new [`SortField`](crate::core::search::sort_field::SortField) for [`BytesRef`] values.
  ///
  /// * `field` - Field name.
  /// * `reverse` - true if natural order should be reversed.
  /// * `selector` - custom selector type for choosing the sort value from the set.
  pub fn new_sort_field<T>(
    field: T,
    reverse: bool,
    selector: SortedSetSelectorType,
  ) -> Result<SortFieldEnum>
  where
    T: Into<String>,
  {
    Ok(SortedSetSortField::with_selector(field, reverse, selector)?.into())
  }

  /// Create a query that matches any of the specified values. This is the keyword equivalent of
  /// [`PointInSetQuery`](crate::core::search::point_in_set_query::PointInSetQuery) for point fields.
  ///
  /// # Arguments
  ///
  /// * `field` - Field name.
  /// * `values` - Values to match.
  ///
  /// Accepts arrays, vectors, or iterators of text, bytes, or `BytesRef` values.
  /// For an empty set, pass `std::iter::empty::<&str>()`.
  pub fn new_set_query<T, I, B>(field: T, values: I) -> Result<IndexOrDocValuesQuery>
  where
    T: Into<String>,
    I: IntoIterator<Item = B>,
    B: Into<BytesRef<Vec<u8>>>,
  {
    let field = field.into();
    let index_query = TermInSetQuery::new(field, values)?;
    let dv_query = index_query.clone_with_rewrite_method(DOC_VALUES_REWRITE);
    Ok(IndexOrDocValuesQuery::new(
      MultiTermQuerySet::from(index_query),
      MultiTermQuerySet::from(dv_query),
    ))
  }
}

impl FieldBase for KeywordField {
  fn set_string_value<'a, T>(&mut self, value: T) -> Result<()>
  where
    T: Into<Cow<'a, str>>,
  {
    self.parent_field.set_string_value(value)?;
    self.has_stored_value = true;
    Ok(())
  }

  fn set_bytes_value<B>(&mut self, value: B) -> Result<()>
  where
    B: Into<BytesRef<Vec<u8>>>,
  {
    self.parent_field.set_bytes_value(value)?;
    self.has_stored_value = true;
    Ok(())
  }
}

impl IndexableField for KeywordField {
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

  #[inline]
  fn binary_value(&self) -> Result<Option<BinaryValueEnum<'_>>> {
    match &self.parent_field.fields_data {
      FieldDataEnum::String(value) => Ok(Some(BinaryValueEnum::Slice(BytesRef {
        bytes: value.as_bytes(),
        offset: 0,
        length: value.len(),
      }))),
      FieldDataEnum::Binary(value) => Ok(Some(BinaryValueEnum::Slice(value.as_bytes_ref()))),
      _ => Err(LuceneError::illegal_state(
        "KeywordField contains neither a string nor a binary value",
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
    if self.has_stored_value {
      self.parent_field.stored_value()
    } else {
      Ok(None)
    }
  }

  fn invertable_type(&self) -> &InvertableType {
    &InvertableType::BINARY
  }
}

impl Display for KeywordField {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    self.parent_field.fmt(f)
  }
}
