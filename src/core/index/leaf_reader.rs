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
use crate::core::index::binary_doc_values::BinaryDocValues;
use crate::core::index::doc_values_skipper::DocValuesSkipper;
use crate::core::index::dummy::dummy_postings_enum::DummyPostingsEnum;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::point_values::PointValues;
use crate::core::index::postings_enum::{Either2PostingsEnum, FREQS};
use crate::core::index::sorted_doc_values::SortedDocValues;
use crate::core::index::sorted_numeric_doc_values::SortedNumericDocValues;
use crate::core::index::sorted_set_doc_values::SortedSetDocValues;
use crate::core::index::term::Term;
use crate::core::index::terms::{Terms, terms_util};
use crate::core::index::terms_enum::TermsEnum;
use crate::core::util::bits::Bits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::sync::Arc;

pub trait LeafReader: IndexReader {
    fn doc_freq(&self, term: &Term) -> Result<i32>
    where
        Self: Sized,
    {
        let terms = terms_util::get_terms(self, term.field())?;
        let mut terms_enum = terms.iterator()?;

        if terms_enum.seek_exact(term.bytes())? {
            terms_enum.doc_freq()
        } else {
            Ok(0)
        }
    }
    /// Returns the number of documents containing the term `t`.
    /// This method returns `0` if the term or field does not exist.
    /// This method does not take into account deleted documents
    /// that have not yet been merged away.
    fn total_term_freq(&self, term: &Term) -> Result<i64>
    where
        Self: Sized,
    {
        let terms = terms_util::get_terms(self, term.field())?;
        let mut terms_enum = terms.iterator()?;

        if terms_enum.seek_exact(term.bytes())? {
            terms_enum.total_term_freq()
        } else {
            Ok(0)
        }
    }
    fn sum_doc_freq(&self, field: &str) -> Result<i64>
    where
        Self: Sized,
    {
        if let Some(terms) = self.terms(field)? {
            terms.get_sum_doc_freq()
        } else {
            Ok(0)
        }
    }

    fn doc_count(&self, field: &str) -> Result<i32>
    where
        Self: Sized,
    {
        if let Some(terms) = self.terms(field)? {
            terms.get_doc_count()
        } else {
            Ok(0)
        }
    }

    fn sum_total_term_freq(&self, field: &str) -> Result<i64>
    where
        Self: Sized,
    {
        if let Some(terms) = self.terms(field)? {
            terms.get_sum_total_term_freq()
        } else {
            Ok(0)
        }
    }

    type Terms: Terms;
    fn terms(&self, field: &str) -> Result<Option<Self::Terms>>;
    /// Returns [`PostingsEnum`](crate::core::index::postings_enum::PostingsEnum) for the specified term.
    /// This will return `None` if either the field or term does not exist.
    ///
    /// **NOTE:** The returned [`PostingsEnum`](crate::core::index::postings_enum::PostingsEnum) may contain deleted docs.
    ///
    /// See [`TermsEnum::postings`].
    fn postings_with_flag(
        &mut self,
        term: &Term,
        flags: i32,
    ) -> Result<Option<LeafPostingsEnum<Self::Terms>>>
    where
        Self: Sized,
    {
        let terms = terms_util::get_terms(self, term.field())?;
        let mut terms_enum = terms.iterator()?;
        if terms_enum.seek_exact(term.bytes())? {
            Ok(Some(terms_enum.postings_with_flags(None, flags)?))
        } else {
            Ok(None)
        }
    }
    /// Returns [`PostingsEnum`](crate::core::index::postings_enum::PostingsEnum) for the specified term with [`FREQS`].
    ///
    /// Use this method if you only require documents and frequencies,
    /// and do not need any proximity data.
    /// This method is equivalent to [`Self::postings_with_flag`].
    ///
    /// **NOTE:** The returned [`PostingsEnum`](crate::core::index::postings_enum::PostingsEnum) may contain deleted docs.
    ///
    /// See [`Self::postings_with_flag`].
    fn postings(&mut self, term: &Term) -> Result<Option<LeafPostingsEnum<Self::Terms>>>
    where
        Self: Sized,
    {
        self.postings_with_flag(term, FREQS as i32)
    }

    type NumericDocValues: NumericDocValues;
    fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>>;

    type BinaryDocValues: BinaryDocValues;
    fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>>;

    type SortedDocValues: SortedDocValues;
    fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>>;

    type SortedNumericDocValues: SortedNumericDocValues;
    fn get_sorted_numeric_doc_values(
        &self,
        field: &str,
    ) -> Result<Option<Self::SortedNumericDocValues>>;

    type SortedSetDocValues: SortedSetDocValues;
    fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>>;

    type NormNumericDocValues: NumericDocValues;
    fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>>;

    type DocValuesSkipper: DocValuesSkipper;
    fn get_doc_values_skipper(&self, field: &str) -> Result<Option<Self::DocValuesSkipper>>;

    fn get_field_infos(&self) -> Result<Arc<FieldInfos>>;

    type Bits: Bits;
    fn get_live_docs(&self) -> Result<Option<Self::Bits>>;

    type PointValuesType: PointValues;
    fn get_point_values(&self, field: &str) -> Result<Option<Self::PointValuesType>>;

    fn check_integrity(&self) -> Result<()> {
        Err(LuceneError::unsupported_operation(""))
    }
}

// DummyPostingsEnum from  EmptyTerms's EmptyTermsEnum's PostingsEnum
type LeafPostingsEnum<T> =
    Either2PostingsEnum<<<T as Terms>::TermsEnum as TermsEnum>::PostingsEnum, DummyPostingsEnum>;

// TermsEnum
pub type LRTermsEnum<LR> = <<LR as LeafReader>::Terms as Terms>::TermsEnum;
// TermState
pub type LRTermState<LR> =
    <<<LR as LeafReader>::Terms as Terms>::TermsEnum as TermsEnum>::TermState;
// NumericDocValues
pub type LRNumericDocValues<LR> = <LR as LeafReader>::NumericDocValues;
// ImpactsEnum
pub type LRImpactsEnum<LR> =
    <<<LR as LeafReader>::Terms as Terms>::TermsEnum as TermsEnum>::ImpactsEnum;
// PostingsEnum
pub type LRPosting<LR> =
    <<<LR as LeafReader>::Terms as Terms>::TermsEnum as TermsEnum>::PostingsEnum;
pub type LRNormNumericDocValues<LR> = <LR as LeafReader>::NormNumericDocValues;
