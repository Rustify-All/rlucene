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
use std::borrow::Cow;
use std::fmt::{Debug, Display, Formatter};

use crate::core::index::BytesRef;
use crate::core::index::term_state::{Either2TermState, TermState, TermStateEnum};
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::util::attribute_source::Either2AttributeSource;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::dummy::dummy_attribute_source::DummyAttributeSource;
use crate::core::util::error::lucene_error::{LuceneError, Result};

/// A base `TermsEnum` that provides default implementations for:
///
/// - [`attributes()`](BaseTermsEnum::attributes)
/// - [`term_state()`](BaseTermsEnum::term_state)
/// - [`seek_exact(&BytesRef)`](BaseTermsEnum::seek_exact)
/// - [`seek_exact_with_state(&BytesRef,
///   &TermState)`](BaseTermsEnum::seek_exact_with_state)
///
/// In some cases, the default implementation may be slow and consume large
/// amounts of memory, so SubStruct SHOULD provide their own implementation if
/// possible.
pub struct BaseTermsEnum<S>
where
    S: TermsEnum,
{
    sub: S,
}
impl<S> BaseTermsEnum<S>
where
    S: TermsEnum,
{
    pub fn new(sub: S) -> Self {
        Self { sub }
    }
}

impl<S> BytesRefIterator for BaseTermsEnum<S>
where
    S: TermsEnum,
{
    fn next(&mut self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
        self.sub.next()
    }
}

impl<S> TermsEnum for BaseTermsEnum<S>
where
    S: TermsEnum,
{
    type AttributeSource = Either2AttributeSource<S::AttributeSource, DummyAttributeSource>;

    fn attributes(&self) -> Result<Self::AttributeSource> {
        match self.sub.attributes() {
            Ok(v) => Ok(Either2AttributeSource::A(v)),
            Err(e) => match e {
                LuceneError::NotImplemented(_) => {
                    Ok(Either2AttributeSource::B(DummyAttributeSource))
                },
                _ => Err(e),
            },
        }
    }

    fn seek_exact(&mut self, term: &BytesRef<Vec<u8>>) -> Result<bool> {
        match self.sub.seek_exact(term) {
            Ok(v) => Ok(v),
            Err(e) => match e {
                LuceneError::NotImplemented(_) => Ok(self.seek_ceil(term)? == SeekStatus::Found),
                _ => Err(e),
            },
        }
    }

    fn prepare_seek_exact(&mut self, text: &BytesRef<Vec<u8>>) -> Result<bool> {
        match self.sub.prepare_seek_exact(text) {
            Ok(v) => Ok(v),
            Err(e) => match e {
                LuceneError::NotImplemented(_) => self.seek_exact(text),
                _ => Err(e),
            },
        }
    }

    fn seek_ceil(&mut self, term: &BytesRef<Vec<u8>>) -> Result<SeekStatus> {
        self.sub.seek_ceil(term)
    }

    fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
        self.sub.seek_exact_with_ord(ord)
    }

    fn seek_exact_with_state(
        &mut self,
        term: &BytesRef<Vec<u8>>,
        state: &TermStateEnum,
    ) -> Result<()> {
        match self.sub.seek_exact_with_state(term, state) {
            Ok(v) => Ok(v),
            Err(e) => match e {
                LuceneError::NotImplemented(_) => {
                    if !self.seek_exact(term)? {
                        return Err(LuceneError::illegal_argument(format!(
                            "term= {term} does not exist"
                        )));
                    };
                    Ok(())
                },
                _ => Err(e),
            },
        }
    }

    fn term(&self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        self.sub.term()
    }

    fn ord(&self) -> Result<i64> {
        self.sub.ord()
    }

    fn doc_freq(&mut self) -> Result<i32> {
        self.sub.doc_freq()
    }

    fn total_term_freq(&mut self) -> Result<i64> {
        self.sub.total_term_freq()
    }

    type PostingsEnum = S::PostingsEnum;

    fn postings(&mut self, reuse: Option<Self::PostingsEnum>) -> Result<Self::PostingsEnum> {
        self.sub.postings(reuse)
    }

    fn postings_with_flags(
        &mut self,
        reuse: Option<Self::PostingsEnum>,
        flags: i32,
    ) -> Result<Self::PostingsEnum> {
        self.sub.postings_with_flags(reuse, flags)
    }

    type ImpactsEnum = S::ImpactsEnum;

    fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
        self.sub.impacts(flags)
    }

    type TermState = Either2TermState<TermStateImpl1, S::TermState>;

    fn term_state(&mut self) -> Result<Self::TermState> {
        match self.sub.term_state() {
            Ok(v) => Ok(Either2TermState::B(v)),
            Err(e) => match e {
                LuceneError::NotImplemented(_) => Ok(Either2TermState::A(TermStateImpl1)),
                _ => Err(e),
            },
        }
    }
}
#[derive(Debug, Clone)]
pub struct TermStateImpl1;
impl Display for TermStateImpl1 {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", std::any::type_name::<Self>())
    }
}
impl TermState for TermStateImpl1 {
    fn copy_from(&mut self, _other: &TermStateEnum) -> Result<()> {
        Err(LuceneError::unsupported_operation(""))
    }
}
