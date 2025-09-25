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
use crate::core::codecs::dummy::dummy_binary_doc_values::DummyBinaryDocValues;
use crate::core::codecs::dummy::dummy_doc_values_skipper::DummyDocValuesSkipper;
use crate::core::codecs::dummy::dummy_numeric_doc_values::DummyNumericDocValues;
use crate::core::codecs::dummy::dummy_sorted_doc_values::DummySortedDocValues;
use crate::core::codecs::dummy::dummy_sorted_numeric_doc_values::DummySortedNumericDocValues;
use crate::core::codecs::dummy::dummy_sorted_set_doc_values::DummySortedSetDocValues;
use crate::core::index::dummy::dummy_point_value_base::DummyPointValuesBase;
use crate::core::index::dummy::dummy_terms::DummyTerms;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::index_reader::IndexReader;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::term::Term;
use crate::core::util::dummy::dummy_bits::DummyBits;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

#[derive(Default)]
pub(crate) struct DocValuesLeafReader;

impl Display for DocValuesLeafReader {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", std::any::type_name::<Self>())
    }
}

impl IndexReader for DocValuesLeafReader {
    fn max_doc(&self) -> Result<i32> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn num_docs(&self) -> Result<i32> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn do_close(&mut self) -> Result<()> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn doc_freq(&self, term: &Term) -> Result<i32> {
        LeafReader::doc_freq(self, term)
    }

    fn total_term_freq(&self, term: &Term) -> Result<i64> {
        LeafReader::total_term_freq(self, term)
    }

    fn sum_doc_freq(&self, field: &str) -> Result<i64> {
        LeafReader::sum_doc_freq(self, field)
    }

    fn doc_count(&self, field: &str) -> Result<i32> {
        LeafReader::doc_count(self, field)
    }

    fn sum_total_term_freq(&self, field: &str) -> Result<i64> {
        LeafReader::sum_total_term_freq(self, field)
    }
}

impl LeafReader for DocValuesLeafReader {
    type Terms = DummyTerms;

    fn terms(&self, _field: &str) -> Result<Option<Self::Terms>> {
        Err(LuceneError::unsupported_operation(""))
    }

    type NumericDocValues = DummyNumericDocValues;

    fn get_numeric_doc_values(&self, _field: &str) -> Result<Option<Self::NumericDocValues>> {
        Err(LuceneError::unsupported_operation(""))
    }

    type BinaryDocValues = DummyBinaryDocValues;

    fn get_binary_doc_values(&self, _field: &str) -> Result<Option<Self::BinaryDocValues>> {
        Err(LuceneError::unsupported_operation(""))
    }

    type SortedDocValues = DummySortedDocValues;

    fn get_sorted_doc_values(&self, _field: &str) -> Result<Option<Self::SortedDocValues>> {
        Err(LuceneError::unsupported_operation(""))
    }

    type SortedNumericDocValues = DummySortedNumericDocValues;

    fn get_sorted_numeric_doc_values(
        &self,
        _field: &str,
    ) -> Result<Option<Self::SortedNumericDocValues>> {
        Err(LuceneError::unsupported_operation(""))
    }

    type SortedSetDocValues = DummySortedSetDocValues;

    fn get_sorted_set_doc_values(&self, _field: &str) -> Result<Option<Self::SortedSetDocValues>> {
        Err(LuceneError::unsupported_operation(""))
    }

    type NormNumericDocValues = DummyNumericDocValues;

    fn get_norm_values(&self, _field: &str) -> Result<Option<Self::NormNumericDocValues>> {
        Err(LuceneError::unsupported_operation(""))
    }

    type DocValuesSkipper = DummyDocValuesSkipper;

    fn get_doc_values_skipper(&self, _field: &str) -> Result<Option<Self::DocValuesSkipper>> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn get_field_infos(&self) -> Result<Arc<FieldInfos>> {
        Err(LuceneError::unsupported_operation(""))
    }

    type Bits = DummyBits;

    fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
        Err(LuceneError::unsupported_operation(""))
    }

    type PointValuesType = DummyPointValuesBase;

    fn get_point_values(&self, _field: &str) -> Result<Option<Self::PointValuesType>> {
        Err(LuceneError::unsupported_operation(""))
    }

    fn check_integrity(&self) -> Result<()> {
        Err(LuceneError::unsupported_operation(""))
    }
}
