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
use crate::core::codecs::doc_values_producer::{DocValuesProducer, DocValuesProducerEnum2};
use crate::core::codecs::dummy::dummy_doc_values_skipper::DummyDocValuesSkipper;
use crate::core::codecs::dummy::dummy_mutable_point_tree::DummyMutablePointTree;
use crate::core::codecs::fields_producer::{FieldsProducer, FieldsProducerEnum2};
use crate::core::codecs::norms_producer::{NormsProducer, NormsProducerEnum2};
use crate::core::codecs::points_reader::{PointsReader, PointsReaderEnum2};
use crate::core::codecs::stored_fields_reader::{
    DefaultStoredFieldsReader, StoredFieldsReader, StoredFieldsReaderEnum2,
};
use crate::core::codecs::stored_fields_writer::StoredFieldsWriter;
use crate::core::codecs::term_vectors_reader::{
    DefaultTermVectorsReader, TermVectorsReader, TermVectorsReaderEnum2,
};
use crate::core::index::binary_doc_values_writer::{BinaryDVs, SortingBinaryDocValues};
use crate::core::index::codec_reader::{
    CRBits, CRDocValuesProducer, CRFieldsProducer, CRNormsProducer, CRPointsReader,
    CRStoredFieldsReader, CRTermVectorsReader, CodecReader,
};
use crate::core::index::dummy::dummy_cache_helper::DummyCacheHelper;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::fields::Fields;
use crate::core::index::filter_codec_reader::FilterCodecReader;
use crate::core::index::filter_leaf_reader::FilterTerms;
use crate::core::index::freq_prox_terms_writer::SortingTerms;
use crate::core::index::index_reader::{IndexReader, IndexReaderBase};
use crate::core::index::leaf_metadata::LeafMetaData;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::numeric_doc_values_writer::{NumericDVs, SortingNumericDocValues};
use crate::core::index::point_values::{
    IntersectVisitor, PointTree, PointTreeEnum, PointValues, Relation,
};
use crate::core::index::sorted_doc_values::SortedDocValues;
use crate::core::index::sorted_doc_values_writer::SortingSortedDocValues;
use crate::core::index::sorted_numeric_doc_values_writer::{
    LongValues, SortingSortedNumericDocValues,
};
use crate::core::index::sorted_set_doc_values_writer::{
    DocOrds, START_BITS_PER_VALUE, SortingSortedSetDocValues,
};
use crate::core::index::sorter::{DocMap, DocMapImpl, Sorter};
use crate::core::index::stored_field_visitor::StoredFieldVisitor;
use crate::core::index::stored_fields::{RawStoredFieldsReader, StoredFields};
use crate::core::index::term::Term;
use crate::core::index::term_vectors::{RawTermVectors, TermVectors};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
use crate::core::search::sort::Sort;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bits::{Bits, BitsEnum2};
use crate::core::util::clone::TryClone;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::packed::PackedInts;
use parking_lot::{Mutex, MutexGuard};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

#[derive(Clone)]
pub enum CachedObject {
    Numeric(NumericDVs<FixedBitSet>),
    Binary(BinaryDVs),
    Sorted(Arc<Vec<i32>>),
    SortedNumeric(LongValues),
    SortedSet(DocOrds),
}
/// An [`CodecReader`] which supports sorting documents by a given `Sort`. This can be used to
/// re-sort an index after it has been created by wrapping all readers of the index with this reader
/// and adding it to a fresh [`IndexWriter`](crate::core::index::index_writer::IndexWriter) via
/// `IndexWriter::add_indexes(CodecReader...)`.
///
/// **NOTE**: This reader should only be used for merging. Pulling fields from this reader might be
/// very costly and memory intensive.
pub struct SortingCodecReader<CR, DM>
where
    CR: CodecReader,
    DM: DocMap + Clone,
{
    base: FilterCodecReader<CR>,
    doc_map: DM,
    meta_data: LeafMetaData,
    inner: Arc<Mutex<Inner>>,
    index_base: IndexReaderBase,
}
pub struct Inner {
    // we try to cache the last used DV or Norms instance since during merge
    // this instance is used more than once. We could in addition to this single instance
    // also cache the fields that are used for sorting since we do the work twice for these fields
    cached_field: Option<String>,
    cache_is_norms: bool,
    cached_object: Option<CachedObject>,
    cache_stats: HashMap<String, i32>,
    sort: Option<Arc<Sort>>,
}

impl<CR, DM> SortingCodecReader<CR, DM>
where
    CR: CodecReader,
    DM: DocMap + Clone,
{
    pub fn new(base: CR, doc_map: DM, meta_data: LeafMetaData) -> Self {
        let inner = Arc::new(Mutex::new(Inner {
            cached_field: None,
            cache_is_norms: false,
            cached_object: None,
            cache_stats: HashMap::new(),
            sort: meta_data.sort.clone(),
        }));
        Self {
            base: FilterCodecReader::new(base),
            doc_map,
            meta_data,
            inner,
            index_base: IndexReaderBase::new(),
        }
    }
}

impl<CR, DM> LeafReader for SortingCodecReader<CR, DM>
where
    CR: CodecReader,
    DM: DocMap + Clone,
{
    type CacheHelper = DummyCacheHelper;

    fn get_core_cache_helper_ref(&self) -> Result<Option<&Self::CacheHelper>> {
        Ok(None)
    }

    fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
        Ok(None)
    }

    type Terms = <FilterCodecReader<CR> as LeafReader>::Terms;

    fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
        LeafReader::terms(&self.base, field)
    }

    type NumericDocValues = <FilterCodecReader<CR> as LeafReader>::NumericDocValues;

    fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>> {
        LeafReader::get_numeric_doc_values(&self.base, field)
    }

    type BinaryDocValues = <FilterCodecReader<CR> as LeafReader>::BinaryDocValues;

    fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
        LeafReader::get_binary_doc_values(&self.base, field)
    }

    type SortedDocValues = <FilterCodecReader<CR> as LeafReader>::SortedDocValues;

    fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
        LeafReader::get_sorted_doc_values(&self.base, field)
    }

    type SortedNumericDocValues = <FilterCodecReader<CR> as LeafReader>::SortedNumericDocValues;

    fn get_sorted_numeric_doc_values(
        &self,
        field: &str,
    ) -> Result<Option<Self::SortedNumericDocValues>> {
        LeafReader::get_sorted_numeric_doc_values(&self.base, field)
    }

    type SortedSetDocValues = <FilterCodecReader<CR> as LeafReader>::SortedSetDocValues;

    fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>> {
        LeafReader::get_sorted_set_doc_values(&self.base, field)
    }

    type NormNumericDocValues = <FilterCodecReader<CR> as LeafReader>::NormNumericDocValues;

    fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>> {
        LeafReader::get_norm_values(&self.base, field)
    }

    type DocValuesSkipper = <FilterCodecReader<CR> as LeafReader>::DocValuesSkipper;

    fn get_doc_values_skipper(&self, field: &str) -> Result<Option<Self::DocValuesSkipper>> {
        LeafReader::get_doc_values_skipper(&self.base, field)
    }

    fn get_field_infos(&self) -> Result<Arc<FieldInfos>> {
        self.base.get_field_infos()
    }

    type Bits = SortingBitsImpl<CRBits<CR>, DM>;

    fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
        Ok(self
            .base
            .in_
            .get_live_docs()?
            .map(|in_live_docs| SortingBitsImpl::new(in_live_docs, self.doc_map.clone())))
    }

    type PointValues = <FilterCodecReader<CR> as LeafReader>::PointValues;

    fn get_point_values(&self, field: &str) -> Result<Option<Self::PointValues>> {
        LeafReader::get_point_values(&self.base, field)
    }

    fn get_metadata(&self) -> Result<&LeafMetaData> {
        Ok(&self.meta_data)
    }
}

impl<CR, DM> IndexReader for SortingCodecReader<CR, DM>
where
    CR: CodecReader,
    DM: DocMap + Clone,
{
    type TermVectors = <FilterCodecReader<CR> as IndexReader>::TermVectors;

    fn term_vectors(&self) -> Result<Self::TermVectors> {
        IndexReader::term_vectors(&self.base)
    }

    fn max_doc(&self) -> Result<i32> {
        self.base.max_doc()
    }

    fn num_docs(&self) -> Result<i32> {
        self.base.num_docs()
    }

    type StoredFields = <FilterCodecReader<CR> as IndexReader>::StoredFields;

    fn stored_fields(&self) -> Result<Self::StoredFields> {
        IndexReader::stored_fields(&self.base)
    }

    fn do_close(&self) -> Result<()> {
        self.base.do_close()
    }

    type ReaderCacheHelper = DummyCacheHelper;

    fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
        Ok(None)
    }

    fn doc_freq(&self, term: &Term) -> Result<i32> {
        IndexReader::doc_freq(&self.base, term)
    }

    fn total_term_freq(&self, term: &Term) -> Result<i64> {
        IndexReader::total_term_freq(&self.base, term)
    }

    fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
        IndexReader::get_sum_doc_freq(&self.base, field)
    }
    fn get_doc_count(&self, field: &str) -> Result<i32> {
        IndexReader::get_doc_count(&self.base, field)
    }

    fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
        IndexReader::get_sum_total_term_freq(&self.base, field)
    }

    fn index_base(&self) -> &IndexReaderBase {
        &self.index_base
    }
}

impl<CR, DM> Display for SortingCodecReader<CR, DM>
where
    CR: CodecReader,
    DM: DocMap + Clone,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "SortingCodecReader({})", self.base)
    }
}

impl<CR, DM> CodecReader for SortingCodecReader<CR, DM>
where
    CR: CodecReader,
    DM: DocMap + Clone,
{
    type StoredFieldsReader = StoredFieldsReaderImpl<CRStoredFieldsReader<CR>, DM>;
    type TermVectorsReader = TermVectorsReaderImpl<CRTermVectorsReader<CR>, DM>;
    type NormsProducer = NormsProducerImpl<CRNormsProducer<CR>, DM>;
    type DocValuesProducer = DocValuesProducerImpl<CRDocValuesProducer<CR>, DM>;
    type FieldsProducer = FieldsProducerImpl<CRFieldsProducer<CR>, DM>;
    type PointsReader = PointsReaderImpl<CRPointsReader<CR>, DM>;

    fn get_fields_reader(&self) -> Result<Option<Self::StoredFieldsReader>> {
        Ok(self
            .base
            .in_
            .get_fields_reader()?
            .map(|delegate| new_stored_fields_reader(delegate, self.doc_map.clone())))
    }

    fn get_term_vectors_reader(&self) -> Result<Option<Self::TermVectorsReader>> {
        let delegate = self
            .base
            .in_
            .get_term_vectors_reader()?
            .ok_or_else(|| LuceneError::illegal_state("term vectors reader was None"))?;
        let v = new_term_vectors_reader(delegate, self.doc_map.clone());
        Ok(Some(v))
    }

    fn get_norms_reader(&self) -> Result<Option<Self::NormsProducer>> {
        let delegate = self
            .base
            .in_
            .get_norms_reader()?
            .ok_or_else(|| LuceneError::illegal_state("norm reader was None"))?;
        let v = NormsProducerImpl::new(
            delegate,
            self.inner.clone(),
            self.max_doc()?,
            self.doc_map.clone(),
        );
        Ok(Some(v))
    }

    fn get_doc_values_reader(&self) -> Result<Option<Self::DocValuesProducer>> {
        let delegate = self
            .base
            .in_
            .get_doc_values_reader()?
            .ok_or_else(|| LuceneError::illegal_state("norm reader was None"))?;
        let v = DocValuesProducerImpl::new(
            delegate,
            self.inner.clone(),
            self.max_doc()?,
            self.doc_map.clone(),
        );
        Ok(Some(v))
    }

    fn get_postings_reader(&self) -> Result<Option<Self::FieldsProducer>> {
        let posting_reader = self
            .base
            .in_
            .get_postings_reader()?
            .ok_or_else(|| LuceneError::illegal_state("postings reader was None"))?;
        let field_infos = self.base.in_.get_field_infos()?;
        Ok(Some(FieldsProducerImpl::new(
            posting_reader,
            self.doc_map.clone(),
            field_infos,
        )))
    }

    fn get_points_reader(&self) -> Result<Option<Self::PointsReader>> {
        let delegate = self
            .base
            .in_
            .get_points_reader()?
            .ok_or_else(|| LuceneError::illegal_state("points reader was None"))?;
        Ok(Some(PointsReaderImpl::new(delegate, self.doc_map.clone())))
    }
}

fn new_term_vectors_reader<T, DM>(delegate: T, doc_map: DM) -> TermVectorsReaderImpl<T, DM>
where
    T: TermVectorsReader,
    DM: DocMap + Clone,
{
    TermVectorsReaderImpl::new(delegate, doc_map)
}

pub struct TermVectorsReaderImpl<T, DM>
where
    T: TermVectorsReader,
    DM: DocMap + Clone,
{
    delegate: T,
    doc_map: DM,
}
impl<T, DM> TermVectorsReaderImpl<T, DM>
where
    T: TermVectorsReader,
    DM: DocMap + Clone,
{
    pub fn new(delegate: T, doc_map: DM) -> Self {
        Self { delegate, doc_map }
    }
}

impl<T, DM> TermVectors for TermVectorsReaderImpl<T, DM>
where
    DM: DocMap + Clone,
    T: TermVectorsReader,
{
    fn prefetch(&mut self, doc_id: i32) -> Result<()> {
        self.delegate.prefetch(self.doc_map.new_to_old(doc_id)?)
    }

    type Fields = T::Fields;

    fn get(&mut self, doc: i32) -> Result<Option<Self::Fields>> {
        self.delegate.get(self.doc_map.new_to_old(doc)?)
    }

    type Terms = <Self::Fields as Fields>::Terms;

    fn get_field_terms(
        &mut self,
        doc: i32,
        field: &str,
    ) -> Result<Option<<Self::Fields as Fields>::Terms>> {
        self.default_get_field_terms(doc, field)
    }
}

impl<T, DM> RawTermVectors for TermVectorsReaderImpl<T, DM>
where
    DM: DocMap + Clone,
    T: TermVectorsReader,
{
    type IndexInput = <T as RawTermVectors>::IndexInput;

    fn raw_term_vectors_mut(&mut self) -> Result<&mut DefaultTermVectorsReader<Self::IndexInput>> {
        self.delegate.raw_term_vectors_mut()
    }

    fn raw_term_vectors(&self) -> Result<&DefaultTermVectorsReader<Self::IndexInput>> {
        self.delegate.raw_term_vectors()
    }
}

impl<T, DM> Clone for TermVectorsReaderImpl<T, DM>
where
    DM: DocMap + Clone,
    T: TermVectorsReader,
{
    fn clone(&self) -> Self {
        new_term_vectors_reader(self.delegate.clone(), self.doc_map.clone())
    }
}

impl<T, DM> TermVectorsReader for TermVectorsReaderImpl<T, DM>
where
    T: TermVectorsReader,
    DM: DocMap + Clone,
{
    fn check_integrity(&self) -> Result<()> {
        self.delegate.check_integrity()
    }
}

pub struct NormsProducerImpl<NP, DM>
where
    NP: NormsProducer,
    DM: DocMap + Clone,
{
    delegate: NP,
    inner: Arc<Mutex<Inner>>,
    max_doc: i32,
    doc_map: DM,
}
impl<NP, DM> NormsProducerImpl<NP, DM>
where
    NP: NormsProducer,
    DM: DocMap + Clone,
{
    fn new(delegate: NP, inner: Arc<Mutex<Inner>>, max_doc: i32, doc_map: DM) -> Self {
        Self {
            delegate,
            inner,
            max_doc,
            doc_map,
        }
    }
}

impl<NP, DM> NormsProducer for NormsProducerImpl<NP, DM>
where
    NP: NormsProducer,
    DM: DocMap + Clone,
{
    type NumericDocValues = SortingNumericDocValues<FixedBitSet>;

    fn get_norms(&self, field: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
        let v = get_or_create_norms(
            &field.name,
            || {
                let numeric = get_numeric_doc_values(
                    &mut self.delegate.get_norms(field)?,
                    self.max_doc as usize,
                    &self.doc_map,
                )?;
                Ok(CachedObject::Numeric(numeric))
            },
            &self.inner,
        )?;

        let numeric = match v {
            CachedObject::Numeric(numeric) => numeric,
            _ => {
                return Err(LuceneError::illegal_state(
                    "CachedObject is not Numeric (norms)".to_string(),
                ));
            },
        };

        Ok(SortingNumericDocValues::new(numeric))
    }

    fn check_integrity(&self) -> Result<()> {
        self.delegate.check_integrity()
    }
}

pub struct DocValuesProducerImpl<DVP, DM>
where
    DVP: DocValuesProducer,
    DM: DocMap + Clone,
{
    delegate: DVP,
    inner: Arc<Mutex<Inner>>,
    max_doc: i32,
    doc_map: DM,
}
impl<DVP, DM> DocValuesProducerImpl<DVP, DM>
where
    DVP: DocValuesProducer,
    DM: DocMap + Clone,
{
    fn new(delegate: DVP, inner: Arc<Mutex<Inner>>, max_doc: i32, doc_map: DM) -> Self {
        Self {
            delegate,
            inner,
            max_doc,
            doc_map,
        }
    }
}

impl<DVP, DM> DocValuesProducer for DocValuesProducerImpl<DVP, DM>
where
    DVP: DocValuesProducer,
    DM: DocMap + Clone,
{
    type NumericDocValues = SortingNumericDocValues<FixedBitSet>;

    fn get_numeric(&self, field: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
        let v = get_or_create_dv(
            &field.name,
            || {
                let v = get_numeric_doc_values(
                    &mut self.delegate.get_numeric(field)?,
                    self.max_doc as usize,
                    &self.doc_map,
                )?;
                Ok(CachedObject::Numeric(v))
            },
            &self.inner,
        )?;
        let numeric = match v {
            CachedObject::Numeric(numeric) => numeric,
            _ => {
                return Err(LuceneError::illegal_state(
                    "CachedObject is not Numeric".to_string(),
                ));
            },
        };
        Ok(SortingNumericDocValues::new(numeric))
    }

    type BinaryDocValues = SortingBinaryDocValues;

    fn get_binary(&self, field: &Arc<FieldInfo>) -> Result<Self::BinaryDocValues> {
        let v = get_or_create_dv(
            &field.name,
            || {
                let binary = BinaryDVs::new(
                    self.max_doc as usize,
                    &self.doc_map,
                    &mut self.delegate.get_binary(field)?,
                )?;
                Ok(CachedObject::Binary(binary))
            },
            &self.inner,
        )?;

        let binary = match v {
            CachedObject::Binary(binary) => binary,
            _ => {
                return Err(LuceneError::illegal_state(
                    "CachedObject is not Binary".to_string(),
                ));
            },
        };

        Ok(SortingBinaryDocValues::new(binary))
    }

    type SortedDocValues = SortingSortedDocValues<<DVP as DocValuesProducer>::SortedDocValues>;

    fn get_sorted(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedDocValues> {
        let mut old_doc_values = self.delegate.get_sorted(field)?;

        let v = get_or_create_dv(
            &field.name,
            || {
                let max_doc = self.max_doc as usize;

                let mut ords = vec![-1; max_doc];

                let mut doc_id = old_doc_values.next_doc()?;
                while doc_id != NO_MORE_DOCS {
                    let new_doc_id = self.doc_map.old_to_new(doc_id)? as usize;
                    ords[new_doc_id] = old_doc_values.ord_value()?;
                    doc_id = old_doc_values.next_doc()?;
                }

                Ok(CachedObject::Sorted(Arc::new(ords)))
            },
            &self.inner,
        )?;

        let ords = match v {
            CachedObject::Sorted(ords) => ords,
            _ => {
                return Err(LuceneError::illegal_state(
                    "CachedObject is not SortedOrds".to_string(),
                ));
            },
        };

        Ok(SortingSortedDocValues::new(old_doc_values, ords))
    }

    type SortedNumericDocValues =
        SortingSortedNumericDocValues<<DVP as DocValuesProducer>::SortedNumericDocValues>;

    fn get_sorted_numeric(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedNumericDocValues> {
        let mut old_doc_values = self.delegate.get_sorted_numeric(field)?;

        let v = get_or_create_dv(
            &field.name,
            || {
                let long_values = LongValues::new(
                    self.max_doc as usize,
                    &self.doc_map,
                    &mut old_doc_values,
                    PackedInts::FAST,
                )?;
                Ok(CachedObject::SortedNumeric(long_values))
            },
            &self.inner,
        )?;
        let long_values = match v {
            CachedObject::SortedNumeric(v) => v,
            _ => {
                return Err(LuceneError::illegal_state(
                    "CachedObject is not SortedNumeric".to_string(),
                ));
            },
        };

        Ok(SortingSortedNumericDocValues::new(
            old_doc_values,
            long_values,
        ))
    }

    type SortedSetDocValues =
        SortingSortedSetDocValues<<DVP as DocValuesProducer>::SortedSetDocValues>;

    fn get_sorted_set(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedSetDocValues> {
        let mut old_doc_values = self.delegate.get_sorted_set(field)?;

        let v = get_or_create_dv(
            &field.name,
            || {
                let doc_ords = DocOrds::new(
                    self.max_doc,
                    &self.doc_map,
                    &mut old_doc_values,
                    PackedInts::FAST,
                    START_BITS_PER_VALUE,
                )?;
                Ok(CachedObject::SortedSet(doc_ords))
            },
            &self.inner,
        )?;

        let doc_ords = match v {
            CachedObject::SortedSet(doc_ords) => doc_ords,
            _ => {
                return Err(LuceneError::illegal_state(
                    "CachedObject is not SortedSet".to_string(),
                ));
            },
        };

        Ok(SortingSortedSetDocValues::new(old_doc_values, doc_ords))
    }

    type DocValuesSkipper = DummyDocValuesSkipper;

    fn get_skipper(&self, _field: &Arc<FieldInfo>) -> Result<Option<Self::DocValuesSkipper>> {
        Ok(None)
    }

    fn check_integrity(&self) -> Result<()> {
        self.delegate.check_integrity()
    }
}
fn get_or_create_dv<F>(field: &str, supplier: F, inner: &Arc<Mutex<Inner>>) -> Result<CachedObject>
where
    F: FnOnce() -> Result<CachedObject>,
{
    get_or_create(field, false, supplier, inner)
}
fn get_or_create_norms<F>(
    field: &str,
    supplier: F,
    inner: &Arc<Mutex<Inner>>,
) -> Result<CachedObject>
where
    F: FnOnce() -> Result<CachedObject>,
{
    get_or_create(field, true, supplier, inner)
}

fn get_or_create<F>(
    field: &str,
    norms: bool,
    supplier: F,
    inner: &Arc<Mutex<Inner>>,
) -> Result<CachedObject>
where
    F: FnOnce() -> Result<CachedObject>,
{
    let mut inner = inner.lock();
    if inner.cached_field.as_deref() != Some(field) || inner.cache_is_norms != norms {
        debug_assert!(assert_created_only_once(field, norms, &mut inner));
        let new_object = supplier()?;
        inner.cached_field = Some(field.to_string());
        inner.cache_is_norms = norms;
        inner.cached_object = Some(new_object);
    }
    debug_assert!(inner.cached_object.is_some());
    let v = inner.cached_object.as_ref().unwrap().clone();
    Ok(v)
}
fn get_numeric_doc_values<N, DM>(
    old_numerics: &mut N,
    max_doc: usize,
    doc_map: &DM,
) -> Result<NumericDVs<FixedBitSet>>
where
    N: NumericDocValues,
    DM: DocMap + Clone,
{
    let mut docs_with_field = FixedBitSet::new(max_doc);
    let mut values = vec![0i64; max_doc];

    let doc_id = old_numerics.next_doc()?;
    loop {
        if doc_id == NO_MORE_DOCS {
            break;
        }
        let new_doc_id = doc_map.old_to_new(doc_id)? as usize;
        docs_with_field.set(new_doc_id);
        values[new_doc_id] = old_numerics.long_value()?;
    }

    Ok(NumericDVs::new(values, Some(docs_with_field)))
}
fn assert_created_only_once(field: &str, norms: bool, inner: &mut MutexGuard<'_, Inner>) -> bool {
    // this is mainly there to make sure we change anything in the way we merge we realize it early
    let key = format!("{}N:{}", field, norms);

    let times_cached = {
        let stats = &mut inner.cache_stats;
        let entry = stats.entry(key).or_insert(0);
        *entry += 1;
        *entry
    };

    if times_cached > 1 {
        debug_assert!(!norms, "[{}] norms must not be cached twice", field);

        let mut is_sort_field = false;

        // For things that aren't sort fields, it's possible for sort to be null here
        // In the event that we accidentally cache twice, its better not to throw an NPE
        if let Some(ref sort) = inner.sort {
            for sf in sort.get_sort() {
                if Some(field) == sf.get_field() {
                    is_sort_field = true;
                    break;
                }
            }
        }

        debug_assert!(
            times_cached == 2,
            "[{}] must not be cached more than twice but was cached: {} times is_sort_field: {}",
            field,
            times_cached,
            is_sort_field
        );

        debug_assert!(
            is_sort_field,
            "only sort fields should be cached twice but [{}] is not a sort field",
            field
        );
    }

    true
}

pub struct PointsReaderImpl<PR, DM>
where
    PR: PointsReader,
    DM: DocMap + Clone,
{
    delegate: PR,
    doc_map: DM,
}
impl<PR, DM> PointsReaderImpl<PR, DM>
where
    PR: PointsReader,
    DM: DocMap + Clone,
{
    fn new(delegate: PR, doc_map: DM) -> Self {
        Self { delegate, doc_map }
    }
}
impl<PR, DM> PointsReader for PointsReaderImpl<PR, DM>
where
    PR: PointsReader,
    DM: DocMap + Clone,
{
    fn check_integrity(&self) -> Result<()> {
        self.delegate.check_integrity()
    }

    type PointValuesType = SortingPointValues<<PR as PointsReader>::PointValuesType, DM>;

    fn get_values(&self, field: &str) -> Result<Option<Self::PointValuesType>> {
        Ok(self
            .delegate
            .get_values(field)?
            .map(|values| SortingPointValues::new(values, self.doc_map.clone())))
    }
}

pub struct SortingPointValues<PV, DM>
where
    PV: PointValues,
    DM: DocMap + Clone,
{
    in_: PV,
    doc_map: DM,
}
impl<PV, DM> SortingPointValues<PV, DM>
where
    PV: PointValues,
    DM: DocMap + Clone,
{
    pub fn new(delegate: PV, doc_map: DM) -> Self {
        Self {
            in_: delegate,
            doc_map,
        }
    }
}

impl<PV, DM> Clone for SortingPointValues<PV, DM>
where
    DM: Clone + DocMap,
    PV: PointValues,
{
    fn clone(&self) -> Self {
        Self::new(self.in_.clone(), self.doc_map.clone())
    }
}

impl<PV, DM> PointValues for SortingPointValues<PV, DM>
where
    PV: PointValues,
    DM: DocMap + Clone,
{
    fn get_min_packed_value(&self) -> Result<Option<Cow<'_, [u8]>>> {
        self.in_.get_min_packed_value()
    }

    fn get_max_packed_value(&self) -> Result<Option<Cow<'_, [u8]>>> {
        self.in_.get_max_packed_value()
    }

    fn get_num_dimensions(&self) -> Result<usize> {
        self.in_.get_num_dimensions()
    }

    fn get_num_index_dimensions(&self) -> Result<usize> {
        self.in_.get_num_index_dimensions()
    }

    fn get_bytes_per_dimension(&self) -> Result<usize> {
        self.in_.get_bytes_per_dimension()
    }

    fn size(&self) -> Result<usize> {
        self.in_.size()
    }

    fn get_doc_count(&self) -> Result<i32> {
        self.in_.get_doc_count()
    }

    type PointTree = SortingPointTree<
        PointTreeEnum<<PV as PointValues>::MutablePointTree, <PV as PointValues>::PointTree>,
        DM,
    >;
    type MutablePointTree = DummyMutablePointTree;

    fn get_point_tree(&self) -> Result<PointTreeEnum<Self::MutablePointTree, Self::PointTree>> {
        let tree = self.in_.get_point_tree()?;
        Ok(PointTreeEnum::Other(SortingPointTree::new(
            tree,
            self.doc_map.clone(),
        )))
    }
}

pub struct SortingPointTree<PT, DM>
where
    PT: PointTree,
    DM: DocMap + Clone,
{
    index_tree: PT,
    doc_map: DM,
}
impl<PT, DM> SortingPointTree<PT, DM>
where
    PT: PointTree,
    DM: DocMap + Clone,
{
    pub fn new(delegate: PT, doc_map: DM) -> Self {
        Self {
            index_tree: delegate,
            doc_map,
        }
    }
}

impl<PT, DM> TryClone for SortingPointTree<PT, DM>
where
    DM: Clone + DocMap,
    PT: PointTree,
{
    fn try_clone(&self) -> Result<Self>
    where
        Self: Sized,
    {
        Ok(SortingPointTree::new(
            self.index_tree.try_clone()?,
            self.doc_map.clone(),
        ))
    }
}

impl<PT, DM> PointTree for SortingPointTree<PT, DM>
where
    PT: PointTree,
    DM: DocMap + Clone,
{
    fn move_to_child(&mut self) -> Result<bool> {
        self.index_tree.move_to_child()
    }

    fn move_to_sibling(&mut self) -> Result<bool> {
        self.index_tree.move_to_sibling()
    }

    fn move_to_parent(&mut self) -> Result<bool> {
        self.index_tree.move_to_parent()
    }

    fn get_min_packed_value(&self) -> Result<Cow<'_, [u8]>> {
        self.index_tree.get_min_packed_value()
    }

    fn get_max_packed_value(&self) -> Result<Cow<'_, [u8]>> {
        self.index_tree.get_max_packed_value()
    }

    fn size(&self) -> Result<usize> {
        self.index_tree.size()
    }

    fn visit_doc_ids<IV>(&mut self, visitor: &mut IV) -> Result<()>
    where
        IV: IntersectVisitor,
    {
        let mut visitor = SortingIntersectVisitor::new(self.doc_map.clone(), visitor);
        self.index_tree.visit_doc_values(&mut visitor)
    }

    fn visit_doc_values<IV>(&mut self, visitor: &mut IV) -> Result<()>
    where
        IV: IntersectVisitor,
    {
        let mut visitor = SortingIntersectVisitor::new(self.doc_map.clone(), visitor);
        self.index_tree.visit_doc_values(&mut visitor)
    }
}

pub struct SortingIntersectVisitor<'a, DM, IV>
where
    DM: DocMap + Clone,
    IV: IntersectVisitor,
{
    doc_map: DM,
    visitor: &'a mut IV,
}
impl<'a, DM, IV> SortingIntersectVisitor<'a, DM, IV>
where
    DM: DocMap + Clone,
    IV: IntersectVisitor,
{
    fn new(doc_map: DM, visitor: &'a mut IV) -> Self {
        Self { doc_map, visitor }
    }
}
impl<DM, IV> IntersectVisitor for SortingIntersectVisitor<'_, DM, IV>
where
    DM: DocMap + Clone,
    IV: IntersectVisitor,
{
    fn visit(&mut self, doc_id: i32) -> Result<()> {
        self.visitor.visit(self.doc_map.old_to_new(doc_id)?)
    }

    fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
        self.visitor
            .visit_with_packed_value(self.doc_map.old_to_new(doc_id)?, packed_value)
    }

    fn compare(&self, min_packed_value: &[u8], max_packed_value: &[u8]) -> Result<Relation> {
        self.visitor.compare(min_packed_value, max_packed_value)
    }
}
pub struct SortingBitsImpl<B, DM>
where
    B: Bits,
    DM: DocMap + Clone,
{
    in_: B,
    doc_map: DM,
}
impl<B, DM> SortingBitsImpl<B, DM>
where
    B: Bits,
    DM: DocMap + Clone,
{
    fn new(in_: B, doc_map: DM) -> Self {
        Self { in_, doc_map }
    }
}
impl<B, DM> Bits for SortingBitsImpl<B, DM>
where
    B: Bits,
    DM: DocMap + Clone,
{
    fn get(&self, index: usize) -> Result<bool> {
        self.in_
            .get(self.doc_map.new_to_old(index as i32)? as usize)
    }

    fn length(&self) -> usize {
        self.in_.length()
    }
}
pub fn new_stored_fields_reader<SFR, DM>(
    delegate: SFR,
    doc_map: DM,
) -> StoredFieldsReaderImpl<SFR, DM>
where
    SFR: StoredFieldsReader,
    DM: DocMap + Clone,
{
    StoredFieldsReaderImpl::new(delegate, doc_map)
}

pub struct StoredFieldsReaderImpl<SFR, DM>
where
    SFR: StoredFieldsReader,
    DM: DocMap + Clone,
{
    delegate: SFR,
    doc_map: DM,
}
impl<SFR, DM> StoredFieldsReaderImpl<SFR, DM>
where
    SFR: StoredFieldsReader,
    DM: DocMap + Clone,
{
    fn new(delegate: SFR, doc_map: DM) -> Self {
        Self { delegate, doc_map }
    }
}

impl<SFR, DM> StoredFields for StoredFieldsReaderImpl<SFR, DM>
where
    DM: Clone + DocMap,
    SFR: StoredFieldsReader,
{
    fn prefetch(&mut self, doc_id: i32) -> Result<()> {
        self.delegate.prefetch(self.doc_map.new_to_old(doc_id)?)
    }

    fn document_with_visitor<S: StoredFieldsWriter>(
        &mut self,
        doc_id: i32,
        visitor: &mut impl StoredFieldVisitor,
        writer: Option<&mut S>,
    ) -> Result<()> {
        self.delegate
            .document_with_visitor(self.doc_map.new_to_old(doc_id)?, visitor, writer)
    }
}

impl<SFR, DM> Clone for StoredFieldsReaderImpl<SFR, DM>
where
    DM: Clone + DocMap,
    SFR: StoredFieldsReader,
{
    fn clone(&self) -> Self {
        new_stored_fields_reader(self.delegate.clone(), self.doc_map.clone())
    }
}

impl<SFR, DM> StoredFieldsReader for StoredFieldsReaderImpl<SFR, DM>
where
    SFR: StoredFieldsReader,
    DM: DocMap + Clone,
{
    fn check_integrity(&self) -> Result<()> {
        self.delegate.check_integrity()
    }
}

impl<SFR, DM> RawStoredFieldsReader for StoredFieldsReaderImpl<SFR, DM>
where
    SFR: RawStoredFieldsReader + StoredFieldsReader,
    DM: DocMap + Clone,
{
    type IndexInput = SFR::IndexInput;

    fn raw_stored_fields(&mut self) -> Result<&mut DefaultStoredFieldsReader<Self::IndexInput>> {
        self.delegate.raw_stored_fields()
    }
}

pub struct FieldsProducerImpl<FP, DM>
where
    FP: FieldsProducer,
    DM: DocMap + Clone,
{
    postings_reader: FP,
    doc_map: DM,
    field_infos: Arc<FieldInfos>,
}

impl<FP, DM> FieldsProducerImpl<FP, DM>
where
    FP: FieldsProducer,
    DM: DocMap + Clone,
{
    fn new(postings_reader: FP, doc_map: DM, field_infos: Arc<FieldInfos>) -> Self {
        Self {
            postings_reader,
            doc_map,
            field_infos,
        }
    }
}

impl<FP, DM> Fields for FieldsProducerImpl<FP, DM>
where
    FP: FieldsProducer,
    DM: DocMap + Clone,
{
    type FieldIter<'a>
        = FP::FieldIter<'a>
    where
        Self: 'a;

    fn iterator(&self) -> Result<Self::FieldIter<'_>> {
        self.postings_reader.iterator()
    }

    type Terms = SortingTerms<<FP as Fields>::Terms, DM>;

    fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
        match self.postings_reader.terms(field)? {
            Some(terms) => {
                let filter = FilterTerms::new(terms);
                let field_info = self
                    .field_infos
                    .field_info_by_name(field)
                    .ok_or_else(|| LuceneError::illegal_state(format!("{}'s field info", field)))?;
                Ok(Some(SortingTerms::new(
                    filter,
                    *field_info.get_index_options(),
                    self.doc_map.clone(),
                )))
            },
            None => Ok(None),
        }
    }

    fn size(&self) -> Result<i32> {
        self.postings_reader.size()
    }
}

impl<FP, DM> FieldsProducer for FieldsProducerImpl<FP, DM>
where
    FP: FieldsProducer,
    DM: DocMap + Clone,
{
    fn check_integrity(&self) -> Result<()> {
        self.postings_reader.check_integrity()
    }
}

pub struct FilterCodecReaderImpl<CR>
where
    CR: CodecReader,
{
    base: FilterCodecReader<CR>,
    new_meta_data: LeafMetaData,
    index_base: IndexReaderBase,
}
impl<CR> FilterCodecReaderImpl<CR>
where
    CR: CodecReader,
{
    pub fn new(reader: CR, new_meta_data: LeafMetaData) -> Self {
        let base = FilterCodecReader::new(reader);
        Self {
            base,
            new_meta_data,
            index_base: IndexReaderBase::new(),
        }
    }
}

impl<CR> LeafReader for FilterCodecReaderImpl<CR>
where
    CR: CodecReader,
{
    type CacheHelper = DummyCacheHelper;

    fn get_core_cache_helper_ref(&self) -> Result<Option<&Self::CacheHelper>> {
        Ok(None)
    }

    fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
        Ok(None)
    }

    type Terms = <FilterCodecReader<CR> as LeafReader>::Terms;

    fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
        LeafReader::terms(&self.base, field)
    }

    type NumericDocValues = <FilterCodecReader<CR> as LeafReader>::NumericDocValues;

    fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>> {
        LeafReader::get_numeric_doc_values(&self.base, field)
    }

    type BinaryDocValues = <FilterCodecReader<CR> as LeafReader>::BinaryDocValues;

    fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
        LeafReader::get_binary_doc_values(&self.base, field)
    }

    type SortedDocValues = <FilterCodecReader<CR> as LeafReader>::SortedDocValues;

    fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
        LeafReader::get_sorted_doc_values(&self.base, field)
    }

    type SortedNumericDocValues = <FilterCodecReader<CR> as LeafReader>::SortedNumericDocValues;

    fn get_sorted_numeric_doc_values(
        &self,
        field: &str,
    ) -> Result<Option<Self::SortedNumericDocValues>> {
        LeafReader::get_sorted_numeric_doc_values(&self.base, field)
    }

    type SortedSetDocValues = <FilterCodecReader<CR> as LeafReader>::SortedSetDocValues;

    fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>> {
        LeafReader::get_sorted_set_doc_values(&self.base, field)
    }

    type NormNumericDocValues = <FilterCodecReader<CR> as LeafReader>::NormNumericDocValues;

    fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>> {
        LeafReader::get_norm_values(&self.base, field)
    }

    type DocValuesSkipper = <FilterCodecReader<CR> as LeafReader>::DocValuesSkipper;

    fn get_doc_values_skipper(&self, field: &str) -> Result<Option<Self::DocValuesSkipper>> {
        LeafReader::get_doc_values_skipper(&self.base, field)
    }

    fn get_field_infos(&self) -> Result<Arc<FieldInfos>> {
        self.base.get_field_infos()
    }

    type Bits = <FilterCodecReader<CR> as LeafReader>::Bits;

    fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
        LeafReader::get_live_docs(&self.base)
    }

    type PointValues = <FilterCodecReader<CR> as LeafReader>::PointValues;

    fn get_point_values(&self, field: &str) -> Result<Option<Self::PointValues>> {
        LeafReader::get_point_values(&self.base, field)
    }

    fn get_metadata(&self) -> Result<&LeafMetaData> {
        Ok(&self.new_meta_data)
    }
}

impl<CR> IndexReader for FilterCodecReaderImpl<CR>
where
    CR: CodecReader,
{
    type TermVectors = <FilterCodecReader<CR> as IndexReader>::TermVectors;

    fn term_vectors(&self) -> Result<Self::TermVectors> {
        IndexReader::term_vectors(&self.base)
    }

    fn max_doc(&self) -> Result<i32> {
        self.base.max_doc()
    }

    fn num_docs(&self) -> Result<i32> {
        self.base.num_docs()
    }

    type StoredFields = <FilterCodecReader<CR> as IndexReader>::StoredFields;

    fn stored_fields(&self) -> Result<Self::StoredFields> {
        IndexReader::stored_fields(&self.base)
    }

    fn do_close(&self) -> Result<()> {
        self.base.do_close()
    }

    type ReaderCacheHelper = DummyCacheHelper;

    fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
        Ok(None)
    }

    fn doc_freq(&self, term: &Term) -> Result<i32> {
        IndexReader::doc_freq(&self.base, term)
    }

    fn total_term_freq(&self, term: &Term) -> Result<i64> {
        IndexReader::total_term_freq(&self.base, term)
    }

    fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
        IndexReader::get_sum_doc_freq(&self.base, field)
    }

    fn get_doc_count(&self, field: &str) -> Result<i32> {
        IndexReader::get_doc_count(&self.base, field)
    }

    fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
        IndexReader::get_sum_total_term_freq(&self.base, field)
    }

    fn index_base(&self) -> &IndexReaderBase {
        &self.index_base
    }
}

impl<CR> Display for FilterCodecReaderImpl<CR>
where
    CR: CodecReader,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "SortingCodecReader({})", self.base)
    }
}

impl<CR> CodecReader for FilterCodecReaderImpl<CR>
where
    CR: CodecReader,
{
    type StoredFieldsReader = <FilterCodecReader<CR> as CodecReader>::StoredFieldsReader;
    type TermVectorsReader = <FilterCodecReader<CR> as CodecReader>::TermVectorsReader;
    type NormsProducer = <FilterCodecReader<CR> as CodecReader>::NormsProducer;
    type DocValuesProducer = <FilterCodecReader<CR> as CodecReader>::DocValuesProducer;
    type FieldsProducer = <FilterCodecReader<CR> as CodecReader>::FieldsProducer;
    type PointsReader = <FilterCodecReader<CR> as CodecReader>::PointsReader;

    fn get_fields_reader(&self) -> Result<Option<Self::StoredFieldsReader>> {
        self.base.get_fields_reader()
    }

    fn get_term_vectors_reader(&self) -> Result<Option<Self::TermVectorsReader>> {
        self.base.get_term_vectors_reader()
    }

    fn get_norms_reader(&self) -> Result<Option<Self::NormsProducer>> {
        self.base.get_norms_reader()
    }

    fn get_doc_values_reader(&self) -> Result<Option<Self::DocValuesProducer>> {
        self.base.get_doc_values_reader()
    }

    fn get_postings_reader(&self) -> Result<Option<Self::FieldsProducer>> {
        self.base.get_postings_reader()
    }

    fn get_points_reader(&self) -> Result<Option<Self::PointsReader>> {
        self.base.get_points_reader()
    }
}
/// Returns a sorted view of `reader` according to the order defined by `sort`.
///
/// If the reader is already sorted, this method may return the reader as-is.
pub fn wrap<CR>(reader: CR, sort: Sort) -> Result<SortingCodecReaderEnum<CR, Arc<DocMapImpl>>>
where
    CR: CodecReader,
{
    let sorter = Sorter::new(sort)?;
    let doc_map = sorter.sort_with_reader(&reader)?.map(Arc::new);
    wrap_with_doc_map(reader, doc_map, Some(Arc::new(sorter.sort)))
}
/// Expert: same as [`wrap_with`] but operates directly on a [`DocMap`].
pub fn wrap_with_doc_map<CR, DM>(
    reader: CR,
    doc_map: Option<DM>,
    sort: Option<Arc<Sort>>,
) -> Result<SortingCodecReaderEnum<CR, DM>>
where
    CR: CodecReader,
    DM: DocMap + Clone,
{
    let meta_data = reader.get_metadata()?;
    let new_meta_data = LeafMetaData::new(
        meta_data.get_created_version_major(),
        meta_data.get_min_version().clone(),
        sort,
        meta_data.get_has_blocks(),
    )?;
    match doc_map {
        Some(doc_map) => {
            if reader.max_doc()? != doc_map.size() {
                return Err(LuceneError::illegal_argument(format!(
                    "reader.maxDoc() should be equal to docMap.size(), got {} != {}",
                    reader.max_doc()?,
                    doc_map.size()
                )));
            }
            debug_assert!(Sorter::is_consistent(&doc_map)?);
            let v = SortingCodecReader::new(reader, doc_map, new_meta_data);
            Ok(SortingCodecReaderEnum::Sorting(v))
        },
        None => Ok(SortingCodecReaderEnum::Filter(FilterCodecReaderImpl::new(
            reader,
            new_meta_data,
        ))),
    }
}

pub enum SortingCodecReaderEnum<CR, DM>
where
    CR: CodecReader,
    DM: DocMap + Clone,
{
    Filter(FilterCodecReaderImpl<CR>),
    Sorting(SortingCodecReader<CR, DM>),
}

impl<CR, DM> LeafReader for SortingCodecReaderEnum<CR, DM>
where
    CR: CodecReader,
    DM: Clone + DocMap,
{
    type CacheHelper = DummyCacheHelper;

    fn get_core_cache_helper_ref(&self) -> Result<Option<&Self::CacheHelper>> {
        match self {
            SortingCodecReaderEnum::Filter(reader) => reader.get_core_cache_helper_ref(),
            SortingCodecReaderEnum::Sorting(reader) => reader.get_core_cache_helper_ref(),
        }
    }

    fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
        match self {
            SortingCodecReaderEnum::Filter(reader) => reader.get_core_cache_helper(),
            SortingCodecReaderEnum::Sorting(reader) => reader.get_core_cache_helper(),
        }
    }

    type Terms = <FilterCodecReader<CR> as LeafReader>::Terms;

    fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
        match self {
            SortingCodecReaderEnum::Filter(reader) => LeafReader::terms(reader, field),
            SortingCodecReaderEnum::Sorting(reader) => LeafReader::terms(reader, field),
        }
    }

    type NumericDocValues = <FilterCodecReader<CR> as LeafReader>::NumericDocValues;

    fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>> {
        match self {
            SortingCodecReaderEnum::Filter(reader) => {
                LeafReader::get_numeric_doc_values(reader, field)
            },
            SortingCodecReaderEnum::Sorting(reader) => {
                LeafReader::get_numeric_doc_values(reader, field)
            },
        }
    }

    type BinaryDocValues = <FilterCodecReader<CR> as LeafReader>::BinaryDocValues;

    fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
        match self {
            SortingCodecReaderEnum::Filter(reader) => {
                LeafReader::get_binary_doc_values(reader, field)
            },
            SortingCodecReaderEnum::Sorting(reader) => {
                LeafReader::get_binary_doc_values(reader, field)
            },
        }
    }

    type SortedDocValues = <FilterCodecReader<CR> as LeafReader>::SortedDocValues;

    fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
        match self {
            SortingCodecReaderEnum::Filter(reader) => {
                LeafReader::get_sorted_doc_values(reader, field)
            },
            SortingCodecReaderEnum::Sorting(reader) => {
                LeafReader::get_sorted_doc_values(reader, field)
            },
        }
    }

    type SortedNumericDocValues = <FilterCodecReader<CR> as LeafReader>::SortedNumericDocValues;

    fn get_sorted_numeric_doc_values(
        &self,
        field: &str,
    ) -> Result<Option<Self::SortedNumericDocValues>> {
        match self {
            SortingCodecReaderEnum::Filter(reader) => {
                LeafReader::get_sorted_numeric_doc_values(reader, field)
            },
            SortingCodecReaderEnum::Sorting(reader) => {
                LeafReader::get_sorted_numeric_doc_values(reader, field)
            },
        }
    }

    type SortedSetDocValues = <FilterCodecReader<CR> as LeafReader>::SortedSetDocValues;

    fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>> {
        match self {
            SortingCodecReaderEnum::Filter(reader) => {
                LeafReader::get_sorted_set_doc_values(reader, field)
            },
            SortingCodecReaderEnum::Sorting(reader) => {
                LeafReader::get_sorted_set_doc_values(reader, field)
            },
        }
    }

    type NormNumericDocValues = <FilterCodecReader<CR> as LeafReader>::NormNumericDocValues;

    fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>> {
        match self {
            SortingCodecReaderEnum::Filter(reader) => LeafReader::get_norm_values(reader, field),
            SortingCodecReaderEnum::Sorting(reader) => LeafReader::get_norm_values(reader, field),
        }
    }

    type DocValuesSkipper = <FilterCodecReader<CR> as LeafReader>::DocValuesSkipper;

    fn get_doc_values_skipper(&self, field: &str) -> Result<Option<Self::DocValuesSkipper>> {
        match self {
            SortingCodecReaderEnum::Filter(reader) => {
                LeafReader::get_doc_values_skipper(reader, field)
            },
            SortingCodecReaderEnum::Sorting(reader) => {
                LeafReader::get_doc_values_skipper(reader, field)
            },
        }
    }

    fn get_field_infos(&self) -> Result<Arc<FieldInfos>> {
        match self {
            SortingCodecReaderEnum::Filter(reader) => reader.get_field_infos(),
            SortingCodecReaderEnum::Sorting(reader) => reader.get_field_infos(),
        }
    }

    type Bits = BitsEnum2<CRBits<CR>, SortingBitsImpl<CRBits<CR>, DM>>;

    fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
        match self {
            SortingCodecReaderEnum::Filter(reader) => Ok(reader.get_live_docs()?.map(BitsEnum2::A)),
            SortingCodecReaderEnum::Sorting(reader) => {
                Ok(reader.get_live_docs()?.map(BitsEnum2::B))
            },
        }
    }

    type PointValues = <FilterCodecReader<CR> as LeafReader>::PointValues;

    fn get_point_values(&self, field: &str) -> Result<Option<Self::PointValues>> {
        match self {
            SortingCodecReaderEnum::Filter(reader) => LeafReader::get_point_values(reader, field),
            SortingCodecReaderEnum::Sorting(reader) => LeafReader::get_point_values(reader, field),
        }
    }

    fn get_metadata(&self) -> Result<&LeafMetaData> {
        match self {
            SortingCodecReaderEnum::Filter(reader) => reader.get_metadata(),
            SortingCodecReaderEnum::Sorting(reader) => reader.get_metadata(),
        }
    }
}

impl<CR, DM> IndexReader for SortingCodecReaderEnum<CR, DM>
where
    CR: CodecReader,
    DM: Clone + DocMap,
{
    type TermVectors = <FilterCodecReader<CR> as IndexReader>::TermVectors;

    fn term_vectors(&self) -> Result<Self::TermVectors> {
        match self {
            SortingCodecReaderEnum::Filter(reader) => IndexReader::term_vectors(reader),
            SortingCodecReaderEnum::Sorting(reader) => IndexReader::term_vectors(reader),
        }
    }

    fn max_doc(&self) -> Result<i32> {
        match self {
            SortingCodecReaderEnum::Filter(reader) => reader.max_doc(),
            SortingCodecReaderEnum::Sorting(reader) => reader.max_doc(),
        }
    }

    fn num_docs(&self) -> Result<i32> {
        match self {
            SortingCodecReaderEnum::Filter(reader) => reader.num_docs(),
            SortingCodecReaderEnum::Sorting(reader) => reader.num_docs(),
        }
    }

    type StoredFields = <FilterCodecReader<CR> as IndexReader>::StoredFields;

    fn stored_fields(&self) -> Result<Self::StoredFields> {
        match self {
            SortingCodecReaderEnum::Filter(reader) => IndexReader::stored_fields(reader),
            SortingCodecReaderEnum::Sorting(reader) => IndexReader::stored_fields(reader),
        }
    }

    fn do_close(&self) -> Result<()> {
        match self {
            SortingCodecReaderEnum::Filter(reader) => reader.do_close(),
            SortingCodecReaderEnum::Sorting(reader) => reader.do_close(),
        }
    }

    type ReaderCacheHelper = DummyCacheHelper;

    fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
        match self {
            SortingCodecReaderEnum::Filter(v) => v.get_reader_cache_helper(),
            SortingCodecReaderEnum::Sorting(v) => v.get_reader_cache_helper(),
        }
    }

    fn doc_freq(&self, term: &Term) -> Result<i32> {
        match self {
            SortingCodecReaderEnum::Filter(v) => IndexReader::doc_freq(v, term),
            SortingCodecReaderEnum::Sorting(v) => IndexReader::doc_freq(v, term),
        }
    }

    fn total_term_freq(&self, term: &Term) -> Result<i64> {
        match self {
            SortingCodecReaderEnum::Filter(v) => IndexReader::total_term_freq(v, term),
            SortingCodecReaderEnum::Sorting(v) => IndexReader::total_term_freq(v, term),
        }
    }

    fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
        match self {
            SortingCodecReaderEnum::Filter(v) => IndexReader::get_sum_doc_freq(v, field),
            SortingCodecReaderEnum::Sorting(v) => IndexReader::get_sum_doc_freq(v, field),
        }
    }

    fn get_doc_count(&self, field: &str) -> Result<i32> {
        match self {
            SortingCodecReaderEnum::Filter(v) => IndexReader::get_doc_count(v, field),
            SortingCodecReaderEnum::Sorting(v) => IndexReader::get_doc_count(v, field),
        }
    }

    fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
        match self {
            SortingCodecReaderEnum::Filter(v) => IndexReader::get_sum_total_term_freq(v, field),
            SortingCodecReaderEnum::Sorting(v) => IndexReader::get_sum_total_term_freq(v, field),
        }
    }

    fn index_base(&self) -> &IndexReaderBase {
        match self {
            SortingCodecReaderEnum::Filter(v) => v.index_base(),
            SortingCodecReaderEnum::Sorting(v) => v.index_base(),
        }
    }
}

impl<CR, DM> Display for SortingCodecReaderEnum<CR, DM>
where
    CR: CodecReader,
    DM: Clone + DocMap,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            SortingCodecReaderEnum::Filter(v) => write!(f, "{}", v),
            SortingCodecReaderEnum::Sorting(v) => write!(f, "{}", v),
        }
    }
}

impl<CR, DM> CodecReader for SortingCodecReaderEnum<CR, DM>
where
    CR: CodecReader,
    DM: DocMap + Clone,
{
    type StoredFieldsReader = StoredFieldsReaderEnum2<
        <FilterCodecReader<CR> as CodecReader>::StoredFieldsReader,
        <SortingCodecReader<CR, DM> as CodecReader>::StoredFieldsReader,
    >;
    type TermVectorsReader = TermVectorsReaderEnum2<
        <FilterCodecReader<CR> as CodecReader>::TermVectorsReader,
        <SortingCodecReader<CR, DM> as CodecReader>::TermVectorsReader,
    >;
    type NormsProducer = NormsProducerEnum2<
        <FilterCodecReader<CR> as CodecReader>::NormsProducer,
        <SortingCodecReader<CR, DM> as CodecReader>::NormsProducer,
    >;
    type DocValuesProducer = DocValuesProducerEnum2<
        <FilterCodecReader<CR> as CodecReader>::DocValuesProducer,
        <SortingCodecReader<CR, DM> as CodecReader>::DocValuesProducer,
    >;
    type FieldsProducer = FieldsProducerEnum2<
        <FilterCodecReader<CR> as CodecReader>::FieldsProducer,
        <SortingCodecReader<CR, DM> as CodecReader>::FieldsProducer,
    >;
    type PointsReader = PointsReaderEnum2<
        <FilterCodecReader<CR> as CodecReader>::PointsReader,
        <SortingCodecReader<CR, DM> as CodecReader>::PointsReader,
    >;

    fn get_fields_reader(&self) -> Result<Option<Self::StoredFieldsReader>> {
        Ok(match self {
            SortingCodecReaderEnum::Filter(f) => {
                f.get_fields_reader()?.map(StoredFieldsReaderEnum2::A)
            },
            SortingCodecReaderEnum::Sorting(s) => {
                s.get_fields_reader()?.map(StoredFieldsReaderEnum2::B)
            },
        })
    }

    fn get_term_vectors_reader(&self) -> Result<Option<Self::TermVectorsReader>> {
        Ok(match self {
            SortingCodecReaderEnum::Filter(f) => {
                f.get_term_vectors_reader()?.map(TermVectorsReaderEnum2::A)
            },
            SortingCodecReaderEnum::Sorting(s) => {
                s.get_term_vectors_reader()?.map(TermVectorsReaderEnum2::B)
            },
        })
    }

    fn get_norms_reader(&self) -> Result<Option<Self::NormsProducer>> {
        Ok(match self {
            SortingCodecReaderEnum::Filter(f) => f.get_norms_reader()?.map(NormsProducerEnum2::A),
            SortingCodecReaderEnum::Sorting(s) => s.get_norms_reader()?.map(NormsProducerEnum2::B),
        })
    }

    fn get_doc_values_reader(&self) -> Result<Option<Self::DocValuesProducer>> {
        Ok(match self {
            SortingCodecReaderEnum::Filter(f) => {
                f.get_doc_values_reader()?.map(DocValuesProducerEnum2::A)
            },
            SortingCodecReaderEnum::Sorting(s) => {
                s.get_doc_values_reader()?.map(DocValuesProducerEnum2::B)
            },
        })
    }

    fn get_postings_reader(&self) -> Result<Option<Self::FieldsProducer>> {
        Ok(match self {
            SortingCodecReaderEnum::Filter(f) => {
                f.get_postings_reader()?.map(FieldsProducerEnum2::A)
            },
            SortingCodecReaderEnum::Sorting(s) => {
                s.get_postings_reader()?.map(FieldsProducerEnum2::B)
            },
        })
    }

    fn get_points_reader(&self) -> Result<Option<Self::PointsReader>> {
        Ok(match self {
            SortingCodecReaderEnum::Filter(f) => f.get_points_reader()?.map(PointsReaderEnum2::A),
            SortingCodecReaderEnum::Sorting(s) => s.get_points_reader()?.map(PointsReaderEnum2::B),
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::core::codecs::doc_values_producer::DocValuesProducer;
    use crate::core::document::document::Document;
    use crate::core::document::sorted_set_doc_values_field::SortedSetDocValuesField;
    use crate::core::index::BytesRef;
    use crate::core::index::codec_reader::CodecReader;
    use crate::core::index::composite_reader::get_context;
    use crate::core::index::directory_reader::directory_reader_util;

    use crate::core::index::index_reader_context::IndexReaderContext;
    use crate::core::index::index_writer::IndexWriter;
    use crate::core::index::leaf_reader::LeafReader;
    use crate::core::index::slow_codec_reader_wrapper::SlowCodecReaderWrapper;
    use crate::core::index::sorted_set_doc_values::SortedSetDocValues;
    use crate::core::index::sorting_codec_reader::{SortingCodecReaderEnum, wrap};
    use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
    use crate::core::search::doc_id_set_iterator::disi_const::NO_MORE_DOCS;
    use crate::core::search::sort::Sort;
    use crate::core::search::sorted_set_selector::SortedSetSelectorType::Min;
    use crate::core::search::sorted_set_sort_field::SortedSetSortField;
    use crate::core::util::error::lucene_error::Result;
    use crate::test::util::lucene_test_case::lucene_test_case_util::{
        new_directory_shared, new_index_writer_config, random,
    };

    #[test]
    fn test_sort_on_add_indices_ord() -> Result<()> {
        let mut random = random();
        let tmp_dir = new_directory_shared(&mut random)?;
        // TODO IMPORTANT MockAnalyzer 未实现

        let iwc = new_index_writer_config(&mut random);
        let w = IndexWriter::new(tmp_dir.clone(), iwc)?;

        let mut doc = Document::new();
        doc.add(SortedSetDocValuesField::new(
            "foo",
            BytesRef::from_string("b"),
        ));
        w.add_document(doc.clone())?;

        doc.add(SortedSetDocValuesField::new(
            "foo",
            BytesRef::from_string("a"),
        ));
        doc.add(SortedSetDocValuesField::new(
            "foo",
            BytesRef::from_string("b"),
        ));
        doc.add(SortedSetDocValuesField::new(
            "foo",
            BytesRef::from_string("b"),
        ));
        w.add_document(doc)?;

        w.commit()?;

        let index_sort =
            Sort::with_fields(vec![SortedSetSortField::with_selector("foo", false, Min)?])?;

        let reader = directory_reader_util::open(tmp_dir.clone())?;
        let reader = get_context(reader)?;
        for ctx in reader.leaves()? {
            let leaf_reader = ctx.reader().clone();
            let slow = SlowCodecReaderWrapper::wrap_leaf_reader(leaf_reader);
            let wrap = wrap(slow, index_sort.clone())?;

            let s = wrap.to_string();
            assert!(s.starts_with("SortingCodecReader("), "{}", s);
            match wrap {
                SortingCodecReaderEnum::Sorting(sorting_codec_reader) => {
                    let fi = ctx
                        .reader()
                        .get_field_infos()?
                        .field_info_by_name("foo")
                        .expect("field foo must exist");

                    let mut sorted_set_doc_values = sorting_codec_reader
                        .get_doc_values_reader()?
                        .expect("doc values reader must exist")
                        .get_sorted_set(&fi)?;

                    sorted_set_doc_values.next_doc()?;
                    assert_eq!(sorted_set_doc_values.doc_value_count()?, 2);

                    sorted_set_doc_values.next_doc()?;
                    assert_eq!(sorted_set_doc_values.doc_value_count()?, 1);

                    assert_eq!(sorted_set_doc_values.next_doc()?, NO_MORE_DOCS);
                },
                _ => unreachable!("wrap should be SortingCodecReader"),
            }
        }
        Ok(())
    }

    #[test]
    fn test_sort_on_add_indices_int() -> Result<()> {
        // add_indexes未实现
        Ok(())
    }

    #[test]
    fn test_sort_on_add_indices_random() -> Result<()> {
        // add_indexes未实现
        Ok(())
    }
}
