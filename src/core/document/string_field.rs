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
use std::fmt::{Display, Formatter};
use std::rc::Rc;

use once_cell::sync::Lazy;

use crate::core::analysis::analyzer::Analyzer;
use crate::core::analysis::reader::ReaderEnum;
use crate::core::document::field::{Field, FieldBase, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::invertable_field::InvertableType;
use crate::core::document::stored_value::StoredValue;
use crate::core::index::BytesRef;
use crate::core::index::index_options::IndexOptions;
use crate::core::index::indexable_field::IndexableField;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::number::Number;

/// Indexed, not tokenized, omits norms, indexes DOCS_ONLY, not stored.
static TYPE_NOT_STORED: Lazy<FieldType> = Lazy::new(|| {
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
static TYPE_STORED: Lazy<FieldType> = Lazy::new(|| {
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
/// A field that is indexed but not tokenized: the entire string value is
/// indexed as a single token. For example, this might be used for a `country`
/// field or an `id` field. If sorting on this field is required, add a
/// [`SortedDocValuesField`](crate::core::document::sorted_doc_values_field::SortedDocValuesField)
/// separately to the document.
pub struct StringField {
    parent_field: Field,
    binary_value: Rc<BytesRef<Vec<u8>>>,
    stored_value: Option<StoredValue>,
}

impl StringField {
    /// Creates a new textual `StringField`, indexing the provided string value
    /// as a single token.
    ///
    /// # Parameters
    /// - `name`: Field name.
    /// - `value`: String value.
    /// - `stored`: `Store::Yes` if the content should also be stored.
    pub fn with_string(name: &str, value: &str, store: Store) -> Result<Self> {
        let store = store.into();
        let field_type = if store {
            TYPE_STORED.clone()
        } else {
            TYPE_NOT_STORED.clone()
        };
        let value_str = Rc::new(value.to_string());
        let parent_field = Field::with_string(name, value_str.clone(), field_type)?;
        let binary_value = Rc::new(BytesRef::from_string(value));
        let stored_value = if store {
            None
        } else {
            Option::from(StoredValue::new_string(value_str))
        };
        Ok(Self {
            parent_field,
            binary_value,
            stored_value,
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
    pub fn with_bytes_ref(name: &str, value: Rc<BytesRef<Vec<u8>>>, store: Store) -> Result<Self> {
        let store = store.into();
        let field_type = if store {
            TYPE_STORED.clone()
        } else {
            TYPE_NOT_STORED.clone()
        };
        let parent_field = Field::with_bytes_ref(name, value.clone(), field_type)?;
        let stored_value = if store {
            None
        } else {
            Option::from(StoredValue::new_binary(value.clone()))
        };
        Ok(Self {
            parent_field,
            binary_value: value,
            stored_value,
        })
    }
}

impl FieldBase for StringField {
    fn set_bytes_value(&mut self, value: Rc<BytesRef<Vec<u8>>>) -> Result<()> {
        self.parent_field.set_bytes_value(value.clone())?;
        if let Some(ref mut stored_value) = self.stored_value {
            stored_value.set_binary_value(value.clone())?;
        }
        self.binary_value = value;
        Ok(())
    }

    fn set_string_value(&mut self, value: &str) -> Result<()> {
        let value_str = Rc::new(value.to_string());
        self.parent_field.set_string_value(value_str.clone())?;
        if let Some(ref mut stored_value) = self.stored_value {
            stored_value.set_string_value(value_str.clone())?;
        }
        // TODO: could we avoid clone here?
        self.binary_value = Rc::new(BytesRef::from_string(value));
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

    fn token_stream<'a, A>(&'a mut self, analyzer: &A) -> Result<Option<&'a mut Self::TokenStream>>
    where
        A: Analyzer,
    {
        self.parent_field.token_stream(analyzer)
    }
    fn binary_value(&self) -> Result<Option<Rc<BytesRef<Vec<u8>>>>> {
        Ok(Some(self.binary_value.clone()))
    }

    fn string_value(&self) -> Result<Option<Rc<String>>> {
        self.parent_field.string_value()
    }

    fn reader_value(&self) -> Result<Option<ReaderEnum>> {
        self.parent_field.reader_value()
    }

    fn numeric_value(&self) -> Result<Option<Number>> {
        self.parent_field.numeric_value()
    }

    fn stored_value(&self) -> Result<Option<StoredValue>> {
        Ok(self.stored_value.clone())
    }

    fn invertable_type(&self) -> &InvertableType {
        &InvertableType::BINARY
    }
}
