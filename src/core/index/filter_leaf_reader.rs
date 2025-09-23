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
use crate::core::index::automaton_terms_enum::AutomatonTermsEnum;
use crate::core::index::fields::Fields;
use crate::core::index::filtered_terms_enum::{FilteredTermsEnum, FilteredTermsEnumBase};
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::util::automation::compiled_automaton::CompiledAutomaton;
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::error::lucene_error::Result;
use std::borrow::Cow;

pub trait FilterLeafReader {}
/// Base struct for filtering [`Fields`] implementations.
pub struct FilterFields<F>
where
    F: Fields,
{
    /// The underlying Fields instance.
    inner: F,
}
impl<F> FilterFields<F>
where
    F: Fields,
{
    pub fn new(inner: F) -> FilterFields<F> {
        Self { inner }
    }
}
impl<F> Fields for FilterFields<F>
where
    F: Fields,
{
    type FieldIter<'a>
        = F::FieldIter<'a>
    where
        F: 'a;

    fn iterator(&self) -> Self::FieldIter<'_> {
        self.inner.iterator()
    }

    type Terms = F::Terms;

    fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
        self.inner.terms(field)
    }

    fn size(&self) -> Result<i32> {
        self.inner.size()
    }
}

/// Base struct for filtering [`Terms`] implementations.
///
/// **NOTE**: If the order of terms and documents is not changed, and if these terms are
/// going to be intersected with automata, you could consider overriding [`Self::intersect`](Terms::intersect) for
/// better performance.
pub struct FilterTerms<T>
where
    T: Terms,
{
    /// The underlying `Terms` instance.
    pub(crate) inner: T,
}

impl<T> FilterTerms<T>
where
    T: Terms,
{
    pub fn new(inner: T) -> Self {
        Self { inner }
    }
}
impl<T> Terms for FilterTerms<T>
where
    T: Terms,
{
    type TermsEnum = T::TermsEnum;

    fn iterator(&self) -> Result<Self::TermsEnum> {
        self.inner.iterator()
    }

    type IntersectIter
        = FilteredTermsEnum<Self::TermsEnum, AutomatonTermsEnum>
    where
        Self::TermsEnum: BytesRefIterator,
        AutomatonTermsEnum: FilteredTermsEnumBase;

    fn intersect(
        &self,
        compiled: &mut CompiledAutomaton,
        start_term: Option<BytesRef<Vec<u8>>>,
    ) -> Result<Self::IntersectIter> {
        self.default_intersect(compiled, start_term)
    }

    fn size(&self) -> Result<i64> {
        self.inner.size()
    }

    fn get_sum_total_term_freq(&self) -> Result<i64> {
        self.inner.get_sum_total_term_freq()
    }

    fn get_sum_doc_freq(&self) -> Result<i64> {
        self.inner.get_sum_doc_freq()
    }

    fn get_doc_count(&self) -> Result<i32> {
        self.inner.get_doc_count()
    }

    fn has_freqs(&self) -> bool {
        self.inner.has_freqs()
    }

    fn has_offsets(&self) -> bool {
        self.inner.has_offsets()
    }

    fn has_positions(&self) -> bool {
        self.inner.has_positions()
    }

    fn has_payloads(&self) -> bool {
        self.inner.has_payloads()
    }

    fn get_stats(&self) -> Result<String> {
        self.inner.get_stats()
    }
}

/// Base struct for filtering `TermsEnum` implementations.
pub struct FilterTermsEnum<T>
where
    T: TermsEnum,
{
    terms_enum: T,
}
impl<T> FilterTermsEnum<T>
where
    T: TermsEnum,
{
    pub fn new(terms_enum: T) -> Self {
        Self { terms_enum }
    }
}

impl<T> BytesRefIterator for FilterTermsEnum<T>
where
    T: TermsEnum,
{
    fn next(&mut self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
        self.terms_enum.next()
    }
}

impl<T> TermsEnum for FilterTermsEnum<T>
where
    T: TermsEnum,
{
    type AttributeSource = T::AttributeSource;

    fn attributes(&self) -> Result<Self::AttributeSource> {
        self.terms_enum.attributes()
    }

    fn seek_exact(&mut self, term: &BytesRef<Vec<u8>>) -> Result<bool> {
        self.terms_enum.seek_exact(term)
    }

    fn seek_ceil(&mut self, term: &BytesRef<Vec<u8>>) -> Result<SeekStatus> {
        self.terms_enum.seek_ceil(term)
    }

    fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
        self.terms_enum.seek_exact_with_ord(ord)
    }

    fn seek_exact_with_state(
        &mut self,
        term: &BytesRef<Vec<u8>>,
        state: &Self::TermState,
    ) -> Result<()> {
        self.terms_enum.seek_exact_with_state(term, state)
    }

    fn term(&self) -> Result<Cow<'_, BytesRef<Vec<u8>>>> {
        self.terms_enum.term()
    }

    fn ord(&self) -> Result<i64> {
        self.terms_enum.ord()
    }

    fn doc_freq(&mut self) -> Result<i32> {
        self.terms_enum.doc_freq()
    }

    fn total_term_freq(&mut self) -> Result<i64> {
        self.terms_enum.total_term_freq()
    }

    type PostingsEnum = T::PostingsEnum;

    fn postings_with_flags(
        &mut self,
        reuse: Option<Self::PostingsEnum>,
        flags: i32,
    ) -> Result<Self::PostingsEnum> {
        self.terms_enum.postings_with_flags(reuse, flags)
    }

    type ImpactsEnum = T::ImpactsEnum;

    fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
        self.terms_enum.impacts(flags)
    }

    type TermState = T::TermState;

    fn term_state(&mut self) -> Result<Self::TermState> {
        self.terms_enum.term_state()
    }
}
