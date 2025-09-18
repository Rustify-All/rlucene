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
use crate::core::index::impact::Impact;
use crate::core::index::impacts::Impacts;
use crate::core::index::impacts_enum::ImpactsEnum;
use crate::core::index::impacts_source::ImpactsSource;
use crate::core::index::postings_enum::PostingsEnum;
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::util::error::lucene_error::Result;
use std::borrow::Cow;
/// [`ImpactsEnum`] that doesn't index impacts but implements the API in a legal way.
/// This is typically used for short postings that do not need skipping.
pub struct SlowImpactsEnum<P>
where
    P: PostingsEnum,
{
    pub(crate) delegate: P,
}
impl<P> SlowImpactsEnum<P>
where
    P: PostingsEnum,
{
    pub fn new(delegate: P) -> Self {
        SlowImpactsEnum { delegate }
    }
}

impl<P> PostingsEnum for SlowImpactsEnum<P>
where
    P: PostingsEnum,
{
    fn freq(&mut self) -> Result<i32> {
        self.delegate.freq()
    }

    fn next_position(&mut self) -> Result<i32> {
        self.delegate.next_position()
    }

    fn start_offset(&self) -> Result<i32> {
        self.delegate.start_offset()
    }

    fn end_offset(&self) -> Result<i32> {
        self.delegate.end_offset()
    }

    fn get_payload(&self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
        self.delegate.get_payload()
    }
}

impl<P> DocIdSetIterator for SlowImpactsEnum<P>
where
    P: PostingsEnum,
{
    fn doc_id(&self) -> i32 {
        self.delegate.doc_id()
    }

    fn next_doc(&mut self) -> Result<i32> {
        self.delegate.next_doc()
    }

    fn cost(&self) -> Result<i64> {
        self.delegate.cost()
    }
}

impl<P> ImpactsSource for SlowImpactsEnum<P>
where
    P: PostingsEnum,
{
    fn advance_shallow(&mut self, _target: i32) -> Result<()> {
        Ok(())
    }

    type Impacts = DummyImpacts;

    fn get_impacts(&mut self) -> Result<Self::Impacts> {
        Ok(DummyImpacts::new())
    }
}

impl<P> ImpactsEnum for SlowImpactsEnum<P> where P: PostingsEnum {}

pub struct DummyImpacts {
    impacts: Vec<Impact>,
}
impl Default for DummyImpacts {
    fn default() -> Self {
        Self::new()
    }
}

impl DummyImpacts {
    pub fn new() -> Self {
        DummyImpacts {
            impacts: vec![Impact::new(i32::MAX, 0)],
        }
    }
}
impl Impacts for DummyImpacts {
    fn num_levels(&self) -> i32 {
        1
    }

    fn get_doc_id_upto(&self, _level: i32) -> i32 {
        NO_MORE_DOCS
    }

    fn get_impacts(&'_ mut self, _level: i32) -> Result<Cow<'_, [Impact]>> {
        Ok(Cow::Borrowed(self.impacts.as_slice()))
    }
}
