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
use crate::core::index::BytesRef;
use crate::core::index::index_reader::{IndexReader, IndexReaderContextType};
use crate::core::index::index_reader_context::{IRCLeafReader, IndexReaderContext};
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::multi_terms_enum::{MultiTermsEnum, MultiTermsEnumType};
use crate::core::index::postings_enum::ALL;
use crate::core::index::reader_slice::ReaderSlice;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{
  EmptyTermsEnum, TermsEnum, TermsEnumWithUnsupportedSecondAttributes2,
};
use crate::core::index::terms_enum_index::TermsEnumIndex;
use crate::core::util::ToInt;
use crate::core::util::automation::compiled_automaton::CompiledAutomaton;
use crate::core::util::error::lucene_error::Result;
use std::borrow::Cow;

/// Exposes flex API, merged from flex API of sub-segments.
pub struct MultiTerms<T> {
  subs: Box<[T]>,
  sub_slices: Box<[ReaderSlice]>,
  has_freqs: bool,
  has_offsets: bool,
  has_positions: bool,
  has_payloads: bool,
}
impl<T> MultiTerms<T>
where
  T: Terms,
{
  /// Creates a new instance. Use [`get_terms`] instead if possible.
  ///
  /// # Parameters
  /// * `subs` – The [`Terms`] instances of all sub-readers.
  /// * `sub_slices` – A parallel array (matching `subs`) describing the sub-reader slices.
  pub fn new(subs: Vec<T>, sub_slices: Vec<ReaderSlice>) -> Result<Self> {
    debug_assert!(
      !subs.is_empty(),
      "inefficient: don't use MultiTerms over one sub"
    );

    let mut has_freqs = true;
    let mut has_offsets = true;
    let mut has_positions = true;
    let mut has_payloads_any = false;

    for t in &subs {
      has_freqs &= t.has_freqs();
      has_offsets &= t.has_offsets();
      has_positions &= t.has_positions();
      has_payloads_any |= t.has_payloads();
    }

    // if all subs have pos, and at least one has payloads
    let has_payloads = has_positions && has_payloads_any;

    Ok(Self {
      subs: subs.into_boxed_slice(),
      sub_slices: sub_slices.into_boxed_slice(),
      has_freqs,
      has_offsets,
      has_positions,
      has_payloads,
    })
  }
}
pub type IntersectIterType<T> = MultiTermsEnumType<<T as Terms>::IntersectIter>;
pub type IteratorType<T> = MultiTermsEnumType<<T as Terms>::TermsEnum>;
impl<T> Terms for MultiTerms<T>
where
  T: Terms,
{
  type TermsEnum = IteratorType<T>;

  fn iterator(&self) -> Result<Self::TermsEnum> {
    let mut terms_enums = Vec::with_capacity(self.subs.len());

    let mut i = 0;
    while i < self.subs.len() {
      let sub = &self.subs[i];
      let terms_enum = sub.iterator()?;
      terms_enums.push(TermsEnumIndex::new(Some(terms_enum), i));
      i += 1;
    }

    if !terms_enums.is_empty() {
      let v = MultiTermsEnum::new(self.sub_slices.iter().copied())?;
      v.reset(terms_enums)
    } else {
      Ok(MultiTermsEnumType::B(EmptyTermsEnum))
    }
  }

  type IntersectIter = IntersectIterType<T>;

  fn intersect(
    &self,
    compiled: &CompiledAutomaton,
    start_term: Option<&BytesRef<Vec<u8>>>,
  ) -> Result<Self::IntersectIter> {
    let mut terms_enums = Vec::with_capacity(self.subs.len());

    let mut i = 0;
    while i < self.subs.len() {
      let sub = &self.subs[i];
      let terms_enum = sub.intersect(compiled, start_term)?;
      terms_enums.push(TermsEnumIndex::new(Some(terms_enum), i));
      i += 1;
    }
    if !terms_enums.is_empty() {
      let v = MultiTermsEnum::new(self.sub_slices.iter().copied())?;
      v.reset(terms_enums)
    } else {
      Ok(MultiTermsEnumType::B(EmptyTermsEnum))
    }
  }

  fn size(&self) -> Result<i64> {
    Ok(-1)
  }

  fn get_sum_total_term_freq(&self) -> Result<i64> {
    let mut sum = 0i64;
    for terms in &self.subs {
      let v = terms.get_sum_total_term_freq()?;
      debug_assert!(v != -1);
      sum += v;
    }
    Ok(sum)
  }

  fn get_sum_doc_freq(&self) -> Result<i64> {
    let mut sum = 0i64;
    for terms in &self.subs {
      let v = terms.get_sum_doc_freq()?;
      debug_assert!(v != -1);
      sum += v;
    }
    Ok(sum)
  }

  fn get_doc_count(&self) -> Result<i32> {
    let mut sum = 0;
    for terms in &self.subs {
      let v = terms.get_doc_count()?;
      debug_assert!(v != -1);
      sum += v;
    }
    Ok(sum)
  }

  fn has_freqs(&self) -> bool {
    self.has_freqs
  }

  fn has_offsets(&self) -> bool {
    self.has_offsets
  }

  fn has_positions(&self) -> bool {
    self.has_positions
  }

  fn has_payloads(&self) -> bool {
    self.has_payloads
  }

  fn get_min(&self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
    let mut min_term = None;

    for terms in &self.subs {
      if let Some(term) = terms.get_min()? {
        match &min_term {
          None => min_term = Some(term),
          Some(cur) => {
            if term.as_ref().cmp(cur.as_ref()).to_int() < 0 {
              min_term = Some(term);
            }
          },
        }
      }
    }

    Ok(min_term)
  }

  fn get_max(&self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
    let mut max_term = None;

    for terms in &self.subs {
      if let Some(term) = terms.get_max()? {
        match &max_term {
          None => max_term = Some(term),
          Some(cur) => {
            if term.as_ref().cmp(cur.as_ref()).to_int() > 0 {
              max_term = Some(term);
            }
          },
        }
      }
    }

    Ok(max_term)
  }
}

pub enum MultiTermsType<T> {
  A(T),
  B(MultiTerms<T>),
}

impl<T> Terms for MultiTermsType<T>
where
  T: Terms,
{
  type TermsEnum =
    TermsEnumWithUnsupportedSecondAttributes2<T::TermsEnum, <MultiTerms<T> as Terms>::TermsEnum>;
  type IntersectIter = TermsEnumWithUnsupportedSecondAttributes2<
    T::IntersectIter,
    <MultiTerms<T> as Terms>::IntersectIter,
  >;

  fn iterator(&self) -> Result<Self::TermsEnum> {
    match self {
      Self::A(terms) => terms
        .iterator()
        .map(TermsEnumWithUnsupportedSecondAttributes2::WithAttributes),
      Self::B(terms) => terms
        .iterator()
        .map(TermsEnumWithUnsupportedSecondAttributes2::WithoutAttributes),
    }
  }

  fn intersect(
    &self,
    compiled: &CompiledAutomaton,
    start_term: Option<&BytesRef<Vec<u8>>>,
  ) -> Result<Self::IntersectIter> {
    match self {
      Self::A(terms) => terms
        .intersect(compiled, start_term)
        .map(TermsEnumWithUnsupportedSecondAttributes2::WithAttributes),
      Self::B(terms) => terms
        .intersect(compiled, start_term)
        .map(TermsEnumWithUnsupportedSecondAttributes2::WithoutAttributes),
    }
  }

  fn size(&self) -> Result<i64> {
    match self {
      Self::A(terms) => terms.size(),
      Self::B(terms) => terms.size(),
    }
  }

  fn get_sum_total_term_freq(&self) -> Result<i64> {
    match self {
      Self::A(terms) => terms.get_sum_total_term_freq(),
      Self::B(terms) => terms.get_sum_total_term_freq(),
    }
  }

  fn get_sum_doc_freq(&self) -> Result<i64> {
    match self {
      Self::A(terms) => terms.get_sum_doc_freq(),
      Self::B(terms) => terms.get_sum_doc_freq(),
    }
  }

  fn get_doc_count(&self) -> Result<i32> {
    match self {
      Self::A(terms) => terms.get_doc_count(),
      Self::B(terms) => terms.get_doc_count(),
    }
  }

  fn has_freqs(&self) -> bool {
    match self {
      Self::A(terms) => terms.has_freqs(),
      Self::B(terms) => terms.has_freqs(),
    }
  }

  fn has_offsets(&self) -> bool {
    match self {
      Self::A(terms) => terms.has_offsets(),
      Self::B(terms) => terms.has_offsets(),
    }
  }

  fn has_positions(&self) -> bool {
    match self {
      Self::A(terms) => terms.has_positions(),
      Self::B(terms) => terms.has_positions(),
    }
  }

  fn has_payloads(&self) -> bool {
    match self {
      Self::A(terms) => terms.has_payloads(),
      Self::B(terms) => terms.has_payloads(),
    }
  }

  fn get_min(&self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
    match self {
      Self::A(terms) => terms.get_min(),
      Self::B(terms) => terms.get_min(),
    }
  }

  fn get_max(&self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
    match self {
      Self::A(terms) => terms.get_max(),
      Self::B(terms) => terms.get_max(),
    }
  }

  fn get_stats(&self) -> Result<String> {
    match self {
      Self::A(terms) => terms.get_stats(),
      Self::B(terms) => terms.get_stats(),
    }
  }
}

pub type TermsType<IR> =
  MultiTermsType<<IRCLeafReader<IndexReaderContextType<IR>> as LeafReader>::Terms>;
pub type TermsPostingType<IR> = <<TermsType<IR> as Terms>::TermsEnum as TermsEnum>::PostingsEnum;
/// This method may return `None` if the field does not exist or if it has no terms.
pub fn get_terms<IR>(reader: IR, field: &str) -> Result<Option<TermsType<IR>>>
where
  IR: IndexReader,
{
  let max_doc = reader.max_doc()?;
  let reader = reader.get_context()?;
  let leaves = reader.leaves()?;

  if leaves.len() == 1 {
    return match leaves[0].reader().terms(field)? {
      Some(terms) => Ok(Some(MultiTermsType::A(terms))),
      None => return Ok(None),
    };
  }

  let mut terms_per_leaf = Vec::with_capacity(leaves.len());
  let mut slice_per_leaf = Vec::with_capacity(leaves.len());

  for (leaf_idx, ctx) in leaves.iter().enumerate() {
    if let Some(sub_terms) = ctx.reader().terms(field)? {
      terms_per_leaf.push(sub_terms);
      slice_per_leaf.push(ReaderSlice::new(ctx.doc_base, max_doc, leaf_idx));
    }
  }

  if terms_per_leaf.is_empty() {
    Ok(None)
  } else {
    Ok(Some(MultiTermsType::B(MultiTerms::new(
      terms_per_leaf,
      slice_per_leaf,
    )?)))
  }
}
/// Returns [`PostingsEnum`](crate::core::index::postings_enum::PostingsEnum) for the specified field and term.
///
/// This returns `None` if the field or term does not exist, or if positions were not indexed.
///
/// See [`get_term_postings_enum_with_flag`] to control the requested postings features.
pub fn get_term_postings_enum<IR>(
  reader: IR,
  field: &str,
  term: &BytesRef<Vec<u8>>,
) -> Result<Option<TermsPostingType<IR>>>
where
  IR: IndexReader,
{
  get_term_postings_enum_with_flag(reader, field, term, ALL as i32)
}

/// Returns [`PostingsEnum`](crate::core::index::postings_enum::PostingsEnum) for the specified field and term, with control over whether freqs,
/// positions, offsets or payloads are required.
///
/// This returns `None` if the field or term does not exist.
/// See [`TermsEnum::postings`].
pub fn get_term_postings_enum_with_flag<IR>(
  reader: IR,
  field: &str,
  term: &BytesRef<Vec<u8>>,
  flags: i32,
) -> Result<Option<TermsPostingType<IR>>>
where
  IR: IndexReader,
{
  if let Some(terms) = get_terms(reader, field)? {
    let mut terms_enum = terms.iterator()?;
    if terms_enum.seek_exact(term)? {
      return Ok(Some(terms_enum.postings_with_flags(None, flags)?));
    }
  }
  Ok(None)
}
