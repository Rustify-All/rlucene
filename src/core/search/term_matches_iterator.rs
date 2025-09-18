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
use crate::core::index::postings_enum::PostingsEnum;
use crate::core::search::dummy::dummy_matches_iterator::DummyMatchesIterator;
use crate::core::search::matches_iterator::MatchesIterator;
use crate::core::search::query::Query;
use crate::core::util::error::lucene_error::Result;
use std::sync::Arc;

/// A [`MatchesIterator`] over a single term's postings list
pub(crate) struct TermMatchesIterator<Q, PE>
where
    PE: PostingsEnum,
    Q: Query,
{
    upto: i32,
    pos: i32,
    pe: PE,
    query: Arc<Q>,
}
impl<Q, PE> TermMatchesIterator<Q, PE>
where
    PE: PostingsEnum,
    Q: Query,
{
    pub fn new(mut pe: PE, query: Arc<Q>) -> Result<Self> {
        Ok(TermMatchesIterator {
            upto: pe.freq()?,
            pos: 0,
            pe,
            query,
        })
    }
}
impl<Q, PE> MatchesIterator for TermMatchesIterator<Q, PE>
where
    PE: PostingsEnum,
    Q: Query,
{
    fn next(&mut self) -> Result<bool> {
        let prev = self.upto;
        self.upto -= 1;
        if prev > 0 {
            self.pos = self.pe.next_position()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn start_position(&self) -> Result<i32> {
        Ok(self.pos)
    }

    fn end_position(&self) -> i32 {
        self.upto
    }

    fn start_offset(&self) -> Result<i32> {
        self.pe.start_offset()
    }

    fn end_offset(&self) -> Result<i32> {
        self.pe.end_offset()
    }

    type MatchesIterator = DummyMatchesIterator;

    fn get_sub_matches(&mut self) -> Result<Option<&Self::MatchesIterator>> {
        Ok(None)
    }

    type Query = Q;

    fn get_query(&self) -> &Self::Query {
        &self.query
    }
}
