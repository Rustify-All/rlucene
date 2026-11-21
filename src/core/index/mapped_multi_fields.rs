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
use crate::core::index::automaton_terms_enum::AutomatonTermsEnum;
use crate::core::index::fields::Fields;
use crate::core::index::filtered_terms_enum::{FilteredTermsEnum, FilteredTermsEnumBase};
use crate::core::index::mapping_multi_postings_enum::MappingMultiPostingsEnum;
use crate::core::index::merge_state::{DocMap, MergeStateMeta};
use crate::core::index::multi_fields::{MultiFields, MultiFieldsTerms};
use crate::core::index::multi_terms_enum::MultiTermsEnum;
use crate::core::index::multi_terms_enum::MultiTermsEnumType;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{EmptyTermsEnum, SeekStatus, TermsEnum};
use crate::core::util::access::ByteSource;
use crate::core::util::automation::compiled_automaton::CompiledAutomaton;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::dummy::dummy_attribute_source::DummyAttributeSource;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::borrow::Cow;
use std::sync::Arc;

/// A [`Fields`] implementation that merges multiple
/// [`Fields`](crate::core::index::fields::Fields) instances into one,
/// while accounting for deleted documents.
///
/// This implementation is used during index merging.
pub struct MappedMultiFields<'a, F, DM>
where
  F: Fields,
{
  merge_state_meta: MergeStateMeta<DM>,
  inner: &'a MultiFields<F>,
}

impl<'a, F, DM> MappedMultiFields<'a, F, DM>
where
  F: Fields,
{
  pub fn new(merge_state_meta: MergeStateMeta<DM>, multi_fields: &'a MultiFields<F>) -> Self {
    MappedMultiFields {
      merge_state_meta,
      inner: multi_fields,
    }
  }
}
impl<F, DM> Fields for MappedMultiFields<'_, F, DM>
where
  F: Fields,
  DM: DocMap,
{
  type FieldIter<'a>
    = <MultiFields<F> as Fields>::FieldIter<'a>
  where
    Self: 'a;

  fn iterator(&self) -> Result<Self::FieldIter<'_>> {
    self.inner.iterator()
  }

  type Terms = MappedMultiTerms<<F as Fields>::Terms, DM>;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    let terms = self.inner.terms(field)?;
    match terms {
      Some(v) => Ok(Some(MappedMultiTerms::new(
        field,
        self.merge_state_meta.clone(),
        v,
      ))),
      None => Ok(None),
    }
  }

  fn size(&self) -> Result<i32> {
    self.inner.size()
  }
}

pub struct MappedMultiTerms<T, DM> {
  merge_state: MergeStateMeta<DM>,
  field: Arc<str>,
  inner: MultiFieldsTerms<T>,
}
impl<T, DM> MappedMultiTerms<T, DM> {
  pub fn new<FName>(
    field: FName,
    merge_state: MergeStateMeta<DM>,
    multi_terms: MultiFieldsTerms<T>,
  ) -> Self
  where
    FName: Into<Arc<str>>,
  {
    let field = field.into();
    MappedMultiTerms {
      merge_state,
      field,
      inner: multi_terms,
    }
  }
}
#[allow(clippy::large_enum_variant)] // Keep mapped terms iteration allocation-free.
pub enum MappedMultiTermsTE<T, DM>
where
  T: Terms,
{
  A(EmptyTermsEnum),
  B(MappedMultiTermsEnum<T::TermsEnum, DM>),
}

impl<T, DM> BytesRefIterator for MappedMultiTermsTE<T, DM>
where
  T: Terms,
  DM: DocMap,
{
  type Value<'a>
    = &'a BytesRef<Vec<u8>>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    match self {
      Self::A(terms) => terms.next(),
      Self::B(terms) => terms.next(),
    }
  }

  fn set_next(&mut self) -> Result<bool> {
    match self {
      Self::A(terms) => terms.set_next(),
      Self::B(terms) => terms.set_next(),
    }
  }
}

impl<T, DM> TermsEnum for MappedMultiTermsTE<T, DM>
where
  T: Terms,
  DM: DocMap,
{
  type AttributeSource<'a>
    = &'a DummyAttributeSource
  where
    Self: 'a;
  type AttributeSourceMut<'a>
    = &'a mut DummyAttributeSource
  where
    Self: 'a;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    match self {
      Self::A(terms) => terms.attributes(),
      Self::B(terms) => terms.attributes(),
    }
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    match self {
      Self::A(terms) => terms.attributes_mut(),
      Self::B(terms) => terms.attributes_mut(),
    }
  }

  fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool> {
    match self {
      Self::A(terms) => terms.seek_exact(term),
      Self::B(terms) => terms.seek_exact(term),
    }
  }

  crate::core::index::terms_enum::prepare_seek_exact_enum!(
    A(EmptyTermsEnum) => A,
    B(MappedMultiTermsEnum<T::TermsEnum, DM>) => B,
  );

  fn seek_ceil<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<SeekStatus> {
    match self {
      Self::A(terms) => terms.seek_ceil(term),
      Self::B(terms) => terms.seek_ceil(term),
    }
  }

  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    match self {
      Self::A(terms) => terms.seek_exact_with_ord(ord),
      Self::B(terms) => terms.seek_exact_with_ord(ord),
    }
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    state: &TermStateEnum,
  ) -> Result<()> {
    match self {
      Self::A(terms) => terms.seek_exact_with_state(term, state),
      Self::B(terms) => terms.seek_exact_with_state(term, state),
    }
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    match self {
      Self::A(terms) => terms.term(),
      Self::B(terms) => terms.term(),
    }
  }

  fn ord(&self) -> Result<i64> {
    match self {
      Self::A(terms) => terms.ord(),
      Self::B(terms) => terms.ord(),
    }
  }

  fn doc_freq(&mut self) -> Result<i32> {
    match self {
      Self::A(terms) => terms.doc_freq(),
      Self::B(terms) => terms.doc_freq(),
    }
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    match self {
      Self::A(terms) => terms.total_term_freq(),
      Self::B(terms) => terms.total_term_freq(),
    }
  }

  type PostingsEnum = <MappedMultiTermsEnum<T::TermsEnum, DM> as TermsEnum>::PostingsEnum;

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    match self {
      Self::A(_) => Err(LuceneError::illegal_state(
        "this method should never be called",
      )),
      Self::B(terms) => terms.postings_with_flags(reuse, flags),
    }
  }

  type ImpactsEnum = <MappedMultiTermsEnum<T::TermsEnum, DM> as TermsEnum>::ImpactsEnum;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    match self {
      Self::A(_) => Err(LuceneError::illegal_state(
        "this method should never be called",
      )),
      Self::B(terms) => terms.impacts(flags),
    }
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    match self {
      Self::A(terms) => terms.term_state(),
      Self::B(terms) => terms.term_state(),
    }
  }
}
impl<T, DM> Terms for MappedMultiTerms<T, DM>
where
  T: Terms,
  DM: DocMap,
{
  type TermsEnum = MappedMultiTermsTE<T, DM>;

  fn iterator(&self) -> Result<Self::TermsEnum> {
    let iterator = self.inner.iterator()?;
    match iterator {
      MultiTermsEnumType::B(empty) => Ok(MappedMultiTermsTE::A(empty)),
      MultiTermsEnumType::A(v) => {
        let v = MappedMultiTermsEnum::new(self.field.clone(), self.merge_state.clone(), v);
        Ok(MappedMultiTermsTE::B(v))
      },
    }
  }

  type IntersectIter
    = FilteredTermsEnum<Self::TermsEnum, AutomatonTermsEnum>
  where
    Self::TermsEnum: BytesRefIterator,
    AutomatonTermsEnum: FilteredTermsEnumBase;

  fn intersect(
    &self,
    compiled: &CompiledAutomaton,
    start_term: Option<&BytesRef<Vec<u8>>>,
  ) -> Result<Self::IntersectIter> {
    self.default_intersect(compiled, start_term)
  }

  fn size(&self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_sum_total_term_freq(&self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_sum_doc_freq(&self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn get_doc_count(&self) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn has_freqs(&self) -> bool {
    self.inner.has_freqs()
  }

  fn has_offsets(&self) -> bool {
    self.inner.has_offsets()
  }

  fn has_positions(&self) -> bool {
    self.inner.has_positions()
  }

  fn has_payloads(&self) -> bool {
    self.inner.has_payloads()
  }

  fn get_min(&self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
    self.inner.get_min()
  }

  fn get_max(&self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
    self.inner.get_max()
  }

  fn get_stats(&self) -> Result<String> {
    self.inner.get_stats()
  }
}

pub struct MappedMultiTermsEnum<TE, DM> {
  field: Arc<str>,
  merge_state_meta: MergeStateMeta<DM>,
  in_: MultiTermsEnum<TE>,
}
impl<TE, DM> MappedMultiTermsEnum<TE, DM> {
  pub fn new<FName>(
    field: FName,
    merge_state: MergeStateMeta<DM>,
    multi_terms_enum: MultiTermsEnum<TE>,
  ) -> Self
  where
    FName: Into<Arc<str>>,
  {
    let field = field.into();
    Self {
      field,
      merge_state_meta: merge_state,
      in_: multi_terms_enum,
    }
  }
}

impl<TE, DM> BytesRefIterator for MappedMultiTermsEnum<TE, DM>
where
  TE: TermsEnum,
  DM: DocMap,
{
  type Value<'a>
    = &'a BytesRef<Vec<u8>>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    self.in_.next()
  }
}

impl<TE, DM> TermsEnum for MappedMultiTermsEnum<TE, DM>
where
  TE: TermsEnum,
  DM: DocMap,
{
  type AttributeSource<'a>
    = <MultiTermsEnum<TE> as TermsEnum>::AttributeSource<'a>
  where
    Self: 'a;
  type AttributeSourceMut<'a>
    = <MultiTermsEnum<TE> as TermsEnum>::AttributeSourceMut<'a>
  where
    Self: 'a;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    self.in_.attributes()
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    self.in_.attributes_mut()
  }

  fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool> {
    self.in_.seek_exact(term)
  }

  crate::core::index::terms_enum::prepare_seek_exact_delegate!(MultiTermsEnum<TE> => in_);

  fn seek_ceil<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<SeekStatus> {
    self.in_.seek_ceil(term)
  }

  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    self.in_.seek_exact_with_ord(ord)
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    state: &TermStateEnum,
  ) -> Result<()> {
    self.in_.seek_exact_with_state(term, state)
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    self.in_.term()
  }

  fn ord(&self) -> Result<i64> {
    self.in_.ord()
  }

  fn doc_freq(&mut self) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }

  type PostingsEnum = MappingMultiPostingsEnum<<TE as TermsEnum>::PostingsEnum, DM>;

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    let mut mapping_docs_and_positions_enum = match reuse {
      Some(postings) => {
        if postings.field == self.field {
          postings
        } else {
          MappingMultiPostingsEnum::new(
            self.field.clone(),
            &self.merge_state_meta.doc_maps,
            self.merge_state_meta.fields_producers_len,
            self.merge_state_meta.needs_index_sort,
          )?
        }
      },
      None => MappingMultiPostingsEnum::new(
        self.field.clone(),
        &self.merge_state_meta.doc_maps,
        self.merge_state_meta.fields_producers_len,
        self.merge_state_meta.needs_index_sort,
      )?,
    };
    let v = mapping_docs_and_positions_enum.take_multi_docs_and_positions_enum();
    let docs_and_positions_enum = self.in_.postings_with_flags(v, flags)?;
    mapping_docs_and_positions_enum.reset(docs_and_positions_enum)?;
    Ok(mapping_docs_and_positions_enum)
  }

  type ImpactsEnum = <MultiTermsEnum<TE> as TermsEnum>::ImpactsEnum;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    self.in_.impacts(flags)
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    self.in_.term_state()
  }
}
