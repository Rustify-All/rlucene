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
use crate::core::index::dummy::dummy_terms_enum::DummyTermsEnum;
use crate::core::index::filtered_terms_enum::FilteredTermsEnum;
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::TermsEnum;
use crate::core::util::automation::compiled_automaton::CompiledAutomaton;
use crate::core::util::error::lucene_error::Result;
use std::borrow::Cow;

pub struct DummyTerms;
impl Terms for DummyTerms {
    type TermsEnum = DummyTermsEnum;

    fn iterator(&self) -> Result<Self::TermsEnum> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    type IntersectIter = DummyTermsEnum;

    fn intersect(
        &self,
        _compiled: &mut CompiledAutomaton,
        _start_term: Option<BytesRef<Vec<u8>>>,
    ) -> Result<Self::IntersectIter> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn default_intersect(
        &self,
        _compiled: &mut CompiledAutomaton,
        _start_term: Option<BytesRef<Vec<u8>>>,
    ) -> Result<FilteredTermsEnum<Self::TermsEnum, AutomatonTermsEnum>>
    where
        Self: Sized,
    {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn size(&self) -> Result<i64> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn get_sum_total_term_freq(&self) -> Result<i64> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn get_sum_doc_freq(&self) -> Result<i64> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn get_doc_count(&self) -> Result<i32> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn has_freqs(&self) -> bool {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn has_offsets(&self) -> bool {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn has_positions(&self) -> bool {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn has_payloads(&self) -> bool {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn get_min<'a, T>(&'a self, _iterator: &'a mut T) -> Result<Option<Cow<'a, BytesRef<Vec<u8>>>>>
    where
        T: TermsEnum,
    {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn get_max<'a, T>(&'a self, _iterator: &'a mut T) -> Result<Option<Cow<'a, BytesRef<Vec<u8>>>>>
    where
        T: TermsEnum,
    {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }

    fn get_stats(&self) -> Result<String> {
        unreachable!("Dummy implementation: this method should never be called in real usage")
    }
}
