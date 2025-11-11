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
use crate::core::analysis::dummy::dummy_token_stream::DummyTokenStream;
use crate::core::analysis::reader::ReaderEnum;
use crate::core::analysis::token_stream::{Either2TokenStream, InnerTokenStreams};
use crate::core::document::binary_doc_values_field::BinaryDocValuesField;
use crate::core::document::double_point::DoublePoint;
use crate::core::document::field::{Field, FieldDataEnum};
use crate::core::document::field_type::FieldType;
use crate::core::document::int_point::IntPoint;
use crate::core::document::invertable_field::InvertableType;
use crate::core::document::long_point::LongPoint;
use crate::core::document::numeric_doc_values_field::NumericDocValuesField;
use crate::core::document::sorted_doc_values_field::SortedDocValuesField;
use crate::core::document::sorted_numeric_doc_values_field::SortedNumericDocValuesField;
use crate::core::document::sorted_set_doc_values_field::SortedSetDocValuesField;
use crate::core::document::stored_field::StoredField;
use crate::core::document::string_field::StringField;
use crate::core::document::text_field::TextField;
use crate::core::index::BytesRef;
use crate::core::index::indexable_field::IndexableField;
use crate::core::index::indexing_chain::ReservedField;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::number::Number;
use std::borrow::Cow;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub enum Fields {
    Field(Field),
    Text(TextField),
    String(StringField),
    Stored(StoredField),
    NumericDocValues(NumericDocValuesField),
    Reverse(ReservedField<NumericDocValuesField>),
    LongPoint(LongPoint),
    IntPoint(IntPoint),
    DoublePoint(DoublePoint),
    BinaryDocValues(BinaryDocValuesField),
    SortedDocValues(SortedDocValuesField),
    SortedSetDocValues(SortedSetDocValuesField),
    SortedNumericDocValues(SortedNumericDocValuesField),
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
impl From<NumericDocValuesField> for Fields {
    fn from(n: NumericDocValuesField) -> Self {
        Fields::NumericDocValues(n)
    }
}
impl From<ReservedField<NumericDocValuesField>> for Fields {
    fn from(r: ReservedField<NumericDocValuesField>) -> Self {
        Fields::Reverse(r)
    }
}
impl From<LongPoint> for Fields {
    fn from(l: LongPoint) -> Self {
        Fields::LongPoint(l)
    }
}

impl From<IntPoint> for Fields {
    fn from(l: IntPoint) -> Self {
        Fields::IntPoint(l)
    }
}
impl From<DoublePoint> for Fields {
    fn from(d: DoublePoint) -> Self {
        Fields::DoublePoint(d)
    }
}
impl From<BinaryDocValuesField> for Fields {
    fn from(b: BinaryDocValuesField) -> Self {
        Fields::BinaryDocValues(b)
    }
}
impl From<SortedDocValuesField> for Fields {
    fn from(s: SortedDocValuesField) -> Self {
        Fields::SortedDocValues(s)
    }
}
impl From<SortedSetDocValuesField> for Fields {
    fn from(s: SortedSetDocValuesField) -> Self {
        Fields::SortedSetDocValues(s)
    }
}
impl From<SortedNumericDocValuesField> for Fields {
    fn from(s: SortedNumericDocValuesField) -> Self {
        Fields::SortedNumericDocValues(s)
    }
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
            Fields::LongPoint(f1) => f1.fmt(f),
            Fields::IntPoint(f1) => f1.fmt(f),
            Fields::DoublePoint(f1) => f1.fmt(f),
            Fields::BinaryDocValues(f1) => f1.fmt(f),
            Fields::SortedDocValues(f1) => f1.fmt(f),
            Fields::SortedSetDocValues(f1) => f1.fmt(f),
            Fields::SortedNumericDocValues(f1) => f1.fmt(f),
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
            Fields::LongPoint(f) => f.name(),
            Fields::IntPoint(f) => f.name(),
            Fields::DoublePoint(f) => f.name(),
            Fields::BinaryDocValues(f) => f.name(),
            Fields::SortedDocValues(f) => f.name(),
            Fields::SortedSetDocValues(f) => f.name(),
            Fields::SortedNumericDocValues(f) => f.name(),
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
            Fields::LongPoint(f) => f.field_type(),
            Fields::IntPoint(f) => f.field_type(),
            Fields::DoublePoint(f) => f.field_type(),
            Fields::BinaryDocValues(f) => f.field_type(),
            Fields::SortedDocValues(f) => f.field_type(),
            Fields::SortedSetDocValues(f) => f.field_type(),
            Fields::SortedNumericDocValues(f) => f.field_type(),
        }
    }

    type TokenStream = <Field as IndexableField>::TokenStream;
    fn token_stream<'a>(
        &'a mut self,
        token_stream: Option<&'a mut InnerTokenStreams>,
    ) -> Result<Option<Either2TokenStream<&'a mut InnerTokenStreams, &'a mut Self::TokenStream>>>
    {
        match self {
            Fields::Field(f) => f.token_stream(token_stream),
            Fields::Text(f) => f.token_stream(token_stream),
            Fields::String(f) => f.token_stream(token_stream),
            Fields::Stored(f) => f.token_stream(token_stream),
            Fields::NumericDocValues(f) => f.token_stream(token_stream),
            Fields::Reverse(f) => f.token_stream(token_stream),
            Fields::LongPoint(f) => f.token_stream(token_stream),
            Fields::IntPoint(f) => f.token_stream(token_stream),
            Fields::DoublePoint(f) => f.token_stream(token_stream),
            Fields::BinaryDocValues(f) => f.token_stream(token_stream),
            Fields::SortedDocValues(f) => f.token_stream(token_stream),
            Fields::SortedSetDocValues(f) => f.token_stream(token_stream),
            Fields::SortedNumericDocValues(f) => f.token_stream(token_stream),
        }
    }

    fn binary_value(&self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
        match self {
            Fields::Field(f) => f.binary_value(),
            Fields::Text(f) => f.binary_value(),
            Fields::String(f) => f.binary_value(),
            Fields::Stored(f) => f.binary_value(),
            Fields::NumericDocValues(f) => f.binary_value(),
            Fields::Reverse(f) => f.binary_value(),
            Fields::LongPoint(f) => f.binary_value(),
            Fields::IntPoint(f) => f.binary_value(),
            Fields::DoublePoint(f) => f.binary_value(),
            Fields::BinaryDocValues(f) => f.binary_value(),
            Fields::SortedDocValues(f) => f.binary_value(),
            Fields::SortedSetDocValues(f) => f.binary_value(),
            Fields::SortedNumericDocValues(f) => f.binary_value(),
        }
    }

    fn take_binary_value(&mut self) -> Result<Option<BytesRef<Vec<u8>>>> {
        match self {
            Fields::Field(f) => f.take_binary_value(),
            Fields::Text(f) => f.take_binary_value(),
            Fields::String(f) => f.take_binary_value(),
            Fields::Stored(f) => f.take_binary_value(),
            Fields::NumericDocValues(f) => f.take_binary_value(),
            Fields::Reverse(f) => f.take_binary_value(),
            Fields::LongPoint(f) => f.take_binary_value(),
            Fields::IntPoint(f) => f.take_binary_value(),
            Fields::DoublePoint(f) => f.take_binary_value(),
            Fields::BinaryDocValues(f) => f.take_binary_value(),
            Fields::SortedDocValues(f) => f.take_binary_value(),
            Fields::SortedSetDocValues(f) => f.take_binary_value(),
            Fields::SortedNumericDocValues(f) => f.take_binary_value(),
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
            Fields::LongPoint(f) => f.string_value(),
            Fields::IntPoint(f) => f.string_value(),
            Fields::DoublePoint(f) => f.string_value(),
            Fields::BinaryDocValues(f) => f.string_value(),
            Fields::SortedDocValues(f) => f.string_value(),
            Fields::SortedSetDocValues(f) => f.string_value(),
            Fields::SortedNumericDocValues(f) => f.string_value(),
        }
    }

    fn take_string_value(&mut self) -> Result<Option<String>> {
        match self {
            Fields::Field(f) => f.take_string_value(),
            Fields::Text(f) => f.take_string_value(),
            Fields::String(f) => f.take_string_value(),
            Fields::Stored(f) => f.take_string_value(),
            Fields::NumericDocValues(f) => f.take_string_value(),
            Fields::Reverse(f) => f.take_string_value(),
            Fields::LongPoint(f) => f.take_string_value(),
            Fields::IntPoint(f) => f.take_string_value(),
            Fields::DoublePoint(f) => f.take_string_value(),
            Fields::BinaryDocValues(f) => f.take_string_value(),
            Fields::SortedDocValues(f) => f.take_string_value(),
            Fields::SortedSetDocValues(f) => f.take_string_value(),
            Fields::SortedNumericDocValues(f) => f.take_string_value(),
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
            Fields::LongPoint(f) => f.get_char_sequence_value(),
            Fields::IntPoint(f) => f.get_char_sequence_value(),
            Fields::DoublePoint(f) => f.get_char_sequence_value(),
            Fields::BinaryDocValues(f) => f.get_char_sequence_value(),
            Fields::SortedDocValues(f) => f.get_char_sequence_value(),
            Fields::SortedSetDocValues(f) => f.get_char_sequence_value(),
            Fields::SortedNumericDocValues(f) => f.get_char_sequence_value(),
        }
    }

    fn take_reader_value(&mut self) -> Result<Option<ReaderEnum>> {
        match self {
            Fields::Field(f) => f.take_reader_value(),
            Fields::Text(f) => f.take_reader_value(),
            Fields::String(f) => f.take_reader_value(),
            Fields::Stored(f) => f.take_reader_value(),
            Fields::NumericDocValues(f) => f.take_reader_value(),
            Fields::Reverse(f) => f.take_reader_value(),
            Fields::LongPoint(f) => f.take_reader_value(),
            Fields::IntPoint(f) => f.take_reader_value(),
            Fields::DoublePoint(f) => f.take_reader_value(),
            Fields::BinaryDocValues(f) => f.take_reader_value(),
            Fields::SortedDocValues(f) => f.take_reader_value(),
            Fields::SortedSetDocValues(f) => f.take_reader_value(),
            Fields::SortedNumericDocValues(f) => f.take_reader_value(),
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
            Fields::LongPoint(f) => f.numeric_value(),
            Fields::IntPoint(f) => f.numeric_value(),
            Fields::DoublePoint(f) => f.numeric_value(),
            Fields::BinaryDocValues(f) => f.numeric_value(),
            Fields::SortedDocValues(f) => f.numeric_value(),
            Fields::SortedSetDocValues(f) => f.numeric_value(),
            Fields::SortedNumericDocValues(f) => f.numeric_value(),
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
            Fields::LongPoint(f) => f.stored_value(),
            Fields::IntPoint(f) => f.stored_value(),
            Fields::DoublePoint(f) => f.stored_value(),
            Fields::BinaryDocValues(f) => f.stored_value(),
            Fields::SortedDocValues(f) => f.stored_value(),
            Fields::SortedSetDocValues(f) => f.stored_value(),
            Fields::SortedNumericDocValues(f) => f.stored_value(),
        }
    }

    fn take_stored_value(&mut self) -> Option<FieldDataEnum> {
        match self {
            Fields::Field(f) => f.take_stored_value(),
            Fields::Text(f) => f.take_stored_value(),
            Fields::String(f) => f.take_stored_value(),
            Fields::Stored(f) => f.take_stored_value(),
            Fields::NumericDocValues(f) => f.take_stored_value(),
            Fields::Reverse(f) => f.take_stored_value(),
            Fields::LongPoint(f) => f.take_stored_value(),
            Fields::IntPoint(f) => f.take_stored_value(),
            Fields::DoublePoint(f) => f.take_stored_value(),
            Fields::BinaryDocValues(f) => f.take_stored_value(),
            Fields::SortedDocValues(f) => f.take_stored_value(),
            Fields::SortedSetDocValues(f) => f.take_stored_value(),
            Fields::SortedNumericDocValues(f) => f.take_stored_value(),
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
            Fields::LongPoint(f) => f.invertable_type(),
            Fields::IntPoint(f) => f.invertable_type(),
            Fields::DoublePoint(f) => f.invertable_type(),
            Fields::BinaryDocValues(f) => f.invertable_type(),
            Fields::SortedDocValues(f) => f.invertable_type(),
            Fields::SortedSetDocValues(f) => f.invertable_type(),
            Fields::SortedNumericDocValues(f) => f.invertable_type(),
        }
    }

    fn init_token_stream<A>(&mut self, analyzer: &A) -> Result<()>
    where
        A: Analyzer,
    {
        match self {
            Fields::Field(f) => f.init_token_stream(analyzer),
            Fields::Text(f) => f.init_token_stream(analyzer),
            Fields::String(f) => f.init_token_stream(analyzer),
            Fields::Stored(f) => f.init_token_stream(analyzer),
            Fields::NumericDocValues(f) => f.init_token_stream(analyzer),
            Fields::Reverse(f) => f.init_token_stream(analyzer),
            Fields::IntPoint(f) => f.init_token_stream(analyzer),
            Fields::LongPoint(f) => f.init_token_stream(analyzer),
            Fields::DoublePoint(f) => f.init_token_stream(analyzer),
            Fields::BinaryDocValues(f) => f.init_token_stream(analyzer),
            Fields::SortedDocValues(f) => f.init_token_stream(analyzer),
            Fields::SortedSetDocValues(f) => f.init_token_stream(analyzer),
            Fields::SortedNumericDocValues(f) => f.init_token_stream(analyzer),
        }
    }
}

#[derive(Debug, Clone)]
pub enum TokenStreamEnum {
    Dummy(Arc<DummyTokenStream>),
}
