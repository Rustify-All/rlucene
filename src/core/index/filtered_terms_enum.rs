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
use std::fmt::Debug;

use crate::core::index::BytesRef;
use crate::core::index::dummy::dummy_postings_enum::DummyPostingsEnum;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::LuceneError;
use crate::core::util::error::lucene_error::Result;

/// Struct for enumerating a subset of all terms.
///
/// Term enumerations are always ordered by [`BytesRef::cmp`] Each term in the
/// enumeration is greater than all that precede it.
///
/// *Please note:* Consumers of this enum cannot call `seek()`, it is forward
/// only; it will return
/// [`UnsupportedOperationError`](LuceneError::unsupported_operation) when a
/// seeking method is called.
pub struct FilteredTermsEnum<T, F>
where
    T: TermsEnum,
    F: FilteredTermsEnumBase,
{
    initial_seek_term: Option<BytesRef<Vec<u8>>>,
    do_seek: bool,
    pub actual_term: BytesRef<Vec<u8>>,
    pub tenum: T,
    sub: F,
}
impl<T, F> FilteredTermsEnum<T, F>
where
    T: TermsEnum,
    F: FilteredTermsEnumBase,
{
    pub(crate) fn new(tenum: T, sub: F) -> Self {
        Self::with_seek(tenum, true, sub)
    }

    /// Creates a new filtered enumerator with control over initial seeking.
    pub(crate) fn with_seek(tenum: T, start_with_seek: bool, sub: F) -> Self {
        FilteredTermsEnum {
            initial_seek_term: None,
            do_seek: start_with_seek,
            actual_term: BytesRef::default(),
            tenum,
            sub,
        }
    }
    pub(crate) fn set_initial_seek_term(&mut self, term: BytesRef<Vec<u8>>) {
        self.initial_seek_term = Some(term);
    }
    pub fn next_seek_term(&mut self) -> Result<Option<BytesRef<Vec<u8>>>> {
        match self.sub.next_seek_term(Option::from(&self.actual_term)) {
            Ok(v) => Ok(v),
            Err(e) => match e {
                LuceneError::NotImplemented(_) => {
                    let mut a = self.initial_seek_term.take().unwrap();
                    Ok(Some(BytesRef::from_slice(
                        std::mem::take(&mut a.bytes),
                        a.offset,
                        a.length,
                    )))
                },
                _ => Err(e),
            },
        }
    }
}

impl<T, F> BytesRefIterator for FilteredTermsEnum<T, F>
where
    T: TermsEnum,
    F: FilteredTermsEnumBase,
{
    fn next(&mut self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
        loop {
            if self.do_seek {
                self.do_seek = false;
                let t = self.next_seek_term()?;

                if t.is_none() || self.tenum.seek_ceil(t.as_ref().unwrap())? == SeekStatus::End {
                    return Ok(None);
                }
                // TODO: avoid copy here?
                self.actual_term = self.tenum.term()?.into_owned();
            } else {
                match self.tenum.next()? {
                    Some(term) => {
                        self.actual_term = term.into_owned();
                    },
                    None => return Ok(None),
                };
            }
            // check if term is accepted
            match self.sub.accept(&self.actual_term)? {
                AcceptStatus::YesAndSeek => {
                    self.do_seek = true;
                    return Ok(Some(Cow::Borrowed(&self.actual_term)));
                },
                // term accepted, but we need to seek so fall-through
                AcceptStatus::Yes => {
                    return Ok(Some(Cow::Borrowed(&self.actual_term)));
                },
                AcceptStatus::NoAndSeek => {
                    // invalid term, seek next time
                    self.do_seek = true;
                },
                AcceptStatus::End => {
                    // we are supposed to end the enum
                    return Ok(None);
                },
                // we just iterate again
                AcceptStatus::No => {},
            }
        }
    }
}

impl<T, F> TermsEnum for FilteredTermsEnum<T, F>
where
    T: TermsEnum,
    F: FilteredTermsEnumBase,
{
    type AttributeSource = T::AttributeSource;

    fn attributes(&self) -> Result<Self::AttributeSource> {
        self.tenum.attributes()
    }

    fn seek_ceil(&mut self, _term: &BytesRef<Vec<u8>>) -> Result<SeekStatus> {
        Err(LuceneError::unsupported_operation(
            "FilteredTermsEnum::seek_ceil",
        ))
    }

    fn seek_exact_with_ord(&mut self, _ord: i64) -> Result<()> {
        Err(LuceneError::unsupported_operation(
            "FilteredTermsEnum::seek_exact_with_ord",
        ))
    }

    fn seek_exact_with_state(
        &mut self,
        _term: &BytesRef<Vec<u8>>,
        _state: &Self::TermState,
    ) -> Result<()> {
        Err(LuceneError::unsupported_operation(
            "FilteredTermsEnum::seek_exact_with_state",
        ))
    }

    fn term(&self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        self.tenum.term()
    }

    fn ord(&self) -> Result<i64> {
        self.tenum.ord()
    }

    fn doc_freq(&mut self) -> Result<i32> {
        self.tenum.doc_freq()
    }

    fn total_term_freq(&mut self) -> Result<i64> {
        self.tenum.total_term_freq()
    }

    type PostingsEnum = DummyPostingsEnum;

    fn postings_with_flags(
        &mut self,
        _reuse: Option<Self::PostingsEnum>,
        _flags: i32,
    ) -> Result<Self::PostingsEnum> {
        Err(LuceneError::unsupported_operation(
            "FilteredTermsEnum::postings_with_flags",
        ))
    }

    type ImpactsEnum = T::ImpactsEnum;

    fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
        self.tenum.impacts(flags)
    }

    type TermState = T::TermState;

    fn term_state(&mut self) -> Result<Self::TermState> {
        self.tenum.term_state()
    }
}

/// Return value indicating whether the term should be accepted or the iteration
/// should end. The `*_SEEK` values denote that after handling the current term,
/// the enum should call [`next_seek_term`](FilteredTermsEnum::next_seek_term)
/// and step forward.
///
/// See also:
/// - [`accept`](FilteredTermsEnumBase::accept)
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AcceptStatus {
    /// Accept the term and continue.
    Yes,
    /// Accept the term then seek to the next term returned by
    /// `next_seek_term()`.
    YesAndSeek,
    /// Reject the term and continue.
    No,
    /// Reject the term then seek to the next term returned by
    /// `next_seek_term()`.
    NoAndSeek,
    /// Reject the term and terminate enumeration.
    End,
}
pub trait FilteredTermsEnumBase {
    /// Return if term is accepted, not accepted or the iteration should ended
    /// (and possibly seek).
    fn accept(&mut self, term: &BytesRef<Vec<u8>>) -> Result<AcceptStatus>;
    fn next_seek_term(
        &mut self,
        current: Option<&BytesRef<Vec<u8>>>,
    ) -> Result<Option<BytesRef<Vec<u8>>>>;
}
