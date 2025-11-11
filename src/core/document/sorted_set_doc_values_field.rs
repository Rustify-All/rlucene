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
use crate::core::document::field::{Field, FieldDataEnum};
use crate::core::document::field_type::FieldType;
use crate::core::document::invertable_field::InvertableType;
use crate::core::index::BytesRef;
use crate::core::index::doc_values_skip_index_type::DocValuesSkipIndexType;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::indexable_field::IndexableField;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::number::Number;
use once_cell::sync::Lazy;
use std::borrow::Cow;
use std::fmt::{Display, Formatter};
/// Field that stores a set of per-document [`BytesRef`] values, indexed for
/// faceting, grouping, and joining
/// If you also need to store the value, you should add a separate [`StoredField`](crate::core::document::stored_field::StoredField) instance.
///
/// Each value can be at most **32766 bytes** long.
static TYPE: Lazy<FieldType> = Lazy::new(|| {
    let mut ft = FieldType::new();
    ft.set_doc_values_type(DocValuesType::SortedSet)
        .expect("set_doc_values_type should never fail in this context");
    ft.freeze();
    ft
});

static INDEXED_TYPE: Lazy<FieldType> = Lazy::new(|| {
    let mut ft =
        FieldType::from_ref(&*TYPE).expect("FieldType::from_ref should never fail in this context");
    ft.set_doc_values_skip_index_type(DocValuesSkipIndexType::Range)
        .expect("set_doc_values_skip_index_type should never fail in this context");
    ft.freeze();
    ft
});

pub struct SortedSetDocValuesField {
    parent_field: Field,
}

impl SortedSetDocValuesField {
    /// Creates a new sorted-set DocValues field.
    pub fn new<T>(name: T, bytes: BytesRef<Vec<u8>>) -> Self
    where
        T: Into<String>,
    {
        Self::with_type(name, bytes, TYPE.clone())
    }

    /// Creates a new sorted-set DocValues field that also creates a skip index.
    pub fn indexed_field<T>(name: T, bytes: BytesRef<Vec<u8>>) -> Self
    where
        T: Into<String>,
    {
        Self::with_type(name, bytes, INDEXED_TYPE.clone())
    }

    pub fn with_type<T>(name: T, bytes: BytesRef<Vec<u8>>, field_type: FieldType) -> Self
    where
        T: Into<String>,
    {
        let parent_field = Field::new(name, field_type, bytes);
        Self { parent_field }
    }
}

impl Display for SortedSetDocValuesField {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}(name: {}, value: {:?})",
            std::any::type_name::<Self>(),
            self.parent_field.name(),
            self.binary_value().ok()
        )
    }
}

impl IndexableField for SortedSetDocValuesField {
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

    fn stored_value(&self) -> Option<&FieldDataEnum> {
        self.parent_field.stored_value()
    }

    fn take_stored_value(&mut self) -> Option<FieldDataEnum> {
        self.parent_field.take_stored_value()
    }

    fn invertable_type(&self) -> &InvertableType {
        self.parent_field.invertable_type()
    }

    fn init_token_stream<A>(&mut self, analyzer: &A) -> Result<()>
    where
        A: Analyzer,
    {
        self.parent_field.init_token_stream(analyzer)
    }
}
