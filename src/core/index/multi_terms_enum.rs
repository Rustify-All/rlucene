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
use crate::core::index::base_terms_enum::BaseTermsEnumTermStateImpl;
use crate::core::index::index_reader::Identity;
use crate::core::index::multi_postings_enum::{EnumWithSlice, MultiPostingsEnum};
use crate::core::index::reader_slice::ReaderSlice;
use crate::core::index::slow_impacts_enum::SlowImpactsEnum;
use crate::core::index::terms_enum::{EmptyTermsEnum, SeekStatus, TermsEnum};
use crate::core::index::terms_enum_index::TermsEnumIndex;
use crate::core::index::{BytesRef, BytesRefBuilder};
use crate::core::util::access::ByteSource;
use crate::core::util::array_util::ArrayUtil;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::dummy::dummy_attribute_source::DummyAttributeSource;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::priority_queue::{Compare, PriorityQueue};
use crate::core::util::{Comparator, ToInt};

/// Exposes [`TermsEnum`] API, merged from [`TermsEnum`] API of sub-segments. This does a
/// merge sort, by term text, of the sub-readers.
pub struct MultiTermsEnum<TE> {
  queue: TermMergeQueue<TE>,
  /// Current subs that have at least one term for this field
  // Equal-length current-sub and top-sub index columns.
  sub_indices: Box<[usize]>,
  sub_docs: Box<[EnumWithSlice]>,
  last_seek_exact: bool,
  has_last_seek: bool,
  last_seek_scratch: BytesRefBuilder<Vec<u8>>,
  num_top: usize,
  num_subs: usize,
  current: Option<BytesRef<Vec<u8>>>,
  parent: Identity,
}
impl<TE> MultiTermsEnum<TE>
where
  TE: TermsEnum,
{
  pub fn new<I>(slices: I) -> Result<Self>
  where
    I: IntoIterator<Item = ReaderSlice>,
    I::IntoIter: ExactSizeIterator,
  {
    let slices = slices.into_iter();
    let len = slices.len();
    let sub_indices = vec![0usize; len * 2];
    let mut sub_docs = Vec::with_capacity(len);
    let mut all_terms_enum_with_slice = Vec::with_capacity(len);
    for (i, slice) in slices.enumerate() {
      all_terms_enum_with_slice.push(TermsEnumWithSlice::new(i, slice));
      sub_docs.push(EnumWithSlice::with_slice(slice));
    }
    let queue = TermMergeQueue::new(len, all_terms_enum_with_slice)?;
    Ok(Self {
      queue,
      sub_indices: sub_indices.into_boxed_slice(),
      sub_docs: sub_docs.into_boxed_slice(),
      last_seek_exact: false,
      has_last_seek: false,
      last_seek_scratch: BytesRefBuilder::new(),
      num_top: 0,
      num_subs: 0,
      current: None,
      parent: Identity::new(),
    })
  }

  /// The terms array must contain newly created [`TermsEnum`] instances, meaning
  /// [`BytesRefIterator::next`](crate::core::util::bytes_ref_iterator::BytesRefIterator::next) has
  /// not yet been called.
  pub fn reset(
    mut self,
    terms_enums_index: Vec<TermsEnumIndex<TE>>,
  ) -> Result<MultiTermsEnumType<TE>> {
    debug_assert!(terms_enums_index.len() <= self.sub_docs.len());

    self.num_subs = 0;
    self.num_top = 0;
    self.queue.q.clear();

    for mut terms_enum_index in terms_enums_index.into_iter() {
      if (terms_enum_index.next()?).is_some() {
        let entry_idx = terms_enum_index.sub_index;
        let entry = self.queue.entry_mut(entry_idx);
        entry.base.reset(terms_enum_index);
        self.queue.q.add(entry_idx)?;
        self.sub_indices[self.num_subs] = entry_idx;
        self.num_subs += 1;
      } else {
        // field has no terms
      }
    }

    if self.queue.q.size() == 0 {
      Ok(MultiTermsEnumType::B(EmptyTermsEnum))
    } else {
      Ok(MultiTermsEnumType::A(self))
    }
  }
  fn pull_top(&mut self) -> Result<()> {
    // extract all subs from the queue that have the same
    // top term
    debug_assert_eq!(self.num_top, 0);

    self.num_top = self
      .queue
      .fill_top(&mut self.sub_indices[self.sub_docs.len()..])?;

    let top0_idx = self.sub_indices[self.sub_docs.len()];
    let top0 = self.queue.entry(top0_idx);
    match top0.base.term() {
      Some(term) => self
        .current
        .get_or_insert_with(BytesRef::new)
        .copy_from_slice(&term.bytes[term.offset..term.offset + term.length]),
      None => self.current = None,
    }

    Ok(())
  }
  fn push_top(&mut self) -> Result<()> {
    // call next() on each top, and reorder queue
    for _ in 0..self.num_top {
      let top_idx = *self
        .queue
        .q
        .top()
        .ok_or_else(|| LuceneError::illegal_state("top() returned None"))?;
      let top = self.queue.entry_mut(top_idx);

      if top.base.next()?.is_none() {
        self.queue.q.pop()?;
      } else {
        self.queue.q.update_top()?;
      }
    }
    self.num_top = 0;
    Ok(())
  }
}

impl<TE> BytesRefIterator for MultiTermsEnum<TE>
where
  TE: TermsEnum,
{
  type Value<'a>
    = &'a BytesRef<Vec<u8>>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    if self.last_seek_exact {
      // Must seekCeil at this point, so those subs that
      // didn't have the term can find the following term.
      // NOTE: we could save some CPU by only seekCeil the
      // subs that didn't match the last exact seek... but
      // most impls short-circuit if you seekCeil to term
      // they are already on.
      let current = self
        .current
        .take()
        .ok_or_else(|| LuceneError::illegal_state("current is None but last_seek_exact=true"))?;
      let status = match self.seek_ceil(&current) {
        Ok(status) => status,
        Err(error) => {
          if self.current.is_none() {
            self.current = Some(current);
          }
          return Err(error);
        },
      };
      debug_assert_eq!(status, SeekStatus::Found);
      self.last_seek_exact = false;
    }

    self.has_last_seek = false;

    // restore queue
    self.push_top()?;

    // gather equal top fields
    if self.queue.q.size() > 0 {
      self.pull_top()?;
    } else {
      self.current = None;
    }
    match self.current {
      None => Ok(None),
      Some(ref v) => Ok(Some(v)),
    }
  }
}

impl<TE> TermsEnum for MultiTermsEnum<TE>
where
  TE: TermsEnum,
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
    Err(LuceneError::unsupported_operation(""))
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool> {
    self.queue.q.clear();
    self.num_top = 0;

    let mut seek_opt = false;
    if self.has_last_seek
      && self
        .last_seek_scratch
        .get_bytes_ref()
        .compare_to(term)
        .to_int()
        <= 0
    {
      seek_opt = true;
    }

    self.has_last_seek = false;
    self.last_seek_exact = true;

    for i in 0..self.num_subs {
      let entry_idx = self.sub_indices[i];
      let status: bool;

      // LUCENE-2130: if we had just seek'd already, prior
      // to this seek, and the new seek term is after the
      // previous one, don't try to re-seek this sub if its
      // current term is already beyond this new seek term.
      // Doing so is a waste because this sub will simply
      // seek to the same spot.
      if seek_opt {
        let entry = self.queue.entry_mut(entry_idx);
        let cur_term = entry.base.term();

        if let Some(cur) = cur_term {
          let cmp = term.compare_to(cur).to_int();
          if cmp == 0 {
            status = true;
          } else if cmp < 0 {
            status = false;
          } else {
            status = entry.base.seek_exact(term)?;
          }
        } else {
          status = false;
        }
      } else {
        let entry = self.queue.entry_mut(entry_idx);
        status = entry.base.seek_exact(term)?;
      }

      if status {
        self.sub_indices[self.sub_docs.len() + self.num_top] = entry_idx;
        self.num_top += 1;

        let cur = {
          let entry = self.queue.entry_mut(entry_idx);
          entry.base.term()
        };
        match cur {
          Some(term) => self
            .current
            .get_or_insert_with(BytesRef::new)
            .copy_from_slice(&term.bytes[term.offset..term.offset + term.length]),
          None => self.current = None,
        }

        debug_assert!({
          let t = {
            let entry = self.queue.entry_mut(entry_idx);
            entry.base.term()
          };
          match t {
            Some(v) => term.compare_to(v) == std::cmp::Ordering::Equal,
            None => false,
          }
        });
      }
    }
    // if at least one sub had exact match to the requested
    // term then we found match
    Ok(self.num_top > 0)
  }

  fn prepare_seek_exact<BS: ByteSource>(&mut self, _text: &BytesRef<BS>) -> Result<Option<()>> {
    Ok(Some(()))
  }

  fn get_prepare_seek_exact_status<BS: ByteSource>(
    &mut self,
    target: &BytesRef<BS>,
  ) -> Result<bool> {
    self.seek_exact(target)
  }

  fn seek_ceil<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<SeekStatus> {
    self.queue.q.clear();
    self.num_top = 0;
    self.last_seek_exact = false;

    let mut seek_opt = false;
    if self.has_last_seek
      && self
        .last_seek_scratch
        .get_bytes_ref()
        .compare_to(term)
        .to_int()
        <= 0
    {
      seek_opt = true;
    }

    self.last_seek_scratch.copy_bytes_from_ref(term)?;
    self.has_last_seek = true;

    for i in 0..self.num_subs {
      let entry_idx = self.sub_indices[i];
      let status: SeekStatus;

      // LUCENE-2130: if we had just seek'd already, prior
      // to this seek, and the new seek term is after the
      // previous one, don't try to re-seek this sub if its
      // current term is already beyond this new seek term.
      // Doing so is a waste because this sub will simply
      // seek to the same spot.
      if seek_opt {
        let entry = self.queue.entry_mut(entry_idx);
        let cur_term = entry.base.term();

        if let Some(cur) = cur_term {
          let cmp = term.compare_to(cur).to_int();
          if cmp == 0 {
            status = SeekStatus::Found;
          } else if cmp < 0 {
            status = SeekStatus::NotFound;
          } else {
            status = entry.base.seek_ceil(term)?;
          }
        } else {
          status = SeekStatus::End;
        }
      } else {
        let entry = self.queue.entry_mut(entry_idx);
        status = entry.base.seek_ceil(term)?;
      }

      if status == SeekStatus::Found {
        self.sub_indices[self.sub_docs.len() + self.num_top] = entry_idx;
        self.num_top += 1;

        let cur = {
          let entry = self.queue.entry_mut(entry_idx);
          entry.base.term()
        };
        match cur {
          Some(term) => self
            .current
            .get_or_insert_with(BytesRef::new)
            .copy_from_slice(&term.bytes[term.offset..term.offset + term.length]),
          None => self.current = None,
        }

        self.queue.q.add(entry_idx)?;
      } else if status == SeekStatus::NotFound {
        debug_assert!({
          let entry = self.queue.entry_mut(entry_idx);
          entry.base.term().is_some()
        });
        self.queue.q.add(entry_idx)?;
      } else {
        debug_assert_eq!(status, SeekStatus::End);
      }
    }

    if self.num_top > 0 {
      // at least one sub had exact match to the requested term
      Ok(SeekStatus::Found)
    } else if self.queue.q.size() > 0 {
      // no sub had exact match, but at least one sub found
      // a term after the requested term -- advance to that
      // next term:
      self.pull_top()?;
      Ok(SeekStatus::NotFound)
    } else {
      Ok(SeekStatus::End)
    }
  }

  fn seek_exact_with_ord(&mut self, _ord: i64) -> Result<()> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    _state: &TermStateEnum,
  ) -> Result<()> {
    if !self.seek_exact(term)? {
      return Err(LuceneError::illegal_state(format!(
        "term {} does not exist",
        term
      )));
    }
    Ok(())
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    match self.current {
      None => Err(LuceneError::illegal_state("current is None in term() call")),
      Some(ref v) => Ok(v),
    }
  }

  fn ord(&self) -> Result<i64> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn doc_freq(&mut self) -> Result<i32> {
    let mut sum: i32 = 0;
    for i in 0..(self.num_top) {
      let idx = self.sub_indices[self.sub_docs.len() + i];
      let entry = self.queue.entry_mut(idx);
      match entry.base.terms_enum {
        Some(ref mut terms_enum) => {
          sum += terms_enum.doc_freq()?;
        },
        None => return Err(LuceneError::illegal_state("terms_enum is None")),
      }
    }
    Ok(sum)
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    let mut sum: i64 = 0;
    for i in 0..(self.num_top) {
      let idx = self.sub_indices[self.sub_docs.len() + i];
      let entry = self.queue.entry_mut(idx);
      match entry.base.terms_enum {
        Some(ref mut terms_enum) => {
          let v = terms_enum.total_term_freq()?;
          debug_assert!(v != -1);
          sum += v;
        },
        None => return Err(LuceneError::illegal_state("terms_enum is None")),
      }
    }
    Ok(sum)
  }

  type PostingsEnum = MultiPostingsEnum<TE::PostingsEnum>;

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    let mut docs_enum = match reuse {
      Some(reuse) => {
        if reuse.can_reuse(&self.parent) {
          reuse
        } else {
          MultiPostingsEnum::new(self.parent.clone(), self.sub_docs.len())
        }
      },
      None => MultiPostingsEnum::new(self.parent.clone(), self.sub_docs.len()),
    };
    let mut upto: usize = 0;
    let cmp = TopTermsEnumWithSliceCmp::new(self.queue.entries());
    ArrayUtil::do_tim_sort(
      &mut self.sub_indices[self.sub_docs.len()..],
      0,
      self.num_top,
      cmp,
    )?;

    for i in 0..(self.num_top) {
      let entry_idx = self.sub_indices[self.sub_docs.len() + i];
      let entry = self.queue.entry_mut(entry_idx);

      let sub_index = entry.base.sub_index;
      debug_assert!(
        sub_index < docs_enum.sub_postings_enums.len(),
        "{} vs {}; {}",
        sub_index,
        docs_enum.sub_postings_enums.len(),
        self.sub_docs.len()
      );

      let terms_enum = entry
        .base
        .terms_enum
        .as_mut()
        .ok_or_else(|| LuceneError::illegal_state("terms enum is not initialized"))?;
      let sub_postings_enum =
        terms_enum.postings_with_flags(docs_enum.sub_postings_enums[sub_index].take(), flags)?;
      docs_enum.sub_postings_enums[sub_index] = Some(sub_postings_enum);
      self.sub_docs[upto].postings_enum_idx = sub_index;
      self.sub_docs[upto].slice = entry.sub_slice;
      upto += 1;
    }
    docs_enum.reset(&self.sub_docs, upto);
    Ok(docs_enum)
  }

  type ImpactsEnum = SlowImpactsEnum<MultiPostingsEnum<TE::PostingsEnum>>;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    Ok(SlowImpactsEnum::new(self.postings_with_flags(None, flags)?))
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    Ok(BaseTermsEnumTermStateImpl.into())
  }
}
struct TopTermsEnumWithSliceCmp<'a, TE> {
  terms_enums: &'a [TermsEnumWithSlice<TE>],
}
impl<'a, TE> TopTermsEnumWithSliceCmp<'a, TE>
where
  TE: TermsEnum,
{
  pub fn new(terms_enums: &'a [TermsEnumWithSlice<TE>]) -> Self {
    Self { terms_enums }
  }
}
impl<TE> Comparator<usize> for TopTermsEnumWithSliceCmp<'_, TE>
where
  TE: TermsEnum,
{
  const TYPE: &'static str = "TopTermsEnumWithSliceCmp";

  fn compare(&self, a: &usize, b: &usize) -> Result<i32> {
    let va = self.terms_enums[*a].base.sub_index;
    let vb = self.terms_enums[*b].base.sub_index;
    Ok(va.cmp(&vb).to_int())
  }
}

#[allow(clippy::large_enum_variant)] // Keep multi-segment terms iteration allocation-free.
pub enum MultiTermsEnumType<TE>
where
  TE: TermsEnum,
{
  A(MultiTermsEnum<TE>),
  B(EmptyTermsEnum),
}

impl<TE> BytesRefIterator for MultiTermsEnumType<TE>
where
  TE: TermsEnum,
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

impl<TE> TermsEnum for MultiTermsEnumType<TE>
where
  TE: TermsEnum,
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

  fn prepare_seek_exact<BS: ByteSource>(&mut self, text: &BytesRef<BS>) -> Result<Option<()>> {
    match self {
      Self::A(terms) => terms.prepare_seek_exact(text),
      Self::B(terms) => terms.prepare_seek_exact(text),
    }
  }

  fn get_prepare_seek_exact_status<BS: ByteSource>(
    &mut self,
    target: &BytesRef<BS>,
  ) -> Result<bool> {
    match self {
      Self::A(terms) => terms.get_prepare_seek_exact_status(target),
      Self::B(terms) => terms.get_prepare_seek_exact_status(target),
    }
  }

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

  type PostingsEnum = <MultiTermsEnum<TE> as TermsEnum>::PostingsEnum;

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    match self {
      Self::A(terms) => terms.postings_with_flags(reuse, flags),
      Self::B(_) => Err(LuceneError::illegal_state(
        "this method should never be called",
      )),
    }
  }

  type ImpactsEnum = <MultiTermsEnum<TE> as TermsEnum>::ImpactsEnum;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    match self {
      Self::A(terms) => terms.impacts(flags),
      Self::B(_) => Err(LuceneError::illegal_state(
        "this method should never be called",
      )),
    }
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    match self {
      Self::A(terms) => terms.term_state(),
      Self::B(terms) => terms.term_state(),
    }
  }
}

struct TermsEnumWithSlice<TE> {
  base: TermsEnumIndex<TE>,
  sub_slice: ReaderSlice,
}
impl<TE> TermsEnumWithSlice<TE>
where
  TE: TermsEnum,
{
  pub fn new(index: usize, sub_slice: ReaderSlice) -> Self {
    debug_assert!(sub_slice.length >= 0, "length={}", sub_slice.length);

    Self {
      base: TermsEnumIndex::new(None, index),
      sub_slice,
    }
  }
}

struct TermMergeQueue<TE> {
  // PriorityQueue limits size below i32::MAX; binary DFS needs at most 31 pending nodes.
  stack: [usize; 32],
  q: PriorityQueue<usize, TermMergeQueueCmp<TE>>,
}
impl<TE> TermMergeQueue<TE>
where
  TE: TermsEnum,
{
  pub fn new(size: usize, all_terms_enum_with_slice: Vec<TermsEnumWithSlice<TE>>) -> Result<Self> {
    let cmp = TermMergeQueueCmp::new(all_terms_enum_with_slice);
    let queue = PriorityQueue::new(size, cmp)?;
    Ok(Self {
      stack: [0; 32],
      q: queue,
    })
  }

  fn entry(&self, index: usize) -> &TermsEnumWithSlice<TE> {
    &self.q.compare.all_terms_enum_with_slice[index]
  }

  fn entry_mut(&mut self, index: usize) -> &mut TermsEnumWithSlice<TE> {
    &mut self.q.compare.all_terms_enum_with_slice[index]
  }

  fn entries(&self) -> &[TermsEnumWithSlice<TE>] {
    &self.q.compare.all_terms_enum_with_slice
  }

  /// Add the top() slice as well as all slices that are positionned on the same term to tops and return how many of them there are.
  pub(crate) fn fill_top(&mut self, tops: &mut [usize]) -> Result<usize> {
    let size = self.q.size();
    if size == 0 {
      return Ok(0);
    }

    tops[0] = *self
      .q
      .top()
      .ok_or_else(|| LuceneError::illegal_state("top() returned None"))?;
    let mut num_top: usize = 1;
    self.stack[0] = 1;
    let mut stack_len: usize = 1;

    while stack_len != 0 {
      stack_len -= 1;
      let index = self.stack[stack_len];

      let left_child = index << 1;
      let end = std::cmp::min(size, left_child + 1);

      for child in left_child..=end {
        let te_idx = self.get(child)?;
        let top0_idx = tops[0];

        let cmp = {
          let te = self.entry(te_idx);
          let top0 = self.entry(top0_idx);
          te.base.compare_term_to(&top0.base)?
        };

        if cmp == 0 {
          tops[num_top] = te_idx;
          num_top += 1;

          self.stack[stack_len] = child;
          stack_len += 1;
        }
      }
    }
    Ok(num_top)
  }
  fn get(&self, i: usize) -> Result<usize> {
    self.q.get_heap_array()[i]
      .ok_or_else(|| LuceneError::illegal_state("get_heap_array() returned None"))
  }
}
struct TermMergeQueueCmp<TE> {
  all_terms_enum_with_slice: Box<[TermsEnumWithSlice<TE>]>,
}
impl<TE> TermMergeQueueCmp<TE>
where
  TE: TermsEnum,
{
  fn new(all_terms_enum_with_slice: Vec<TermsEnumWithSlice<TE>>) -> Self {
    Self {
      all_terms_enum_with_slice: all_terms_enum_with_slice.into_boxed_slice(),
    }
  }
}
impl<TE> Compare<usize> for TermMergeQueueCmp<TE>
where
  TE: TermsEnum,
{
  fn less_than(&self, a: &usize, b: &usize) -> Result<bool> {
    Ok(
      self.all_terms_enum_with_slice[*a]
        .base
        .compare_term_to(&self.all_terms_enum_with_slice[*b].base)?
        < 0,
    )
  }
}
