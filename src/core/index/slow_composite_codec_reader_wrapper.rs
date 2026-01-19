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
use crate::core::codecs::doc_values_producer::DocValuesProducer;
use crate::core::codecs::dummy::dummy_doc_values_skipper::DummyDocValuesSkipper;
use crate::core::codecs::dummy::dummy_mutable_point_tree::DummyMutablePointTree;
use crate::core::codecs::fields_producer::FieldsProducer;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::points_reader::PointsReader;
use crate::core::codecs::stored_fields_reader::{DefaultStoredFieldsReader, StoredFieldsReader};
use crate::core::codecs::stored_fields_writer::StoredFieldsWriter;
use crate::core::codecs::term_vectors_reader::{DefaultTermVectorsReader, TermVectorsReader};
use crate::core::index::codec_reader::{
    CRDocValuesProducer, CRFieldsProducer, CRNormsProducer, CRPointsReader, CRStoredFieldsReader,
    CRTermVectorsReader, CodecReader, CodecReaderEnum2,
};
use crate::core::index::doc_values::{DocValues, EmptySorted};
use crate::core::index::dummy::dummy_cache_helper::DummyCacheHelper;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::{FieldInfos, get_merged_field_infos};
use crate::core::index::fields::Fields;
use crate::core::index::index_reader::{IndexReader, IndexReaderBase};
use crate::core::index::leaf_metadata::LeafMetaData;
use crate::core::index::leaf_reader::{LRSortedDocValuesEmpty, LRSortedSetDocValues, LeafReader};
use crate::core::index::multi_bits::{BitsType, get_live_docs};
use crate::core::index::multi_doc_values::{
    MultiBinaryDocValues, MultiDocValues, MultiNormNumericDocValues, MultiNumericDocValues,
    MultiSortedDocValues, MultiSortedDocValuesType, MultiSortedNumericDocValues,
    MultiSortedSetDocValues, MultiSortedSetDocValuesType,
};
use crate::core::index::multi_fields::{MultiFields, MultiFieldsTerms};
use crate::core::index::multi_reader::{MultiLeafReader, MultiReader};
use crate::core::index::ordinal_map::OrdinalMap;
use crate::core::index::point_values::{
    IntersectVisitor, PointTree, PointTreeEnum, PointValues, Relation,
};
use crate::core::index::reader_slice::ReaderSlice;
use crate::core::index::singleton_sorted_set_doc_values::SingletonSortedSetDocValues;
use crate::core::index::sorted_doc_values::SortedDocValuesEnum2;
use crate::core::index::sorted_set_doc_values_writer::SortedSetDocValuesEnum2;
use crate::core::index::stored_field_visitor::{Status, StoredFieldVisitor};
use crate::core::index::stored_fields::{RawStoredFieldsReader, StoredFields};
use crate::core::index::term::Term;
use crate::core::index::term_vectors::{
    EmptyTermVectors, RawTermVectors, TermVectors, TermVectorsEnum2,
};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::util::array_util::{ArrayUtil, ByteArrayComparator};
use crate::core::util::clone::TryClone;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::merged_iterator::MergedIterator;
use crate::core::util::{CoreHelper, SliceCopyOps};
use parking_lot::Mutex;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::rc::Rc;
use std::sync::Arc;

pub(crate) fn wrap<CR>(
    mut readers: Vec<CR>,
) -> Result<CodecReaderEnum2<CR, SlowCompositeCodecReaderWrapper<CR>>>
where
    CR: CodecReader + Clone,
{
    match readers.len() {
        0 => Err(LuceneError::illegal_argument(
            "Must take at least one reader, got 0",
        )),
        1 => Ok(CodecReaderEnum2::A(readers.pop().unwrap())),
        _ => Ok(CodecReaderEnum2::B(SlowCompositeCodecReaderWrapper::new(
            readers,
        )?)),
    }
}

/// A merged [`CodecReader`] view of multiple [`CodecReader`]s.
///
/// This view is primarily targeted at merging, not searching.
pub(crate) struct SlowCompositeCodecReaderWrapper<CR>
where
    CR: CodecReader + Clone,
{
    meta: LeafMetaData,
    codec_readers: Vec<CR>,
    doc_stats: Arc<Vec<usize>>,
    field_infos: Arc<FieldInfos>,
    live_docs: Option<Arc<BitsType<MultiLeafReader<CR>>>>,
    inner: Mutex<Inner>,
    index_base: IndexReaderBase,
}
struct Inner {
    num_docs: i32,
}
impl<CR> SlowCompositeCodecReaderWrapper<CR>
where
    CR: CodecReader + Clone,
{
    pub(crate) fn new(codec_readers: Vec<CR>) -> Result<Self> {
        let mut doc_stats = Vec::with_capacity(codec_readers.len() + 1);

        doc_stats.push(0);
        let mut doc_start = 0;
        for reader in &codec_readers {
            doc_start += reader.max_doc()?;
            doc_stats.push(doc_start as usize);
        }

        let mut major_version = -1;
        let mut min_version = None;
        let mut has_blocks = false;

        for reader in &codec_readers {
            let reader_meta = reader.get_metadata()?;
            if major_version == -1 {
                major_version = reader_meta.get_created_version_major();
            } else if major_version != reader_meta.get_created_version_major() {
                return Err(LuceneError::illegal_argument(
                    "Cannot combine leaf readers created with different major versions",
                ));
            } else {
                if reader_meta.get_min_version().is_none() {
                    return Err(LuceneError::illegal_state("min_version must be set"));
                }
                match &min_version {
                    None => min_version = reader_meta.get_min_version().clone(),
                    Some(v) if v.on_or_after(reader_meta.get_min_version().as_ref().unwrap()) => {
                        min_version = reader_meta.get_min_version().clone();
                    },
                    _ => {},
                }

                has_blocks |= reader_meta.get_has_blocks();
            }
        }

        let meta = LeafMetaData::new(major_version, min_version, None, has_blocks)?;

        let multi_reader: MultiLeafReader<CR> =
            MultiReader::with_leaf_reader(codec_readers.clone())?;
        let field_infos = get_merged_field_infos(&multi_reader)?;
        let live_docs = get_live_docs(multi_reader)?.map(Arc::new);
        let inner = Mutex::new(Inner { num_docs: -1 });
        Ok(Self {
            meta,
            codec_readers,
            doc_stats: Arc::new(doc_stats),
            field_infos,
            live_docs,
            inner,
            index_base: IndexReaderBase::new(),
        })
    }
}

impl<CR> LeafReader for SlowCompositeCodecReaderWrapper<CR>
where
    CR: Clone + CodecReader,
{
    type CacheHelper = DummyCacheHelper;

    fn get_core_cache_helper_ref(&self) -> Result<Option<&Self::CacheHelper>> {
        Ok(None)
    }

    fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
        Ok(None)
    }

    type Terms = <<Self as CodecReader>::FieldsProducer as Fields>::Terms;

    fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
        CodecReader::terms(self, field)
    }

    type NumericDocValues =
        <<Self as CodecReader>::DocValuesProducer as DocValuesProducer>::NumericDocValues;

    fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>> {
        CodecReader::get_numeric_doc_values(self, field)
    }

    type BinaryDocValues =
        <<Self as CodecReader>::DocValuesProducer as DocValuesProducer>::BinaryDocValues;

    fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
        CodecReader::get_binary_doc_values(self, field)
    }

    type SortedDocValues =
        <<Self as CodecReader>::DocValuesProducer as DocValuesProducer>::SortedDocValues;

    fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
        CodecReader::get_sorted_doc_values(self, field)
    }

    type SortedNumericDocValues =
        <<Self as CodecReader>::DocValuesProducer as DocValuesProducer>::SortedNumericDocValues;

    fn get_sorted_numeric_doc_values(
        &self,
        field: &str,
    ) -> Result<Option<Self::SortedNumericDocValues>> {
        CodecReader::get_sorted_numeric_doc_values(self, field)
    }

    type SortedSetDocValues =
        <<Self as CodecReader>::DocValuesProducer as DocValuesProducer>::SortedSetDocValues;

    fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>> {
        CodecReader::get_sorted_set_doc_values(self, field)
    }

    type NormNumericDocValues =
        <<Self as CodecReader>::NormsProducer as NormsProducer>::NumericDocValues;

    fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>> {
        CodecReader::get_norm_values(self, field)
    }

    type DocValuesSkipper =
        <<Self as CodecReader>::DocValuesProducer as DocValuesProducer>::DocValuesSkipper;

    fn get_doc_values_skipper(&self, field: &str) -> Result<Option<Self::DocValuesSkipper>> {
        CodecReader::get_doc_values_skipper(self, field)
    }

    fn get_field_infos(&self) -> Result<Arc<FieldInfos>> {
        Ok(self.field_infos.clone())
    }

    type Bits = Arc<BitsType<MultiLeafReader<CR>>>;

    fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
        Ok(self.live_docs.clone())
    }

    type PointValues = <<Self as CodecReader>::PointsReader as PointsReader>::PointValuesType;

    fn get_point_values(&self, field: &str) -> Result<Option<Self::PointValues>> {
        CodecReader::get_point_values(self, field)
    }

    fn get_metadata(&self) -> Result<&LeafMetaData> {
        Ok(&self.meta)
    }
}

impl<CR> IndexReader for SlowCompositeCodecReaderWrapper<CR>
where
    CR: Clone + CodecReader,
{
    type TermVectors = TermVectorsEnum2<
        <Self as CodecReader>::TermVectorsReader,
        EmptyTermVectors<<<Self as CodecReader>::TermVectorsReader as RawTermVectors>::IndexInput>,
    >;

    fn term_vectors(&self) -> Result<Self::TermVectors> {
        match self.get_term_vectors_reader()? {
            Some(tvr) => Ok(TermVectorsEnum2::A(tvr)),
            None => Ok(TermVectorsEnum2::B(EmptyTermVectors::default())),
        }
    }

    fn max_doc(&self) -> Result<i32> {
        Ok(self.doc_stats[self.doc_stats.len() - 1] as i32)
    }

    fn num_docs(&self) -> Result<i32> {
        // Compute the number of docs lazily, in case some leaves need to recompute it the first time it
        // is called, see BaseCompositeReader#numDocs.
        let mut inner = self.inner.lock();
        if inner.num_docs == -1 {
            let mut total = 0;
            for reader in self.codec_readers.iter() {
                total += reader.num_docs()?;
            }
            inner.num_docs = total;
        }

        Ok(inner.num_docs)
    }

    type StoredFields = StoredFieldsImpl<<Self as CodecReader>::StoredFieldsReader>;

    fn stored_fields(&self) -> Result<Self::StoredFields> {
        let reader = self
            .get_fields_reader()?
            .ok_or_else(|| LuceneError::illegal_state("fields reader is None"))?;
        Ok(StoredFieldsImpl::new(reader, self.max_doc()?))
    }

    type ReaderCacheHelper = DummyCacheHelper;

    fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
        Ok(None)
    }

    fn doc_freq(&self, term: &Term) -> Result<i32> {
        LeafReader::doc_freq(self, term)
    }

    fn total_term_freq(&self, term: &Term) -> Result<i64> {
        LeafReader::get_total_term_freq(self, term)
    }

    fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
        LeafReader::get_sum_doc_freq(self, field)
    }

    fn get_doc_count(&self, field: &str) -> Result<i32> {
        LeafReader::get_doc_count(self, field)
    }

    fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
        LeafReader::get_sum_total_term_freq(self, field)
    }

    fn index_base(&self) -> &IndexReaderBase {
        &self.index_base
    }
}

impl<CR> Display for SlowCompositeCodecReaderWrapper<CR>
where
    CR: Clone + CodecReader,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SlowCompositeCodecReaderWrapper({} readers)",
            self.codec_readers.len()
        )
    }
}

impl<CR> CodecReader for SlowCompositeCodecReaderWrapper<CR>
where
    CR: CodecReader + Clone,
{
    type StoredFieldsReader = SlowCompositeStoredFieldsReaderWrapper<CRStoredFieldsReader<CR>>;
    type TermVectorsReader = SlowCompositeTermVectorsReaderWrapper<CRTermVectorsReader<CR>>;
    type NormsProducer = SlowCompositeNormsProducer<CR>;
    type DocValuesProducer = SlowCompositeDocValuesProducerWrapper<CR>;
    type FieldsProducer = SlowCompositeFieldsProducerWrapper<CRFieldsProducer<CR>>;
    type PointsReader = SlowCompositePointsReaderWrapper<CR>;

    fn get_fields_reader(&self) -> Result<Option<Self::StoredFieldsReader>> {
        let mut readers = Vec::with_capacity(self.codec_readers.len());
        for cr in self.codec_readers.iter() {
            readers.push(cr.get_fields_reader()?);
        }
        Ok(Some(SlowCompositeStoredFieldsReaderWrapper::new(
            self.doc_stats.clone(),
            readers,
            self.field_infos.clone(),
        )))
    }

    fn get_term_vectors_reader(&self) -> Result<Option<Self::TermVectorsReader>> {
        let mut readers = Vec::with_capacity(self.codec_readers.len());
        for cr in self.codec_readers.iter() {
            readers.push(cr.get_term_vectors_reader()?);
        }
        Ok(Some(SlowCompositeTermVectorsReaderWrapper::new(
            self.doc_stats.clone(),
            readers,
        )))
    }

    fn get_norms_reader(&self) -> Result<Option<Self::NormsProducer>> {
        Ok(Some(SlowCompositeNormsProducer::new(
            self.codec_readers.clone(),
        )?))
    }

    fn get_doc_values_reader(&self) -> Result<Option<Self::DocValuesProducer>> {
        Ok(Some(SlowCompositeDocValuesProducerWrapper::new(
            self.codec_readers.clone(),
            self.doc_stats.clone(),
        )?))
    }

    fn get_postings_reader(&self) -> Result<Option<Self::FieldsProducer>> {
        let mut readers = Vec::with_capacity(self.codec_readers.len());
        for cr in self.codec_readers.iter() {
            readers.push(cr.get_postings_reader()?);
        }
        Ok(Some(SlowCompositeFieldsProducerWrapper::new(
            readers,
            self.doc_stats.as_slice(),
        )?))
    }

    fn get_points_reader(&self) -> Result<Option<Self::PointsReader>> {
        Ok(Some(SlowCompositePointsReaderWrapper::new(
            self.codec_readers.clone(),
            self.doc_stats.clone(),
        )?))
    }
}

pub struct StoredFieldsImpl<SF>
where
    SF: StoredFields,
{
    reader: SF,
    max_doc: i32,
}
impl<SF> StoredFieldsImpl<SF>
where
    SF: StoredFields,
{
    pub fn new(reader: SF, max_doc: i32) -> Self {
        Self { reader, max_doc }
    }
}
impl<SF> StoredFields for StoredFieldsImpl<SF>
where
    SF: StoredFields,
{
    fn prefetch(&mut self, doc_id: i32) -> Result<()> {
        CoreHelper::check_index(doc_id as usize, self.max_doc as usize)?;
        self.reader.prefetch(doc_id)
    }

    fn document_with_visitor<S: StoredFieldsWriter>(
        &mut self,
        doc_id: i32,
        visitor: &mut impl StoredFieldVisitor,
        writer: Option<&mut S>,
    ) -> Result<()> {
        CoreHelper::check_index(doc_id as usize, self.max_doc as usize)?;
        self.reader.document_with_visitor(doc_id, visitor, writer)
    }
}

impl<SF> RawStoredFieldsReader for StoredFieldsImpl<SF>
where
    SF: RawStoredFieldsReader + StoredFields,
{
    type IndexInput = SF::IndexInput;

    fn raw_stored_fields(&mut self) -> Result<&mut DefaultStoredFieldsReader<Self::IndexInput>> {
        self.reader.raw_stored_fields()
    }
}

fn doc_id_to_reader_id(doc: i32, doc_starts: &[usize]) -> Result<usize> {
    CoreHelper::check_index(doc as usize, doc_starts[doc_starts.len() - 1])?;
    match doc_starts.binary_search(&(doc as usize)) {
        Ok(reader_id) => Ok(reader_id),
        Err(insert_pos) => Ok(insert_pos - 1),
    }
}

pub struct SlowCompositeStoredFieldsReaderWrapper<SFR>
where
    SFR: StoredFieldsReader,
{
    doc_starts: Arc<Vec<usize>>,
    readers: Vec<Option<SFR>>,
    field_infos: Arc<FieldInfos>,
}
impl<SFR> SlowCompositeStoredFieldsReaderWrapper<SFR>
where
    SFR: StoredFieldsReader,
{
    pub fn new(
        doc_starts: Arc<Vec<usize>>,
        readers: Vec<Option<SFR>>,
        field_infos: Arc<FieldInfos>,
    ) -> Self {
        Self {
            doc_starts,
            readers,
            field_infos,
        }
    }
}

impl<SFR> StoredFields for SlowCompositeStoredFieldsReaderWrapper<SFR>
where
    SFR: StoredFieldsReader,
{
    fn prefetch(&mut self, doc_id: i32) -> Result<()> {
        let reader_id = doc_id_to_reader_id(doc_id, self.doc_starts.as_slice())?;
        let reader = self.readers[reader_id]
            .as_mut()
            .ok_or_else(|| LuceneError::illegal_state("StoredFieldsReader is None"))?;
        reader.prefetch(doc_id - self.doc_starts[reader_id] as i32)
    }

    fn document_with_visitor<S: StoredFieldsWriter>(
        &mut self,
        doc_id: i32,
        visitor: &mut impl StoredFieldVisitor,
        writer: Option<&mut S>,
    ) -> Result<()> {
        let reader_id = doc_id_to_reader_id(doc_id, self.doc_starts.as_slice())?;
        let mut sf_visitor = StoredFieldVisitorImpl::new(visitor, self.field_infos.clone());
        let reader = &mut self.readers[reader_id]
            .as_mut()
            .ok_or_else(|| LuceneError::illegal_state("StoredFieldsReader is None"))?;
        reader.document_with_visitor(doc_id, &mut sf_visitor, writer)
    }
}

impl<SFR> Clone for SlowCompositeStoredFieldsReaderWrapper<SFR>
where
    SFR: StoredFieldsReader,
{
    fn clone(&self) -> Self {
        SlowCompositeStoredFieldsReaderWrapper::new(
            self.doc_starts.clone(),
            self.readers.clone(),
            self.field_infos.clone(),
        )
    }
}

impl<SFR> StoredFieldsReader for SlowCompositeStoredFieldsReaderWrapper<SFR>
where
    SFR: StoredFieldsReader,
{
    fn check_integrity(&self) -> Result<()> {
        for r in self.readers.iter().flatten() {
            r.check_integrity()?;
        }
        Ok(())
    }
}

impl<SFR> RawStoredFieldsReader for SlowCompositeStoredFieldsReaderWrapper<SFR>
where
    SFR: RawStoredFieldsReader + StoredFieldsReader,
{
    type IndexInput = SFR::IndexInput;

    fn raw_stored_fields(&mut self) -> Result<&mut DefaultStoredFieldsReader<Self::IndexInput>> {
        Err(LuceneError::illegal_state(
            "Raw stored fields are not available for composite readers",
        ))
    }
}

pub struct StoredFieldVisitorImpl<'a, SFV>
where
    SFV: StoredFieldVisitor,
{
    visitor: &'a mut SFV,
    field_infos: Arc<FieldInfos>,
}
impl<'a, SFV> StoredFieldVisitorImpl<'a, SFV>
where
    SFV: StoredFieldVisitor,
{
    fn new(visitor: &'a mut SFV, field_infos: Arc<FieldInfos>) -> Self {
        Self {
            visitor,
            field_infos,
        }
    }
    fn remap(&self, field_info: &FieldInfo) -> Result<Arc<FieldInfo>> {
        let fi = self.field_infos.field_info_by_name(&field_info.name);
        match fi {
            Some(fi) => Ok(fi.clone()),
            None => Err(LuceneError::illegal_state(format!(
                "FieldInfo not found by {}",
                field_info.name
            ))),
        }
    }
}

impl<'a, SFV> StoredFieldVisitor for StoredFieldVisitorImpl<'a, SFV>
where
    SFV: StoredFieldVisitor,
{
    fn binary_field<S: StoredFieldsWriter>(
        &mut self,
        field_info: Arc<FieldInfo>,
        value: Vec<u8>,
        writer: Option<&mut S>,
    ) -> Result<()> {
        self.visitor
            .binary_field(self.remap(&field_info)?, value, writer)
    }

    fn string_field<S: StoredFieldsWriter>(
        &mut self,
        field_info: Arc<FieldInfo>,
        value: String,
        writer: Option<&mut S>,
    ) -> Result<()> {
        self.visitor
            .string_field(self.remap(&field_info)?, value, writer)
    }

    fn int_field<S: StoredFieldsWriter>(
        &mut self,
        field_info: Arc<FieldInfo>,
        value: i32,
        writer: Option<&mut S>,
    ) -> Result<()> {
        self.visitor
            .int_field(self.remap(&field_info)?, value, writer)
    }

    fn long_field<S: StoredFieldsWriter>(
        &mut self,
        field_info: Arc<FieldInfo>,
        value: i64,
        writer: Option<&mut S>,
    ) -> Result<()> {
        self.visitor
            .long_field(self.remap(&field_info)?, value, writer)
    }

    fn float_field<S: StoredFieldsWriter>(
        &mut self,
        field_info: Arc<FieldInfo>,
        value: f32,
        writer: Option<&mut S>,
    ) -> Result<()> {
        self.visitor
            .float_field(self.remap(&field_info)?, value, writer)
    }

    fn double_field<S: StoredFieldsWriter>(
        &mut self,
        field_info: Arc<FieldInfo>,
        value: f64,
        writer: Option<&mut S>,
    ) -> Result<()> {
        self.visitor
            .double_field(self.remap(&field_info)?, value, writer)
    }

    fn needs_field<S: StoredFieldsWriter>(
        &mut self,
        field_info: Arc<FieldInfo>,
        writer: Option<&mut S>,
    ) -> Result<Status> {
        self.visitor.needs_field(self.remap(&field_info)?, writer)
    }
}

pub struct SlowCompositeTermVectorsReaderWrapper<TVR>
where
    TVR: TermVectorsReader,
{
    doc_starts: Arc<Vec<usize>>,
    readers: Vec<Option<TVR>>,
}

impl<TVR> SlowCompositeTermVectorsReaderWrapper<TVR>
where
    TVR: TermVectorsReader,
{
    pub fn new(doc_starts: Arc<Vec<usize>>, readers: Vec<Option<TVR>>) -> Self {
        Self {
            doc_starts,
            readers,
        }
    }
}

impl<TVR> Clone for SlowCompositeTermVectorsReaderWrapper<TVR>
where
    TVR: TermVectorsReader,
{
    fn clone(&self) -> Self {
        Self {
            doc_starts: self.doc_starts.clone(),
            readers: self.readers.to_vec(),
        }
    }
}

impl<TVR> TermVectors for SlowCompositeTermVectorsReaderWrapper<TVR>
where
    TVR: TermVectorsReader,
{
    fn prefetch(&mut self, doc: i32) -> Result<()> {
        let reader_id = doc_id_to_reader_id(doc, self.doc_starts.as_slice())?;
        if let Some(reader) = &mut self.readers[reader_id] {
            let local_doc = doc - self.doc_starts[reader_id] as i32;
            reader.prefetch(local_doc)?;
        }
        Ok(())
    }

    type Fields = <TVR as TermVectors>::Fields;

    fn get(&mut self, doc: i32) -> Result<Option<Self::Fields>> {
        let reader_id = doc_id_to_reader_id(doc, self.doc_starts.as_slice())?;
        match &mut self.readers[reader_id] {
            None => Ok(None),
            Some(reader) => {
                let local_doc = doc - self.doc_starts[reader_id] as i32;
                reader.get(local_doc)
            },
        }
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

impl<TVR> TermVectorsReader for SlowCompositeTermVectorsReaderWrapper<TVR>
where
    TVR: TermVectorsReader,
{
    fn check_integrity(&self) -> Result<()> {
        for r in self.readers.iter().flatten() {
            r.check_integrity()?;
        }
        Ok(())
    }
}

impl<TVR> RawTermVectors for SlowCompositeTermVectorsReaderWrapper<TVR>
where
    TVR: TermVectorsReader,
{
    type IndexInput = <TVR as RawTermVectors>::IndexInput;

    fn raw_term_vectors_mut(&mut self) -> Result<&mut DefaultTermVectorsReader<Self::IndexInput>> {
        Err(LuceneError::illegal_state(
            "raw term vectors reader is not available".to_string(),
        ))
    }

    fn raw_term_vectors(&self) -> Result<&DefaultTermVectorsReader<Self::IndexInput>> {
        Err(LuceneError::illegal_state(
            "raw term vectors reader is not available".to_string(),
        ))
    }
}

pub struct SlowCompositeNormsProducer<CR>
where
    CR: CodecReader + Clone,
{
    codec_readers: Vec<CR>,
    producers: Vec<Option<CRNormsProducer<CR>>>,
}

impl<CR> SlowCompositeNormsProducer<CR>
where
    CR: CodecReader + Clone,
{
    pub fn new(codec_readers: Vec<CR>) -> Result<Self> {
        let mut producers = Vec::with_capacity(codec_readers.len());
        for reader in &codec_readers {
            producers.push(reader.get_norms_reader()?);
        }
        Ok(Self {
            codec_readers,
            producers,
        })
    }
}

impl<CR> NormsProducer for SlowCompositeNormsProducer<CR>
where
    CR: CodecReader + Clone,
{
    type NumericDocValues = MultiNormNumericDocValues<MultiLeafReader<CR>>;

    fn get_norms(&self, field: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
        let multi_reader = MultiReader::with_leaf_reader(self.codec_readers.clone())?;
        match MultiDocValues::get_norm_values(multi_reader, &field.name)? {
            Some(norms) => Ok(norms),
            None => Err(LuceneError::illegal_state(format!(
                "No norms found for field {}",
                field.name
            ))),
        }
    }

    fn check_integrity(&self) -> Result<()> {
        for p in self.producers.iter().flatten() {
            p.check_integrity()?;
        }
        Ok(())
    }
}

pub struct SlowCompositeDocValuesProducerWrapper<CR>
where
    CR: CodecReader + Clone,
{
    codec_readers: Vec<CR>,
    producers: Vec<Option<CRDocValuesProducer<CR>>>,
    doc_starts: Arc<Vec<usize>>,
    cached_ord_maps: Mutex<HashMap<String, Arc<OrdinalMap>>>,
}

impl<CR> SlowCompositeDocValuesProducerWrapper<CR>
where
    CR: CodecReader + Clone,
{
    pub fn new(codec_readers: Vec<CR>, doc_starts: Arc<Vec<usize>>) -> Result<Self> {
        let mut producers = Vec::with_capacity(codec_readers.len());
        for reader in &codec_readers {
            producers.push(reader.get_doc_values_reader()?);
        }
        Ok(Self {
            codec_readers,
            producers,
            doc_starts,
            cached_ord_maps: Mutex::new(HashMap::new()),
        })
    }
}
impl<CR> DocValuesProducer for SlowCompositeDocValuesProducerWrapper<CR>
where
    CR: CodecReader + Clone,
{
    type NumericDocValues = MultiNumericDocValues<MultiLeafReader<CR>>;

    fn get_numeric(&self, field: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
        let mr = MultiReader::with_leaf_reader(self.codec_readers.clone())?;
        match MultiDocValues::get_numeric_values(mr, &field.name)? {
            Some(numeric) => Ok(numeric),
            None => Err(LuceneError::illegal_state(format!(
                "No numeric doc values found for field {}",
                field.name
            ))),
        }
    }

    type BinaryDocValues = MultiBinaryDocValues<MultiLeafReader<CR>>;

    fn get_binary(&self, field: &Arc<FieldInfo>) -> Result<Self::BinaryDocValues> {
        let mr = MultiReader::with_leaf_reader(self.codec_readers.clone())?;
        match MultiDocValues::get_binary_values(mr, &field.name)? {
            Some(binary) => Ok(binary),
            None => Err(LuceneError::illegal_state(format!(
                "No binary doc values found for field {}",
                field.name
            ))),
        }
    }

    type SortedDocValues = SortedDocValuesEnum2<
        MultiSortedDocValuesType<MultiLeafReader<CR>>,
        MultiSortedDocValues<LRSortedDocValuesEmpty<CR>>,
    >;

    fn get_sorted(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedDocValues> {
        if let Some(map) = self.cached_ord_maps.lock().get(&field.name).cloned() {
            let mut values = Vec::with_capacity(self.codec_readers.len());
            let mut total_cost = 0;

            for reader in &self.codec_readers {
                match LeafReader::get_sorted_doc_values(reader, &field.name)? {
                    Some(v) => {
                        total_cost += v.cost()?;
                        values.push(SortedDocValuesEnum2::A(v));
                    },
                    None => {
                        let v = DocValues::empty_sorted();
                        total_cost += v.cost()?;
                        values.push(SortedDocValuesEnum2::B(v));
                    },
                }
            }

            return Ok(SortedDocValuesEnum2::B(MultiSortedDocValues::new(
                self.doc_starts.clone(),
                values,
                map,
                total_cost,
            )));
        }

        let mr: MultiLeafReader<CR> = MultiReader::with_leaf_reader(self.codec_readers.clone())?;

        let dv = MultiDocValues::get_sorted_values(mr, &field.name)?.ok_or_else(|| {
            LuceneError::illegal_state(format!(
                "No sorted doc values found for field {}",
                field.name
            ))
        })?;

        if let SortedDocValuesEnum2::B(ref multi) = dv {
            self.cached_ord_maps
                .lock()
                .insert(field.name.clone(), multi.mapping.clone());
        }

        Ok(SortedDocValuesEnum2::A(dv))
    }

    type SortedNumericDocValues = MultiSortedNumericDocValues<MultiLeafReader<CR>>;

    fn get_sorted_numeric(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedNumericDocValues> {
        let mr = MultiReader::with_leaf_reader(self.codec_readers.clone())?;
        match MultiDocValues::get_sorted_numeric_values(mr, &field.name)? {
            Some(sorted_numeric) => Ok(sorted_numeric),
            None => Err(LuceneError::illegal_state(format!(
                "No sorted numeric doc values found for field {}",
                field.name
            ))),
        }
    }

    type SortedSetDocValues = SortedSetDocValuesEnum2<
        MultiSortedSetDocValuesType<MultiLeafReader<CR>>,
        MultiSortedSetDocValues<
            SortedSetDocValuesEnum2<
                LRSortedSetDocValues<CR>,
                SingletonSortedSetDocValues<EmptySorted>,
            >,
        >,
    >;

    fn get_sorted_set(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedSetDocValues> {
        if let Some(map) = self.cached_ord_maps.lock().get(&field.name).cloned() {
            let mut values = Vec::with_capacity(self.codec_readers.len());
            let mut total_cost = 0;

            for reader in &self.codec_readers {
                match LeafReader::get_sorted_set_doc_values(reader, &field.name)? {
                    Some(v) => {
                        total_cost += v.cost()?;
                        values.push(SortedSetDocValuesEnum2::A(v));
                    },
                    None => {
                        let v = DocValues::empty_sorted_set()?;
                        total_cost += v.cost()?;
                        values.push(SortedSetDocValuesEnum2::B(v));
                    },
                }
            }

            return Ok(SortedSetDocValuesEnum2::B(MultiSortedSetDocValues::new(
                values,
                self.doc_starts.clone(),
                map,
                total_cost,
            )));
        }

        let mr: MultiLeafReader<CR> = MultiReader::with_leaf_reader(self.codec_readers.clone())?;

        let dv = MultiDocValues::get_sorted_set_values(mr, &field.name)?.ok_or_else(|| {
            LuceneError::illegal_state(format!(
                "No sorted-set doc values found for field {}",
                field.name
            ))
        })?;

        if let SortedSetDocValuesEnum2::B(ref multi) = dv {
            self.cached_ord_maps
                .lock()
                .insert(field.name.clone(), multi.mapping.clone());
        }

        Ok(SortedSetDocValuesEnum2::A(dv))
    }

    type DocValuesSkipper = DummyDocValuesSkipper;

    fn get_skipper(&self, _field: &Arc<FieldInfo>) -> Result<Option<Self::DocValuesSkipper>> {
        Err(LuceneError::unsupported_operation(
            "This method is for searching not for merging",
        ))
    }

    fn check_integrity(&self) -> Result<()> {
        for p in self.producers.iter().flatten() {
            p.check_integrity()?;
        }
        Ok(())
    }
}

pub struct SlowCompositeFieldsProducerWrapper<FP>
where
    FP: FieldsProducer,
{
    fields: MultiFields<FP>,
}

impl<FP> SlowCompositeFieldsProducerWrapper<FP>
where
    FP: FieldsProducer,
{
    pub fn new(producers: Vec<Option<FP>>, doc_starts: &[usize]) -> Result<Self> {
        let mut subs = Vec::new();
        let mut slices = Vec::new();

        for (i, producer) in producers.into_iter().enumerate() {
            if let Some(p) = producer {
                subs.push(p);
                slices.push(Rc::new(ReaderSlice::new(
                    doc_starts[i],
                    i as i32,
                    doc_starts[i + 1] as i32,
                )));
            }
        }

        let fields = MultiFields::new(subs, slices);

        Ok(Self { fields })
    }
}

impl<FP> Fields for SlowCompositeFieldsProducerWrapper<FP>
where
    FP: FieldsProducer,
{
    type FieldIter<'a>
        = MergedIterator<<FP as Fields>::FieldIter<'a>>
    where
        Self: 'a;

    fn iterator(&self) -> Result<Self::FieldIter<'_>> {
        self.fields.iterator()
    }

    type Terms = MultiFieldsTerms<<FP as Fields>::Terms>;

    fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
        self.fields.terms(field)
    }

    fn size(&self) -> Result<i32> {
        self.fields.size()
    }
}

impl<FP> FieldsProducer for SlowCompositeFieldsProducerWrapper<FP>
where
    FP: FieldsProducer,
{
    fn check_integrity(&self) -> Result<()> {
        for producer in self.fields.subs.iter() {
            producer.check_integrity()?;
        }
        Ok(())
    }
}

pub struct PointValuesSub<PV>
where
    PV: PointValues,
{
    pub sub: PV,
    pub doc_base: i32,
}

impl<PV> PointValuesSub<PV>
where
    PV: PointValues,
{
    pub fn new(sub: PV, doc_base: i32) -> Self {
        Self { sub, doc_base }
    }
}
pub struct SlowCompositePointsReaderWrapper<CR>
where
    CR: CodecReader + Clone,
{
    codec_readers: Vec<CR>,
    readers: Vec<Option<CRPointsReader<CR>>>,
    doc_starts: Arc<Vec<usize>>,
}

impl<CR> SlowCompositePointsReaderWrapper<CR>
where
    CR: CodecReader + Clone,
{
    pub fn new(codec_readers: Vec<CR>, doc_starts: Arc<Vec<usize>>) -> Result<Self> {
        let mut readers = Vec::with_capacity(codec_readers.len());
        for reader in &codec_readers {
            readers.push(reader.get_points_reader()?);
        }

        Ok(Self {
            codec_readers,
            readers,
            doc_starts,
        })
    }
}
impl<CR> PointsReader for SlowCompositePointsReaderWrapper<CR>
where
    CR: CodecReader + Clone,
{
    fn check_integrity(&self) -> Result<()> {
        for r in self.readers.iter().flatten() {
            r.check_integrity()?;
        }
        Ok(())
    }

    type PointValuesType = PointValuesImpl<<CRPointsReader<CR> as PointsReader>::PointValuesType>;

    fn get_values(&self, field: &str) -> Result<Option<Self::PointValuesType>> {
        let mut values = Vec::new();

        for i in 0..self.readers.len() {
            if let Some(fi) = self.codec_readers[i]
                .get_field_infos()?
                .field_info_by_name(field)
                && fi.get_point_dimension_count() > 0
                && let Some(reader) = &self.readers[i]
                && let Some(v) = reader.get_values(field)?
            {
                // Apparently FieldInfo can claim a field has points, yet the returned
                // PointValues is null
                values.push(PointValuesSub::new(v, self.doc_starts[i] as i32));
            }
        }
        if values.is_empty() {
            return Ok(None);
        }
        Ok(Some(PointValuesImpl::new(values)))
    }
}

pub struct PointValuesImpl<PV>
where
    PV: PointValues,
{
    values: Rc<Vec<PointValuesSub<PV>>>,
}
impl<PV> PointValuesImpl<PV>
where
    PV: PointValues,
{
    pub fn new<T>(values: T) -> Self
    where
        T: Into<Rc<Vec<PointValuesSub<PV>>>>,
    {
        let values = values.into();
        Self { values }
    }
}

impl<PV> Clone for PointValuesImpl<PV>
where
    PV: PointValues,
{
    fn clone(&self) -> Self {
        todo!()
    }
}

impl<PV> PointValues for PointValuesImpl<PV>
where
    PV: PointValues,
{
    fn get_min_packed_value(&self) -> Result<Option<Cow<'_, [u8]>>> {
        let pt = self.get_point_tree()?;
        let v = pt.get_min_packed_value()?;
        debug_assert!(matches!(v, Cow::Owned(_)));

        Ok(Some(Cow::Owned(v.into_owned())))
    }

    fn get_max_packed_value(&self) -> Result<Option<Cow<'_, [u8]>>> {
        let pt = self.get_point_tree()?;
        let v = pt.get_max_packed_value()?;
        debug_assert!(matches!(v, Cow::Owned(_)));
        Ok(Some(Cow::Owned(v.into_owned())))
    }

    fn get_num_dimensions(&self) -> Result<usize> {
        self.values[0].sub.get_num_dimensions()
    }

    fn get_num_index_dimensions(&self) -> Result<usize> {
        self.values[0].sub.get_num_index_dimensions()
    }

    fn get_bytes_per_dimension(&self) -> Result<usize> {
        self.values[0].sub.get_bytes_per_dimension()
    }

    fn size(&self) -> Result<usize> {
        self.get_point_tree()?.size()
    }

    fn get_doc_count(&self) -> Result<i32> {
        let mut count = 0;
        for sub in self.values.iter() {
            count += sub.sub.get_doc_count()?;
        }
        Ok(count)
    }

    type PointTree = PointTreeImpl<PV>;
    type MutablePointTree = DummyMutablePointTree;

    fn get_point_tree(&self) -> Result<PointTreeEnum<Self::MutablePointTree, Self::PointTree>> {
        Ok(PointTreeEnum::Other(PointTreeImpl::new(
            self.values.clone(),
        )))
    }
}

pub struct PointTreeImpl<PV>
where
    PV: PointValues,
{
    values: Rc<Vec<PointValuesSub<PV>>>,
}
impl<PV> PointTreeImpl<PV>
where
    PV: PointValues,
{
    pub fn new<T>(values: T) -> Self
    where
        T: Into<Rc<Vec<PointValuesSub<PV>>>>,
    {
        Self {
            values: values.into(),
        }
    }
}

impl<PV> TryClone for PointTreeImpl<PV>
where
    PV: PointValues,
{
    fn try_clone(&self) -> Result<Self>
    where
        Self: Sized,
    {
        Ok(PointTreeImpl::new(self.values.clone()))
    }
}

impl<PV> PointTree for PointTreeImpl<PV>
where
    PV: PointValues,
{
    fn move_to_child(&mut self) -> Result<bool> {
        Ok(false)
    }

    fn move_to_sibling(&mut self) -> Result<bool> {
        Ok(false)
    }

    fn move_to_parent(&mut self) -> Result<bool> {
        Ok(false)
    }

    fn get_min_packed_value(&self) -> Result<Cow<'_, [u8]>> {
        let mut min_packed_value_opt: Option<Vec<u8>> = None;

        for sub in self.values.iter() {
            let leaf_min_packed_value = sub.sub.get_min_packed_value()?.ok_or_else(|| {
                LuceneError::illegal_state("PointValues returned no min packed value")
            })?;

            match &mut min_packed_value_opt {
                None => {
                    min_packed_value_opt = Some(leaf_min_packed_value.into_owned());
                },
                Some(min_packed_value) => {
                    let num_index_dims = sub.sub.get_num_index_dimensions()?;
                    let num_bytes_per_dim = sub.sub.get_bytes_per_dimension()?;
                    let comparator = ArrayUtil::get_unsigned_comparator(num_bytes_per_dim);
                    for i in 0..num_index_dims {
                        let off = i * num_bytes_per_dim;
                        let v = comparator.compare(
                            leaf_min_packed_value.as_ref(),
                            off,
                            min_packed_value.as_slice(),
                            off,
                        );
                        // unsigned byte-wise compare (lexicographic)
                        if v < 0 {
                            min_packed_value.copy_from(leaf_min_packed_value.as_ref(), off);
                        }
                    }
                },
            }
        }

        Ok(Cow::Owned(min_packed_value_opt.ok_or_else(|| {
            LuceneError::illegal_state("min_packed_value_opt is None")
        })?))
    }

    fn get_max_packed_value(&self) -> Result<Cow<'_, [u8]>> {
        let mut max_packed_value_opt: Option<Vec<u8>> = None;

        for sub in self.values.iter() {
            let leaf_max_packed_value = sub.sub.get_max_packed_value()?.ok_or_else(|| {
                LuceneError::illegal_state("PointValues returned no max packed value")
            })?;

            match &mut max_packed_value_opt {
                None => {
                    max_packed_value_opt = Some(leaf_max_packed_value.into_owned());
                },
                Some(max_packed_value) => {
                    let num_index_dims = sub.sub.get_num_index_dimensions()?;
                    let num_bytes_per_dim = sub.sub.get_bytes_per_dimension()?;
                    let comparator = ArrayUtil::get_unsigned_comparator(num_bytes_per_dim);

                    for i in 0..num_index_dims {
                        let off = i * num_bytes_per_dim;
                        let v = comparator.compare(
                            leaf_max_packed_value.as_ref(),
                            off,
                            max_packed_value.as_slice(),
                            off,
                        );
                        // unsigned byte-wise compare (lexicographic)
                        if v > 0 {
                            max_packed_value.copy_from(leaf_max_packed_value.as_ref(), off);
                        }
                    }
                },
            }
        }

        Ok(Cow::Owned(max_packed_value_opt.ok_or_else(|| {
            LuceneError::illegal_state("max_packed_value_opt is None")
        })?))
    }

    fn size(&self) -> Result<usize> {
        let mut size = 0;
        for sub in self.values.iter() {
            size += sub.sub.size()?;
        }
        Ok(size)
    }

    fn visit_doc_ids<IV>(&mut self, visitor: &mut IV) -> Result<()>
    where
        IV: IntersectVisitor,
    {
        for sub in self.values.iter() {
            let mut wrapped = wrap_intersect_visitor(visitor, sub.doc_base);
            sub.sub.get_point_tree()?.visit_doc_ids(&mut wrapped)?;
        }
        Ok(())
    }

    fn visit_doc_values<IV>(&mut self, visitor: &mut IV) -> Result<()>
    where
        IV: IntersectVisitor,
    {
        for sub in self.values.iter() {
            let mut wrapped = wrap_intersect_visitor(visitor, sub.doc_base);
            sub.sub.get_point_tree()?.visit_doc_values(&mut wrapped)?;
        }
        Ok(())
    }
}
fn wrap_intersect_visitor<IV>(visitor: &mut IV, doc_start: i32) -> IntersectVisitorImpl<'_, IV>
where
    IV: IntersectVisitor,
{
    IntersectVisitorImpl::new(visitor, doc_start)
}

struct IntersectVisitorImpl<'a, IV>
where
    IV: IntersectVisitor,
{
    visitor: &'a mut IV,
    doc_start: i32,
}
impl<'a, IV> IntersectVisitorImpl<'a, IV>
where
    IV: IntersectVisitor,
{
    fn new(visitor: &'a mut IV, doc_start: i32) -> Self {
        Self { visitor, doc_start }
    }
}

impl<'a, IV> IntersectVisitor for IntersectVisitorImpl<'a, IV>
where
    IV: IntersectVisitor,
{
    fn visit(&mut self, doc_id: i32) -> Result<()> {
        self.visitor.visit(self.doc_start + doc_id)
    }

    fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
        self.visitor
            .visit_with_packed_value(self.doc_start + doc_id, packed_value)
    }

    fn compare(&self, min_packed_value: &[u8], max_packed_value: &[u8]) -> Result<Relation> {
        self.visitor.compare(min_packed_value, max_packed_value)
    }
}
