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
use crate::core::document::field::FieldDataEnum;
use crate::core::document::invertable_field::InvertableType;
use crate::core::index::BinaryValueEnum;
use crate::core::index::BytesRef;
use crate::core::index::dummy::dummy_indexable_field_type::DummyIndexableFieldType;
use crate::core::index::indexable_field::{
  IndexableField, IndexingTokenStream, ReusedIndexingTokenStream,
};
use crate::core::util::error::lucene_error::Result;
use crate::core::util::number::Number;
use std::borrow::Cow;
use std::fmt::{Display, Formatter};

pub struct DummyIndexableField;

impl Display for DummyIndexableField {
  fn fmt(&self, _f: &mut Formatter<'_>) -> std::fmt::Result {
    dummy_unreachable!()
  }
}

impl IndexableField for DummyIndexableField {
  fn name(&self) -> &str {
    dummy_unreachable!()
  }

  type FieldType<'a>
    = &'a DummyIndexableFieldType
  where
    Self: 'a;

  fn field_type(&self) -> Self::FieldType<'_> {
    dummy_unreachable!()
  }
  fn token_stream<'a, A>(
    &'a mut self,
    _analyzer: &'a A,
    _reuse_token_stream: &'a mut Option<ReusedIndexingTokenStream>,
  ) -> Result<IndexingTokenStream<'a>>
  where
    A: Analyzer,
  {
    dummy_unreachable!()
  }

  fn binary_value(&self) -> Result<Option<BinaryValueEnum<'_>>> {
    dummy_unreachable!()
  }

  fn take_binary_value(&mut self) -> Result<Option<BytesRef<Vec<u8>>>> {
    dummy_unreachable!()
  }

  fn string_value(&self) -> Result<Option<Cow<'_, str>>> {
    dummy_unreachable!()
  }

  fn take_string_value(&mut self) -> Result<Option<String>> {
    dummy_unreachable!()
  }

  fn get_char_sequence_value(&self) -> Result<Option<Cow<'_, str>>> {
    dummy_unreachable!()
  }

  fn take_reader_value(&mut self) -> Result<Option<ReaderEnum>> {
    dummy_unreachable!()
  }

  fn numeric_value(&self) -> Result<Option<Number>> {
    dummy_unreachable!()
  }

  type StoredValue<'a>
    = &'a FieldDataEnum
  where
    Self: 'a;

  fn stored_value(&self) -> Result<Option<Self::StoredValue<'_>>> {
    dummy_unreachable!()
  }

  fn invertable_type(&self) -> &InvertableType {
    dummy_unreachable!()
  }

  fn is_reserved(&self) -> bool {
    dummy_unreachable!()
  }
}
