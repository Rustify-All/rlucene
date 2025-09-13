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
use crate::core::document::field::FieldDataEnum;
use crate::core::document::invertable_field::InvertableType;
use crate::core::index::BytesRef;
use crate::core::index::dummy::dummy_indexable_field_type::DummyIndexableFieldType;
use crate::core::index::indexable_field::IndexableField;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::number::Number;
use std::borrow::Cow;
use std::fmt::{Display, Formatter};

pub struct DummyIndexableField;

impl Display for DummyIndexableField {
    fn fmt(&self, _f: &mut Formatter<'_>) -> std::fmt::Result {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}

impl IndexableField for DummyIndexableField {
    fn name(&self) -> &str {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    type FieldType = DummyIndexableFieldType;

    fn field_type(&self) -> &Self::FieldType {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    type TokenStream = DummyTokenStream;

    fn token_stream<'a>(
        &'a mut self,
        _token_stream: Option<&'a mut InnerTokenStreams>,
    ) -> Result<Option<Either2TokenStream<&'a mut InnerTokenStreams, &'a mut Self::TokenStream>>>
    {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn binary_value(&self) -> Result<Option<&BytesRef<Vec<u8>>>> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn take_binary_value(&mut self) -> Result<Option<BytesRef<Vec<u8>>>> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn string_value(&self) -> Result<Option<Cow<'_, String>>> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn take_string_value(&mut self) -> Result<Option<String>> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn get_char_sequence_value(&self) -> Result<Option<Cow<'_, String>>> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn take_reader_value(&mut self) -> Result<Option<ReaderEnum>> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn numeric_value(&self) -> Result<Option<Number>> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn stored_value(&self) -> Option<&FieldDataEnum> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn take_stored_value(&mut self) -> Option<FieldDataEnum> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn invertable_type(&self) -> &InvertableType {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn is_reserved(&self) -> bool {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn init_token_stream<A>(&mut self, _analyzer: &A) -> Result<()>
    where
        A: Analyzer,
    {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}
