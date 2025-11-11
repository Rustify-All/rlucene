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
use crate::core::analysis::token_stream::{Either2TokenStream, InnerTokenStreams};
use crate::core::document::field::{Field, FieldBase, FieldDataEnum, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::invertable_field::InvertableType;
use crate::core::index::BytesRef;
use crate::core::index::indexable_field::IndexableField;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::number::Number;

pub mod string {
    use crate::core::document::field_type::FieldType;
    use crate::core::index::index_options::IndexOptions;
    use once_cell::sync::Lazy;

    /// Indexed, not tokenized, omits norms, indexes DOCS_ONLY, not stored.
    pub(crate) static TYPE_NOT_STORED: Lazy<FieldType> = Lazy::new(|| {
        let mut ft = FieldType::new();
        ft.set_omit_norms(true)
            .expect("set_omit_norms(true) should never fail in this context");
        ft.set_index_options(IndexOptions::Docs)
            .expect("set_index_options should never fail in this context");
        ft.set_tokenized(false)
            .expect("set_tokenized(false) should never fail in this context");
        ft.freeze();
        ft
    });
    /// Indexed, not tokenized, omits norms, indexes DOCS_ONLY, stored.
    pub(crate) static TYPE_STORED: Lazy<FieldType> = Lazy::new(|| {
        let mut ft = FieldType::new();
        ft.set_omit_norms(true)
            .expect("set_omit_norms(true) should never fail in this context");
        ft.set_index_options(IndexOptions::Docs)
            .expect("set_index_options should never fail in this context");
        ft.set_stored(true)
            .expect("set_stored(true) should never fail in this context");
        ft.set_tokenized(false)
            .expect("set_tokenized(false) should never fail in this context");
        ft.freeze();
        ft
    });
}
/// A field that is indexed but not tokenized: the entire string value is
/// indexed as a single token. For example, this might be used for a `country`
/// field or an `id` field. If sorting on this field is required, add a
/// [`SortedDocValuesField`](crate::core::document::sorted_doc_values_field::SortedDocValuesField)
/// separately to the document.
pub struct StringField {
    parent_field: Field,
    binary_value: Option<BytesRef<Vec<u8>>>,
}

impl StringField {
    /// Creates a new textual `StringField`, indexing the provided string value
    /// as a single token.
    ///
    /// # Parameters
    /// - `name`: Field name.
    /// - `value`: String value.
    /// - `stored`: `Store::Yes` if the content should also be stored.
    pub fn with_string<T1, T2>(name: T1, value: T2, store: Store) -> Result<Self>
    where
        T1: Into<String>,
        T2: Into<String>,
    {
        let store = store.into();
        let field_type = if store {
            string::TYPE_STORED.clone()
        } else {
            string::TYPE_NOT_STORED.clone()
        };
        let value_str: String = value.into();
        let binary_value = Some(BytesRef::from_string(&value_str));
        let parent_field = Field::with_string(name, value_str, field_type)?;

        Ok(Self {
            parent_field,
            binary_value,
        })
    }
    /// Creates a new binary `StringField`, indexing the provided binary
    /// (`BytesRef`) value as a single token.
    ///
    /// # Parameters
    /// - `name`: Field name.
    /// - `value`: `BytesRef` value. The provided value is **not cloned**, so it
    ///   must not be modified until the document(s) holding it have been
    ///   indexed.
    /// - `stored`: `Store::Yes` if the content should also be stored.
    pub fn with_bytes_ref<T>(name: T, value: BytesRef<Vec<u8>>, store: Store) -> Result<Self>
    where
        T: Into<String>,
    {
        let store = store.into();
        let field_type = if store {
            string::TYPE_STORED.clone()
        } else {
            string::TYPE_NOT_STORED.clone()
        };
        let parent_field = Field::with_bytes_ref(name, value, field_type)?;
        Ok(Self {
            parent_field,
            binary_value: None,
        })
    }
}

impl FieldBase for StringField {
    fn set_bytes_value(&mut self, value: BytesRef<Vec<u8>>) -> Result<()> {
        debug_assert!(self.binary_value.is_none());
        self.parent_field.set_bytes_value(value)?;
        Ok(())
    }

    fn set_string_value<T>(&mut self, value: T) -> Result<()>
    where
        T: Into<String>,
    {
        debug_assert!(self.binary_value.is_some());
        let v = value.into();
        self.binary_value = Some(BytesRef::from_string(&v));
        self.parent_field.set_string_value(v)?;
        Ok(())
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

    type FieldType = FieldType;

    fn field_type(&self) -> &Self::FieldType {
        self.parent_field.field_type()
    }

    type TokenStream = <Field as IndexableField>::TokenStream;

    fn token_stream<'a>(
        &'a mut self,
        token_stream: Option<&'a mut InnerTokenStreams>,
    ) -> Result<Option<Either2TokenStream<&'a mut InnerTokenStreams, &'a mut Self::TokenStream>>>
    {
        self.parent_field.token_stream(token_stream)
    }
    fn binary_value(&self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
        match self.binary_value {
            Some(ref b) => Ok(Some(Cow::Borrowed(b))),
            None => self.parent_field.binary_value(),
        }
    }

    fn take_binary_value(&mut self) -> Result<Option<BytesRef<Vec<u8>>>> {
        match &mut self.binary_value {
            Some(b) => Ok(Some(std::mem::take(b))),
            None => self.parent_field.take_binary_value(),
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

    fn stored_value(&self) -> Option<&FieldDataEnum> {
        self.parent_field.stored_value()
    }

    fn take_stored_value(&mut self) -> Option<FieldDataEnum> {
        self.parent_field.take_stored_value()
    }

    fn invertable_type(&self) -> &InvertableType {
        &InvertableType::BINARY
    }

    fn init_token_stream<A>(&mut self, analyzer: &A) -> Result<()>
    where
        A: Analyzer,
    {
        self.parent_field.init_token_stream(analyzer)
    }
}
