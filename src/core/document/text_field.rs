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
use crate::core::analysis::token_stream::{Either2TokenStream, InnerTokenStreams};
use crate::core::document::field::{Field, FieldBase, FieldDataEnum, Store};
use crate::core::document::field_type::FieldType;
use crate::core::document::fields::TokenStreamEnum;
use crate::core::document::invertable_field::InvertableType;
use crate::core::index::BytesRef;
use crate::core::index::indexable_field::IndexableField;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::number::Number;
use std::borrow::Cow;
use std::fmt;

pub mod text {

    use once_cell::sync::Lazy;

    use crate::core::document::field_type::FieldType;
    use crate::core::index::index_options::IndexOptions;

    /// Indexed, tokenized, not stored.
    pub(crate) static TYPE_NOT_STORED: Lazy<FieldType> = Lazy::new(|| {
        let mut ft = FieldType::new();
        ft.set_index_options(IndexOptions::DocsAndFreqsAndPositions)
            .expect("set_index_options should never fail in this context");
        ft.set_tokenized(true)
            .expect("set_tokenized(true) should never fail in this context");
        ft.freeze();
        ft
    });
    /// Indexed, tokenized, stored.
    pub(crate) static TYPE_STORED: Lazy<FieldType> = Lazy::new(|| {
        let mut ft = FieldType::new();
        ft.set_index_options(IndexOptions::DocsAndFreqsAndPositions)
            .expect("set_index_options should never fail in this context");
        ft.set_tokenized(true)
            .expect("set_tokenized(true) should never fail in this context");
        ft.set_stored(true)
            .expect("set_stored(true) should never fail in this context");
        ft.freeze();
        ft
    });
}

/// A field that is indexed and tokenized, without term vectors.
/// For example, this would be used on a `body` field that contains the bulk of
/// a document's text.
pub struct TextField {
    parent_field: Field,
}

impl TextField {
    /// Creates a new un-stored `TextField` with a `ReaderEnum` value.
    ///
    /// # Parameters
    /// - `name`: Field name.
    /// - `reader`: `ReaderEnum` value.
    pub fn with_reader<T>(name: T, reader: ReaderEnum) -> Result<Self>
    where
        T: Into<String>,
    {
        let parent_field = Field::with_reader(name, reader, text::TYPE_NOT_STORED.clone())?;
        Ok(Self { parent_field })
    }
    /// Creates a new `TextField` with a string value.
    ///
    /// # Parameters
    /// - `name`: Field name.
    /// - `value`: String value.
    /// - `store`: `Store::Yes` if the content should also be stored.
    pub fn with_string<T>(name: T, value: T, store: Store) -> Result<Self>
    where
        T: Into<String>,
    {
        let store = store.into();
        let field_type = if store {
            text::TYPE_STORED.clone()
        } else {
            text::TYPE_NOT_STORED.clone()
        };
        let parent_field = Field::with_string(name, value, field_type.clone())?;
        Ok(Self { parent_field })
    }
    /// Creates a new un-stored `TextField` with a `TokenStreamEnum` value.
    ///
    /// # Parameters
    /// - `name`: Field name.
    /// - `stream`: `TokenStream` value.
    pub fn with_token_stream<T>(name: T, stream: TokenStreamEnum) -> Result<Self>
    where
        T: Into<String>,
    {
        let parent_field = Field::with_token_stream(name, stream, text::TYPE_NOT_STORED.clone())?;
        Ok(Self { parent_field })
    }
}
impl FieldBase for TextField {
    fn set_string_value<T>(&mut self, value: T) -> Result<()>
    where
        T: Into<String>,
    {
        self.parent_field.set_string_value(value)?;
        Ok(())
    }
}
impl IndexableField for TextField {
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

    fn binary_value(&self) -> Result<Option<&BytesRef<Vec<u8>>>> {
        self.parent_field.binary_value()
    }

    fn take_binary_value(&mut self) -> Result<BytesRef<Vec<u8>>> {
        self.parent_field.take_binary_value()
    }

    fn string_value(&self) -> Result<Option<Cow<'_, String>>> {
        self.parent_field.string_value()
    }

    fn take_string_value(&mut self) -> Result<String> {
        self.parent_field.take_string_value()
    }

    fn get_char_sequence_value(&self) -> Result<Option<Cow<'_, String>>> {
        self.parent_field.get_char_sequence_value()
    }

    fn reader_value(&self) -> Result<Option<ReaderEnum>> {
        self.parent_field.reader_value()
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
        todo!()
    }

    fn init_token_stream<A>(&mut self, analyzer: &A) -> Result<()>
    where
        A: Analyzer,
    {
        self.parent_field.init_token_stream(analyzer)
    }
}

impl fmt::Display for TextField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}(name: {})",
            std::any::type_name::<Self>(),
            self.parent_field.name()
        )
    }
}
