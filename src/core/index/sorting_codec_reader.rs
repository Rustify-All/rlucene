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
use crate::core::codecs::block_term_state::TermStateEnum;
use crate::core::codecs::doc_values_producer::DocValuesProducer;
use crate::core::codecs::dummy::dummy_doc_values_skipper::DummyDocValuesSkipper;
use crate::core::codecs::dummy::dummy_mutable_point_tree::DummyMutablePointTree;
use crate::core::codecs::fields_producer::FieldsProducer;
use crate::core::codecs::hnsw::hnsw_graph_provider::HnswGraphProvider;
use crate::core::codecs::knn_vectors_reader::KnnVectorsReader;
use crate::core::codecs::norms_producer::NormsProducer;
use crate::core::codecs::points_reader::PointsReader;
use crate::core::codecs::stored_fields_reader::{StoredFieldsReader, StoredFieldsReaderEnum2};
use crate::core::codecs::stored_fields_writer::StoredFieldsWriter;
use crate::core::codecs::term_vectors_reader::{DefaultTermVectorsReader, TermVectorsReader};
use crate::core::index::binary_doc_values::BinaryDocValues;
use crate::core::index::binary_doc_values_writer::{BinaryDVs, SortingBinaryDocValues};
use crate::core::index::byte_vector_values::ByteVectorValue;
use crate::core::index::byte_vector_values::ByteVectorValues;
use crate::core::index::codec_reader::{
  CRBits, CRDocValuesProducer, CRFieldsProducer, CRKnnVectorReader, CRNormsProducer,
  CRPointsReader, CRStoredFieldsReader, CRTermVectorsReader, CodecReader,
};
use crate::core::index::doc_values_iterator::DocValuesIterator;
use crate::core::index::dummy::dummy_byte_vector_values::DummyByteVectorValues;
use crate::core::index::dummy::dummy_cache_helper::DummyCacheHelper;
use crate::core::index::dummy::dummy_float_vector_values::DummyFloatVectorValues;
use crate::core::index::dummy::dummy_knn_vector_values::DummyKnnVectorsWriter;
use crate::core::index::field_info::FieldInfo;
use crate::core::index::field_infos::FieldInfos;
use crate::core::index::fields::Fields;
use crate::core::index::float_vector_values::{FloatVectorValue, FloatVectorValues};
use crate::core::index::freq_prox_terms_writer::{
  SortingPostingsEnumType, SortingTerms, SortingTermsEnum,
};
use crate::core::index::index_reader::{
  Identity, IndexReader, IndexReaderBase, LeafReaderContextKind,
};
use crate::core::index::knn_vector_values::{
  BitsImpl, DocIndexIterator, DocIndexIteratorEnum2, KnnVectorValues,
};
use crate::core::index::leaf_metadata::LeafMetaData;
use crate::core::index::leaf_reader::LeafReader;
use crate::core::index::numeric_doc_values::NumericDocValues;
use crate::core::index::numeric_doc_values_writer::{NumericDVs, SortingNumericDocValues};
use crate::core::index::point_values::{
  IntersectVisitor, PointTree, PointTreeEnum, PointTreeEnum2, PointValues, Relation,
};
use crate::core::index::postings_enum::PostingsEnumEnum2;
use crate::core::index::sorted_doc_values::SortedDocValues;
use crate::core::index::sorted_doc_values_terms_enum::SortedDocValuesTermsEnum;
use crate::core::index::sorted_doc_values_writer::SortingSortedDocValues;
use crate::core::index::sorted_numeric_doc_values::SortedNumericDocValues;
use crate::core::index::sorted_numeric_doc_values_writer::{
  LongValues, SortingSortedNumericDocValues,
};
use crate::core::index::sorted_set_doc_values::SortedSetDocValues;
use crate::core::index::sorted_set_doc_values_terms_enum::SortedSetDocValuesTermsEnum;
use crate::core::index::sorted_set_doc_values_writer::{
  DocOrds, START_BITS_PER_VALUE, SortingSortedSetDocValues,
};
use crate::core::index::sorter::{DocMap, DocMapImpl, Sorter};
use crate::core::index::stored_field_visitor::StoredFieldVisitor;
use crate::core::index::stored_fields::{RawStoredFieldsReader, StoredFields};
use crate::core::index::term::Term;
use crate::core::index::term_vectors::{RawTermVectors, TermVectors};
use crate::core::index::terms::Terms;
use crate::core::index::terms_enum::{SeekStatus, TermsEnum};
use crate::core::index::vector_encoding::VectorEncoding;
use crate::core::index::{BytesRef, BytesRefValue, BytesRefValueEnum};
use crate::core::search::doc_id_set_iterator::DocIdSetIterator;
use crate::core::search::doc_id_set_iterator::NO_MORE_DOCS;
use crate::core::search::dummy::dummy_vector_scorer::DummyVectorScorer;
use crate::core::search::knn_collector::KnnCollector;
use crate::core::search::sort::Sort;
use crate::core::util::HasIdentity;
use crate::core::util::access::ByteSource;
use crate::core::util::automation::compiled_automaton::CompiledAutomaton;
use crate::core::util::bit_set::BitSet;
use crate::core::util::bit_set_iterator::BitSetIterator;
use crate::core::util::bits::{Bits, BitsEnum2};
use crate::core::util::bytes_ref_iterator::BytesRefIterator;
use crate::core::util::clone::TryClone;
use crate::core::util::close::CloseableRef;
use crate::core::util::dummy::dummy_hnsw_graph::DummyHnswGraph;
use crate::core::util::error::lucene_error::{LuceneError, Result};
use crate::core::util::fixed_bit_set::FixedBitSet;
use crate::core::util::packed::PackedInts;
use crate::core::util::supplier::Supplier;
use parking_lot::{Mutex, MutexGuard};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

#[derive(Clone)]
pub enum CachedObject {
  Numeric(Arc<NumericDVs<FixedBitSet>>),
  Binary(Arc<BinaryDVs>),
  Sorted(Arc<Vec<i32>>),
  SortedNumeric(Arc<LongValues>),
  SortedSet(Arc<DocOrds>),
}

/// An [`CodecReader`] which supports sorting documents by a given [`Sort`]. This can be used to
/// re-sort an index after it has been created by wrapping all readers of the index with this reader
/// and adding it to a fresh [`IndexWriter`](crate::core::index::index_writer::IndexWriter) via
/// `IndexWriter::add_indexes(CodecReader...)`.
///
/// **NOTE**: This reader should only be used for merging. Pulling fields from this reader might be
/// very costly and memory intensive.
pub struct SortingCodecReader<CR, DM> {
  in_: CR,
  doc_map: DM,
  meta_data: LeafMetaData,
  inner: Arc<(IndexReaderBase, Mutex<Inner>)>,
}
pub struct Inner {
  // we try to cache the last used DV or Norms instance since during merge
  // this instance is used more than once. We could in addition to this single instance
  // also cache the fields that are used for sorting since we do the work twice for these fields
  cached_field: Option<Arc<FieldInfo>>,
  cache_is_norms: bool,
  cached_object: Option<CachedObject>,
  cache_stats: HashMap<String, i32>,
  sort: Option<Arc<Sort>>,
}

impl<CR, DM> SortingCodecReader<CR, DM> {
  pub fn new(base: CR, doc_map: DM, meta_data: LeafMetaData) -> Self {
    let inner = Arc::new((
      IndexReaderBase::new(),
      Mutex::new(Inner {
        cached_field: None,
        cache_is_norms: false,
        cached_object: None,
        cache_stats: HashMap::new(),
        sort: meta_data.sort.clone(),
      }),
    ));
    Self {
      in_: base,
      doc_map,
      meta_data,
      inner,
    }
  }
}

impl<CR, DM> LeafReader for SortingCodecReader<CR, DM>
where
  CR: CodecReader,
  DM: DocMap + Clone,
{
  type CacheHelper = DummyCacheHelper;

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    Ok(None)
  }

  type Terms = <CR as LeafReader>::Terms;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    LeafReader::terms(&self.in_, field)
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

  type FloatVectorValues = <CR as LeafReader>::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Option<Self::FloatVectorValues>> {
    LeafReader::get_float_vector_values(&self.in_, field)
  }

  type ByteVectorValues = <CR as LeafReader>::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Option<Self::ByteVectorValues>> {
    LeafReader::get_byte_vector_values(&self.in_, field)
  }

  fn search_nearest_vectors_f32<B, K, TV>(
    &self,
    field: &str,
    target: TV,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
    TV: Into<Vec<f32>>,
  {
    let target = target.into();
    LeafReader::search_nearest_vectors_f32(&self.in_, field, target, knn_collector, accept_docs)
  }

  fn search_nearest_vectors_u8<B, K, TV>(
    &self,
    field: &str,
    target: TV,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
    TV: Into<Vec<u8>>,
  {
    let target = target.into();
    LeafReader::search_nearest_vectors_u8(&self.in_, field, target, knn_collector, accept_docs)
  }

  fn get_field_infos(&self) -> Result<Arc<FieldInfos>> {
    self.in_.get_field_infos()
  }

  type Bits = SortingBitsImpl<CRBits<CR>, DM>;

  fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
    Ok(
      self
        .in_
        .get_live_docs()?
        .map(|in_live_docs| SortingBitsImpl::new(in_live_docs, self.doc_map.clone())),
    )
  }

  type PointValues = <CR as LeafReader>::PointValues;

  fn get_point_values(&self, field: &str) -> Result<Option<Self::PointValues>> {
    LeafReader::get_point_values(&self.in_, field)
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
  type ContextKind = LeafReaderContextKind;

  type TermVectors = <CR as IndexReader>::TermVectors;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    IndexReader::term_vectors(&self.in_)
  }

  fn max_doc(&self) -> Result<i32> {
    self.in_.max_doc()
  }

  fn num_docs(&self) -> Result<i32> {
    self.in_.num_docs()
  }

  type StoredFields = <CR as IndexReader>::StoredFields;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    IndexReader::stored_fields(&self.in_)
  }

  fn do_close(&self) -> Result<()> {
    self.in_.do_close()
  }

  type ReaderCacheHelper = DummyCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    Ok(None)
  }

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    IndexReader::doc_freq(&self.in_, term)
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    IndexReader::total_term_freq(&self.in_, term)
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    IndexReader::get_sum_doc_freq(&self.in_, field)
  }
  fn get_doc_count(&self, field: &str) -> Result<i32> {
    IndexReader::get_doc_count(&self.in_, field)
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    IndexReader::get_sum_total_term_freq(&self.in_, field)
  }

  fn index_base(&self) -> &IndexReaderBase {
    &self.inner.0
  }
}

impl<CR, DM> Display for SortingCodecReader<CR, DM>
where
  CR: CodecReader,
  DM: DocMap + Clone,
{
  fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
    write!(f, "SortingCodecReader({})", self.in_)
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
  type KnnVectorsReader = KnnVectorsReaderImpl<CRKnnVectorReader<CR>, DM>;

  fn get_fields_reader(&self) -> Result<Option<Self::StoredFieldsReader>> {
    Ok(
      self
        .in_
        .get_fields_reader()?
        .map(|delegate| new_stored_fields_reader(delegate, self.doc_map.clone())),
    )
  }

  fn get_term_vectors_reader(&self) -> Result<Option<Self::TermVectorsReader>> {
    Ok(
      self
        .in_
        .get_term_vectors_reader()?
        .map(|delegate| new_term_vectors_reader(delegate, self.doc_map.clone())),
    )
  }

  fn get_norms_reader(&self) -> Result<Option<Self::NormsProducer>> {
    let Some(delegate) = self.in_.get_norms_reader()? else {
      return Ok(None);
    };
    let v = NormsProducerImpl::new(
      delegate,
      self.inner.clone(),
      self.max_doc()?,
      self.doc_map.clone(),
    );
    Ok(Some(v))
  }

  fn get_doc_values_reader(&self) -> Result<Option<Self::DocValuesProducer>> {
    let Some(delegate) = self.in_.get_doc_values_reader()? else {
      return Ok(None);
    };
    let v = DocValuesProducerImpl::new(
      delegate,
      self.inner.clone(),
      self.max_doc()?,
      self.doc_map.clone(),
    );
    Ok(Some(v))
  }

  fn get_postings_reader(&self) -> Result<Option<Self::FieldsProducer>> {
    let Some(posting_reader) = self.in_.get_postings_reader()? else {
      return Ok(None);
    };
    let field_infos = self.in_.get_field_infos()?;
    Ok(Some(FieldsProducerImpl::new(
      posting_reader,
      self.doc_map.clone(),
      field_infos,
    )))
  }

  fn get_points_reader(&self) -> Result<Option<Self::PointsReader>> {
    let Some(delegate) = self.in_.get_points_reader()? else {
      return Ok(None);
    };
    Ok(Some(PointsReaderImpl::new(delegate, self.doc_map.clone())))
  }

  fn get_vector_reader(&self) -> Result<Option<Self::KnnVectorsReader>> {
    let Some(delegate) = self.in_.get_vector_reader()? else {
      return Ok(None);
    };
    Ok(Some(KnnVectorsReaderImpl::new(
      delegate,
      self.doc_map.clone(),
    )))
  }
}

fn new_term_vectors_reader<T, DM>(delegate: T, doc_map: DM) -> TermVectorsReaderImpl<T, DM>
where
  T: TermVectorsReader,
  DM: DocMap + Clone,
{
  TermVectorsReaderImpl::new(delegate, doc_map)
}

pub struct TermVectorsReaderImpl<T, DM> {
  delegate: T,
  doc_map: DM,
}
impl<T, DM> TermVectorsReaderImpl<T, DM> {
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
    Err(LuceneError::unsupported_operation(
      "raw term vectors are not available for SortingCodecReader",
    ))
  }

  fn raw_term_vectors(&self) -> Result<&DefaultTermVectorsReader<Self::IndexInput>> {
    Err(LuceneError::unsupported_operation(
      "raw term vectors are not available for SortingCodecReader",
    ))
  }
}

impl<T, DM> TryClone for TermVectorsReaderImpl<T, DM>
where
  DM: DocMap + Clone,
  T: TermVectorsReader,
{
  fn try_clone(&self) -> Result<Self>
  where
    Self: Sized,
  {
    Ok(new_term_vectors_reader(
      self.delegate.try_clone()?,
      self.doc_map.clone(),
    ))
  }
}

impl<T, DM> CloseableRef for TermVectorsReaderImpl<T, DM>
where
  T: CloseableRef,
{
  fn close(&self) -> Result<()> {
    self.delegate.close()
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

pub enum SortingCodecReaderTermVectorsReader<T, DM> {
  Filter(T),
  Sorting(TermVectorsReaderImpl<T, DM>),
}

impl<T, DM> CloseableRef for SortingCodecReaderTermVectorsReader<T, DM>
where
  T: TermVectorsReader,
  DM: DocMap + Clone,
{
  fn close(&self) -> Result<()> {
    match self {
      Self::Filter(reader) => reader.close(),
      Self::Sorting(reader) => reader.close(),
    }
  }
}

impl<T, DM> RawTermVectors for SortingCodecReaderTermVectorsReader<T, DM>
where
  T: TermVectorsReader,
  DM: DocMap + Clone,
{
  type IndexInput = T::IndexInput;

  fn raw_term_vectors_mut(&mut self) -> Result<&mut DefaultTermVectorsReader<Self::IndexInput>> {
    match self {
      Self::Filter(reader) => reader.raw_term_vectors_mut(),
      Self::Sorting(reader) => reader.raw_term_vectors_mut(),
    }
  }

  fn raw_term_vectors(&self) -> Result<&DefaultTermVectorsReader<Self::IndexInput>> {
    match self {
      Self::Filter(reader) => reader.raw_term_vectors(),
      Self::Sorting(reader) => reader.raw_term_vectors(),
    }
  }
}

impl<T, DM> TermVectors for SortingCodecReaderTermVectorsReader<T, DM>
where
  T: TermVectorsReader,
  DM: DocMap + Clone,
{
  type Fields = T::Fields;
  type Terms = <T::Fields as Fields>::Terms;

  fn prefetch(&mut self, doc_id: i32) -> Result<()> {
    match self {
      Self::Filter(reader) => reader.prefetch(doc_id),
      Self::Sorting(reader) => reader.prefetch(doc_id),
    }
  }

  fn get(&mut self, doc: i32) -> Result<Option<Self::Fields>> {
    match self {
      Self::Filter(reader) => reader.get(doc),
      Self::Sorting(reader) => reader.get(doc),
    }
  }

  fn get_field_terms(
    &mut self,
    doc: i32,
    field: &str,
  ) -> Result<Option<<Self::Fields as Fields>::Terms>> {
    match self {
      Self::Filter(reader) => reader.get_field_terms(doc, field),
      Self::Sorting(reader) => reader.get_field_terms(doc, field),
    }
  }
}

impl<T, DM> TryClone for SortingCodecReaderTermVectorsReader<T, DM>
where
  T: TermVectorsReader,
  DM: DocMap + Clone,
{
  fn try_clone(&self) -> Result<Self> {
    match self {
      Self::Filter(reader) => reader.try_clone().map(Self::Filter),
      Self::Sorting(reader) => reader.try_clone().map(Self::Sorting),
    }
  }
}

impl<T, DM> TermVectorsReader for SortingCodecReaderTermVectorsReader<T, DM>
where
  T: TermVectorsReader,
  DM: DocMap + Clone,
{
  fn check_integrity(&self) -> Result<()> {
    match self {
      Self::Filter(reader) => reader.check_integrity(),
      Self::Sorting(reader) => reader.check_integrity(),
    }
  }

  fn get_merge_instance(&self) -> Result<Option<Self>> {
    match self {
      Self::Filter(reader) => Ok(reader.get_merge_instance()?.map(Self::Filter)),
      Self::Sorting(reader) => Ok(reader.get_merge_instance()?.map(Self::Sorting)),
    }
  }
}

pub struct NormsProducerImpl<NP, DM> {
  delegate: NP,
  inner: Arc<(IndexReaderBase, Mutex<Inner>)>,
  max_doc: i32,
  doc_map: DM,
}
impl<NP, DM> NormsProducerImpl<NP, DM> {
  fn new(
    delegate: NP,
    inner: Arc<(IndexReaderBase, Mutex<Inner>)>,
    max_doc: i32,
    doc_map: DM,
  ) -> Self {
    Self {
      delegate,
      inner,
      max_doc,
      doc_map,
    }
  }
}

impl<NP, DM> CloseableRef for NormsProducerImpl<NP, DM>
where
  NP: CloseableRef,
{
  fn close(&self) -> Result<()> {
    self.delegate.close()
  }
}

impl<NP, DM> NormsProducer for NormsProducerImpl<NP, DM>
where
  NP: NormsProducer,
  DM: DocMap,
{
  type MergeInstance = Self;
  type NumericDocValues = SortingNumericDocValues<FixedBitSet>;

  fn get_norms(&self, field: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
    let v = get_or_create_norms(
      field,
      || {
        let numeric = get_numeric_doc_values(
          &mut self.delegate.get_norms(field)?,
          self.max_doc as usize,
          &self.doc_map,
        )?;
        Ok(CachedObject::Numeric(Arc::new(numeric)))
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

pub enum SortingNormsProducerEnum<NP, DM> {
  A(NP),
  B(NormsProducerImpl<NP, DM>),
}

impl<NP, DM> CloseableRef for SortingNormsProducerEnum<NP, DM>
where
  NP: CloseableRef,
{
  fn close(&self) -> Result<()> {
    match self {
      Self::A(producer) => producer.close(),
      Self::B(producer) => producer.close(),
    }
  }
}

impl<NP, DM> NormsProducer for SortingNormsProducerEnum<NP, DM>
where
  NP: NormsProducer,
  DM: DocMap,
{
  type MergeInstance = crate::core::codecs::norms_producer::NormsProducerEnum2<
    NP::MergeInstance,
    NormsProducerImpl<NP, DM>,
  >;
  type NumericDocValues = SortingCodecReaderNumericDocValues<NP::NumericDocValues>;

  fn get_norms(&self, field: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
    match self {
      Self::A(producer) => producer
        .get_norms(field)
        .map(SortingCodecReaderNumericDocValues::Original),
      Self::B(producer) => producer
        .get_norms(field)
        .map(SortingCodecReaderNumericDocValues::Sorting),
    }
  }

  fn check_integrity(&self) -> Result<()> {
    match self {
      Self::A(producer) => producer.check_integrity(),
      Self::B(producer) => producer.check_integrity(),
    }
  }

  fn get_merge_instance(&self) -> Result<Option<Self::MergeInstance>> {
    match self {
      Self::A(producer) => Ok(
        producer
          .get_merge_instance()?
          .map(crate::core::codecs::norms_producer::NormsProducerEnum2::A),
      ),
      Self::B(producer) => Ok(
        producer
          .get_merge_instance()?
          .map(crate::core::codecs::norms_producer::NormsProducerEnum2::B),
      ),
    }
  }
}

pub struct DocValuesProducerImpl<DVP, DM> {
  delegate: DVP,
  inner: Arc<(IndexReaderBase, Mutex<Inner>)>,
  max_doc: i32,
  doc_map: DM,
}
impl<DVP, DM> DocValuesProducerImpl<DVP, DM> {
  fn new(
    delegate: DVP,
    inner: Arc<(IndexReaderBase, Mutex<Inner>)>,
    max_doc: i32,
    doc_map: DM,
  ) -> Self {
    Self {
      delegate,
      inner,
      max_doc,
      doc_map,
    }
  }
}

impl<DVP, DM> CloseableRef for DocValuesProducerImpl<DVP, DM>
where
  DVP: CloseableRef,
{
  fn close(&self) -> Result<()> {
    self.delegate.close()
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
      field,
      || {
        let v = get_numeric_doc_values(
          &mut self.delegate.get_numeric(field)?,
          self.max_doc as usize,
          &self.doc_map,
        )?;
        Ok(CachedObject::Numeric(Arc::new(v)))
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
      field,
      || {
        let binary = BinaryDVs::new(
          self.max_doc as usize,
          &self.doc_map,
          &mut self.delegate.get_binary(field)?,
        )?;
        Ok(CachedObject::Binary(Arc::new(binary)))
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
      field,
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

  type SortedNumericDocValues = SortingSortedNumericDocValues<
    <DVP as DocValuesProducer>::SortedNumericDocValues,
    Arc<LongValues>,
  >;

  fn get_sorted_numeric(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedNumericDocValues> {
    let mut old_doc_values = self.delegate.get_sorted_numeric(field)?;

    let v = get_or_create_dv(
      field,
      || {
        let long_values = LongValues::new(
          self.max_doc as usize,
          &self.doc_map,
          &mut old_doc_values,
          PackedInts::FAST,
        )?;
        Ok(CachedObject::SortedNumeric(Arc::new(long_values)))
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
    SortingSortedSetDocValues<<DVP as DocValuesProducer>::SortedSetDocValues, Arc<DocOrds>>;

  fn get_sorted_set(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedSetDocValues> {
    let mut old_doc_values = self.delegate.get_sorted_set(field)?;

    let v = get_or_create_dv(
      field,
      || {
        let doc_ords = DocOrds::new(
          self.max_doc,
          &self.doc_map,
          &mut old_doc_values,
          PackedInts::FAST,
          START_BITS_PER_VALUE,
        )?;
        Ok(CachedObject::SortedSet(Arc::new(doc_ords)))
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

pub enum SortingCodecReaderSortedDocValues<S> {
  Original(S),
  Sorting(SortingSortedDocValues<S>),
}

impl<S> DocValuesIterator for SortingCodecReaderSortedDocValues<S>
where
  S: SortedDocValues,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::Original(values) => values.advance_exact(target),
      Self::Sorting(values) => values.advance_exact(target),
    }
  }
}

impl<S> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SortingCodecReaderSortedDocValues<S>
where
  S: SortedDocValues,
{
}
impl<S> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for SortingCodecReaderSortedDocValues<S>
where
  S: SortedDocValues,
{
}

impl<S> DocIdSetIterator for SortingCodecReaderSortedDocValues<S>
where
  S: SortedDocValues,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::Original(values) => values.doc_id(),
      Self::Sorting(values) => values.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::Original(values) => values.next_doc(),
      Self::Sorting(values) => values.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Original(values) => values.advance(target),
      Self::Sorting(values) => values.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Original(values) => values.slow_advance(target),
      Self::Sorting(values) => values.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::Original(values) => values.cost(),
      Self::Sorting(values) => values.cost(),
    }
  }
}

pub enum SortingCodecReaderSortedDocValuesTermsEnum<'a, S>
where
  S: SortedDocValues,
{
  Original(S::TermsEnum<'a>),
  Sorting(SortedDocValuesTermsEnum<&'a mut SortingSortedDocValues<S>>),
}

impl<'a, S> BytesRefIterator for SortingCodecReaderSortedDocValuesTermsEnum<'a, S>
where
  S: SortedDocValues,
{
  type Value<'b>
    = BytesRefValueEnum<'b>
  where
    Self: 'b;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    match self {
      Self::Original(terms) => terms
        .next()
        .map(|value| value.map(BytesRefValue::into_value)),
      Self::Sorting(terms) => terms
        .next()
        .map(|value| value.map(BytesRefValue::into_value)),
    }
  }

  fn set_next(&mut self) -> Result<bool> {
    match self {
      Self::Original(terms) => terms.set_next(),
      Self::Sorting(terms) => terms.set_next(),
    }
  }
}

impl<'a, S> TermsEnum for SortingCodecReaderSortedDocValuesTermsEnum<'a, S>
where
  S: SortedDocValues,
{
  type AttributeSource<'b>
    = <S::TermsEnum<'a> as TermsEnum>::AttributeSource<'b>
  where
    Self: 'b;
  type AttributeSourceMut<'b>
    = <S::TermsEnum<'a> as TermsEnum>::AttributeSourceMut<'b>
  where
    Self: 'b;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    match self {
      Self::Original(terms) => terms.attributes(),
      Self::Sorting(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    match self {
      Self::Original(terms) => terms.attributes_mut(),
      Self::Sorting(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool> {
    match self {
      Self::Original(terms) => terms.seek_exact(term),
      Self::Sorting(terms) => terms.seek_exact(term),
    }
  }

  fn prepare_seek_exact<BS: ByteSource>(&mut self, text: &BytesRef<BS>) -> Result<Option<()>> {
    match self {
      Self::Original(terms) => terms.prepare_seek_exact(text),
      Self::Sorting(terms) => terms.prepare_seek_exact(text),
    }
  }

  fn get_prepare_seek_exact_status<BS: ByteSource>(
    &mut self,
    target: &BytesRef<BS>,
  ) -> Result<bool> {
    match self {
      Self::Original(terms) => terms.get_prepare_seek_exact_status(target),
      Self::Sorting(terms) => terms.get_prepare_seek_exact_status(target),
    }
  }

  fn seek_ceil<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<SeekStatus> {
    match self {
      Self::Original(terms) => terms.seek_ceil(term),
      Self::Sorting(terms) => terms.seek_ceil(term),
    }
  }

  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    match self {
      Self::Original(terms) => terms.seek_exact_with_ord(ord),
      Self::Sorting(terms) => terms.seek_exact_with_ord(ord),
    }
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    state: &TermStateEnum,
  ) -> Result<()> {
    match self {
      Self::Original(terms) => terms.seek_exact_with_state(term, state),
      Self::Sorting(terms) => terms.seek_exact_with_state(term, state),
    }
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    match self {
      Self::Original(terms) => terms.term().map(BytesRefValue::into_value),
      Self::Sorting(terms) => terms.term().map(BytesRefValue::into_value),
    }
  }

  fn ord(&self) -> Result<i64> {
    match self {
      Self::Original(terms) => terms.ord(),
      Self::Sorting(terms) => terms.ord(),
    }
  }

  fn doc_freq(&mut self) -> Result<i32> {
    match self {
      Self::Original(terms) => terms.doc_freq(),
      Self::Sorting(terms) => terms.doc_freq(),
    }
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    match self {
      Self::Original(terms) => terms.total_term_freq(),
      Self::Sorting(terms) => terms.total_term_freq(),
    }
  }

  type PostingsEnum = <S::TermsEnum<'a> as TermsEnum>::PostingsEnum;

  fn postings(&mut self, reuse: Option<Self::PostingsEnum>) -> Result<Self::PostingsEnum> {
    match self {
      Self::Original(terms) => terms.postings(reuse),
      Self::Sorting(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    match self {
      Self::Original(terms) => terms.postings_with_flags(reuse, flags),
      Self::Sorting(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  type ImpactsEnum = <S::TermsEnum<'a> as TermsEnum>::ImpactsEnum;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    match self {
      Self::Original(terms) => terms.impacts(flags),
      Self::Sorting(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    match self {
      Self::Original(terms) => terms.term_state(),
      Self::Sorting(terms) => terms.term_state(),
    }
  }
}

impl<S> SortedDocValues for SortingCodecReaderSortedDocValues<S>
where
  S: SortedDocValues,
{
  type OrdValue<'a>
    = S::OrdValue<'a>
  where
    Self: 'a;

  fn ord_value(&mut self) -> Result<i32> {
    match self {
      Self::Original(values) => values.ord_value(),
      Self::Sorting(values) => values.ord_value(),
    }
  }

  fn lookup_ord(&mut self, ord: i32) -> Result<Self::OrdValue<'_>> {
    match self {
      Self::Original(values) => values.lookup_ord(ord),
      Self::Sorting(values) => values.lookup_ord(ord),
    }
  }

  fn get_value_count(&self) -> Result<i32> {
    match self {
      Self::Original(values) => values.get_value_count(),
      Self::Sorting(values) => values.get_value_count(),
    }
  }

  fn lookup_term<BS: ByteSource>(&mut self, key: &BytesRef<BS>) -> Result<i32> {
    match self {
      Self::Original(values) => values.lookup_term(key),
      Self::Sorting(values) => values.lookup_term(key),
    }
  }

  type TermsEnum<'a>
    = SortingCodecReaderSortedDocValuesTermsEnum<'a, S>
  where
    S: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    match self {
      Self::Original(values) => values
        .terms_enum()
        .map(SortingCodecReaderSortedDocValuesTermsEnum::Original),
      Self::Sorting(values) => values
        .terms_enum()
        .map(SortingCodecReaderSortedDocValuesTermsEnum::Sorting),
    }
  }
}

pub enum SortingCodecReaderSortedSetDocValues<S> {
  Original(S),
  Sorting(SortingSortedSetDocValues<S, Arc<DocOrds>>),
}

impl<S> DocValuesIterator for SortingCodecReaderSortedSetDocValues<S>
where
  S: SortedSetDocValues,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::Original(values) => values.advance_exact(target),
      Self::Sorting(values) => values.advance_exact(target),
    }
  }
}

impl<S> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SortingCodecReaderSortedSetDocValues<S>
where
  S: SortedSetDocValues,
{
}
impl<S> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for SortingCodecReaderSortedSetDocValues<S>
where
  S: SortedSetDocValues,
{
}

impl<S> DocIdSetIterator for SortingCodecReaderSortedSetDocValues<S>
where
  S: SortedSetDocValues,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::Original(values) => values.doc_id(),
      Self::Sorting(values) => values.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::Original(values) => values.next_doc(),
      Self::Sorting(values) => values.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Original(values) => values.advance(target),
      Self::Sorting(values) => values.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Original(values) => values.slow_advance(target),
      Self::Sorting(values) => values.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::Original(values) => values.cost(),
      Self::Sorting(values) => values.cost(),
    }
  }
}

pub enum SortingCodecReaderSortedSetDocValuesTermsEnum<'a, S>
where
  S: SortedSetDocValues,
{
  Original(S::TermsEnum<'a>),
  Sorting(SortedSetDocValuesTermsEnum<&'a mut SortingSortedSetDocValues<S, Arc<DocOrds>>>),
}

impl<'a, S> BytesRefIterator for SortingCodecReaderSortedSetDocValuesTermsEnum<'a, S>
where
  S: SortedSetDocValues,
{
  type Value<'b>
    = BytesRefValueEnum<'b>
  where
    Self: 'b;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    match self {
      Self::Original(terms) => terms
        .next()
        .map(|value| value.map(BytesRefValue::into_value)),
      Self::Sorting(terms) => terms
        .next()
        .map(|value| value.map(BytesRefValue::into_value)),
    }
  }

  fn set_next(&mut self) -> Result<bool> {
    match self {
      Self::Original(terms) => terms.set_next(),
      Self::Sorting(terms) => terms.set_next(),
    }
  }
}

impl<'a, S> TermsEnum for SortingCodecReaderSortedSetDocValuesTermsEnum<'a, S>
where
  S: SortedSetDocValues,
{
  type AttributeSource<'b>
    = <S::TermsEnum<'a> as TermsEnum>::AttributeSource<'b>
  where
    Self: 'b;
  type AttributeSourceMut<'b>
    = <S::TermsEnum<'a> as TermsEnum>::AttributeSourceMut<'b>
  where
    Self: 'b;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    match self {
      Self::Original(terms) => terms.attributes(),
      Self::Sorting(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    match self {
      Self::Original(terms) => terms.attributes_mut(),
      Self::Sorting(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool> {
    match self {
      Self::Original(terms) => terms.seek_exact(term),
      Self::Sorting(terms) => terms.seek_exact(term),
    }
  }

  fn prepare_seek_exact<BS: ByteSource>(&mut self, text: &BytesRef<BS>) -> Result<Option<()>> {
    match self {
      Self::Original(terms) => terms.prepare_seek_exact(text),
      Self::Sorting(terms) => terms.prepare_seek_exact(text),
    }
  }

  fn get_prepare_seek_exact_status<BS: ByteSource>(
    &mut self,
    target: &BytesRef<BS>,
  ) -> Result<bool> {
    match self {
      Self::Original(terms) => terms.get_prepare_seek_exact_status(target),
      Self::Sorting(terms) => terms.get_prepare_seek_exact_status(target),
    }
  }

  fn seek_ceil<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<SeekStatus> {
    match self {
      Self::Original(terms) => terms.seek_ceil(term),
      Self::Sorting(terms) => terms.seek_ceil(term),
    }
  }

  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    match self {
      Self::Original(terms) => terms.seek_exact_with_ord(ord),
      Self::Sorting(terms) => terms.seek_exact_with_ord(ord),
    }
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    state: &TermStateEnum,
  ) -> Result<()> {
    match self {
      Self::Original(terms) => terms.seek_exact_with_state(term, state),
      Self::Sorting(terms) => terms.seek_exact_with_state(term, state),
    }
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    match self {
      Self::Original(terms) => terms.term().map(BytesRefValue::into_value),
      Self::Sorting(terms) => terms.term().map(BytesRefValue::into_value),
    }
  }

  fn ord(&self) -> Result<i64> {
    match self {
      Self::Original(terms) => terms.ord(),
      Self::Sorting(terms) => terms.ord(),
    }
  }

  fn doc_freq(&mut self) -> Result<i32> {
    match self {
      Self::Original(terms) => terms.doc_freq(),
      Self::Sorting(terms) => terms.doc_freq(),
    }
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    match self {
      Self::Original(terms) => terms.total_term_freq(),
      Self::Sorting(terms) => terms.total_term_freq(),
    }
  }

  type PostingsEnum = <S::TermsEnum<'a> as TermsEnum>::PostingsEnum;

  fn postings(&mut self, reuse: Option<Self::PostingsEnum>) -> Result<Self::PostingsEnum> {
    match self {
      Self::Original(terms) => terms.postings(reuse),
      Self::Sorting(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    match self {
      Self::Original(terms) => terms.postings_with_flags(reuse, flags),
      Self::Sorting(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  type ImpactsEnum = <S::TermsEnum<'a> as TermsEnum>::ImpactsEnum;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    match self {
      Self::Original(terms) => terms.impacts(flags),
      Self::Sorting(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    match self {
      Self::Original(terms) => terms.term_state(),
      Self::Sorting(terms) => terms.term_state(),
    }
  }
}

impl<S> SortedSetDocValues for SortingCodecReaderSortedSetDocValues<S>
where
  S: SortedSetDocValues,
{
  type OrdValue<'a>
    = S::OrdValue<'a>
  where
    Self: 'a;

  fn next_ord(&mut self) -> Result<i64> {
    match self {
      Self::Original(values) => values.next_ord(),
      Self::Sorting(values) => values.next_ord(),
    }
  }

  fn doc_value_count(&mut self) -> Result<i32> {
    match self {
      Self::Original(values) => values.doc_value_count(),
      Self::Sorting(values) => values.doc_value_count(),
    }
  }

  fn lookup_ord(&mut self, ord: i64) -> Result<Self::OrdValue<'_>> {
    match self {
      Self::Original(values) => values.lookup_ord(ord),
      Self::Sorting(values) => values.lookup_ord(ord),
    }
  }

  fn get_value_count(&self) -> Result<i64> {
    match self {
      Self::Original(values) => values.get_value_count(),
      Self::Sorting(values) => values.get_value_count(),
    }
  }

  fn lookup_term<BS: ByteSource>(&mut self, key: &BytesRef<BS>) -> Result<i64> {
    match self {
      Self::Original(values) => values.lookup_term(key),
      Self::Sorting(values) => values.lookup_term(key),
    }
  }

  type TermsEnum<'a>
    = SortingCodecReaderSortedSetDocValuesTermsEnum<'a, S>
  where
    S: 'a;

  fn terms_enum(&mut self) -> Result<Self::TermsEnum<'_>> {
    match self {
      Self::Original(values) => values
        .terms_enum()
        .map(SortingCodecReaderSortedSetDocValuesTermsEnum::Original),
      Self::Sorting(values) => values
        .terms_enum()
        .map(SortingCodecReaderSortedSetDocValuesTermsEnum::Sorting),
    }
  }

  fn is_single_valued(&self) -> bool {
    match self {
      Self::Original(values) => values.is_single_valued(),
      Self::Sorting(values) => values.is_single_valued(),
    }
  }

  type SortedDocValues = <S as SortedSetDocValues>::SortedDocValues;

  fn get_sorted_doc_values(&mut self) -> Result<Self::SortedDocValues> {
    match self {
      Self::Original(values) => values.get_sorted_doc_values(),
      Self::Sorting(_) => Err(LuceneError::unsupported_operation("")),
    }
  }
}

pub enum SortingCodecReaderNumericDocValues<N> {
  Original(N),
  Sorting(SortingNumericDocValues<FixedBitSet>),
}

pub enum SortingCodecReaderBinaryDocValues<B> {
  Original(B),
  Sorting(SortingBinaryDocValues),
}

impl<N> DocValuesIterator for SortingCodecReaderNumericDocValues<N>
where
  N: NumericDocValues,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::Original(values) => values.advance_exact(target),
      Self::Sorting(values) => values.advance_exact(target),
    }
  }
}

impl<N> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SortingCodecReaderNumericDocValues<N>
where
  N: NumericDocValues,
{
}
impl<N> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for SortingCodecReaderNumericDocValues<N>
where
  N: NumericDocValues,
{
}

impl<N> DocIdSetIterator for SortingCodecReaderNumericDocValues<N>
where
  N: NumericDocValues,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::Original(values) => values.doc_id(),
      Self::Sorting(values) => values.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::Original(values) => values.next_doc(),
      Self::Sorting(values) => values.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Original(values) => values.advance(target),
      Self::Sorting(values) => values.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Original(values) => values.slow_advance(target),
      Self::Sorting(values) => values.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::Original(values) => values.cost(),
      Self::Sorting(values) => values.cost(),
    }
  }
}

impl<N> NumericDocValues for SortingCodecReaderNumericDocValues<N>
where
  N: NumericDocValues,
{
  fn long_value(&mut self) -> Result<i64> {
    match self {
      Self::Original(values) => values.long_value(),
      Self::Sorting(values) => values.long_value(),
    }
  }
}

impl<B> DocValuesIterator for SortingCodecReaderBinaryDocValues<B>
where
  B: BinaryDocValues,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::Original(values) => values.advance_exact(target),
      Self::Sorting(values) => values.advance_exact(target),
    }
  }
}

impl<B> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SortingCodecReaderBinaryDocValues<B>
where
  B: BinaryDocValues,
{
}
impl<B> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for SortingCodecReaderBinaryDocValues<B>
where
  B: BinaryDocValues,
{
}

impl<B> DocIdSetIterator for SortingCodecReaderBinaryDocValues<B>
where
  B: BinaryDocValues,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::Original(values) => values.doc_id(),
      Self::Sorting(values) => values.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::Original(values) => values.next_doc(),
      Self::Sorting(values) => values.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Original(values) => values.advance(target),
      Self::Sorting(values) => values.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Original(values) => values.slow_advance(target),
      Self::Sorting(values) => values.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::Original(values) => values.cost(),
      Self::Sorting(values) => values.cost(),
    }
  }
}

impl<B> BinaryDocValues for SortingCodecReaderBinaryDocValues<B>
where
  B: BinaryDocValues,
{
  type Value<'a>
    = crate::core::index::BytesRefValueEnum2<B::Value<'a>, BytesRefValueEnum<'a>>
  where
    Self: 'a;

  fn binary_value(&self) -> Result<Self::Value<'_>> {
    match self {
      Self::Original(values) => values
        .binary_value()
        .map(crate::core::index::BytesRefValueEnum2::A),
      Self::Sorting(values) => values
        .binary_value()
        .map(crate::core::index::BytesRefValueEnum2::B),
    }
  }
}

pub enum SortingCodecReaderSortedNumericDocValues<S> {
  Original(S),
  Sorting(SortingSortedNumericDocValues<S, Arc<LongValues>>),
}

impl<S> DocValuesIterator for SortingCodecReaderSortedNumericDocValues<S>
where
  S: SortedNumericDocValues,
{
  fn advance_exact(&mut self, target: i32) -> Result<bool> {
    match self {
      Self::Original(values) => values.advance_exact(target),
      Self::Sorting(values) => values.advance_exact(target),
    }
  }
}

impl<S> crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SortingCodecReaderSortedNumericDocValues<S>
where
  S: SortedNumericDocValues,
{
}
impl<S> crate::core::search::doc_id_set_iterator::BitSetIteratorAccess
  for SortingCodecReaderSortedNumericDocValues<S>
where
  S: SortedNumericDocValues,
{
}

impl<S> DocIdSetIterator for SortingCodecReaderSortedNumericDocValues<S>
where
  S: SortedNumericDocValues,
{
  fn doc_id(&self) -> i32 {
    match self {
      Self::Original(values) => values.doc_id(),
      Self::Sorting(values) => values.doc_id(),
    }
  }

  fn next_doc(&mut self) -> Result<i32> {
    match self {
      Self::Original(values) => values.next_doc(),
      Self::Sorting(values) => values.next_doc(),
    }
  }

  fn advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Original(values) => values.advance(target),
      Self::Sorting(values) => values.advance(target),
    }
  }

  fn slow_advance(&mut self, target: i32) -> Result<i32> {
    match self {
      Self::Original(values) => values.slow_advance(target),
      Self::Sorting(values) => values.slow_advance(target),
    }
  }

  fn cost(&self) -> Result<i64> {
    match self {
      Self::Original(values) => values.cost(),
      Self::Sorting(values) => values.cost(),
    }
  }
}

impl<S> SortedNumericDocValues for SortingCodecReaderSortedNumericDocValues<S>
where
  S: SortedNumericDocValues,
{
  fn next_value(&mut self) -> Result<i64> {
    match self {
      Self::Original(values) => values.next_value(),
      Self::Sorting(values) => values.next_value(),
    }
  }

  fn doc_value_count(&mut self) -> Result<i32> {
    match self {
      Self::Original(values) => values.doc_value_count(),
      Self::Sorting(values) => values.doc_value_count(),
    }
  }

  fn is_single_valued(&self) -> bool {
    match self {
      Self::Original(values) => values.is_single_valued(),
      Self::Sorting(values) => values.is_single_valued(),
    }
  }

  type NumericDocValues = <S as SortedNumericDocValues>::NumericDocValues;

  fn get_numeric_doc_values(&mut self) -> Result<Self::NumericDocValues> {
    match self {
      Self::Original(values) => values.get_numeric_doc_values(),
      Self::Sorting(_) => Err(LuceneError::unsupported_operation(
        "get_numeric_doc_values is unavailable for sorting values",
      )),
    }
  }
}

pub enum SortingCodecReaderDocValuesProducer<DVP, DM> {
  Original(DVP),
  Sorting(DocValuesProducerImpl<DVP, DM>),
}

impl<DVP, DM> CloseableRef for SortingCodecReaderDocValuesProducer<DVP, DM>
where
  DVP: DocValuesProducer,
{
  fn close(&self) -> Result<()> {
    match self {
      Self::Original(producer) => producer.close(),
      Self::Sorting(producer) => producer.close(),
    }
  }
}

impl<DVP, DM> DocValuesProducer for SortingCodecReaderDocValuesProducer<DVP, DM>
where
  DVP: DocValuesProducer,
  DM: DocMap + Clone,
{
  type NumericDocValues = SortingCodecReaderNumericDocValues<DVP::NumericDocValues>;

  fn get_numeric(&self, field: &Arc<FieldInfo>) -> Result<Self::NumericDocValues> {
    match self {
      Self::Original(producer) => producer
        .get_numeric(field)
        .map(SortingCodecReaderNumericDocValues::Original),
      Self::Sorting(producer) => producer
        .get_numeric(field)
        .map(SortingCodecReaderNumericDocValues::Sorting),
    }
  }

  type BinaryDocValues = SortingCodecReaderBinaryDocValues<DVP::BinaryDocValues>;

  fn get_binary(&self, field: &Arc<FieldInfo>) -> Result<Self::BinaryDocValues> {
    match self {
      Self::Original(producer) => producer
        .get_binary(field)
        .map(SortingCodecReaderBinaryDocValues::Original),
      Self::Sorting(producer) => producer
        .get_binary(field)
        .map(SortingCodecReaderBinaryDocValues::Sorting),
    }
  }

  type SortedDocValues = SortingCodecReaderSortedDocValues<DVP::SortedDocValues>;

  fn get_sorted(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedDocValues> {
    match self {
      Self::Original(producer) => producer
        .get_sorted(field)
        .map(SortingCodecReaderSortedDocValues::Original),
      Self::Sorting(producer) => producer
        .get_sorted(field)
        .map(SortingCodecReaderSortedDocValues::Sorting),
    }
  }

  type SortedNumericDocValues =
    SortingCodecReaderSortedNumericDocValues<DVP::SortedNumericDocValues>;

  fn get_sorted_numeric(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedNumericDocValues> {
    match self {
      Self::Original(producer) => producer
        .get_sorted_numeric(field)
        .map(SortingCodecReaderSortedNumericDocValues::Original),
      Self::Sorting(producer) => producer
        .get_sorted_numeric(field)
        .map(SortingCodecReaderSortedNumericDocValues::Sorting),
    }
  }

  type SortedSetDocValues = SortingCodecReaderSortedSetDocValues<DVP::SortedSetDocValues>;

  fn get_sorted_set(&self, field: &Arc<FieldInfo>) -> Result<Self::SortedSetDocValues> {
    match self {
      Self::Original(producer) => producer
        .get_sorted_set(field)
        .map(SortingCodecReaderSortedSetDocValues::Original),
      Self::Sorting(producer) => producer
        .get_sorted_set(field)
        .map(SortingCodecReaderSortedSetDocValues::Sorting),
    }
  }

  type DocValuesSkipper = DVP::DocValuesSkipper;

  fn get_skipper(&self, field: &Arc<FieldInfo>) -> Result<Option<Self::DocValuesSkipper>> {
    match self {
      Self::Original(producer) => producer.get_skipper(field),
      Self::Sorting(_) => Err(LuceneError::unsupported_operation(
        "get_skipper is unavailable for sorting values",
      )),
    }
  }

  fn check_integrity(&self) -> Result<()> {
    match self {
      Self::Original(producer) => producer.check_integrity(),
      Self::Sorting(producer) => producer.check_integrity(),
    }
  }

  fn get_merge_instance(&self) -> Result<Option<Self>> {
    match self {
      Self::Original(producer) => Ok(producer.get_merge_instance()?.map(Self::Original)),
      Self::Sorting(producer) => Ok(producer.get_merge_instance()?.map(Self::Sorting)),
    }
  }
}

fn get_or_create_dv<F>(
  field: &Arc<FieldInfo>,
  supplier: F,
  inner: &Arc<(IndexReaderBase, Mutex<Inner>)>,
) -> Result<CachedObject>
where
  F: FnOnce() -> Result<CachedObject>,
{
  get_or_create(field, false, supplier, inner)
}
fn get_or_create_norms<F>(
  field: &Arc<FieldInfo>,
  supplier: F,
  inner: &Arc<(IndexReaderBase, Mutex<Inner>)>,
) -> Result<CachedObject>
where
  F: FnOnce() -> Result<CachedObject>,
{
  get_or_create(field, true, supplier, inner)
}

fn get_or_create<F>(
  field: &Arc<FieldInfo>,
  norms: bool,
  supplier: F,
  state: &Arc<(IndexReaderBase, Mutex<Inner>)>,
) -> Result<CachedObject>
where
  F: FnOnce() -> Result<CachedObject>,
{
  let _reader_guard = state.0.state.close_lock.lock();
  let mut inner = state.1.lock();
  if inner
    .cached_field
    .as_ref()
    .map(|cached| cached.name.as_str())
    != Some(field.name.as_str())
    || inner.cache_is_norms != norms
  {
    debug_assert!(assert_created_only_once(&field.name, norms, &mut inner));
    drop(inner);
    let new_object = supplier()?;
    let mut inner = state.1.lock();
    inner.cached_field = Some(Arc::clone(field));
    inner.cache_is_norms = norms;
    inner.cached_object = Some(new_object.clone());
    return Ok(new_object);
  }
  inner
    .cached_object
    .as_ref()
    .cloned()
    .ok_or_else(|| LuceneError::illegal_state("cached object is missing"))
}
fn get_numeric_doc_values<N, DM>(
  old_numerics: &mut N,
  max_doc: usize,
  doc_map: &DM,
) -> Result<NumericDVs<FixedBitSet>>
where
  N: NumericDocValues,
  DM: DocMap,
{
  let mut docs_with_field = FixedBitSet::new(max_doc);
  let mut values = vec![0i64; max_doc];

  let mut doc_id = old_numerics.next_doc()?;
  loop {
    if doc_id == NO_MORE_DOCS {
      break;
    }
    let new_doc_id = doc_map.old_to_new(doc_id)? as usize;
    docs_with_field.set(new_doc_id)?;
    values[new_doc_id] = old_numerics.long_value()?;
    doc_id = old_numerics.next_doc()?;
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

    // For things that aren't sort fields, it's possible for sort to be None here
    // In the event that we accidentally cache twice, its better not to return an NPE
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

pub struct PointsReaderImpl<PR, DM> {
  delegate: PR,
  doc_map: DM,
}
impl<PR, DM> PointsReaderImpl<PR, DM> {
  fn new(delegate: PR, doc_map: DM) -> Self {
    Self { delegate, doc_map }
  }
}

impl<PR, DM> CloseableRef for PointsReaderImpl<PR, DM>
where
  PR: CloseableRef,
{
  fn close(&self) -> Result<()> {
    self.delegate.close()
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
    Ok(
      self
        .delegate
        .get_values(field)?
        .map(|values| SortingPointValues::new(values, self.doc_map.clone())),
    )
  }
}

pub struct KnnVectorsReaderImpl<KVR, DM> {
  delegate: KVR,
  doc_map: DM,
}
impl<KVR, DM> KnnVectorsReaderImpl<KVR, DM> {
  fn new(delegate: KVR, doc_map: DM) -> Self {
    Self { delegate, doc_map }
  }
}

impl<KVR, DM> CloseableRef for KnnVectorsReaderImpl<KVR, DM>
where
  KVR: CloseableRef,
{
  fn close(&self) -> Result<()> {
    self.delegate.close()
  }
}

impl<KVR, DM> HnswGraphProvider for KnnVectorsReaderImpl<KVR, DM>
where
  KVR: KnnVectorsReader,
  DM: DocMap,
{
  type HnswGraph = DummyHnswGraph;
}
impl<KVR, DM> KnnVectorsReader for KnnVectorsReaderImpl<KVR, DM>
where
  KVR: KnnVectorsReader,
  DM: DocMap,
{
  fn check_integrity(&self) -> Result<()> {
    self.delegate.check_integrity()
  }

  type FloatVectorValues = SortingFloatVectorValues<<KVR as KnnVectorsReader>::FloatVectorValues>;

  fn get_float_vector_values(&self, field: &str) -> Result<Self::FloatVectorValues> {
    SortingFloatVectorValues::new(self.delegate.get_float_vector_values(field)?, &self.doc_map)
  }

  type ByteVectorValues = SortingByteVectorValues<<KVR as KnnVectorsReader>::ByteVectorValues>;

  fn get_byte_vector_values(&self, field: &str) -> Result<Self::ByteVectorValues> {
    SortingByteVectorValues::new(self.delegate.get_byte_vector_values(field)?, &self.doc_map)
  }

  type QuantizedByteVectorValues = DummyByteVectorValues;

  fn search_f32<B, K, TV>(
    &self,
    _field: &str,
    _target: TV,
    _knn_collector: &mut K,
    _accept_docs: Option<B>,
  ) -> Result<()>
  where
    TV: Into<Vec<f32>>,
    B: Bits,
    K: KnnCollector,
  {
    Err(LuceneError::unsupported_operation(""))
  }

  fn search_u8<B, K, TV>(
    &self,
    _field: &str,
    _target: TV,
    _knn_collector: &mut K,
    _accept_docs: Option<B>,
  ) -> Result<()>
  where
    TV: Into<Vec<u8>>,
    B: Bits,
    K: KnnCollector,
  {
    Err(LuceneError::unsupported_operation(""))
  }
}

pub enum SortingCodecReaderFloatVectorValues<T, U> {
  Filter(T),
  Sorting(U),
}

impl<T, U> KnnVectorValues for SortingCodecReaderFloatVectorValues<T, U>
where
  T: FloatVectorValues,
  U: FloatVectorValues,
{
  fn dimension(&self) -> usize {
    match self {
      Self::Filter(values) => values.dimension(),
      Self::Sorting(values) => values.dimension(),
    }
  }

  fn size(&self) -> usize {
    match self {
      Self::Filter(values) => values.size(),
      Self::Sorting(values) => values.size(),
    }
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    match self {
      Self::Filter(values) => values.ord_to_doc(ord),
      Self::Sorting(values) => values.ord_to_doc(ord),
    }
  }

  type KnnVectorValues = T::KnnVectorValues;

  fn copy(&self) -> Result<Self::KnnVectorValues> {
    match self {
      Self::Filter(values) => values.copy(),
      Self::Sorting(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn get_vector_byte_length(&self) -> usize {
    match self {
      Self::Filter(values) => values.get_vector_byte_length(),
      Self::Sorting(values) => values.get_vector_byte_length(),
    }
  }

  fn get_encoding(&self) -> VectorEncoding {
    match self {
      Self::Filter(values) => KnnVectorValues::get_encoding(values),
      Self::Sorting(values) => KnnVectorValues::get_encoding(values),
    }
  }

  type Bits<'a, B>
    = BitsEnum2<T::Bits<'a, B>, U::Bits<'a, B>>
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, accept_docs: Option<B>) -> Option<Self::Bits<'a, B>>
  where
    B: Bits,
  {
    match self {
      Self::Filter(values) => values.get_accept_ords(accept_docs).map(BitsEnum2::A),
      Self::Sorting(values) => values.get_accept_ords(accept_docs).map(BitsEnum2::B),
    }
  }

  type DocIndexIterator = DocIndexIteratorEnum2<T::DocIndexIterator, U::DocIndexIterator>;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    match self {
      Self::Filter(values) => values.iterator().map(DocIndexIteratorEnum2::A),
      Self::Sorting(values) => values.iterator().map(DocIndexIteratorEnum2::B),
    }
  }
}

impl<T, U> FloatVectorValues for SortingCodecReaderFloatVectorValues<T, U>
where
  T: FloatVectorValues,
  U: FloatVectorValues,
{
  type VectorValue<'v>
    = FloatVectorValue<'v>
  where
    Self: 'v;

  fn vector_value(&self, ord: usize) -> Result<Self::VectorValue<'_>> {
    match self {
      Self::Filter(values) => values.vector_value(ord).map(Into::into),
      Self::Sorting(values) => values.vector_value(ord).map(Into::into),
    }
  }

  type FloatVectorValues = T::FloatVectorValues;

  fn float_copy(&self) -> Result<Option<Self::FloatVectorValues>> {
    match self {
      Self::Filter(values) => values.float_copy(),
      Self::Sorting(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  type VectorScorer = T::VectorScorer;

  fn scorer<'a, TV>(&self, target: TV) -> Result<Option<Self::VectorScorer>>
  where
    TV: Into<std::borrow::Cow<'a, [f32]>>,
  {
    let target: std::borrow::Cow<'a, [f32]> = target.into();
    match self {
      Self::Filter(values) => values.scorer(target),
      Self::Sorting(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn get_encoding(&self) -> VectorEncoding {
    match self {
      Self::Filter(values) => FloatVectorValues::get_encoding(values),
      Self::Sorting(values) => FloatVectorValues::get_encoding(values),
    }
  }

  fn get_vectors_mut(&mut self) -> Result<&mut Vec<Box<[f32]>>> {
    match self {
      Self::Filter(values) => values.get_vectors_mut(),
      Self::Sorting(values) => values.get_vectors_mut(),
    }
  }

  fn get_vectors(&self) -> Result<&[Box<[f32]>]> {
    match self {
      Self::Filter(values) => values.get_vectors(),
      Self::Sorting(values) => values.get_vectors(),
    }
  }

  fn get_vectors_capacity(&self) -> Result<usize> {
    match self {
      Self::Filter(values) => values.get_vectors_capacity(),
      Self::Sorting(values) => values.get_vectors_capacity(),
    }
  }
}

pub enum SortingCodecReaderByteVectorValues<T, U> {
  Filter(T),
  Sorting(U),
}

impl<T, U> KnnVectorValues for SortingCodecReaderByteVectorValues<T, U>
where
  T: ByteVectorValues,
  U: ByteVectorValues,
{
  fn dimension(&self) -> usize {
    match self {
      Self::Filter(values) => values.dimension(),
      Self::Sorting(values) => values.dimension(),
    }
  }

  fn size(&self) -> usize {
    match self {
      Self::Filter(values) => values.size(),
      Self::Sorting(values) => values.size(),
    }
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    match self {
      Self::Filter(values) => values.ord_to_doc(ord),
      Self::Sorting(values) => values.ord_to_doc(ord),
    }
  }

  type KnnVectorValues = T::KnnVectorValues;

  fn copy(&self) -> Result<Self::KnnVectorValues> {
    match self {
      Self::Filter(values) => values.copy(),
      Self::Sorting(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn get_vector_byte_length(&self) -> usize {
    match self {
      Self::Filter(values) => values.get_vector_byte_length(),
      Self::Sorting(values) => values.get_vector_byte_length(),
    }
  }

  fn get_encoding(&self) -> VectorEncoding {
    match self {
      Self::Filter(values) => KnnVectorValues::get_encoding(values),
      Self::Sorting(values) => KnnVectorValues::get_encoding(values),
    }
  }

  type Bits<'a, B>
    = BitsEnum2<T::Bits<'a, B>, U::Bits<'a, B>>
  where
    B: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B>(&'a self, accept_docs: Option<B>) -> Option<Self::Bits<'a, B>>
  where
    B: Bits,
  {
    match self {
      Self::Filter(values) => values.get_accept_ords(accept_docs).map(BitsEnum2::A),
      Self::Sorting(values) => values.get_accept_ords(accept_docs).map(BitsEnum2::B),
    }
  }

  type DocIndexIterator = DocIndexIteratorEnum2<T::DocIndexIterator, U::DocIndexIterator>;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    match self {
      Self::Filter(values) => values.iterator().map(DocIndexIteratorEnum2::A),
      Self::Sorting(values) => values.iterator().map(DocIndexIteratorEnum2::B),
    }
  }
}

impl<T, U> ByteVectorValues for SortingCodecReaderByteVectorValues<T, U>
where
  T: ByteVectorValues,
  U: ByteVectorValues,
{
  type VectorValue<'v>
    = ByteVectorValue<'v>
  where
    Self: 'v;

  fn vector_value(&self, ord: usize) -> Result<Self::VectorValue<'_>> {
    match self {
      Self::Filter(values) => values.vector_value(ord).map(Into::into),
      Self::Sorting(values) => values.vector_value(ord).map(Into::into),
    }
  }

  type ByteVectorValues = T::ByteVectorValues;

  fn byte_copy(&self) -> Result<Option<Self::ByteVectorValues>> {
    match self {
      Self::Filter(values) => values.byte_copy(),
      Self::Sorting(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  type VectorScorer = T::VectorScorer;

  fn scorer<TV>(&self, target: TV) -> Result<Option<Self::VectorScorer>>
  where
    TV: Into<Vec<u8>>,
  {
    let target = target.into();
    match self {
      Self::Filter(values) => values.scorer(target),
      Self::Sorting(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn get_encoding(&self) -> VectorEncoding {
    match self {
      Self::Filter(values) => ByteVectorValues::get_encoding(values),
      Self::Sorting(values) => ByteVectorValues::get_encoding(values),
    }
  }

  fn get_vectors_mut(&mut self) -> Result<&mut Vec<Box<[u8]>>> {
    match self {
      Self::Filter(values) => values.get_vectors_mut(),
      Self::Sorting(values) => values.get_vectors_mut(),
    }
  }

  fn get_vectors(&self) -> Result<&[Box<[u8]>]> {
    match self {
      Self::Filter(values) => values.get_vectors(),
      Self::Sorting(values) => values.get_vectors(),
    }
  }

  fn get_vectors_capacity(&self) -> Result<usize> {
    match self {
      Self::Filter(values) => values.get_vectors_capacity(),
      Self::Sorting(values) => values.get_vectors_capacity(),
    }
  }
}

pub enum SortingCodecReaderPointValues<T, U> {
  Filter(T),
  Sorting(U),
}

impl<T, U> PointValues for SortingCodecReaderPointValues<T, U>
where
  T: PointValues,
  U: PointValues,
{
  fn get_min_packed_value(&self) -> Result<Option<Cow<'_, [u8]>>> {
    match self {
      Self::Filter(values) => values.get_min_packed_value(),
      Self::Sorting(values) => values.get_min_packed_value(),
    }
  }

  fn get_max_packed_value(&self) -> Result<Option<Cow<'_, [u8]>>> {
    match self {
      Self::Filter(values) => values.get_max_packed_value(),
      Self::Sorting(values) => values.get_max_packed_value(),
    }
  }

  fn get_num_dimensions(&self) -> Result<usize> {
    match self {
      Self::Filter(values) => values.get_num_dimensions(),
      Self::Sorting(values) => values.get_num_dimensions(),
    }
  }

  fn get_num_index_dimensions(&self) -> Result<usize> {
    match self {
      Self::Filter(values) => values.get_num_index_dimensions(),
      Self::Sorting(values) => values.get_num_index_dimensions(),
    }
  }

  fn get_bytes_per_dimension(&self) -> Result<usize> {
    match self {
      Self::Filter(values) => values.get_bytes_per_dimension(),
      Self::Sorting(values) => values.get_bytes_per_dimension(),
    }
  }

  fn size(&self) -> Result<usize> {
    match self {
      Self::Filter(values) => values.size(),
      Self::Sorting(values) => values.size(),
    }
  }

  fn get_doc_count(&self) -> Result<i32> {
    match self {
      Self::Filter(values) => values.get_doc_count(),
      Self::Sorting(values) => values.get_doc_count(),
    }
  }

  type PointTree = PointTreeEnum<
    <T::PointTree as PointTree>::MutablePointTree,
    PointTreeEnum2<T::PointTree, U::PointTree>,
  >;

  fn get_point_tree(&self) -> Result<Self::PointTree> {
    match self {
      Self::Filter(values) => match values.get_point_tree()?.into_mutable() {
        PointTreeEnum::Mutable(tree) => Ok(PointTreeEnum::Mutable(tree)),
        PointTreeEnum::Other(tree) => Ok(PointTreeEnum::Other(PointTreeEnum2::A(tree))),
      },
      Self::Sorting(values) => match values.get_point_tree()?.into_mutable() {
        PointTreeEnum::Mutable(_) => Err(LuceneError::unsupported_operation("")),
        PointTreeEnum::Other(tree) => Ok(PointTreeEnum::Other(PointTreeEnum2::B(tree))),
      },
    }
  }
}

pub enum SortingCodecReaderPointsReader<T, U> {
  Filter(T),
  Sorting(U),
}

impl<T, U> SortingCodecReaderPointsReader<T, U> {
  #[allow(non_snake_case)]
  pub(crate) fn A(reader: T) -> Self {
    Self::Filter(reader)
  }

  #[allow(non_snake_case)]
  pub(crate) fn B(reader: U) -> Self {
    Self::Sorting(reader)
  }
}

impl<T, U> CloseableRef for SortingCodecReaderPointsReader<T, U>
where
  T: CloseableRef,
  U: CloseableRef,
{
  fn close(&self) -> Result<()> {
    match self {
      Self::Filter(reader) => reader.close(),
      Self::Sorting(reader) => reader.close(),
    }
  }
}

impl<T, U> PointsReader for SortingCodecReaderPointsReader<T, U>
where
  T: PointsReader,
  U: PointsReader,
{
  fn check_integrity(&self) -> Result<()> {
    match self {
      Self::Filter(reader) => reader.check_integrity(),
      Self::Sorting(reader) => reader.check_integrity(),
    }
  }

  type PointValuesType = SortingCodecReaderPointValues<T::PointValuesType, U::PointValuesType>;

  fn get_values(&self, field: &str) -> Result<Option<Self::PointValuesType>> {
    match self {
      Self::Filter(reader) => reader
        .get_values(field)
        .map(|values| values.map(SortingCodecReaderPointValues::Filter)),
      Self::Sorting(reader) => reader
        .get_values(field)
        .map(|values| values.map(SortingCodecReaderPointValues::Sorting)),
    }
  }

  fn get_merge_instance(&self) -> Result<Option<Self>>
  where
    Self: Sized,
  {
    match self {
      Self::Filter(reader) => match reader.get_merge_instance()? {
        Some(values) => Ok(Some(Self::Filter(values))),
        None => Ok(None),
      },
      Self::Sorting(reader) => match reader.get_merge_instance()? {
        Some(values) => Ok(Some(Self::Sorting(values))),
        None => Ok(None),
      },
    }
  }
}

pub enum SortingCodecReaderKnnVectorsReader<T, U> {
  Filter(T),
  Sorting(U),
}

impl<T, U> SortingCodecReaderKnnVectorsReader<T, U> {
  #[allow(non_snake_case)]
  pub(crate) fn A(reader: T) -> Self {
    Self::Filter(reader)
  }

  #[allow(non_snake_case)]
  pub(crate) fn B(reader: U) -> Self {
    Self::Sorting(reader)
  }
}

impl<T, U> CloseableRef for SortingCodecReaderKnnVectorsReader<T, U>
where
  T: CloseableRef,
  U: CloseableRef,
{
  fn close(&self) -> Result<()> {
    match self {
      Self::Filter(reader) => reader.close(),
      Self::Sorting(reader) => reader.close(),
    }
  }
}

impl<T, U> HnswGraphProvider for SortingCodecReaderKnnVectorsReader<T, U>
where
  T: HnswGraphProvider,
  U: HnswGraphProvider,
{
  type HnswGraph = T::HnswGraph;

  fn is_hnsw_graph_provider(&self, field: &str) -> bool {
    match self {
      Self::Filter(reader) => reader.is_hnsw_graph_provider(field),
      Self::Sorting(reader) => reader.is_hnsw_graph_provider(field),
    }
  }

  fn get_graph(&self, field: &str) -> Result<Self::HnswGraph> {
    match self {
      Self::Filter(reader) => reader.get_graph(field),
      Self::Sorting(_) => Err(LuceneError::unsupported_operation("")),
    }
  }
}

impl<T, U> KnnVectorsReader for SortingCodecReaderKnnVectorsReader<T, U>
where
  T: KnnVectorsReader,
  U: KnnVectorsReader,
{
  fn check_integrity(&self) -> Result<()> {
    match self {
      Self::Filter(reader) => reader.check_integrity(),
      Self::Sorting(reader) => reader.check_integrity(),
    }
  }

  type FloatVectorValues =
    SortingCodecReaderFloatVectorValues<T::FloatVectorValues, U::FloatVectorValues>;

  fn get_float_vector_values(&self, field: &str) -> Result<Self::FloatVectorValues> {
    match self {
      Self::Filter(reader) => reader
        .get_float_vector_values(field)
        .map(SortingCodecReaderFloatVectorValues::Filter),
      Self::Sorting(reader) => reader
        .get_float_vector_values(field)
        .map(SortingCodecReaderFloatVectorValues::Sorting),
    }
  }

  type ByteVectorValues =
    SortingCodecReaderByteVectorValues<T::ByteVectorValues, U::ByteVectorValues>;

  fn get_byte_vector_values(&self, field: &str) -> Result<Self::ByteVectorValues> {
    match self {
      Self::Filter(reader) => reader
        .get_byte_vector_values(field)
        .map(SortingCodecReaderByteVectorValues::Filter),
      Self::Sorting(reader) => reader
        .get_byte_vector_values(field)
        .map(SortingCodecReaderByteVectorValues::Sorting),
    }
  }

  type QuantizedByteVectorValues = T::QuantizedByteVectorValues;

  fn get_quantized_vector_values(
    &self,
    field: &str,
  ) -> Result<Option<Self::QuantizedByteVectorValues>> {
    match self {
      Self::Filter(reader) => reader.get_quantized_vector_values(field),
      Self::Sorting(_) => Ok(None),
    }
  }

  fn get_quantization_state(
    &self,
    field: &str,
  ) -> Result<Option<crate::core::util::quantization::scalar_quantizer::ScalarQuantizer>> {
    match self {
      Self::Filter(reader) => reader.get_quantization_state(field),
      Self::Sorting(reader) => reader.get_quantization_state(field),
    }
  }

  fn is_flat_vectors_reader(&self, field: &str) -> bool {
    match self {
      Self::Filter(reader) => reader.is_flat_vectors_reader(field),
      Self::Sorting(reader) => reader.is_flat_vectors_reader(field),
    }
  }

  fn search_f32<B, K, TV>(
    &self,
    field: &str,
    target: TV,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    TV: Into<Vec<f32>>,
    B: Bits,
    K: KnnCollector,
  {
    let target = target.into();
    match self {
      Self::Filter(reader) => reader.search_f32(field, target, knn_collector, accept_docs),
      Self::Sorting(reader) => reader.search_f32(field, target, knn_collector, accept_docs),
    }
  }

  fn search_u8<B, K, TV>(
    &self,
    field: &str,
    target: TV,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    TV: Into<Vec<u8>>,
    B: Bits,
    K: KnnCollector,
  {
    let target = target.into();
    match self {
      Self::Filter(reader) => reader.search_u8(field, target, knn_collector, accept_docs),
      Self::Sorting(reader) => reader.search_u8(field, target, knn_collector, accept_docs),
    }
  }

  fn get_merge_instance(&self) -> Result<Option<Self>>
  where
    Self: Sized,
  {
    match self {
      Self::Filter(reader) => match reader.get_merge_instance()? {
        Some(values) => Ok(Some(Self::Filter(values))),
        None => Ok(None),
      },
      Self::Sorting(reader) => match reader.get_merge_instance()? {
        Some(values) => Ok(Some(Self::Sorting(values))),
        None => Ok(None),
      },
    }
  }

  fn finish_merge(&self) -> Result<()> {
    match self {
      Self::Filter(reader) => reader.finish_merge(),
      Self::Sorting(reader) => reader.finish_merge(),
    }
  }
}

pub struct SortingByteVectorValues<B> {
  delegate: B,
  iterator_supplier: SortingIteratorSupplier,
}
impl<B> SortingByteVectorValues<B>
where
  B: ByteVectorValues,
{
  fn new<DM>(mut delegate: B, doc_map: &DM) -> Result<Self>
  where
    DM: DocMap,
  {
    let iterator_supplier = iterator_supplier(&mut delegate, doc_map)?;
    // SortingValuesIterator consumes the iterator and records the docs and ord mapping
    Ok(Self {
      delegate,
      iterator_supplier,
    })
  }
}

impl<B> KnnVectorValues for SortingByteVectorValues<B>
where
  B: ByteVectorValues,
{
  fn dimension(&self) -> usize {
    self.delegate.dimension()
  }

  fn size(&self) -> usize {
    self.iterator_supplier.size()
  }

  type KnnVectorValues = DummyKnnVectorsWriter;

  fn get_encoding(&self) -> VectorEncoding {
    ByteVectorValues::get_encoding(self)
  }

  type Bits<'a, B1>
    = BitsImpl<B1, &'a Self>
  where
    B1: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B1>(&'a self, accept_docs: Option<B1>) -> Option<Self::Bits<'a, B1>>
  where
    B1: Bits,
  {
    self.default_get_accept_ords(accept_docs)
  }

  type DocIndexIterator = SortingValuesIterator;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    self.iterator_supplier.get()
  }
}

impl<B> ByteVectorValues for SortingByteVectorValues<B>
where
  B: ByteVectorValues,
{
  type VectorValue<'v>
    = B::VectorValue<'v>
  where
    Self: 'v;

  fn vector_value(&self, ord: usize) -> Result<Self::VectorValue<'_>> {
    self.delegate.vector_value(ord)
  }

  type ByteVectorValues = DummyByteVectorValues;

  fn byte_copy(&self) -> Result<Option<Self::ByteVectorValues>> {
    Err(LuceneError::unsupported_operation(""))
  }

  type VectorScorer = DummyVectorScorer;
}
/// Sorting FloatVectorValues that maps ordinals using the provided sortMap
pub struct SortingFloatVectorValues<B> {
  delegate: B,
  iterator_supplier: SortingIteratorSupplier,
}
impl<B> SortingFloatVectorValues<B>
where
  B: FloatVectorValues,
{
  fn new<DM>(mut delegate: B, doc_map: &DM) -> Result<Self>
  where
    DM: DocMap,
  {
    let iterator_supplier = iterator_supplier(&mut delegate, doc_map)?;
    // SortingValuesIterator consumes the iterator and records the docs and ord mapping
    Ok(Self {
      delegate,
      iterator_supplier,
    })
  }
}

impl<B> KnnVectorValues for SortingFloatVectorValues<B>
where
  B: FloatVectorValues,
{
  fn dimension(&self) -> usize {
    self.delegate.dimension()
  }

  fn size(&self) -> usize {
    self.iterator_supplier.size()
  }

  type KnnVectorValues = DummyKnnVectorsWriter;

  fn get_encoding(&self) -> VectorEncoding {
    FloatVectorValues::get_encoding(self)
  }

  type Bits<'a, B1>
    = BitsImpl<B1, &'a Self>
  where
    B1: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B1>(&'a self, accept_docs: Option<B1>) -> Option<Self::Bits<'a, B1>>
  where
    B1: Bits,
  {
    self.default_get_accept_ords(accept_docs)
  }

  type DocIndexIterator = SortingValuesIterator;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    self.iterator_supplier.get()
  }
}

impl<B> FloatVectorValues for SortingFloatVectorValues<B>
where
  B: FloatVectorValues,
{
  type VectorValue<'v>
    = B::VectorValue<'v>
  where
    Self: 'v;

  fn vector_value(&self, ord: usize) -> Result<Self::VectorValue<'_>> {
    self.delegate.vector_value(ord)
  }

  type FloatVectorValues = DummyFloatVectorValues;

  fn float_copy(&self) -> Result<Option<Self::FloatVectorValues>> {
    Err(LuceneError::unsupported_operation(""))
  }

  type VectorScorer = DummyVectorScorer;
}

pub struct SortingPointValues<PV, DM> {
  in_: PV,
  doc_map: DM,
}
impl<PV, DM> SortingPointValues<PV, DM> {
  pub fn new(delegate: PV, doc_map: DM) -> Self {
    Self {
      in_: delegate,
      doc_map,
    }
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

  type PointTree = SortingPointTree<PV::PointTree, DM>;

  fn get_point_tree(&self) -> Result<Self::PointTree> {
    let tree = self.in_.get_point_tree()?.into_mutable();
    let PointTreeEnum::Other(tree) = tree else {
      return Err(LuceneError::unsupported_operation(""));
    };
    Ok(SortingPointTree::new(tree, self.doc_map.clone()))
  }
}

pub struct SortingPointTree<PT, DM> {
  index_tree: PT,
  doc_map: DM,
}
impl<PT, DM> SortingPointTree<PT, DM> {
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
  type MutablePointTree = DummyMutablePointTree;

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
    let mut visitor = SortingIntersectVisitor::new(&self.doc_map, visitor);
    self.index_tree.visit_doc_values(&mut visitor)
  }

  fn visit_doc_values<IV>(&mut self, visitor: &mut IV) -> Result<()>
  where
    IV: IntersectVisitor,
  {
    let mut visitor = SortingIntersectVisitor::new(&self.doc_map, visitor);
    self.index_tree.visit_doc_values(&mut visitor)
  }
}

pub struct SortingIntersectVisitor<'a, DM, IV> {
  doc_map: &'a DM,
  visitor: &'a mut IV,
}
impl<'a, DM, IV> SortingIntersectVisitor<'a, DM, IV> {
  fn new(doc_map: &'a DM, visitor: &'a mut IV) -> Self {
    Self { doc_map, visitor }
  }
}
impl<DM, IV> IntersectVisitor for SortingIntersectVisitor<'_, DM, IV>
where
  DM: DocMap,
  IV: IntersectVisitor,
{
  fn visit(&mut self, doc_id: i32) -> Result<()> {
    self.visitor.visit(self.doc_map.old_to_new(doc_id)?)
  }

  fn visit_with_packed_value(&mut self, doc_id: i32, packed_value: &[u8]) -> Result<()> {
    self
      .visitor
      .visit_with_packed_value(self.doc_map.old_to_new(doc_id)?, packed_value)
  }

  fn compare(&self, min_packed_value: &[u8], max_packed_value: &[u8]) -> Result<Relation> {
    self.visitor.compare(min_packed_value, max_packed_value)
  }
}
pub struct SortingBitsImpl<B, DM> {
  in_: B,
  doc_map: DM,
  id: Identity,
}
impl<B, DM> SortingBitsImpl<B, DM> {
  fn new(in_: B, doc_map: DM) -> Self {
    Self {
      in_,
      doc_map,
      id: Identity::new(),
    }
  }
}

impl<B, DM> HasIdentity for SortingBitsImpl<B, DM> {
  fn identity(&self) -> &Identity {
    &self.id
  }
}

impl<B, DM> Bits for SortingBitsImpl<B, DM>
where
  B: Bits,
  DM: DocMap + Clone,
{
  fn get(&self, index: usize) -> Result<bool> {
    self
      .in_
      .get(self.doc_map.new_to_old(index as i32)? as usize)
  }

  fn length(&self) -> usize {
    self.in_.length()
  }
}

pub enum SortingCodecReaderBits<B, DM> {
  Filter(B),
  Sorting(SortingBitsImpl<B, DM>),
}

impl<B, DM> HasIdentity for SortingCodecReaderBits<B, DM>
where
  B: HasIdentity,
{
  fn identity(&self) -> &Identity {
    match self {
      Self::Filter(bits) => bits.identity(),
      Self::Sorting(bits) => bits.identity(),
    }
  }
}

impl<B, DM> Bits for SortingCodecReaderBits<B, DM>
where
  B: Bits,
  DM: DocMap + Clone,
{
  fn get(&self, index: usize) -> Result<bool> {
    match self {
      Self::Filter(bits) => bits.get(index),
      Self::Sorting(bits) => bits.get(index),
    }
  }

  fn length(&self) -> usize {
    match self {
      Self::Filter(bits) => bits.length(),
      Self::Sorting(bits) => bits.length(),
    }
  }

  fn copy_of(&self) -> Result<FixedBitSet> {
    match self {
      Self::Filter(bits) => bits.copy_of(),
      Self::Sorting(bits) => bits.copy_of(),
    }
  }

  fn to_string(&self) -> String {
    match self {
      Self::Filter(bits) => bits.to_string(),
      Self::Sorting(bits) => bits.to_string(),
    }
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

pub struct StoredFieldsReaderImpl<SFR, DM> {
  delegate: SFR,
  doc_map: DM,
}
impl<SFR, DM> StoredFieldsReaderImpl<SFR, DM> {
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

  fn document_with_visitor<S, V>(
    &mut self,
    doc_id: i32,
    visitor: &mut V,
    writer: Option<&mut S>,
  ) -> Result<()>
  where
    S: StoredFieldsWriter,
    V: StoredFieldVisitor,
  {
    self
      .delegate
      .document_with_visitor(self.doc_map.new_to_old(doc_id)?, visitor, writer)
  }
}

impl<SFR, DM> TryClone for StoredFieldsReaderImpl<SFR, DM>
where
  DM: Clone + DocMap,
  SFR: StoredFieldsReader,
{
  fn try_clone(&self) -> Result<Self>
  where
    Self: Sized,
  {
    Ok(new_stored_fields_reader(
      self.delegate.try_clone()?,
      self.doc_map.clone(),
    ))
  }
}

impl<SFR, DM> CloseableRef for StoredFieldsReaderImpl<SFR, DM>
where
  SFR: CloseableRef,
{
  fn close(&self) -> Result<()> {
    self.delegate.close()
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

  fn get_merge_instance(&self) -> Result<Option<Self>> {
    match self.delegate.get_merge_instance()? {
      Some(delegate) => Ok(Some(new_stored_fields_reader(
        delegate,
        self.doc_map.clone(),
      ))),
      None => Ok(None),
    }
  }
}

impl<SFR, DM> RawStoredFieldsReader for StoredFieldsReaderImpl<SFR, DM>
where
  SFR: RawStoredFieldsReader + StoredFieldsReader,
  DM: DocMap + Clone,
{
  type IndexInput = SFR::IndexInput;
}

pub struct FieldsProducerImpl<FP, DM> {
  postings_reader: FP,
  doc_map: DM,
  field_infos: Arc<FieldInfos>,
}

impl<FP, DM> FieldsProducerImpl<FP, DM> {
  fn new(postings_reader: FP, doc_map: DM, field_infos: Arc<FieldInfos>) -> Self {
    Self {
      postings_reader,
      doc_map,
      field_infos,
    }
  }
}

impl<FP, DM> CloseableRef for FieldsProducerImpl<FP, DM>
where
  FP: CloseableRef,
{
  fn close(&self) -> Result<()> {
    self.postings_reader.close()
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
        let field_info = self
          .field_infos
          .field_info_by_name(field)?
          .ok_or_else(|| LuceneError::illegal_state(format!("{}'s field info", field)))?;
        Ok(Some(SortingTerms::new(
          terms,
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

pub enum SortingCodecReaderTerms<T, DM> {
  Original(T),
  Sorting(SortingTerms<T, DM>),
}

impl<T, DM> Terms for SortingCodecReaderTerms<T, DM>
where
  T: Terms,
  DM: DocMap + Clone,
{
  type TermsEnum = SortingCodecReaderTermsEnum<T::TermsEnum, DM>;

  fn iterator(&self) -> Result<Self::TermsEnum> {
    match self {
      Self::Original(terms) => terms.iterator().map(SortingCodecReaderTermsEnum::Original),
      Self::Sorting(terms) => terms.iterator().map(SortingCodecReaderTermsEnum::Sorting),
    }
  }

  type IntersectIter = SortingCodecReaderTermsEnum<T::IntersectIter, DM>;

  fn intersect(
    &self,
    compiled: &CompiledAutomaton,
    start_term: Option<&BytesRef<Vec<u8>>>,
  ) -> Result<Self::IntersectIter> {
    match self {
      Self::Original(terms) => terms
        .intersect(compiled, start_term)
        .map(SortingCodecReaderTermsEnum::Original),
      Self::Sorting(terms) => terms
        .intersect(compiled, start_term)
        .map(SortingCodecReaderTermsEnum::Sorting),
    }
  }

  fn size(&self) -> Result<i64> {
    match self {
      Self::Original(terms) => terms.size(),
      Self::Sorting(terms) => terms.size(),
    }
  }

  fn get_sum_total_term_freq(&self) -> Result<i64> {
    match self {
      Self::Original(terms) => terms.get_sum_total_term_freq(),
      Self::Sorting(terms) => terms.get_sum_total_term_freq(),
    }
  }

  fn get_sum_doc_freq(&self) -> Result<i64> {
    match self {
      Self::Original(terms) => terms.get_sum_doc_freq(),
      Self::Sorting(terms) => terms.get_sum_doc_freq(),
    }
  }

  fn get_doc_count(&self) -> Result<i32> {
    match self {
      Self::Original(terms) => terms.get_doc_count(),
      Self::Sorting(terms) => terms.get_doc_count(),
    }
  }

  fn has_freqs(&self) -> bool {
    match self {
      Self::Original(terms) => terms.has_freqs(),
      Self::Sorting(terms) => terms.has_freqs(),
    }
  }

  fn has_offsets(&self) -> bool {
    match self {
      Self::Original(terms) => terms.has_offsets(),
      Self::Sorting(terms) => terms.has_offsets(),
    }
  }

  fn has_positions(&self) -> bool {
    match self {
      Self::Original(terms) => terms.has_positions(),
      Self::Sorting(terms) => terms.has_positions(),
    }
  }

  fn has_payloads(&self) -> bool {
    match self {
      Self::Original(terms) => terms.has_payloads(),
      Self::Sorting(terms) => terms.has_payloads(),
    }
  }

  fn get_min(&self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
    match self {
      Self::Original(terms) => terms.get_min(),
      Self::Sorting(terms) => terms.get_min(),
    }
  }

  fn get_max(&self) -> Result<Option<Cow<'_, BytesRef<Vec<u8>>>>> {
    match self {
      Self::Original(terms) => terms.get_max(),
      Self::Sorting(terms) => terms.get_max(),
    }
  }

  fn get_stats(&self) -> Result<String> {
    match self {
      Self::Original(terms) => terms.get_stats(),
      Self::Sorting(terms) => terms.get_stats(),
    }
  }
}

pub enum SortingCodecReaderTermsEnum<T, DM> {
  Original(T),
  Sorting(SortingTermsEnum<T, DM>),
}

impl<T, DM> BytesRefIterator for SortingCodecReaderTermsEnum<T, DM>
where
  T: TermsEnum,
  DM: DocMap,
{
  type Value<'a>
    = BytesRefValueEnum<'a>
  where
    Self: 'a;

  fn next(&mut self) -> Result<Option<Self::Value<'_>>> {
    match self {
      Self::Original(terms) => terms
        .next()
        .map(|value| value.map(BytesRefValue::into_value)),
      Self::Sorting(terms) => terms
        .next()
        .map(|value| value.map(BytesRefValue::into_value)),
    }
  }

  fn set_next(&mut self) -> Result<bool> {
    match self {
      Self::Original(terms) => terms.set_next(),
      Self::Sorting(terms) => terms.set_next(),
    }
  }
}

impl<T, DM> TermsEnum for SortingCodecReaderTermsEnum<T, DM>
where
  T: TermsEnum,
  DM: DocMap,
{
  type AttributeSource<'a>
    = T::AttributeSource<'a>
  where
    Self: 'a;
  type AttributeSourceMut<'a>
    = T::AttributeSourceMut<'a>
  where
    Self: 'a;

  fn attributes(&self) -> Result<Self::AttributeSource<'_>> {
    match self {
      Self::Original(terms) => terms.attributes(),
      Self::Sorting(terms) => terms.attributes(),
    }
  }

  fn attributes_mut(&mut self) -> Result<Self::AttributeSourceMut<'_>> {
    match self {
      Self::Original(terms) => terms.attributes_mut(),
      Self::Sorting(terms) => terms.attributes_mut(),
    }
  }

  fn seek_exact<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<bool> {
    match self {
      Self::Original(terms) => terms.seek_exact(term),
      Self::Sorting(terms) => terms.seek_exact(term),
    }
  }

  fn prepare_seek_exact<BS: ByteSource>(&mut self, text: &BytesRef<BS>) -> Result<Option<()>> {
    match self {
      Self::Original(terms) => terms.prepare_seek_exact(text),
      Self::Sorting(terms) => terms.prepare_seek_exact(text),
    }
  }

  fn get_prepare_seek_exact_status<BS: ByteSource>(
    &mut self,
    target: &BytesRef<BS>,
  ) -> Result<bool> {
    match self {
      Self::Original(terms) => terms.get_prepare_seek_exact_status(target),
      Self::Sorting(terms) => terms.get_prepare_seek_exact_status(target),
    }
  }

  fn seek_ceil<BS: ByteSource>(&mut self, term: &BytesRef<BS>) -> Result<SeekStatus> {
    match self {
      Self::Original(terms) => terms.seek_ceil(term),
      Self::Sorting(terms) => terms.seek_ceil(term),
    }
  }

  fn seek_exact_with_ord(&mut self, ord: i64) -> Result<()> {
    match self {
      Self::Original(terms) => terms.seek_exact_with_ord(ord),
      Self::Sorting(terms) => terms.seek_exact_with_ord(ord),
    }
  }

  fn seek_exact_with_state<BS: ByteSource>(
    &mut self,
    term: &BytesRef<BS>,
    state: &TermStateEnum,
  ) -> Result<()> {
    match self {
      Self::Original(terms) => terms.seek_exact_with_state(term, state),
      Self::Sorting(terms) => terms.seek_exact_with_state(term, state),
    }
  }

  fn term(&self) -> Result<Self::Value<'_>> {
    match self {
      Self::Original(terms) => terms.term().map(BytesRefValue::into_value),
      Self::Sorting(terms) => terms.term().map(BytesRefValue::into_value),
    }
  }

  fn ord(&self) -> Result<i64> {
    match self {
      Self::Original(terms) => terms.ord(),
      Self::Sorting(terms) => terms.ord(),
    }
  }

  fn doc_freq(&mut self) -> Result<i32> {
    match self {
      Self::Original(terms) => terms.doc_freq(),
      Self::Sorting(terms) => terms.doc_freq(),
    }
  }

  fn total_term_freq(&mut self) -> Result<i64> {
    match self {
      Self::Original(terms) => terms.total_term_freq(),
      Self::Sorting(terms) => terms.total_term_freq(),
    }
  }

  type PostingsEnum = PostingsEnumEnum2<T::PostingsEnum, SortingPostingsEnumType<T::PostingsEnum>>;

  fn postings_with_flags(
    &mut self,
    reuse: Option<Self::PostingsEnum>,
    flags: i32,
  ) -> Result<Self::PostingsEnum> {
    match self {
      Self::Original(terms) => {
        let reuse = match reuse {
          Some(PostingsEnumEnum2::A(reuse)) => Some(reuse),
          _ => None,
        };
        terms
          .postings_with_flags(reuse, flags)
          .map(PostingsEnumEnum2::A)
      },
      Self::Sorting(terms) => {
        let reuse = match reuse {
          Some(PostingsEnumEnum2::B(reuse)) => Some(reuse),
          _ => None,
        };
        terms
          .postings_with_flags(reuse, flags)
          .map(PostingsEnumEnum2::B)
      },
    }
  }

  type ImpactsEnum = T::ImpactsEnum;

  fn impacts(&mut self, flags: i32) -> Result<Self::ImpactsEnum> {
    match self {
      Self::Original(terms) => terms.impacts(flags),
      Self::Sorting(terms) => terms.impacts(flags),
    }
  }

  fn term_state(&mut self) -> Result<TermStateEnum> {
    match self {
      Self::Original(terms) => terms.term_state(),
      Self::Sorting(terms) => terms.term_state(),
    }
  }
}

pub enum SortingCodecReaderFieldsProducer<FP, DM> {
  Original(FP),
  Sorting(FieldsProducerImpl<FP, DM>),
}

impl<FP, DM> CloseableRef for SortingCodecReaderFieldsProducer<FP, DM>
where
  FP: FieldsProducer,
{
  fn close(&self) -> Result<()> {
    match self {
      Self::Original(producer) => producer.close(),
      Self::Sorting(producer) => producer.close(),
    }
  }
}

impl<FP, DM> Fields for SortingCodecReaderFieldsProducer<FP, DM>
where
  FP: FieldsProducer,
  DM: DocMap + Clone,
{
  type FieldIter<'a>
    = FP::FieldIter<'a>
  where
    Self: 'a;

  fn iterator(&self) -> Result<Self::FieldIter<'_>> {
    match self {
      Self::Original(producer) => producer.iterator(),
      Self::Sorting(producer) => producer.iterator(),
    }
  }

  type Terms = SortingCodecReaderTerms<FP::Terms, DM>;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    match self {
      Self::Original(producer) => producer
        .terms(field)
        .map(|terms| terms.map(SortingCodecReaderTerms::Original)),
      Self::Sorting(producer) => producer
        .terms(field)
        .map(|terms| terms.map(SortingCodecReaderTerms::Sorting)),
    }
  }

  fn size(&self) -> Result<i32> {
    match self {
      Self::Original(producer) => producer.size(),
      Self::Sorting(producer) => producer.size(),
    }
  }
}

impl<FP, DM> FieldsProducer for SortingCodecReaderFieldsProducer<FP, DM>
where
  FP: FieldsProducer,
  DM: DocMap + Clone,
{
  fn check_integrity(&self) -> Result<()> {
    match self {
      Self::Original(producer) => producer.check_integrity(),
      Self::Sorting(producer) => producer.check_integrity(),
    }
  }

  fn get_merge_instance(&self) -> Result<Option<Self>> {
    match self {
      Self::Original(producer) => Ok(producer.get_merge_instance()?.map(Self::Original)),
      Self::Sorting(producer) => Ok(producer.get_merge_instance()?.map(Self::Sorting)),
    }
  }
}

pub struct FilterCodecReaderImpl<CR> {
  in_: CR,
  new_meta_data: LeafMetaData,
  index_base: IndexReaderBase,
}
impl<CR> FilterCodecReaderImpl<CR> {
  pub fn new(reader: CR, new_meta_data: LeafMetaData) -> Self {
    Self {
      in_: reader,
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

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    Ok(None)
  }

  type Terms = <CR as LeafReader>::Terms;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    LeafReader::terms(&self.in_, field)
  }

  type NumericDocValues = <CR as LeafReader>::NumericDocValues;

  fn get_numeric_doc_values(&self, field: &str) -> Result<Option<Self::NumericDocValues>> {
    LeafReader::get_numeric_doc_values(&self.in_, field)
  }

  type BinaryDocValues = <CR as LeafReader>::BinaryDocValues;

  fn get_binary_doc_values(&self, field: &str) -> Result<Option<Self::BinaryDocValues>> {
    LeafReader::get_binary_doc_values(&self.in_, field)
  }

  type SortedDocValues = <CR as LeafReader>::SortedDocValues;

  fn get_sorted_doc_values(&self, field: &str) -> Result<Option<Self::SortedDocValues>> {
    LeafReader::get_sorted_doc_values(&self.in_, field)
  }

  type SortedNumericDocValues = <CR as LeafReader>::SortedNumericDocValues;

  fn get_sorted_numeric_doc_values(
    &self,
    field: &str,
  ) -> Result<Option<Self::SortedNumericDocValues>> {
    LeafReader::get_sorted_numeric_doc_values(&self.in_, field)
  }

  type SortedSetDocValues = <CR as LeafReader>::SortedSetDocValues;

  fn get_sorted_set_doc_values(&self, field: &str) -> Result<Option<Self::SortedSetDocValues>> {
    LeafReader::get_sorted_set_doc_values(&self.in_, field)
  }

  type NormNumericDocValues = <CR as LeafReader>::NormNumericDocValues;

  fn get_norm_values(&self, field: &str) -> Result<Option<Self::NormNumericDocValues>> {
    LeafReader::get_norm_values(&self.in_, field)
  }

  type DocValuesSkipper = <CR as LeafReader>::DocValuesSkipper;

  fn get_doc_values_skipper(&self, field: &str) -> Result<Option<Self::DocValuesSkipper>> {
    LeafReader::get_doc_values_skipper(&self.in_, field)
  }

  type FloatVectorValues = <CR as LeafReader>::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Option<Self::FloatVectorValues>> {
    LeafReader::get_float_vector_values(&self.in_, field)
  }

  type ByteVectorValues = <CR as LeafReader>::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Option<Self::ByteVectorValues>> {
    LeafReader::get_byte_vector_values(&self.in_, field)
  }

  fn search_nearest_vectors_f32<B, K, TV>(
    &self,
    field: &str,
    target: TV,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
    TV: Into<Vec<f32>>,
  {
    let target = target.into();
    LeafReader::search_nearest_vectors_f32(&self.in_, field, target, knn_collector, accept_docs)
  }

  fn search_nearest_vectors_u8<B, K, TV>(
    &self,
    field: &str,
    target: TV,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
    TV: Into<Vec<u8>>,
  {
    let target = target.into();
    LeafReader::search_nearest_vectors_u8(&self.in_, field, target, knn_collector, accept_docs)
  }

  fn get_field_infos(&self) -> Result<Arc<FieldInfos>> {
    self.in_.get_field_infos()
  }

  type Bits = <CR as LeafReader>::Bits;

  fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
    LeafReader::get_live_docs(&self.in_)
  }

  type PointValues = <CR as LeafReader>::PointValues;

  fn get_point_values(&self, field: &str) -> Result<Option<Self::PointValues>> {
    LeafReader::get_point_values(&self.in_, field)
  }

  fn get_metadata(&self) -> Result<&LeafMetaData> {
    Ok(&self.new_meta_data)
  }
}

impl<CR> IndexReader for FilterCodecReaderImpl<CR>
where
  CR: CodecReader,
{
  type ContextKind = LeafReaderContextKind;

  type TermVectors = <CR as IndexReader>::TermVectors;

  fn term_vectors(&self) -> Result<Self::TermVectors> {
    IndexReader::term_vectors(&self.in_)
  }

  fn max_doc(&self) -> Result<i32> {
    self.in_.max_doc()
  }

  fn num_docs(&self) -> Result<i32> {
    self.in_.num_docs()
  }

  type StoredFields = <CR as IndexReader>::StoredFields;

  fn stored_fields(&self) -> Result<Self::StoredFields> {
    IndexReader::stored_fields(&self.in_)
  }

  fn do_close(&self) -> Result<()> {
    self.in_.do_close()
  }

  type ReaderCacheHelper = DummyCacheHelper;

  fn get_reader_cache_helper(&self) -> Result<Option<Self::ReaderCacheHelper>> {
    Ok(None)
  }

  fn doc_freq(&self, term: &Term) -> Result<i32> {
    IndexReader::doc_freq(&self.in_, term)
  }

  fn total_term_freq(&self, term: &Term) -> Result<i64> {
    IndexReader::total_term_freq(&self.in_, term)
  }

  fn get_sum_doc_freq(&self, field: &str) -> Result<i64> {
    IndexReader::get_sum_doc_freq(&self.in_, field)
  }

  fn get_doc_count(&self, field: &str) -> Result<i32> {
    IndexReader::get_doc_count(&self.in_, field)
  }

  fn get_sum_total_term_freq(&self, field: &str) -> Result<i64> {
    IndexReader::get_sum_total_term_freq(&self.in_, field)
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
    write!(f, "SortingCodecReader({})", self.in_)
  }
}

impl<CR> CodecReader for FilterCodecReaderImpl<CR>
where
  CR: CodecReader,
{
  type StoredFieldsReader = <CR as CodecReader>::StoredFieldsReader;
  type TermVectorsReader = <CR as CodecReader>::TermVectorsReader;
  type NormsProducer = <CR as CodecReader>::NormsProducer;
  type DocValuesProducer = <CR as CodecReader>::DocValuesProducer;
  type FieldsProducer = <CR as CodecReader>::FieldsProducer;
  type PointsReader = <CR as CodecReader>::PointsReader;
  type KnnVectorsReader = <CR as CodecReader>::KnnVectorsReader;

  fn get_fields_reader(&self) -> Result<Option<Self::StoredFieldsReader>> {
    self.in_.get_fields_reader()
  }

  fn get_term_vectors_reader(&self) -> Result<Option<Self::TermVectorsReader>> {
    self.in_.get_term_vectors_reader()
  }

  fn get_norms_reader(&self) -> Result<Option<Self::NormsProducer>> {
    self.in_.get_norms_reader()
  }

  fn get_doc_values_reader(&self) -> Result<Option<Self::DocValuesProducer>> {
    self.in_.get_doc_values_reader()
  }

  fn get_postings_reader(&self) -> Result<Option<Self::FieldsProducer>> {
    self.in_.get_postings_reader()
  }

  fn get_points_reader(&self) -> Result<Option<Self::PointsReader>> {
    self.in_.get_points_reader()
  }

  fn get_vector_reader(&self) -> Result<Option<Self::KnnVectorsReader>> {
    self.in_.get_vector_reader()
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
  match doc_map {
    Some(doc_map) => Ok(SortingCodecReaderEnum::Sorting(wrap_with_doc_map(
      reader,
      doc_map,
      Some(Arc::new(sorter.sort)),
    )?)),
    None => {
      let meta_data = reader.get_metadata()?;
      let new_meta_data = LeafMetaData::new(
        meta_data.get_created_version_major(),
        meta_data.get_min_version().clone(),
        Some(Arc::new(sorter.sort)),
        meta_data.get_has_blocks(),
      )?;
      Ok(SortingCodecReaderEnum::Filter(FilterCodecReaderImpl::new(
        reader,
        new_meta_data,
      )))
    },
  }
}
/// Expert: same as `wrap_with` but operates directly on a [`DocMap`].
pub fn wrap_with_doc_map<CR, DM>(
  reader: CR,
  doc_map: DM,
  sort: Option<Arc<Sort>>,
) -> Result<SortingCodecReader<CR, DM>>
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
  if reader.max_doc()? != doc_map.size() {
    return Err(LuceneError::illegal_argument(format!(
      "reader.max_doc() should equal doc_map.len(), got {} != {}",
      reader.max_doc()?,
      doc_map.size()
    )));
  }
  debug_assert!(Sorter::is_consistent(&doc_map)?);
  Ok(SortingCodecReader::new(reader, doc_map, new_meta_data))
}

pub enum SortingCodecReaderEnum<CR, DM> {
  Filter(FilterCodecReaderImpl<CR>),
  Sorting(SortingCodecReader<CR, DM>),
}

impl<CR, DM> LeafReader for SortingCodecReaderEnum<CR, DM>
where
  CR: CodecReader,
  DM: Clone + DocMap,
{
  type CacheHelper = DummyCacheHelper;

  fn get_core_cache_helper(&self) -> Result<Option<Self::CacheHelper>> {
    match self {
      SortingCodecReaderEnum::Filter(reader) => reader.get_core_cache_helper(),
      SortingCodecReaderEnum::Sorting(reader) => reader.get_core_cache_helper(),
    }
  }

  type Terms = <CR as LeafReader>::Terms;

  fn terms(&self, field: &str) -> Result<Option<Self::Terms>> {
    match self {
      SortingCodecReaderEnum::Filter(reader) => LeafReader::terms(reader, field),
      SortingCodecReaderEnum::Sorting(reader) => LeafReader::terms(reader, field),
    }
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

  type FloatVectorValues = <CR as LeafReader>::FloatVectorValues;

  fn get_float_vector_values(&self, field: &str) -> Result<Option<Self::FloatVectorValues>> {
    match self {
      SortingCodecReaderEnum::Filter(reader) => LeafReader::get_float_vector_values(reader, field),
      SortingCodecReaderEnum::Sorting(reader) => LeafReader::get_float_vector_values(reader, field),
    }
  }

  type ByteVectorValues = <CR as LeafReader>::ByteVectorValues;

  fn get_byte_vector_values(&self, field: &str) -> Result<Option<Self::ByteVectorValues>> {
    match self {
      SortingCodecReaderEnum::Filter(reader) => LeafReader::get_byte_vector_values(reader, field),
      SortingCodecReaderEnum::Sorting(reader) => LeafReader::get_byte_vector_values(reader, field),
    }
  }

  fn search_nearest_vectors_f32<B, K, TV>(
    &self,
    field: &str,
    target: TV,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
    TV: Into<Vec<f32>>,
  {
    let target = target.into();
    match self {
      SortingCodecReaderEnum::Filter(reader) => {
        LeafReader::search_nearest_vectors_f32(reader, field, target, knn_collector, accept_docs)
      },
      SortingCodecReaderEnum::Sorting(reader) => {
        LeafReader::search_nearest_vectors_f32(reader, field, target, knn_collector, accept_docs)
      },
    }
  }

  fn search_nearest_vectors_u8<B, K, TV>(
    &self,
    field: &str,
    target: TV,
    knn_collector: &mut K,
    accept_docs: Option<B>,
  ) -> Result<()>
  where
    B: Bits,
    K: KnnCollector,
    TV: Into<Vec<u8>>,
  {
    let target = target.into();
    match self {
      SortingCodecReaderEnum::Filter(reader) => {
        LeafReader::search_nearest_vectors_u8(reader, field, target, knn_collector, accept_docs)
      },
      SortingCodecReaderEnum::Sorting(reader) => {
        LeafReader::search_nearest_vectors_u8(reader, field, target, knn_collector, accept_docs)
      },
    }
  }

  fn get_field_infos(&self) -> Result<Arc<FieldInfos>> {
    match self {
      SortingCodecReaderEnum::Filter(reader) => reader.get_field_infos(),
      SortingCodecReaderEnum::Sorting(reader) => reader.get_field_infos(),
    }
  }

  type Bits = SortingCodecReaderBits<CRBits<CR>, DM>;

  fn get_live_docs(&self) -> Result<Option<Self::Bits>> {
    match self {
      SortingCodecReaderEnum::Filter(reader) => {
        Ok(reader.get_live_docs()?.map(SortingCodecReaderBits::Filter))
      },
      SortingCodecReaderEnum::Sorting(reader) => {
        Ok(reader.get_live_docs()?.map(SortingCodecReaderBits::Sorting))
      },
    }
  }

  type PointValues = <CR as LeafReader>::PointValues;

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
  type ContextKind = LeafReaderContextKind;

  type TermVectors = <CR as IndexReader>::TermVectors;

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

  type StoredFields = <CR as IndexReader>::StoredFields;

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
    <CR as CodecReader>::StoredFieldsReader,
    <SortingCodecReader<CR, DM> as CodecReader>::StoredFieldsReader,
  >;
  type TermVectorsReader =
    SortingCodecReaderTermVectorsReader<<CR as CodecReader>::TermVectorsReader, DM>;
  type NormsProducer = SortingNormsProducerEnum<CRNormsProducer<CR>, DM>;
  type DocValuesProducer = SortingCodecReaderDocValuesProducer<CRDocValuesProducer<CR>, DM>;
  type FieldsProducer = SortingCodecReaderFieldsProducer<CRFieldsProducer<CR>, DM>;
  type PointsReader = SortingCodecReaderPointsReader<
    <CR as CodecReader>::PointsReader,
    <SortingCodecReader<CR, DM> as CodecReader>::PointsReader,
  >;
  type KnnVectorsReader = SortingCodecReaderKnnVectorsReader<
    <CR as CodecReader>::KnnVectorsReader,
    <SortingCodecReader<CR, DM> as CodecReader>::KnnVectorsReader,
  >;

  fn get_fields_reader(&self) -> Result<Option<Self::StoredFieldsReader>> {
    Ok(match self {
      SortingCodecReaderEnum::Filter(f) => f.get_fields_reader()?.map(StoredFieldsReaderEnum2::A),
      SortingCodecReaderEnum::Sorting(s) => s.get_fields_reader()?.map(StoredFieldsReaderEnum2::B),
    })
  }

  fn get_term_vectors_reader(&self) -> Result<Option<Self::TermVectorsReader>> {
    Ok(match self {
      SortingCodecReaderEnum::Filter(f) => f
        .get_term_vectors_reader()?
        .map(SortingCodecReaderTermVectorsReader::Filter),
      SortingCodecReaderEnum::Sorting(s) => s
        .get_term_vectors_reader()?
        .map(SortingCodecReaderTermVectorsReader::Sorting),
    })
  }

  fn get_norms_reader(&self) -> Result<Option<Self::NormsProducer>> {
    Ok(match self {
      SortingCodecReaderEnum::Filter(f) => f.get_norms_reader()?.map(SortingNormsProducerEnum::A),
      SortingCodecReaderEnum::Sorting(s) => s.get_norms_reader()?.map(SortingNormsProducerEnum::B),
    })
  }

  fn get_doc_values_reader(&self) -> Result<Option<Self::DocValuesProducer>> {
    Ok(match self {
      SortingCodecReaderEnum::Filter(f) => f
        .get_doc_values_reader()?
        .map(SortingCodecReaderDocValuesProducer::Original),
      SortingCodecReaderEnum::Sorting(s) => s
        .get_doc_values_reader()?
        .map(SortingCodecReaderDocValuesProducer::Sorting),
    })
  }

  fn get_postings_reader(&self) -> Result<Option<Self::FieldsProducer>> {
    Ok(match self {
      SortingCodecReaderEnum::Filter(f) => f
        .get_postings_reader()?
        .map(SortingCodecReaderFieldsProducer::Original),
      SortingCodecReaderEnum::Sorting(s) => s
        .get_postings_reader()?
        .map(SortingCodecReaderFieldsProducer::Sorting),
    })
  }

  fn get_points_reader(&self) -> Result<Option<Self::PointsReader>> {
    Ok(match self {
      SortingCodecReaderEnum::Filter(f) => f
        .get_points_reader()?
        .map(SortingCodecReaderPointsReader::Filter),
      SortingCodecReaderEnum::Sorting(s) => s
        .get_points_reader()?
        .map(SortingCodecReaderPointsReader::Sorting),
    })
  }

  fn get_vector_reader(&self) -> Result<Option<Self::KnnVectorsReader>> {
    Ok(match self {
      SortingCodecReaderEnum::Filter(f) => f
        .get_vector_reader()?
        .map(SortingCodecReaderKnnVectorsReader::Filter),
      SortingCodecReaderEnum::Sorting(s) => s
        .get_vector_reader()?
        .map(SortingCodecReaderKnnVectorsReader::Sorting),
    })
  }
}
/// Creates a factory for [`SortingValuesIterator`]. Does the work of computing the
/// (new docId to old ordinal) mapping, and caches the result, enabling it to create
/// new iterators cheaply.
///
/// # Arguments
///
/// * `values` - the values over which to iterate
/// * `doc_map` - the mapping from "old" docIds to "new" (sorted) docIds.
pub fn iterator_supplier<V, D>(values: &mut V, doc_map: &D) -> Result<SortingIteratorSupplier>
where
  V: KnnVectorValues,
  D: DocMap,
{
  let doc_map_size = doc_map.size() as usize;
  let mut doc_to_ord = vec![0i32; doc_map_size];
  let mut doc_bits = FixedBitSet::new(doc_map_size);
  let mut count = 0usize;

  // Note: doc_to_ord will contain zero for docids that have no vector. This is OK though
  // because the iterator cannot be positioned on such docs
  let mut iter = values.iterator()?;
  let mut doc = iter.next_doc()?;
  while doc != NO_MORE_DOCS {
    let new_doc_id = doc_map.old_to_new(doc)?;
    if new_doc_id != -1 {
      let new_doc_id = new_doc_id as usize;
      doc_to_ord[new_doc_id] = iter.index()?;
      doc_bits.set(new_doc_id)?;
      count += 1;
    }
    doc = iter.next_doc()?;
  }

  Ok(SortingIteratorSupplier::new(doc_bits, doc_to_ord, count))
}
pub struct SortingValuesIterator {
  docs_with_values: BitSetIterator<Arc<FixedBitSet>>,
  doc_to_ord: Arc<Vec<i32>>,
  doc: i32,
}

impl SortingValuesIterator {
  pub fn new(doc_bits: Arc<FixedBitSet>, doc_to_ord: Arc<Vec<i32>>, size: i32) -> Result<Self> {
    let docs_with_values = BitSetIterator::new(doc_bits, size as i64)?;
    Ok(Self {
      docs_with_values,
      doc_to_ord,
      doc: -1,
    })
  }
}

impl crate::core::search::doc_id_set_iterator::DocIdSetIteratorExtensions
  for SortingValuesIterator
{
}
impl crate::core::search::doc_id_set_iterator::BitSetIteratorAccess for SortingValuesIterator {}

impl DocIdSetIterator for SortingValuesIterator {
  fn doc_id(&self) -> i32 {
    self.doc
  }

  fn next_doc(&mut self) -> Result<i32> {
    if self.doc != NO_MORE_DOCS {
      self.doc = self.docs_with_values.next_doc()?;
    }
    Ok(self.doc)
  }

  fn advance(&mut self, _target: i32) -> Result<i32> {
    Err(LuceneError::unsupported_operation(""))
  }

  fn cost(&self) -> Result<i64> {
    Ok(self.docs_with_values.bits.cardinality() as i64)
  }
}

impl DocIndexIterator for SortingValuesIterator {
  fn index(&self) -> Result<i32> {
    let doc_index = self.doc as usize;
    debug_assert!(self.docs_with_values.bits.get(doc_index)?);
    Ok(self.doc_to_ord[doc_index])
  }
}
pub struct SortingIteratorSupplier {
  doc_bits: Arc<FixedBitSet>,
  doc_to_ord: Arc<Vec<i32>>,
  size: usize,
}

impl SortingIteratorSupplier {
  pub fn new(doc_bits: FixedBitSet, doc_to_ord: Vec<i32>, size: usize) -> Self {
    Self {
      doc_bits: Arc::new(doc_bits),
      doc_to_ord: Arc::new(doc_to_ord),
      size,
    }
  }

  pub fn size(&self) -> usize {
    self.size
  }
}
impl Supplier<SortingValuesIterator> for SortingIteratorSupplier {
  fn get(&self) -> Result<SortingValuesIterator> {
    SortingValuesIterator::new(
      self.doc_bits.clone(),
      self.doc_to_ord.clone(),
      self.size as i32,
    )
  }
}
/// Float vector values returned by the reordered merge reader.
///
/// The sorting branch cannot create a vector scorer, so its scorer result does
/// not need a second enum layer.  The other associated return types still vary
/// by branch and remain explicitly enumerated below.
pub(crate) enum ReorderedMergeFloatVectorValues<T, U> {
  A(T),
  B(U),
}

impl<T, U> KnnVectorValues for ReorderedMergeFloatVectorValues<T, U>
where
  T: FloatVectorValues,
  U: FloatVectorValues,
{
  fn dimension(&self) -> usize {
    match self {
      Self::A(values) => values.dimension(),
      Self::B(values) => values.dimension(),
    }
  }

  fn size(&self) -> usize {
    match self {
      Self::A(values) => values.size(),
      Self::B(values) => values.size(),
    }
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    match self {
      Self::A(values) => values.ord_to_doc(ord),
      Self::B(values) => values.ord_to_doc(ord),
    }
  }

  type KnnVectorValues = T::KnnVectorValues;

  fn copy(&self) -> Result<Self::KnnVectorValues> {
    match self {
      Self::A(values) => values.copy(),
      Self::B(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn get_vector_byte_length(&self) -> usize {
    match self {
      Self::A(values) => values.get_vector_byte_length(),
      Self::B(values) => values.get_vector_byte_length(),
    }
  }

  fn get_encoding(&self) -> VectorEncoding {
    match self {
      Self::A(values) => KnnVectorValues::get_encoding(values),
      Self::B(values) => KnnVectorValues::get_encoding(values),
    }
  }

  type Bits<'a, B1>
    = BitsEnum2<T::Bits<'a, B1>, U::Bits<'a, B1>>
  where
    B1: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B1>(&'a self, accept_docs: Option<B1>) -> Option<Self::Bits<'a, B1>>
  where
    B1: Bits,
  {
    match self {
      Self::A(values) => values.get_accept_ords(accept_docs).map(BitsEnum2::A),
      Self::B(values) => values.get_accept_ords(accept_docs).map(BitsEnum2::B),
    }
  }

  type DocIndexIterator = DocIndexIteratorEnum2<T::DocIndexIterator, U::DocIndexIterator>;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    match self {
      Self::A(values) => values.iterator().map(DocIndexIteratorEnum2::A),
      Self::B(values) => values.iterator().map(DocIndexIteratorEnum2::B),
    }
  }
}

impl<T, U> FloatVectorValues for ReorderedMergeFloatVectorValues<T, U>
where
  T: FloatVectorValues,
  U: FloatVectorValues,
{
  type VectorValue<'v>
    = FloatVectorValue<'v>
  where
    Self: 'v;

  fn vector_value(&self, ord: usize) -> Result<Self::VectorValue<'_>> {
    match self {
      Self::A(values) => values.vector_value(ord).map(Into::into),
      Self::B(values) => values.vector_value(ord).map(Into::into),
    }
  }

  type FloatVectorValues = T::FloatVectorValues;

  fn float_copy(&self) -> Result<Option<Self::FloatVectorValues>> {
    match self {
      Self::A(values) => values.float_copy(),
      Self::B(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  type VectorScorer = T::VectorScorer;

  fn scorer<'a, TV>(&self, target: TV) -> Result<Option<Self::VectorScorer>>
  where
    TV: Into<std::borrow::Cow<'a, [f32]>>,
  {
    let target: std::borrow::Cow<'a, [f32]> = target.into();
    match self {
      Self::A(values) => values.scorer(target),
      Self::B(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn get_encoding(&self) -> VectorEncoding {
    match self {
      Self::A(values) => FloatVectorValues::get_encoding(values),
      Self::B(values) => FloatVectorValues::get_encoding(values),
    }
  }

  fn get_vectors_mut(&mut self) -> Result<&mut Vec<Box<[f32]>>> {
    match self {
      Self::A(values) => values.get_vectors_mut(),
      Self::B(values) => values.get_vectors_mut(),
    }
  }

  fn get_vectors(&self) -> Result<&[Box<[f32]>]> {
    match self {
      Self::A(values) => values.get_vectors(),
      Self::B(values) => values.get_vectors(),
    }
  }

  fn get_vectors_capacity(&self) -> Result<usize> {
    match self {
      Self::A(values) => values.get_vectors_capacity(),
      Self::B(values) => values.get_vectors_capacity(),
    }
  }
}

/// Byte vector values returned by the reordered merge reader.
///
/// The sorting branch cannot create a copy or vector scorer, so those results
/// do not need a second enum layer.  The other associated return types still
/// vary by branch and remain explicitly enumerated below.
pub(crate) enum ReorderedMergeByteVectorValues<T, U> {
  A(T),
  B(U),
}

impl<T, U> KnnVectorValues for ReorderedMergeByteVectorValues<T, U>
where
  T: ByteVectorValues,
  U: ByteVectorValues,
{
  fn dimension(&self) -> usize {
    match self {
      Self::A(values) => values.dimension(),
      Self::B(values) => values.dimension(),
    }
  }

  fn size(&self) -> usize {
    match self {
      Self::A(values) => values.size(),
      Self::B(values) => values.size(),
    }
  }

  fn ord_to_doc(&self, ord: usize) -> Result<usize> {
    match self {
      Self::A(values) => values.ord_to_doc(ord),
      Self::B(values) => values.ord_to_doc(ord),
    }
  }

  type KnnVectorValues = T::KnnVectorValues;

  fn copy(&self) -> Result<Self::KnnVectorValues> {
    match self {
      Self::A(values) => values.copy(),
      Self::B(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn get_vector_byte_length(&self) -> usize {
    match self {
      Self::A(values) => values.get_vector_byte_length(),
      Self::B(values) => values.get_vector_byte_length(),
    }
  }

  fn get_encoding(&self) -> VectorEncoding {
    match self {
      Self::A(values) => KnnVectorValues::get_encoding(values),
      Self::B(values) => KnnVectorValues::get_encoding(values),
    }
  }

  type Bits<'a, B1>
    = BitsEnum2<T::Bits<'a, B1>, U::Bits<'a, B1>>
  where
    B1: Bits,
    Self: 'a;

  fn get_accept_ords<'a, B1>(&'a self, accept_docs: Option<B1>) -> Option<Self::Bits<'a, B1>>
  where
    B1: Bits,
  {
    match self {
      Self::A(values) => values.get_accept_ords(accept_docs).map(BitsEnum2::A),
      Self::B(values) => values.get_accept_ords(accept_docs).map(BitsEnum2::B),
    }
  }

  type DocIndexIterator = DocIndexIteratorEnum2<T::DocIndexIterator, U::DocIndexIterator>;

  fn iterator(&self) -> Result<Self::DocIndexIterator> {
    match self {
      Self::A(values) => values.iterator().map(DocIndexIteratorEnum2::A),
      Self::B(values) => values.iterator().map(DocIndexIteratorEnum2::B),
    }
  }
}

impl<T, U> ByteVectorValues for ReorderedMergeByteVectorValues<T, U>
where
  T: ByteVectorValues,
  U: ByteVectorValues,
{
  type VectorValue<'v>
    = ByteVectorValue<'v>
  where
    Self: 'v;

  fn vector_value(&self, ord: usize) -> Result<Self::VectorValue<'_>> {
    match self {
      Self::A(values) => values.vector_value(ord).map(Into::into),
      Self::B(values) => values.vector_value(ord).map(Into::into),
    }
  }

  type ByteVectorValues = T::ByteVectorValues;

  fn byte_copy(&self) -> Result<Option<Self::ByteVectorValues>> {
    match self {
      Self::A(values) => values.byte_copy(),
      Self::B(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  type VectorScorer = T::VectorScorer;

  fn scorer<TV>(&self, target: TV) -> Result<Option<Self::VectorScorer>>
  where
    TV: Into<Vec<u8>>,
  {
    let target = target.into();
    match self {
      Self::A(values) => values.scorer(target),
      Self::B(_) => Err(LuceneError::unsupported_operation("")),
    }
  }

  fn get_encoding(&self) -> VectorEncoding {
    match self {
      Self::A(values) => ByteVectorValues::get_encoding(values),
      Self::B(values) => ByteVectorValues::get_encoding(values),
    }
  }

  fn get_vectors_mut(&mut self) -> Result<&mut Vec<Box<[u8]>>> {
    match self {
      Self::A(values) => values.get_vectors_mut(),
      Self::B(values) => values.get_vectors_mut(),
    }
  }

  fn get_vectors(&self) -> Result<&[Box<[u8]>]> {
    match self {
      Self::A(values) => values.get_vectors(),
      Self::B(values) => values.get_vectors(),
    }
  }
}

/// Point values returned by the reordered merge reader.
pub(crate) enum ReorderedMergePointValues<T, U> {
  A(T),
  B(U),
}

impl<T, U> PointValues for ReorderedMergePointValues<T, U>
where
  T: PointValues,
  U: PointValues,
{
  fn get_min_packed_value(&self) -> Result<Option<Cow<'_, [u8]>>> {
    match self {
      Self::A(values) => values.get_min_packed_value(),
      Self::B(values) => values.get_min_packed_value(),
    }
  }

  fn get_max_packed_value(&self) -> Result<Option<Cow<'_, [u8]>>> {
    match self {
      Self::A(values) => values.get_max_packed_value(),
      Self::B(values) => values.get_max_packed_value(),
    }
  }

  fn get_num_dimensions(&self) -> Result<usize> {
    match self {
      Self::A(values) => values.get_num_dimensions(),
      Self::B(values) => values.get_num_dimensions(),
    }
  }

  fn get_num_index_dimensions(&self) -> Result<usize> {
    match self {
      Self::A(values) => values.get_num_index_dimensions(),
      Self::B(values) => values.get_num_index_dimensions(),
    }
  }

  fn get_bytes_per_dimension(&self) -> Result<usize> {
    match self {
      Self::A(values) => values.get_bytes_per_dimension(),
      Self::B(values) => values.get_bytes_per_dimension(),
    }
  }

  fn size(&self) -> Result<usize> {
    match self {
      Self::A(values) => values.size(),
      Self::B(values) => values.size(),
    }
  }

  fn get_doc_count(&self) -> Result<i32> {
    match self {
      Self::A(values) => values.get_doc_count(),
      Self::B(values) => values.get_doc_count(),
    }
  }

  type PointTree = PointTreeEnum<
    <T::PointTree as PointTree>::MutablePointTree,
    PointTreeEnum2<T::PointTree, U::PointTree>,
  >;

  fn get_point_tree(&self) -> Result<Self::PointTree> {
    match self {
      Self::A(values) => match values.get_point_tree()?.into_mutable() {
        PointTreeEnum::Mutable(tree) => Ok(PointTreeEnum::Mutable(tree)),
        PointTreeEnum::Other(tree) => Ok(PointTreeEnum::Other(PointTreeEnum2::A(tree))),
      },
      Self::B(values) => match values.get_point_tree()?.into_mutable() {
        PointTreeEnum::Mutable(_) => Err(LuceneError::unsupported_operation("")),
        PointTreeEnum::Other(tree) => Ok(PointTreeEnum::Other(PointTreeEnum2::B(tree))),
      },
    }
  }
}
