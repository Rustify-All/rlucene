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
use crate::core::codecs::mutable_point_tree::MutablePointTree;
use crate::core::codecs::stored_fields_writer::StoredFieldsWriter;
use crate::core::codecs::term_vectors_reader::DefaultTermVectorsReader;
use crate::core::index::BytesRef;
use crate::core::index::binary_doc_values::BinaryDocValues;
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::doc_values_skip_index_type::DocValuesSkipIndexType;
use crate::core::index::doc_values_skipper::DocValuesSkipper;
use crate::core::index::doc_values_type::DocValuesType;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::fields::Fields;
use crate::core::index::impacts::Impacts;
use crate::core::index::impacts_enum::ImpactsEnum;
use crate::core::index::impacts_source::ImpactsSource;
use crate::core::index::index_reader::{
  CacheHelper, CacheKey, Identity, IndexReader, IndexReaderBase, LeafReaderContextKind,
};
use crate::core::index::leaf_metadata::LeafMetaData;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::point_values::{
  IntersectVisitor, PointTree, PointTreeEnum, PointValues, Relation,
};
use crate::core::index::postings_enum::{FREQS, PostingsEnum};
use crate::core::index::singleton_sorted_numeric_doc_values::SingletonSortedNumericDocValues;
use crate::core::index::singleton_sorted_set_doc_values::SingletonSortedSetDocValues;
use crate::core::index::sorted_doc_values::SortedDocValues;
use crate::core::index::sorted_doc_values_terms_enum::SortedDocValuesTermsEnum;
use crate::core::index::sorted_numeric_doc_values::SortedNumericDocValues;
use crate::core::index::sorted_set_doc_values::SortedSetDocValues;
use crate::core::index::sorted_set_doc_values_terms_enum::SortedSetDocValuesTermsEnum;
use crate::core::index::stored_field_visitor::StoredFieldVisitor;
use crate::core::index::stored_fields::{RawStoredFieldsReader, StoredFields};
use crate::core::index::term::Term;
use crate::core::index::term_vectors::{RawTermVectors, TermVectors};
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum, TermsEnumEnum2};
use crate::core::search::doc_id_set_iterator::{DocIdSetIterator, NO_MORE_DOCS};
use crate::core::search::knn_collector::KnnCollector;
use crate::core::util::HasIdentity;
use crate::core::util::automation::compiled_automaton::CompiledAutomaton;
use crate::core::util::bits::Bits;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::clone::TryClone;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::test_framework::core::util::index_package_access::{
  IndexPackageAccess, IndexPackageAccessImpl,
};
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::thread::ThreadId;

fn assert_thread(object: &str, creation_thread: ThreadId) {
  let current_thread = std::thread::current().id();
  assert_eq!(
    creation_thread, current_thread,
    "{object} are only supposed to be consumed in the thread in which they have been acquired. \
     But were acquired in {creation_thread:?} and consumed in {current_thread:?}."
  );
}

/// A [`LeafReader`] that can be used to apply additional checks for tests.
pub struct AssertingLeafReader<LR> {
  in_: LR,
  index_base: IndexReaderBase,
}

impl<LR> AssertingLeafReader<LR>
where
  LR: LeafReader,
{
  pub fn new(in_: LR) -> Result<Self> {
    // Check some basic reader sanity.
    let max_doc = in_.max_doc()?;
    let num_docs = in_.num_docs()?;
    let num_deleted_docs = in_.num_deleted_docs()?;
    let has_deletions = in_.has_deletions()?;
    assert!(max_doc >= 0);
    assert!(num_docs <= max_doc);
    assert_eq!(num_deleted_docs + num_docs, max_doc);
    assert!(!has_deletions || (num_deleted_docs > 0 && num_docs < max_doc));

    if let Some(core_cache_helper) = in_.get_core_cache_helper()? {
      let expected_key = core_cache_helper.get_key();
      core_cache_helper.add_closed_listener(Arc::new(move |cache_key: &CacheKey| {
        assert_eq!(
          expected_key, *cache_key,
          "Core closed listener called on a different key"
        );
        Ok(())
      }))?;
    }

    if let Some(reader_cache_helper) = in_.get_reader_cache_helper()? {
      let expected_key = reader_cache_helper.get_key();
      reader_cache_helper.add_closed_listener(Arc::new(move |cache_key: &CacheKey| {
        assert_eq!(
          expected_key, *cache_key,
          "Reader closed listener called on a different key"
        );
        Ok(())
      }))?;
    }

    let index_base = IndexReaderBase::new();
    in_.register_parent_reader(&index_base)?;
    Ok(Self { in_, index_base })
  }
}

impl<LR> Clone for AssertingLeafReader<LR>
where
  LR: LeafReader + Clone,
{
  fn clone(&self) -> Self {
    Self {
      in_: self.in_.clone(),
      index_base: self.index_base.clone(),
    }
  }
}

impl<LR> Display for AssertingLeafReader<LR>
where
  LR: LeafReader,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "AssertingLeafReader({})", self.in_)
  }
}

/// Wraps [`StoredFields`] with additional assertions.
pub struct AssertingStoredFields<S> {
  in_: S,
  creation_thread: ThreadId,
}

impl<S> RawStoredFieldsReader for AssertingStoredFields<S>
where
  S: StoredFields,
{
  type IndexInput = S::IndexInput;
}

impl<S> StoredFields for AssertingStoredFields<S>
where
  S: StoredFields,
{
  fn prefetch(&mut self, doc_id: i32) -> Result<()> {
    assert_thread("StoredFields", self.creation_thread);
    self.in_.prefetch(doc_id)
  }

  fn document_with_visitor<W>(
    &mut self,
    doc_id: i32,
    visitor: &mut impl StoredFieldVisitor,
    writer: Option<&mut W>,
  ) -> Result<()>
  where
    W: StoredFieldsWriter,
  {
    assert_thread("StoredFields", self.creation_thread);
    self.in_.document_with_visitor(doc_id, visitor, writer)
  }
}

/// Wraps [`TermVectors`] with additional assertions.
pub struct AssertingTermVectors<TV> {
  in_: TV,
  creation_thread: ThreadId,
}

impl<TV> RawTermVectors for AssertingTermVectors<TV>
where
  TV: TermVectors,
{
  type IndexInput = TV::IndexInput;

  fn raw_term_vectors_mut(&mut self) -> Result<&mut DefaultTermVectorsReader<Self::IndexInput>> {
    Err(LuceneError::unsupported_operation(
      "raw term vectors are not available",
    ))
  }

  fn raw_term_vectors(&self) -> Result<&DefaultTermVectorsReader<Self::IndexInput>> {
    Err(LuceneError::unsupported_operation(
      "raw term vectors are not available",
    ))
  }
}

impl<TV> TermVectors for AssertingTermVectors<TV>
where
  TV: TermVectors,
{
  type Fields = AssertingFields<TV::Fields>;
  type Terms = <Self::Fields as Fields>::Terms;

  fn prefetch(&mut self, doc_id: i32) -> Result<()> {
    assert_thread("TermVectors", self.creation_thread);
    self.in_.prefetch(doc_id)
  }

  fn get(&mut self, doc: i32) -> Result<Option<Self::Fields>> {
    assert_thread("TermVectors", self.creation_thread);
    Ok(self.in_.get(doc)?.map(AssertingFields::new))
  }

  fn get_field_terms(
    &mut self,
    doc: i32,
    field: &str,
  ) -> Result<Option<<Self::Fields as crate::core::index::fields::Fields>::Terms>> {
    assert_thread("TermVectors", self.creation_thread);
    self.default_get_field_terms(doc, field)
  }
}

/// Wraps [`Fields`] with additional assertions.
pub struct AssertingFields<F> {
  in_: F,
  creation_thread: ThreadId,
}

impl<F> AssertingFields<F>
where
  F: Fields,
{
  pub fn new(in_: F) -> Self {
    Self {
      in_,
      creation_thread: std::thread::current().id(),
    }
  }
}

impl<F> Fields for AssertingFields<F>
where
  F: Fields,
{
  type FieldIter<'a>
    = F::FieldIter<'a>
  where
    Self: 'a;

  fn iterator(&self) -> Result<Self::FieldIter<'_>> {
    assert_thread("Fields", self.creation_thread);
    self.in_.iterator()
  }

  type Terms = AssertingTerms<F::Terms>;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    assert_thread("Fields", self.creation_thread);
    Ok(self.in_.terms(field)?.map(AssertingTerms::new))
  }

  fn size(&self) -> Result<i32> {
    self.in_.size()
  }
}

/// Wraps [`Terms`] with additional assertions.
pub struct AssertingTerms<T> {
  in_: T,
  creation_thread: ThreadId,
  asserting: bool,
}

impl<T> AssertingTerms<T>
where
  T: Terms,
{
  pub fn new(in_: T) -> Self {
    Self {
      in_,
      creation_thread: std::thread::current().id(),
      asserting: true,
    }
  }

  pub(crate) fn new_default(in_: T) -> Self {
    Self {
      in_,
      creation_thread: std::thread::current().id(),
      asserting: false,
    }
  }
}

impl<T> Terms for AssertingTerms<T>
where
  T: Terms,
{
  type TermsEnum = AssertingTermsEnum<T::TermsEnum>;

  fn iterator(&self) -> Result<Self::TermsEnum> {
    if !self.asserting {
      return self.in_.iterator().map(AssertingTermsEnum::new_default);
    }
    assert_thread("Terms", self.creation_thread);
    Ok(AssertingTermsEnum::new(
      self.in_.iterator()?,
      self.has_freqs(),
    ))
  }

  type IntersectIter = AssertingTermsEnum<T::IntersectIter>;

  fn intersect(
    &self,
    compiled: &CompiledAutomaton,
    start_term: Option<&BytesRef<Vec<u8>>>,
  ) -> Result<Self::IntersectIter> {
    if !self.asserting {
      return self
        .in_
        .intersect(compiled, start_term)
        .map(AssertingTermsEnum::new_default);
    }
    assert_thread("Terms", self.creation_thread);
    let terms_enum = self.in_.intersect(compiled, start_term)?;
    if let Some(start_term) = start_term {
      assert!(start_term.is_valid()?);
    }
    Ok(AssertingTermsEnum::new(terms_enum, self.has_freqs()))
  }

  fn size(&self) -> Result<i64> {
    self.in_.size()
  }

  fn get_sum_total_term_freq(&self) -> Result<i64> {
    if !self.asserting {
      return self.in_.get_sum_total_term_freq();
    }
    assert_thread("Terms", self.creation_thread);
    let sum_total_term_freq = self.in_.get_sum_total_term_freq()?;
    if !self.has_freqs() {
      assert_eq!(sum_total_term_freq, self.in_.get_sum_doc_freq()?);
    }
    assert!(sum_total_term_freq >= self.get_sum_doc_freq()?);
    Ok(sum_total_term_freq)
  }

  fn get_sum_doc_freq(&self) -> Result<i64> {
    if !self.asserting {
      return self.in_.get_sum_doc_freq();
    }
    assert_thread("Terms", self.creation_thread);
    let sum_doc_freq = self.in_.get_sum_doc_freq()?;
    assert!(sum_doc_freq >= self.get_doc_count()? as i64);
    Ok(sum_doc_freq)
  }

  fn get_doc_count(&self) -> Result<i32> {
    if !self.asserting {
      return self.in_.get_doc_count();
    }
    assert_thread("Terms", self.creation_thread);
    let doc_count = self.in_.get_doc_count()?;
    assert!(doc_count > 0);
    Ok(doc_count)
  }

  fn has_freqs(&self) -> bool {
    self.in_.has_freqs()
  }

  fn has_offsets(&self) -> bool {
    self.in_.has_offsets()
  }

  fn has_positions(&self) -> bool {
    self.in_.has_positions()
  }

  fn has_payloads(&self) -> bool {
    self.in_.has_payloads()
  }

  fn get_min(&self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
    if !self.asserting {
      return self.in_.get_min();
    }
    assert_thread("Terms", self.creation_thread);
    let value = self.in_.get_min()?;
    if let Some(term) = value.as_ref() {
      assert!(term.is_valid()?);
    }
    Ok(value)
  }

  fn get_max(&self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
    if !self.asserting {
      return self.in_.get_max();
    }
    assert_thread("Terms", self.creation_thread);
    let value = self.in_.get_max()?;
    if let Some(term) = value.as_ref() {
      assert!(term.is_valid()?);
    }
    Ok(value)
  }

  fn get_stats(&self) -> Result<String> {
    self.in_.get_stats()
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AssertingTermsEnumState {
  Initial,
  Positioned,
  Unpositioned,
  TwoPhaseSeeking,
}

pub struct AssertingTermsEnum<TE> {
  in_: TE,
  creation_thread: ThreadId,
  state: AssertingTermsEnumState,
  has_freqs: bool,
  asserting: bool,
}

impl<TE> AssertingTermsEnum<TE>
where
  TE: TermsEnum,
{
  fn new(in_: TE, has_freqs: bool) -> Self {
    Self {
      in_,
      creation_thread: std::thread::current().id(),
      state: AssertingTermsEnumState::Initial,
      has_freqs,
      asserting: true,
    }
  }

  fn new_default(in_: TE) -> Self {
    Self {
      in_,
      creation_thread: std::thread::current().id(),
      state: AssertingTermsEnumState::Initial,
      has_freqs: false,
      asserting: false,
    }
  }

  #[allow(dead_code)]
  fn reset(&mut self) {
    self.state = AssertingTermsEnumState::Initial;
  }
}

impl<TE> BytesRefIterator for AssertingTermsEnum<TE>
where
  TE: TermsEnum,
{
  // TODO: we should separately track if we are "at the end"?
  // Someone should not call next() after it returns `None`!
  fn next(&mut self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
    if !self.asserting {
      return self.in_.next();
    }
    assert_thread("Terms enums", self.creation_thread);
    assert!(
      self.state == AssertingTermsEnumState::Initial
        || self.state == AssertingTermsEnumState::Positioned,
      "next() called on unpositioned TermsEnum"
    );
    let result = self.in_.next()?;
    if let Some(term) = result.as_ref() {
      assert!(term.is_valid()?);
      self.state = AssertingTermsEnumState::Positioned;
    } else {
      self.state = AssertingTermsEnumState::Unpositioned;
    }
    Ok(result)
  }

  fn set_next(&mut self) -> Result<bool> {
    Ok(self.next()?.is_some())
  }
}

impl<TE> TermsEnum for AssertingTermsEnum<TE>
where
  TE: TermsEnum,
{
  type AttributeSource<'a>
    = TE::AttributeSource<'a>
  where
    Self: 'a;
  type AttributeSourceMut<'a>
    = TE::AttributeSourceMut<'a>
  where
    Self: 'a;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    self.in_.attributes()
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    self.in_.attributes_mut()
  }

  fn seek_exact(&mut self, text: &BytesRef<Vec<u8>>) -> Result<bool> {
    if !self.asserting {
      return self.in_.seek_exact(text);
    }
    assert_thread("Terms enums", self.creation_thread);
    assert_ne!(
      self.state,
      AssertingTermsEnumState::TwoPhaseSeeking,
      "Unfinished two-phase seeking"
    );
    assert!(text.is_valid()?);
    let result = self.in_.seek_exact(text)?;
    self.state = if result {
      AssertingTermsEnumState::Positioned
    } else {
      AssertingTermsEnumState::Unpositioned
    };
    Ok(result)
  }

  fn prepare_seek_exact(&mut self, text: &BytesRef<Vec<u8>>) -> Result<Option<()>> {
    if !self.asserting {
      return self.in_.prepare_seek_exact(text);
    }
    assert_thread("Terms enums", self.creation_thread);
    assert_ne!(
      self.state,
      AssertingTermsEnumState::TwoPhaseSeeking,
      "Unfinished two-phase seeking"
    );
    assert!(text.is_valid()?);
    let result = self.in_.prepare_seek_exact(text)?;
    if result.is_some() {
      self.state = AssertingTermsEnumState::TwoPhaseSeeking;
    }
    Ok(result)
  }

  fn get_prepare_seek_exact_status(&mut self, target: &BytesRef<Vec<u8>>) -> Result<bool> {
    if !self.asserting {
      return self.in_.get_prepare_seek_exact_status(target);
    }
    assert_thread("Terms enums", self.creation_thread);
    assert_eq!(
      self.state,
      AssertingTermsEnumState::TwoPhaseSeeking,
      "getPrepareSeekExactStatus() called without pending two-phase seeking"
    );
    let result = self.in_.get_prepare_seek_exact_status(target)?;
    self.state = if result {
      AssertingTermsEnumState::Positioned
    } else {
      AssertingTermsEnumState::Unpositioned
    };
    Ok(result)
  }

  fn seek_ceil(&mut self, term: &BytesRef<Vec<u8>>) -> Result<SeekStatus> {
    if !self.asserting {
      return self.in_.seek_ceil(term);
    }
    assert_thread("Terms enums", self.creation_thread);
    assert_ne!(
      self.state,
      AssertingTermsEnumState::TwoPhaseSeeking,
      "Unfinished two-phase seeking"
    );
    assert!(term.is_valid()?);
    let result = self.in_.seek_ceil(term)?;
    self.state = if result == SeekStatus::End {
      AssertingTermsEnumState::Unpositioned
    } else {
      AssertingTermsEnumState::Positioned
    };
    Ok(result)
  }

  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    if !self.asserting {
      return self.in_.seek_exact_with_ord(ord);
    }
    assert_thread("Terms enums", self.creation_thread);
    assert_ne!(
      self.state,
      AssertingTermsEnumState::TwoPhaseSeeking,
      "Unfinished two-phase seeking"
    );
    self.in_.seek_exact_with_ord(ord)?;
    self.state = AssertingTermsEnumState::Positioned;
    Ok(())
  }

  fn seek_exact_with_state(
    &mut self,
    term: &BytesRef<Vec<u8>>,
    state: &TermStateEnum,
  ) -> Result<()> {
    if !self.asserting {
      return self.in_.seek_exact_with_state(term, state);
    }
    assert_thread("Terms enums", self.creation_thread);
    assert_ne!(
      self.state,
      AssertingTermsEnumState::TwoPhaseSeeking,
      "Unfinished two-phase seeking"
    );
    assert!(term.is_valid()?);
    self.in_.seek_exact_with_state(term, state)?;
    self.state = AssertingTermsEnumState::Positioned;
    Ok(())
  }

  fn term(&self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
    if !self.asserting {
      return self.in_.term();
    }
    assert_thread("Terms enums", self.creation_thread);
    assert_eq!(
      self.state,
      AssertingTermsEnumState::Positioned,
      "term() called on unpositioned TermsEnum"
    );
    let term = self.in_.term()?;
    assert!(term.is_valid()?);
    Ok(term)
  }

  fn ord(&self) -> Result<i64> {
    if !self.asserting {
      return self.in_.ord();
    }
    assert_thread("Terms enums", self.creation_thread);
    assert_eq!(
      self.state,
      AssertingTermsEnumState::Positioned,
      "ord() called on unpositioned TermsEnum"
    );
    self.in_.ord()
  }

  fn doc_freq(&mut self) -> Result<i32> {
    if !self.asserting {
      return self.in_.doc_freq();
    }
    assert_thread("Terms enums", self.creation_thread);
    assert_eq!(
      self.state,
      AssertingTermsEnumState::Positioned,
      "docFreq() called on unpositioned TermsEnum"
    );
    let doc_freq = self.in_.doc_freq()?;
    assert!(doc_freq > 0);
    Ok(doc_freq)
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    if !self.asserting {
      return self.in_.total_term_freq();
    }
    assert_thread("Terms enums", self.creation_thread);
    assert_eq!(
      self.state,
      AssertingTermsEnumState::Positioned,
      "totalTermFreq() called on unpositioned TermsEnum"
    );
    let total_term_freq = self.in_.total_term_freq()?;
    if self.has_freqs {
      assert!(total_term_freq >= self.doc_freq()? as i64);
    } else {
      assert_eq!(total_term_freq, self.doc_freq()? as i64);
    }
    Ok(total_term_freq)
  }

  type PostingsEnum = AssertingPostingsEnum<TE::PostingsEnum>;

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    if !self.asserting {
      let reuse = reuse.map(AssertingPostingsEnum::unwrap);
      return Ok(AssertingPostingsEnum::new_default(
        self.in_.postings_with_flags(reuse, flags)?,
      ));
    }
    assert_thread("Terms enums", self.creation_thread);
    assert_eq!(
      self.state,
      AssertingTermsEnumState::Positioned,
      "docs(...) called on unpositioned TermsEnum"
    );

    let (actual_reuse, reuse_thread) = if let Some(reuse) = reuse {
      let AssertingPostingsEnum {
        in_,
        creation_thread,
        ..
      } = reuse;
      (Some(in_), Some(creation_thread))
    } else {
      (None, None)
    };
    let docs = self.in_.postings_with_flags(actual_reuse, flags)?;
    Ok(match reuse_thread {
      Some(creation_thread) => AssertingPostingsEnum::with_creation_thread(docs, creation_thread),
      None => AssertingPostingsEnum::new(docs),
    })
  }

  type ImpactsEnum = AssertingImpactsEnum<TE::ImpactsEnum>;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    if !self.asserting {
      return Ok(AssertingImpactsEnum::new_default(self.in_.impacts(flags)?));
    }
    assert_thread("Terms enums", self.creation_thread);
    assert_eq!(
      self.state,
      AssertingTermsEnumState::Positioned,
      "docs(...) called on unpositioned TermsEnum"
    );
    assert_ne!(
      flags & FREQS as i32,
      0,
      "Freqs should be requested on impacts"
    );
    Ok(AssertingImpactsEnum::new(self.in_.impacts(flags)?))
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    if !self.asserting {
      return self.in_.term_state();
    }
    assert_thread("Terms enums", self.creation_thread);
    assert_eq!(
      self.state,
      AssertingTermsEnumState::Positioned,
      "termState() called on unpositioned TermsEnum"
    );
    self.in_.term_state()
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DocsEnumState {
  Start,
  Iterating,
  Finished,
}

/// Wraps a docs enum with additional checks.
pub struct AssertingPostingsEnum<PE> {
  in_: PE,
  creation_thread: ThreadId,
  state: DocsEnumState,
  position_count: i32,
  position_max: i32,
  doc: i32,
  asserting: bool,
}

impl<PE> AssertingPostingsEnum<PE>
where
  PE: PostingsEnum,
{
  pub fn new(in_: PE) -> Self {
    Self::with_creation_thread(in_, std::thread::current().id())
  }

  fn new_default(in_: PE) -> Self {
    let doc = in_.doc_id();
    Self {
      in_,
      creation_thread: std::thread::current().id(),
      state: DocsEnumState::Start,
      position_count: 0,
      position_max: 0,
      doc,
      asserting: false,
    }
  }

  fn with_creation_thread(in_: PE, creation_thread: ThreadId) -> Self {
    let doc = in_.doc_id();
    Self {
      in_,
      creation_thread,
      state: DocsEnumState::Start,
      position_count: 0,
      position_max: 0,
      doc,
      asserting: true,
    }
  }

  pub fn unwrap(self) -> PE {
    self.in_
  }

  fn reset(&mut self) {
    self.state = DocsEnumState::Start;
    self.doc = self.in_.doc_id();
    self.position_count = 0;
    self.position_max = 0;
  }
}

impl<PE> DocIdSetIterator for AssertingPostingsEnum<PE>
where
  PE: PostingsEnum,
{
  fn doc_id(&self) -> i32 {
    if !self.asserting {
      return self.in_.doc_id();
    }
    assert_thread("Docs enums", self.creation_thread);
    assert_eq!(
      self.doc,
      self.in_.doc_id(),
      "invalid docID() in wrapped postings enum"
    );
    self.doc
  }

  fn next_doc(&mut self) -> Result<i32> {
    if !self.asserting {
      return self.in_.next_doc();
    }
    assert_thread("Docs enums", self.creation_thread);
    assert_ne!(
      self.state,
      DocsEnumState::Finished,
      "nextDoc() called after NO_MORE_DOCS"
    );
    let next_doc = self.in_.next_doc()?;
    assert!(
      next_doc > self.doc,
      "backwards nextDoc from {} to {}",
      self.doc,
      next_doc
    );
    if next_doc == NO_MORE_DOCS {
      self.state = DocsEnumState::Finished;
      self.position_max = 0;
    } else {
      self.state = DocsEnumState::Iterating;
      self.position_max = self.in_.freq()?;
    }
    self.position_count = 0;
    assert_eq!(next_doc, self.in_.doc_id());
    self.doc = next_doc;
    Ok(next_doc)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    if !self.asserting {
      return self.in_.advance(target);
    }
    assert_thread("Docs enums", self.creation_thread);
    assert_ne!(
      self.state,
      DocsEnumState::Finished,
      "advance() called after NO_MORE_DOCS"
    );
    assert!(
      target > self.doc,
      "target must be > docID(), got {target} <= {}",
      self.doc
    );
    let advanced = self.in_.advance(target)?;
    assert!(
      advanced >= target,
      "backwards advance from: {target} to: {advanced}"
    );
    if advanced == NO_MORE_DOCS {
      self.state = DocsEnumState::Finished;
      self.position_max = 0;
    } else {
      self.state = DocsEnumState::Iterating;
      self.position_max = self.in_.freq()?;
    }
    self.position_count = 0;
    assert_eq!(advanced, self.in_.doc_id());
    self.doc = advanced;
    Ok(advanced)
  }

  fn cost(&self) -> Result<i64> {
    self.in_.cost()
  }
}

impl<PE> PostingsEnum for AssertingPostingsEnum<PE>
where
  PE: PostingsEnum,
{
  fn freq(&mut self) -> Result<i32> {
    if !self.asserting {
      return self.in_.freq();
    }
    assert_thread("Docs enums", self.creation_thread);
    assert_ne!(
      self.state,
      DocsEnumState::Start,
      "freq() called before nextDoc()/advance()"
    );
    assert_ne!(
      self.state,
      DocsEnumState::Finished,
      "freq() called after NO_MORE_DOCS"
    );
    let freq = self.in_.freq()?;
    assert!(freq > 0);
    Ok(freq)
  }

  fn next_position(&mut self) -> Result<i32> {
    if !self.asserting {
      return self.in_.next_position();
    }
    assert_ne!(
      self.state,
      DocsEnumState::Start,
      "nextPosition() called before nextDoc()/advance()"
    );
    assert_ne!(
      self.state,
      DocsEnumState::Finished,
      "nextPosition() called after NO_MORE_DOCS"
    );
    assert!(
      self.position_count < self.position_max,
      "nextPosition() called more than freq() times!"
    );
    let position = self.in_.next_position()?;
    assert!(
      position >= 0 || position == -1,
      "invalid position: {position}"
    );
    self.position_count += 1;
    Ok(position)
  }

  fn start_offset(&self) -> Result<i32> {
    if !self.asserting {
      return self.in_.start_offset();
    }
    assert_ne!(
      self.state,
      DocsEnumState::Start,
      "startOffset() called before nextDoc()/advance()"
    );
    assert_ne!(
      self.state,
      DocsEnumState::Finished,
      "startOffset() called after NO_MORE_DOCS"
    );
    assert!(
      self.position_count > 0,
      "startOffset() called before nextPosition()!"
    );
    self.in_.start_offset()
  }

  fn end_offset(&self) -> Result<i32> {
    if !self.asserting {
      return self.in_.end_offset();
    }
    assert_ne!(
      self.state,
      DocsEnumState::Start,
      "endOffset() called before nextDoc()/advance()"
    );
    assert_ne!(
      self.state,
      DocsEnumState::Finished,
      "endOffset() called after NO_MORE_DOCS"
    );
    assert!(
      self.position_count > 0,
      "endOffset() called before nextPosition()!"
    );
    self.in_.end_offset()
  }

  fn get_payload(&self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
    if !self.asserting {
      return self.in_.get_payload();
    }
    assert_ne!(
      self.state,
      DocsEnumState::Start,
      "getPayload() called before nextDoc()/advance()"
    );
    assert_ne!(
      self.state,
      DocsEnumState::Finished,
      "getPayload() called after NO_MORE_DOCS"
    );
    assert!(
      self.position_count > 0,
      "getPayload() called before nextPosition()!"
    );
    let payload = self.in_.get_payload()?;
    assert!(payload.as_ref().is_none_or(|payload| payload.length > 0));
    Ok(payload)
  }
}

/// Wraps an [`ImpactsEnum`] with additional checks.
pub struct AssertingImpactsEnum<IE> {
  asserting_postings: AssertingPostingsEnum<IE>,
  last_shallow_target: i32,
  valid_for: Option<Arc<AtomicI32>>,
  asserting: bool,
}

impl<IE> AssertingImpactsEnum<IE>
where
  IE: ImpactsEnum,
{
  fn new(impacts: IE) -> Self {
    let doc_id = impacts.doc_id();
    Self {
      asserting_postings: AssertingPostingsEnum::new(impacts),
      last_shallow_target: -1,
      valid_for: Some(Arc::new(AtomicI32::new(doc_id.max(-1)))),
      asserting: true,
    }
  }

  fn new_default(impacts: IE) -> Self {
    Self {
      asserting_postings: AssertingPostingsEnum::new_default(impacts),
      last_shallow_target: -1,
      valid_for: None,
      asserting: false,
    }
  }

  fn update_valid_for(&self) {
    self
      .valid_for
      .as_ref()
      .expect("asserting impacts enum must track validity")
      .store(
        self
          .asserting_postings
          .doc_id()
          .max(self.last_shallow_target),
        Ordering::Relaxed,
      );
  }
}

impl<IE> ImpactsSource for AssertingImpactsEnum<IE>
where
  IE: ImpactsEnum,
{
  fn advance_shallow(&mut self, target: i32) -> Result<()> {
    if !self.asserting {
      return self.asserting_postings.in_.advance_shallow(target);
    }
    assert!(
      target >= self.last_shallow_target,
      "called on decreasing targets: target = {target} < last target = {}",
      self.last_shallow_target
    );
    assert!(
      target >= self.doc_id(),
      "target = {target} < docID = {}",
      self.doc_id()
    );
    self.last_shallow_target = target;
    self.update_valid_for();
    self.asserting_postings.in_.advance_shallow(target)
  }

  type Impacts<'a>
    = AssertingImpacts<IE::Impacts<'a>>
  where
    Self: 'a;

  fn get_impacts(&self) -> Result<Self::Impacts<'_>> {
    if !self.asserting {
      return self
        .asserting_postings
        .in_
        .get_impacts()
        .map(AssertingImpacts::new_default);
    }
    assert!(
      self.doc_id() >= 0 || self.last_shallow_target >= 0,
      "Cannot get impacts until the iterator is positioned or advanceShallow has been called"
    );
    let impacts = self.asserting_postings.in_.get_impacts()?;
    let valid_for = self.doc_id().max(self.last_shallow_target);
    IndexPackageAccessImpl.check_impacts(&impacts, valid_for)?;
    Ok(AssertingImpacts::new(
      impacts,
      Arc::clone(
        self
          .valid_for
          .as_ref()
          .expect("asserting impacts enum must track validity"),
      ),
      valid_for,
    ))
  }
}

impl<IE> DocIdSetIterator for AssertingImpactsEnum<IE>
where
  IE: ImpactsEnum,
{
  fn doc_id(&self) -> i32 {
    self.asserting_postings.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    if !self.asserting {
      return self.asserting_postings.next_doc();
    }
    assert!(
      self.doc_id() + 1 >= self.last_shallow_target,
      "target = {} < last shallow target = {}",
      self.doc_id() + 1,
      self.last_shallow_target
    );
    let doc_id = self.asserting_postings.next_doc()?;
    self.update_valid_for();
    Ok(doc_id)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    if !self.asserting {
      return self.asserting_postings.advance(target);
    }
    assert!(
      target >= self.last_shallow_target,
      "target = {target} < last shallow target = {}",
      self.last_shallow_target
    );
    let doc_id = self.asserting_postings.advance(target)?;
    self.update_valid_for();
    Ok(doc_id)
  }

  fn cost(&self) -> Result<i64> {
    self.asserting_postings.cost()
  }
}

impl<IE> PostingsEnum for AssertingImpactsEnum<IE>
where
  IE: ImpactsEnum,
{
  fn freq(&mut self) -> Result<i32> {
    self.asserting_postings.freq()
  }

  fn next_position(&mut self) -> Result<i32> {
    self.asserting_postings.next_position()
  }

  fn start_offset(&self) -> Result<i32> {
    self.asserting_postings.start_offset()
  }

  fn end_offset(&self) -> Result<i32> {
    self.asserting_postings.end_offset()
  }

  fn get_payload(&self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
    self.asserting_postings.get_payload()
  }
}

impl<IE> ImpactsEnum for AssertingImpactsEnum<IE> where IE: ImpactsEnum {}

pub struct AssertingImpacts<I> {
  in_: I,
  current_valid_for: Option<Arc<AtomicI32>>,
  valid_for: i32,
  asserting: bool,
}

impl<I> AssertingImpacts<I>
where
  I: Impacts,
{
  fn new(in_: I, current_valid_for: Arc<AtomicI32>, valid_for: i32) -> Self {
    Self {
      in_,
      current_valid_for: Some(current_valid_for),
      valid_for,
      asserting: true,
    }
  }

  fn new_default(in_: I) -> Self {
    Self {
      in_,
      current_valid_for: None,
      valid_for: -1,
      asserting: false,
    }
  }

  fn assert_still_valid(&self) {
    assert_eq!(
      self.valid_for,
      self
        .current_valid_for
        .as_ref()
        .expect("asserting impacts must track validity")
        .load(Ordering::Relaxed),
      "Cannot reuse impacts after advancing the iterator"
    );
  }
}

impl<I> Impacts for AssertingImpacts<I>
where
  I: Impacts,
{
  fn num_levels(&self) -> i32 {
    if self.asserting {
      self.assert_still_valid();
    }
    self.in_.num_levels()
  }

  fn get_doc_id_upto(&self, level: i32) -> i32 {
    if self.asserting {
      self.assert_still_valid();
    }
    self.in_.get_doc_id_upto(level)
  }

  fn get_impacts(&self, level: i32) -> Result<Vec<crate::core::index::impact::Impact>> {
    if self.asserting {
      self.assert_still_valid();
    }
    // Rust's Vec always provides random access.
    self.in_.get_impacts(level)
  }
}

/// Wraps a [`NumericDocValues`] with additional assertions.
pub struct AssertingNumericDocValues<DV> {
  asserting: bool,
  creation_thread: ThreadId,
  in_: DV,
  max_doc: i32,
  last_doc_id: i32,
  exists: bool,
}

impl<DV> AssertingNumericDocValues<DV>
where
  DV: NumericDocValues,
{
  pub fn new(in_: DV, max_doc: i32) -> Self {
    // Should start unpositioned.
    assert_eq!(-1, in_.doc_id());
    Self {
      asserting: true,
      creation_thread: std::thread::current().id(),
      in_,
      max_doc,
      last_doc_id: -1,
      exists: false,
    }
  }

  pub(crate) fn new_default(in_: DV, max_doc: i32) -> Self {
    Self {
      asserting: false,
      creation_thread: std::thread::current().id(),
      in_,
      max_doc,
      last_doc_id: -1,
      exists: false,
    }
  }
}

impl<DV> DocIdSetIterator for AssertingNumericDocValues<DV>
where
  DV: NumericDocValues,
{
  fn doc_id(&self) -> i32 {
    if self.asserting {
      assert_thread("Numeric doc values", self.creation_thread);
    }
    self.in_.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    if self.asserting {
      assert_thread("Numeric doc values", self.creation_thread);
    }
    let doc_id = self.in_.next_doc()?;
    if self.asserting {
      assert!(doc_id > self.last_doc_id);
      assert!(doc_id == NO_MORE_DOCS || doc_id < self.max_doc);
      assert_eq!(doc_id, self.in_.doc_id());
    }
    self.last_doc_id = doc_id;
    self.exists = doc_id != NO_MORE_DOCS;
    Ok(doc_id)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    if self.asserting {
      assert_thread("Numeric doc values", self.creation_thread);
      assert!(target >= 0);
      assert!(target > self.in_.doc_id());
    }
    let doc_id = self.in_.advance(target)?;
    if self.asserting {
      assert!(doc_id >= target);
      assert!(doc_id == NO_MORE_DOCS || doc_id < self.max_doc);
    }
    self.last_doc_id = doc_id;
    self.exists = doc_id != NO_MORE_DOCS;
    Ok(doc_id)
  }

  fn cost(&self) -> Result<i64> {
    if self.asserting {
      assert_thread("Numeric doc values", self.creation_thread);
    }
    let cost = self.in_.cost()?;
    if self.asserting {
      assert!(cost >= 0);
    }
    Ok(cost)
  }
}

impl<DV> DocValuesIterator for AssertingNumericDocValues<DV>
where
  DV: NumericDocValues,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    if self.asserting {
      assert_thread("Numeric doc values", self.creation_thread);
      assert!(target >= 0);
      assert!(target >= self.in_.doc_id());
      assert!(target < self.max_doc);
    }
    self.exists = self.in_.advance_exact(target)?;
    if self.asserting {
      assert_eq!(target, self.in_.doc_id());
    }
    self.last_doc_id = target;
    Ok(self.exists)
  }
}

impl<DV> NumericDocValues for AssertingNumericDocValues<DV>
where
  DV: NumericDocValues,
{
  fn long_value(&mut self) -> Result<i64> {
    if self.asserting {
      assert_thread("Numeric doc values", self.creation_thread);
      assert!(self.exists);
    }
    self.in_.long_value()
  }
}

/// Wraps a [`BinaryDocValues`] with additional assertions.
pub struct AssertingBinaryDocValues<DV> {
  asserting: bool,
  creation_thread: ThreadId,
  in_: DV,
  max_doc: i32,
  last_doc_id: i32,
  exists: bool,
}

impl<DV> AssertingBinaryDocValues<DV>
where
  DV: BinaryDocValues,
{
  pub fn new(in_: DV, max_doc: i32) -> Self {
    // Should start unpositioned.
    assert_eq!(-1, in_.doc_id());
    Self {
      asserting: true,
      creation_thread: std::thread::current().id(),
      in_,
      max_doc,
      last_doc_id: -1,
      exists: false,
    }
  }

  pub(crate) fn new_default(in_: DV, max_doc: i32) -> Self {
    Self {
      asserting: false,
      creation_thread: std::thread::current().id(),
      in_,
      max_doc,
      last_doc_id: -1,
      exists: false,
    }
  }
}

impl<DV> DocIdSetIterator for AssertingBinaryDocValues<DV>
where
  DV: BinaryDocValues,
{
  fn doc_id(&self) -> i32 {
    if self.asserting {
      assert_thread("Binary doc values", self.creation_thread);
    }
    self.in_.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    if self.asserting {
      assert_thread("Binary doc values", self.creation_thread);
    }
    let doc_id = self.in_.next_doc()?;
    if self.asserting {
      assert!(doc_id > self.last_doc_id);
      assert!(doc_id == NO_MORE_DOCS || doc_id < self.max_doc);
      assert_eq!(doc_id, self.in_.doc_id());
    }
    self.last_doc_id = doc_id;
    self.exists = doc_id != NO_MORE_DOCS;
    Ok(doc_id)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    if self.asserting {
      assert_thread("Binary doc values", self.creation_thread);
      assert!(target >= 0);
      assert!(target > self.in_.doc_id());
    }
    let doc_id = self.in_.advance(target)?;
    if self.asserting {
      assert!(doc_id >= target);
      assert!(doc_id == NO_MORE_DOCS || doc_id < self.max_doc);
    }
    self.last_doc_id = doc_id;
    self.exists = doc_id != NO_MORE_DOCS;
    Ok(doc_id)
  }

  fn cost(&self) -> Result<i64> {
    if self.asserting {
      assert_thread("Binary doc values", self.creation_thread);
    }
    let cost = self.in_.cost()?;
    if self.asserting {
      assert!(cost >= 0);
    }
    Ok(cost)
  }
}

impl<DV> DocValuesIterator for AssertingBinaryDocValues<DV>
where
  DV: BinaryDocValues,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    if self.asserting {
      assert_thread("Numeric doc values", self.creation_thread);
      assert!(target >= 0);
      assert!(target >= self.in_.doc_id());
      assert!(target < self.max_doc);
    }
    self.exists = self.in_.advance_exact(target)?;
    if self.asserting {
      assert_eq!(target, self.in_.doc_id());
    }
    self.last_doc_id = target;
    Ok(self.exists)
  }
}

impl<DV> BinaryDocValues for AssertingBinaryDocValues<DV>
where
  DV: BinaryDocValues,
{
  fn binary_value(&mut self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
    if self.asserting {
      assert_thread("Binary doc values", self.creation_thread);
      assert!(self.exists);
    }
    self.in_.binary_value()
  }
}

/// Wraps a [`SortedDocValues`] with additional assertions.
pub struct AssertingSortedDocValues<DV> {
  asserting: bool,
  creation_thread: ThreadId,
  in_: DV,
  max_doc: i32,
  value_count: i32,
  last_doc_id: i32,
  exists: bool,
}

impl<DV> AssertingSortedDocValues<DV>
where
  DV: SortedDocValues,
{
  pub fn new(in_: DV, max_doc: i32) -> Result<Self> {
    let value_count = in_.get_value_count()?;
    assert!(value_count >= 0 && value_count <= max_doc);
    Ok(Self {
      asserting: true,
      creation_thread: std::thread::current().id(),
      in_,
      max_doc,
      value_count,
      last_doc_id: -1,
      exists: false,
    })
  }

  pub(crate) fn new_default(in_: DV, max_doc: i32) -> Result<Self> {
    Ok(Self {
      asserting: false,
      creation_thread: std::thread::current().id(),
      in_,
      max_doc,
      value_count: 0,
      last_doc_id: -1,
      exists: false,
    })
  }
}

impl<DV> DocIdSetIterator for AssertingSortedDocValues<DV>
where
  DV: SortedDocValues,
{
  fn doc_id(&self) -> i32 {
    if self.asserting {
      assert_thread("Sorted doc values", self.creation_thread);
    }
    self.in_.doc_id()
  }

  fn next_doc(&mut self) -> Result<i32> {
    if self.asserting {
      assert_thread("Sorted doc values", self.creation_thread);
    }
    let doc_id = self.in_.next_doc()?;
    if self.asserting {
      assert!(doc_id > self.last_doc_id);
      assert!(doc_id == NO_MORE_DOCS || doc_id < self.max_doc);
      assert_eq!(doc_id, self.in_.doc_id());
    }
    self.last_doc_id = doc_id;
    self.exists = doc_id != NO_MORE_DOCS;
    Ok(doc_id)
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    if self.asserting {
      assert_thread("Sorted doc values", self.creation_thread);
      assert!(target >= 0);
      assert!(target > self.in_.doc_id());
    }
    let doc_id = self.in_.advance(target)?;
    if self.asserting {
      assert!(doc_id >= target);
      assert!(doc_id == NO_MORE_DOCS || doc_id < self.max_doc);
    }
    self.last_doc_id = doc_id;
    self.exists = doc_id != NO_MORE_DOCS;
    Ok(doc_id)
  }

  fn cost(&self) -> Result<i64> {
    if self.asserting {
      assert_thread("Sorted doc values", self.creation_thread);
    }
    let cost = self.in_.cost()?;
    if self.asserting {
      assert!(cost >= 0);
    }
    Ok(cost)
  }
}

impl<DV> DocValuesIterator for AssertingSortedDocValues<DV>
where
  DV: SortedDocValues,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    if self.asserting {
      assert_thread("Numeric doc values", self.creation_thread);
      assert!(target >= 0);
      assert!(target >= self.in_.doc_id());
      assert!(target < self.max_doc);
    }
    self.exists = self.in_.advance_exact(target)?;
    if self.asserting {
      assert_eq!(target, self.in_.doc_id());
    }
    self.last_doc_id = target;
    Ok(self.exists)
  }
}

impl<DV> SortedDocValues for AssertingSortedDocValues<DV>
where
  DV: SortedDocValues,
{
  fn ord_value(&mut self) -> Result<i32> {
    if self.asserting {
      assert_thread("Sorted doc values", self.creation_thread);
      assert!(self.exists);
    }
    let ord = self.in_.ord_value()?;
    if self.asserting {
      assert!(ord >= -1 && ord < self.value_count);
    }
    Ok(ord)
  }

  fn lookup_ord(&mut self, ord: i32) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
    if self.asserting {
      assert_thread("Sorted doc values", self.creation_thread);
      assert!(ord >= 0 && ord < self.value_count);
    }
    let result = self.in_.lookup_ord(ord)?;
    if self.asserting {
      assert!(result.is_valid()?);
    }
    Ok(result)
  }

  fn get_value_count(&self) -> Result<i32> {
    if self.asserting {
      assert_thread("Sorted doc values", self.creation_thread);
    }
    let value_count = self.in_.get_value_count()?;
    if self.asserting {
      assert_eq!(self.value_count, value_count); // Should not change.
    }
    Ok(value_count)
  }

  fn lookup_term(&mut self, key: &BytesRef<Vec<u8>>) -> Result<i32> {
    if self.asserting {
      assert_thread("Sorted doc values", self.creation_thread);
      assert!(key.is_valid()?);
    }
    let result = self.in_.lookup_term(key)?;
    if self.asserting {
      assert!(result < self.value_count);
      assert!(key.is_valid()?);
    }
    Ok(result)
  }

  type TermsEnum<'a>
    = TermsEnumEnum2<DV::TermsEnum<'a>, SortedDocValuesTermsEnum<&'a mut Self>>
  where
    Self: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    if self.asserting {
      Ok(TermsEnumEnum2::B(self.default_terms_enum()?))
    } else {
      Ok(TermsEnumEnum2::A(self.in_.terms_enum()?))
    }
  }
}

/// Wraps a [`SortedNumericDocValues`] with additional assertions.
pub enum AssertingSortedNumericDocValues<DV>
where
  DV: SortedNumericDocValues,
{
  Default(DV),
  Multi {
    creation_thread: ThreadId,
    in_: DV,
    max_doc: i32,
    last_doc_id: i32,
    value_upto: i32,
    exists: bool,
  },
  Single(SingletonSortedNumericDocValues<AssertingNumericDocValues<DV::NumericDocValues>>),
}

impl<DV> AssertingSortedNumericDocValues<DV>
where
  DV: SortedNumericDocValues,
{
  fn new(in_: DV, max_doc: i32) -> Self {
    Self::Multi {
      creation_thread: std::thread::current().id(),
      in_,
      max_doc,
      last_doc_id: -1,
      value_upto: 0,
      exists: false,
    }
  }

  pub fn create(mut in_: DV, max_doc: i32) -> Result<Self> {
    if !in_.is_single_valued() {
      Ok(Self::new(in_, max_doc))
    } else {
      let single_doc_values = in_.get_numeric_doc_values()?;
      let asserting_doc_values = AssertingNumericDocValues::new(single_doc_values, max_doc);
      Ok(Self::Single(SingletonSortedNumericDocValues::new(
        asserting_doc_values,
      )?))
    }
  }

  pub(crate) fn create_default(in_: DV) -> Self {
    Self::Default(in_)
  }
}

impl<DV> DocIdSetIterator for AssertingSortedNumericDocValues<DV>
where
  DV: SortedNumericDocValues,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::Default(in_) => in_.doc_id(),
      Self::Multi { in_, .. } => in_.doc_id(),
      Self::Single(in_) => in_.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::Default(in_) => in_.next_doc(),
      Self::Multi {
        creation_thread,
        in_,
        max_doc,
        last_doc_id,
        value_upto,
        exists,
      } => {
        assert_thread("Sorted numeric doc values", *creation_thread);
        let doc_id = in_.next_doc()?;
        assert!(doc_id > *last_doc_id);
        assert!(doc_id == NO_MORE_DOCS || doc_id < *max_doc);
        assert_eq!(doc_id, in_.doc_id());
        *last_doc_id = doc_id;
        *value_upto = 0;
        *exists = doc_id != NO_MORE_DOCS;
        Ok(doc_id)
      },
      Self::Single(in_) => in_.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Default(in_) => in_.advance(target),
      Self::Multi {
        creation_thread,
        in_,
        max_doc,
        last_doc_id,
        value_upto,
        exists,
      } => {
        assert_thread("Sorted numeric doc values", *creation_thread);
        assert!(target >= 0);
        assert!(target > in_.doc_id());
        let doc_id = in_.advance(target)?;
        assert_eq!(doc_id, in_.doc_id());
        assert!(doc_id >= target);
        assert!(doc_id == NO_MORE_DOCS || doc_id < *max_doc);
        *last_doc_id = doc_id;
        *value_upto = 0;
        *exists = doc_id != NO_MORE_DOCS;
        Ok(doc_id)
      },
      Self::Single(in_) => in_.advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::Default(in_) => in_.cost(),
      Self::Multi {
        creation_thread,
        in_,
        ..
      } => {
        assert_thread("Sorted numeric doc values", *creation_thread);
        let cost = in_.cost()?;
        assert!(cost >= 0);
        Ok(cost)
      },
      Self::Single(in_) => in_.cost(),
    }
  }
}

impl<DV> DocValuesIterator for AssertingSortedNumericDocValues<DV>
where
  DV: SortedNumericDocValues,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::Default(in_) => in_.advance_exact(target),
      Self::Multi {
        creation_thread,
        in_,
        max_doc,
        last_doc_id,
        value_upto,
        exists,
      } => {
        assert_thread("Numeric doc values", *creation_thread);
        assert!(target >= 0);
        assert!(target >= in_.doc_id());
        assert!(target < *max_doc);
        *exists = in_.advance_exact(target)?;
        assert_eq!(target, in_.doc_id());
        *last_doc_id = target;
        *value_upto = 0;
        Ok(*exists)
      },
      Self::Single(in_) => in_.advance_exact(target),
    }
  }
}

impl<DV> SortedNumericDocValues for AssertingSortedNumericDocValues<DV>
where
  DV: SortedNumericDocValues,
{
  fn next_value(&mut self) -> Result<i64> {
    match self {
      Self::Default(in_) => in_.next_value(),
      Self::Multi {
        creation_thread,
        in_,
        value_upto,
        exists,
        ..
      } => {
        assert_thread("Sorted numeric doc values", *creation_thread);
        assert!(*exists);
        assert!(
          *value_upto < in_.doc_value_count()?,
          "valueUpto={} in.docValueCount()={}",
          *value_upto,
          in_.doc_value_count()?
        );
        *value_upto += 1;
        in_.next_value()
      },
      Self::Single(in_) => in_.next_value(),
    }
  }

  fn doc_value_count(&mut self) -> Result<i32> {
    match self {
      Self::Default(in_) => in_.doc_value_count(),
      Self::Multi {
        creation_thread,
        in_,
        exists,
        ..
      } => {
        assert_thread("Sorted numeric doc values", *creation_thread);
        assert!(*exists);
        let count = in_.doc_value_count()?;
        assert!(count > 0);
        Ok(count)
      },
      Self::Single(in_) => in_.doc_value_count(),
    }
  }

  fn is_single_valued(&self) -> bool {
    match self {
      Self::Default(in_) => in_.is_single_valued(),
      Self::Multi { .. } => false,
      Self::Single(_) => true,
    }
  }

  type NumericDocValues = AssertingNumericDocValues<DV::NumericDocValues>;

  fn get_numeric_doc_values(&mut self) -> Result<Self::NumericDocValues> {
    match self {
      Self::Default(in_) => Ok(AssertingNumericDocValues::new_default(
        in_.get_numeric_doc_values()?,
        i32::MAX,
      )),
      Self::Multi { .. } => Err(LuceneError::unsupported_operation(
        "sorted numeric doc values are not single-valued",
      )),
      Self::Single(in_) => in_.get_numeric_doc_values(),
    }
  }
}

/// Wraps a [`SortedSetDocValues`] with additional assertions.
pub enum AssertingSortedSetDocValues<DV>
where
  DV: SortedSetDocValues,
{
  Default(DV),
  Multi {
    creation_thread: ThreadId,
    in_: DV,
    max_doc: i32,
    value_count: i64,
    last_doc_id: i32,
    ords_retrieved: i32,
    exists: bool,
  },
  Single(SingletonSortedSetDocValues<AssertingSortedDocValues<DV::SortedDocValues>>),
}

impl<DV> AssertingSortedSetDocValues<DV>
where
  DV: SortedSetDocValues,
{
  fn new(in_: DV, max_doc: i32) -> Result<Self> {
    let value_count = in_.get_value_count()?;
    assert!(value_count >= 0);
    Ok(Self::Multi {
      creation_thread: std::thread::current().id(),
      in_,
      max_doc,
      value_count,
      last_doc_id: -1,
      ords_retrieved: 0,
      exists: false,
    })
  }

  pub fn create(mut in_: DV, max_doc: i32) -> Result<Self> {
    if !in_.is_single_valued() {
      Self::new(in_, max_doc)
    } else {
      let single_doc_values = in_.get_sorted_doc_values()?;
      let asserting_doc_values = AssertingSortedDocValues::new(single_doc_values, max_doc)?;
      Ok(Self::Single(SingletonSortedSetDocValues::new(
        asserting_doc_values,
      )?))
    }
  }

  pub(crate) fn create_default(in_: DV) -> Self {
    Self::Default(in_)
  }
}

impl<DV> DocIdSetIterator for AssertingSortedSetDocValues<DV>
where
  DV: SortedSetDocValues,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::Default(in_) => in_.doc_id(),
      Self::Multi {
        creation_thread,
        in_,
        ..
      } => {
        assert_thread("Sorted set doc values", *creation_thread);
        in_.doc_id()
      },
      Self::Single(in_) => in_.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::Default(in_) => in_.next_doc(),
      Self::Multi {
        creation_thread,
        in_,
        max_doc,
        last_doc_id,
        ords_retrieved,
        exists,
        ..
      } => {
        assert_thread("Sorted set doc values", *creation_thread);
        let doc_id = in_.next_doc()?;
        assert!(doc_id > *last_doc_id);
        assert!(doc_id == NO_MORE_DOCS || doc_id < *max_doc);
        assert_eq!(doc_id, in_.doc_id());
        *last_doc_id = doc_id;
        *exists = doc_id != NO_MORE_DOCS;
        *ords_retrieved = 0;
        Ok(doc_id)
      },
      Self::Single(in_) => in_.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Default(in_) => in_.advance(target),
      Self::Multi {
        creation_thread,
        in_,
        max_doc,
        last_doc_id,
        ords_retrieved,
        exists,
        ..
      } => {
        assert_thread("Sorted set doc values", *creation_thread);
        assert!(target >= 0);
        assert!(target > in_.doc_id());
        let doc_id = in_.advance(target)?;
        assert_eq!(doc_id, in_.doc_id());
        assert!(doc_id >= target);
        assert!(doc_id == NO_MORE_DOCS || doc_id < *max_doc);
        *last_doc_id = doc_id;
        *exists = doc_id != NO_MORE_DOCS;
        *ords_retrieved = 0;
        Ok(doc_id)
      },
      Self::Single(in_) => in_.advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::Default(in_) => in_.cost(),
      Self::Multi {
        creation_thread,
        in_,
        ..
      } => {
        assert_thread("Sorted set doc values", *creation_thread);
        let cost = in_.cost()?;
        assert!(cost >= 0);
        Ok(cost)
      },
      Self::Single(in_) => in_.cost(),
    }
  }
}

impl<DV> DocValuesIterator for AssertingSortedSetDocValues<DV>
where
  DV: SortedSetDocValues,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::Default(in_) => in_.advance_exact(target),
      Self::Multi {
        creation_thread,
        in_,
        max_doc,
        last_doc_id,
        ords_retrieved,
        exists,
        ..
      } => {
        assert_thread("Numeric doc values", *creation_thread);
        assert!(target >= 0);
        assert!(target >= in_.doc_id());
        assert!(target < *max_doc);
        *exists = in_.advance_exact(target)?;
        assert_eq!(target, in_.doc_id());
        *last_doc_id = target;
        *ords_retrieved = 0;
        Ok(*exists)
      },
      Self::Single(in_) => in_.advance_exact(target),
    }
  }
}

impl<DV> SortedSetDocValues for AssertingSortedSetDocValues<DV>
where
  DV: SortedSetDocValues,
{
  fn next_ord(&mut self) -> Result<i64> {
    match self {
      Self::Default(in_) => in_.next_ord(),
      Self::Multi {
        creation_thread,
        in_,
        value_count,
        ords_retrieved,
        exists,
        ..
      } => {
        assert_thread("Sorted set doc values", *creation_thread);
        assert!(*exists);
        assert!(*ords_retrieved < in_.doc_value_count()?);
        *ords_retrieved += 1;
        let ord = in_.next_ord()?;
        assert!(ord < *value_count);
        Ok(ord)
      },
      Self::Single(in_) => in_.next_ord(),
    }
  }

  fn doc_value_count(&mut self) -> Result<i32> {
    match self {
      Self::Default(in_) => in_.doc_value_count(),
      Self::Multi { in_, .. } => in_.doc_value_count(),
      Self::Single(in_) => in_.doc_value_count(),
    }
  }

  fn lookup_ord(&mut self, ord: i64) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
    match self {
      Self::Default(in_) => in_.lookup_ord(ord),
      Self::Multi {
        creation_thread,
        in_,
        value_count,
        ..
      } => {
        assert_thread("Sorted set doc values", *creation_thread);
        assert!(ord >= 0 && ord < *value_count);
        let result = in_.lookup_ord(ord)?;
        assert!(result.is_valid()?);
        Ok(result)
      },
      Self::Single(in_) => in_.lookup_ord(ord),
    }
  }

  fn get_value_count(&self) -> Result<i64> {
    match self {
      Self::Default(in_) => in_.get_value_count(),
      Self::Multi {
        creation_thread,
        in_,
        value_count,
        ..
      } => {
        assert_thread("Sorted set doc values", *creation_thread);
        let current_value_count = in_.get_value_count()?;
        assert_eq!(*value_count, current_value_count); // Should not change.
        Ok(current_value_count)
      },
      Self::Single(in_) => in_.get_value_count(),
    }
  }

  fn lookup_term(&mut self, key: &BytesRef<Vec<u8>>) -> Result<i64> {
    match self {
      Self::Default(in_) => in_.lookup_term(key),
      Self::Multi {
        creation_thread,
        in_,
        value_count,
        ..
      } => {
        assert_thread("Sorted set doc values", *creation_thread);
        assert!(key.is_valid()?);
        let result = in_.lookup_term(key)?;
        assert!(result < *value_count);
        assert!(key.is_valid()?);
        Ok(result)
      },
      Self::Single(in_) => in_.lookup_term(key),
    }
  }

  type TermsEnum<'a>
    = TermsEnumEnum2<DV::TermsEnum<'a>, SortedSetDocValuesTermsEnum<&'a mut Self>>
  where
    Self: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    match self {
      Self::Default(in_) => Ok(TermsEnumEnum2::A(in_.terms_enum()?)),
      Self::Multi { .. } | Self::Single(_) => Ok(TermsEnumEnum2::B(self.default_terms_enum()?)),
    }
  }

  fn is_single_valued(&self) -> bool {
    match self {
      Self::Default(in_) => in_.is_single_valued(),
      Self::Multi { .. } => false,
      Self::Single(_) => true,
    }
  }

  type SortedDocValues = AssertingSortedDocValues<DV::SortedDocValues>;

  fn get_sorted_doc_values(&mut self) -> Result<Self::SortedDocValues> {
    match self {
      Self::Default(in_) => {
        AssertingSortedDocValues::new_default(in_.get_sorted_doc_values()?, i32::MAX)
      },
      Self::Multi { .. } => Err(LuceneError::unsupported_operation(
        "sorted set doc values are not single-valued",
      )),
      Self::Single(in_) => in_.get_sorted_doc_values(),
    }
  }
}

/// Wraps a [`DocValuesSkipper`] with additional assertions.
pub struct AssertingDocValuesSkipper<S> {
  creation_thread: ThreadId,
  in_: S,
}

impl<S> AssertingDocValuesSkipper<S>
where
  S: DocValuesSkipper,
{
  /// Sole constructor.
  pub fn new(in_: S) -> Self {
    let asserting = Self {
      creation_thread: std::thread::current().id(),
      in_,
    };
    assert_eq!(-1, asserting.min_doc_id_with_level(0));
    assert_eq!(-1, asserting.max_doc_id_with_level(0));
    asserting
  }

  fn iterating(&self) -> bool {
    self.max_doc_id_with_level(0) != -1
      && self.min_doc_id_with_level(0) != -1
      && self.max_doc_id_with_level(0) != NO_MORE_DOCS
      && self.min_doc_id_with_level(0) != NO_MORE_DOCS
  }
}

impl<S> DocValuesSkipper for AssertingDocValuesSkipper<S>
where
  S: DocValuesSkipper,
{
  fn advance(&mut self, target: i32) -> Result<()> {
    assert_thread("Doc values skipper", self.creation_thread);
    assert!(
      target > self.max_doc_id_with_level(0),
      "Illegal to call advance() on a target that is not beyond the current interval"
    );
    self.in_.advance(target)?;
    assert!(self.in_.min_doc_id_with_level(0) <= self.in_.max_doc_id_with_level(0));
    Ok(())
  }

  fn num_levels(&self) -> usize {
    assert_thread("Doc values skipper", self.creation_thread);
    self.in_.num_levels()
  }

  fn min_doc_id_with_level(&self, level: usize) -> i32 {
    assert_thread("Doc values skipper", self.creation_thread);
    assert!(level < self.num_levels());
    let min_doc_id = self.in_.min_doc_id_with_level(level);
    assert!(min_doc_id <= self.in_.max_doc_id_with_level(level));
    if level > 0 {
      assert!(min_doc_id <= self.in_.min_doc_id_with_level(level - 1));
    }
    min_doc_id
  }

  fn max_doc_id_with_level(&self, level: usize) -> i32 {
    assert_thread("Doc values skipper", self.creation_thread);
    assert!(level < self.num_levels());
    let max_doc_id = self.in_.max_doc_id_with_level(level);
    assert!(max_doc_id >= self.in_.min_doc_id_with_level(level));
    if level > 0 {
      assert!(max_doc_id >= self.in_.max_doc_id_with_level(level - 1));
    }
    max_doc_id
  }

  fn min_value_with_level(&self, level: usize) -> i64 {
    assert_thread("Doc values skipper", self.creation_thread);
    assert!(self.iterating(), "Unpositioned iterator");
    assert!(level < self.num_levels());
    self.in_.min_value_with_level(level)
  }

  fn max_value_with_level(&self, level: usize) -> i64 {
    assert_thread("Doc values skipper", self.creation_thread);
    assert!(self.iterating(), "Unpositioned iterator");
    assert!(level < self.num_levels());
    self.in_.max_value_with_level(level)
  }

  fn doc_count_with_level(&self, level: usize) -> i32 {
    assert_thread("Doc values skipper", self.creation_thread);
    assert!(self.iterating(), "Unpositioned iterator");
    assert!(level < self.num_levels());
    self.in_.doc_count_with_level(level)
  }

  fn min_value(&self) -> i64 {
    assert_thread("Doc values skipper", self.creation_thread);
    self.in_.min_value()
  }

  fn max_value(&self) -> i64 {
    assert_thread("Doc values skipper", self.creation_thread);
    self.in_.max_value()
  }

  fn doc_count(&self) -> i32 {
    assert_thread("Doc values skipper", self.creation_thread);
    self.in_.doc_count()
  }
}

/// Wraps [`PointValues`] with additional assertions.
pub struct AssertingPointValues<PV> {
  creation_thread: ThreadId,
  in_: PV,
}

impl<PV> AssertingPointValues<PV>
where
  PV: PointValues,
{
  /// Sole constructor.
  pub fn new(in_: PV, max_doc: i32) -> Result<Self> {
    let asserting = Self {
      creation_thread: std::thread::current().id(),
      in_,
    };
    asserting.assert_stats(max_doc)?;
    Ok(asserting)
  }

  pub fn get_wrapped(&self) -> &PV {
    &self.in_
  }

  fn assert_stats(&self, max_doc: i32) -> Result<()> {
    let size = self.in_.size()?;
    let doc_count = self.in_.get_doc_count()?;
    assert!(size > 0);
    assert!(doc_count > 0);
    assert!(doc_count as usize <= size);
    assert!(doc_count <= max_doc);
    Ok(())
  }
}

impl<PV> PointValues for AssertingPointValues<PV>
where
  PV: PointValues,
{
  fn get_min_packed_value(&self) -> Result<Option<Cow<'_, [u8]>>> {
    assert_thread("Points", self.creation_thread);
    let value = self.in_.get_min_packed_value()?;
    assert!(value.is_some());
    Ok(value)
  }

  fn get_max_packed_value(&self) -> Result<Option<Cow<'_, [u8]>>> {
    assert_thread("Points", self.creation_thread);
    let value = self.in_.get_max_packed_value()?;
    assert!(value.is_some());
    Ok(value)
  }

  fn get_num_dimensions(&self) -> Result<usize> {
    assert_thread("Points", self.creation_thread);
    self.in_.get_num_dimensions()
  }

  fn get_num_index_dimensions(&self) -> Result<usize> {
    assert_thread("Points", self.creation_thread);
    self.in_.get_num_index_dimensions()
  }

  fn get_bytes_per_dimension(&self) -> Result<usize> {
    assert_thread("Points", self.creation_thread);
    self.in_.get_bytes_per_dimension()
  }

  fn size(&self) -> Result<usize> {
    assert_thread("Points", self.creation_thread);
    self.in_.size()
  }

  fn get_doc_count(&self) -> Result<i32> {
    assert_thread("Points", self.creation_thread);
    self.in_.get_doc_count()
  }

  type PointTree = AssertingPointTree<PV::PointTree>;
  type MutablePointTree = AssertingMutablePointTree<PV::MutablePointTree>;

  fn get_point_tree(&self) -> Result<PointTreeEnum<Self>> {
    assert_thread("Points", self.creation_thread);
    let point_tree = self.in_.get_point_tree()?;
    let num_data_dims = self.in_.get_num_dimensions()?;
    let num_index_dims = self.in_.get_num_index_dimensions()?;
    let bytes_per_dim = self.in_.get_bytes_per_dimension()?;
    Ok(match point_tree {
      PointTreeEnum::Mutable(point_tree) => PointTreeEnum::Mutable(AssertingMutablePointTree::new(
        point_tree,
        num_data_dims,
        num_index_dims,
        bytes_per_dim,
      )),
      PointTreeEnum::Other(point_tree) => PointTreeEnum::Other(AssertingPointTree::new(
        point_tree,
        num_data_dims,
        num_index_dims,
        bytes_per_dim,
      )),
    })
  }
}

pub struct AssertingPointTree<PT> {
  in_: PT,
  num_data_dims: usize,
  num_index_dims: usize,
  bytes_per_dim: usize,
}

impl<PT> AssertingPointTree<PT>
where
  PT: PointTree,
{
  fn new(in_: PT, num_data_dims: usize, num_index_dims: usize, bytes_per_dim: usize) -> Self {
    Self {
      in_,
      num_data_dims,
      num_index_dims,
      bytes_per_dim,
    }
  }
}

impl<PT> TryClone for AssertingPointTree<PT>
where
  PT: PointTree,
{
  fn try_clone(&self) -> Result<Self> {
    Ok(Self::new(
      self.in_.try_clone()?,
      self.num_data_dims,
      self.num_index_dims,
      self.bytes_per_dim,
    ))
  }
}

impl<PT> PointTree for AssertingPointTree<PT>
where
  PT: PointTree,
{
  fn move_to_child(&mut self) -> Result<bool> {
    self.in_.move_to_child()
  }

  fn move_to_sibling(&mut self) -> Result<bool> {
    self.in_.move_to_sibling()
  }

  fn move_to_parent(&mut self) -> Result<bool> {
    self.in_.move_to_parent()
  }

  fn get_min_packed_value(&self) -> Result<Cow<'_, [u8]>> {
    self.in_.get_min_packed_value()
  }

  fn get_max_packed_value(&self) -> Result<Cow<'_, [u8]>> {
    self.in_.get_max_packed_value()
  }

  fn size(&self) -> Result<usize> {
    let size = self.in_.size()?;
    assert!(size > 0);
    Ok(size)
  }

  fn visit_doc_ids<IV>(&mut self, visitor: &mut IV) -> Result<()>
  where
    IV: IntersectVisitor,
  {
    self.in_.visit_doc_ids(&mut AssertingIntersectVisitor::new(
      self.num_data_dims,
      self.num_index_dims,
      self.bytes_per_dim,
      visitor,
    ))
  }

  fn visit_doc_values<IV>(&mut self, visitor: &mut IV) -> Result<()>
  where
    IV: IntersectVisitor,
  {
    self
      .in_
      .visit_doc_values(&mut AssertingIntersectVisitor::new(
        self.num_data_dims,
        self.num_index_dims,
        self.bytes_per_dim,
        visitor,
      ))
  }
}

pub struct AssertingMutablePointTree<MPT> {
  in_: MPT,
  num_data_dims: usize,
  num_index_dims: usize,
  bytes_per_dim: usize,
}

impl<MPT> AssertingMutablePointTree<MPT>
where
  MPT: MutablePointTree,
{
  fn new(in_: MPT, num_data_dims: usize, num_index_dims: usize, bytes_per_dim: usize) -> Self {
    Self {
      in_,
      num_data_dims,
      num_index_dims,
      bytes_per_dim,
    }
  }
}

impl<MPT> TryClone for AssertingMutablePointTree<MPT>
where
  MPT: MutablePointTree,
{
  fn try_clone(&self) -> Result<Self> {
    Ok(Self::new(
      self.in_.try_clone()?,
      self.num_data_dims,
      self.num_index_dims,
      self.bytes_per_dim,
    ))
  }
}

impl<MPT> PointTree for AssertingMutablePointTree<MPT>
where
  MPT: MutablePointTree,
{
  fn move_to_child(&mut self) -> Result<bool> {
    self.in_.move_to_child()
  }

  fn move_to_sibling(&mut self) -> Result<bool> {
    self.in_.move_to_sibling()
  }

  fn move_to_parent(&mut self) -> Result<bool> {
    self.in_.move_to_parent()
  }

  fn get_min_packed_value(&self) -> Result<Cow<'_, [u8]>> {
    self.in_.get_min_packed_value()
  }

  fn get_max_packed_value(&self) -> Result<Cow<'_, [u8]>> {
    self.in_.get_max_packed_value()
  }

  fn size(&self) -> Result<usize> {
    let size = self.in_.size()?;
    assert!(size > 0);
    Ok(size)
  }

  fn visit_doc_ids<IV>(&mut self, visitor: &mut IV) -> Result<()>
  where
    IV: IntersectVisitor,
  {
    self.in_.visit_doc_ids(&mut AssertingIntersectVisitor::new(
      self.num_data_dims,
      self.num_index_dims,
      self.bytes_per_dim,
      visitor,
    ))
  }

  fn visit_doc_values<IV>(&mut self, visitor: &mut IV) -> Result<()>
  where
    IV: IntersectVisitor,
  {
    self
      .in_
      .visit_doc_values(&mut AssertingIntersectVisitor::new(
        self.num_data_dims,
        self.num_index_dims,
        self.bytes_per_dim,
        visitor,
      ))
  }
}

impl<MPT> MutablePointTree for AssertingMutablePointTree<MPT>
where
  MPT: MutablePointTree,
{
  fn get_value(&self, i: usize, packed_value: &mut BytesRef<Vec<u8>>) -> Result<()> {
    self.in_.get_value(i, packed_value)
  }

  fn get_byte_at(&self, i: usize, k: usize) -> u8 {
    self.in_.get_byte_at(i, k)
  }

  fn get_doc_id(&self, i: usize) -> Result<i32> {
    self.in_.get_doc_id(i)
  }

  fn swap(&mut self, i: usize, j: usize) {
    self.in_.swap(i, j);
  }

  fn save(&mut self, i: usize, j: usize) {
    self.in_.save(i, j);
  }

  fn restore(&mut self, i: usize, j: usize) {
    self.in_.restore(i, j);
  }
}

/// Validates in the 1D case that all points are visited in order, and point
/// values are in bounds of the last cell checked.
pub struct AssertingIntersectVisitor<'a, IV> {
  in_: &'a mut IV,
  num_data_dims: usize,
  num_index_dims: usize,
  bytes_per_dim: usize,
  last_doc_value: Option<RefCell<Vec<u8>>>,
  last_min_packed_value: RefCell<Vec<u8>>,
  last_max_packed_value: RefCell<Vec<u8>>,
  last_compare_result: Cell<Option<Relation>>,
  last_doc_id: Cell<i32>,
  doc_budget: Cell<i32>,
}

impl<'a, IV> AssertingIntersectVisitor<'a, IV>
where
  IV: IntersectVisitor,
{
  fn new(
    num_data_dims: usize,
    num_index_dims: usize,
    bytes_per_dim: usize,
    in_: &'a mut IV,
  ) -> Self {
    Self {
      in_,
      num_data_dims,
      num_index_dims,
      bytes_per_dim,
      last_doc_value: (num_data_dims == 1).then(|| RefCell::new(vec![0; bytes_per_dim])),
      last_min_packed_value: RefCell::new(vec![0; num_data_dims * bytes_per_dim]),
      last_max_packed_value: RefCell::new(vec![0; num_data_dims * bytes_per_dim]),
      last_compare_result: Cell::new(None),
      last_doc_id: Cell::new(-1),
      doc_budget: Cell::new(0),
    }
  }

  fn decrement_doc_budget(&self) {
    let doc_budget = self.doc_budget.get() - 1;
    self.doc_budget.set(doc_budget);
    assert!(
      doc_budget >= 0,
      "called add() more times than the last call to grow() reserved"
    );
  }
}

impl<IV> IntersectVisitor for AssertingIntersectVisitor<'_, IV>
where
  IV: IntersectVisitor,
{
  fn visit(&mut self, doc_id: i32) -> Result<()> {
    self.decrement_doc_budget();

    // This method, not filtering each hit, should only be invoked when the
    // cell is inside the query shape.
    assert!(
      self.last_compare_result.get().is_none()
        || self.last_compare_result.get() == Some(Relation::CellInsideQuery)
    );
    self.in_.visit(doc_id)
  }

  #[allow(clippy::assertions_on_constants)]
  fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
    self.decrement_doc_budget();

    // This method, to filter each doc's value, should only be invoked when the
    // cell crosses the query shape.
    assert!(
      self.last_compare_result.get().is_none()
        || self.last_compare_result.get() == Some(Relation::CellCrossesQuery)
    );

    if self.last_compare_result.get().is_some() {
      // This doc's packed value should be contained in the last cell passed to
      // compare.
      let last_min_packed_value = self.last_min_packed_value.borrow();
      let last_max_packed_value = self.last_max_packed_value.borrow();
      for dim in 0..self.num_index_dims {
        let start = dim * self.bytes_per_dim;
        let end = start + self.bytes_per_dim;
        assert!(
          last_min_packed_value[start..end] <= packed_value[start..end],
          "dim={} of {} value={:?}",
          dim,
          self.num_data_dims,
          packed_value
        );
        assert!(
          last_max_packed_value[start..end] >= packed_value[start..end],
          "dim={} of {} value={:?}",
          dim,
          self.num_data_dims,
          packed_value
        );
      }
      self.last_compare_result.set(None);
    }

    // TODO: we should assert that this "matches" whatever relation the last
    // call to compare had returned.
    assert_eq!(packed_value.len(), self.num_data_dims * self.bytes_per_dim);
    if let Some(last_doc_value) = self.last_doc_value.as_ref() {
      let comparison = last_doc_value.borrow().as_slice().cmp(packed_value);
      if comparison.is_lt() {
        // ok
      } else if comparison.is_eq() {
        assert!(
          self.last_doc_id.get() <= doc_id,
          "doc ids are out of order when point values are the same!"
        );
      } else {
        assert!(false, "point values are out of order");
      }
      last_doc_value.borrow_mut().copy_from_slice(packed_value);
      self.last_doc_id.set(doc_id);
    }
    self.in_.visit_with_packed_value(doc_id, packed_value)
  }

  fn compare(&self, min_packed_value: &[u8], max_packed_value: &[u8]) -> Result<Relation> {
    for dim in 0..self.num_index_dims {
      let start = dim * self.bytes_per_dim;
      let end = start + self.bytes_per_dim;
      assert!(min_packed_value[start..end] <= max_packed_value[start..end]);
    }
    let index_bytes = self.num_index_dims * self.bytes_per_dim;
    self.last_max_packed_value.borrow_mut()[..index_bytes]
      .copy_from_slice(&max_packed_value[..index_bytes]);
    self.last_min_packed_value.borrow_mut()[..index_bytes]
      .copy_from_slice(&min_packed_value[..index_bytes]);
    let result = self.in_.compare(min_packed_value, max_packed_value)?;
    self.last_compare_result.set(Some(result));
    Ok(result)
  }

  fn grow(&mut self, count: usize) -> Result<()> {
    self.in_.grow(count)?;
    self.doc_budget.set(
      count
        .try_into()
        .map_err(|_| LuceneError::illegal_argument("point visitor budget exceeds i32::MAX"))?,
    );
    Ok(())
  }
}

/// Wraps [`Bits`] with additional assertions.
pub struct AssertingBits<B> {
  creation_thread: ThreadId,
  in_: B,
  identity: Identity,
}

impl<B> AssertingBits<B>
where
  B: Bits,
{
  fn new(in_: B) -> Self {
    Self {
      creation_thread: std::thread::current().id(),
      in_,
      identity: Identity::new(),
    }
  }
}

impl<B> HasIdentity for AssertingBits<B>
where
  B: Bits,
{
  fn identity(&self) -> &Identity {
    &self.identity
  }
}

impl<B> Bits for AssertingBits<B>
where
  B: Bits,
{
  fn get(&self, index: usize) -> Result<bool> {
    assert_thread("Bits", self.creation_thread);
    assert!(index < self.length());
    self.in_.get(index)
  }

  fn length(&self) -> usize {
    assert_thread("Bits", self.creation_thread);
    self.in_.length()
  }
}

impl<LR> IndexReader for AssertingLeafReader<LR>
where
  LR: LeafReader,
{
  type ContextKind = LeafReaderContextKind;

  type TermVectors = AssertingTermVectors<LR::TermVectors>;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    self.ensure_open()?;
    Ok(AssertingTermVectors {
      in_: self.in_.term_vectors()?,
      creation_thread: std::thread::current().id(),
    })
  }

  fn max_doc(&self) -> Result<i32> {
    self.in_.max_doc()
  }

  fn num_docs(&self) -> Result<i32> {
    self.in_.num_docs()
  }

  type StoredFields = AssertingStoredFields<LR::StoredFields>;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    self.ensure_open()?;
    Ok(AssertingStoredFields {
      in_: self.in_.stored_fields()?,
      creation_thread: std::thread::current().id(),
    })
  }

  fn do_close(&self) -> Result<()> {
    self.in_.close()
  }

  type ReaderCacheHelper = LR::ReaderCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    self.in_.get_reader_cache_helper()
  }

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    LeafReader::doc_freq(self, term)
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    self.get_total_term_freq(term)
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    LeafReader::get_sum_doc_freq(self, field)
  }

  fn get_doc_count(&self, field: &str) -> Result<i32> {
    LeafReader::get_doc_count(self, field)
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    LeafReader::get_sum_total_term_freq(self, field)
  }

  fn index_base(&self) -> &IndexReaderBase {
    &self.index_base
  }
}

impl<LR> LeafReader for AssertingLeafReader<LR>
where
  LR: LeafReader,
{
  type CacheHelper = LR::CacheHelper;

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    self.in_.get_core_cache_helper()
  }

  type Terms = AssertingTerms<LR::Terms>;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    self.ensure_open()?;
    Ok(self.in_.terms(field)?.map(AssertingTerms::new))
  }

  type NumericDocValues = AssertingNumericDocValues<LR::NumericDocValues>;

  fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>> {
    self.ensure_open()?;
    let doc_values = self.in_.get_numeric_doc_values(field)?;
    let field_info = self.get_field_infos()?.field_info_by_name(field)?;
    if doc_values.is_some() {
      assert!(field_info.is_some());
      assert_eq!(
        &DocValuesType::Numeric,
        field_info
          .as_ref()
          .expect("numeric doc values require field info")
          .get_doc_values_type()
      );
      let max_doc = self.max_doc()?;
      return Ok(doc_values.map(|doc_values| AssertingNumericDocValues::new(doc_values, max_doc)));
    } else {
      assert!(
        field_info.is_none()
          || field_info
            .as_ref()
            .is_some_and(|info| info.get_doc_values_type() != &DocValuesType::Numeric)
      );
    }
    Ok(None)
  }

  type BinaryDocValues = AssertingBinaryDocValues<LR::BinaryDocValues>;

  fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
    self.ensure_open()?;
    let doc_values = self.in_.get_binary_doc_values(field)?;
    let field_info = self.get_field_infos()?.field_info_by_name(field)?;
    if doc_values.is_some() {
      assert!(field_info.is_some());
      assert_eq!(
        &DocValuesType::Binary,
        field_info
          .as_ref()
          .expect("binary doc values require field info")
          .get_doc_values_type()
      );
      let max_doc = self.max_doc()?;
      return Ok(doc_values.map(|doc_values| AssertingBinaryDocValues::new(doc_values, max_doc)));
    } else {
      assert!(
        field_info.is_none()
          || field_info
            .as_ref()
            .is_some_and(|info| info.get_doc_values_type() != &DocValuesType::Binary)
      );
    }
    Ok(None)
  }

  type SortedDocValues = AssertingSortedDocValues<LR::SortedDocValues>;

  fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
    self.ensure_open()?;
    let doc_values = self.in_.get_sorted_doc_values(field)?;
    let field_info = self.get_field_infos()?.field_info_by_name(field)?;
    if doc_values.is_some() {
      assert!(field_info.is_some());
      assert_eq!(
        &DocValuesType::Sorted,
        field_info
          .as_ref()
          .expect("sorted doc values require field info")
          .get_doc_values_type()
      );
      let max_doc = self.max_doc()?;
      return doc_values
        .map(|doc_values| AssertingSortedDocValues::new(doc_values, max_doc))
        .transpose();
    } else {
      assert!(
        field_info.is_none()
          || field_info
            .as_ref()
            .is_some_and(|info| info.get_doc_values_type() != &DocValuesType::Sorted)
      );
    }
    Ok(None)
  }

  type SortedNumericDocValues = AssertingSortedNumericDocValues<LR::SortedNumericDocValues>;

  fn get_sorted_numeric_doc_values(
    &self,
    field: &str,
  ) -> Result<Option<Self::SortedNumericDocValues>> {
    self.ensure_open()?;
    let doc_values = self.in_.get_sorted_numeric_doc_values(field)?;
    let field_info = self.get_field_infos()?.field_info_by_name(field)?;
    if doc_values.is_some() {
      assert!(field_info.is_some());
      assert_eq!(
        &DocValuesType::SortedNumeric,
        field_info
          .as_ref()
          .expect("sorted numeric doc values require field info")
          .get_doc_values_type()
      );
      let max_doc = self.max_doc()?;
      return doc_values
        .map(|doc_values| AssertingSortedNumericDocValues::create(doc_values, max_doc))
        .transpose();
    } else {
      assert!(
        field_info.is_none()
          || field_info
            .as_ref()
            .is_some_and(|info| info.get_doc_values_type() != &DocValuesType::SortedNumeric)
      );
    }
    Ok(None)
  }

  type SortedSetDocValues = AssertingSortedSetDocValues<LR::SortedSetDocValues>;

  fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>> {
    self.ensure_open()?;
    let doc_values = self.in_.get_sorted_set_doc_values(field)?;
    let field_info = self.get_field_infos()?.field_info_by_name(field)?;
    if doc_values.is_some() {
      assert!(field_info.is_some());
      assert_eq!(
        &DocValuesType::SortedSet,
        field_info
          .as_ref()
          .expect("sorted set doc values require field info")
          .get_doc_values_type()
      );
      let max_doc = self.max_doc()?;
      return doc_values
        .map(|doc_values| AssertingSortedSetDocValues::new(doc_values, max_doc))
        .transpose();
    } else {
      assert!(
        field_info.is_none()
          || field_info
            .as_ref()
            .is_some_and(|info| info.get_doc_values_type() != &DocValuesType::SortedSet)
      );
    }
    Ok(None)
  }

  type NormNumericDocValues = AssertingNumericDocValues<LR::NormNumericDocValues>;

  fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>> {
    self.ensure_open()?;
    let norms = self.in_.get_norm_values(field)?;
    let field_info = self.get_field_infos()?.field_info_by_name(field)?;
    if norms.is_some() {
      assert!(
        field_info
          .as_ref()
          .is_some_and(|field_info| field_info.has_norms())
      );
      let max_doc = self.max_doc()?;
      return Ok(norms.map(|norms| AssertingNumericDocValues::new(norms, max_doc)));
    } else {
      assert!(
        field_info.is_none()
          || field_info
            .as_ref()
            .is_some_and(|field_info| !field_info.has_norms())
      );
    }
    Ok(None)
  }

  type DocValuesSkipper = AssertingDocValuesSkipper<LR::DocValuesSkipper>;

  fn get_doc_values_skipper(&self, field: &str) -> Result<Option<Self::DocValuesSkipper>> {
    self.ensure_open()?;
    let skipper = self.in_.get_doc_values_skipper(field)?;
    let field_info = self.get_field_infos()?.field_info_by_name(field)?;
    if skipper.is_some() {
      assert!(
        field_info
          .as_ref()
          .is_some_and(|info| info.doc_values_skip_index_type() != &DocValuesSkipIndexType::None)
      );
      return Ok(skipper.map(AssertingDocValuesSkipper::new));
    } else {
      assert!(
        field_info.is_none()
          || field_info.as_ref().is_some_and(|info| {
            info.doc_values_skip_index_type() == &DocValuesSkipIndexType::None
          })
      );
    }
    Ok(None)
  }

  type FloatVectorValues = LR::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Option<Self::FloatVectorValues>> {
    self.in_.get_float_vector_values(field)
  }

  type ByteVectorValues = LR::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Option<Self::ByteVectorValues>> {
    self.in_.get_byte_vector_values(field)
  }

  fn search_nearest_vectors_f32<B, K>(
    &self,
    field: &str,
    target: Vec<f32>,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
  {
    self
      .in_
      .search_nearest_vectors_f32(field, target, knn_collector, accept_docs)
  }

  fn search_nearest_vectors_u8<B, K>(
    &self,
    field: &str,
    target: Vec<u8>,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
  {
    self
      .in_
      .search_nearest_vectors_u8(field, target, knn_collector, accept_docs)
  }

  fn get_field_infos(&self) -> Result<Arc<FieldInfos>> {
    self.in_.get_field_infos()
  }

  type Bits = AssertingBits<LR::Bits>;

  fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
    self.ensure_open()?;
    let live_docs = self.in_.get_live_docs()?;
    if let Some(live_docs) = live_docs {
      assert_eq!(self.max_doc()? as usize, live_docs.length());
      Ok(Some(AssertingBits::new(live_docs)))
    } else {
      assert_eq!(self.max_doc()?, self.num_docs()?);
      assert!(!self.has_deletions()?);
      Ok(None)
    }
  }

  type PointValues = AssertingPointValues<LR::PointValues>;

  fn get_point_values(&self, field: &str) -> Result<Option<Self::PointValues>> {
    let values = self.in_.get_point_values(field)?;
    let Some(values) = values else {
      return Ok(None);
    };
    Ok(Some(AssertingPointValues::new(values, self.max_doc()?)?))
  }

  fn check_integrity(&self) -> Result<()> {
    self.ensure_open()?;
    self.in_.check_integrity()
  }

  fn get_metadata(&self) -> Result<&LeafMetaData> {
    self.ensure_open()?;
    self.in_.get_metadata()
  }
}
