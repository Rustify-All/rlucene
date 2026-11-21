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
use crate::core::index::BytesRefValue;
use crate::core::index::dummy::dummy_impacts_enum::DummyImpactsEnum;
use crate::core::index::dummy::dummy_postings_enum::DummyPostingsEnum;
use crate::core::index::ord_term_state::OrdTermState;
use crate::core::index::sorted_set_doc_values::SortedSetDocValues;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::util::access::ByteSource;
use crate::core::util::attribute_source::EmptyAttributeSource;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::{LuceneError, Result};

/// Implements a [`TermsEnum `]wrapping a provided [`SortedSetDocValues`].
pub struct SortedSetDocValuesTermsEnum<S> {
  values: S,
  current_ord: i64,
  scratch: BytesRefBuilder<Vec<u8>>,
  attributes: EmptyAttributeSource,
}
impl<S> SortedSetDocValuesTermsEnum<S> {
  pub fn new(values: S) -> SortedSetDocValuesTermsEnum<S> {
    SortedSetDocValuesTermsEnum {
      values,
      current_ord: -1,
      scratch: BytesRefBuilder::new(),
      attributes: EmptyAttributeSource,
    }
  }
}

impl<S> BytesRefIterator for SortedSetDocValuesTermsEnum<S>
where
  S: SortedSetDocValues,
{
  type Value<'a>
    = &'a BytesRef<Vec<u8>>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    self.current_ord += 1;
    if self.current_ord >= self.values.get_value_count()? {
      return Ok(None);
    }
    let term = self.values.lookup_ord(self.current_ord)?;
    let term = term.as_bytes_ref();
    self
      .scratch
      .copy_bytes_from_vec(term.bytes, term.offset, term.length)?;
    Ok(Some(self.scratch.get_bytes_ref()))
  }
}

impl<S> TermsEnum for SortedSetDocValuesTermsEnum<S>
where
  S: SortedSetDocValues,
{
  type AttributeSource<'a>
    = &'a EmptyAttributeSource
  where
    Self: 'a;
  type AttributeSourceMut<'a>
    = &'a mut EmptyAttributeSource
  where
    Self: 'a;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    Ok(&self.attributes)
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    Ok(&mut self.attributes)
  }

  fn seek_exact<BS: ByteSource>(&mut self, text: &BytesRef<BS>) -> Result<bool> {
    let ord = self.values.lookup_term(text)?;
    if ord >= 0 {
      self.current_ord = ord;
      self.scratch.copy_bytes_from_ref(text)?;
      Ok(true)
    } else {
      Ok(false)
    }
  }

  type PreparedSeek<BS: ByteSource> = crate::core::index::terms_enum::SeekExact;

  fn prepare_seek_exact<BS: ByteSource>(
    &mut self,
    _text: &BytesRef<BS>,
  ) -> Result<Option<Self::PreparedSeek<BS>>> {
    Ok(Some(crate::core::index::terms_enum::SeekExact))
  }

  fn seek_ceil<BS: ByteSource>(&mut self, text: &BytesRef<BS>) -> Result<SeekStatus> {
    let ord = self.values.lookup_term(text)?;
    if ord >= 0 {
      self.current_ord = ord;
      self.scratch.copy_bytes_from_ref(text)?;
      Ok(SeekStatus::Found)
    } else {
      // not found
      self.current_ord = -ord - 1;
      if self.current_ord == self.values.get_value_count()? {
        Ok(SeekStatus::End)
      } else {
        // perform lookup of next larger term
        let next_term = self.values.lookup_ord(self.current_ord)?;
        let next_term = next_term.as_bytes_ref();
        self
          .scratch
          .copy_bytes_from_vec(next_term.bytes, next_term.offset, next_term.length)?;
        Ok(SeekStatus::NotFound)
      }
    }
  }

  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    debug_assert!(
      ord >= 0 && ord < self.values.get_value_count()?,
      "ord out of range: {ord}"
    );
    self.current_ord = ord;
    let term = self.values.lookup_ord(self.current_ord)?;
    let term = term.as_bytes_ref();
    self
      .scratch
      .copy_bytes_from_vec(term.bytes, term.offset, term.length)?;
    Ok(())
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    _term: &BytesRef<BS>,
    state: &TermStateEnum,
  ) -> Result<()> {
    debug_assert!(matches!(
      state,
      TermStateEnum::Ord(_) | TermStateEnum::Block(_) | TermStateEnum::Int(_)
    ));
    self.seek_exact_with_ord(state.ord()?)
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    Ok(self.scratch.get_bytes_ref())
  }

  fn ord(&self) -> Result<i64> {
    Ok(self.current_ord)
  }

  fn doc_freq(&mut self) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }

  type PostingsEnum = DummyPostingsEnum;

  fn postings(&mut self, _reuse: Option<Self::PostingsEnum>) -> Result<Self::PostingsEnum> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn postings_with_flags(
    &mut self,
    _reuse: Option<Self::PostingsEnum>,
    _flags: i32,
  ) -> Result<Self::PostingsEnum> {
    Err(LuceneError::unsupported_operation(""))
  }

  type ImpactsEnum = DummyImpactsEnum;

  fn impacts(&mut self, _flags: i32) -> Result<Self::ImpactsEnum> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    let mut state = OrdTermState::new();
    state.ord = self.current_ord;
    Ok(state.into())
  }
}
