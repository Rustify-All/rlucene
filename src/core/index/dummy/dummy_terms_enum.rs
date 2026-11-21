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
use crate::core::codecs::block_term_state::TermStateEnum;
use crate::core::index::BytesRef;
use crate::core::index::dummy::dummy_impacts_enum::DummyImpactsEnum;
use crate::core::index::dummy::dummy_postings_enum::DummyPostingsEnum;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::util::access::ByteSource;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::dummy::dummy_attribute_source::DummyAttributeSource;
use crate::core::util::error::lucene_error::Result;

pub struct DummyTermsEnum;
impl BytesRefIterator for DummyTermsEnum {
  type Value<'a>
    = &'a BytesRef<Vec<u8>>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    dummy_unreachable!()
  }
}

impl TermsEnum for DummyTermsEnum {
  type AttributeSource<'a>
    = &'a DummyAttributeSource
  where
    Self: 'a;
  type AttributeSourceMut<'a>
    = &'a mut DummyAttributeSource
  where
    Self: 'a;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    dummy_unreachable!()
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    dummy_unreachable!()
  }

  fn seek_exact<BS: ByteSource>(&mut self, _term: &BytesRef<BS>) -> Result<bool> {
    dummy_unreachable!()
  }

  type PreparedSeek<BS: ByteSource> = crate::core::index::terms_enum::SeekExact;

  fn prepare_seek_exact<BS: ByteSource>(
    &mut self,
    _text: &BytesRef<BS>,
  ) -> Result<Option<Self::PreparedSeek<BS>>> {
    dummy_unreachable!()
  }

  fn seek_ceil<BS: ByteSource>(&mut self, _term: &BytesRef<BS>) -> Result<SeekStatus> {
    dummy_unreachable!()
  }

  fn seek_exact_with_ord(&mut self, _ord: i64) -> Result<()> {
    dummy_unreachable!()
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    _term: &BytesRef<BS>,
    _state: &TermStateEnum,
  ) -> Result<()> {
    dummy_unreachable!()
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    dummy_unreachable!()
  }

  fn ord(&self) -> Result<i64> {
    dummy_unreachable!()
  }

  fn doc_freq(&mut self) -> Result<i32> {
    dummy_unreachable!()
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    dummy_unreachable!()
  }

  type PostingsEnum = DummyPostingsEnum;

  fn postings_with_flags(
    &mut self,
    _reuse: Option<Self::PostingsEnum>,
    _flags: i32,
  ) -> Result<Self::PostingsEnum> {
    dummy_unreachable!()
  }

  type ImpactsEnum = DummyImpactsEnum;

  fn impacts(&mut self, _flags: i32) -> Result<Self::ImpactsEnum> {
    dummy_unreachable!()
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    dummy_unreachable!()
  }
}
