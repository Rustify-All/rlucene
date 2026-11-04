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
use crate::core::index::bytes_ref::BytesRefValueEnum;
use crate::core::index::index_reader::Identity;
use crate::core::index::postings_enum::PostingsEnum;
use crate::core::index::reader_slice::ReaderSlice;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::fmt::{Display, Formatter};
/// Exposes [`PostingsEnum`], merged from [`PostingsEnum`] API of sub-segments.
pub struct MultiPostingsEnum<PE> {
  parent: Identity,
  pub(crate) sub_postings_enums: Box<[Option<PE>]>,
  subs: smallvec::SmallVec<[EnumWithSlice; 4]>,
  num_subs: usize,
  upto: Option<usize>,
  current: Option<usize>,
  current_base: i32,
  doc: i32,
}
impl<PE> MultiPostingsEnum<PE> {
  pub fn new(parent: Identity, sub_reader_count: usize) -> Self {
    let subs = smallvec::smallvec![EnumWithSlice::new(); sub_reader_count];
    let mut sub_postings_enums = Vec::with_capacity(sub_reader_count);
    for _ in 0..sub_reader_count {
      sub_postings_enums.push(None);
    }
    Self {
      parent,
      sub_postings_enums: sub_postings_enums.into_boxed_slice(),
      subs,
      num_subs: 0,
      upto: None,
      current: None,
      current_base: 0,
      doc: -1,
    }
  }
  /// Returns `true` if this instance can be reused by the provided `MultiTermsEnum`.
  pub fn can_reuse(&self, other: &Identity) -> bool {
    self.parent == *other
  }
  /// Re-use and reset this instance on the provided slices.
  pub fn reset(&mut self, subs: &[EnumWithSlice], num_subs: usize) {
    self.num_subs = num_subs;

    for (i, sub) in subs.iter().enumerate().take(self.num_subs) {
      self.subs[i].postings_enum_idx = sub.postings_enum_idx;
      self.subs[i].slice = sub.slice;
    }

    self.upto = None;
    self.doc = -1;
    self.current = None;
  }

  /// How many sub-readers we are merging.
  pub fn get_num_subs(&self) -> usize {
    self.num_subs
  }

  /// Returns sub-readers we are merging.
  pub fn get_subs(&self) -> &[EnumWithSlice] {
    &self.subs
  }
  pub fn postings_enums_mut(&mut self) -> &mut [Option<PE>] {
    self.sub_postings_enums.as_mut()
  }

  fn postings_enum_mut(&mut self, idx: usize) -> Result<&mut PE> {
    self
      .sub_postings_enums
      .get_mut(idx)
      .and_then(Option::as_mut)
      .ok_or_else(|| LuceneError::illegal_state(format!("PostingsEnum {idx} is not set")))
  }

  fn postings_enum_ref(&self, idx: usize) -> Result<&PE> {
    self
      .sub_postings_enums
      .get(idx)
      .and_then(Option::as_ref)
      .ok_or_else(|| LuceneError::illegal_state(format!("PostingsEnum {idx} is not set")))
  }

  fn current_postings_mut(&mut self) -> Result<&mut PE> {
    let current = self
      .current
      .ok_or_else(|| LuceneError::illegal_state("No current sub PostingsEnum"))?;
    let pe_idx = self
      .subs
      .get(current)
      .ok_or_else(|| LuceneError::illegal_state("Current postings sub is missing"))?
      .postings_enum_idx;
    self.postings_enum_mut(pe_idx)
  }

  fn current_postings_ref(&self) -> Result<&PE> {
    let current = self
      .current
      .ok_or_else(|| LuceneError::illegal_state("No current sub PostingsEnum"))?;
    let pe_idx = self
      .subs
      .get(current)
      .ok_or_else(|| LuceneError::illegal_state("Current postings sub is missing"))?
      .postings_enum_idx;
    self.postings_enum_ref(pe_idx)
  }
}

impl<PE> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for MultiPostingsEnum<PE>
where
  PE: PostingsEnum,
{
}
impl<PE> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for MultiPostingsEnum<PE> where
  PE: PostingsEnum
{
}

impl<PE> DocIdSetIterator for MultiPostingsEnum<PE>
where
  PE: PostingsEnum,
{
  fn doc_id(&self) -> i32 {
    self.doc
  }

  fn next_doc(&mut self) -> Result<i32> {
    loop {
      let current = if let Some(current) = self.current {
        current
      } else {
        if self.upto.map_or(0, |upto| upto + 1) == self.num_subs {
          self.doc = NO_MORE_DOCS;
          return Ok(self.doc);
        } else {
          let idx = self.upto.map_or(0, |upto| upto + 1);
          self.upto = Some(idx);
          self.current = Some(idx);
          self.current_base = self.subs[idx].slice.get_start() as i32;
          idx
        }
      };

      let idx = self.subs[current].postings_enum_idx;
      let doc = self.postings_enum_mut(idx)?.next_doc()?;
      if doc != NO_MORE_DOCS {
        self.doc = self.current_base + doc;
        return Ok(self.doc);
      } else {
        self.current = None;
      }
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    debug_assert!(target > self.doc);
    loop {
      if let Some(idx) = self.current {
        let pe_idx = self.subs[idx].postings_enum_idx;
        let doc = if target < self.current_base {
          // target was in the previous slice but there was no matching doc after it
          self.postings_enum_mut(pe_idx)?.next_doc()?
        } else {
          let target = target - self.current_base;
          self.postings_enum_mut(pe_idx)?.advance(target)?
        };

        if doc == NO_MORE_DOCS {
          self.current = None;
        } else {
          self.doc = doc + self.current_base;
          return Ok(self.doc);
        }
      } else if self.upto.map_or(0, |upto| upto + 1) == self.num_subs {
        self.doc = NO_MORE_DOCS;
        return Ok(self.doc);
      } else {
        let idx = self.upto.map_or(0, |upto| upto + 1);
        self.upto = Some(idx);
        self.current = Some(idx);
        self.current_base = self.subs[idx].slice.get_start() as i32;
      }
    }
  }

  fn cost(&self) -> Result<i64> {
    let mut cost: i64 = 0;
    for i in 0..(self.num_subs) {
      let pe_idx = self.subs[i].postings_enum_idx;
      cost += self.postings_enum_ref(pe_idx)?.cost()?;
    }
    Ok(cost)
  }
}

impl<PE> PostingsEnum for MultiPostingsEnum<PE>
where
  PE: PostingsEnum,
{
  fn freq(&mut self) -> Result<i32> {
    self.current_postings_mut()?.freq()
  }

  fn next_position(&mut self) -> Result<i32> {
    self.current_postings_mut()?.next_position()
  }

  fn start_offset(&self) -> Result<i32> {
    self.current_postings_ref()?.start_offset()
  }

  fn end_offset(&self) -> Result<i32> {
    self.current_postings_ref()?.end_offset()
  }

  fn get_payload(&self) -> Result<Option<BytesRefValueEnum<'_>>> {
    self.current_postings_ref()?.get_payload()
  }
}
impl<PE> Display for MultiPostingsEnum<PE> {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    let mut first = true;
    for sub in self.get_subs().iter() {
      if !first {
        write!(f, ", ")?;
      }
      first = false;
      write!(f, "{}", sub)?;
    }
    write!(f, "])")
  }
}
/// Holds a [`PostingsEnum`] along with the corresponding [`ReaderSlice`].
#[derive(Clone)]
pub struct EnumWithSlice {
  /// [`PostingsEnum`]'s idx for this sub-reader
  pub(crate) postings_enum_idx: usize,
  /// [`ReaderSlice`] describing how this sub-reader fits into the composite reader.
  pub(crate) slice: ReaderSlice,
}
impl EnumWithSlice {
  /// Creates a new [`EnumWithSlice`].
  pub fn new() -> Self {
    Self {
      postings_enum_idx: 0,
      slice: ReaderSlice::default(),
    }
  }
  pub fn with_slice(slice: ReaderSlice) -> Self {
    Self {
      postings_enum_idx: 0,
      slice,
    }
  }
}
impl Default for EnumWithSlice {
  fn default() -> Self {
    Self::new()
  }
}
impl Display for EnumWithSlice {
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "{:?}", self.slice)
  }
}
