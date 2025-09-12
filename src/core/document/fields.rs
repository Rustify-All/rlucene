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
use std::sync::Arc;

use crate::core::analysis::dummy::dummy_token_stream::DummyTokenStream;
use crate::core::analysis::reader::ReaderEnum;
use crate::core::analysis::token_stream::TokenStream;
use crate::core::document::field::{Field, FieldDataEnum};
use crate::core::document::field_type::FieldType;
use crate::core::document::invertable_field::InvertableType;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::stored_field::StoredField;
use crate::core::document::stored_value::StoredValue;
use crate::core::document::string_field::StringField;
use crate::core::document::text_field::TextField;
use crate::core::index::BytesRef;
use crate::core::index::indexable_field::IndexableField;
use crate::core::index::indexing_chain::ReservedField;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::number::Number;

pub enum Fields {
    Field(Field),
    Text(TextField),
    String(StringField),
    Stored(StoredField),
    NumericDocValues(NumericDocValuesField),
    Reverse(ReservedField<NumericDocValuesField>),
}
impl Display for Fields {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Fields::Field(f1) => f1.fmt(f),
            Fields::Text(f1) => f1.fmt(f),
            Fields::String(f1) => f1.fmt(f),
            Fields::Stored(f1) => f1.fmt(f),
            Fields::NumericDocValues(f1) => f1.fmt(f),
            Fields::Reverse(f1) => f1.fmt(f),
        }
    }
}

impl IndexableField for Fields {
    fn name(&self) -> &str {
        match self {
            Fields::Field(f) => f.name(),
            Fields::Text(f) => f.name(),
            Fields::String(f) => f.name(),
            Fields::Stored(f) => f.name(),
            Fields::NumericDocValues(f) => f.name(),
            Fields::Reverse(f) => f.name(),
        }
    }

    type FieldType = FieldType;

    fn field_type(&self) -> &Self::FieldType {
        match self {
            Fields::Field(f) => f.field_type(),
            Fields::Text(f) => f.field_type(),
            Fields::String(f) => f.field_type(),
            Fields::Stored(f) => f.field_type(),
            Fields::NumericDocValues(f) => f.field_type(),
            Fields::Reverse(f) => f.field_type(),
        }
    }

    type TokenStream = <Field as IndexableField>::TokenStream;
    fn token_stream<'a, TS>(
        &'a mut self,
        token_stream: &'a TS,
    ) -> Result<Option<&mut Self::TokenStream>>
    where
        TS: TokenStream,
    {
        match self {
            Fields::Field(f) => f.token_stream(token_stream),
            Fields::Text(f) => f.token_stream(token_stream),
            Fields::String(f) => f.token_stream(token_stream),
            Fields::Stored(f) => f.token_stream(token_stream),
            Fields::NumericDocValues(f) => f.token_stream(token_stream),
            Fields::Reverse(f) => f.token_stream(token_stream),
        }
    }

    fn binary_value(&self) -> Result<Option<&BytesRef<Vec<u8>>>> {
        match self {
            Fields::Field(f) => f.binary_value(),
            Fields::Text(f) => f.binary_value(),
            Fields::String(f) => f.binary_value(),
            Fields::Stored(f) => f.binary_value(),
            Fields::NumericDocValues(f) => f.binary_value(),
            Fields::Reverse(f) => f.binary_value(),
        }
    }

    fn string_value(&self) -> Result<Option<Cow<'_, String>>> {
        match self {
            Fields::Field(f) => f.string_value(),
            Fields::Text(f) => f.string_value(),
            Fields::String(f) => f.string_value(),
            Fields::Stored(f) => f.string_value(),
            Fields::NumericDocValues(f) => f.string_value(),
            Fields::Reverse(f) => f.string_value(),
        }
    }

    fn get_char_sequence_value(&self) -> Result<Option<Cow<'_, String>>> {
        match self {
            Fields::Field(f) => f.get_char_sequence_value(),
            Fields::Text(f) => f.get_char_sequence_value(),
            Fields::String(f) => f.get_char_sequence_value(),
            Fields::Stored(f) => f.get_char_sequence_value(),
            Fields::NumericDocValues(f) => f.get_char_sequence_value(),
            Fields::Reverse(f) => f.get_char_sequence_value(),
        }
    }

    fn reader_value(&self) -> Result<Option<ReaderEnum>> {
        match self {
            Fields::Field(f) => f.reader_value(),
            Fields::Text(f) => f.reader_value(),
            Fields::String(f) => f.reader_value(),
            Fields::Stored(f) => f.reader_value(),
            Fields::NumericDocValues(f) => f.reader_value(),
            Fields::Reverse(f) => f.reader_value(),
        }
    }

    fn numeric_value(&self) -> Result<Option<Number>> {
        match self {
            Fields::Field(f) => f.numeric_value(),
            Fields::Text(f) => f.numeric_value(),
            Fields::String(f) => f.numeric_value(),
            Fields::Stored(f) => f.numeric_value(),
            Fields::NumericDocValues(f) => f.numeric_value(),
            Fields::Reverse(f) => f.numeric_value(),
        }
    }

    fn stored_value(&self) -> Option<&FieldDataEnum> {
        match self {
            Fields::Field(f) => f.stored_value(),
            Fields::Text(f) => f.stored_value(),
            Fields::String(f) => f.stored_value(),
            Fields::Stored(f) => f.stored_value(),
            Fields::NumericDocValues(f) => f.stored_value(),
            Fields::Reverse(f) => f.stored_value(),
        }
    }

    fn take_stored_value(&self) -> Result<Option<StoredValue>> {
        match self {
            Fields::Field(f) => f.take_stored_value(),
            Fields::Text(f) => f.take_stored_value(),
            Fields::String(f) => f.take_stored_value(),
            Fields::Stored(f) => f.take_stored_value(),
            Fields::NumericDocValues(f) => f.take_stored_value(),
            Fields::Reverse(f) => f.take_stored_value(),
        }
    }

    fn invertable_type(&self) -> &InvertableType {
        match self {
            Fields::Field(f) => f.invertable_type(),
            Fields::Text(f) => f.invertable_type(),
            Fields::String(f) => f.invertable_type(),
            Fields::Stored(f) => f.invertable_type(),
            Fields::NumericDocValues(f) => f.invertable_type(),
            Fields::Reverse(f) => f.invertable_type(),
        }
    }
}

impl From<Field> for Fields {
    fn from(f: Field) -> Self {
        Fields::Field(f)
    }
}

impl From<TextField> for Fields {
    fn from(t: TextField) -> Self {
        Fields::Text(t)
    }
}

impl From<StringField> for Fields {
    fn from(s: StringField) -> Self {
        Fields::String(s)
    }
}

impl From<StoredField> for Fields {
    fn from(s: StoredField) -> Self {
        Fields::Stored(s)
    }
}

#[derive(Debug, Clone)]
pub enum TokenStreamEnum {
    Dummy(Arc<DummyTokenStream>),
}
