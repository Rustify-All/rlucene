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
use crate::core::index::terms_enum::TermsEnum;
use crate::core::search::matches_iterator::MatchesIterator;
use crate::core::search::query::{Query, QueryBase};
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::Result;
use crate::core::util::priority_queue::{Compare, PriorityQueue};
use std::marker::PhantomData;
use std::sync::Arc;

/// A [`MatchesIterator`] that combines matches from a set of sub-iterators.
///
/// Matches are sorted by their start positions, and then by their end positions,
/// so that prefixes sort first.
///
/// Matches may overlap, or be duplicated if they appear in more than one of the
/// sub-iterators.
pub struct DisjunctionMatchesIterator<M>
where
    M: MatchesIterator,
{
    queue: PriorityQueue<M, DisjunctionMatchesIteratorPQCmp<M>>,
    started: bool,
}
impl<M> DisjunctionMatchesIterator<M>
where
    M: MatchesIterator,
{
    pub fn new(mut matches: Vec<M>) -> Result<Self> {
        debug_assert!(matches.len() <= i32::MAX as usize);
        let size = matches.len() as i32;
        let mut queue = PriorityQueue::new(
            size,
            DisjunctionMatchesIteratorPQCmp {
                _phantom: PhantomData,
            },
        )?;
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
    fn next(&mut self) -> Result<bool> {
        if !self.started {
            self.started = true;
            return Ok(self.queue.size() > 0);
        }

        if !self
            .queue
            .top_mut()
            .expect("priority queue top element should exist")
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
        self.queue
            .top()
            .expect("priority queue top element should exist")
            .start_position()
    }

    fn end_position(&self) -> i32 {
        self.queue
            .top()
            .expect("priority queue top element should exist")
            .end_position()
    }

    fn start_offset(&self) -> Result<i32> {
        self.queue
            .top()
            .expect("priority queue top element should exist")
            .start_offset()
    }

    fn end_offset(&self) -> Result<i32> {
        self.queue
            .top()
            .expect("priority queue top element should exist")
            .end_offset()
    }

    type MatchesIterRef<'a>
        = M::MatchesIterRef<'a>
    where
        Self: 'a;

    fn get_sub_matches(&mut self) -> Result<Option<Self::MatchesIterRef<'_>>> {
        self.queue
            .top_mut()
            .expect("priority queue top element should exist")
            .get_sub_matches()
    }

    fn get_query(&self) -> Arc<Query> {
        self.queue
            .top()
            .expect("priority queue top element should exist")
            .get_query()
    }
}

pub(crate) struct DisjunctionMatchesIteratorPQCmp<M>
where
    M: MatchesIterator,
{
    _phantom: PhantomData<M>,
}
impl<M> Compare<M> for DisjunctionMatchesIteratorPQCmp<M>
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
        let a_end = a.end_position();
        let b_end = b.end_position();
        Ok(a_start < b_start || (a_start == b_start && a_end <= b_end))
    }
}
// MatchesIterator over a set of terms that only loads the first matching term at construction,
// waiting until the iterator is actually used before it loads all other matching terms.
pub(crate) struct TermsEnumDisjunctionMatchesIterator<Q, MI, TE, BRI>
where
    Q: QueryBase,
    MI: MatchesIterator,
    TE: TermsEnum,
    BRI: BytesRefIterator,
{
    first: MI,
    terms: BRI,
    te: TE,
    doc: i32,
    query: Arc<Q>,
    it: Option<MI>,
}
impl<Q, MI, TE, BRI> TermsEnumDisjunctionMatchesIterator<Q, MI, TE, BRI>
where
    Q: QueryBase,
    MI: MatchesIterator,
    TE: TermsEnum,
    BRI: BytesRefIterator,
{
    pub fn new(first: MI, terms: BRI, te: TE, doc: i32, query: Arc<Q>) -> Self {
        TermsEnumDisjunctionMatchesIterator {
            first,
            terms,
            te,
            doc,
            query,
            it: None,
        }
    }
}

pub fn from_sub_iterators<M>(mut mis: Vec<M>) -> Result<Option<DisjunctionMatchesIterator<M>>>
where
    M: MatchesIterator,
{
    if mis.is_empty() {
        return Ok(None);
    }
    if mis.len() == 1 {
        let only = mis.pop().unwrap();
        return Ok(Some(DisjunctionMatchesIterator::new(vec![only])?));
    }
    Ok(Some(DisjunctionMatchesIterator::new(mis)?))
}
