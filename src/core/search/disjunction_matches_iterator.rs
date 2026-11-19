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
use crate::core::index::index_reader_context::IndexReaderContext;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::leaf_reader_context::LeafReaderContext;
use crate::core::index::postings_enum::OFFSETS;
use crate::core::index::term::Term;
use crate::core::index::terms::{Terms, get_terms};
use crate::core::index::terms_enum::TermsEnum;
use crate::core::index::{BytesRef, BytesRefValue};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::matches_iterator::MatchesIterator;
use crate::core::search::matches_iterator::MatchesIteratorEnum2;
use crate::core::search::query::{Query, QueryWeightMatchesIterator};
use crate::core::search::similarities_impl::similarities::Similarity;
use crate::core::search::term_matches_iterator::TermMatchesIterator;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::priority_queue::{Compare, PriorityQueue};
use std::sync::Arc;

/// A [`MatchesIterator`] that combines matches from a set of sub-iterators.
///
/// Matches are sorted by their start positions, and then by their end positions,
/// so that prefixes sort first.
///
/// Matches may overlap, or be duplicated if they appear in more than one of the
/// sub-iterators.
pub struct DisjunctionMatchesIterator<M> {
  queue: PriorityQueue<M, DisjunctionMatchesIteratorPQCmp>,
  started: bool,
}
impl<M> DisjunctionMatchesIterator<M>
where
  M: MatchesIterator,
{
  pub fn new(mut matches: Vec<M>) -> Result<Self> {
    debug_assert!(matches.len() <= i32::MAX as usize);
    let size = matches.len();
    let mut queue = PriorityQueue::new(size, DisjunctionMatchesIteratorPQCmp)?;
    for mut sub in matches.drain(..) {
      if sub.next()? {
        queue.add(sub)?;
      }
    }
    Ok(DisjunctionMatchesIterator {
      queue,
      started: false,
    })
  }
}
impl<M> MatchesIterator for DisjunctionMatchesIterator<M>
where
  M: MatchesIterator,
{
  type SubMatches<'b>
    = M::SubMatches<'b>
  where
    Self: 'b;
  fn next(&mut self) -> Result<bool> {
    if !self.started {
      self.started = true;
      return Ok(self.queue.size() > 0);
    }

    if !self
      .queue
      .top_mut()
      .ok_or_else(|| LuceneError::illegal_state("no top available"))?
      .next()?
    {
      self.queue.pop_unchecked()?;
    }

    if self.queue.size() > 0 {
      self.queue.update_top()?;
      Ok(true)
    } else {
      Ok(false)
    }
  }

  fn start_position(&self) -> Result<i32> {
    self
      .queue
      .top()
      .ok_or_else(|| LuceneError::illegal_state("priority queue top element should exist"))?
      .start_position()
  }

  fn end_position(&self) -> Result<i32> {
    self
      .queue
      .top()
      .ok_or_else(|| LuceneError::illegal_state("priority queue top element should exist"))?
      .end_position()
  }

  fn start_offset(&self) -> Result<i32> {
    self
      .queue
      .top()
      .ok_or_else(|| LuceneError::illegal_state("priority queue top element should exist"))?
      .start_offset()
  }

  fn end_offset(&self) -> Result<i32> {
    self
      .queue
      .top()
      .ok_or_else(|| LuceneError::illegal_state("priority queue top element should exist"))?
      .end_offset()
  }

  fn get_sub_matches(&mut self) -> Result<Option<Self::SubMatches<'_>>> {
    self
      .queue
      .top_mut()
      .ok_or_else(|| LuceneError::illegal_state("no top available"))?
      .get_sub_matches()
  }

  fn get_query(&self) -> Result<&Arc<Query>> {
    self
      .queue
      .top()
      .ok_or_else(|| LuceneError::illegal_state("priority queue top element should exist"))?
      .get_query()
  }
}

pub(crate) struct DisjunctionMatchesIteratorPQCmp;
impl<M> Compare<M> for DisjunctionMatchesIteratorPQCmp
where
  M: MatchesIterator,
{
  fn less_than(&self, a: &M, b: &M) -> Result<bool> {
    if a.start_position()? == -1 && b.start_position()? == -1 {
      let a_start = a.start_offset()?;
      let b_start = b.start_offset()?;
      let a_end = a.end_offset()?;
      let b_end = b.end_offset()?;
      return Ok(a_start < b_start || (a_start == b_start && a_end <= b_end));
    }
    let a_start = a.start_position()?;
    let b_start = b.start_position()?;
    let a_end = a.end_position()?;
    let b_end = b.end_position()?;
    Ok(a_start < b_start || (a_start == b_start && a_end <= b_end))
  }
}
// MatchesIterator over a set of terms that only loads the first matching term at construction,
// waiting until the iterator is actually used before it loads all other matching terms.
pub struct TermsEnumDisjunctionMatchesIterator<TE, BRI>
where
  TE: TermsEnum,
{
  first: Option<TermMatchesIterator<TE::PostingsEnum>>,
  terms: BRI,
  te: TE,
  doc: i32,
  query: Arc<Query>,
  it: Option<TermDisjunction<TE>>,
}
impl<TE, BRI> TermsEnumDisjunctionMatchesIterator<TE, BRI>
where
  TE: TermsEnum,
  BRI: BytesRefIterator,
{
  pub fn new(
    first: TermMatchesIterator<TE::PostingsEnum>,
    terms: BRI,
    te: TE,
    doc: i32,
    query: Arc<Query>,
  ) -> Self {
    TermsEnumDisjunctionMatchesIterator {
      first: Some(first),
      terms,
      te,
      doc,
      query,
      it: None,
    }
  }

  fn init(&mut self) -> Result<()> {
    let mut first = Some(
      self
        .first
        .take()
        .ok_or_else(|| LuceneError::illegal_state("first matches iterator is missing"))?,
    );
    let mut matches = Vec::new();
    let mut reuse = None;
    while let Some(term) = self.terms.next()? {
      if self.te.seek_exact(&term.as_bytes_ref())? {
        let mut postings = self.te.postings_with_flags(reuse, OFFSETS as i32)?;
        if postings.advance(self.doc)? == self.doc {
          if let Some(first) = first.take() {
            matches.push(first);
          }
          matches.push(TermMatchesIterator::new(postings, self.query.clone())?);
          reuse = None;
        } else {
          reuse = Some(postings);
        }
      }
    }
    self.it = Some(match first {
      Some(first) => MatchesIteratorEnum2::A(first),
      None => MatchesIteratorEnum2::B(DisjunctionMatchesIterator::new(matches)?),
    });
    Ok(())
  }
}

impl<TE, BRI> MatchesIterator for TermsEnumDisjunctionMatchesIterator<TE, BRI>
where
  TE: TermsEnum,
  BRI: BytesRefIterator,
{
  type SubMatches<'b>
    = <TermDisjunction<TE> as MatchesIterator>::SubMatches<'b>
  where
    Self: 'b;
  fn next(&mut self) -> Result<bool> {
    if self.it.is_none() {
      self.init()?;
    }
    self
      .it
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("matches iterator is not initialized"))?
      .next()
  }

  fn start_position(&self) -> Result<i32> {
    self
      .it
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("matches iterator is not initialized"))?
      .start_position()
  }

  fn end_position(&self) -> Result<i32> {
    self
      .it
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("matches iterator is not initialized"))?
      .end_position()
  }

  fn start_offset(&self) -> Result<i32> {
    self
      .it
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("matches iterator is not initialized"))?
      .start_offset()
  }

  fn end_offset(&self) -> Result<i32> {
    self
      .it
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("matches iterator is not initialized"))?
      .end_offset()
  }

  fn get_sub_matches(&mut self) -> Result<Option<Self::SubMatches<'_>>> {
    self
      .it
      .as_mut()
      .ok_or_else(|| LuceneError::illegal_state("matches iterator is not initialized"))?
      .get_sub_matches()
  }

  fn get_query(&self) -> Result<&Arc<Query>> {
    self
      .it
      .as_ref()
      .ok_or_else(|| LuceneError::illegal_state("matches iterator is not initialized"))?
      .get_query()
  }
}

pub struct TermBytesRefIterator {
  terms: Arc<[Term]>,
  index: usize,
}
impl BytesRefIterator for TermBytesRefIterator {
  type Value<'a>
    = &'a BytesRef<Vec<u8>>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    if self.index == self.terms.len() {
      return Ok(None);
    }
    let term = &self.terms[self.index];
    self.index += 1;
    Ok(Some(term.bytes()))
  }
}

pub(crate) type LeafTermsMatchesIterator<LR, BRI> = TermsEnumDisjunctionMatchesIterator<
  <crate::core::index::terms::TermsWithEmpty<<LR as LeafReader>::Terms> as Terms>::TermsEnum,
  BRI,
>;

/// Create a [`MatchesIterator`] over terms extracted from a [`BytesRefIterator`].
///
/// Only terms that have at least one match in the given document will be included.
pub(crate) fn from_terms_enum<'a, LR, BRI>(
  context: &LeafReaderContext<LR>,
  doc: i32,
  query: Arc<Query>,
  field: &str,
  mut terms: BRI,
) -> Result<Option<LeafTermsMatchesIterator<LR, BRI>>>
where
  LR: LeafReader,
  BRI: BytesRefIterator + 'a,
  <LR::Terms as Terms>::TermsEnum: 'a,
  <<LR::Terms as Terms>::TermsEnum as TermsEnum>::PostingsEnum: 'a,
{
  let indexed_terms = get_terms(context.reader(), field)?;
  let mut terms_enum = indexed_terms.iterator()?;
  let mut reuse = None;
  loop {
    let found = {
      let term = match terms.next()? {
        Some(term) => term,
        None => break,
      };
      terms_enum.seek_exact(&term.as_bytes_ref())?
    };
    if found {
      let mut postings = terms_enum.postings_with_flags(reuse, OFFSETS as i32)?;
      if postings.advance(doc)? == doc {
        return Ok(Some(TermsEnumDisjunctionMatchesIterator::new(
          TermMatchesIterator::new(postings, query.clone())?,
          terms,
          terms_enum,
          doc,
          query,
        )));
      }
      reuse = Some(postings);
    }
  }
  Ok(None)
}

type TermDisjunction<TE> = MatchesIteratorEnum2<
  TermMatchesIterator<<TE as TermsEnum>::PostingsEnum>,
  DisjunctionMatchesIterator<TermMatchesIterator<<TE as TermsEnum>::PostingsEnum>>,
>;
pub fn from_sub_iterators<'a, LR: LeafReader, SIM>(
  mut mis: Vec<QueryWeightMatchesIterator<'a, LR, SIM>>,
) -> Result<Option<QueryWeightMatchesIterator<'a, LR, SIM>>>
where
  SIM: Similarity,
{
  if mis.is_empty() {
    return Ok(None);
  }
  if mis.len() == 1 {
    return Ok(mis.pop());
  }
  Ok(Some(Box::new(DisjunctionMatchesIterator::new(mis)?).into()))
}
